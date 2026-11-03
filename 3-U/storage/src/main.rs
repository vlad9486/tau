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

extern crate alloc;

mod sys;
mod sdio;

use core::mem::MaybeUninit;

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
    mapped_regions: &[tau::MappedRegion {
        dtb_path: "/soc/sdio1",
        ptr: REGISTERS.as_ptr().cast(),
        pages: 1,
    }],
    wait_slots: &WAIT_SLOTS,
};

static WAIT_SLOTS: [tau::WaitSlot; 1] = [tau::WaitSlot::new()];

static REGISTERS: MaybeUninit<sdio::Registers> = MaybeUninit::uninit();

#[cold]
extern "C" fn main(a0: usize, a1: usize, a2: usize, _: usize, _: usize, _: usize) -> ! {
    let regs = unsafe { REGISTERS.assume_init_ref() };
    let arg = a0 as u16;
    match arg {
        0 => {
            let fifo_depth = a1 as u32;
            let freq = a2 as u64;
            match run(regs, fifo_depth, freq) {
                Ok(()) => tau::Ubi::respond(0, []),
                Err(_err) => {
                    // TODO: handle error
                    tau::Ubi::respond(1, [])
                }
            }
        }
        1 => {
            // TODO: enqueue request
            tau::Ubi::respond(0, [])
        }
        _ => tau::Ubi::respond(1, []),
    }
}

fn run(regs: &sdio::Registers, fifo_depth: u32, freq: u64) -> Result<(), sdio::DriverError> {
    use core::sync::atomic::Ordering;

    let mut state = sdio::State::new(freq).ok_or(sdio::DriverError::Dma)?;
    state.init(regs, fifo_depth)?;
    WAIT_SLOTS[0].occupied.store(true, Ordering::SeqCst);
    loop {
        // TODO: create FIFO queue of requests, check the queue here and do
        // `state.read(regs, page, phys)` or corresponding `write`
        let _ = tau::Ubi::wait(&WAIT_SLOTS[0], None);
        let _ = || {
            let _ = state.read(regs, 0x600, 0xdeadbeef);
            let _ = state.write(regs, 0x600, 0xdeadbeef);
        };
    }
}
