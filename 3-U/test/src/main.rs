// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]

use core::{arch::naked_asm, num::NonZeroUsize, ptr};

// Wire protocol v1: three little-endian u64 words: magic, version, scenario.
// The host writes scenario only while stopped at tau_test_ready. Volatile access
// keeps that external write visible even with release LTO enabled.
#[unsafe(no_mangle)]
static mut TAU_TEST_CONTROL: [u64; 3] = [0x5441_5554_4553_5431, 1, 0];

#[unsafe(no_mangle)]
static MANIFEST: tau::Manifest = tau::Manifest {
    this: tau::ModuleId {
        version: (0, 1),
        name: *b"system-test\0\0\0\0\0\0\0\0\0",
    },
    entry: main,
    dependencies: &[],
    mapped_regions: &[tau::MappedRegion::stack(0x20000)],
};

// Naked C functions preserve their argument registers at the first instruction.
// The host installs hardware breakpoints at the exact symbol addresses.
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn tau_test_ready(_hart: usize) {
    naked_asm!("ret");
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn tau_test_progress(_case: usize, _value: usize, _extra: usize) {
    naked_asm!("ret");
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn tau_test_passed(_case: usize, _value: usize) -> ! {
    naked_asm!("2: j 2b");
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn tau_test_failed(_case: usize, _code: usize, _value: usize) -> ! {
    naked_asm!("nop", "2: j 2b");
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let case = unsafe { ptr::read_volatile(ptr::addr_of!(TAU_TEST_CONTROL[2])) };
    tau_test_failed(
        case as usize,
        1,
        info.location().map_or(0, |p| p.line() as usize),
    )
}

// An unmapped canonical VA, separate from the payload, stack, and mapping test.
const FAULT_ADDRESS: usize = 0x20_0000_0000;

#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn tau_test_fault() -> ! {
    naked_asm!(
        "li t0, {address}",
        ".global tau_test_fault_instruction",
        "tau_test_fault_instruction:",
        "ld t1, 0(t0)",
        "li a0, 4",
        "li a1, 6",
        "mv a2, t1",
        "j {failed}",
        address = const FAULT_ADDRESS,
        failed = sym tau_test_failed,
    );
}

extern "C" fn main(event: usize, hart: usize, info: usize, pages: usize, _: usize, _: usize) -> ! {
    if !matches!(tau::Event::decode(event), Ok(tau::Event::Invocation { .. })) {
        tau_test_failed(0, 2, event);
    }
    tau_test_ready(hart);
    let case = unsafe { ptr::read_volatile(ptr::addr_of!(TAU_TEST_CONTROL[2])) } as usize;
    match case {
        1 => {
            // Validate the actual boot arguments and DTB, not just arrival here.
            if info == 0 || pages == 0 {
                tau_test_failed(case, 3, info);
            }
            let raw = tau::Area::new(info, pages << 12).sl();
            let Ok((dtb, _)) = tau::Dtb::new(raw) else {
                tau_test_failed(case, 3, info);
            };
            for name in ["system", "system-test", "sdio"] {
                let Some(module) = dtb.boot_module(name) else {
                    tau_test_failed(case, 3, info);
                };
                let bytes = module.sl::<u8>();
                if bytes.get(..4) != Some(b"\x7fELF") {
                    tau_test_failed(case, 3, bytes.as_ptr().addr());
                }
            }
            tau_test_passed(case, hart)
        }
        2 => {
            const ADDRESS: usize = 0x10_0000_0000;
            if let Err(error) = tau::Ubi::map(None, ADDRESS, 2) {
                tau_test_failed(case, 4, error as usize);
            }
            // Touch both ends of both pages to check the complete allocation.
            for offset in [0, 0xff8, 0x1000, 0x1ff8] {
                let pointer = (ADDRESS + offset) as *mut usize;
                let pattern = 0x1234_5678_9abc_def0 ^ offset;
                unsafe {
                    pointer.write_volatile(pattern);
                    assert_eq!(pointer.read_volatile(), pattern);
                }
            }
            tau_test_progress(case, ADDRESS, 2);
            tau_test_passed(case, ADDRESS)
        }
        3 => {
            // QEMU virt timebase is 10 MHz: a 100 ms deadline. The host checks
            // elapsed guest ticks, never debugger-sensitive wall-clock timing.
            let start = tau::asm::read_time();
            let deadline = start.checked_add(1_000_000).unwrap();
            let event = tau::Ubi::wait(NonZeroUsize::new(deadline));
            if !matches!(event, tau::Event::Timeout) {
                tau_test_failed(case, 5, event.encode());
            }
            let end = tau::asm::read_time();
            assert!(end >= deadline);
            tau_test_progress(case, deadline, end);
            tau_test_passed(case, end - start)
        }
        4 => {
            tau_test_progress(case, FAULT_ADDRESS, 0);
            tau_test_fault()
        }
        _ => tau_test_failed(case, 0, case),
    }
}
