// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Syscalls change protected wait/scheduling state without switching roots.

use tau::{Event, SpawnError, WaitError};

use super::{
    csr, sbi,
    thread::{ActiveWait, Phase, Resume, ThreadState},
    runtime, Context, Scheduler,
    matter::{self, Module},
    space::{Window, WindowAccess},
};

pub fn spawn(
    window: &mut WindowAccess,
    module: &Module,
    context: &Context,
    scheduler: &Scheduler,
    entry: usize,
    message: [usize; 6],
) -> Result<u16, SpawnError> {
    let mut scheduler = scheduler.lock();
    if scheduler.count == scheduler.threads.len() {
        return Err(SpawnError::NoSlots);
    }
    let id = scheduler.count + 1;
    let hart = window.hart().hart_id;
    let mut desc = module.header.descriptor;
    desc.entry = entry;
    let root = matter::spawn(window, context, desc)?;
    let mut message = message;
    message[0] = 0;
    let thread = ThreadState::runnable(hart, root, message, Resume::Entry(entry));
    *scheduler
        .threads
        .get_mut(id - 1)
        .unwrap_or_else(|| runtime::invariant_failed()) = Some(thread);
    unsafe { scheduler.set_deadline(id - 1, 0) };
    scheduler.count = id;
    scheduler.enqueue(hart, id);
    scheduler.enable(hart);
    scheduler.program(hart);
    Ok(id as u16)
}

pub fn start_hart(
    window: &mut WindowAccess,
    module: &Module,
    context: &Context,
    scheduler: &Scheduler,
    hart_id: usize,
) -> Result<(), SpawnError> {
    if !module.header.caps.loader {
        return Err(SpawnError::AccessDenied);
    }
    if hart_id >= runtime::MAX_HARTS {
        return Err(SpawnError::InvalidHart);
    }
    {
        let scheduler = scheduler.lock();
        if scheduler.hart(hart_id).running != 0 {
            return Err(SpawnError::InvalidHart);
        }
        if scheduler.count == scheduler.threads.len() {
            return Err(SpawnError::NoSlots);
        }
    }
    let root = matter::spawn_hart(window, context, module.header.descriptor)?;
    if sbi::hart_start(hart_id, context.base_addr, root.0.get()).is_err() {
        let local_hart = window.hart().hart_id;
        let _ = window.destroy(root, |page| context.free(local_hart, page));
        return Err(SpawnError::InvalidHart);
    }
    Ok(())
}

pub fn wait(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &Scheduler,
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
    let mut wait_slot = unsafe { module.read_wait(index) };
    if wait_slot.thread_id != 0 {
        return Err(WaitError::Busy);
    }
    if wait_slot.pending != 0 {
        let pending = wait_slot.pending;
        wait_slot.pending = 0;
        unsafe { module.write_wait(index, wait_slot) };
        return Ok(Some(Event(pending)));
    }
    if deadline != 0 && deadline <= runtime::now() {
        return Ok(Some(Event::TIMEOUT));
    }
    let module_addr = module.header.descriptor.physical;
    let hart = window.hart().hart_id;
    let running = scheduler.running(hart);
    let thread = scheduler.thread_state_mut(running);
    // No old timer references remain: deadlines are embedded in active waits.
    thread.sequence = thread.sequence.wrapping_add(1);
    thread.root = root;
    thread.resume = Resume::Syscall;
    thread.phase = Phase::Parking;
    thread.wait = Some(ActiveWait {
        module: module_addr,
        slot: index,
        sequence: thread.sequence,
    });
    wait_slot.thread_id = running;
    wait_slot.sequence = thread.sequence;
    unsafe { module.write_wait(index, wait_slot) };
    unsafe { scheduler.set_deadline(running - 1, deadline) };
    if module.header.caps.loader && index == hart {
        scheduler.hart_mut(hart).interrupt_target = Some(runtime::InterruptTarget {
            module: module_addr,
            thread_id: running,
            slot: index,
        });
        csr::rs!("sie", 1 << 9);
    }
    scheduler.enable(hart);
    Ok(None)
}

pub fn notify(
    window: &mut WindowAccess,
    module: &Module,
    scheduler: &Scheduler,
    pointer: usize,
) -> Result<(), WaitError> {
    let mut scheduler = scheduler.lock();
    let hart = window.hart().hart_id;
    let root = Window::current_root();
    let table = &module.header.wait_table;
    let index = table.index(pointer)?;
    if !table.occupied(window, root, index)? {
        return Ok(());
    }
    let mut slot = unsafe { module.read_wait(index) };
    let mut target_hart = hart;
    if slot.thread_id == 0 {
        slot.pending |= 1;
        unsafe { module.write_wait(index, slot) };
    } else {
        let module_addr = module.header.descriptor.physical;
        let Some(thread) = scheduler.take_thread(slot.thread_id) else {
            return Err(WaitError::InvalidWaiter);
        };
        if let Some(wait) = &thread.wait
            && wait.module == module_addr
            && wait.slot == index
            && wait.sequence == slot.sequence
        {
            target_hart = thread.hart;
            let event = Event::NOTIFIED;
            scheduler.complete(module, slot.thread_id, thread, slot, event, false);
        } else {
            scheduler.set_thread(slot.thread_id, thread);
        }
    }
    if target_hart == hart {
        if scheduler.hart(hart).active {
            scheduler.program(hart);
        }
    } else if scheduler.hart(target_hart).active {
        let _ = sbi::wake_hart(target_hart);
    }
    Ok(())
}

pub fn exit(window: &mut WindowAccess, module: &Module, scheduler: &Scheduler, hart_id: usize) {
    let mut scheduler = scheduler.lock();
    let running = scheduler.running(hart_id);
    unsafe { scheduler.set_deadline(running - 1, 0) };
    let hart_state = scheduler.hart_mut(hart_id);
    if hart_state
        .interrupt_target
        .as_ref()
        .is_some_and(|t| t.thread_id == running)
    {
        hart_state.interrupt_target = None;
        csr::rc!("sie", 1 << 9);
    }
    let thread = scheduler.thread_state_mut(running);
    if let Some(wait) = &thread.wait {
        let target_module = if wait.module == module.header.descriptor.physical {
            module
        } else {
            &*window.map_module(wait.module)
        };
        let mut slot = unsafe { target_module.read_wait(wait.slot) };
        if slot.thread_id == running && slot.sequence == wait.sequence {
            slot.thread_id = 0;
            slot.sequence = 0;
            unsafe { target_module.write_wait(wait.slot, slot) };
        }
    }
    thread.phase = Phase::Exited;
    thread.wait = None;
    thread.sequence = thread.sequence.wrapping_add(1);
    scheduler.set_running(hart_id, 0);
    scheduler.enable(hart_id);
}
