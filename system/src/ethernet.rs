//! VisionFive 2 v1.3B YT8531/DWMAC TX-only bring-up.
//! See docs/visionfive2/transmit-test.md for the boot contract and frame format.
use core::time::Duration;

use super::{
    scheduler::{self, Shared, DriverState},
    register::Register,
};

const OWN: u32 = 1 << 31;
const RING_LEN: usize = 4;
const DMA_IRQ_MASK: u32 = (1 << 15) | (1 << 14) | (1 << 12) | 1;
const DMA_ACK_MASK: u32 = 0xd7ff;
// Byte offsets, not u32 array indices; DWMAC4/5 register layout.
const MAC_CONFIG: usize = 0;
const MDIO_ADDR: usize = 0x200;
const MDIO_DATA: usize = 0x204;
const DMA_MODE: usize = 0x1000;
const TX_CONTROL: usize = 0x1104;
const TX_TAIL: usize = 0x1120;
const DMA_IRQ_ENABLE: usize = 0x1134;
const DMA_STATUS: usize = 0x1160;

#[repr(C)]
struct Registers([Register<u32, u32>; 0x4000]);

impl Registers {
    fn read(&self, offset: usize) -> u32 {
        self.0[offset / 4].read()
    }

    fn write(&self, offset: usize, value: u32) {
        self.0[offset / 4].write(value);
    }

    fn modify(&self, offset: usize, clear: u32, set: u32) {
        self.write(offset, (self.read(offset) & !clear) | set);
    }
}

#[repr(C)]
struct Descriptor([Register<u32, u32>; 4]);

const _: () = assert!(size_of::<Descriptor>() == 16);

/// One complete Ethernet frame, without FCS. The caller owns the DMA mapping.
/// Keep the buffer valid and untouched until completion.
#[derive(Clone, Copy, Debug)]
pub struct TxTask {
    pub phys: u32,
    pub len: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    NotReady,
    InvalidBuffer,
    Failed,
    Descriptor,
    /// DMA did not release ownership. The buffer must remain reserved and
    /// untouched until the hardware has been reset (currently until reboot).
    BufferUnavailable,
}

pub type TxDone = Result<(), TxError>;

fn valid_tx(task: TxTask) -> bool {
    (14..=1514).contains(&task.len)
        && task.phys.checked_add(u32::from(task.len) - 1).is_some()
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Start,
    PhyReset(u8),
    Link,
    DmaReset(u8),
    Ready,
    Failed,
}

pub struct State {
    reg: &'static Registers,
    port: u8,
    phy: u8,
    phase: Phase,
    descriptors: &'static [Descriptor; RING_LEN],
    dma_phys: u32,
    index: usize,
    pending: bool,
    pending_ticks: u8,
    ticks: u32,
    link: u16,
    irq_count: u32,
}

impl State {
    pub fn new(config: tau::DtbProps<'_>) -> Option<Self> {
        let area = config.find_reg()?;
        // Board-specific profile: reject other controllers, including QEMU NICs.
        let port = match area.base {
            0x1603_0000 => 0,
            0x1604_0000 => 1,
            _ => return None,
        };
        // SDIO descriptors: 0x70000000; user.rs data: 0x70001000.
        // Two pages per MAC: descriptors, then a page reserved for user.rs.
        let dma_phys = 0x7000_2000 + u32::from(port) * 0x2000;
        Some(Self {
            reg: area.r(),
            port,
            phy: 0,
            phase: Phase::Start,
            descriptors: tau::Area::new(dma_phys as usize, 0x1000).r(),
            dma_phys,
            index: 0,
            pending: false,
            pending_ticks: 0,
            ticks: 0,
            link: 0,
            irq_count: 0,
        })
    }

    // Timer issuers 1/2 belong to SDIO/user.rs.
    pub fn timer_issuer(&self) -> u8 {
        3 + self.port
    }

    fn wait_mdio(&self) -> Result<(), &'static str> {
        scheduler::spin(100_000, || self.reg.read(MDIO_ADDR) & 1 == 0)
            .map_err(|_| "MDIO busy timeout")
    }

    fn mdio(&self, phy: u8, register: u8, value: Option<u16>) -> Result<u16, &'static str> {
        self.wait_mdio()?;
        if let Some(value) = value {
            self.reg.write(MDIO_DATA, u32::from(value));
        }
        // Clause 22; CR=5 divides the <=300MHz CSR clock by 124 (vendor setting).
        let op = if value.is_some() { 1 } else { 3 };
        tau::asm::fence();
        self.reg.write(
            MDIO_ADDR,
            (u32::from(phy) << 21) | (u32::from(register) << 16) | (5 << 8) | (op << 2) | 1,
        );
        self.wait_mdio()?;
        Ok(self.reg.read(MDIO_DATA) as u16)
    }

    fn phy_read(&self, register: u8) -> Result<u16, &'static str> {
        self.mdio(self.phy, register, None)
    }

    fn phy_write(&self, register: u8, value: u16) -> Result<(), &'static str> {
        self.mdio(self.phy, register, Some(value)).map(|_| ())
    }

    fn phy_modify_ext(&self, register: u16, clear: u16, set: u16) -> Result<(), &'static str> {
        self.phy_write(0x1e, register)?;
        let value = self.phy_read(0x1f)?;
        self.phy_write(0x1f, (value & !clear) | set)
    }

    fn configure_phy(&self) -> Result<(), &'static str> {
        // Exact v1.3B fields from the repo DTB and vendor motorcomm.c.
        self.phy_modify_ext(0xa001, 1 << 8, 0)?;
        self.phy_modify_ext(
            0xa010,
            (3 << 4) | (1 << 12) | (7 << 13),
            (3 << 4) | (6 << 13),
        )?;
        let (rx, tx) = if self.port == 0 { (10, 10) } else { (2, 0) };
        self.phy_modify_ext(
            0xa003,
            (15 << 10) | (15 << 4) | 15,
            (rx << 10) | (5 << 4) | tx,
        )?;
        // Advertise full-duplex 10/100/1000, without pause negotiation.
        self.phy_write(4, 0x0141)?;
        let gigabit = self.phy_read(9)?;
        self.phy_write(9, (gigabit & !0x0300) | 0x0200)?;
        self.phy_write(0, 0x1200)
    }

    fn platform_init(&self) -> Result<(), &'static str> {
        // Firmware supplies root clocks, pinmux, PHY power/reset. Enable local
        // bus gates and select the v1.3B external PHY clock path (divider = 1).
        // Sources: clk-starfive-jh7110-{aon,sys}.c and vendor reset-jh7110.c.
        let (base, ahb, axi, rtx, tx, rx, rst, status, mask, syscon, mode, shift) =
            if self.port == 0 {
                (
                    0x1700_0000,
                    0x08,
                    0x0c,
                    0x10,
                    0x14,
                    0x1c,
                    0x38,
                    0x3c,
                    3,
                    0x1701_0000,
                    0x0c,
                    18,
                )
            } else {
                (
                    0x1302_0000,
                    0x184,
                    0x188,
                    0x194,
                    0x1a4,
                    0x19c,
                    0x300,
                    0x310,
                    12,
                    0x1303_0000,
                    0x90,
                    2,
                )
            };
        let crg = tau::Area::new(base, 0x10000).r::<Registers>();
        let cfg = tau::Area::new(syscon, 0x10000).r::<Registers>();
        crg.modify(ahb, 0, 1 << 31);
        crg.modify(axi, 0, 1 << 31);
        let sys = if self.port == 1 {
            crg
        } else {
            tau::Area::new(0x1302_0000, 0x10000).r::<Registers>()
        };
        // Keep firmware's GTX divider/root configuration, enable GTX/GTXC gates.
        let (gtx, gtxc) = if self.port == 0 {
            (0x1b0, 0x1bc)
        } else {
            (0x190, 0x1ac)
        };
        sys.modify(gtx, 0, 1 << 31);
        sys.modify(gtxc, 0, 1 << 31);
        crg.modify(rtx, 0x1f, 1);
        crg.modify(tx, 0x3f << 24, (1 << 31) | (1 << 24));
        crg.modify(rx, 0x3f << 24, 0);
        cfg.modify(mode, 7 << shift, 1 << shift); // RGMII
        crg.modify(rst, mask, 0);
        tau::asm::fence();
        scheduler::spin(100_000, || crg.read(status) & mask == mask)
            .map_err(|_| "GMAC bus reset did not deassert")
    }

    fn configure_tx(&mut self) {
        let r = self.reg;
        for desc in self.descriptors {
            for word in &desc.0 {
                word.write(0u32);
            }
        }
        self.index = 0;
        self.pending = false;
        // No receive engine, EEE, flow control, or jumbo frames.
        for offset in [0xb4, 0xd0, 0x70, 0x90, 0xa0, 0x1108] {
            r.write(offset, 0);
        }
        // MMC counter interrupts share macirq; leave them masked.
        for offset in [0x70c, 0x710, 0x800] {
            r.write(offset, u32::MAX);
        }
        r.write(0x300, u32::from(self.port + 1) << 8);
        r.write(0x304, 0x5541_5402); // 02:54:41:55:00:01/02
        let speed_bits = match self.link >> 14 {
            2 => 0,
            1 => (1 << 15) | (1 << 14),
            _ => 1 << 15,
        };
        r.write(MAC_CONFIG, speed_bits | (1 << 13));
        // Hardware feature 1 encodes FIFO bytes as 128 << field.
        let fifo_log = (r.read(0x120) >> 6) & 0x1f;
        let fifo_bytes = 128u32.checked_shl(fifo_log).unwrap_or(2048);
        let queue_size = (fifo_bytes / 256).saturating_sub(1).min(0x1ff);
        r.write(0xd00, (queue_size << 16) | (2 << 2) | (1 << 1));
        r.write(0xd18, 0x10);
        r.write(
            0x1004,
            (3 << 24) | (3 << 16) | (1 << 3) | (1 << 2) | (1 << 1),
        );
        r.write(0x1100, 0); // contiguous 16-byte descriptors, PBLx8 off
        r.write(TX_CONTROL, 16 << 16);
        r.write(0x1110, 0);
        r.write(0x1114, self.dma_phys);
        r.write(0x112c, (RING_LEN - 1) as u32);
        r.write(TX_TAIL, self.dma_phys);
        r.write(DMA_STATUS, DMA_ACK_MASK);
        r.write(DMA_IRQ_ENABLE, DMA_IRQ_MASK);
        tau::asm::fence();
        r.modify(MAC_CONFIG, 0, 1 << 1);
        r.modify(TX_CONTROL, 0, 1);
    }

    fn send(&mut self, task: TxTask) {
        let desc = &self.descriptors[self.index].0;
        desc[0].write(task.phys);
        desc[1].write(0u32);
        desc[2].write((1u32 << 31) | u32::from(task.len)); // completion interrupt
        tau::asm::fence();
        desc[3].write(OWN | (1 << 29) | (1 << 28) | u32::from(task.len));
        tau::asm::fence();
        let next = (self.index + 1) % RING_LEN;
        self.reg.write(TX_TAIL, self.dma_phys + (next * 16) as u32);
        self.pending = true;
        self.pending_ticks = 0;
    }

    /// Called by the scheduler even without an interrupt or timer event.
    pub fn submit(&mut self, shared: &mut Shared) {
        let port = usize::from(self.port);
        if self.pending || shared.ethernet_done[port].is_some() {
            return;
        }
        let Some(task) = shared.ethernet_task[port].take() else {
            return;
        };
        let error = if !valid_tx(task) {
            TxError::InvalidBuffer
        } else {
            match self.phase {
                Phase::Ready => {
                    self.send(task);
                    return;
                }
                Phase::Failed => TxError::Failed,
                _ => TxError::NotReady,
            }
        };
        shared.ethernet_done[port] = Some(Err(error));
    }

    fn step(&mut self, shared: &mut Shared, timer: bool) -> Result<(), &'static str> {
        match self.phase {
            Phase::Start => {
                if let Err(error) = self.platform_init() {
                    // MAC MMIO may not be usable if the bus is still in reset.
                    shared.write(format_args!("eth{}: {error}", self.port));
                    self.phase = Phase::Failed;
                    return Ok(());
                }
                let version = self.reg.read(0x110);
                shared.write(format_args!(
                    "eth{}: version={version:08x} features={:08x}/{:08x}/{:08x} DMA={:08x}",
                    self.port,
                    self.reg.read(0x11c),
                    self.reg.read(0x120),
                    self.reg.read(0x124),
                    self.dma_phys
                ));
                if !matches!(version & 0xff, 0x51 | 0x52) {
                    return Err("unsupported GMAC version (expected 5.10/5.20)");
                }
                self.reg.write(DMA_IRQ_ENABLE, 0);
                self.reg.write(0xb4, 0);
                self.reg.modify(TX_CONTROL, 1, 0);
                self.reg.modify(0x1108, 1, 0);
                self.reg.modify(MAC_CONFIG, 3, 0);
                let mut found = None;
                for phy in 0..32 {
                    let id = (u32::from(self.mdio(phy, 2, None)?) << 16)
                        | u32::from(self.mdio(phy, 3, None)?);
                    if id == 0x4f51_e91b {
                        found = Some(phy);
                        break;
                    }
                }
                self.phy = found.ok_or("YT8531 not found on MDIO")?;
                shared.write(format_args!(
                    "eth{}: YT8531 at MDIO {}",
                    self.port, self.phy
                ));
                self.phy_write(0, 0x8000)?;
                self.phase = Phase::PhyReset(0);
            }
            Phase::PhyReset(attempt) if timer => {
                if self.phy_read(0)? & 0x8000 == 0 {
                    self.configure_phy()?;
                    self.phase = Phase::Link;
                    shared.write(format_args!(
                        "eth{}: waiting for auto-negotiation",
                        self.port
                    ));
                } else if attempt == 20 {
                    return Err("PHY reset timeout");
                } else {
                    self.phase = Phase::PhyReset(attempt + 1);
                }
            }
            Phase::Link if timer => {
                self.phy_read(1)?; // latch-low link bit
                let status = self.phy_read(1)?;
                if status & 0x24 == 0x24 {
                    self.link = self.phy_read(0x11)? & 0xe000;
                    if self.link & 0x2000 == 0 || self.link >> 14 == 3 {
                        return Err("unsupported negotiated speed/duplex");
                    }
                    let speed = match self.link >> 14 {
                        2 => 1000,
                        1 => 100,
                        _ => 10,
                    };
                    let invert = self.port == 0 || speed != 1000;
                    self.phy_modify_ext(0xa003, 1 << 14, if invert { 1 << 14 } else { 0 })?;
                    shared.write(format_args!(
                        "eth{}: link {speed} Mbit/s full duplex",
                        self.port
                    ));
                    self.reg.write(DMA_MODE, 1);
                    self.phase = Phase::DmaReset(0);
                } else if self.ticks.is_multiple_of(50) {
                    shared.write(format_args!(
                        "eth{}: waiting for cable/link, BMSR={status:04x}",
                        self.port
                    ));
                }
            }
            Phase::DmaReset(attempt) if timer => {
                if self.reg.read(DMA_MODE) & 1 == 0 {
                    self.configure_tx();
                    self.phase = Phase::Ready;
                } else if attempt == 20 {
                    return Err("DMA reset timeout (check PHY/TX clocks)");
                } else {
                    self.phase = Phase::DmaReset(attempt + 1);
                }
            }
            Phase::Ready => {
                let status = self.reg.read(DMA_STATUS);
                self.reg.write(DMA_STATUS, status & DMA_ACK_MASK);
                if status & (1 << 12) != 0 {
                    // Preserve the fault bits before the W1C acknowledgement.
                    shared.write(format_args!(
                        "eth{}: fatal DMA status={status:08x}",
                        self.port
                    ));
                    return Err("DMA fatal bus error");
                }
                if self.pending {
                    let desc = self.descriptors[self.index].0[3].read();
                    if desc & OWN == 0 {
                        tau::asm::fence();
                        shared.write(format_args!(
                            "eth{}: TX completed descriptor={desc:08x} DMA={status:08x} irqs={}",
                            self.port, self.irq_count
                        ));
                        shared.ethernet_done[usize::from(self.port)] = Some(
                            if desc & (1 << 15) != 0 {
                                Err(TxError::Descriptor)
                            } else {
                                Ok(())
                            },
                        );
                        self.pending = false;
                        self.index = (self.index + 1) % RING_LEN;
                    } else if timer {
                        self.pending_ticks += 1;
                        if self.pending_ticks >= 20 {
                            shared.write(format_args!(
                                "eth{}: stalled descriptor={desc:08x} DMA={status:08x}",
                                self.port
                            ));
                            return Err("TX completion timeout");
                        }
                    }
                }
                if timer && !self.pending && self.ticks.is_multiple_of(10) {
                    self.phy_read(1)?;
                    if self.phy_read(1)? & 4 == 0 || self.phy_read(0x11)? & 0xe000 != self.link {
                        self.reg.write(DMA_IRQ_ENABLE, 0);
                        self.reg.modify(TX_CONTROL, 1, 0);
                        self.reg.modify(MAC_CONFIG, 2, 0);
                        self.phase = Phase::Link;
                        shared.write(format_args!("eth{}: link changed, waiting", self.port));
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }
}

impl DriverState for State {
    fn handle(&mut self, shared: &mut Shared, event: tau::Event) {
        if matches!(self.phase, Phase::Failed) {
            return;
        }
        let timer = matches!(event, tau::Event::Timeout);
        if timer {
            self.ticks = self.ticks.wrapping_add(1);
        }
        if matches!(event, tau::Event::Interrupt { .. }) {
            self.irq_count = self.irq_count.wrapping_add(1);
        }
        if let Err(error) = self.step(shared, timer) {
            shared.write(format_args!(
                "eth{}: {error}; phase={:?} DMA={:08x} TX={:08x} MTL={:08x}",
                self.port,
                self.phase,
                self.reg.read(DMA_STATUS),
                self.reg.read(TX_CONTROL),
                self.reg.read(0xd08)
            ));
            self.reg.write(DMA_IRQ_ENABLE, 0);
            self.reg.modify(TX_CONTROL, 1, 0);
            self.reg.modify(MAC_CONFIG, 3, 0);
            self.phase = Phase::Failed;
            if self.pending {
                // Stopping the channel alone does not prove all bus accesses
                // have drained. Do not promise the caller buffer ownership.
                self.pending = false;
                shared.ethernet_done[usize::from(self.port)] =
                    Some(Err(TxError::BufferUnavailable));
            }
        }
        // Interrupts never enqueue timers: exactly one outstanding per MAC.
        if timer && !matches!(self.phase, Phase::Failed) {
            shared.sleep(self.timer_issuer(), Duration::from_millis(100));
        }
    }
}
