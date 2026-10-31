// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! This crate will implement storage service that has built-in drivers for some devices,
//! but also ensure ACID-like behavior and tolerance to crash and power outage.

#![no_std]
#![no_main]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner)
)]

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

extern crate alloc;

mod sys;
mod sdio;

#[cfg(not(test))]
#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    tau::Ubi::respond(1, []);
}

#[unsafe(no_mangle)]
static MANIFEST: tau::Manifest = tau::Manifest {
    this: tau::ModuleId {
        version: (0, 1),
        name: *b"storage\0\0\0\0\0\0\0\0\0\0\0\0\0",
    },
    entry: main,
    stack_pages: 0x10,
    dependencies: &[],
    wait_slots: &WAIT_SLOTS,
};

static WAIT_SLOTS: [tau::WaitSlot; 2] = [tau::WaitSlot::new(), tau::WaitSlot::new()];

static STATE: FakeMutex<Option<sdio::State>> = FakeMutex::new(Option::None);

// TODO:
// WARNING: replace with real mutex
struct FakeMutex<T>(UnsafeCell<T>)
where
    T: ?Sized;

impl<T> FakeMutex<T> {
    pub const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    pub fn lock(&self) -> FakeMutexGuard<'_, T> {
        FakeMutexGuard(self)
    }
}

unsafe impl<T> Send for FakeMutex<T> {}

unsafe impl<T> Sync for FakeMutex<T> {}

struct FakeMutexGuard<'a, T>(&'a FakeMutex<T>)
where
    T: 'a + ?Sized;

impl<T: ?Sized> Deref for FakeMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.0.0.get() }
    }
}

impl<T: ?Sized> DerefMut for FakeMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.0.get() }
    }
}

static REGISTERS: MaybeUninit<sdio::Registers> = MaybeUninit::uninit();

#[cold]
extern "C" fn main(a0: usize, a1: usize, a2: usize, _: usize, _: usize, _: usize) -> ! {
    let arg = a0 as u16;
    match arg {
        0 => {
            let fifo_depth = a1 as u32;
            let freq = a2 as u64;
            match sdio::State::new(freq) {
                None => tau::Ubi::respond(1, []),
                Some(mut state) => {
                    match state.init(unsafe { REGISTERS.assume_init_ref() }, fifo_depth) {
                        Err(_err) => {
                            // TODO: log
                        }
                        Ok(()) => {
                            let mut lock = STATE.lock();
                            *lock = Some(state);
                            drop(lock);
                        }
                    }
                    tau::Ubi::respond(0, [])
                }
            }
        }
        1 | 2 => {
            let mut lock = STATE.lock();
            let Some(state) = &mut *lock else {
                drop(lock);
                tau::Ubi::respond(1, [])
            };

            let page = a1 as u32;
            let phys = a2 as u32;
            let res = if arg == 1 {
                state.read(unsafe { REGISTERS.assume_init_ref() }, page, phys)
            } else {
                state.write(unsafe { REGISTERS.assume_init_ref() }, page, phys)
            };
            drop(lock);

            match res {
                Ok(()) => tau::Ubi::respond(0, []),
                Err(_err) => tau::Ubi::respond(1, []),
            }
        }
        _ => tau::Ubi::respond(1, []),
    }
}
