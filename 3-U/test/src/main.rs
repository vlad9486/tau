// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]

mod graph;
mod scheduling;
mod elf;

use core::{arch::naked_asm, num::NonZeroUsize, ptr, sync::atomic::Ordering};

use dtb::Dtb;

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
    stack_pages: 0x20,
    dependencies: &[],
    mapped_regions: &[],
    wait_slots: &scheduling::SLOTS,
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

extern "C" fn main(arg: usize, hart: usize, info: usize, pages: usize, _: usize, _: usize) -> ! {
    if arg != 0 {
        tau_test_failed(0, 2, arg);
    }
    if info == 0 && pages == 0 {
        scheduling::secondary_hart(hart);
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
            let Ok((dtb, _)) = Dtb::new(raw) else {
                tau_test_failed(case, 3, info);
            };
            for name in ["system", "system-test", "dummy-test"] {
                let Some(module) = dtb.boot_module(name) else {
                    tau_test_failed(case, 3, info);
                };
                let bytes = module.sl::<u8>();
                if bytes.get(..4) != Some(b"\x7fELF") {
                    tau_test_failed(case, 3, bytes.as_ptr().addr());
                }
            }
            tau::Ubi::debug(1, hart);
            tau_test_passed(case, hart)
        }
        2 => {
            const ADDRESS: usize = 0x10_0000_0000;
            if let Err(error) = tau::Ubi::map(None, ADDRESS, 2) {
                tau_test_failed(case, 4, error as usize);
            }
            assert_eq!(
                tau::Ubi::map(None, ADDRESS, 1),
                Err(tau::AllocError::AlreadyAllocated)
            );
            // Touch both ends of both pages to check the complete allocation.
            for offset in [0, 0xff8, 0x1000, 0x1ff8] {
                let pointer = (ADDRESS + offset) as *mut usize;
                let pattern = 0x1234_5678_9abc_def0 ^ offset;
                unsafe {
                    assert_eq!(pointer.read_volatile(), 0);
                    pointer.write_volatile(pattern);
                    assert_eq!(pointer.read_volatile(), pattern);
                }
            }
            tau::Ubi::unmap(ADDRESS, 2).unwrap();
            assert_eq!(
                tau::Ubi::unmap(ADDRESS, 0),
                Err(tau::FreeError::AlreadyFree)
            );
            // Physical allocation and shared physical aliasing via Map/Unmap.
            let phys = tau::Ubi::alloc_physical(0, tau::MemoryZone::Normal).unwrap();
            const ALIAS_A: usize = 0x10_0001_0000;
            const ALIAS_B: usize = 0x10_0002_0000;
            tau::Ubi::map(NonZeroUsize::new(phys), ALIAS_A, 1).unwrap();
            tau::Ubi::map(NonZeroUsize::new(phys), ALIAS_B, 1).unwrap();
            unsafe {
                (ALIAS_A as *mut usize).write_volatile(0xcafe_babe_dead_beef);
                assert_eq!(
                    (ALIAS_B as *const usize).read_volatile(),
                    0xcafe_babe_dead_beef
                );
            }
            tau::Ubi::unmap(ALIAS_A, 1).unwrap();
            tau::Ubi::unmap(ALIAS_B, 1).unwrap();
            assert_eq!(
                tau::Ubi::free_physical(0, 0, tau::MemoryZone::Normal),
                Err(tau::FreeError::InvalidAddress)
            );
            tau::Ubi::free_physical(phys, 0, tau::MemoryZone::Normal).unwrap();
            tau_test_passed(case, ADDRESS)
        }
        3 => {
            // QEMU virt timebase is 10 MHz: a 100 ms deadline. The host checks
            // elapsed guest ticks, never debugger-sensitive wall-clock timing.
            let start = tau::asm::read_time();
            let deadline = start.checked_add(1_000_000).unwrap();
            scheduling::SLOTS[0].occupied.store(true, Ordering::SeqCst);
            let event = tau::Ubi::wait(&scheduling::SLOTS[0], NonZeroUsize::new(deadline));
            scheduling::SLOTS[0].occupied.store(false, Ordering::SeqCst);
            if !matches!(event, Ok(tau::Event::Timeout)) {
                let code = match event {
                    Ok(e) => e.encode(),
                    Err(err) => err as usize,
                };
                tau_test_failed(case, 5, code);
            }
            let end = tau::asm::read_time();
            assert!(end >= deadline);
            tau_test_passed(case, end - start)
        }
        4 => {
            let raw = tau::Area::new(info, pages << 12).sl();
            let (dtb, _) = Dtb::new(raw).unwrap();
            let image = dtb.boot_module("dummy-test").unwrap().sl::<u8>();
            let (slot, _) = elf::prepare_module(tau::ModuleHandle::ROOT, image, &dtb).unwrap();
            let (sibling, _) = elf::prepare_module(tau::ModuleHandle::ROOT, image, &dtb).unwrap();
            // Faulting module triggers Load Page Fault (cause 13 -> 0x800d) and becomes poisoned.
            assert_eq!(
                tau::Ubi::invoke::<0, 0>(sibling, 12, []).unwrap_err().get(),
                0x800d
            );
            assert_eq!(
                tau::Ubi::invoke::<1, 0>(sibling, 3, [1]).unwrap_err().get(),
                tau::ModuleError::Poisoned.encode()
            );
            // Independent module instance remains healthy.
            assert_eq!(
                tau::Ubi::invoke::<1, 1>(slot, 3, [42]).unwrap(),
                [42 ^ 0x5344_494f]
            );
            tau_test_passed(case, 0x800d)
        }
        5 => {
            let raw = tau::Area::new(info, pages << 12).sl();
            let (dtb, _) = Dtb::new(raw).unwrap();
            let image = dtb.boot_module("dummy-test").unwrap().sl::<u8>();
            assert_eq!(
                elf::prepare_module(tau::ModuleHandle::ROOT, &[0], &dtb),
                Err(tau::ModuleError::InvalidImage)
            );
            let (slot, module) = elf::prepare_module(tau::ModuleHandle::ROOT, image, &dtb)
                .expect("prepare dummy-test");
            let (sibling, sibling_module) =
                elf::prepare_module(tau::ModuleHandle::ROOT, image, &dtb)
                    .expect("prepare second dummy-test");
            assert_ne!(module, sibling_module);
            assert_eq!(
                tau::Ubi::bind_dependency(module, sibling_module).unwrap(),
                1
            );
            // Reject a late load segment after earlier segments allocated pages.
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
            // The loader supplied an acyclic A -> B binding. A -> B
            // uses a separate B context but the same module-owned globals.
            for (value, count) in [(3, 1), (4, 2)] {
                assert_eq!(
                    tau::Ubi::invoke::<1, 2>(slot, 5, [value]).unwrap(),
                    [value.rotate_left(7) ^ 0x5344_494f, count]
                );
            }
            // These failed preparations must not retain snapshot/root pages.
            for _ in 0..8 {
                assert_eq!(
                    elf::prepare_module(tau::ModuleHandle::ROOT, &[0], &dtb),
                    Err(tau::ModuleError::InvalidImage)
                );
                assert_eq!(
                    elf::prepare_module(tau::ModuleHandle::ROOT, malformed, &dtb),
                    Err(tau::ModuleError::InvalidImage)
                );
            }
            let phys = tau::Ubi::alloc_physical(0, tau::MemoryZone::Normal).unwrap();
            assert_eq!(
                tau::Ubi::free_physical(0, 0, tau::MemoryZone::Normal),
                Err(tau::FreeError::InvalidAddress)
            );
            assert_eq!(
                tau::Ubi::free_physical(usize::MAX, 0, tau::MemoryZone::Normal),
                Err(tau::FreeError::InvalidAddress)
            );
            assert_eq!(
                tau::Ubi::alloc_physical(0, tau::MemoryZone::Dma),
                Err(tau::AllocError::OutOfMemory)
            );
            assert_eq!(
                tau::Ubi::free_physical(phys, 0, tau::MemoryZone::Dma),
                Err(tau::FreeError::InvalidAddress)
            );
            tau::Ubi::free_physical(phys, 0, tau::MemoryZone::Normal).unwrap();
            for (entry, stack_pages) in [
                (0x1001, 1),                // unaligned entry
                (0xffff_ffc0_0021_0000, 1), // supervisor-only storage
                (usize::MAX, 1),            // high stack stem 511
                (0x1000, 0),                // invalid stack pages
            ] {
                assert_eq!(
                    tau::Ubi::create_module(
                        entry,
                        stack_pages,
                        0,
                        0,
                        tau::ModuleCapabilities::empty()
                    ),
                    Err(tau::ModuleError::InvalidImage)
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
            // Exercise continuation-preserving Respond (keep = true) across invocations.
            assert_eq!(tau::Ubi::invoke::<1, 1>(slot, 13, [25]).unwrap(), [35]);
            assert_eq!(tau::Ubi::invoke::<1, 1>(slot, 0, [7]).unwrap(), [42]);
            // The assembly response path must keep every message register
            // intact while using scratch registers to choose the MMU policy.
            let words = [usize::MAX, 0x1234_5678_9abc_def0, 3, 4, 5];
            assert_eq!(tau::Ubi::invoke::<5, 5>(slot, 6, words).unwrap(), words);
            assert_eq!(
                tau::Ubi::invoke::<0, 0>(0, 0, []).unwrap_err().get(),
                tau::ModuleError::InvalidDependency.encode()
            );
            // The ABI encodes 12 slot bits, while the protected tables contain
            // 0x800 entries. A cache-first lookup must reject the upper half.
            for invalid in [0x7ff, 0x800, 0xfff] {
                assert_eq!(
                    tau::Ubi::invoke::<0, 0>(invalid, 0, []).unwrap_err().get(),
                    tau::ModuleError::InvalidDependency.encode()
                );
            }
            // Bootstrap loader authority is restored on return.
            const ALIAS: usize = 0x2_0000_0000;
            tau::Ubi::map(NonZeroUsize::new(0x8020_0000), ALIAS, 1).unwrap();
            tau::Ubi::unmap(ALIAS, 1).unwrap();

            // Sibling module triggers Load Page Fault (cause 13 -> 0x800d).
            assert_eq!(
                tau::Ubi::invoke::<0, 0>(sibling, 12, []).unwrap_err().get(),
                0x800d
            );
            // Sibling is not pure, so it is quarantined/poisoned; subsequent calls fail with 0x8040.
            assert_eq!(
                tau::Ubi::invoke::<1, 0>(sibling, 3, [1]).unwrap_err().get(),
                tau::ModuleError::Poisoned.encode()
            );
            // Other modules (slot) remain healthy and unpoisoned.
            assert_eq!(
                tau::Ubi::invoke::<1, 1>(slot, 3, [42]).unwrap(),
                [42 ^ 0x5344_494f]
            );

            tau_test_passed(case, 128)
        }
        6 => graph::run(case, info, pages),
        7 => scheduling::run(case, hart, info, pages),
        8 => scheduling::rollback(case),
        _ => tau_test_failed(case, 0, case),
    }
}
