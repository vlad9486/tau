#![no_main]
#![no_std]

include!("../firmware.rs");

mod board {
    use super::*;
    use rustsbi::Reset;

    pub(super) const FIRST_HART: usize = 0;
    pub(super) const MAX_HARTS: usize = 8;

    pub(super) const BOOT_MESSAGE: &[u8] = b"Tau RustSBI (QEMU), hart=";
    const UART_LINE_STATUS: usize = 5;
    const UART_TX_IDLE: u8 = 1 << 5;
    const RESET_SHUTDOWN: u32 = 0x5555;
    const RESET_REBOOT: u32 = 0x7777;

    #[used]
    #[unsafe(link_section = ".dtb")]
    pub(super) static DTB: [u8; include_bytes!("../../boards/qemu/virt.dtb").len()] =
        *include_bytes!("../../boards/qemu/virt.dtb");

    unsafe extern "C" {
        static __UART: u8;
        static __RESET: u8;
    }

    #[derive(rustsbi::RustSBI)]
    pub(super) struct Sbi {
        console: FirmwareConsole,
        timer: FirmwareTimer,
        hsm: FirmwareHsm,
        reset: QemuReset,
    }

    pub(super) static SBI: Sbi = Sbi {
        console: FirmwareConsole,
        timer: FirmwareTimer,
        hsm: FirmwareHsm,
        reset: QemuReset,
    };

    struct QemuReset;

    impl Reset for QemuReset {
        fn system_reset(&self, reset_type: u32, _reset_reason: u32) -> SbiRet {
            let value = match reset_type {
                0 => RESET_SHUTDOWN,
                1 | 2 => RESET_REBOOT,
                _ => return SbiRet::invalid_param(),
            };
            unsafe { (symbol_addr(&raw const __RESET) as *mut u32).write_volatile(value) };
            loop {
                hint::spin_loop();
            }
        }
    }

    pub(super) fn valid_hart(hart_id: usize) -> bool {
        (FIRST_HART..MAX_HARTS).contains(&hart_id)
    }

    pub(super) fn handle_illegal_instruction(_frame: &mut TrapFrame) -> bool {
        false
    }

    pub(super) fn uart_write(byte: u8) {
        let uart = symbol_addr(&raw const __UART) as *mut u8;
        while unsafe { uart.add(UART_LINE_STATUS).read_volatile() } & UART_TX_IDLE == 0 {
            hint::spin_loop();
        }
        unsafe { uart.write_volatile(byte) };
    }
}
