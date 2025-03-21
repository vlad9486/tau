//! Work in progress

use core::{arch, fmt, hint};

use super::{
    plic::{PlicThresholdClaim, InterruptNumber},
    register::Register,
};

#[repr(C, align(0x1000))]
pub struct Device {
    ctrl: Register<u32, u32>,
    _pwren: Register<u32, u32>,
    clkdiv: Register<u32, u32>,
    clksrc: Register<u32, u32>,
    clkena: Register<u32, u32>,
    tmout: Register<u32, u32>,
    _ctype: Register<u32, u32>,
    blksiz: Register<u32, u32>,
    bytcnt: Register<u32, u32>,
    int_mask: Register<u32, u32>,
    cmd_arg: Register<u32, u32>,
    cmd: Register<u32, u32>,
    resp0: Register<u32, u32>,
    resp1: Register<u32, u32>,
    resp2: Register<u32, u32>,
    resp3: Register<u32, u32>,
    mask_int_status: Register<u32, u32>,
    raw_int_status: Register<u32, u32>,
    status: Register<u32, u32>,
    fifo_threshold: Register<u32, u32>,
    _h0: [u32; 0xc],
    bmod: Register<u32, u32>,
    _hole: Register<[u32; 0x5f], ()>,
    data: Register<u32, u32>,
}

impl Device {
    pub fn init(&self, writer: &mut impl fmt::Write, plic_tc: &PlicThresholdClaim) -> u32 {
        self.ctrl.write(0b111_u32);
        while self.ctrl.read() & 0b111 != 0 {
            core::hint::spin_loop();
        }

        self.init_clk(writer);

        self.raw_int_status.write(u32::MAX);
        self.int_mask.write(0u32);
        self.tmout.write(u32::MAX);
        self.raw_int_status.write(u32::MAX);
        self.int_mask
            .write(1u32 << 2 | 1 << 3 | 1 << 4 | 1 << 5 | 1 << 6 | 1 << 7);

        self.ctrl.write(self.ctrl.read() | (1 << 4));

        //  | 1 << 15 | 1 << 29
        self.send_cmd(writer, 0, 0, 0);
        self.send_cmd(writer, 8, 0x000001aa, 1 << 6 | 1 << 8 | 1 << 15);
        self.receive_response(writer, plic_tc);

        let r = loop {
            self.send_cmd(writer, 55, 0, 1 << 6); // CMD55: Prefix for ACMD
            self.receive_response(writer, plic_tc);
            self.send_cmd(writer, 41, 0xC0FF8000, 1 << 6); // ACMD41: Card Initialization
            let r = self.receive_response(writer, plic_tc);
            if r & (1 << 31) != 0 {
                break r;
            }
            for _ in 0..0x10000 {
                hint::spin_loop();
            }
        };
        if r & (1 << 24) != 0 {
            self.send_cmd(writer, 11, 0, 1 << 6);
            self.receive_response(writer, plic_tc);
        }

        self.send_cmd(writer, 2, 0, 1 << 7 | 1 << 6);
        let _cid = self.receive_response_long(writer, plic_tc);

        self.send_cmd(writer, 3, 0, 1 << 6);
        let r = self.receive_response(writer, plic_tc);
        let rca = r & 0xffff0000;

        self.send_cmd(writer, 7, rca, 1 << 6);
        self.receive_response(writer, plic_tc);

        loop {
            self.send_cmd(writer, 13, rca, 1 << 6);
            let r = self.receive_response(writer, plic_tc);
            if r == 0x900 {
                break;
            }
            for _ in 0..0x10000 {
                hint::spin_loop();
            }
        }

        rca
    }

    fn init_clk(&self, _writer: &mut impl fmt::Write) {
        const SDMMC_INPUT_FREQ_HZ: u32 = 50000000; // 50 MHz typical
        const SD_INIT_FREQ_HZ: u32 = 400000; // 400 kHz during init

        // Disable clock output while changing dividers
        self.clkena.write(0u32);
        self.clksrc.write(0u32);

        // Compute divider: SDCLK = INPUT_CLK / (CLKDIV + 1)
        let clkdiv = (SDMMC_INPUT_FREQ_HZ / SD_INIT_FREQ_HZ).saturating_sub(1);
        self.clkdiv.write(clkdiv); // e.g., 124 for ~400kHz at 50 MHz input

        // Trigger clock update (CMD with bit 21)
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21)); // START_CMD | UPDATE_CLOCK

        // Wait for CMD_DONE
        while self.cmd.read() & (1u32 << 31) != 0 {
            hint::spin_loop();
        }

        // Enable clock to the card
        self.clkena.write(1u32 | (1 << 16));

        // Trigger another update
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21));

        while self.cmd.read() & (1u32 << 31) != 0 {
            hint::spin_loop();
        }
    }

    fn send_cmd(&self, writer: &mut impl fmt::Write, cmd_index: u32, cmd_arg: u32, flags: u32) {
        self.cmd_arg.write(cmd_arg);
        unsafe { arch::asm!("fence ow, ow") };
        self.cmd.write(1 << 31 | cmd_index | flags);
        writeln!(writer, "sent: CMD{cmd_index} {flags:032b} {cmd_arg:08x}\r").unwrap_or_default();
    }

    fn wait_interrupt(&self, plic_tc: &PlicThresholdClaim) -> u32 {
        tau::Ubi::wait();
        while let Some(int) = plic_tc.next() {
            let desired = int == InterruptNumber::new(75);
            plic_tc.complete(int);
            if desired {
                break;
            }
        }
        let int = self.mask_int_status.read();
        self.raw_int_status.write(int);
        int
    }

    fn receive_response(&self, writer: &mut impl fmt::Write, plic_tc: &PlicThresholdClaim) -> u32 {
        let int = self.wait_interrupt(plic_tc);
        let r = self.resp0.read();
        writeln!(writer, "int: {int:016b} response: {r:08x}\r").unwrap_or_default();
        r
    }

    fn receive_response_long(
        &self,
        writer: &mut impl fmt::Write,
        plic_tc: &PlicThresholdClaim,
    ) -> [u32; 4] {
        let int = self.wait_interrupt(plic_tc);
        let r = [
            self.resp0.read(),
            self.resp1.read(),
            self.resp2.read(),
            self.resp3.read(),
        ];
        writeln!(
            writer,
            "int: {int:016b} response: {:08x}, {:08x}, {:08x}, {:08x}\r",
            r[0], r[1], r[2], r[3]
        )
        .unwrap_or_default();
        r
    }

    pub fn test(&self, writer: &mut impl fmt::Write, plic_tc: &PlicThresholdClaim, rca: u32) {
        self.bytcnt.write(0x200_u32);
        self.blksiz.write(0x200_u32);
        self.fifo_threshold.write(2u32 << 28 | (15 << 16) | 16);
        unsafe { arch::asm!("fence ow, ow") };
        self.send_cmd(writer, 17, 20, 1 << 6 | 1 << 9 | 1 << 12 | 1 << 13); //  | 1 << 10
        self.receive_response(writer, plic_tc);

        for _ in 0..10 {
            writeln!(writer, "status: {:032b}\r", self.status.read()).unwrap_or_default();
            for i in 0..0x20 {
                let d0 = self.data.read().to_be();
                let d1 = self.data.read().to_be();
                let d2 = self.data.read().to_be();
                let d3 = self.data.read().to_be();
                writeln!(
                    writer,
                    "{:08x}: {d0:08x} {d1:08x} {d2:08x} {d3:08x}\r",
                    i * 16
                )
                .unwrap_or_default();
                while self.status.read() & (1u32 << 2) != 0 {
                    for _ in 0..0x1000 {
                        hint::spin_loop();
                    }
                }
            }
            writeln!(writer, "status: {:032b}\r", self.status.read()).unwrap_or_default();

            self.send_cmd(writer, 13, rca | 0x8000, 1 << 6 | 1 << 13);
            self.receive_response(writer, plic_tc);
        }
    }
}
