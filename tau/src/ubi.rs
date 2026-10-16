// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{arch, hint, num::NonZeroUsize};

use super::{Call, Entry, Event, WaitError, ModuleError, ModuleHandle};

#[inline(always)]
fn ubi<const I: usize, const O: usize>(call: Call, arg: [usize; I]) -> (usize, [usize; O]) {
    const {
        assert!(I <= 5 && O <= 5);
    }

    let mut words = [0; 5];
    words[..I].copy_from_slice(&arg);
    let status;
    unsafe {
        arch::asm!(
            "ecall",
            inout("a0") call.encode() => status,
            inout("a1") words[0],
            inout("a2") words[1],
            inout("a3") words[2],
            inout("a4") words[3],
            inout("a5") words[4],
            clobber_abi("C"),
            options(nostack),
        );
    }
    let mut output = [0; O];
    output.copy_from_slice(&words[..O]);
    (status, output)
}

/// The system interface (User Binary Interface).
pub struct Ubi;

impl Ubi {
    /// Prepare a fresh module instance and attach it to the specified parent.
    /// Return the parent's dependency slot and the new module's handle.
    /// Only the trusted bootstrap loader may call this. It must supply issued
    /// handles and finish resolving dependencies before invoking the module.
    pub fn prepare_module(
        parent_module_id: ModuleHandle,
        image: &[u8],
    ) -> Result<(u16, ModuleHandle), ModuleError> {
        let (status, [slot, handle]) = ubi(
            Call::PrepareModule,
            [
                image.as_ptr().addr(),
                image.len(),
                parent_module_id.as_raw(),
            ],
        );
        module_status(status)?;
        Ok((slot as u16, ModuleHandle::from_raw(handle)))
    }

    /// Attach an existing module instance without copying its ELF or globals.
    /// Each call allocates a fresh parent-local slot. ROOT is valid only as a
    /// parent. The trusted loader must validate that this edge creates no cycle.
    pub fn bind_dependency(
        parent_module_id: ModuleHandle,
        dependency_module_id: ModuleHandle,
    ) -> Result<u16, ModuleError> {
        let (status, [slot]) = ubi(
            Call::BindDependency,
            [parent_module_id.as_raw(), dependency_module_id.as_raw()],
        );
        module_status(status)?;
        Ok(slot as u16)
    }

    pub fn invoke<const I: usize, const O: usize>(
        slot: u16,
        arg: u16,
        msg: [usize; I],
    ) -> Result<[usize; O], NonZeroUsize> {
        let (r0, msg) = ubi(Call::Invoke { slot, arg }, msg);
        if let Some(error) = NonZeroUsize::new(r0) {
            Err(error)
        } else {
            Ok(msg)
        }
    }

    pub fn respond<const O: usize>(code: u16, msg: [usize; O]) -> ! {
        let (_, []) = ubi(Call::Respond { code }, msg);
        unsafe { hint::unreachable_unchecked() }
    }

    pub fn spawn<const I: usize>(entry: Entry, msg: [usize; I]) -> u16 {
        let entry = entry as usize;
        let (r0, []) = ubi(Call::Spawn { entry }, msg);
        r0 as u16
    }

    pub fn exit<const O: usize>(msg: [usize; O]) -> ! {
        let (_, []) = ubi(Call::Exit, msg);
        unsafe { hint::unreachable_unchecked() }
    }

    pub fn join<const O: usize>(thread_id: u16) -> Result<[usize; O], NonZeroUsize> {
        let (r0, msg) = ubi(Call::Join { thread_id }, []);
        if let Some(error) = NonZeroUsize::new(r0) {
            Err(error)
        } else {
            Ok(msg)
        }
    }

    pub fn map(
        physical_addr: Option<NonZeroUsize>,
        virtual_addr: usize,
        number_of_pages: usize,
    ) -> Result<(), AllocError> {
        let physical_addr = physical_addr.map(NonZeroUsize::get).unwrap_or_default();
        let (r0, []) = ubi(Call::Map, [physical_addr, virtual_addr, number_of_pages]);
        if let Some(code) = NonZeroUsize::new(r0) {
            Err(unsafe { AllocError::decode(code) })
        } else {
            Ok(())
        }
    }

    pub fn unmap(virtual_addr: usize, number_of_pages: usize) -> Result<(), FreeError> {
        let (r0, []) = ubi(Call::Unmap, [virtual_addr, number_of_pages]);
        if let Some(code) = NonZeroUsize::new(r0) {
            Err(unsafe { FreeError::decode(code) })
        } else {
            Ok(())
        }
    }

    /// Bootstrap-only interrupt wait.
    /// TODO: unify with ordinary `wait`.
    pub fn wait_external(deadline: Option<NonZeroUsize>) -> Event {
        let t = deadline.map(NonZeroUsize::get).unwrap_or_default();
        let (a0, []) = ubi(Call::WaitExternal, [t]);
        // supervisor will not send invalid value
        unsafe { Event::decode(a0).unwrap_unchecked() }
    }

    /// Consume readiness or suspend this thread, resuming here in this context.
    /// Shared-state publication/checking still needs module synchronization.
    pub fn wait(deadline: Option<NonZeroUsize>) -> Result<Event, WaitError> {
        let deadline = deadline.map(NonZeroUsize::get).unwrap_or_default();
        let (status, [event]) = ubi(Call::Wait, [deadline]);
        wait_status(status)?;
        Event::decode(event).map_err(|_| WaitError::Unsupported)
    }

    /// Record coalesced readiness for a waiter in this module. Does not transfer
    /// this thread or execute code in the receiver's context.
    /// `waiter` is thread id, TODO: define thread id
    pub fn notify(waiter: usize) -> Result<(), WaitError> {
        let (status, []) = ubi(Call::Notify, [waiter]);
        wait_status(status)
    }

    /// Print a diagnostic checkpoint and one machine-word value through the
    /// supervisor's SBI debug console.
    pub fn debug(checkpoint: usize, value: usize) {
        let (_, []) = ubi(Call::Debug, [checkpoint, value]);
    }
}

fn module_status(status: usize) -> Result<(), ModuleError> {
    match status {
        0 => Ok(()),
        1 => Err(ModuleError::AccessDenied),
        3 => Err(ModuleError::OutOfMemory),
        4 => Err(ModuleError::NoSlots),
        5 => Err(ModuleError::InvalidDependency),
        _ => Err(ModuleError::InvalidImage),
    }
}

#[repr(usize)]
#[derive(Debug)]
pub enum AllocError {
    AlreadyAllocated = 1,
    OutOfMemory = 2,
    AccessDenied = 3,
    InvalidAddress = 4,
}

impl AllocError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZeroUsize) -> Self {
        match code.get() {
            1 => Self::AlreadyAllocated,
            2 => Self::OutOfMemory,
            3 => Self::AccessDenied,
            4 => Self::InvalidAddress,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

#[repr(usize)]
#[derive(Debug)]
pub enum FreeError {
    AlreadyFree = 1,
}

impl FreeError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZeroUsize) -> Self {
        match code.get() {
            1 => Self::AlreadyFree,
            _ => unsafe { hint::unreachable_unchecked() },
        }
    }
}

fn wait_status(status: usize) -> Result<(), WaitError> {
    match status {
        0 => Ok(()),
        1 => Err(WaitError::Unsupported),
        2 => Err(WaitError::InvalidWaiter),
        3 => Err(WaitError::AccessDenied),
        4 => Err(WaitError::Busy),
        5 => Err(WaitError::OutOfMemory),
        _ => Err(WaitError::Unsupported),
    }
}
