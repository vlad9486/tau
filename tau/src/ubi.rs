// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{hint, num::NonZeroUsize};

use super::{
    common::{Call, Entry, Event, WaitError},
    asm,
};

// TODO: investigate it
#[inline(never)]
fn ubi<const I: usize, const O: usize>(call: Call, arg: [usize; I]) -> (usize, [usize; O]) {
    use core::arch;

    let a0 = call.encode();
    let mut output = [0; O];
    let r0;
    match (I, O) {
        (0, 0) => unsafe {
            arch::asm!(
                "ecall",
                inout("a0") a0 => r0,
                options(nostack),
            )
        },
        (0, 1) => unsafe {
            arch::asm!(
                "ecall",
                inout("a0") a0 => r0,
                out("a1") output[0],
                options(nostack),
            )
        },
        (1, 0) => unsafe {
            arch::asm!(
                "ecall",
                inout("a0") a0 => r0,
                in("a1") arg[0],
                options(nostack),
            )
        },
        (2, 0) => unsafe {
            arch::asm!(
                "ecall",
                inout("a0") a0 => r0,
                in("a1") arg[0],
                in("a2") arg[1],
                options(nostack),
            )
        },
        (2, 1) => unsafe {
            arch::asm!(
                "ecall",
                inout("a0") a0 => r0,
                inout("a1") arg[0] => output[0],
                in("a2") arg[1],
                options(nostack),
            )
        },
        (3, 0) => unsafe {
            arch::asm!(
                "ecall",
                inout("a0") a0 => r0,
                in("a1") arg[0],
                in("a2") arg[1],
                in("a3") arg[2],
                options(nostack),
            )
        },
        _ => {
            asm::dbg([I, O, 0xdeadbeef]);
            unsafe { hint::unreachable_unchecked() };
        }
    }

    (r0, output)
}

/// The system interface (User Binary Interface).
pub struct Ubi;

impl Ubi {
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

#[repr(usize)]
#[derive(Debug)]
pub enum AllocError {
    AlreadyAllocated = 1,
    OutOfMemory = 2,
}

impl AllocError {
    /// # Safety
    /// The code must be in range of `Self`
    #[inline]
    pub const unsafe fn decode(code: NonZeroUsize) -> Self {
        match code.get() {
            1 => Self::AlreadyAllocated,
            2 => Self::OutOfMemory,
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
