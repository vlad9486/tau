use core::num::NonZeroUsize;

use thiserror_no_std::Error;

use tau::DtbProps;

use super::{
    asm,
    register::Register,
    driver::{self, Timeout, Runtime},
};

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("dts missing register address")]
    DtsMissingReg,
    #[error("control reset {0}")]
    Control(Timeout),
    #[error("clock setup {0}")]
    Clock(Timeout),
}

pub async fn run(rt: Runtime<'_>, config: DtbProps<'_>, v_addr: usize) -> Result<(), DriverError> {
    let Some([addr_hi, addr_lo, size_hi, size_lo]) = config.find_int(|name| name == "reg") else {
        return Err(DriverError::DtsMissingReg);
    };
    let addr = ((addr_hi.to_be() as usize) << 32) + (addr_lo.to_be() as usize);
    let size = ((size_hi.to_be() as usize) << 32) + (size_lo.to_be() as usize);
    tau::Ubi::map(NonZeroUsize::new(addr), v_addr, size >> 12).unwrap_or_default();
    let reg = unsafe { &*(v_addr as *const Reg) };
    let rca = reg.init(rt).await?;
    reg.test(rt, rca, v_addr + size).await;

    Ok(())
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
const CLK_SET_TO: u32 = 1000;

#[allow(dead_code)]
const MMC_READ_BLOCKS: u32 = 18;
#[allow(dead_code)]
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
    async fn init(&self, rt: Runtime<'_>) -> Result<u32, DriverError> {
        rt.info(format_args!("off"));
        self.pwren.write(0u32);
        for _ in 0..13 {
            self.wait_timeout(rt).await;
        }
        rt.info(format_args!("on"));
        self.pwren.write(1u32);
        for _ in 0..13 {
            self.wait_timeout(rt).await;
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
        self.ctrl.write(self.ctrl.read() | Ctrl::ENABLE_INTERRUPTS);

        self.init_clk().await?;

        self.ctype.write(0u32);

        self.send_cmd::<0>(rt, 0, CmdFl::empty()).await;
        self.wait_timeout(rt).await;
        self.timeout.write(u32::MAX);
        self.fifo_threshold
            .write(self.fifo_threshold.read() | ((2u32 << 28) | (15 << 16) | 16));

        self.send_cmd::<8>(rt, 0x1aa, CmdFl::RESP_EXP | CmdFl::RESP_CRC)
            .await;

        // TODO: timeout
        self.wait_timeout(rt).await;
        loop {
            // CMD55: Prefix for ACMD
            self.send_cmd::<55>(rt, 0, CmdFl::RESP_EXP).await;
            // ACMD41: Card Initialization
            let r = self.send_cmd::<41>(rt, 0xc0ff8000, CmdFl::RESP_EXP).await;
            if r & (1 << 31) != 0 {
                if r & (1 << 24) != 0 {
                    self.send_cmd::<11>(rt, 0, CmdFl::RESP_EXP).await;
                }

                break;
            } else {
                self.wait_timeout(rt).await;
            }
        }

        self.send_cmd::<2>(rt, 0, CmdFl::RESP_EXP | CmdFl::RESP_LONG_EXP)
            .await;
        let rca = self.send_cmd::<3>(rt, 0, CmdFl::RESP_EXP).await & 0xffff0000;
        self.send_cmd::<7>(rt, rca, CmdFl::RESP_EXP).await;

        loop {
            if self.send_cmd::<13>(rt, rca, CmdFl::RESP_EXP).await == 0x900 {
                break;
            }
        }

        Ok(rca)
    }

    async fn init_clk(&self) -> Result<(), DriverError> {
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

    async fn test(&self, rt: Runtime<'_>, rca: u32, dma_virt: usize) {
        #[repr(C, align(32))]
        pub struct IdmacDesc {
            pub des0: u32, // control bits
            pub des1: u32, // buffer size
            pub des2: u32, // buffer address (physical)
            pub des3: u32, // next descriptor address (physical)
        }

        self.bus_mod
            .write((self.bus_mod.read() & !((1 << 1) | (1 << 7))) | (1 << 0));

        self.i_dmac_status
            .write(self.i_dmac_status.read() | 0b1100110111);
        self.i_dmac_int_enable.write(0b1100110111_u32);

        self.bytcnt.write(0x400_u32);
        self.blksiz.write(0x200_u32);
        {
            let dma_desc_phys = 0x7000_0000;
            let dma_buf_phys = 0x7000_1000;
            tau::Ubi::map(NonZeroUsize::new(0x7000_0000), dma_virt, 1).unwrap_or_default();
            tau::Ubi::map(NonZeroUsize::new(0x7000_1000), dma_virt + 0x1000, 1).unwrap_or_default();
            unsafe {
                (dma_virt as *mut IdmacDesc).write_volatile(IdmacDesc {
                    des0: 0b1000_0000_0000_0000_0000_0000_0001_1100,
                    des1: 0x400,
                    des2: dma_buf_phys as u32,
                    des3: 0,
                });

                ((dma_virt + 0x1000) as *mut [u8; 0x400]).write_volatile([0xcf; 0x400]);
                asm::fence();
            }

            self.desc_base.write(dma_desc_phys as u32);

            self.ctrl
                .write(self.ctrl.read() | Ctrl::ENABLE_DMA | Ctrl::USE_INTERNAL_DMAC);
            asm::fence();

            self.bus_mod
                .write(self.bus_mod.read() | (1 << 1) | (1 << 7));
        }

        let start_block = 0x700000 / 0x200;
        let flags = CmdFl::RESP_EXP
            | CmdFl::DATA_EXP
            | CmdFl::DATA_WRITE
            | CmdFl::PREV_DATA
            | CmdFl::A_STOP;
        self.send_cmd::<MMC_WRITE_BLOCKS>(rt, start_block, flags)
            .await;
        loop {
            let r = self.send_cmd::<13>(rt, rca, CmdFl::RESP_EXP).await;
            if (r >> 9) & 0b1111 == 4 {
                break;
            }
            self.wait_timeout(rt).await;
        }

        let IdmacDesc {
            des0,
            des1,
            des2,
            des3,
        } = unsafe { (dma_virt as *mut IdmacDesc).read_volatile() };
        rt.info(format_args!("{des0:08x} {des1:08x} {des2:08x} {des3:08x}"));

        // let dma_data = unsafe { ((dma_virt + 0x1000) as *mut [u8; 0x800]).read_volatile() };
        // for chunk in dma_data.chunks(0x10) {
        //     rt.info(format_args!("{chunk:x?}"));
        // }
        rt.info(format_args!("status: {:032b}", self.status.read()));
    }

    async fn send_cmd<const CMD: u32>(&self, rt: Runtime<'_>, arg: u32, flags: CmdFl) -> u32 {
        self.cmdarg.write(arg);
        asm::fence();
        self.cmd.write(1 << 31 | 1 << 29 | CMD | flags.bits());
        rt.info(format_args!("CMD{CMD} {flags:032b} {arg:08x}"));
        self.wait(rt).await;
        if flags.contains(CmdFl::RESP_EXP) {
            if flags.contains(CmdFl::RESP_LONG_EXP) {
                let r0 = self.resp0.read();
                let r1 = self.resp1.read();
                let r2 = self.resp2.read();
                let r3 = self.resp3.read();
                rt.info(format_args!(
                    "response: {r0:08x} {r1:08x} {r2:08x} {r3:08x}"
                ));
                0
            } else {
                let r = self.resp0.read();
                rt.info(format_args!("response: {r:08x}"));
                r
            }
        } else {
            0
        }
    }

    async fn wait_timeout(&self, rt: Runtime<'_>) {
        // TODO: fix this, don't drop interrupt
        if let tau::Event::Interrupt(id) = rt.wait().await {
            rt.complete_interrupt(id);
        }
    }

    async fn wait(&self, rt: Runtime<'_>) -> u32 {
        loop {
            let tau::Event::Interrupt(id) = rt.wait().await else {
                continue;
            };
            let int = self.mintsts.read();
            let dma_status = self.i_dmac_status.read();
            if int != 0 || dma_status != 0 {
                if int != 0 {
                    self.rintsts.write(int);
                    rt.info(format_args!("interrupt: {int:08x}"));
                }
                if dma_status != 0 {
                    rt.info(format_args!("dma status: {dma_status:08x}"));
                    self.i_dmac_status.write(dma_status & 0x3ff);
                }
                rt.complete_interrupt(id);
                break int;
            }
        }
    }
}
