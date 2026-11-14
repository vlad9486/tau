// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{
    arch,
    cell::UnsafeCell,
    hint,
    ops::{Deref, DerefMut},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};

use tau::Event;

use super::{
    sbi, csr,
    thread::{Phase, Resume, ThreadState},
    matter::{WaitRecord, Module},
    space::{self, Mapping, Root, Window, WindowAccess},
};

const CAPACITY: usize = Module::WAIT_SIZE;
pub(super) const MAX_HARTS: usize = 64;
// Raw time-counter ticks, deliberately independent of wall-clock time.
const QUANTUM: usize = 100_000;

#[repr(C)]
pub struct Scheduler {
    lock: AtomicBool,
    pub(super) state: UnsafeCell<State>,
}

pub struct InterruptTarget {
    pub module: usize,
    pub thread_id: usize,
    pub slot: usize,
}

pub(super) struct HartState {
    pub(super) queue: [u16; CAPACITY],
    pub(super) head: usize,
    pub(super) length: usize,
    pub(super) running: usize,
    pub(super) active: bool,
    pub(super) quantum: usize,
    pub(super) interrupt_target: Option<InterruptTarget>,
}

impl HartState {
    const EMPTY: Self = Self {
        queue: [0; CAPACITY],
        head: 0,
        length: 0,
        running: 0,
        active: false,
        quantum: 0,
        interrupt_target: None,
    };
}

pub(super) struct State {
    pub(super) threads: [Option<ThreadState>; CAPACITY],
    pub(super) count: usize,
    pub(super) harts: [HartState; MAX_HARTS],
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
            csr::rs!("sstatus", 2);
        }
    }
}

impl Scheduler {
    pub(super) fn lock(&self) -> Guard<'_> {
        let interrupts: usize;
        unsafe {
            arch::asm!("csrrc {0}, sstatus, {1}", out(reg) interrupts, in(reg) 2usize, options(nostack))
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

    pub(super) fn hart(&self, hart_id: usize) -> &HartState {
        self.harts
            .get(hart_id)
            .unwrap_or_else(|| invariant_failed())
    }

    pub(super) fn hart_mut(&mut self, hart_id: usize) -> &mut HartState {
        self.harts
            .get_mut(hart_id)
            .unwrap_or_else(|| invariant_failed())
    }

    pub(super) fn running(&self, hart_id: usize) -> usize {
        self.hart(hart_id).running
    }

    pub(super) fn set_running(&mut self, hart_id: usize, id: usize) {
        self.hart_mut(hart_id).running = id;
    }

    pub(super) fn enqueue(&mut self, hart_id: usize, id: usize) {
        let hart = self.hart_mut(hart_id);
        if hart.length >= CAPACITY {
            invariant_failed();
        }
        hart.queue[(hart.head + hart.length) % CAPACITY] = id as u16;
        hart.length += 1;
    }

    pub(super) fn push_front(&mut self, hart_id: usize, id: usize) {
        let hart = self.hart_mut(hart_id);
        if hart.length >= CAPACITY {
            invariant_failed();
        }
        hart.head = (hart.head + CAPACITY - 1) % CAPACITY;
        hart.queue[hart.head] = id as u16;
        hart.length += 1;
    }

    fn pop(&mut self, hart_id: usize) -> Option<usize> {
        let hart = self.hart_mut(hart_id);
        if hart.length == 0 {
            return None;
        }
        let id = hart
            .queue
            .get(hart.head)
            .copied()
            .unwrap_or_else(|| invariant_failed())
            .into();
        hart.head = (hart.head + 1) % CAPACITY;
        hart.length -= 1;
        Some(id)
    }

    pub(super) fn enable(&mut self, hart_id: usize) {
        let hart = self.hart_mut(hart_id);
        if !hart.active {
            hart.active = true;
            csr::rs!("sie", 1 << 5);
            hart.quantum = now().saturating_add(QUANTUM);
        }
    }

    pub(super) fn program(&self, hart_id: usize) {
        let hart = self.hart(hart_id);
        let mut next = if hart.running != 0 && hart.length != 0 {
            hart.quantum
        } else {
            usize::MAX
        };
        for (index, &deadline) in self.deadlines.iter().take(self.count).enumerate() {
            if deadline != 0
                && self
                    .threads
                    .get(index)
                    .and_then(Option::as_ref)
                    .is_some_and(|t| t.hart == hart_id)
            {
                next = next.min(deadline);
            }
        }
        sbi::set_timer(next).unwrap_or_default();
    }

    pub(super) fn complete(
        &mut self,
        module: &Module,
        thread_id: usize,
        mut slot: WaitRecord,
        event: Event,
        high_priority: bool,
    ) -> usize {
        slot.thread_id = 0;
        slot.sequence = 0;
        let thread = self.thread_state_mut(thread_id);
        let hart_id = thread.hart;
        let wait = unsafe { thread.wait.take().unwrap_unchecked() };
        unsafe { module.write_wait(wait.slot, slot) };
        let enqueue = thread.complete([0, event.0 as usize, 0, 0, 0, 0]);
        unsafe { self.set_deadline(thread_id - 1, 0) };
        if enqueue {
            if high_priority {
                self.push_front(hart_id, thread_id);
            } else {
                self.enqueue(hart_id, thread_id);
            }
        }
        hart_id
    }

    fn complete_wait(
        &mut self,
        window: &mut WindowAccess,
        module: &Module,
        thread_id: usize,
        event: Event,
        high_priority: bool,
    ) -> bool {
        let Some(&wait) = self.thread_state_mut(thread_id).wait.as_ref() else {
            return false;
        };
        let target_module = if wait.module == module.header.descriptor.physical {
            module
        } else {
            &*window.map_module(wait.module)
        };
        let slot = unsafe { target_module.read_wait(wait.slot) };
        if slot.thread_id == thread_id && slot.sequence == wait.sequence {
            self.complete(target_module, thread_id, slot, event, high_priority);
            true
        } else {
            false
        }
    }

    fn expire(&mut self, window: &mut WindowAccess, module: &Module, hart_id: usize) {
        let time = now();
        for index in 0..self.count {
            let deadline = unsafe { self.deadline(index) };
            if deadline != 0
                && deadline <= time
                && self
                    .threads
                    .get(index)
                    .and_then(Option::as_ref)
                    .is_some_and(|t| t.hart == hart_id)
            {
                self.complete_wait(window, module, index + 1, Event::TIMEOUT, false);
            }
        }
    }

    fn pop_running(&mut self, hart_id: usize) -> Option<Dispatch> {
        let id = self.pop(hart_id)?;
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
        let hart = self.hart_mut(hart_id);
        hart.running = id;
        hart.quantum = now().saturating_add(QUANTUM);
        self.program(hart_id);
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
    pub unsafe fn init(this: *mut Self, mut thread: ThreadState) -> Result<(), tau::AllocError> {
        space::access()
            .map(
                Window::current_root(),
                Mapping {
                    range: space::MappingRange {
                        addr: space::SCHEDULER,
                        pages: size_of::<Scheduler>().div_ceil(space::PAGE),
                    },
                    physical_addr: None,
                    flags: *b"rw--g",
                },
            )
            .map_err(|_| tau::AllocError::OutOfMemory)?;
        let hart_id = thread.hart;
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
            let harts = ptr::addr_of_mut!((*state).harts).cast::<HartState>();
            for i in 0..MAX_HARTS {
                harts.add(i).write(HartState::EMPTY);
            }
            if hart_id >= MAX_HARTS {
                invariant_failed();
            }
            ptr::addr_of_mut!((*harts.add(hart_id)).running).write(1);
            ptr::addr_of_mut!((*state).deadlines).write([0; CAPACITY]);
        }
        Ok(())
    }

    pub fn init_hart(&self, mut thread: ThreadState) -> Result<(), tau::SpawnError> {
        let mut scheduler = self.lock();
        let hart_id = thread.hart;
        if hart_id >= MAX_HARTS || scheduler.hart(hart_id).running != 0 {
            return Err(tau::SpawnError::InvalidHart);
        }
        if scheduler.count == CAPACITY {
            return Err(tau::SpawnError::NoSlots);
        }
        let id = scheduler.count + 1;
        thread.phase = Phase::Running;
        scheduler.set_thread(id, thread);
        unsafe { scheduler.set_deadline(id - 1, 0) };
        scheduler.count = id;
        scheduler.set_running(hart_id, id);
        Ok(())
    }

    pub fn active(&self, hart_id: usize) -> bool {
        self.lock().hart(hart_id).active
    }
}

pub struct Dispatch {
    pub root: Root,
    pub kind: super::ReturnKind,
    pub message: [usize; 6],
}

pub fn notify_interrupt(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &Scheduler,
    current_hart: usize,
    target_hart: usize,
    bit: u8,
) {
    let mut scheduler = scheduler.lock();
    let Some(target) = &scheduler.hart(target_hart).interrupt_target else {
        return;
    };
    let target_module_addr = target.module;
    let target_slot = target.slot;
    let thread_id = target.thread_id;

    let mask = 1u64 << bit;
    let event = Event(mask);
    if !scheduler.complete_wait(window, module, thread_id, event, true) {
        let target_module = if target_module_addr == module.header.descriptor.physical {
            module
        } else {
            &*window.map_module(target_module_addr)
        };
        let mut slot = unsafe { target_module.read_wait(target_slot) };
        slot.pending |= mask;
        unsafe { target_module.write_wait(target_slot, slot) };
    }
    if target_hart == current_hart {
        if scheduler.hart(current_hart).active {
            scheduler.program(current_hart);
        }
    } else if scheduler.hart(target_hart).active {
        let _ = sbi::wake_hart(target_hart);
    }
}

pub fn notify_hardware_interrupt(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &Scheduler,
    hart_id: usize,
) {
    notify_interrupt(window, module, scheduler, hart_id, hart_id, 0);
}

pub fn preempt(window: &mut WindowAccess, module: &Module, scheduler: &Scheduler) -> Dispatch {
    let mut scheduler = scheduler.lock();
    let hart_id = window.hart().hart_id;
    scheduler.expire(window, module, hart_id);
    let length = scheduler.hart(hart_id).length;
    let id = scheduler.running(hart_id);
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
        scheduler.hart_mut(hart_id).quantum = now().saturating_add(QUANTUM);
        scheduler.program(hart_id);
        dispatch
    } else {
        thread.phase = Phase::Runnable;
        scheduler.enqueue(hart_id, id);
        scheduler
            .pop_running(hart_id)
            .unwrap_or_else(|| invariant_failed())
    }
}

/// Interrupts remain disabled between committing departure and installing the
/// selected root. An idle hart waits with SIE clear and STIE enabled: WFI may
/// wake for a pending timer without entering the user trap entry from S-mode.
pub fn dispatch(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &Scheduler,
    hart_id: usize,
) -> Dispatch {
    loop {
        {
            let mut scheduler = scheduler.lock();
            scheduler.expire(window, module, hart_id);
            let running = scheduler.running(hart_id);
            if running != 0 {
                let thread = scheduler.thread_state_mut(running);
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
                    scheduler.program(hart_id);
                    return dispatch;
                }
                scheduler.set_running(hart_id, 0);
            }
            if let Some(dispatch) = scheduler.pop_running(hart_id) {
                return dispatch;
            }
            let mut hart_alive = false;
            let mut system_alive = false;
            for thread in scheduler.threads.iter().take(scheduler.count).flatten() {
                if thread.phase != Phase::Exited {
                    system_alive = true;
                    if thread.hart == hart_id {
                        hart_alive = true;
                        break;
                    }
                }
            }
            if !hart_alive {
                scheduler.hart_mut(hart_id).active = false;
                sbi::set_timer(usize::MAX).unwrap_or_default();
                drop(scheduler);
                if !system_alive {
                    sbi::system_reset();
                }
                let _ = sbi::hart_stop();
                loop {
                    hint::spin_loop();
                }
            }
            scheduler.program(hart_id);
        }
        unsafe { arch::asm!("wfi", options(nostack)) };
        if csr::r!("sip") & (1 << 9) != 0 {
            csr::rc!("sie", 1 << 9);
            notify_hardware_interrupt(window, module, scheduler, hart_id);
        }
    }
}
