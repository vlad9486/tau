// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner),
    feature(strict_provenance_lints),
    warn(implicit_provenance_casts)
)]

use core::{arch, cell::UnsafeCell, fmt::Write as _, hint, mem, num::NonZero};

use supervisor::{llfree::Allocator, module, sbi, scheduler, state, vmem, mmu};

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
    // Fixed virtual addresses supplied by build.rs. Their scope is determined by
    // the physical pages mapped there, not by these external static declarations.

    // Per-hart sparse window and stack; pass its pointer without a whole borrow.
    static __WINDOW: vmem::Window;

    // Local thread metadata shared by this thread's module contexts; parent/child
    // lifetime information belongs here, without a separate thread collection.
    #[allow(improper_ctypes)]
    static __THREAD: UnsafeCell<scheduler::Thread>;
    // Context-private root, dependency roots, and active invocation state.
    // Linker-provided Rust storage, not a foreign-language representation.
    #[allow(improper_ctypes)]
    static __MODULE_CONTEXT: UnsafeCell<module::ModuleContext>;
    // Module-wide privileged tables shared across its contexts: synchronized
    // dependency stems/templates and validated wait-table metadata.
    static __MODULE: UnsafeCell<module::ModuleTables>;

    // System-wide scheduler backing, initialized before the first user entry.
    #[allow(improper_ctypes)]
    static __SCHEDULER: UnsafeCell<scheduler::Scheduler>;
    // System-wide physical-memory allocator metadata, shared between harts.
    static __ALLOCATOR: UnsafeCell<u8>;
    // System-wide allocator access and physical-memory base, initialized by the
    // boot hart. Despite its name, this is not a thread's module context.
    #[allow(improper_ctypes)]
    static __CONTEXT: UnsafeCell<state::Context>;
}

/// # Safety
/// The current root's thread, module-context, and module pages must be mapped
/// and initialized, and no other references to them may exist on this hart.
#[inline(always)]
unsafe fn local_access<'a>() -> mmu::Local<'a> {
    unsafe {
        mmu::Local {
            thread: &mut *UnsafeCell::raw_get(&raw const __THREAD),
            module_context: &mut *UnsafeCell::raw_get(&raw const __MODULE_CONTEXT),
            module: &mut *UnsafeCell::raw_get(&raw const __MODULE),
        }
    }
}

#[unsafe(link_section = ".text.init")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn _start() -> ! {
    arch::naked_asm! {
        "la t0, {trap_entry}",
        "addi t0, t0, 3",
        "andi t0, t0, -4",
        "csrw stvec, t0",
        "li sp, {stack}",
        "j {init}",
        trap_entry = sym trap_entry,
        init = sym init,
        stack = const { vmem::WINDOW_END },
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
    let mut window = unsafe { vmem::WindowAccess::new(&raw const __WINDOW) };
    let module = UnsafeCell::raw_get(&raw const __MODULE_CONTEXT);
    let tables = UnsafeCell::raw_get(&raw const __MODULE);
    let scheduler = UnsafeCell::raw_get(&raw const __SCHEDULER);

    // `cores != 0` means this is first run on the bootstrap core
    // otherwise this is additional core bring up
    if cores != 0 {
        writeln!(
            sbi::Console,
            "SU: init hart={hart_id} cores={cores} frames=0x{frames:016x}\r"
        )
        .unwrap_or_default();

        let frames = unsafe { NonZero::new_unchecked(frames) };
        let allocator = unsafe {
            Allocator::new(frames, __ALLOCATOR.get())
                .unwrap_unchecked()
                .into_lower()
        };

        unsafe {
            UnsafeCell::raw_get(&raw const __CONTEXT).write(state::Context {
                allocator,
                base_addr,
                bootstrap_root: vmem::Window::current_root(),
            });
        }
        unsafe { mmu::init(&mut window) };
        writeln!(
            sbi::Console,
            "SU: ASID mask=0x{:04x} force_flush={}\r",
            mmu::asid_mask(&window),
            mmu::force_flush(&window)
        )
        .unwrap_or_default();
    }
    let thread = UnsafeCell::raw_get(&raw const __THREAD);
    unsafe {
        thread.write(scheduler::Thread::new(
            hart_id,
            1,
            vmem::Window::current_root(),
            [0; 6],
            scheduler::ReturnKind::Syscall,
        ));
    }
    let thread = unsafe { &*thread };

    let context = unsafe { &*UnsafeCell::raw_get(&raw const __CONTEXT) };
    match unsafe { state::init(&mut window, thread, module, tables, scheduler, context) } {
        Ok((satp, sepc)) => unsafe {
            arch::asm! {
                "li t0, 0x100",
                "csrrc t1, sstatus, t0", // set unprivileged mode
                "li t0, 0x20",
                "csrrs t1, sstatus, t0", // enable interrupts
                "csrw satp, t2",
                "csrw sepc, t3",
                "la t0, {module_context}",
                "csrw sscratch, t0",
                "li sp, 0",
                "sret",
                options(noreturn),
                module_context = sym __MODULE_CONTEXT,
                in("a0") 0,
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
extern "C" fn trap_entry() -> ! {
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
        // "sd t0, {t0}(tp)",
        "sd t1, {t1}(tp)",
        "csrr t1, sscratch",
        "sd t1, {tp}(tp)",
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

        "csrr a0, scause",
        "csrr t0, sepc",
        "sd t0, {pc}(tp)",
        "li sp, {stack}",
        "j {non_syscall_trap}",

        ra  = const { mem::offset_of!(module::ModuleContext, registers) + 0o010 },
        sp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o020 },
        gp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o030 },
        tp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o040 },
        t0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o050 },
        t1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o060 },
        t2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o070 },
        s0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o100 },
        s1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o110 },
        a0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o120 },
        a1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o130 },
        a2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o140 },
        a3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o150 },
        a4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o160 },
        a5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o170 },
        a6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o200 },
        a7  = const { mem::offset_of!(module::ModuleContext, registers) + 0o210 },
        s2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o220 },
        s3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o230 },
        s4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o240 },
        s5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o250 },
        s6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o260 },
        s7  = const { mem::offset_of!(module::ModuleContext, registers) + 0o270 },
        s8  = const { mem::offset_of!(module::ModuleContext, registers) + 0o300 },
        s9  = const { mem::offset_of!(module::ModuleContext, registers) + 0o310 },
        s10 = const { mem::offset_of!(module::ModuleContext, registers) + 0o320 },
        s11 = const { mem::offset_of!(module::ModuleContext, registers) + 0o330 },
        t3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o340 },
        t4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o350 },
        t5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o360 },
        t6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o370 },
        pc = const { mem::offset_of!(module::ModuleContext, sepc) },

        stack = const { vmem::WINDOW_END },
        handle_syscall = sym handle_syscall,
        non_syscall_trap = sym handle_non_syscall_trap,
    }
}

#[unsafe(naked)]
extern "C" fn handle_syscall() -> ! {
    arch::naked_asm! {
        // Invoke still saves its caller's ABI-preserved registers below.
        // Respond ends the callee's invocation; its PC and registers will
        // never be resumed, so skip saving only that terminal call.
        // Match the complete low word, including the reserved bit and slot.
        "slli t0, a0, 48",
        "srli t0, t0, 48",
        "addi t0, t0, -5",
        "beqz t0, {respond}",
        "csrr t0, sepc",
        "addi t0, t0, 4",
        "sd t0, {pc}(tp)",
        "sd ra, {ra}(tp)",
        "sd sp, {sp}(tp)",
        "sd gp, {gp}(tp)",
        "csrr t0, sscratch",
        "sd t0, {tp}(tp)",

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

        pc  = const { mem::offset_of!(module::ModuleContext, sepc) },
        ra  = const { mem::offset_of!(module::ModuleContext, registers) + 0o010 },
        sp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o020 },
        gp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o030 },
        tp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o040 },

        s0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o100 },
        s1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o110 },
        s2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o220 },
        s3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o230 },
        s4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o240 },
        s5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o250 },
        s6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o260 },
        s7  = const { mem::offset_of!(module::ModuleContext, registers) + 0o270 },
        s8  = const { mem::offset_of!(module::ModuleContext, registers) + 0o300 },
        s9  = const { mem::offset_of!(module::ModuleContext, registers) + 0o310 },
        s10 = const { mem::offset_of!(module::ModuleContext, registers) + 0o320 },
        s11 = const { mem::offset_of!(module::ModuleContext, registers) + 0o330 },

        stack = const { vmem::WINDOW_END },
        respond = sym respond,
        syscall = sym syscall,
    }
}

extern "C" fn handle_non_syscall_trap(cause: isize) -> ! {
    let local = unsafe { local_access() };
    let scheduler = unsafe { &*UnsafeCell::raw_get(&raw const __SCHEDULER) };
    if cause as usize == (1usize << 63) | 5 && scheduler.active() {
        let mut window = unsafe { vmem::WindowAccess::new(&raw const __WINDOW) };
        let selected = scheduler::preempt(&mut window, scheduler);
        resume_scheduled(&window, local, selected)
    }
    state::handle_non_syscall_trap(cause, local.thread);
    resume_after_interrupt()
}

extern "C" fn respond(a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> ! {
    let mut window = unsafe { vmem::WindowAccess::new(&raw const __WINDOW) };
    let local = unsafe { local_access() };
    let scheduler = unsafe { &*UnsafeCell::raw_get(&raw const __SCHEDULER) };

    let Ok(tau::Call::Respond { code }) = tau::Call::decode(a0) else {
        unsafe { hint::unreachable_unchecked() };
    };
    if let Some(caller) = local.module_context.take_caller() {
        let flush = mmu::switch_flush(&window, local.module_context.root, caller);
        let _local = unsafe { mmu::switch(local, caller, flush) };

        restore_syscall(code as _, a1, a2, a3, a4, a5)
    } else {
        respond_exit(&mut window, local, scheduler)
    }
}

#[cold]
#[inline(never)]
fn respond_exit(
    window: &mut vmem::WindowAccess,
    local: mmu::Local<'_>,
    scheduler: &scheduler::Scheduler,
) -> ! {
    scheduler::exit(window, scheduler);
    let selected = scheduler::dispatch(window, scheduler);
    resume_scheduled(window, local, selected)
}

extern "C" fn syscall(a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> ! {
    if let Ok(tau::Call::Invoke { slot, arg }) = tau::Call::decode(a0) {
        let window = unsafe { vmem::WindowAccess::new(&raw const __WINDOW) };
        let local = unsafe { local_access() };
        if let Some(root) = local.module_context.dependency(slot) {
            let caller = local.module_context.root;
            let flush = mmu::switch_flush(&window, caller, root);
            // The per-hart window/stack and global supervisor code retain the same
            // backing in both roots. Moving `local` ends all pre-switch borrows.
            let local = unsafe { mmu::switch(local, root, flush) };
            local.module_context.enter(caller);
            enter_invocation(arg as usize, a1, a2, a3, a4, a5)
        }
    }
    syscall_slow(a0, a1, a2, a3, a4, a5)
}

#[cold]
#[inline(never)]
extern "C" fn syscall_slow(
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> ! {
    let mut window = unsafe { vmem::WindowAccess::new(&raw const __WINDOW) };
    let local = unsafe { local_access() };
    if matches!(
        tau::Call::decode(a0),
        Ok(tau::Call::Spawn { .. } | tau::Call::Exit | tau::Call::Wait | tau::Call::Notify)
    ) {
        scheduled_syscall(&mut window, local, [a0, a1, a2, a3, a4, a5]);
    }
    let context = unsafe { &*UnsafeCell::raw_get(&raw const __CONTEXT) };
    let scheduler = unsafe { &*UnsafeCell::raw_get(&raw const __SCHEDULER) };
    let current_root = local.module_context.root;
    let outcome = state::syscall(
        &mut window,
        local.thread,
        local.module_context,
        local.module,
        scheduler,
        context,
        [a0, a1, a2, a3, a4, a5],
    );
    if let Some(root) = outcome.root {
        let flush = mmu::switch_flush(&window, current_root, root);
        // The per-hart window/stack and global supervisor code retain the same
        // backing in both roots. Moving `local` ends all pre-switch borrows.
        let local = unsafe { mmu::switch(local, root, flush) };
        if let Some(caller) = outcome.enter_from {
            local.module_context.enter(caller);
            let [a0, a1, a2, a3, a4, a5] = outcome.message;
            enter_invocation(a0, a1, a2, a3, a4, a5)
        }
    }
    let [a0, a1, a2, a3, a4, a5] = outcome.message;
    restore_syscall(a0, a1, a2, a3, a4, a5)
}

#[cold]
#[inline(never)]
fn scheduled_syscall(
    window: &mut vmem::WindowAccess,
    local: mmu::Local<'_>,
    message: [usize; 6],
) -> ! {
    let context = unsafe { &*UnsafeCell::raw_get(&raw const __CONTEXT) };
    let scheduler = unsafe { &*UnsafeCell::raw_get(&raw const __SCHEDULER) };
    let mut result = [0; 6];
    match tau::Call::decode(message[0]) {
        Ok(tau::Call::Spawn { entry }) => {
            match scheduler::spawn(
                window,
                local.thread,
                local.module,
                context,
                scheduler,
                entry,
                message,
            ) {
                Ok(id) => result[1] = id as usize,
                Err(error) => result[0] = error as usize,
            }
        }
        Ok(tau::Call::Wait) => {
            match scheduler::wait(window, local.module, scheduler, message[1], message[2]) {
                Ok(Some(event)) => result[1] = event.encode(),
                Ok(None) => {
                    let selected = scheduler::dispatch(window, scheduler);
                    resume_scheduled(window, local, selected)
                }
                Err(error) => result[0] = error as usize,
            }
        }
        Ok(tau::Call::Notify) => {
            if let Err(error) =
                scheduler::notify(window, scheduler, local.module.wait_table, message[1])
            {
                result[0] = error as usize;
            }
        }
        Ok(tau::Call::Exit) => {
            scheduler::exit(window, scheduler);
            let selected = scheduler::dispatch(window, scheduler);
            resume_scheduled(window, local, selected)
        }
        _ => {
            result[0] = tau::WaitError::Unsupported as usize;
        }
    }
    let [a0, a1, a2, a3, a4, a5] = result;
    restore_syscall(a0, a1, a2, a3, a4, a5)
}

fn resume_scheduled(
    window: &vmem::WindowAccess,
    local: mmu::Local<'_>,
    selected: scheduler::Dispatch,
) -> ! {
    let source = vmem::Window::current_root();
    let _local = if source != selected.root {
        let flush = mmu::switch_flush(window, source, selected.root);
        unsafe { mmu::switch(local, selected.root, flush) }
    } else {
        local
    };
    // The idle path and timer entry both return to U-mode with interrupts on.
    supervisor::cpu::csrrc!("sstatus", 1 << 8);
    supervisor::cpu::csrrs!("sstatus", 1 << 5);
    match selected.kind {
        scheduler::ReturnKind::Interrupt => resume_after_interrupt(),
        scheduler::ReturnKind::Entry => {
            let [a0, a1, a2, a3, a4, a5] = selected.message;
            enter_invocation(a0, a1, a2, a3, a4, a5)
        }
        scheduler::ReturnKind::Syscall => {
            let [a0, a1, a2, a3, a4, a5] = selected.message;
            restore_syscall(a0, a1, a2, a3, a4, a5)
        }
    }
}

/// Enter a fresh invocation with only the message registers populated.
/// The next user trap saves any register state required for resumption.
#[inline(always)]
fn enter_invocation(a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> ! {
    unsafe {
        arch::asm! {
            "ld t0, {pc}(tp)",
            "csrw sepc, t0",
            "csrw sscratch, zero",
            "mv ra, zero",
            "mv sp, zero",
            "mv gp, zero",
            "mv s0, zero",
            "mv s1, zero",
            "mv s2, zero",
            "mv s3, zero",
            "mv s4, zero",
            "mv s5, zero",
            "mv s6, zero",
            "mv s7, zero",
            "mv s8, zero",
            "mv s9, zero",
            "mv s10, zero",
            "mv s11, zero",
            "mv t0, zero",
            "mv t1, zero",
            "mv t2, zero",
            "mv t3, zero",
            "mv t4, zero",
            "mv t5, zero",
            "mv t6, zero",
            "mv a6, zero",
            "mv a7, zero",
            // Install user tp = 0 and retain the context pointer in sscratch.
            "csrrw tp, sscratch, tp",
            "sret",
            options(noreturn),
            in("a0") a0,
            in("a1") a1,
            in("a2") a2,
            in("a3") a3,
            in("a4") a4,
            in("a5") a5,
            pc = const { mem::offset_of!(module::ModuleContext, sepc) },
        }
    }
}

/// Restore the active context with return values already in `a0`–`a5`.
/// Called through the C ABI by Rust dispatch, or jumped to after Respond.
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
            "ld t0, {pc}(tp)",
            "csrw sepc, t0",
            "ld t0, {tp}(tp)",
            "csrw sscratch, t0",
            "ld gp, {gp}(tp)",
            "mv t0, zero",
            "mv t1, zero",
            "mv t2, zero",
            "mv t3, zero",
            "mv t4, zero",
            "mv t5, zero",
            "mv t6, zero",
            "mv a6, zero",
            "mv a7, zero",
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
            pc  = const { mem::offset_of!(module::ModuleContext, sepc) },
            gp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o030 },
            tp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o040 },
            ra  = const { mem::offset_of!(module::ModuleContext, registers) + 0o010 },
            sp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o020 },

            s0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o100 },
            s1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o110 },
            s2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o220 },
            s3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o230 },
            s4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o240 },
            s5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o250 },
            s6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o260 },
            s7  = const { mem::offset_of!(module::ModuleContext, registers) + 0o270 },
            s8  = const { mem::offset_of!(module::ModuleContext, registers) + 0o300 },
            s9  = const { mem::offset_of!(module::ModuleContext, registers) + 0o310 },
            s10 = const { mem::offset_of!(module::ModuleContext, registers) + 0o320 },
            s11 = const { mem::offset_of!(module::ModuleContext, registers) + 0o330 },
        }
    }
}

extern "C" fn resume_after_interrupt() -> ! {
    unsafe {
        arch::asm! {
            "ld t0, {pc}(tp)",
            "csrw sepc, t0",
            "ld ra, {ra}(tp)",
            "ld sp, {sp}(tp)",
            "ld gp, {gp}(tp)",
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
            "ld t0, {tp}(tp)",
            "csrw sscratch, t0",
            "ld t0, {t0}(tp)",
            "csrrw tp, sscratch, tp",
            "sret",
            options(noreturn),
            ra  = const { mem::offset_of!(module::ModuleContext, registers) + 0o010 },
            sp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o020 },
            tp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o040 },
            gp  = const { mem::offset_of!(module::ModuleContext, registers) + 0o030 },
            t0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o050 },
            t1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o060 },
            t2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o070 },
            s0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o100 },
            s1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o110 },
            a0  = const { mem::offset_of!(module::ModuleContext, registers) + 0o120 },
            a1  = const { mem::offset_of!(module::ModuleContext, registers) + 0o130 },
            a2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o140 },
            a3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o150 },
            a4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o160 },
            a5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o170 },
            a6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o200 },
            a7  = const { mem::offset_of!(module::ModuleContext, registers) + 0o210 },
            s2  = const { mem::offset_of!(module::ModuleContext, registers) + 0o220 },
            s3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o230 },
            s4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o240 },
            s5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o250 },
            s6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o260 },
            s7  = const { mem::offset_of!(module::ModuleContext, registers) + 0o270 },
            s8  = const { mem::offset_of!(module::ModuleContext, registers) + 0o300 },
            s9  = const { mem::offset_of!(module::ModuleContext, registers) + 0o310 },
            s10 = const { mem::offset_of!(module::ModuleContext, registers) + 0o320 },
            s11 = const { mem::offset_of!(module::ModuleContext, registers) + 0o330 },
            t3  = const { mem::offset_of!(module::ModuleContext, registers) + 0o340 },
            t4  = const { mem::offset_of!(module::ModuleContext, registers) + 0o350 },
            t5  = const { mem::offset_of!(module::ModuleContext, registers) + 0o360 },
            t6  = const { mem::offset_of!(module::ModuleContext, registers) + 0o370 },
            pc = const { mem::offset_of!(module::ModuleContext, sepc) },
        }
    }
}
