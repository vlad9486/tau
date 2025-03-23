use core::{arch, num::NonZeroUsize};

use thiserror_no_std::Error;

use tau::DtbProps;

use super::{
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
    reg.test(rt, rca).await;

    Ok(())
}

#[repr(C, align(0x1000))]
struct Reg {
    ctrl: Register<Ctrl, Ctrl>,
    _pwren: Register<u32, u32>,
    clkdiv: Register<u32, u32>,
    clksrc: Register<u32, u32>,
    clkena: Register<u32, u32>,
    timeout: Register<u32, u32>,
    _ctype: Register<u32, u32>,
    blksiz: Register<u32, u32>,
    bytcnt: Register<u32, u32>,
    int_mask: Register<Interrupt, Interrupt>,
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

const MMC_GO_IDLE_STATE: u32 = 0;
const SD_SEND_IF_COND: u32 = 8;
const MMC_READ_SINGLE_BLOCK: u32 = 17;
const MMC_APP_CMD: u32 = 55;
const MMC_APP_CARD_INIT: u32 = 41;

bitflags::bitflags! {
    struct CmdFl: u32 {
        const RESP_EXP = 1 << 6;
        const RESP_LONG_EXP = 1 << 7;
        const RESP_CRC = 1 << 8;
        const DATA_EXP = 1 << 9;
        const DATA_WRITE = 1 << 10;
        const A_STOP = 1 << 12;
        const PREV_DATA = 1 << 13;
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
        rt.info(format_args!("init"));

        let ctrl_reset = Ctrl::RESET_CONTROLLER | Ctrl::FIFO_RESET | Ctrl::DMA_RESET;
        self.ctrl.write(ctrl_reset);
        driver::spin(CTRL_RESET_TO, || !self.ctrl.read().intersects(ctrl_reset))
            .map_err(DriverError::Control)?;

        self.init_clk().await?;

        self.raw_int_status.write(u32::MAX);
        self.int_mask.write(Interrupt::empty());
        self.timeout.write(u32::MAX);
        self.raw_int_status.write(u32::MAX);
        self.int_mask.write(
            Interrupt::ERR | Interrupt::DONE | Interrupt::TXDR | Interrupt::RXDR | Interrupt::ACD,
        );
        self.ctrl.write(self.ctrl.read() | Ctrl::ENABLE_INTERRUPTS);

        self.send_cmd::<MMC_GO_IDLE_STATE>(rt, 0, CmdFl::SEND_INIT)
            .await;
        self.send_cmd::<SD_SEND_IF_COND>(rt, 0x1aa, CmdFl::RESP_EXP | CmdFl::RESP_CRC)
            .await;

        // TODO: timeout
        loop {
            // CMD55: Prefix for ACMD
            self.send_cmd::<MMC_APP_CMD>(rt, 0, CmdFl::RESP_EXP).await;
            // ACMD41: Card Initialization
            let r = self
                .send_cmd::<MMC_APP_CARD_INIT>(rt, 0xC0FF8000, CmdFl::RESP_EXP)
                .await;
            if r & (1 << 31) != 0 {
                if r & (1 << 24) != 0 {
                    self.send_cmd::<11>(rt, 0, CmdFl::RESP_EXP).await;
                }

                break;
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

    async fn test(&self, rt: Runtime<'_>, rca: u32) {
        let start_block = 20;
        self.bytcnt.write(0x200_u32);
        self.blksiz.write(0x200_u32);
        self.fifo_threshold.write(2u32 << 28 | (15 << 16) | 16);
        unsafe { arch::asm!("fence ow, ow") };
        self.send_cmd::<MMC_READ_SINGLE_BLOCK>(
            rt,
            start_block,
            CmdFl::RESP_EXP | CmdFl::DATA_EXP | CmdFl::A_STOP | CmdFl::PREV_DATA,
        )
        .await;
        //  | 1 << 10

        for i in 0..0x20 {
            let d0 = self.data.read().to_be();
            let d1 = self.data.read().to_be();
            let d2 = self.data.read().to_be();
            let d3 = self.data.read().to_be();
            let addr = start_block * 0x200 + i * 0x10;
            rt.info(format_args!(
                "0x{addr:08x}: {d0:08x} {d1:08x} {d2:08x} {d3:08x}",
            ));
            driver::spin(1000, || self.status.read() & (1u32 << 2) == 0).unwrap();
        }
        rt.info(format_args!("status: {:032b}", self.status.read()));

        self.send_cmd::<13>(rt, rca | 0x8000, CmdFl::RESP_EXP | CmdFl::PREV_DATA)
            .await;
    }

    async fn send_cmd<const CMD: u32>(&self, rt: Runtime<'_>, arg: u32, flags: CmdFl) -> u32 {
        self.cmd_arg.write(arg);
        unsafe { arch::asm!("fence ow, ow") };
        self.cmd.write(1 << 31 | CMD | flags.bits());
        rt.info(format_args!("CMD{CMD} {flags:032b} {arg:08x}"));
        if flags.contains(CmdFl::RESP_EXP) {
            self.wait(rt).await;
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

    async fn wait(&self, rt: Runtime<'_>) -> u32 {
        let id = rt.wait_interrupt().await;
        let int = self.mask_int_status.read();
        self.raw_int_status.write(int);
        rt.info(format_args!("interrupt: {int:016b}"));
        rt.complete_interrupt(id);
        int
    }
}
