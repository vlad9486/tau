// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tester::test_runner)
)]

pub mod tester;

pub mod asm;

mod common;
pub use self::common::{
    Call, Entry, Event, Manifest, RawManifest, MemoryZone, ModuleHandle, ModuleId, PageFlags,
    ModuleCapabilities, WaitSlot, MappedRegion, BINDING_BASE,
};

pub mod error;
pub use self::error::{SpawnError, ModuleError, WaitError, AllocError, FreeError};

mod ubi;
pub use self::ubi::Ubi;

mod heap;
pub use self::heap::Area;

mod register;
pub use self::register::Register;

pub fn to_size<T>(v: T) -> usize
where
    T: Into<u32>,
{
    #[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
    compile_error!("Tau requires 32- or 64-bit targets");

    unsafe { usize::try_from(v.into()).unwrap_unchecked() }
}

#[derive(Debug)]
pub struct Timeout;

pub fn spin(mut timeout: u32, cond: impl Fn() -> bool) -> Result<(), Timeout> {
    while !cond() {
        core::hint::spin_loop();
        timeout -= 1;
        if timeout == 0 {
            return Err(Timeout);
        }
    }

    Ok(())
}
