// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_main]
#![no_std]

include!("../firmware.rs");

mod board {
    use super::*;

    // JH7110 hart 0 is the disabled S7 monitor core. Tau runs on U74 harts 1-4.
    pub(super) const FIRST_HART: usize = 1;
    pub(super) const MAX_HARTS: usize = 5;

    pub(super) const BOOT_MESSAGE: &[u8] = b"Tau RustSBI (VisionFive 2), hart=";
    const UART_LINE_STATUS: usize = 5;
    const UART_TX_IDLE: u32 = 1 << 5;
    const CLINT_MTIME: usize = 0xbff8;

    // CSRRS rd, time, x0, with the destination-register field omitted.
    const RDTIME_MASK: usize = 0xffff_f07f;
    const RDTIME_INSTRUCTION: usize = 0xc010_2073;

    #[used]
    #[unsafe(link_section = ".dtb")]
    pub(super) static DTB: [u8; include_bytes!(
        "../../boards/vf2/jh7110-starfive-visionfive-2-v1.3b.dtb"
    )
    .len()] = *include_bytes!("../../boards/vf2/jh7110-starfive-visionfive-2-v1.3b.dtb");

    unsafe extern "C" {
        static __UART: u8;
    }

    #[derive(rustsbi::RustSBI)]
    pub(super) struct Sbi {
        console: FirmwareConsole,
        timer: FirmwareTimer,
        hsm: FirmwareHsm,
    }

    pub(super) static SBI: Sbi = Sbi {
        console: FirmwareConsole,
        timer: FirmwareTimer,
        hsm: FirmwareHsm,
    };

    pub(super) fn valid_hart(hart_id: usize) -> bool {
        (FIRST_HART..MAX_HARTS).contains(&hart_id)
    }

    pub(super) fn handle_illegal_instruction(frame: &mut TrapFrame) -> bool {
        // The U74 traps RDTIME to M-mode even when mcounteren/scounteren permit it.
        let instruction = read_csr::<0x343>();
        if instruction & RDTIME_MASK != RDTIME_INSTRUCTION {
            return false;
        }

        let rd = instruction >> 7 & 0x1f;
        if rd != 0 {
            frame.registers[rd] = unsafe {
                ((symbol_addr(&raw const __CLINT) + CLINT_MTIME) as *const u64).read_volatile()
                    as usize
            };
        }
        write_csr::<0x341>(read_csr::<0x341>() + 4);
        true
    }

    pub(super) fn uart_write(byte: u8) {
        // JH7110 UART0 is a DW APB UART with reg-shift=2 and reg-io-width=4.
        let uart = symbol_addr(&raw const __UART) as *mut u32;
        while unsafe { uart.add(UART_LINE_STATUS).read_volatile() } & UART_TX_IDLE == 0 {
            hint::spin_loop();
        }
        unsafe { uart.write_volatile(u32::from(byte)) };
    }
}
