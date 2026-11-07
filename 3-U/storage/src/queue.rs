// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Slot<T> {
    ready: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

/// A bounded, lock-free multi-producer single-consumer (MPSC) FIFO queue.
///
/// Capacity `N` must be a non-zero power of two.
pub struct Queue<T, const N: usize> {
    slots: [Slot<T>; N],
    head: AtomicUsize,
    tail: AtomicUsize,
}

unsafe impl<T: Send, const N: usize> Send for Queue<T, N> {}
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}

impl<T, const N: usize> Queue<T, N> {
    const MASK: usize = N - 1;

    pub const fn new() -> Self {
        assert!(N > 0 && N.is_power_of_two());
        Self {
            slots: [const { Slot::new() }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let slot = &self.slots[tail & Self::MASK];
        !slot.ready.load(Ordering::Acquire)
    }

    /// Enqueues `value` into the queue. Can be called concurrently by multiple producers.
    ///
    /// Returns `Err(value)` if the queue is full.
    pub fn enqueue(&self, value: T) -> Result<(), T> {
        let mut head = self.head.load(Ordering::Relaxed);
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            if head.wrapping_sub(tail) >= N {
                let current_head = self.head.load(Ordering::Relaxed);
                if current_head != head {
                    head = current_head;
                    continue;
                }
                return Err(value);
            }
            match self.head.compare_exchange_weak(
                head,
                head.wrapping_add(1),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => head = actual,
            }
        }

        let slot = &self.slots[head & Self::MASK];
        unsafe { (*slot.value.get()).write(value) };
        slot.ready.store(true, Ordering::Release);
        Ok(())
    }

    /// Dequeues the oldest item from the queue.
    ///
    /// Must only be called by the single consumer thread.
    pub fn dequeue(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let slot = &self.slots[tail & Self::MASK];
        if !slot.ready.load(Ordering::Acquire) {
            return None;
        }

        let value = unsafe { (*slot.value.get()).assume_init_read() };
        slot.ready.store(false, Ordering::Relaxed);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(value)
    }
}

impl<T, const N: usize> Default for Queue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for Queue<T, N> {
    fn drop(&mut self) {
        while self.dequeue().is_some() {}
    }
}
