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

fn prepare_raw(address: usize, length: usize) -> usize {
    let status;
    unsafe {
        core::arch::asm!(
            "ecall",
            inout("a0") tau::Call::PrepareModule.encode() => status,
            in("a1") address,
            in("a2") length,
            clobber_abi("C"),
            options(nostack),
        );
    }
    status
}

// Exercise the module boundary with every preserved register set explicitly.
#[unsafe(naked)]
extern "C" fn invoke_registers(_slot: usize, _value: usize, _count: usize) -> usize {
    naked_asm!(
        "addi sp, sp, -160",
        "sd ra, 0(sp)",
        "sd gp, 8(sp)",
        "sd tp, 16(sp)",
        "sd a1, 24(sp)",
        "sd a2, 32(sp)",
        "sd sp, 144(sp)",
        "sd s0, 48(sp)",
        "sd s1, 56(sp)",
        "sd s2, 64(sp)",
        "sd s3, 72(sp)",
        "sd s4, 80(sp)",
        "sd s5, 88(sp)",
        "sd s6, 96(sp)",
        "sd s7, 104(sp)",
        "sd s8, 112(sp)",
        "sd s9, 120(sp)",
        "sd s10, 128(sp)",
        "sd s11, 136(sp)",
        "slli a0, a0, 4",
        "ori a0, a0, 1",
        "li gp, 0x13579",
        "li tp, 0x24680",
        "li s0, 100",
        "li s1, 101",
        "li s2, 102",
        "li s3, 103",
        "li s4, 104",
        "li s5, 105",
        "li s6, 106",
        "li s7, 107",
        "li s8, 108",
        "li s9, 109",
        "li s10, 110",
        "li s11, 111",
        "ecall",
        "bnez a0, 2f",
        "ld t0, 24(sp)",
        "slli t1, t0, 7",
        "srli t0, t0, 57",
        "or t0, t0, t1",
        "li t1, 0x5344494f",
        "xor t0, t0, t1",
        "bne a1, t0, 2f",
        "ld t0, 32(sp)",
        "bne a2, t0, 2f",
        "ld t0, 144(sp)",
        "bne sp, t0, 2f",
        "li t0, 0x13579",
        "bne gp, t0, 2f",
        "li t0, 0x24680",
        "bne tp, t0, 2f",
        "li t0, 100",
        "bne s0, t0, 2f",
        "li t0, 101",
        "bne s1, t0, 2f",
        "li t0, 102",
        "bne s2, t0, 2f",
        "li t0, 103",
        "bne s3, t0, 2f",
        "li t0, 104",
        "bne s4, t0, 2f",
        "li t0, 105",
        "bne s5, t0, 2f",
        "li t0, 106",
        "bne s6, t0, 2f",
        "li t0, 107",
        "bne s7, t0, 2f",
        "li t0, 108",
        "bne s8, t0, 2f",
        "li t0, 109",
        "bne s9, t0, 2f",
        "li t0, 110",
        "bne s10, t0, 2f",
        "li t0, 111",
        "bne s11, t0, 2f",
        "li a0, 0",
        "j 3f",
        "2: li a0, 1",
        "3:",
        "ld ra, 0(sp)",
        "ld gp, 8(sp)",
        "ld tp, 16(sp)",
        "ld s0, 48(sp)",
        "ld s1, 56(sp)",
        "ld s2, 64(sp)",
        "ld s3, 72(sp)",
        "ld s4, 80(sp)",
        "ld s5, 88(sp)",
        "ld s6, 96(sp)",
        "ld s7, 104(sp)",
        "ld s8, 112(sp)",
        "ld s9, 120(sp)",
        "ld s10, 128(sp)",
        "ld s11, 136(sp)",
        "addi sp, sp, 160",
        "ret",
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
            let event = tau::Ubi::wait_external(NonZeroUsize::new(deadline));
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
        5 => {
            let raw = tau::Area::new(info, pages << 12).sl();
            let (dtb, _) = tau::Dtb::new(raw).unwrap();
            let image = dtb.boot_module("sdio").unwrap().sl::<u8>();
            assert_eq!(
                tau::Ubi::prepare_module(&[0]),
                Err(tau::ModuleError::InvalidImage)
            );
            let slot = tau::Ubi::prepare_module(image).expect("prepare sdio");
            let sibling = tau::Ubi::prepare_module(image).expect("prepare second sdio");
            // Reject a late load segment after earlier segments allocated pages.
            // Keep the damaged fixture on our existing stack so the host can
            // compare allocator counts before and after failed preparations.
            let mut malformed = [0u8; 0x8000];
            let malformed = &mut malformed[..image.len()];
            malformed.copy_from_slice(image);
            let headers = usize::from_le_bytes(malformed[32..40].try_into().unwrap());
            let stride = u16::from_le_bytes(malformed[54..56].try_into().unwrap()) as usize;
            let count = u16::from_le_bytes(malformed[56..58].try_into().unwrap()) as usize;
            let mut load_count = 0;
            let mut last_load = 0;
            for i in 0..count {
                let offset = headers + i * stride;
                if malformed[offset..offset + 4] == 1u32.to_le_bytes() {
                    load_count += 1;
                    last_load = offset;
                }
            }
            assert!(load_count > 1);
            malformed[last_load + 4..last_load + 8].copy_from_slice(&7u32.to_le_bytes());
            // The prepared module owns its ELF backing independently of this alias.
            tau::Ubi::unmap(image.as_ptr().addr(), image.len().div_ceil(4096)).unwrap();
            tau_test_progress(case, slot as usize, sibling as usize);
            // The host temporarily exhausts ASIDs to check construction rollback.
            assert_eq!(
                tau::Ubi::invoke::<0, 0>(slot, 3, []).unwrap_err().get(),
                tau::ModuleError::OutOfMemory as usize
            );
            tau_test_progress(case, slot as usize, 0);
            // Construct each destination once, then immediately reuse it.
            for destination in [slot, sibling] {
                for value in [1, 2] {
                    assert_eq!(
                        tau::Ubi::invoke::<1, 1>(destination, 3, [value]).unwrap(),
                        [value ^ 0x5344_494f]
                    );
                }
            }
            for (destination, value) in [(slot, 0x8020_0000), (sibling, 0x8030_0000)] {
                assert_eq!(
                    tau::Ubi::invoke::<1, 1>(destination, 1, [value]).unwrap(),
                    [1]
                );
            }
            // The host supplies A -> B and B -> A fixture bindings. A -> B
            // uses a separate B context but the same module-owned globals.
            for (value, count) in [(3, 1), (4, 2)] {
                assert_eq!(
                    tau::Ubi::invoke::<1, 2>(slot, 5, [value]).unwrap(),
                    [value.rotate_left(7) ^ 0x5344_494f, count]
                );
            }
            tau_test_progress(case, slot as usize, 1);
            // These failed preparations must not retain snapshot/root pages.
            for _ in 0..8 {
                assert_eq!(
                    tau::Ubi::prepare_module(&[0]),
                    Err(tau::ModuleError::InvalidImage)
                );
                assert_eq!(
                    tau::Ubi::prepare_module(malformed),
                    Err(tau::ModuleError::InvalidImage)
                );
            }
            for (address, length) in [
                (FAULT_ADDRESS, 4096),         // canonical but unmapped
                (0xffff_ffc0_0021_0000, 4096), // supervisor-only storage
                (usize::MAX, 2),               // range overflow
                (0, 0),
            ] {
                assert_eq!(
                    prepare_raw(address, length),
                    tau::ModuleError::InvalidImage as usize
                );
            }
            // Exercise mapping across a 1 GiB root boundary and reclaim both branches.
            const EDGE: usize = (12 << 30) - 4096;
            tau::Ubi::map(None, EDGE, 2).unwrap();
            for address in [EDGE, EDGE + 4096] {
                let pointer = address as *mut usize;
                unsafe {
                    assert_eq!(pointer.read_volatile(), 0);
                    pointer.write_volatile(address);
                    assert_eq!(pointer.read_volatile(), address);
                }
            }
            tau::Ubi::unmap(EDGE, 2).unwrap();
            let mut marker = [0x1234_5678_9abc_def0usize; 8];
            for count in 1..=64 {
                let value = 0xfedc_ba98_7654_3210usize ^ count;
                assert_eq!(invoke_registers(slot as usize, value, count), 0);
                assert_eq!(invoke_registers(sibling as usize, value, count + 2), 0);
                // Volatile loads force a real check of the caller's stack.
                for word in &mut marker {
                    assert_eq!(unsafe { ptr::read_volatile(word) }, 0x1234_5678_9abc_def0);
                }
            }
            for (destination, value) in [(slot, 0x8020_0000), (sibling, 0x8030_0000)] {
                assert_eq!(
                    tau::Ubi::invoke::<1, 1>(destination, 4, [value]).unwrap(),
                    [1]
                );
            }
            assert_eq!(
                tau::Ubi::invoke::<1, 2>(slot, 5, [5]).unwrap(),
                [5usize.rotate_left(7) ^ 0x5344_494f, 67]
            );
            assert_eq!(
                tau::Ubi::invoke::<1, 2>(sibling, 0, [6]).unwrap(),
                [6usize.rotate_left(7) ^ 0x5344_494f, 68]
            );
            assert_eq!(tau::Ubi::invoke::<0, 0>(slot, 2, []).unwrap_err().get(), 42);
            assert_eq!(
                tau::Ubi::invoke::<0, 0>(0, 0, []).unwrap_err().get(),
                tau::ModuleError::InvalidDependency as usize
            );
            // Bootstrap loader authority is restored on return.
            const ALIAS: usize = 0x2_0000_0000;
            tau::Ubi::map(NonZeroUsize::new(0x8020_0000), ALIAS, 1).unwrap();
            tau::Ubi::unmap(ALIAS, 1).unwrap();
            tau_test_passed(case, 128)
        }
        _ => tau_test_failed(case, 0, case),
    }
}
