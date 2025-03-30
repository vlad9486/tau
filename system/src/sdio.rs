use core::{mem, num::NonZeroUsize};

use thiserror_no_std::Error;

use super::{
    asm,
    register::Register,
    plic::InterruptId,
    driver::{self, Timeout, Runtime},
};

pub async fn run(rt: &Runtime, config: tau::DtbProps<'_>, v_addr: usize) {
    match inner(rt, config, v_addr).await {
        Ok(()) => rt.info(format_args!("sdio: done")),
        Err(err) => rt.error(format_args!("sdio: {err}")),
    }
}

#[derive(Default)]
pub enum Task {
    #[default]
    Idle,
    #[allow(dead_code)]
    Read { page: u32, phys: u32, cnt: u16 },
    #[allow(dead_code)]
    Write { page: u32, phys: u32, cnt: u16 },
}

#[derive(Debug, Error)]
enum DriverError {
    #[error("dts missing register address")]
    DtsMissingReg,
    #[error("control reset {0}")]
    Control(Timeout),
    #[error("clock setup {0}")]
    Clock(Timeout),
    #[error("init timeout")]
    InitTimeout,
}

async fn inner(rt: &Runtime, config: tau::DtbProps<'_>, v_addr: usize) -> Result<(), DriverError> {
    let Some([addr_hi, addr_lo, size_hi, size_lo]) = config.find_int(|name| name == "reg") else {
        return Err(DriverError::DtsMissingReg);
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);
    let size = ((size_hi.to_be() as usize) << 32) + (size_lo.to_be() as usize);
    tau::Ubi::map(NonZeroUsize::new(addr), v_addr, size.div_ceil(0x1000)).unwrap_or_default();
    let reg = unsafe { &*(v_addr as *const Reg) };
    let _rca = reg.init(rt).await?;
    // reg.test(rt, v_addr + size).await;

    // TODO: allocator for DMA
    let dma_virt = v_addr + size;
    tau::Ubi::map(NonZeroUsize::new(0x7000_0000), dma_virt, 1).unwrap_or_default();
    let dma_phys = 0x7000_0000_u32;

    // {
    //     let page_phys = 0x7000_1000;
    //     let page_virt = dma_virt + 0x1000;
    //     tau::Ubi::map(NonZeroUsize::new(0x7000_1000), page_virt, 1).unwrap_or_default();
    //     let page = page_virt as *mut [u8; 0x1000];

    //     unsafe { page.write_volatile([0x10; 0x1000]) };
    //     reg.data(rt, dma_virt, dma_phys, 0x700, page_phys, true)
    //         .await;

    //     let dma_data = unsafe { page.read_volatile() };
    //     for chunk in dma_data.chunks(0x10) {
    //         let &[a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = chunk else {
    //             break;
    //         };
    //         rt.info(format_args!("{a:02x}{b:02x}{c:02x}{d:02x}{e:02x}{f:02x}{g:02x}{h:02x}{i:02x}{j:02x}{k:02x}{l:02x}{m:02x}{n:02x}{o:02x}{p:02x}"));
    //     }
    // }
    loop {
        // TODO: queue
        match rt.wait().await {
            tau::Event::Interrupt(id) => drop(reg.complete_interrupt(rt, id)),
            tau::Event::Timeout => {}
            tau::Event::Signal { .. } => match mem::take(&mut rt.shared_mut().sdio_task) {
                Task::Idle => {}
                Task::Read { page, phys, cnt } => {
                    rt.info(format_args!("read: page={page}, phys={phys}, cnt={cnt}"));
                    for i in 0..cnt {
                        let phys = phys + (i as u32) * 0x1000;
                        let page = page + (i as u32);
                        reg.data(rt, dma_virt, dma_phys, page, phys, false).await;
                    }
                }
                Task::Write { page, phys, cnt } => {
                    rt.info(format_args!("write: page={page}, phys={phys}, cnt={cnt}"));
                    for i in 0..cnt {
                        let phys = phys + (i as u32) * 0x1000;
                        let page = page + (i as u32);
                        reg.data(rt, dma_virt, dma_phys, page, phys, true).await;
                    }
                }
            },
        }
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
    intmask: Register<Interrupt, Interrupt>,
    cmdarg: Register<u32, u32>,
    cmd: Register<u32, u32>,
    resp0: Register<u32, u32>,
    resp1: Register<u32, u32>,
    resp2: Register<u32, u32>,
    resp3: Register<u32, u32>,
    mintsts: Register<u32, u32>,
    rintsts: Register<u32, u32>,
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
    async fn init(&self, rt: &Runtime) -> Result<u32, DriverError> {
        rt.info(format_args!("off"));
        self.pwren.write(0u32);
        for _ in 0..13 {
            self.sleep(rt).await;
        }
        rt.info(format_args!("on"));
        self.pwren.write(1u32);
        for _ in 0..13 {
            self.sleep(rt).await;
        }
        rt.info(format_args!("ready"));
        let ctrl_reset = Ctrl::RESET_CONTROLLER | Ctrl::FIFO_RESET | Ctrl::DMA_RESET;
        self.ctrl.write(ctrl_reset);
        driver::spin(CTRL_RESET_TO, || !self.ctrl.read().intersects(ctrl_reset))
            .map_err(DriverError::Control)?;

        self.rintsts.write(u32::MAX);
        self.intmask.write(
            Interrupt::ERR | Interrupt::DONE | Interrupt::ACD | Interrupt::DTO | Interrupt::CD,
        );
        self.ctrl.write(
            self.ctrl.read() | Ctrl::ENABLE_INTERRUPTS | Ctrl::ENABLE_DMA | Ctrl::USE_INTERNAL_DMAC,
        );

        self.blksiz.write(0x200_u32);
        self.bus_mod
            .write((self.bus_mod.read() & !((1 << 1) | (1 << 7))) | (1 << 0));

        self.i_dmac_status
            .write(self.i_dmac_status.read() | 0b1100110111);
        self.i_dmac_int_enable.write(0b1100110111_u32);

        self.init_clk()?;
        self.ctype.write(0u32);

        self.send_cmd::<0>(rt, 0, CmdFl::empty()).await;
        self.timeout.write(u32::MAX);
        self.fifo_threshold
            .write(self.fifo_threshold.read() | ((2u32 << 28) | (15 << 16) | 16));

        self.send_cmd::<8>(rt, 0x1aa, CmdFl::RESP_EXP | CmdFl::RESP_CRC)
            .await;

        let mut timeout = INIT_TO;
        loop {
            // CMD55: Prefix for ACMD
            self.send_cmd::<55>(rt, 0, CmdFl::RESP_EXP).await;
            // ACMD41: Card Initialization
            self.send_cmd::<41>(rt, 0xc0ff8000, CmdFl::RESP_EXP).await;
            let r = self.resp0.read();
            if r & (1 << 31) != 0 {
                if r & (1 << 24) != 0 {
                    self.send_cmd::<11>(rt, 0, CmdFl::RESP_EXP).await;
                }

                break;
            }
            self.sleep(rt).await;
            timeout -= 1;
            if timeout == 0 {
                return Err(DriverError::InitTimeout);
            }
        }

        self.send_cmd::<2>(rt, 0, CmdFl::RESP_EXP | CmdFl::RESP_LONG_EXP)
            .await;
        self.send_cmd::<3>(rt, 0, CmdFl::RESP_EXP).await;
        let rca = self.resp0.read() & 0xffff0000;
        self.send_cmd::<7>(rt, rca, CmdFl::RESP_EXP).await;

        loop {
            self.send_cmd::<13>(rt, rca, CmdFl::RESP_EXP).await;
            let r = self.resp0.read();
            if (r >> 9) & 0b1111 == 4 {
                break;
            }
        }

        Ok(rca)
    }

    fn init_clk(&self) -> Result<(), DriverError> {
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
        driver::spin(CLK_SET_TO, || self.cmd.read() & (1u32 << 31) == 0)
            .map_err(DriverError::Clock)?;

        // Enable clock to the card
        self.clkena.write(1u32 | (1 << 16));

        // Trigger another update
        self.cmd.write((1u32 << 31) | (1 << 13) | (1 << 21));
        driver::spin(CLK_SET_TO, || self.cmd.read() & (1u32 << 31) == 0)
            .map_err(DriverError::Clock)?;

        Ok(())
    }

    async fn data(
        &self,
        rt: &Runtime,
        dma_virt: usize,
        dma_phys: u32,
        page_no: u32,
        page_phys: u32,
        write: bool,
    ) {
        self.bytcnt.write(0x1000_u32);
        unsafe {
            // control bits
            // buffer size
            // buffer address (physical)
            // next descriptor address (physical)
            (dma_virt as *mut [u32; 8]).write_volatile([
                0b1000_0000_0000_0000_0000_0000_0001_1010,
                0x800,
                page_phys,
                dma_phys + 0x10,
                0b1000_0000_0000_0000_0000_0000_0001_0100,
                0x800,
                page_phys + 0x800,
                0,
            ]);
            asm::fence();
        }
        self.desc_base.write(dma_phys);

        self.bus_mod
            .write(self.bus_mod.read() | (1 << 1) | (1 << 7));
        let flags = CmdFl::RESP_EXP | CmdFl::DATA_EXP | CmdFl::PREV_DATA | CmdFl::A_STOP;
        if write {
            self.send_cmd::<MMC_WRITE_BLOCKS>(rt, page_no * 8, flags | CmdFl::DATA_WRITE)
                .await;
        } else {
            self.send_cmd::<MMC_READ_BLOCKS>(rt, page_no * 8, flags)
                .await;
        }
        while self.wait(rt).await & Interrupt::DTO.bits() == 0 {}
    }

    async fn send_cmd<const CMD: u32>(&self, rt: &Runtime, arg: u32, flags: CmdFl) {
        self.cmdarg.write(arg);
        asm::fence();
        self.cmd.write(1 << 31 | 1 << 29 | CMD | flags.bits());
        rt.info(format_args!("CMD{CMD} {flags:032b} {arg:08x}"));
        self.wait(rt).await;
    }

    async fn sleep(&self, rt: &Runtime) {
        // TODO: fix this, don't drop interrupt
        loop {
            let event = rt.wait().await;
            if let tau::Event::Interrupt(id) = event {
                self.complete_interrupt(rt, id);
            } else if let tau::Event::Timeout = event {
                break;
            }
        }
    }

    async fn wait(&self, rt: &Runtime) -> u32 {
        loop {
            if let tau::Event::Interrupt(id) = rt.wait().await {
                let int = self.complete_interrupt(rt, id);
                if int != 0 {
                    return int;
                }
            }
        }
    }

    fn complete_interrupt(&self, rt: &Runtime, id: InterruptId) -> u32 {
        rt.complete_interrupt(id);
        let int = self.mintsts.read();
        let dma_status = self.i_dmac_status.read();
        if int != 0 || dma_status != 0 {
            if int != 0 {
                self.rintsts.write(int);
                rt.debug(format_args!("interrupt: {int:08x}"));
            }
            if dma_status != 0 {
                self.i_dmac_status.write(dma_status & 0x3ff);
                rt.debug(format_args!("dma status: {dma_status:08x}"));
            }
        }
        int
    }
}
