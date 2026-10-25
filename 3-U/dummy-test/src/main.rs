// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]

use core::{
    arch,
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

#[unsafe(no_mangle)]
static MANIFEST: tau::Manifest = tau::Manifest {
    this: tau::ModuleId {
        version: (0, 1),
        name: *b"dummy-test\0\0\0\0\0\0\0\0\0\0",
    },
    wait_slots: &WAIT_SLOTS,
    entry: dummy_entry,
    dependencies: &[],
    mapped_regions: &[tau::MappedRegion::stack(0x1000)],
};

static WAIT_SLOTS: [tau::WaitSlot; 4] = [const { tau::WaitSlot::new() }; 4];
static WAIT_READY: AtomicUsize = AtomicUsize::new(0);

extern "C" fn notify_worker(_: usize, _: usize, _: usize, _: usize, _: usize, _: usize) -> ! {
    WAIT_READY.store(1, Ordering::SeqCst);
    tau::Ubi::notify(&WAIT_SLOTS[0]).unwrap();
    tau::Ubi::exit()
}

// BSS starts at zero and remains shared across separate invocations.
static CALLS: AtomicUsize = AtomicUsize::new(0);
static INITIALIZED: AtomicUsize = AtomicUsize::new(0x5344_494f);

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    tau::Ubi::respond(9, [])
}

#[unsafe(no_mangle)]
#[unsafe(naked)]
extern "C" fn dummy_entry(_: usize, _: usize, _: usize, _: usize, _: usize, _: usize) -> ! {
    arch::naked_asm!(
        "li s0, 500",
        "li s1, 501",
        "li s2, 502",
        "li s3, 503",
        "li s4, 504",
        "li s5, 505",
        "li s6, 506",
        "li s7, 507",
        "li s8, 508",
        "li s9, 509",
        "li s10, 510",
        "li s11, 511",
        "li gp, 12345",
        "li tp, 67890",
        "j {main}",
        main = sym dummy_main,
    );
}

extern "C" fn dummy_main(
    event: usize,
    value: usize,
    second: usize,
    third: usize,
    fourth: usize,
    fifth: usize,
) -> ! {
    let arg = event as u16;
    match arg {
        0 => {
            let invoke_end = tau::asm::read_cycle();
            let count = count_call();
            let value = value.rotate_left(7) ^ 0x5344_494f;
            let respond_start = tau::asm::read_cycle();
            tau::Ubi::respond(0, [value, count, invoke_end, respond_start])
        }
        1 => {
            // Authority belongs to this module, not to the invoking thread.
            let denied = matches!(
                tau::Ubi::map(NonZeroUsize::new(value), 0x1_0000_0000, 1),
                Err(tau::AllocError::AccessDenied)
            ) && matches!(
                tau::Ubi::alloc_physical(0, tau::MemoryZone::Normal),
                Err(tau::AllocError::AccessDenied)
            ) && matches!(
                tau::Ubi::free_physical(value, 0, tau::MemoryZone::Normal),
                Err(tau::FreeError::AccessDenied)
            ) && matches!(
                tau::Ubi::create_module(0x1000, 1, 0, 0),
                Err(tau::ModuleError::AccessDenied)
            ) && matches!(
                tau::Ubi::map(None, 0xffff_ffc0_0020_0000, 1),
                Err(tau::AllocError::InvalidAddress)
            ) && matches!(
                tau::Ubi::map(None, 0x10000, 1),
                Err(tau::AllocError::AccessDenied)
            );
            if !denied {
                tau::Ubi::respond(3, []);
            }
            // Anonymous private mappings survive return to the caller.
            if tau::Ubi::map(None, 0x1_0000_0000, 2).is_err() {
                tau::Ubi::respond(4, []);
            }
            for offset in [0, 0xff8, 0x1000, 0x1ff8] {
                let pointer = (0x1_0000_0000 + offset) as *mut usize;
                unsafe {
                    if pointer.read_volatile() != 0 {
                        tau::Ubi::respond(5, []);
                    }
                    pointer.write_volatile(value ^ offset);
                }
            }
            if !matches!(
                tau::Ubi::map(None, 0x1_0000_0000, 1),
                Err(tau::AllocError::AlreadyAllocated)
            ) {
                tau::Ubi::respond(6, []);
            }
            tau::Ubi::respond(0, [1])
        }
        2 => tau::Ubi::respond(42, []),
        3 => tau::Ubi::respond(0, [value ^ 0x5344_494f]),
        4 => {
            for offset in [0, 0xff8, 0x1000, 0x1ff8] {
                let pointer = (0x1_0000_0000 + offset) as *const usize;
                if unsafe { pointer.read_volatile() } != value ^ offset {
                    tau::Ubi::respond(8, []);
                }
            }
            if !matches!(
                tau::Ubi::map(None, 0x1_0000_0000, 1),
                Err(tau::AllocError::AlreadyAllocated)
            ) {
                tau::Ubi::respond(6, []);
            }
            tau::Ubi::respond(0, [1])
        }
        5 => {
            let response = tau::Ubi::invoke::<1, 2>(1, 0, [value]).unwrap();
            tau::Ubi::respond(0, response)
        }
        6 => tau::Ubi::respond(0, [value, second, third, fourth, fifth]),
        7 => {
            // Route words contain parent-local slots in successive 16-bit fields.
            // Pack each module's shared counter into the response in path order.
            let count = count_call();
            let (reply, counts) = if third == 0 {
                (value.rotate_left(7) ^ 0x5344_494f, 0)
            } else {
                let [reply, counts] =
                    tau::Ubi::invoke::<3, 2>(second as u16, 7, [value, second >> 16, third - 1])
                        .unwrap();
                (reply, counts)
            };
            tau::Ubi::respond(0, [reply, counts | (count << (third * 16))])
        }
        8 => {
            let parent = tau::ModuleHandle::from_raw(value);
            let child = tau::ModuleHandle::from_raw(second);
            let denied = matches!(
                tau::Ubi::create_module(0x1000, 1, 0, 0),
                Err(tau::ModuleError::AccessDenied)
            ) && matches!(
                tau::Ubi::bind_dependency(parent, child),
                Err(tau::ModuleError::AccessDenied)
            );
            tau::Ubi::respond(0, [denied as usize])
        }
        9 => {
            WAIT_READY.store(0, Ordering::SeqCst);
            WAIT_SLOTS[0].occupied.store(true, Ordering::SeqCst);
            tau::Ubi::spawn(notify_worker, []).unwrap();
            while WAIT_READY.load(Ordering::SeqCst) == 0 {
                assert_eq!(
                    tau::Ubi::wait(&WAIT_SLOTS[0], None).unwrap(),
                    tau::Event::Notified { arg: 0 }
                );
            }
            WAIT_SLOTS[0].occupied.store(false, Ordering::SeqCst);
            tau::Ubi::respond(0, [value])
        }
        10 => {
            WAIT_SLOTS[1].occupied.store(true, Ordering::SeqCst);
            tau::Ubi::notify(&WAIT_SLOTS[1]).unwrap();
            tau::Ubi::respond(0, [(&raw const WAIT_SLOTS[1]).addr()])
        }
        11 => {
            WAIT_SLOTS[1].occupied.store(true, Ordering::SeqCst);
            let event = tau::Ubi::wait(&WAIT_SLOTS[1], NonZeroUsize::new(1)).unwrap();
            tau::Ubi::respond(0, [event.encode()])
        }
        12 => {
            let ptr = 0x20_0000_0000 as *const usize;
            let val = unsafe { ptr.read_volatile() };
            tau::Ubi::respond(0, [val])
        }
        _ => tau::Ubi::respond(7, []),
    }
}

fn count_call() -> usize {
    let count = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if INITIALIZED.fetch_add(1, Ordering::Relaxed) != 0x5344_494f + count - 1 {
        tau::Ubi::respond(2, []);
    }
    count
}
