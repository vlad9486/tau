// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Syscalls change protected wait/scheduling state without switching roots.

use tau::{Event, SpawnError, WaitError};

use super::{
    csr,
    thread::{ActiveWait, Phase, Resume, ThreadState},
    runtime, Context, Scheduler,
    matter::{self, Module},
    space::{Window, WindowAccess},
};

pub fn spawn(
    window: &mut WindowAccess,
    module: &Module,
    context: &Context,
    scheduler: &mut Scheduler,
    entry: usize,
    message: [usize; 6],
) -> Result<u16, SpawnError> {
    let mut scheduler = scheduler.lock();
    if scheduler.count == scheduler.threads.len() {
        return Err(SpawnError::NoSlots);
    }
    let id = scheduler.count + 1;
    let root = matter::spawn(window, module, context, entry)?;
    let mut message = message;
    message[0] = 0;
    let thread = ThreadState::runnable(root, message, Resume::Entry(entry));
    *scheduler
        .threads
        .get_mut(id - 1)
        .unwrap_or_else(|| runtime::invariant_failed()) = Some(thread);
    unsafe { scheduler.set_deadline(id - 1, 0) };
    scheduler.count = id;
    scheduler.enqueue(id);
    scheduler.enable();
    scheduler.program();
    Ok(id as u16)
}

pub fn wait(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &mut Scheduler,
    pointer: usize,
    deadline: usize,
) -> Result<Option<Event>, WaitError> {
    let mut scheduler = scheduler.lock();
    let root = Window::current_root();
    let table = &module.header.wait_table;
    let index = table.index(pointer)?;
    if !table.occupied(window, root, index)? {
        return Err(WaitError::InvalidWaiter);
    }
    let mut slot = unsafe { table.read(window, index) };
    if slot.thread_id != 0 {
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
    let module_addr = module.header.descriptor.physical;
    let running = scheduler.running;
    let hart = window.hart().hart_id;
    let thread = scheduler.thread_state_mut(running);
    // No old timer references remain: deadlines are embedded in active waits.
    thread.sequence = thread.sequence.wrapping_add(1);
    thread.root = root;
    thread.resume = Resume::Syscall;
    thread.phase = Phase::Parking;
    thread.wait = Some(ActiveWait {
        module: module_addr,
        table: table.unsafe_clone(),
        slot: index,
        sequence: thread.sequence,
    });
    slot.thread_id = running;
    slot.sequence = thread.sequence;
    unsafe { table.write(window, index, slot) };
    unsafe { scheduler.set_deadline(running - 1, deadline) };
    if module.header.loader_authority && index == hart {
        if hart < scheduler.interrupt_targets.len() {
            scheduler.interrupt_targets[hart] = Some(runtime::InterruptTarget {
                thread_id: running,
                table: table.unsafe_clone(),
                slot: index,
            });
        }
        csr::rs!("sie", 1 << 9);
    }
    scheduler.enable();
    Ok(None)
}

pub fn notify(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &mut Scheduler,
    pointer: usize,
) -> Result<(), WaitError> {
    let mut scheduler = scheduler.lock();
    let root = Window::current_root();
    let table = &module.header.wait_table;
    let index = table.index(pointer)?;
    if !table.occupied(window, root, index)? {
        return Ok(());
    }
    let mut slot = unsafe { table.read(window, index) };
    if slot.thread_id == 0 {
        slot.pending = true;
        unsafe { table.write(window, index, slot) };
    } else {
        let module_addr = module.header.descriptor.physical;
        let Some(mut thread) = scheduler.take_thread(slot.thread_id) else {
            return Err(WaitError::InvalidWaiter);
        };
        if let Some(wait) = &thread.wait
            && wait.module == module_addr
            && wait.slot == index
            && wait.sequence == slot.sequence
        {
            let wait = unsafe { thread.wait.take().unwrap_unchecked() };
            let event = Event::Notified { arg: 0 };
            scheduler.complete(window, slot.thread_id, thread, wait, slot, event, false);
        } else {
            scheduler.set_thread(slot.thread_id, thread);
        }
    }
    if scheduler.active {
        scheduler.program();
    }
    Ok(())
}

pub fn exit(window: &mut WindowAccess, scheduler: &mut Scheduler, hart_id: usize) {
    let mut scheduler = scheduler.lock();
    let running = scheduler.running;
    unsafe { scheduler.set_deadline(running - 1, 0) };
    if let Some(target) = scheduler.interrupt_targets.get_mut(hart_id)
        && target.as_ref().is_some_and(|t| t.thread_id == running)
    {
        *target = None;
        csr::rc!("sie", 1 << 9);
    }
    let thread = scheduler.thread_state_mut(running);
    if let Some(wait) = &thread.wait {
        let mut slot = unsafe { wait.table.read(window, wait.slot) };
        if slot.thread_id == running && slot.sequence == wait.sequence {
            slot.thread_id = 0;
            slot.sequence = 0;
            unsafe { wait.table.write(window, wait.slot, slot) };
        }
    }
    thread.phase = Phase::Exited;
    thread.wait = None;
    thread.sequence = thread.sequence.wrapping_add(1);
    scheduler.running = 0;
    scheduler.enable();
}
