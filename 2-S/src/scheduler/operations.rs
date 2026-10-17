// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Syscalls change protected wait/scheduling state without switching roots.

use tau::{Event, SpawnError, WaitError};

use super::{
    thread::{ActiveWait, Phase, Resume},
    runtime, Context, Thread, Scheduler, loading,
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
    scheduler.count = id;
    scheduler.enqueue(id);
    scheduler.enable();
    scheduler.program(window);
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
    let module = window
        .resolve(root, vmem::MODULE)
        .ok_or(WaitError::InvalidWaiter)?
        .0;
    let physical = scheduler.physical(scheduler.running);
    let mut thread = unsafe { Thread::read(window, physical) };
    // No old timer references remain: deadlines are embedded in active waits.
    thread.sequence = thread.sequence.wrapping_add(1);
    thread.root = root;
    thread.resume = Resume::Syscall;
    thread.phase = Phase::Parking;
    thread.wait = Some(ActiveWait {
        module,
        table,
        slot: index,
        sequence: thread.sequence,
        deadline,
    });
    slot.thread = physical;
    slot.sequence = thread.sequence;
    unsafe {
        table.write(window, index, slot);
        Thread::write(window, physical, thread);
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
        let thread = unsafe { Thread::read(window, slot.thread) };
        let module = window
            .resolve(root, vmem::MODULE)
            .ok_or(WaitError::InvalidWaiter)?
            .0;
        if let Some(wait) = thread.wait
            && wait.module == module
            && wait.slot == index
            && wait.sequence == slot.sequence
        {
            scheduler.complete(window, slot.thread, wait, Event::Notified { arg: 0 });
        }
    }
    if scheduler.active {
        scheduler.program(window);
    }
    Ok(())
}

pub fn exit(window: &mut WindowAccess, scheduler: &Scheduler) {
    let mut scheduler = scheduler.lock();
    let physical = scheduler.physical(scheduler.running);
    let mut thread = unsafe { Thread::read(window, physical) };
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
    unsafe { Thread::write(window, physical, thread) };
    scheduler.running = 0;
    scheduler.enable();
}
