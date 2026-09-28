// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![no_main]

use core::{
    arch::{asm, naked_asm},
    hint,
    sync::atomic::{AtomicUsize, Ordering},
};

mod clock;
mod ddr;
mod ddr_data;
mod mmio;
mod sd;

// ROM-loaded flag: 1 while hart 1 initializes the board; 3 releases secondaries.
#[unsafe(link_section = ".data.boot")]
static BOOT_STATE: AtomicUsize = AtomicUsize::new(1);

unsafe extern "C" {
    static mut __bss_start: u64;
    static mut __bss_end: u64;
}

#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.init")]
extern "C" fn _start() -> ! {
    naked_asm!(
        include_str!("uart_early.S"),
        include_str!("start.S"),
        ".purgem tau_uart_init",
        main = sym start,
    )
}

extern "C" fn start(hart: usize, boot_owner: usize) -> ! {
    if boot_owner == 0 {
        unsafe { asm!("csrw 0x7c1, zero") };
        while BOOT_STATE.load(Ordering::Acquire) != 3 {
            hint::spin_loop();
        }
        jump(hart)
    }
    unsafe { asm!("csrw 0x7c1, zero") };
    let mut p = &raw mut __bss_start as *mut u64;
    let end = &raw mut __bss_end as *mut u64;
    while p < end {
        unsafe {
            p.write_volatile(0);
            p = p.add(1);
        }
    }
    puts(b"Tau SPL: clocks\r\n");
    clock::early();
    clock::uart0_rx();
    puts(b"Tau SPL: DDR\r\n");
    ddr::init();
    puts(b"Tau SPL: SD\r\n");
    clock::sdio1();
    let size = match sd::load_firmware() {
        Ok(size) => size,
        Err(_) => {
            puts(b"Tau SPL: SD failed\r\n");
            halt()
        }
    };
    if size == 0 {
        halt();
    }
    puts(b"Tau SPL: firmware\r\n");
    // `puts` drains UART0. Only release secondaries after the final SPL byte
    // has left the transmitter; otherwise their RustSBI banner races this
    // message on the same UART.
    unsafe { asm!("fence rw, rw") };
    BOOT_STATE.store(3, Ordering::Release);
    jump(hart)
}

fn jump(hart: usize) -> ! {
    unsafe { asm!("fence.i") };
    let entry: extern "C" fn(usize, usize) -> ! = unsafe { core::mem::transmute(0x4000_0000usize) };
    entry(hart, 0)
}

fn delay_us(us: u64) {
    let start = unsafe { (0x0200_bff8 as *const u64).read_volatile() };
    while unsafe { (0x0200_bff8 as *const u64).read_volatile() }.wrapping_sub(start) < us * 4 {
        hint::spin_loop();
    }
}

fn puts(bytes: &[u8]) {
    for &byte in bytes {
        while mmio::read(0x1000_0000 + 5 * 4) & (1 << 5) == 0 {
            hint::spin_loop();
        }
        mmio::write(0x1000_0000, byte as u32);
    }
    // Drain before clock changes or firmware handoff.
    while mmio::read(0x1000_0000 + 5 * 4) & (1 << 6) == 0 {
        hint::spin_loop();
    }
}

fn halt() -> ! {
    loop {
        unsafe { asm!("wfi") }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    halt()
}
