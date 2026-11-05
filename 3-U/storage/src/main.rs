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
mod queue;

use core::mem::MaybeUninit;

use self::queue::Queue;

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

enum Request {
    Read { page: u32, phys: u32 },
    Write { page: u32, phys: u32 },
}

static QUEUE: Queue<Request, 16> = Queue::new();

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
        1 | 2 => {
            let page = a1 as u32;
            let phys = a2 as u32;
            let req = if arg == 1 {
                Request::Read { page, phys }
            } else {
                Request::Write { page, phys }
            };
            match QUEUE.enqueue(req) {
                Ok(()) => {
                    // TODO: raise software irq that will trigger `op = 3`
                    tau::Ubi::respond(0, [])
                }
                Err(_) => tau::Ubi::respond(1, []),
            }
        }
        _ => tau::Ubi::respond(1, []),
    }
}

fn run(regs: &sdio::Registers, fifo_depth: u32, freq: u64) -> Result<(), sdio::DriverError> {
    let mut state = sdio::State::new(freq).ok_or(sdio::DriverError::Dma)?;
    state.init(regs, fifo_depth)?;

    let mut done = 1;
    loop {
        // [1, 0] signals to system that init / the previous request is complete
        let Ok([op, page, phys]) = tau::Ubi::respond_keep(0, [done, 0]) else {
            done = 0;
            continue;
        };
        match op {
            1 => {
                state.read(regs, page as u32, phys as u32)?;
                done = 1;
            }
            2 => {
                state.write(regs, page as u32, phys as u32)?;
                done = 1;
            }
            3 => {
                while let Some(req) = QUEUE.dequeue() {
                    match req {
                        Request::Read { page, phys } => state.read(regs, page, phys)?,
                        Request::Write { page, phys } => state.write(regs, page, phys)?,
                    }
                }
            }
            _ => {
                regs.clear_interrupts();
                done = 0;
            }
        }
    }
}
