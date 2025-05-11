use core::arch;

#[inline(always)]
pub fn fence() {
    unsafe { arch::asm!("fence") };
}

#[inline(always)]
pub fn read_time() -> usize {
    let v: usize;
    unsafe {
        core::arch::asm!(
            "csrr {0}, time",
            out(reg) v,
            options(nomem, nostack)
        );
    }
    v
}

#[inline(always)]
pub fn dbg<const I: usize>(arg: [usize; I]) {
    use core::arch::asm;

    unsafe {
        match I {
            0 => asm!("2: j 2b", options(nomem, nostack, noreturn),),
            1 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                options(nomem, nostack, noreturn),
            ),
            2 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                in("a1") arg[1],
                options(nomem, nostack, noreturn),
            ),
            3 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                in("a1") arg[1],
                in("a2") arg[2],
                options(nomem, nostack, noreturn),
            ),
            4 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                in("a1") arg[1],
                in("a2") arg[2],
                in("a3") arg[3],
                options(nomem, nostack, noreturn),
            ),
            5 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                in("a1") arg[1],
                in("a2") arg[2],
                in("a3") arg[3],
                in("a4") arg[4],
                options(nomem, nostack, noreturn),
            ),
            6 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                in("a1") arg[1],
                in("a2") arg[2],
                in("a3") arg[3],
                in("a4") arg[4],
                in("a5") arg[5],
                options(nomem, nostack, noreturn),
            ),
            7 => asm!(
                "2: j 2b",
                in("a0") arg[0],
                in("a1") arg[1],
                in("a2") arg[2],
                in("a3") arg[3],
                in("a4") arg[4],
                in("a5") arg[5],
                in("a6") arg[6],
                options(nomem, nostack, noreturn),
            ),
            _ => unreachable!(),
        }
    }
}
