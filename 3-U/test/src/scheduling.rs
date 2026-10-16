// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! End-to-end scheduling and software-wait checks on the normal supervisor.
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering::SeqCst},
};
use tau::{Event, ModuleHandle, Ubi, WaitError, WaitSlot};
use super::{tau_test_passed, tau_test_progress};

pub(super) static SLOTS: [WaitSlot; 8] = [const { WaitSlot::new() }; 8];
static TURN: AtomicUsize = AtomicUsize::new(0);
static ACK: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static RACE_DEADLINE: AtomicUsize = AtomicUsize::new(0);
static RUN_WORKER: AtomicUsize = AtomicUsize::new(1);
static COMPUTE_STARTED: AtomicUsize = AtomicUsize::new(0);
static COMPUTED: AtomicUsize = AtomicUsize::new(0);

fn pointer(index: usize) -> *const WaitSlot {
    &SLOTS[index]
}
fn reserve(index: usize) {
    assert!(
        SLOTS[index]
            .occupied
            .compare_exchange(false, true, SeqCst, SeqCst)
            .is_ok()
    );
}
fn release(index: usize) {
    SLOTS[index].occupied.store(false, SeqCst);
}
fn notified(index: usize) {
    assert_eq!(
        Ubi::wait(pointer(index), None),
        Ok(Event::Notified { arg: 0 })
    );
}
fn expired() -> Option<NonZeroUsize> {
    NonZeroUsize::new(1)
}
fn deadline(ticks: usize) -> Option<NonZeroUsize> {
    NonZeroUsize::new(tau::asm::read_time() + ticks)
}
fn wait_done(value: usize) {
    while DONE.load(SeqCst) != value {
        notified(0);
    }
}

extern "C" fn worker(_: usize, role: usize, _: usize, _: usize, _: usize, _: usize) -> ! {
    match role {
        1 => {
            for count in 1..=128 {
                while TURN.load(SeqCst) != count {
                    notified(1);
                }
                ACK.store(count, SeqCst);
                Ubi::notify(pointer(0)).unwrap();
            }
        }
        2 => {
            notified(2);
        }
        3 => {
            reserve(3);
        }
        4 => {
            COMPUTE_STARTED.store(1, SeqCst);
            assert_eq!(registers(), 0);
            COMPUTED.store(1, SeqCst);
        }
        5 => {
            Ubi::notify(pointer(4)).unwrap();
        }
        6 => {
            for count in 1..=16 {
                while TURN.load(SeqCst) != count {
                    notified(1);
                }
                let until = RACE_DEADLINE.load(SeqCst);
                let target = if count & 1 == 0 {
                    until - 10_000
                } else {
                    until + 10_000
                };
                while tau::asm::read_time() < target {
                    core::hint::spin_loop();
                }
                Ubi::notify(pointer(5)).unwrap();
                ACK.store(count, SeqCst);
                Ubi::notify(pointer(0)).unwrap();
            }
        }
        _ => panic!("worker role"),
    }
    DONE.store(role, SeqCst);
    Ubi::notify(pointer(0)).unwrap();
    Ubi::exit([])
}

// Exercise all register classes across several timer interrupts. Only t0 is a
// loop counter; every other caller-saved register must survive preemption too.
#[unsafe(naked)]
extern "C" fn registers() -> usize {
    core::arch::naked_asm!(
        "addi sp, sp, -128",
        "sd ra, 0(sp)", "sd gp, 8(sp)", "sd tp, 16(sp)",
        "sd s0, 24(sp)", "sd s1, 32(sp)", "sd s2, 40(sp)",
        "sd s3, 48(sp)", "sd s4, 56(sp)", "sd s5, 64(sp)",
        "sd s6, 72(sp)", "sd s7, 80(sp)", "sd s8, 88(sp)",
        "sd s9, 96(sp)", "sd s10, 104(sp)", "sd s11, 112(sp)",
        "li ra, 123", "li gp, 123", "li tp, 123",
        "li s0, 123", "li s1, 123", "li s2, 123", "li s3, 123",
        "li s4, 123", "li s5, 123", "li s6, 123", "li s7, 123",
        "li s8, 123", "li s9, 123", "li s10, 123", "li s11, 123",
        "li a0, 123", "li a1, 123", "li a2, 123", "li a3, 123",
        "li a4, 123", "li a5, 123", "li a6, 123", "li a7, 123",
        "li t1, 123", "li t2, 123", "li t3, 123", "li t4, 123", "li t5, 123", "li t6, 123",
        "2: la t0, {run}", "ld t0, 0(t0)", "bnez t0, 2b",
        "li t0, 123",
        "bne ra, t0, 3f", "bne gp, t0, 3f", "bne tp, t0, 3f",
        "bne s0, t0, 3f", "bne s1, t0, 3f", "bne s2, t0, 3f", "bne s3, t0, 3f",
        "bne s4, t0, 3f", "bne s5, t0, 3f", "bne s6, t0, 3f", "bne s7, t0, 3f",
        "bne s8, t0, 3f", "bne s9, t0, 3f", "bne s10, t0, 3f", "bne s11, t0, 3f",
        "bne a0, t0, 3f", "bne a1, t0, 3f", "bne a2, t0, 3f", "bne a3, t0, 3f",
        "bne a4, t0, 3f", "bne a5, t0, 3f", "bne a6, t0, 3f", "bne a7, t0, 3f",
        "bne t1, t0, 3f", "bne t2, t0, 3f", "bne t3, t0, 3f",
        "bne t4, t0, 3f", "bne t5, t0, 3f", "bne t6, t0, 3f",
        "li a0, 0", "j 4f", "3: li a0, 1", "4:",
        "ld ra, 0(sp)", "ld gp, 8(sp)", "ld tp, 16(sp)",
        "ld s0, 24(sp)", "ld s1, 32(sp)", "ld s2, 40(sp)",
        "ld s3, 48(sp)", "ld s4, 56(sp)", "ld s5, 64(sp)",
        "ld s6, 72(sp)", "ld s7, 80(sp)", "ld s8, 88(sp)",
        "ld s9, 96(sp)", "ld s10, 104(sp)", "ld s11, 112(sp)",
        "addi sp, sp, 128", "ret",
        run = sym RUN_WORKER,
    );
}

fn spawn_raw(entry: usize) -> [usize; 6] {
    let mut words = [
        entry,
        usize::MAX,
        usize::MAX,
        usize::MAX,
        usize::MAX,
        usize::MAX,
    ];
    unsafe {
        core::arch::asm!("ecall",
        inout("a0") words[0], inout("a1") words[1], inout("a2") words[2],
        inout("a3") words[3], inout("a4") words[4], inout("a5") words[5],
        clobber_abi("C"), options(nostack));
    }
    words
}

pub(super) fn run(case: usize, info: usize, pages: usize) -> ! {
    let raw = tau::Area::new(info, pages << 12).sl();
    let (dtb, _) = tau::Dtb::new(raw).unwrap();
    let image = dtb.boot_module("dummy-test").unwrap().sl::<u8>();
    let (a, _) = Ubi::prepare_module(ModuleHandle::ROOT, image).unwrap();
    let (b, _) = Ubi::prepare_module(ModuleHandle::ROOT, image).unwrap();
    tau_test_progress(case, pointer(0).addr(), SLOTS.len());

    Ubi::notify(pointer(0)).unwrap(); // valid free slots are no-ops
    assert_eq!(
        Ubi::wait(pointer(0), expired()),
        Err(WaitError::InvalidWaiter)
    );
    for address in [
        0,
        pointer(0).addr() + 1,
        pointer(0).addr() + 63,
        pointer(0).addr() + 64 * SLOTS.len(),
        usize::MAX,
    ] {
        assert_eq!(
            Ubi::wait(address as *const WaitSlot, expired()),
            Err(WaitError::InvalidWaiter)
        );
        assert_eq!(
            Ubi::notify(address as *const WaitSlot),
            Err(WaitError::InvalidWaiter)
        );
    }
    let local = WaitSlot::new();
    assert_eq!(Ubi::wait(&local, expired()), Err(WaitError::InvalidWaiter));
    assert!(Ubi::unmap(pointer(0).addr() & !4095, 1).is_err());
    assert!(matches!(
        Ubi::map(None, pointer(0).addr() & !4095, 1),
        Err(tau::AllocError::AccessDenied)
    ));
    for entry in [0, pointer(0).addr(), 0xffff_ffc0_0020_0000, 0x10_0000_0000] {
        assert_eq!(
            spawn_raw(entry),
            [tau::SpawnError::InvalidEntry as usize, 0, 0, 0, 0, 0]
        );
    }
    reserve(0);
    assert_eq!(Ubi::wait(pointer(0), expired()), Ok(Event::Timeout));
    for _ in 0..3 {
        Ubi::notify(pointer(0)).unwrap();
    }
    assert_eq!(
        Ubi::wait(pointer(0), expired()),
        Ok(Event::Notified { arg: 0 })
    );
    assert_eq!(Ubi::wait(pointer(0), expired()), Ok(Event::Timeout));
    Ubi::notify(pointer(0)).unwrap();
    release(0);
    reserve(0);
    notified(0); // a stale hint may survive slot reuse

    // No Rust bool load sees this hostile representation. The supervisor must
    // read an atomic byte and reject it, then userspace restores its reservation.
    unsafe {
        core::arch::asm!("sb {value}, 63({slot})", value = in(reg) 2usize, slot = in(reg) pointer(0), options(nostack));
    }
    assert_eq!(Ubi::notify(pointer(0)), Err(WaitError::InvalidWaiter));
    assert_eq!(
        Ubi::wait(pointer(0), expired()),
        Err(WaitError::InvalidWaiter)
    );
    unsafe {
        core::arch::asm!("sb {value}, 63({slot})", value = in(reg) 1usize, slot = in(reg) pointer(0), options(nostack));
    }

    // Identical numeric slot addresses in separate prepared instances have
    // independent protected pending state.
    let address = Ubi::invoke::<0, 1>(a, 10, []).unwrap()[0];
    assert_eq!(
        Ubi::invoke::<0, 1>(b, 11, []).unwrap(),
        [Event::Timeout.encode()]
    );
    assert_eq!(
        Ubi::invoke::<0, 1>(a, 11, []).unwrap(),
        [Event::Notified { arg: 0 }.encode()]
    );
    assert_eq!(
        Ubi::wait(address as *const WaitSlot, expired()),
        Err(WaitError::InvalidWaiter)
    );

    // Single-thread timeout exercises idle entry and return without another
    // runnable thread. This also enables scheduled mode.
    let until = deadline(20_000).unwrap();
    assert_eq!(Ubi::wait(pointer(0), Some(until)), Ok(Event::Timeout));
    assert!(tau::asm::read_time() >= until.get());

    reserve(1);
    Ubi::spawn(worker, [1]).unwrap();
    for count in 1..=128 {
        TURN.store(count, SeqCst);
        Ubi::notify(pointer(1)).unwrap();
        while ACK.load(SeqCst) != count {
            notified(0);
        }
    }
    wait_done(1);
    release(1);

    reserve(2);
    Ubi::spawn(worker, [2]).unwrap();
    loop {
        match Ubi::wait(pointer(2), expired()) {
            Err(WaitError::Busy) => break,
            Ok(Event::Timeout) => core::hint::spin_loop(),
            other => panic!("competing wait: {other:?}"),
        }
    }
    Ubi::notify(pointer(2)).unwrap();
    wait_done(2);
    release(2);

    Ubi::spawn(worker, [3]).unwrap();
    wait_done(3);
    assert!(SLOTS[3].occupied.load(SeqCst)); // Exit must not release reservations
    Ubi::notify(pointer(3)).unwrap();
    notified(3);
    release(3);

    reserve(6);
    Ubi::spawn(worker, [4]).unwrap();
    while COMPUTE_STARTED.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }
    // The worker performs no syscalls while the parent's deadline expires.
    assert_eq!(Ubi::wait(pointer(6), deadline(20_000)), Ok(Event::Timeout));
    assert_eq!(COMPUTED.load(SeqCst), 0);
    RUN_WORKER.store(0, SeqCst);
    while COMPUTED.load(SeqCst) == 0 {
        core::hint::spin_loop();
    }
    wait_done(4);
    release(6);

    reserve(4);
    Ubi::spawn(worker, [5]).unwrap();
    assert_eq!(
        Ubi::wait(pointer(4), deadline(500_000)),
        Ok(Event::Notified { arg: 0 })
    );
    wait_done(5);
    let until = deadline(1_000_000).unwrap();
    assert_eq!(Ubi::wait(pointer(4), Some(until)), Ok(Event::Timeout));
    assert!(tau::asm::read_time() >= until.get()); // old timer cannot finish a new wait
    release(4);

    // Alternate notification around a live deadline. Either winner is legal;
    // a late notification remains pending and must not complete a later wait.
    reserve(1);
    reserve(5);
    TURN.store(0, SeqCst);
    ACK.store(0, SeqCst);
    Ubi::spawn(worker, [6]).unwrap();
    for count in 1..=16 {
        let until = deadline(50_000).unwrap();
        RACE_DEADLINE.store(until.get(), SeqCst);
        TURN.store(count, SeqCst);
        Ubi::notify(pointer(1)).unwrap();
        assert!(matches!(
            Ubi::wait(pointer(5), Some(until)),
            Ok(Event::Notified { arg: 0 } | Event::Timeout)
        ));
        while ACK.load(SeqCst) != count {
            notified(0);
        }
        assert!(Ubi::wait(pointer(5), expired()).is_ok());
        assert_eq!(Ubi::wait(pointer(5), expired()), Ok(Event::Timeout));
    }
    wait_done(6);
    release(1);
    release(5);

    // A new thread in the invoked module notifies its suspended client. Returning
    // then restores the original bootstrap caller and its private stack.
    assert_eq!(
        Ubi::invoke::<1, 1>(a, 9, [0x12345678]).unwrap(),
        [0x12345678]
    );
    let marker = [0xabcdefusize; 8];
    for word in &marker {
        assert_eq!(unsafe { core::ptr::read_volatile(word) }, 0xabcdef);
    }
    release(0);
    tau_test_passed(case, 128)
}

pub(super) fn rollback(case: usize) -> ! {
    // The debugger exhausts the ASID allocator at the first checkpoint, then
    // restores it after checking that failed construction reclaimed every page.
    tau_test_progress(case, 0, 0);
    assert_eq!(Ubi::spawn(worker, [3]), Err(tau::SpawnError::OutOfMemory));
    tau_test_progress(case, 0, 1);
    reserve(0);
    let id = Ubi::spawn(worker, [3]).unwrap();
    assert_eq!(id, 2); // failed creation consumed neither registry slot nor ASID
    wait_done(3);
    assert!(SLOTS[3].occupied.load(SeqCst));
    release(3);
    release(0);
    tau_test_passed(case, usize::from(id))
}
