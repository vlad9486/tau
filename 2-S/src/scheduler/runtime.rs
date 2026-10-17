// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    cell::UnsafeCell,
    hint,
    ops::{Deref, DerefMut},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use tau::Event;

use super::{
    thread::{ActiveWait, Phase, Resume},
    Context, Thread, cpu, sbi,
    vmem::{self, Mapping, Root, Window, WindowAccess},
};

const CAPACITY: usize = 128;
// Raw time-counter ticks, deliberately independent of wall-clock time.
const QUANTUM: usize = 100_000;

#[repr(C)]
pub struct Scheduler {
    lock: AtomicBool,
    state: UnsafeCell<State>,
}

pub(super) struct State {
    pub(super) registry: [usize; CAPACITY],
    pub(super) count: usize,
    pub(super) queue: [u16; CAPACITY],
    pub(super) head: usize,
    pub(super) length: usize,
    pub(super) running: usize,
    pub(super) active: bool,
    pub(super) quantum: usize,
}

// All access to the shared State is serialized by the interrupt-safe lock.
unsafe impl Sync for Scheduler {}

pub(super) struct Guard<'a> {
    scheduler: &'a Scheduler,
    interrupts: usize,
}

impl Deref for Guard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        unsafe { &*self.scheduler.state.get() }
    }
}
impl DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        unsafe { &mut *self.scheduler.state.get() }
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.scheduler.lock.store(false, Ordering::Release);
        if self.interrupts & 2 != 0 {
            cpu::csrrs!("sstatus", 2);
        }
    }
}

impl Scheduler {
    pub(super) fn lock(&self) -> Guard<'_> {
        let interrupts: usize;
        unsafe {
            core::arch::asm!("csrrc {0}, sstatus, {1}", out(reg) interrupts, in(reg) 2usize, options(nostack))
        };
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            hint::spin_loop();
        }
        Guard {
            scheduler: self,
            interrupts,
        }
    }
}

pub(super) fn now() -> usize {
    cpu::csrr!("time")
}

#[cold]
pub(super) fn invariant_failed() -> ! {
    sbi::Printer.ch(*b"SU: scheduler invariant failed\r\n");
    loop {
        hint::spin_loop();
    }
}

impl State {
    pub(super) fn physical(&self, id: usize) -> usize {
        id.checked_sub(1)
            .and_then(|i| self.registry.get(i))
            .copied()
            .unwrap_or_else(|| invariant_failed())
    }

    pub(super) fn enqueue(&mut self, id: usize) {
        if self.length >= CAPACITY {
            invariant_failed();
        }
        self.queue[(self.head + self.length) % CAPACITY] = id as u16;
        self.length += 1;
    }

    fn pop(&mut self) -> Option<usize> {
        if self.length == 0 {
            return None;
        }
        let id = self
            .queue
            .get(self.head)
            .copied()
            .unwrap_or_else(|| invariant_failed()) as usize;
        self.head = (self.head + 1) % CAPACITY;
        self.length -= 1;
        Some(id)
    }

    pub(super) fn enable(&mut self) {
        if !self.active {
            self.active = true;
            // Hardware receiver bindings are a later iteration. Scheduled mode
            // never attributes device interrupts to the interrupted worker.
            cpu::csrrc!("sie", 1 << 9);
            cpu::csrrs!("sie", 1 << 5);
            self.quantum = now().saturating_add(QUANTUM);
        }
    }

    pub(super) fn program(&self, window: &mut WindowAccess) {
        let mut next = if self.running != 0 && self.length != 0 {
            self.quantum
        } else {
            usize::MAX
        };
        for physical in self.registry.iter().take(self.count) {
            if let Some(wait) = unsafe { Thread::read(window, *physical) }.wait
                && wait.deadline != 0
            {
                next = next.min(wait.deadline);
            }
        }
        sbi::set_timer(next).unwrap_or_default();
    }

    pub(super) fn complete(
        &mut self,
        window: &mut WindowAccess,
        physical: usize,
        wait: ActiveWait,
        event: Event,
    ) {
        let Some(index) = self
            .registry
            .iter()
            .take(self.count)
            .position(|&p| p == physical)
        else {
            return;
        };
        let mut thread = unsafe { Thread::read(window, physical) };
        let Some(current) = thread.wait else {
            return;
        };
        if !current.matches(wait) {
            return;
        }
        let mut slot = unsafe { wait.table.read(window, wait.slot) };
        if slot.thread != physical || slot.sequence != wait.sequence {
            return;
        }
        slot.thread = 0;
        slot.sequence = 0;
        unsafe { wait.table.write(window, wait.slot, slot) };
        let enqueue = thread.complete([0, event.encode(), 0, 0, 0, 0]);
        unsafe { Thread::write(window, physical, thread) };
        if enqueue {
            self.enqueue(index + 1);
        }
    }

    fn expire(&mut self, window: &mut WindowAccess) {
        let time = now();
        for index in 0..self.count {
            let physical = self.physical(index + 1);
            if let Some(wait) = unsafe { Thread::read(window, physical) }.wait
                && wait.deadline != 0
                && wait.deadline <= time
            {
                self.complete(window, physical, wait, Event::Timeout);
            }
        }
    }
}

impl Scheduler {
    /// Initialize the scheduler once on the bootstrap hart.
    ///
    /// # Safety
    /// Interrupts must be disabled, no other hart may be executing this runtime,
    /// and the current root must already map initialized bootstrap thread metadata.
    /// No reference to the scheduler storage may exist before this initialization.
    pub unsafe fn initialize(
        this: *mut Self,
        window: &mut WindowAccess,
        context: &Context,
        hart: usize,
    ) -> Result<(), tau::AllocError> {
        window
            .map(
                Window::current_root(),
                Mapping {
                    addr_virtual: vmem::SCHEDULER,
                    virtual_pages: size_of::<Scheduler>().div_ceil(vmem::PAGE),
                    flags: *b"rw--g",
                    ..Mapping::default()
                },
                || context.page(hart),
            )
            .map_err(|_| tau::AllocError::OutOfMemory)?;
        let root = Window::current_root();
        let physical = window
            .resolve(root, vmem::THREAD)
            .ok_or(tau::AllocError::OutOfMemory)?
            .0;
        unsafe {
            ptr::addr_of_mut!((*this).lock).write(AtomicBool::new(false));
            let state = (*this).state.get();
            ptr::addr_of_mut!((*state).registry).write([0; CAPACITY]);
            ptr::addr_of_mut!((*state).registry[0]).write(physical);
            ptr::addr_of_mut!((*state).count).write(1);
            ptr::addr_of_mut!((*state).queue).write([0; CAPACITY]);
            ptr::addr_of_mut!((*state).head).write(0);
            ptr::addr_of_mut!((*state).length).write(0);
            ptr::addr_of_mut!((*state).running).write(1);
            ptr::addr_of_mut!((*state).active).write(false);
            ptr::addr_of_mut!((*state).quantum).write(0);
        }
        let mut thread = unsafe { Thread::read(window, physical) };
        thread.phase = Phase::Running;
        unsafe { Thread::write(window, physical, thread) };
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.lock().active
    }
}

pub struct Dispatch {
    pub root: Root,
    pub kind: super::ReturnKind,
    pub message: [usize; 6],
}

pub fn preempt(window: &mut WindowAccess, scheduler: &Scheduler) -> Dispatch {
    {
        let mut scheduler = scheduler.lock();
        let physical = scheduler.physical(scheduler.running);
        let mut thread = unsafe { Thread::read(window, physical) };
        thread.root = Window::current_root();
        thread.resume = Resume::Interrupt;
        thread.phase = Phase::Runnable;
        unsafe { Thread::write(window, physical, thread) };
        scheduler.expire(window);
        let id = scheduler.running;
        scheduler.enqueue(id);
        scheduler.running = 0;
    }
    dispatch(window, scheduler)
}

/// Interrupts remain disabled between committing departure and installing the
/// selected root. An idle hart waits with SIE clear and STIE enabled: WFI may
/// wake for a pending timer without entering the user trap entry from S-mode.
pub fn dispatch(window: &mut WindowAccess, scheduler: &Scheduler) -> Dispatch {
    loop {
        {
            let mut scheduler = scheduler.lock();
            scheduler.expire(window);
            if scheduler.running != 0 {
                let physical = scheduler.physical(scheduler.running);
                let mut thread = unsafe { Thread::read(window, physical) };
                if thread.phase != Phase::Parking {
                    invariant_failed();
                }
                let parked = thread.commit_parking();
                if !parked {
                    unsafe { Thread::write(window, physical, thread) };
                    scheduler.program(window);
                    return Dispatch {
                        root: thread.root,
                        kind: thread.resume,
                        message: thread.message,
                    };
                }
                unsafe { Thread::write(window, physical, thread) };
                scheduler.running = 0;
            }
            if let Some(id) = scheduler.pop() {
                let physical = scheduler.physical(id);
                let mut thread = unsafe { Thread::read(window, physical) };
                if thread.phase != Phase::Runnable {
                    invariant_failed();
                }
                thread.phase = Phase::Running;
                unsafe { Thread::write(window, physical, thread) };
                scheduler.running = id;
                scheduler.quantum = now().saturating_add(QUANTUM);
                scheduler.program(window);
                return Dispatch {
                    root: thread.root,
                    kind: thread.resume,
                    message: thread.message,
                };
            }
            scheduler.program(window);
        }
        unsafe { core::arch::asm!("wfi", options(nostack)) };
    }
}
