#![no_std]
#![no_main]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner),
    feature(strict_provenance_lints),
    warn(fuzzy_provenance_casts)
)]

use core::{arch, cell::UnsafeCell, hint, mem::MaybeUninit, fmt::Write as _};

use supervisor::{
    llfree::{Init, LLFree},
    module, sbi, scheduler, state, vmem,
};

#[cfg(all(not(debug_assertions), feature = "panic-never"))]
use panic_never as _;

#[cfg(all(any(debug_assertions, not(feature = "panic-never")), not(test)))]
#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo) -> ! {
    loop {
        hint::spin_loop();
    }
}

unsafe extern "C" {
    static __WINDOW: UnsafeCell<vmem::Window>;
    static __THREAD: UnsafeCell<scheduler::Thread>;
    static __MODULE: UnsafeCell<module::ModuleTables>;
    static __SCHEDULER: UnsafeCell<scheduler::Scheduler>;
    static __ALLOCATOR: UnsafeCell<[usize; 0o1000]>;

    #[allow(improper_ctypes)]
    static __CONTEXT: UnsafeCell<MaybeUninit<state::Context>>;
}

#[unsafe(link_section = ".text.init")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn _start() -> ! {
    arch::naked_asm! {
        "la t0, {handle_exception}",
        "addi t0, t0, 3",
        "andi t0, t0, -4",
        "csrw stvec, t0",
        "li sp, {stack}",
        "j {init}",
        handle_exception = sym handle_exception,
        init = sym init,
        stack = const { 0x_ffff_ffc0_0020_0000_usize },
    }
}

extern "C" fn init(
    hart_id: usize,
    base_addr: usize,
    info: usize,
    info_pages: usize,
    cores: usize,
    frames: usize,
) -> ! {
    let window = unsafe { &mut *__WINDOW.get() };
    let thread = unsafe { &mut *__THREAD.get() };
    let module = unsafe { __MODULE.get() };

    if cores != 0 {
        let (_, allocator) = unsafe {
            LLFree::new(Init::None, cores, frames, __ALLOCATOR.get().cast()).unwrap_unchecked()
        };

        unsafe {
            __CONTEXT.get().write(MaybeUninit::new(state::Context {
                allocator,
                base_addr,
            }))
        };
    }
    let context = unsafe { (*__CONTEXT.get()).assume_init_ref() };
    thread.set_hart_id(hart_id);

    match unsafe { state::init(window, thread, module, context) } {
        Ok((satp, sepc, inv)) => unsafe {
            arch::asm! {
                "li t0, 0x100",
                "csrrc t1, sstatus, t0", // set unprivileged mode
                "li t0, 0x20",
                "csrrs t1, sstatus, t0", // enable interrupts
                "csrw satp, t2",
                "csrw sepc, t3",
                "li sp, 0",
                "sret",
                options(noreturn),
                in("a0") inv.encode(),
                in("a1") hart_id,
                in("a2") info,
                in("a3") info_pages,
                in("t2") satp.0.get(),
                in("t3") sepc,
            }
        },
        Err(err) => {
            writeln!(sbi::Console, "{err:?}\r").unwrap_or_default();
            loop {
                hint::spin_loop();
            }
        }
    }
}

#[unsafe(naked)]
extern "C" fn handle_exception() -> ! {
    arch::naked_asm! {
        "nop",
        "csrrw tp, sscratch, tp",
        "sd t0, {t0}(tp)",
        "csrr t0, scause",
        "addi t0, t0, -8",
        "beqz t0, {handle_syscall}",

        "sd ra, {ra}(tp)",
        "sd sp, {sp}(tp)",
        "sd gp, {gp}(tp)",
        "sd tp, {tp}(tp)",
        // "sd t0, {t0}(tp)",
        "sd t1, {t1}(tp)",
        "sd t2, {t2}(tp)",
        "sd s0, {s0}(tp)",
        "sd s1, {s1}(tp)",
        "sd a0, {a0}(tp)",
        "sd a1, {a1}(tp)",
        "sd a2, {a2}(tp)",
        "sd a3, {a3}(tp)",
        "sd a4, {a4}(tp)",
        "sd a5, {a5}(tp)",
        "sd a6, {a6}(tp)",
        "sd a7, {a7}(tp)",
        "sd s2, {s2}(tp)",
        "sd s3, {s3}(tp)",
        "sd s4, {s4}(tp)",
        "sd s5, {s5}(tp)",
        "sd s6, {s6}(tp)",
        "sd s7, {s7}(tp)",
        "sd s8, {s8}(tp)",
        "sd s9, {s9}(tp)",
        "sd s10, {s10}(tp)",
        "sd s11, {s11}(tp)",
        "sd t3, {t3}(tp)",
        "sd t4, {t4}(tp)",
        "sd t5, {t5}(tp)",
        "sd t6, {t6}(tp)",
        ".attribute arch, \"rv64gc\"",
        "fsd f0, {f0}(tp)",
        "fsd f1, {f1}(tp)",
        "fsd f2, {f2}(tp)",
        "fsd f3, {f3}(tp)",
        "fsd f4, {f4}(tp)",
        "fsd f5, {f5}(tp)",
        "fsd f6, {f6}(tp)",
        "fsd f7, {f7}(tp)",
        "fsd f8, {f8}(tp)",
        "fsd f9, {f9}(tp)",
        "fsd f10, {f10}(tp)",
        "fsd f11, {f11}(tp)",
        "fsd f12, {f12}(tp)",
        "fsd f13, {f13}(tp)",
        "fsd f14, {f14}(tp)",
        "fsd f15, {f15}(tp)",
        "fsd f16, {f16}(tp)",
        "fsd f17, {f17}(tp)",
        "fsd f18, {f18}(tp)",
        "fsd f19, {f19}(tp)",
        "fsd f20, {f20}(tp)",
        "fsd f21, {f21}(tp)",
        "fsd f22, {f22}(tp)",
        "fsd f23, {f23}(tp)",
        "fsd f24, {f24}(tp)",
        "fsd f25, {f25}(tp)",
        "fsd f26, {f26}(tp)",
        "fsd f27, {f27}(tp)",
        "fsd f28, {f28}(tp)",
        "fsd f29, {f29}(tp)",
        "fsd f30, {f30}(tp)",
        "fsd f31, {f31}(tp)",

        // put scause in a0
        "addi a0, t0, 8",
        "csrr t0, sepc",
        "sd t0, {pc}(tp)",
        "mv sp, tp",
        // handle the trap
        "j {exception}",

        ra  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o010 },
        sp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o020 },
        gp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o030 },
        tp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o040 },
        t0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o050 },
        t1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o060 },
        t2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o070 },
        s0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o100 },
        s1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o110 },
        a0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o120 },
        a1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o130 },
        a2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o140 },
        a3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o150 },
        a4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o160 },
        a5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o170 },
        a6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o200 },
        a7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o210 },
        s2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o220 },
        s3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o230 },
        s4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o240 },
        s5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o250 },
        s6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o260 },
        s7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o270 },
        s8  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o300 },
        s9  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o310 },
        s10 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o320 },
        s11 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o330 },
        t3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o340 },
        t4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o350 },
        t5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o360 },
        t6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o370 },
        f0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o400 },
        f1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o410 },
        f2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o420 },
        f3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o430 },
        f4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o440 },
        f5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o450 },
        f6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o460 },
        f7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o470 },
        f8  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o500 },
        f9  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o510 },
        f10 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o520 },
        f11 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o530 },
        f12 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o540 },
        f13 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o550 },
        f14 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o560 },
        f15 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o570 },
        f16 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o600 },
        f17 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o610 },
        f18 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o620 },
        f19 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o630 },
        f20 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o640 },
        f21 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o650 },
        f22 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o660 },
        f23 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o670 },
        f24 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o700 },
        f25 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o710 },
        f26 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o720 },
        f27 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o730 },
        f28 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o740 },
        f29 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o750 },
        f30 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o760 },
        f31 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o770 },
        pc = const { memoffset::offset_of!(scheduler::Thread, sepc) },

        handle_syscall = sym handle_syscall,
        exception = sym exception,
    }
}

#[unsafe(naked)]
extern "C" fn handle_syscall() -> ! {
    arch::naked_asm! {
        "csrr t0, sepc",
        "addi t0, t0, 4",
        "csrw sepc, t0",
        "sd t0, {pc}(tp)",
        "sd ra, {ra}(tp)",
        "sd sp, {sp}(tp)",

        "sd s0, {s0}(tp)",
        "sd s1, {s1}(tp)",
        "sd s2, {s2}(tp)",
        "sd s3, {s3}(tp)",
        "sd s4, {s4}(tp)",
        "sd s5, {s5}(tp)",
        "sd s6, {s6}(tp)",
        "sd s7, {s7}(tp)",
        "sd s8, {s8}(tp)",
        "sd s9, {s9}(tp)",
        "sd s10, {s10}(tp)",
        "sd s11, {s11}(tp)",

        "li sp, {stack}",
        "j {syscall}",

        pc  = const { memoffset::offset_of!(scheduler::Thread, sepc) },
        ra  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o010 },
        sp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o020 },

        s0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o100 },
        s1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o110 },
        s2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o220 },
        s3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o230 },
        s4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o240 },
        s5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o250 },
        s6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o260 },
        s7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o270 },
        s8  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o300 },
        s9  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o310 },
        s10 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o320 },
        s11 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o330 },

        stack = const { 0x_ffff_ffc0_0020_0000_usize },
        syscall = sym syscall,
    }
}

extern "C" fn exception(cause: isize) -> ! {
    state::exception(cause);
    restore_exception()
}

extern "C" fn syscall(a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> ! {
    let window = unsafe { &mut *__WINDOW.get() };
    let thread = unsafe { &mut *__THREAD.get() };
    let module = unsafe { &*__MODULE.get() };
    let context = unsafe { (*__CONTEXT.get()).assume_init_ref() };
    let [a0, a1, a2, a3, a4, a5] =
        state::syscall(window, thread, module, context, [a0, a1, a2, a3, a4, a5]);
    restore_syscall(a0, a1, a2, a3, a4, a5)
}

extern "C" fn restore_syscall(
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> ! {
    unsafe {
        arch::asm! {
            "mv t0, zero",
            "mv t1, zero",
            "ld ra, {ra}(tp)",
            "ld sp, {sp}(tp)",
            "ld s0, {s0}(tp)",
            "ld s1, {s1}(tp)",
            "ld s2, {s2}(tp)",
            "ld s3, {s3}(tp)",
            "ld s4, {s4}(tp)",
            "ld s5, {s5}(tp)",
            "ld s6, {s6}(tp)",
            "ld s7, {s7}(tp)",
            "ld s8, {s8}(tp)",
            "ld s9, {s9}(tp)",
            "ld s10, {s10}(tp)",
            "ld s11, {s11}(tp)",
            "csrrw tp, sscratch, tp",
            "sret",
            options(noreturn),
            in("a0") a0,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            in("a4") a4,
            in("a5") a5,
            ra  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o010 },
            sp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o020 },

            s0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o100 },
            s1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o110 },
            s2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o220 },
            s3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o230 },
            s4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o240 },
            s5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o250 },
            s6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o260 },
            s7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o270 },
            s8  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o300 },
            s9  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o310 },
            s10 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o320 },
            s11 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o330 },
        }
    }
}

extern "C" fn restore_exception() -> ! {
    unsafe {
        arch::asm! {
            "ld t0, {pc}(tp)",
            "csrw sepc, t0",
            "ld ra, {ra}(tp)",
            "ld sp, {sp}(tp)",
            "ld gp, {gp}(tp)",
            "ld tp, {tp}(tp)",
            "ld t0, {t0}(tp)",
            "ld t1, {t1}(tp)",
            "ld t2, {t2}(tp)",
            "ld s0, {s0}(tp)",
            "ld s1, {s1}(tp)",
            "ld a0, {a0}(tp)",
            "ld a1, {a1}(tp)",
            "ld a2, {a2}(tp)",
            "ld a3, {a3}(tp)",
            "ld a4, {a4}(tp)",
            "ld a5, {a5}(tp)",
            "ld a6, {a6}(tp)",
            "ld a7, {a7}(tp)",
            "ld s2, {s2}(tp)",
            "ld s3, {s3}(tp)",
            "ld s4, {s4}(tp)",
            "ld s5, {s5}(tp)",
            "ld s6, {s6}(tp)",
            "ld s7, {s7}(tp)",
            "ld s8, {s8}(tp)",
            "ld s9, {s9}(tp)",
            "ld s10, {s10}(tp)",
            "ld s11, {s11}(tp)",
            "ld t3, {t3}(tp)",
            "ld t4, {t4}(tp)",
            "ld t5, {t5}(tp)",
            "ld t6, {t6}(tp)",
            ".attribute arch, \"rv64gc\"",
            "fld f0, {f0}(tp)",
            "fld f1, {f1}(tp)",
            "fld f2, {f2}(tp)",
            "fld f3, {f3}(tp)",
            "fld f4, {f4}(tp)",
            "fld f5, {f5}(tp)",
            "fld f6, {f6}(tp)",
            "fld f7, {f7}(tp)",
            "fld f8, {f8}(tp)",
            "fld f9, {f9}(tp)",
            "fld f10, {f10}(tp)",
            "fld f11, {f11}(tp)",
            "fld f12, {f12}(tp)",
            "fld f13, {f13}(tp)",
            "fld f14, {f14}(tp)",
            "fld f15, {f15}(tp)",
            "fld f16, {f16}(tp)",
            "fld f17, {f17}(tp)",
            "fld f18, {f18}(tp)",
            "fld f19, {f19}(tp)",
            "fld f20, {f20}(tp)",
            "fld f21, {f21}(tp)",
            "fld f22, {f22}(tp)",
            "fld f23, {f23}(tp)",
            "fld f24, {f24}(tp)",
            "fld f25, {f25}(tp)",
            "fld f26, {f26}(tp)",
            "fld f27, {f27}(tp)",
            "fld f28, {f28}(tp)",
            "fld f29, {f29}(tp)",
            "fld f30, {f30}(tp)",
            "fld f31, {f31}(tp)",
            "csrrw tp, sscratch, tp",
            "sret",
            options(noreturn),
            ra  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o010 },
            sp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o020 },
            gp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o030 },
            tp  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o040 },
            t0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o050 },
            t1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o060 },
            t2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o070 },
            s0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o100 },
            s1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o110 },
            a0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o120 },
            a1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o130 },
            a2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o140 },
            a3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o150 },
            a4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o160 },
            a5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o170 },
            a6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o200 },
            a7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o210 },
            s2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o220 },
            s3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o230 },
            s4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o240 },
            s5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o250 },
            s6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o260 },
            s7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o270 },
            s8  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o300 },
            s9  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o310 },
            s10 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o320 },
            s11 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o330 },
            t3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o340 },
            t4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o350 },
            t5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o360 },
            t6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o370 },
            f0  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o400 },
            f1  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o410 },
            f2  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o420 },
            f3  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o430 },
            f4  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o440 },
            f5  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o450 },
            f6  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o460 },
            f7  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o470 },
            f8  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o500 },
            f9  = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o510 },
            f10 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o520 },
            f11 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o530 },
            f12 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o540 },
            f13 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o550 },
            f14 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o560 },
            f15 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o570 },
            f16 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o600 },
            f17 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o610 },
            f18 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o620 },
            f19 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o630 },
            f20 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o640 },
            f21 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o650 },
            f22 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o660 },
            f23 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o670 },
            f24 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o700 },
            f25 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o710 },
            f26 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o720 },
            f27 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o730 },
            f28 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o740 },
            f29 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o750 },
            f30 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o760 },
            f31 = const { memoffset::offset_of!(scheduler::Thread, registers) + 0o770 },
            pc = const { memoffset::offset_of!(scheduler::Thread, sepc) },
        }
    }
}
