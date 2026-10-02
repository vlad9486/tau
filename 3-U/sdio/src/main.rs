// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]

#[unsafe(no_mangle)]
static MANIFEST: tau::Manifest = tau::Manifest {
    this: tau::ModuleId {
        version: (0, 1),
        name: *b"sdio\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
    },
    entry: main,
    dependencies: &[],
    mapped_regions: &[tau::MappedRegion::stack(0x1000)],
};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

extern "C" fn main(_: usize, _: usize, _: usize, _: usize, _: usize, _: usize) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
