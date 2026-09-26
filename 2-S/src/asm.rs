#[inline(always)]
pub fn sfence_vma(addr: Option<usize>, asid: Option<u16>) {
    use core::arch;

    // Safety: `sfence.vma` does nothing if argument is invalid
    unsafe {
        match (addr, asid) {
            (None, None) => {
                arch::asm!("sfence.vma zero, zero", options(nostack, nomem))
            }
            (None, Some(asid)) => {
                arch::asm!("sfence.vma zero, a1", in("a1") asid, options(nostack, nomem))
            }
            (Some(addr), None) => {
                arch::asm!("sfence.vma a0, zero", in("a0") addr, options(nostack, nomem))
            }
            (Some(addr), Some(asid)) => {
                arch::asm!("sfence.vma a0, a1", in("a0") addr, in("a1") asid, options(nostack, nomem))
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
