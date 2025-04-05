use core::time::Duration;

use alloc::boxed::Box;

use thiserror_no_std::Error;

use super::{
    register::Register,
    scheduler::{self, Timeout, DriverState, Shared},
};

pub struct State {
    reg: Box<Reg, tau::Area>,
    inner: StateInner,
    fifo_depth: u16,
    rca: u32,
    dma_desc: Box<[u32], tau::Area>,
    dma_phys: u32,
    error: Option<DriverError>,
}

enum StateInner {
    Off,
    On,
    Init(StateInit),
    Ready { task: Option<Task> },
}

enum StateInit {
    Setup,
    Cmd {
        code: u32,
        acmd41_attempt: u32,
        acmd41_sleep: bool,
    },
}

pub enum Task {
    Read { page: u32, phys: u32 },
    Write { page: u32, phys: u32 },
}

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("control reset {0}")]
    Control(Timeout),
    #[error("clock setup {0}")]
    Clock(Timeout),
    #[error("cmd failed {0}")]
    CmdFailed(u32),
}

impl State {
    const RESET_DELAY: Duration = Duration::from_millis(200);
    const ACMD_41_DELAY: Duration = Duration::from_millis(5);

    fn cmd<const CMD: u32>(&mut self, shared: &mut Shared, flags: CmdFl, arg: u32) {
        let reg = &self.reg;

        reg.cmdarg.write(arg);
        tau::asm::fence();
        reg.cmd.write(1 << 31 | 1 << 29 | CMD | flags.bits());
        shared.write(format_args!("sdio: CMD{CMD} arg={arg:08x} flags={flags:?}"));
        if let StateInner::Init(StateInit::Cmd { code, .. }) = &mut self.inner {
            *code = CMD;
        } else if !matches!(&self.inner, StateInner::Ready { .. }) {
            self.inner = StateInner::Init(StateInit::Cmd {
                code: CMD,
                acmd41_attempt: INIT_TO,
                acmd41_sleep: false,
            });
        }
    }

    fn setup_dma(&mut self, phys: u32) {
        let reg = &self.reg;

        reg.bytcnt.write(0x1000_u32);
        // control bits
        // buffer size
        // buffer address (physical)
        // next descriptor address (physical)
        self.dma_desc[..8].clone_from_slice(&[
            0b1000_0000_0000_0000_0000_0000_0001_1010,
            0x800,
            phys,
            self.dma_phys + 0x10,
            0b1000_0000_0000_0000_0000_0000_0001_0100,
            0x800,
            phys + 0x800,
            0,
        ]);
        tau::asm::fence();
        reg.desc_base.write(self.dma_phys);
        reg.bus_mod.write(reg.bus_mod.read() | (1 << 1) | (1 << 7));
    }
}

impl State {
    pub fn new(config: tau::DtbProps<'_>) -> Option<Self> {
        let Some(&[fifo_depth]) = config.find_int(|name| name == "fifo-depth") else {
            return None;
        };
        let fifo_depth = fifo_depth.to_be() as u16;
        let Some([addr_hi, addr_lo, size_hi, size_lo]) = config.find_int(|name| name == "reg")
        else {
            return None;
        };
        let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);
        let size = ((size_hi.to_be() as usize) << 32) + (size_lo.to_be() as usize);
        let a = tau::Area::new(addr, size);
        let reg = unsafe { Box::<Reg, _>::new_uninit_in(a).assume_init() };

        // TODO: allocator for DMA
        let a = tau::Area::new(0x7000_0000, 0x1000);
        let dma_desc = unsafe { Box::<[u32], _>::new_zeroed_slice_in(0x200, a).assume_init() };

        Some(State {
            reg,
            inner: StateInner::Off,
            fifo_depth,
            rca: 0,
            dma_desc,
            dma_phys: 0x7000_0000_u32,
            error: None,
        })
    }
}

impl DriverState for State {
    fn handle(&mut self, shared: &mut Shared, event: tau::Event<u32>) {
        if self.error.is_none() {
            if let Err(err) = self.handle_inner(shared, event) {
                self.error = Some(err);
            }
        }
    }
}

impl State {
    fn handle_inner(
        &mut self,
        shared: &mut Shared,
        _event: tau::Event<u32>,
    ) -> Result<(), DriverError> {
        let reg = &self.reg;
        let int_bits = reg.mintsts.read();
        let int = Interrupt::from_bits_truncate(int_bits);
        let dma_status = reg.i_dmac_status.read();
        if int_bits != 0 {
            reg.rintsts.write(int_bits);
        }
        if dma_status != 0 {
            reg.i_dmac_status.write(dma_status & 0x3ff);
        }

        if int.contains(Interrupt::DTO) {
            if let StateInner::Ready { task } = &mut self.inner {
                shared.sdio_done = task.take();
            }
        }

        match &mut self.inner {
            StateInner::Off => {
                shared.write(format_args!("sdio: off..."));
                reg.pwren.write(0u32);
                self.inner = StateInner::On;
                shared.sleep(1, Self::RESET_DELAY);
                return Ok(());
            }
            StateInner::On => {
                shared.write(format_args!("sdio: on..."));
                reg.pwren.write(1u32);
                self.inner = StateInner::Init(StateInit::Setup);
                shared.sleep(1, Self::RESET_DELAY);
                return Ok(());
            }
            StateInner::Init(StateInit::Setup) => {
                shared.write(format_args!("sdio: init"));
                reg.init(self.fifo_depth)?;
                self.cmd::<0>(shared, CmdFl::empty(), 0);
            }
            StateInner::Init(StateInit::Cmd {
                code,
                acmd41_attempt,
                acmd41_sleep,
            }) => {
                if *acmd41_sleep {
                    *acmd41_sleep = false;
                    self.cmd::<55>(shared, CmdFl::RESP_EXP, 0);
                    return Ok(());
                }

                if int.contains(Interrupt::ERR) {
                    return Err(DriverError::CmdFailed(*code));
                } else if !int.contains(Interrupt::DONE) {
                    return Ok(());
                }

                match *code {
                    0 => self.cmd::<8>(shared, CmdFl::RESP_EXP | CmdFl::RESP_CRC, 0x1aa),
                    8 => self.cmd::<55>(shared, CmdFl::RESP_EXP, 0),
                    55 => self.cmd::<41>(shared, CmdFl::RESP_EXP, 0xc0ff8000),
                    41 => {
                        let r = reg.resp0.read();
                        shared.write(format_args!("sdio: CMD41 resp={r:08x}"));
                        if r & (1 << 31) != 0 {
                            self.cmd::<2>(shared, CmdFl::RESP_EXP | CmdFl::RESP_LONG_EXP, 0);
                        } else {
                            *acmd41_attempt -= 1;
                            return if *acmd41_attempt == 0 {
                                Err(DriverError::Control(Timeout))
                            } else {
                                *acmd41_sleep = true;
                                shared.sleep(1, Self::ACMD_41_DELAY);
                                return Ok(());
                            };
                        }
                    }
                    2 => self.cmd::<3>(shared, CmdFl::RESP_EXP, 0),
                    3 => {
                        self.rca = reg.resp0.read() & 0xffff0000;
                        self.cmd::<7>(shared, CmdFl::RESP_EXP, self.rca);
                    }
                    7 => self.cmd::<13>(shared, CmdFl::RESP_EXP, self.rca),
                    13 => {
                        let r = reg.resp0.read();
                        if (r >> 9) & 0b1111 == 4 {
                            self.inner = StateInner::Ready { task: None };
                            // TODO: set high freq

                            return self.handle_inner(shared, _event);
                        } else {
                            self.cmd::<13>(shared, CmdFl::RESP_EXP, self.rca);
                        }
                    }
                    _ => {}
                }
            }
            StateInner::Ready { task } => {
                if task.is_none() {
                    let flags =
                        CmdFl::RESP_EXP | CmdFl::DATA_EXP | CmdFl::PREV_DATA | CmdFl::A_STOP;
                    *task = shared.sdio_task.take();
                    match *task {
                        None => {}
                        Some(Task::Read { page, phys }) => {
                            self.setup_dma(phys);
                            self.cmd::<MMC_READ_BLOCKS>(shared, flags, page * 8);
                        }
                        Some(Task::Write { page, phys }) => {
                            self.setup_dma(phys);
                            let flags = flags | CmdFl::DATA_WRITE;
                            self.cmd::<MMC_WRITE_BLOCKS>(shared, flags, page * 8);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[repr(C, align(0x1000))]
struct Reg {
    ctrl: Register<Ctrl, Ctrl>,
    pwren: Register<u32, u32>,
    clkdiv: Register<u32, u32>,
    clksrc: Register<u32, u32>,
    clkena: Register<u32, u32>,
    timeout: Register<u32, u32>,
    ctype: Register<u32, u32>,
    blksiz: Register<u32, u32>,
    bytcnt: Register<u32, u32>,
    intmask: Register<(), Interrupt>,
    cmdarg: Register<u32, u32>,
    cmd: Register<u32, u32>,
    resp0: Register<u32, u32>,
    resp1: Register<u32, u32>,
    resp2: Register<u32, u32>,
    resp3: Register<u32, u32>,
    mintsts: Register<u32, ()>,
    rintsts: Register<(), u32>,
    status: Register<u32, u32>,
    fifo_threshold: Register<u32, u32>,
    _h0: [u32; 0xc],
    bus_mod: Register<u32, u32>,
    poll_demand: Register<u32, u32>,
    desc_base: Register<u32, u32>,
    i_dmac_status: Register<u32, u32>,
    i_dmac_int_enable: Register<u32, u32>,
    _hole: Register<[u32; 0x5b], ()>,
    data: Register<u32, u32>,
}

const CTRL_RESET_TO: u32 = 1000;
const CLK_SET_TO: u32 = 20;
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

impl Reg {
    fn init(&self, fifo_depth: u16) -> Result<(), DriverError> {
        let ctrl_reset = Ctrl::RESET_CONTROLLER | Ctrl::FIFO_RESET | Ctrl::DMA_RESET;
        self.ctrl.write(ctrl_reset);
        scheduler::spin(CTRL_RESET_TO, || !self.ctrl.read().intersects(ctrl_reset))
            .map_err(DriverError::Control)?;

        self.rintsts.write(u32::MAX);
        self.i_dmac_status.write(u32::MAX);

        self.intmask
            .write(Interrupt::ERR | Interrupt::DONE | Interrupt::ACD | Interrupt::DTO);
        self.ctrl.write(
            self.ctrl.read() | Ctrl::ENABLE_INTERRUPTS | Ctrl::ENABLE_DMA | Ctrl::USE_INTERNAL_DMAC,
        );
        self.init_clk()?;

        self.blksiz.write(0x200_u32);
        self.bus_mod
            .write((self.bus_mod.read() & !((1 << 1) | (1 << 7))) | (1 << 0));

        self.i_dmac_int_enable.write(0b1100110111_u32);

        self.ctype.write(0u32);
        self.timeout.write(u32::MAX);
        let mid = ((fifo_depth & 0xfff) / 2) as u32;
        self.fifo_threshold
            .write((2u32 << 28) | ((mid - 1) << 16) | mid);

        Ok(())
    }

    fn init_clk(&self) -> Result<(), DriverError> {
        // Disable clock output while changing dividers
        self.clkena.write(0u32);
        self.clksrc.write(0u32);

        const SDMMC_INPUT_FREQ_HZ: u32 = 50000000; // 50 MHz typical
        const SD_INIT_FREQ_HZ: u32 = 400000; // 400 kHz during init
        // Compute divider: SDCLK = INPUT_CLK / (CLKDIV + 1)
        let clkdiv = (SDMMC_INPUT_FREQ_HZ / SD_INIT_FREQ_HZ).saturating_sub(1);
        // e.g., 124 for ~400kHz at 50 MHz input
        self.clkdiv.write(clkdiv);

        // Trigger clock update (CMD with bit 21)
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21)); // START_CMD | UPDATE_CLOCK
        scheduler::spin(CLK_SET_TO, || self.cmd.read() & (1u32 << 31) == 0)
            .map_err(DriverError::Clock)?;

        // Enable clock to the card
        self.clkena.write(1u32 | (1 << 16)); //  low-power mode

        // Trigger another update
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21));
        scheduler::spin(CLK_SET_TO, || self.cmd.read() & (1u32 << 31) == 0)
            .map_err(DriverError::Clock)?;

        Ok(())
    }
}
