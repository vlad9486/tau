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
    sbi, csr,
    thread::{ActiveWait, Phase, Resume, ThreadState},
    matter::{WaitRecord, WaitTable},
    Context,
    space::{self, Mapping, Root, Window, WindowAccess},
};

const CAPACITY: usize = 128;
// Raw time-counter ticks, deliberately independent of wall-clock time.
const QUANTUM: usize = 100_000;

#[repr(C)]
pub struct Scheduler {
    lock: AtomicBool,
    pub(super) state: UnsafeCell<State>,
}

pub struct InterruptTarget {
    pub thread_id: usize,
    pub table: WaitTable,
    pub slot: usize,
}

pub(super) struct State {
    pub(super) threads: [Option<ThreadState>; CAPACITY],
    pub(super) count: usize,
    pub(super) queue: [u16; CAPACITY],
    pub(super) head: usize,
    pub(super) length: usize,
    pub(super) running: usize,
    pub(super) active: bool,
    pub(super) quantum: usize,
    pub(super) interrupt_targets: [Option<InterruptTarget>; 8],
    pub(super) deadlines: [usize; CAPACITY],
}

// All access to the shared State is serialized by the interrupt-safe lock.
unsafe impl Sync for Scheduler {}

pub(super) struct Guard<'a> {
    scheduler: &'a mut Scheduler,
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
            csr::rs!("sstatus", 2);
        }
    }
}

impl Scheduler {
    pub(super) fn lock(&mut self) -> Guard<'_> {
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
    csr::r!("time")
}

#[cold]
pub(super) fn invariant_failed() -> ! {
    sbi::Printer.ch(*b"SU: scheduler invariant failed\r\n");
    loop {
        hint::spin_loop();
    }
}

impl State {
    pub(super) fn take_thread(&mut self, id: usize) -> Option<ThreadState> {
        id.checked_sub(1)
            .filter(|&i| i < self.count)
            .and_then(|i| self.threads.get_mut(i))
            .and_then(Option::take)
    }

    pub(super) fn thread_state_mut(&mut self, id: usize) -> &mut ThreadState {
        id.checked_sub(1)
            .and_then(|i| self.threads.get_mut(i))
            .unwrap_or_else(|| invariant_failed())
            .as_mut()
            .unwrap_or_else(|| invariant_failed())
    }

    pub fn set_thread(&mut self, id: usize, thread: ThreadState) {
        *id.checked_sub(1)
            .and_then(|i| self.threads.get_mut(i))
            .unwrap_or_else(|| invariant_failed()) = Some(thread);
    }

    pub(super) fn enqueue(&mut self, id: usize) {
        if self.length >= CAPACITY {
            invariant_failed();
        }
        self.queue[(self.head + self.length) % CAPACITY] = id as u16;
        self.length += 1;
    }

    pub(super) fn push_front(&mut self, id: usize) {
        if self.length >= CAPACITY {
            invariant_failed();
        }
        self.head = (self.head + CAPACITY - 1) % CAPACITY;
        self.queue[self.head] = id as u16;
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
            csr::rs!("sie", 1 << 5);
            self.quantum = now().saturating_add(QUANTUM);
        }
    }

    pub(super) fn program(&self) {
        let mut next = if self.running != 0 && self.length != 0 {
            self.quantum
        } else {
            usize::MAX
        };
        for &deadline in self.deadlines.iter().take(self.count) {
            if deadline != 0 {
                next = next.min(deadline);
            }
        }
        sbi::set_timer(next).unwrap_or_default();
    }

    pub(super) fn complete(
        &mut self,
        window: &mut WindowAccess,
        thread_id: usize,
        mut thread: ThreadState,
        wait: ActiveWait,
        mut slot: WaitRecord,
        event: Event,
        high_priority: bool,
    ) {
        slot.thread_id = 0;
        slot.sequence = 0;
        unsafe { wait.table.write(window, wait.slot, slot) };
        let enqueue = thread.complete([0, event.encode(), 0, 0, 0, 0]);
        self.set_thread(thread_id, thread);
        unsafe { self.set_deadline(thread_id - 1, 0) };
        if enqueue {
            if high_priority {
                self.push_front(thread_id);
            } else {
                self.enqueue(thread_id);
            }
        }
    }

    fn complete_wait(
        &mut self,
        window: &mut WindowAccess,
        thread_id: usize,
        event: Event,
        high_priority: bool,
    ) -> bool {
        let Some(mut thread) = self.take_thread(thread_id) else {
            return false;
        };
        let Some(wait) = &thread.wait else {
            self.set_thread(thread_id, thread);
            return false;
        };
        let slot = unsafe { wait.table.read(window, wait.slot) };
        if slot.thread_id == thread_id && slot.sequence == wait.sequence {
            let wait = unsafe { thread.wait.take().unwrap_unchecked() };
            self.complete(window, thread_id, thread, wait, slot, event, high_priority);
        } else {
            self.set_thread(thread_id, thread);
        }
        true
    }

    fn expire(&mut self, window: &mut WindowAccess) {
        let time = now();
        for index in 0..self.count {
            let deadline = unsafe { self.deadline(index) };
            if deadline != 0 && deadline <= time {
                self.complete_wait(window, index + 1, Event::Timeout, false);
            }
        }
    }

    fn pop_running(&mut self) -> Option<Dispatch> {
        let id = self.pop()?;
        let thread = self.thread_state_mut(id);
        if thread.phase != Phase::Runnable {
            invariant_failed();
        }
        thread.phase = Phase::Running;
        let dispatch = Dispatch {
            root: thread.root,
            kind: thread.resume,
            message: thread.message,
        };
        self.running = id;
        self.quantum = now().saturating_add(QUANTUM);
        self.program();
        Some(dispatch)
    }

    pub(super) unsafe fn deadline(&self, id: usize) -> usize {
        unsafe { *self.deadlines.get_unchecked(id) }
    }

    pub(super) unsafe fn set_deadline(&mut self, id: usize, deadline: usize) {
        unsafe { *self.deadlines.get_unchecked_mut(id) = deadline };
    }
}

impl Scheduler {
    /// Initialize the scheduler once on the bootstrap hart.
    ///
    /// # Safety
    /// Interrupts must be disabled, no other hart may be executing this runtime.
    /// No reference to the scheduler storage may exist before this initialization.
    pub unsafe fn init(
        this: *mut Self,
        window: &mut WindowAccess,
        context: &Context,
        mut thread: ThreadState,
    ) -> Result<(), tau::AllocError> {
        window
            .map(
                Window::current_root(),
                Mapping {
                    addr_virtual: space::SCHEDULER,
                    virtual_pages: size_of::<Scheduler>().div_ceil(space::PAGE),
                    flags: *b"rw--g",
                    ..Mapping::default()
                },
                || context.page(0), // scheduler is equaly used from all harts
            )
            .map_err(|_| tau::AllocError::OutOfMemory)?;
        thread.phase = Phase::Running;
        unsafe {
            ptr::addr_of_mut!((*this).lock).write(AtomicBool::new(false));
            let state = (*this).state.get();
            let threads = ptr::addr_of_mut!((*state).threads).cast::<Option<ThreadState>>();
            for i in 0..CAPACITY {
                threads.add(i).write(None);
            }
            threads.write(Some(thread));
            ptr::addr_of_mut!((*state).count).write(1);
            ptr::addr_of_mut!((*state).queue).write([0; CAPACITY]);
            ptr::addr_of_mut!((*state).head).write(0);
            ptr::addr_of_mut!((*state).length).write(0);
            ptr::addr_of_mut!((*state).running).write(1);
            ptr::addr_of_mut!((*state).active).write(false);
            ptr::addr_of_mut!((*state).quantum).write(0);
            ptr::addr_of_mut!((*state).interrupt_targets)
                .write([None, None, None, None, None, None, None, None]);
            ptr::addr_of_mut!((*state).deadlines).write([0; CAPACITY]);
        }
        Ok(())
    }

    pub fn active(&mut self) -> bool {
        self.lock().active
    }
}

pub struct Dispatch {
    pub root: Root,
    pub kind: super::ReturnKind,
    pub message: [usize; 6],
}

pub fn notify_hardware_interrupt(
    window: &mut WindowAccess,
    scheduler: &mut Scheduler,
    hart_id: usize,
) {
    let mut scheduler = scheduler.lock();
    let Some(target) = scheduler
        .interrupt_targets
        .get(hart_id)
        .map(Option::as_ref)
        .flatten()
    else {
        return;
    };
    let thread_id = target.thread_id;

    let event = Event::Notified { arg: 0 };
    if !scheduler.complete_wait(window, thread_id, event, true) {
        let target = unsafe {
            scheduler
                .interrupt_targets
                .get(hart_id)
                .map(Option::as_ref)
                .flatten()
                .unwrap_unchecked()
        };

        let mut slot = unsafe { target.table.read(window, target.slot) };
        slot.pending = true;
        unsafe { target.table.write(window, target.slot, slot) };
    }
    if scheduler.active {
        scheduler.program();
    }
}

pub fn preempt(window: &mut WindowAccess, scheduler: &mut Scheduler) -> Dispatch {
    let mut scheduler = scheduler.lock();
    scheduler.expire(window);
    let length = scheduler.length;
    let id = scheduler.running;
    let thread = scheduler.thread_state_mut(id);
    thread.root = Window::current_root();
    thread.resume = Resume::Interrupt;
    if length == 0 {
        thread.phase = Phase::Running;
        let dispatch = Dispatch {
            root: thread.root,
            kind: thread.resume,
            message: thread.message,
        };
        scheduler.quantum = now().saturating_add(QUANTUM);
        scheduler.program();
        dispatch
    } else {
        thread.phase = Phase::Runnable;
        scheduler.enqueue(id);
        scheduler
            .pop_running()
            .unwrap_or_else(|| invariant_failed())
    }
}

/// Interrupts remain disabled between committing departure and installing the
/// selected root. An idle hart waits with SIE clear and STIE enabled: WFI may
/// wake for a pending timer without entering the user trap entry from S-mode.
pub fn dispatch(window: &mut WindowAccess, scheduler: &mut Scheduler, hart_id: usize) -> Dispatch {
    loop {
        {
            let mut scheduler = scheduler.lock();
            scheduler.expire(window);
            if scheduler.running != 0 {
                let id = scheduler.running;
                let thread = scheduler.thread_state_mut(id);
                if thread.phase != Phase::Parking {
                    invariant_failed();
                }
                let parked = thread.commit_parking();
                if !parked {
                    let dispatch = Dispatch {
                        root: thread.root,
                        kind: thread.resume,
                        message: thread.message,
                    };
                    scheduler.program();
                    return dispatch;
                }
                scheduler.running = 0;
            }
            if let Some(dispatch) = scheduler.pop_running() {
                return dispatch;
            }
            scheduler.program();
        }
        unsafe { core::arch::asm!("wfi", options(nostack)) };
        if csr::r!("sip") & (1 << 9) != 0 {
            csr::rc!("sie", 1 << 9);
            notify_hardware_interrupt(window, scheduler, hart_id);
        }
    }
}
