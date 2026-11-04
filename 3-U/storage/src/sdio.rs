// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{time::Duration, fmt};

use super::sys;

pub struct State {
    rca: u32,
    freq: u64,
    dma_desc: &'static [tau::Register<u32, u32>; 0x200],
    dma_phys: u32,
}

#[derive(Debug)]
pub enum DriverError {
    Control(tau::Timeout),
    Clock(tau::Timeout),
    CmdFailed(u32),
    Dma,
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(tau::Timeout) => write!(f, "timeout"),
            Self::Clock(tau::Timeout) => write!(f, "timeout"),
            Self::CmdFailed(code) => write!(f, "failed CMD{code}"),
            Self::Dma => write!(f, "failed allocator dma buffer"),
        }
    }
}

impl State {
    pub fn new(freq: u64) -> Option<Self> {
        let dma_base = tau::Ubi::alloc_physical(0, tau::MemoryZone::Dma).ok()?;
        let dma_phys = u32::try_from(dma_base).ok()?;
        let dma_desc = tau::Area::new(dma_base, 0x1000).r();

        Some(State {
            rca: 0,
            freq,
            dma_desc,
            dma_phys,
        })
    }

    pub fn init(&mut self, reg: &Registers, fifo_depth: u32) -> Result<(), DriverError> {
        sys::write(format_args!("sdio: off..."), self.freq);
        reg.pwren.write(0u32);
        sys::sleep(1, Self::RESET_DELAY, self.freq);
        sys::write(format_args!("sdio: on..."), self.freq);
        reg.pwren.write(1u32);
        sys::sleep(1, Self::RESET_DELAY, self.freq);
        sys::write(format_args!("sdio: init"), self.freq);
        reg.init(fifo_depth)?;
        self.cmd::<0>(reg, CmdFl::empty(), 0)?;
        self.cmd::<8>(reg, CmdFl::RESP_EXP | CmdFl::RESP_CRC, 0x1aa)?;

        let mut attempts = INIT_TO;
        loop {
            self.cmd::<55>(reg, CmdFl::RESP_EXP, 0)?;
            self.cmd::<41>(reg, CmdFl::RESP_EXP, 0xc0ff8000)?;
            let r = reg.resp0.read();
            sys::write(format_args!("sdio: CMD41 resp={r:08x}"), self.freq);
            if r & (1 << 31) != 0 {
                break;
            } else {
                if attempts == 0 {
                    return Err(DriverError::Control(tau::Timeout));
                } else {
                    attempts -= 1;
                    sys::sleep(1, Self::ACMD_41_DELAY, self.freq);
                }
            }
        }
        self.cmd::<2>(reg, CmdFl::RESP_EXP | CmdFl::RESP_LONG_EXP, 0)?;
        self.cmd::<3>(reg, CmdFl::RESP_EXP, 0)?;
        self.rca = reg.resp0.read() & 0xffff0000;
        reg.init_clk(25_000)?;
        self.cmd::<7>(reg, CmdFl::RESP_EXP, self.rca)?;
        loop {
            self.cmd::<13>(reg, CmdFl::RESP_EXP, self.rca)?;
            let r = reg.resp0.read();
            if (r >> 9) & 0b1111 == 4 {
                break;
            }
        }

        Ok(())
    }

    pub fn read(&mut self, reg: &Registers, page: u32, phys: u32) -> Result<(), DriverError> {
        let flags = CmdFl::RESP_EXP | CmdFl::DATA_EXP | CmdFl::A_STOP;
        self.setup_dma(reg, phys);
        self.cmd::<MMC_READ_BLOCKS>(reg, flags, page * 8)
    }

    pub fn write(&mut self, reg: &Registers, page: u32, phys: u32) -> Result<(), DriverError> {
        let flags = CmdFl::RESP_EXP | CmdFl::DATA_EXP | CmdFl::A_STOP | CmdFl::DATA_WRITE;
        self.setup_dma(reg, phys);
        self.cmd::<MMC_WRITE_BLOCKS>(reg, flags, page * 8)
    }

    const RESET_DELAY: Duration = Duration::from_millis(200);
    const ACMD_41_DELAY: Duration = Duration::from_millis(5);

    fn setup_dma(&mut self, reg: &Registers, phys: u32) {
        reg.bytcnt.write(0x1000_u32);
        // control bits
        // buffer size
        // buffer address (physical)
        // next descriptor address (physical)
        self.dma_desc[0].write(0b1000_0000_0000_0000_0000_0000_0001_1010_u32);
        self.dma_desc[1].write(0x800_u32);
        self.dma_desc[2].write(phys);
        self.dma_desc[3].write(self.dma_phys + 0x10);
        self.dma_desc[4].write(0b1000_0000_0000_0000_0000_0000_0001_0100_u32);
        self.dma_desc[5].write(0x800_u32);
        self.dma_desc[6].write(phys + 0x800);
        self.dma_desc[7].write(0_u32);
        tau::asm::fence();
        reg.desc_base.write(self.dma_phys);
        reg.bus_mod.write(reg.bus_mod.read() | (1 << 1) | (1 << 7));
    }

    fn cmd<const CMD: u32>(
        &self,
        reg: &Registers,
        flags: CmdFl,
        arg: u32,
    ) -> Result<(), DriverError> {
        reg.cmdarg.write(arg);
        tau::asm::fence();
        reg.cmd.write(1 << 31 | 1 << 29 | CMD | flags.bits());
        sys::write(
            format_args!("sdio: CMD{CMD} arg={arg:08x} flags={flags:?}"),
            self.freq,
        );

        loop {
            sys::wait_interrupt();

            let int_bits = reg.mintsts.read();
            let int = Interrupt::from_bits_truncate(int_bits);
            if int_bits != 0 {
                reg.rintsts.write(int_bits);
            }
            let dma_status = reg.i_dmac_status.read();
            if dma_status != 0 {
                reg.i_dmac_status.write(dma_status & 0x3ff);
            }
            if int.contains(Interrupt::ERR) {
                break Err(DriverError::CmdFailed(CMD));
            } else if int.contains(Interrupt::DONE) {
                break Ok(());
            }
        }
    }
}

#[repr(C, align(0x1000))]
pub struct Registers {
    ctrl: tau::Register<Ctrl, Ctrl>,
    pwren: tau::Register<u32, u32>,
    clkdiv: tau::Register<u32, u32>,
    clksrc: tau::Register<u32, u32>,
    clkena: tau::Register<u32, u32>,
    timeout: tau::Register<u32, u32>,
    ctype: tau::Register<u32, u32>,
    blksiz: tau::Register<u32, u32>,
    bytcnt: tau::Register<u32, u32>,
    intmask: tau::Register<(), Interrupt>,
    cmdarg: tau::Register<u32, u32>,
    cmd: tau::Register<u32, u32>,
    resp0: tau::Register<u32, u32>,
    resp1: tau::Register<u32, u32>,
    resp2: tau::Register<u32, u32>,
    resp3: tau::Register<u32, u32>,
    mintsts: tau::Register<u32, ()>,
    rintsts: tau::Register<(), u32>,
    status: tau::Register<u32, u32>,
    fifo_threshold: tau::Register<u32, u32>,
    _h0: [u32; 0xc],
    bus_mod: tau::Register<u32, u32>,
    poll_demand: tau::Register<u32, u32>,
    desc_base: tau::Register<u32, u32>,
    i_dmac_status: tau::Register<u32, u32>,
    i_dmac_int_enable: tau::Register<u32, u32>,
    _hole: tau::Register<[u32; 0x5b], ()>,
    data: tau::Register<u32, u32>,
}

const CTRL_RESET_TO: u32 = 1000;
const CLK_SET_TO: u32 = 10000;
const INIT_TO: u32 = 30;

const MMC_READ_BLOCKS: u32 = 18;
const MMC_WRITE_BLOCKS: u32 = 25;

bitflags::bitflags! {
    #[derive(Debug)]
    struct CmdFl: u32 {
        const RESP_EXP = 1 << 6;
        const RESP_LONG_EXP = 1 << 7;
        const RESP_CRC = 1 << 8;
        const DATA_EXP = 1 << 9;
        const DATA_WRITE = 1 << 10;
        const A_STOP = 1 << 12;
        const PREV_DATA = 1 << 13;
        const STOP_ABORT = 1 << 14;
        const SEND_INIT = 1 << 15;
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug)]
    struct Interrupt: u32 {
        /// Card detected
        const CD = 1 << 0;
        /// Response error
        const ERR = 1 << 1;
        /// Command done
        const DONE = 1 << 2;
        /// Data transfer over
        const DTO = 1 << 3;
        /// Transmit FIFO data request
        const TXDR = 1 << 4;
        /// Receive FIFO data request
        const RXDR = 1 << 5;
        /// Auto command done
        const ACD = 1 << 14;
    }
}

bitflags::bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Ctrl: u32 {
        const RESET_CONTROLLER = 1 << 0;
        const FIFO_RESET = 1 << 1;
        const DMA_RESET = 1 << 2;
        const ENABLE_INTERRUPTS = 1 << 4;
        const ENABLE_DMA = 1 << 5;
        const USE_INTERNAL_DMAC = 1 << 25;
    }
}

impl Registers {
    fn init(&self, fifo_depth: u32) -> Result<(), DriverError> {
        let ctrl_reset = Ctrl::RESET_CONTROLLER | Ctrl::FIFO_RESET | Ctrl::DMA_RESET;
        self.ctrl.write(ctrl_reset);
        tau::spin(CTRL_RESET_TO, || !self.ctrl.read().intersects(ctrl_reset))
            .map_err(DriverError::Control)?;

        self.rintsts.write(u32::MAX);
        self.i_dmac_status.write(u32::MAX);

        self.intmask
            .write(Interrupt::ERR | Interrupt::DONE | Interrupt::ACD | Interrupt::DTO);
        self.ctrl.write(
            self.ctrl.read() | Ctrl::ENABLE_INTERRUPTS | Ctrl::ENABLE_DMA | Ctrl::USE_INTERNAL_DMAC,
        );

        self.init_clk(400)?; // 400 kHz during init

        self.blksiz.write(0x200_u32);
        self.bus_mod
            .write((self.bus_mod.read() & !((1 << 1) | (1 << 7))) | (1 << 0));

        self.i_dmac_int_enable.write(0b1100110111_u32);

        self.ctype.write(0u32);
        self.timeout.write(0x_ffff_ff40_u32);
        let mid = (fifo_depth & 0xfff) / 2;
        self.fifo_threshold
            .write((2u32 << 28) | ((mid - 1) << 16) | mid);

        Ok(())
    }

    fn init_clk(&self, freq: u32) -> Result<(), DriverError> {
        // wait while the card is busy
        tau::spin(1000, || self.status.read() & (1 << 9) == 0).map_err(DriverError::Clock)?;

        // Disable clock output while changing dividers
        self.clkena.write(0u32);
        self.clksrc.write(0u32);

        const SDMMC_INPUT_FREQ_KHZ: u32 = 50_000;
        let clkdiv = SDMMC_INPUT_FREQ_KHZ.div_ceil(2 * freq) as u8;
        self.clkdiv.write(clkdiv);

        // Trigger clock update (CMD with bit 21)
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21)); // START_CMD | UPDATE_CLOCK
        tau::spin(CLK_SET_TO, || self.cmd.read() & (1u32 << 31) == 0)
            .map_err(DriverError::Clock)?;

        // Enable clock to the card
        self.clkena.write(1u32 | (1 << 16)); //  low-power mode

        // Trigger another update
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21));
        tau::spin(CLK_SET_TO, || self.cmd.read() & (1u32 << 31) == 0)
            .map_err(DriverError::Clock)?;

        Ok(())
    }
}
