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
    thread::{ActiveWait, Phase, Resume, Thread, ThreadState},
    waiting::WaitRecord,
    Context, cpu, sbi,
    vmem::{self, Mapping, Root, Window, WindowAccess},
};

const CAPACITY: usize = 128;
// Raw time-counter ticks, deliberately independent of wall-clock time.
const QUANTUM: usize = 100_000;

#[repr(C)]
pub struct Scheduler {
    lock: AtomicBool,
    pub(super) state: UnsafeCell<State>,
}

#[derive(Clone, Copy)]
pub struct InterruptTarget {
    pub index: usize,
    pub physical: usize,
    pub table: super::waiting::WaitTable,
    pub slot: usize,
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
    pub(super) interrupt_targets: [Option<InterruptTarget>; 8],
    pub(super) deadlines: [usize; CAPACITY],
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
            cpu::csrrs!("sie", 1 << 5);
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
        index: usize,
        physical: usize,
        mut thread: ThreadState,
        wait: ActiveWait,
        mut slot: WaitRecord,
        event: Event,
        high_priority: bool,
    ) {
        slot.thread = 0;
        slot.sequence = 0;
        unsafe { wait.table.write(window, wait.slot, slot) };
        let enqueue = thread.complete([0, event.encode(), 0, 0, 0, 0]);
        window.thread_mut(physical).state = thread;
        unsafe { self.set_deadline(index, 0) };
        if enqueue {
            if high_priority {
                self.push_front(index + 1);
            } else {
                self.enqueue(index + 1);
            }
        }
    }

    fn complete_wait(
        &mut self,
        window: &mut WindowAccess,
        index: usize,
        physical: usize,
        event: Event,
        high_priority: bool,
    ) -> bool {
        let thread = window.thread(physical).state;
        let Some(wait) = thread.wait else {
            return false;
        };
        let slot = unsafe { wait.table.read(window, wait.slot) };
        if slot.thread == physical && slot.sequence == wait.sequence {
            self.complete(
                window,
                index,
                physical,
                thread,
                wait,
                slot,
                event,
                high_priority,
            );
        }
        true
    }

    fn expire(&mut self, window: &mut WindowAccess) {
        let time = now();
        for index in 0..self.count {
            let deadline = unsafe { self.deadline(index) };
            if deadline != 0 && deadline <= time {
                let physical = self.physical(index + 1);
                self.complete_wait(window, index, physical, Event::Timeout, false);
            }
        }
    }

    fn pop_running(&mut self, window: &mut WindowAccess) -> Option<Dispatch> {
        let id = self.pop()?;
        let physical = self.physical(id);
        let thread = &mut window.thread_mut(physical).state;
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
    /// Interrupts must be disabled, no other hart may be executing this runtime,
    /// and the current root must already map initialized bootstrap thread metadata.
    /// No reference to the scheduler storage may exist before this initialization.
    pub unsafe fn initialize(
        this: *mut Self,
        window: &mut WindowAccess,
        context: &Context,
        thread: &mut Thread,
    ) -> Result<(), tau::AllocError> {
        let hart = thread.hart_id();
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
            ptr::addr_of_mut!((*state).interrupt_targets).write([None; 8]);
            ptr::addr_of_mut!((*state).deadlines).write([0; CAPACITY]);
        }
        thread.state.phase = Phase::Running;
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

pub fn notify_hardware_interrupt(window: &mut WindowAccess, scheduler: &Scheduler, hart_id: usize) {
    let mut scheduler = scheduler.lock();
    let Some(target) = scheduler.interrupt_targets.get(hart_id).copied().flatten() else {
        return;
    };
    let event = Event::Notified { arg: 0 };
    if !scheduler.complete_wait(window, target.index, target.physical, event, true) {
        let mut slot = unsafe { target.table.read(window, target.slot) };
        slot.pending = true;
        unsafe { target.table.write(window, target.slot, slot) };
    }
    if scheduler.active {
        scheduler.program();
    }
}

pub fn preempt(window: &mut WindowAccess, thread: &mut Thread, scheduler: &Scheduler) -> Dispatch {
    let mut scheduler = scheduler.lock();
    scheduler.expire(window);
    let id = scheduler.running;
    let thread = &mut thread.state;
    thread.root = Window::current_root();
    thread.resume = Resume::Interrupt;
    if scheduler.length == 0 {
        thread.phase = Phase::Running;
        let dispatch = Dispatch {
            root: thread.root,
            kind: thread.resume,
            message: thread.message,
        };
        scheduler.quantum = now().saturating_add(QUANTUM);
        scheduler.program();
        return dispatch;
    }
    thread.phase = Phase::Runnable;
    scheduler.enqueue(id);
    scheduler.running = 0;
    scheduler
        .pop_running(window)
        .unwrap_or_else(|| invariant_failed())
}

/// Interrupts remain disabled between committing departure and installing the
/// selected root. An idle hart waits with SIE clear and STIE enabled: WFI may
/// wake for a pending timer without entering the user trap entry from S-mode.
pub fn dispatch(window: &mut WindowAccess, scheduler: &Scheduler, hart_id: usize) -> Dispatch {
    loop {
        {
            let mut scheduler = scheduler.lock();
            scheduler.expire(window);
            if scheduler.running != 0 {
                let physical = scheduler.physical(scheduler.running);
                let thread = &mut window.thread_mut(physical).state;
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
            if let Some(dispatch) = scheduler.pop_running(window) {
                return dispatch;
            }
            scheduler.program();
        }
        unsafe { core::arch::asm!("wfi", options(nostack)) };
        if cpu::csrr!("sip") & (1 << 9) != 0 {
            cpu::csrrc!("sie", 1 << 9);
            notify_hardware_interrupt(window, scheduler, hart_id);
        }
    }
}
