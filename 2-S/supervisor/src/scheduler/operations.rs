// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Syscalls change protected wait/scheduling state without switching roots.

use super::{
    csr, sbi,
    thread::{ActiveWait, Phase, Resume, ThreadState},
    runtime, Context, Scheduler,
    matter::{self, Module},
    space::{self, Window},
};

pub fn spawn(
    module: &Module,
    scheduler: &Scheduler,
    entry: usize,
    message: [usize; 6],
) -> Result<(), tau::SpawnError> {
    let mut window = space::access();
    let hart = window.hart().hart_id;
    let mut desc = module.header.descriptor;
    if entry & 1 != 0
        || space::addr_to_index(entry) != space::ROOT_INDEX
        || !window
            .resolve_stem(desc.stem_phys, entry)
            .is_some_and(|(_, flags)| flags & 26 == 26)
    {
        return Err(tau::SpawnError::InvalidEntry);
    }
    desc.entry = entry;
    let current = Window::current_root();
    let binding_stem_phys = window.root_branch(current, space::BINDING_INDEX);
    let root = matter::spawn(desc, binding_stem_phys)?;
    let mut message = message;
    message[0] = 0;
    let thread = ThreadState::runnable(hart, root, message, Resume::Entry(entry));
    let mut scheduler = scheduler.lock();
    if scheduler.count == scheduler.threads.len() {
        drop(scheduler);
        let _ = window.destroy(root);
        window.release_tag(root.asid());
        return Err(tau::SpawnError::NoSlots);
    }
    let id = scheduler.count + 1;
    *scheduler
        .threads
        .get_mut(id - 1)
        .unwrap_or_else(|| runtime::invariant_failed()) = Some(thread);
    unsafe { scheduler.set_deadline(id - 1, 0) };
    scheduler.count = id;
    scheduler.enqueue(hart, id);
    scheduler.enable(hart);
    scheduler.program(hart);
    Ok(())
}

pub fn start_hart(
    module: &Module,
    context: &Context,
    scheduler: &Scheduler,
    hart_id: usize,
) -> Result<(), tau::SpawnError> {
    {
        let scheduler = scheduler.lock();
        if scheduler.hart(hart_id).running != 0 {
            return Err(tau::SpawnError::InvalidHart);
        }
        if scheduler.count == scheduler.threads.len() {
            return Err(tau::SpawnError::NoSlots);
        }
    }
    let mut window = space::access();
    let root = matter::spawn_hart(context, module.header.descriptor, hart_id)?;
    if sbi::hart_start(hart_id, window.hart().base_addr, root.0.get()).is_err() {
        let _ = window.destroy(root);
        return Err(tau::SpawnError::InvalidHart);
    }
    Ok(())
}

pub fn wait(
    module: &Module,
    scheduler: &Scheduler,
    pointer: usize,
    deadline: usize,
) -> Result<Option<tau::Event>, tau::WaitError> {
    let mut window = space::access();
    let mut scheduler = scheduler.lock();
    let root = Window::current_root();
    let table = &module.header.wait_table;
    let index = table.index(pointer)?;
    if !table.occupied(&mut window, root, index)? {
        return Err(tau::WaitError::InvalidWaiter);
    }
    let mut wait_slot = unsafe { module.read_wait(index) };
    if wait_slot.thread_id != 0 {
        return Err(tau::WaitError::Busy);
    }
    if wait_slot.pending != 0 {
        let pending = wait_slot.pending;
        wait_slot.pending = 0;
        unsafe { module.write_wait(index, wait_slot) };
        return Ok(Some(tau::Event(pending)));
    }
    if deadline != 0 && deadline <= runtime::now() {
        return Ok(Some(tau::Event::TIMEOUT));
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
    module: &Module,
    scheduler: &Scheduler,
    pointer: usize,
) -> Result<(), tau::WaitError> {
    let mut window = space::access();
    let mut scheduler = scheduler.lock();
    let hart = window.hart().hart_id;
    let root = Window::current_root();
    let table = &module.header.wait_table;
    let index = table.index(pointer)?;
    if !table.occupied(&mut window, root, index)? {
        return Ok(());
    }
    let mut slot = unsafe { module.read_wait(index) };
    let mut target_hart = hart;
    if slot.thread_id == 0 {
        slot.pending |= 1;
        unsafe { module.write_wait(index, slot) };
    } else {
        let module_addr = module.header.descriptor.physical;
        let thread = scheduler.thread_state_mut(slot.thread_id);
        if let Some(wait) = &thread.wait
            && wait.module == module_addr
            && wait.slot == index
            && wait.sequence == slot.sequence
        {
            target_hart =
                scheduler.complete(module, slot.thread_id, slot, tau::Event::NOTIFIED, false);
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

pub fn exit(module: &Module, scheduler: &Scheduler) {
    let mut window = space::access();
    let hart_id = window.hart().hart_id;
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
