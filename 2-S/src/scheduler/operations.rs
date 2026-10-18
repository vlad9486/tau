// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Syscalls change protected wait/scheduling state without switching roots.

use tau::{Event, SpawnError, WaitError};

use super::{
    thread::{ActiveWait, Phase, Resume},
    runtime, cpu, Context, Thread, Scheduler, loading,
    vmem::{self, Window, WindowAccess},
    module::ModuleTables,
    waiting::WaitTable,
};

pub fn spawn(
    window: &mut WindowAccess,
    thread: &Thread,
    module: &ModuleTables,
    context: &Context,
    scheduler: &Scheduler,
    entry: usize,
    message: [usize; 6],
) -> Result<u16, SpawnError> {
    let mut scheduler = scheduler.lock();
    if scheduler.count == scheduler.registry.len() {
        return Err(SpawnError::NoSlots);
    }
    let id = scheduler.count + 1;
    let physical = loading::spawn(window, thread, module, context, entry, id, message)?;
    *scheduler
        .registry
        .get_mut(id - 1)
        .unwrap_or_else(|| runtime::invariant_failed()) = physical;
    unsafe { scheduler.set_deadline(id - 1, 0) };
    scheduler.count = id;
    scheduler.enqueue(id);
    scheduler.enable();
    scheduler.program();
    Ok(id as u16)
}

pub fn wait(
    window: &mut WindowAccess,
    module: &ModuleTables,
    scheduler: &Scheduler,
    pointer: usize,
    deadline: usize,
) -> Result<Option<Event>, WaitError> {
    let mut scheduler = scheduler.lock();
    let root = Window::current_root();
    let table = module.wait_table;
    let index = table.index(pointer)?;
    if !table.occupied(window, root, index)? {
        return Err(WaitError::InvalidWaiter);
    }
    let mut slot = unsafe { table.read(window, index) };
    if slot.thread != 0 {
        return Err(WaitError::Busy);
    }
    if slot.pending {
        slot.pending = false;
        unsafe { table.write(window, index, slot) };
        return Ok(Some(Event::Notified { arg: 0 }));
    }
    if deadline != 0 && deadline <= runtime::now() {
        return Ok(Some(Event::Timeout));
    }
    let module_addr = window
        .resolve(root, vmem::MODULE)
        .ok_or(WaitError::InvalidWaiter)?
        .0;
    let running = scheduler.running;
    let physical = scheduler.physical(running);
    let (hart, mut thread) = {
        let thread_ref = window.thread(physical);
        (thread_ref.hart_id(), thread_ref.state)
    };
    // No old timer references remain: deadlines are embedded in active waits.
    thread.sequence = thread.sequence.wrapping_add(1);
    thread.root = root;
    thread.resume = Resume::Syscall;
    thread.phase = Phase::Parking;
    thread.wait = Some(ActiveWait {
        module: module_addr,
        table,
        slot: index,
        sequence: thread.sequence,
    });
    slot.thread = physical;
    slot.sequence = thread.sequence;
    unsafe { table.write(window, index, slot) };
    window.thread_mut(physical).state = thread;
    unsafe { scheduler.set_deadline(running - 1, deadline) };
    if module.loader_authority && index == hart {
        if hart < scheduler.interrupt_targets.len() {
            scheduler.interrupt_targets[hart] = Some(runtime::InterruptTarget {
                physical,
                table,
                slot: index,
            });
        }
        cpu::csrrs!("sie", 1 << 9);
    }
    scheduler.enable();
    Ok(None)
}

pub fn notify(
    window: &mut WindowAccess,
    scheduler: &Scheduler,
    table: WaitTable,
    pointer: usize,
) -> Result<(), WaitError> {
    let mut scheduler = scheduler.lock();
    let root = Window::current_root();
    let index = table.index(pointer)?;
    if !table.occupied(window, root, index)? {
        return Ok(());
    }
    let mut slot = unsafe { table.read(window, index) };
    if slot.thread == 0 {
        slot.pending = true;
        unsafe { table.write(window, index, slot) };
    } else {
        // Resolve only a live, protected registry entry before touching backing.
        if !scheduler
            .registry
            .iter()
            .take(scheduler.count)
            .any(|&p| p == slot.thread)
        {
            return Err(WaitError::InvalidWaiter);
        }
        let module = window
            .resolve(root, vmem::MODULE)
            .ok_or(WaitError::InvalidWaiter)?
            .0;
        if let Some(wait) = window.thread(slot.thread).state.wait
            && wait.module == module
            && wait.slot == index
            && wait.sequence == slot.sequence
        {
            scheduler.complete(window, slot.thread, wait, Event::Notified { arg: 0 }, false);
        }
    }
    if scheduler.active {
        scheduler.program();
    }
    Ok(())
}

pub fn exit(window: &mut WindowAccess, scheduler: &Scheduler) {
    let mut scheduler = scheduler.lock();
    let running = scheduler.running;
    let physical = scheduler.physical(running);
    unsafe { scheduler.set_deadline(running - 1, 0) };
    let (hart, mut thread) = {
        let thread_ref = window.thread(physical);
        (thread_ref.hart_id(), thread_ref.state)
    };
    if let Some(target) = scheduler.interrupt_targets.get_mut(hart)
        && target.as_ref().is_some_and(|t| t.physical == physical)
    {
        *target = None;
        cpu::csrrc!("sie", 1 << 9);
    }
    if let Some(wait) = thread.wait {
        let mut slot = unsafe { wait.table.read(window, wait.slot) };
        if slot.thread == physical && slot.sequence == wait.sequence {
            slot.thread = 0;
            slot.sequence = 0;
            unsafe { wait.table.write(window, wait.slot, slot) };
        }
    }
    thread.phase = Phase::Exited;
    thread.wait = None;
    thread.sequence = thread.sequence.wrapping_add(1);
    window.thread_mut(physical).state = thread;
    scheduler.running = 0;
    scheduler.enable();
}
