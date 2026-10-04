// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#[inline(always)]
pub fn sfence_vma(addr: Option<usize>, asid: Option<u16>) {
    use core::arch;

    // Keep a compiler memory barrier as well as the hardware fence: page-table
    // stores must be visible before subsequent address translations.
    unsafe {
        match (addr, asid) {
            (None, None) => {
                arch::asm!("sfence.vma zero, zero", options(nostack))
            }
            (None, Some(asid)) => {
                arch::asm!("sfence.vma zero, a1", in("a1") asid, options(nostack))
            }
            (Some(addr), None) => {
                arch::asm!("sfence.vma a0, zero", in("a0") addr, options(nostack))
            }
            (Some(addr), Some(asid)) => {
                arch::asm!("sfence.vma a0, a1", in("a0") addr, in("a1") asid, options(nostack))
            }
        }
    }
}

/// # Safety
/// it is a raw system call, must use type-safe wrapper instead
#[inline(always)]
pub unsafe fn sbi<const I: usize>(eid: u32, fid: u32, arg: [usize; I]) -> (isize, usize) {
    use core::arch::asm;

    let error: isize;
    let value: usize;
    match I {
        0 => unsafe {
            asm!(
                "ecall",
                in("a7") eid,
                in("a6") fid,
                out("a0") error,
                out("a1") value,
                options(nostack),
            )
        },
        1 => unsafe {
            asm!(
                "ecall",
                in("a7") eid,
                in("a6") fid,
                inout("a0") arg[0] => error,
                out("a1") value,
                options(nostack),
            )
        },
        2 => unsafe {
            asm!(
                "ecall",
                in("a7") eid,
                in("a6") fid,
                inout("a0") arg[0] => error,
                inout("a1") arg[1] => value,
                options(nostack),
            )
        },
        3 => unsafe {
            asm!(
                "ecall",
                in("a7") eid,
                in("a6") fid,
                inout("a0") arg[0] => error,
                inout("a1") arg[1] => value,
                in("a2") arg[2],
                options(nostack),
            )
        },
        4..=7 => {
            error = 1;
            value = 0;
        }
        _ => unsafe { core::hint::unreachable_unchecked() },
    }

    (error, value)
}
