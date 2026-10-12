// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

#[unsafe(no_mangle)]
static MANIFEST: tau::Manifest = tau::Manifest {
    this: tau::ModuleId {
        version: (0, 1),
        name: *b"sdio\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    },
    entry: sdio_entry,
    dependencies: &[],
    mapped_regions: &[tau::MappedRegion::stack(0x1000)],
};

// BSS starts at zero and remains shared across separate invocations.
static CALLS: AtomicUsize = AtomicUsize::new(0);
static INITIALIZED: AtomicUsize = AtomicUsize::new(0x5344_494f);

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    tau::Ubi::respond(9, [])
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn sdio_entry(_: usize, _: usize, _: usize, _: usize, _: usize, _: usize) -> ! {
    core::arch::naked_asm!(
        "li s0, 500",
        "li s1, 501",
        "li s2, 502",
        "li s3, 503",
        "li s4, 504",
        "li s5, 505",
        "li s6, 506",
        "li s7, 507",
        "li s8, 508",
        "li s9, 509",
        "li s10, 510",
        "li s11, 511",
        "li gp, 12345",
        "li tp, 67890",
        "j {main}",
        main = sym sdio_main,
    );
}

extern "C" fn sdio_main(
    event: usize,
    value: usize,
    second: usize,
    third: usize,
    fourth: usize,
    fifth: usize,
) -> ! {
    let Ok(tau::Event::Invocation { arg }) = tau::Event::decode(event) else {
        tau::Ubi::respond(1, []);
    };
    match arg {
        0 => {
            let count = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
            if INITIALIZED.fetch_add(1, Ordering::Relaxed) != 0x5344_494f + count - 1 {
                tau::Ubi::respond(2, []);
            }
            tau::Ubi::respond(0, [value.rotate_left(7) ^ 0x5344_494f, count])
        }
        1 => {
            // Authority belongs to this module, not to the invoking thread.
            let denied = matches!(
                tau::Ubi::map(NonZeroUsize::new(value), 0x1_0000_0000, 1),
                Err(tau::AllocError::AccessDenied)
            ) && matches!(
                tau::Ubi::prepare_module(&[0]),
                Err(tau::ModuleError::AccessDenied)
            ) && matches!(
                tau::Ubi::map(None, 0xffff_ffc0_0020_0000, 1),
                Err(tau::AllocError::InvalidAddress)
            ) && matches!(
                tau::Ubi::map(None, 0x10000, 1),
                Err(tau::AllocError::AccessDenied)
            );
            if !denied {
                tau::Ubi::respond(3, []);
            }
            // Anonymous private mappings survive return to the caller.
            if tau::Ubi::map(None, 0x1_0000_0000, 2).is_err() {
                tau::Ubi::respond(4, []);
            }
            for offset in [0, 0xff8, 0x1000, 0x1ff8] {
                let pointer = (0x1_0000_0000 + offset) as *mut usize;
                unsafe {
                    if pointer.read_volatile() != 0 {
                        tau::Ubi::respond(5, []);
                    }
                    pointer.write_volatile(value ^ offset);
                }
            }
            if !matches!(
                tau::Ubi::map(None, 0x1_0000_0000, 1),
                Err(tau::AllocError::AlreadyAllocated)
            ) {
                tau::Ubi::respond(6, []);
            }
            tau::Ubi::respond(0, [1])
        }
        2 => tau::Ubi::respond(42, []),
        3 => tau::Ubi::respond(0, [value ^ 0x5344_494f]),
        4 => {
            for offset in [0, 0xff8, 0x1000, 0x1ff8] {
                let pointer = (0x1_0000_0000 + offset) as *const usize;
                if unsafe { pointer.read_volatile() } != value ^ offset {
                    tau::Ubi::respond(8, []);
                }
            }
            if !matches!(
                tau::Ubi::map(None, 0x1_0000_0000, 1),
                Err(tau::AllocError::AlreadyAllocated)
            ) {
                tau::Ubi::respond(6, []);
            }
            tau::Ubi::respond(0, [1])
        }
        5 => {
            let response = tau::Ubi::invoke::<1, 2>(1, 0, [value]).unwrap();
            tau::Ubi::respond(0, response)
        }
        6 => tau::Ubi::respond(0, [value, second, third, fourth, fifth]),
        _ => tau::Ubi::respond(7, []),
    }
}
