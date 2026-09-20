//! VisionFive 2 v1.3B YT8531/DWMAC raw Ethernet bring-up.
//! See docs/visionfive2/transmit-test.md for the boot contract and frame format.

mod registers;
use self::registers::Registers;

mod rx;
pub use self::rx::{RxTask, RxDone, RxError};

mod tx;
pub use self::tx::{TxTask, TxDone, TxError};

use core::time::Duration;

use super::{
    scheduler::{self, Shared, DriverState},
    register::Register,
};

const OWN: u32 = 1 << 31;
const RING_LEN: usize = 4;
const DMA_IRQ_MASK: u32 = (1 << 15) | (1 << 14) | (1 << 12) | (1 << 6) | 1;
const DMA_ACK_MASK: u32 = 0xd7ff;

#[repr(C)]
struct Descriptor([Register<u32, u32>; 4]);

const _: () = assert!(size_of::<Descriptor>() == 16);

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
    tx_descriptors: &'static [Descriptor; RING_LEN],
    rx_descriptors: &'static [Descriptor; RING_LEN],
    rx_index: usize,
    rx_pending: Option<RxTask>,
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
        let rings = tau::Area::new(dma_phys as usize, 0x1000).r::<[Descriptor; 32]>();
        Some(Self {
            reg: area.r(),
            port,
            phy: 0,
            phase: Phase::Start,
            tx_descriptors: rings[..RING_LEN].try_into().ok()?,
            // RX ring shares the descriptor page, 256 bytes after TX.
            rx_descriptors: rings[16..16 + RING_LEN].try_into().ok()?,
            rx_index: 0,
            rx_pending: None,
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
        scheduler::spin(100_000, || self.reg.mdio_addr.read() & 1 == 0)
            .map_err(|_| "MDIO busy timeout")
    }

    fn mdio(&self, phy: u8, register: u8, value: Option<u16>) -> Result<u16, &'static str> {
        self.wait_mdio()?;
        if let Some(value) = value {
            self.reg.mdio_data.write(u32::from(value));
        }
        // Clause 22; CR=5 divides the <=300MHz CSR clock by 124 (vendor setting).
        let op = if value.is_some() { 1 } else { 3 };
        tau::asm::fence();
        self.reg
            .mdio_addr
            .write((u32::from(phy) << 21) | (u32::from(register) << 16) | (5 << 8) | (op << 2) | 1);
        self.wait_mdio()?;
        Ok(self.reg.mdio_data.read() as u16)
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
        let crg = tau::Area::new(base, 0x10000).r::<[Register<u32, u32>; 0x4000]>();
        let cfg = tau::Area::new(syscon, 0x10000).r::<[Register<u32, u32>; 0x4000]>();
        crg[ahb / 4].write(crg[ahb / 4].read() | (1 << 31));
        crg[axi / 4].write(crg[axi / 4].read() | (1 << 31));
        let sys = if self.port == 1 {
            crg
        } else {
            tau::Area::new(0x1302_0000, 0x10000).r::<[Register<u32, u32>; 0x4000]>()
        };
        // Keep firmware's GTX divider/root configuration, enable GTX/GTXC gates.
        let (gtx, gtxc) = if self.port == 0 {
            (0x1b0, 0x1bc)
        } else {
            (0x190, 0x1ac)
        };
        sys[gtx / 4].write(sys[gtx / 4].read() | (1 << 31));
        sys[gtxc / 4].write(sys[gtxc / 4].read() | (1 << 31));
        crg[rtx / 4].write((crg[rtx / 4].read() & !0x1f) | 1);
        crg[tx / 4].write((crg[tx / 4].read() & !(0x3f << 24)) | (1 << 31) | (1 << 24));
        crg[rx / 4].write(crg[rx / 4].read() & !(0x3f << 24));
        cfg[mode / 4].write((cfg[mode / 4].read() & !(7 << shift)) | (1 << shift)); // RGMII
        crg[rst / 4].write(crg[rst / 4].read() & !mask);
        tau::asm::fence();
        scheduler::spin(100_000, || crg[status / 4].read() & mask == mask)
            .map_err(|_| "GMAC bus reset did not deassert")
    }

    fn configure_dma(&mut self) {
        let r = self.reg;
        for desc in self.tx_descriptors.iter().chain(self.rx_descriptors) {
            for word in &desc.0 {
                word.write(0u32);
            }
        }
        self.index = 0;
        self.pending = false;
        self.rx_index = 0;
        // No EEE, flow control, CRC/pad stripping, or jumbo frames.
        r.mac_irq_enable.write(0u32);
        r.mac_lpi_control_status.write(0u32);
        r.mac_tx_flow_control.write(0u32);
        r.mac_rx_flow_control.write(0u32);
        r.mac_rx_queue_control0.write(0u32);
        r.rx_control.write(0u32);
        // MMC counter interrupts share macirq; leave them masked.
        r.mmc_rx_irq_mask.write(u32::MAX);
        r.mmc_tx_irq_mask.write(u32::MAX);
        r.mmc_rx_ipc_irq_mask.write(u32::MAX);
        r.mac_address0_high.write(u32::from(self.port + 1) << 8);
        r.mac_address0_low.write(0x5541_5402u32); // 02:54:41:55:00:01/02
        let speed_bits: u32 = match self.link >> 14 {
            2 => 0,
            1 => (1 << 15) | (1 << 14),
            _ => 1 << 15,
        };
        r.mac_config.write(speed_bits | (1 << 13));
        r.mac_packet_filter.write(0u32); // perfect unicast + broadcast, no promiscuous mode
        r.mac_rx_queue_control0.write(2u32); // RX queue 0: DCB mode
        r.mac_rx_queue_control1.write(1u32 << 20); // multicast/broadcast routed to queue 0
        r.mtl_rx_queue_dma_map0.write(0u32); // RX queue 0 -> DMA channel 0
        let rx_fifo_log = r.mac_hw_feature1.read() & 0x1f;
        let rx_fifo = 128u32.checked_shl(rx_fifo_log).unwrap_or(2048);
        r.mtl_rx_queue0_operation_mode
            .write(((rx_fifo / 256).saturating_sub(1).min(0x3ff) << 20) | (1 << 5));
        // Hardware feature 1 encodes FIFO bytes as 128 << field.
        let fifo_log = (r.mac_hw_feature1.read() >> 6) & 0x1f;
        let fifo_bytes = 128u32.checked_shl(fifo_log).unwrap_or(2048);
        let queue_size = (fifo_bytes / 256).saturating_sub(1).min(0x1ff);
        r.mtl_tx_queue0_operation_mode
            .write((queue_size << 16) | (2 << 2) | (1 << 1));
        r.mtl_queue0_irq_control_status.write(0x10u32);
        r.dma_sysbus_mode
            .write((3u32 << 24) | (3 << 16) | (1 << 3) | (1 << 2) | (1 << 1));
        r.dma_channel0_control.write(0u32); // contiguous 16-byte descriptors, PBLx8 off
        r.tx_control.write(16u32 << 16);
        r.tx_descriptor_list_high.write(0u32);
        r.tx_descriptor_list_low.write(self.dma_phys);
        r.tx_ring_length.write((RING_LEN - 1) as u32);
        r.tx_tail.write(self.dma_phys);
        r.rx_control
            .write((16 << 16) | (u32::from(rx::BUFFER_SIZE) << 1));
        r.rx_descriptor_list_high.write(0u32);
        r.rx_descriptor_list_low.write(self.dma_phys + 0x100);
        r.rx_ring_length.write((RING_LEN - 1) as u32);
        r.rx_tail.write(self.dma_phys + 0x100);
        r.rx_watchdog.write(0u32); // RX watchdog off; every packet requests an IRQ
        r.dma_status.write(DMA_ACK_MASK);
        r.dma_irq_enable.write(DMA_IRQ_MASK);
        tau::asm::fence();
        r.mac_config.write(r.mac_config.read() | (1 << 1));
        r.tx_control.write(r.tx_control.read() | 1);
        // A link change may have reset an outstanding RX. Only after SWR
        // clears can its descriptor safely be rebuilt using the same buffer.
        if let Some(task) = self.rx_pending.take() {
            self.receive(task);
        }
    }

    fn receive(&mut self, task: RxTask) {
        let desc = &self.rx_descriptors[self.rx_index].0;
        desc[0].write(task.phys);
        desc[1].write(0u32);
        desc[2].write(0u32);
        tau::asm::fence();
        desc[3].write(OWN | (1 << 30) | (1 << 24)); // IOC, buffer 1 valid
        tau::asm::fence();
        let next = (self.rx_index + 1) % RING_LEN;
        self.reg
            .rx_tail
            .write(self.dma_phys + 0x100 + (next * 16) as u32);
        self.reg.rx_control.write(self.reg.rx_control.read() | 1);
        self.reg.mac_config.write(self.reg.mac_config.read() | 1);
        self.rx_pending = Some(task);
    }

    fn submit_rx(&mut self, shared: &mut Shared) {
        let port = usize::from(self.port);
        if self.rx_pending.is_some() || shared.ethernet_rx_done[port].is_some() {
            return;
        }
        let Some(task) = shared.ethernet_rx_task[port] else {
            return;
        };
        let error = if !rx::valid(task) {
            RxError::InvalidBuffer
        } else {
            match self.phase {
                Phase::Ready => {
                    shared.ethernet_rx_task[port] = None;
                    self.receive(task);
                    return;
                }
                Phase::Failed => RxError::Failed,
                _ => return, // A posted receive waits for link, without busy retrying.
            }
        };
        shared.ethernet_rx_task[port] = None;
        shared.ethernet_rx_done[port] = Some(Err(error));
    }

    fn send(&mut self, task: TxTask) {
        let desc = &self.tx_descriptors[self.index].0;
        desc[0].write(task.phys);
        desc[1].write(0u32);
        desc[2].write((1u32 << 31) | u32::from(task.len)); // completion interrupt
        tau::asm::fence();
        desc[3].write(OWN | (1 << 29) | (1 << 28) | u32::from(task.len));
        tau::asm::fence();
        let next = (self.index + 1) % RING_LEN;
        self.reg.tx_tail.write(self.dma_phys + (next * 16) as u32);
        self.pending = true;
        self.pending_ticks = 0;
    }

    /// Called by the scheduler even without an interrupt or timer event.
    pub fn submit(&mut self, shared: &mut Shared) {
        self.submit_rx(shared);
        let port = usize::from(self.port);
        if self.pending || shared.ethernet_tx_done[port].is_some() {
            return;
        }
        let Some(task) = shared.ethernet_tx_task[port].take() else {
            return;
        };
        let error = if !tx::valid(task) {
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
        shared.ethernet_tx_done[port] = Some(Err(error));
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
                let version = self.reg.mac_version.read();
                shared.write(format_args!(
                    "eth{}: version={version:08x} features={:08x}/{:08x}/{:08x} DMA={:08x}",
                    self.port,
                    self.reg.mac_hw_feature0.read(),
                    self.reg.mac_hw_feature1.read(),
                    self.reg.mac_hw_feature2.read(),
                    self.dma_phys
                ));
                if !matches!(version & 0xff, 0x51 | 0x52) {
                    return Err("unsupported GMAC version (expected 5.10/5.20)");
                }
                self.reg.dma_irq_enable.write(0u32);
                self.reg.mac_irq_enable.write(0u32);
                self.reg.tx_control.write(self.reg.tx_control.read() & !1);
                self.reg.rx_control.write(self.reg.rx_control.read() & !1);
                self.reg.mac_config.write(self.reg.mac_config.read() & !3);
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
                    self.reg.dma_mode.write(1u32);
                    self.phase = Phase::DmaReset(0);
                } else if self.ticks.is_multiple_of(50) {
                    shared.write(format_args!(
                        "eth{}: waiting for cable/link, BMSR={status:04x}",
                        self.port
                    ));
                }
            }
            Phase::DmaReset(attempt) if timer => {
                if self.reg.dma_mode.read() & 1 == 0 {
                    self.configure_dma();
                    self.phase = Phase::Ready;
                } else if attempt == 20 {
                    return Err("DMA reset timeout (check PHY/TX clocks)");
                } else {
                    self.phase = Phase::DmaReset(attempt + 1);
                }
            }
            Phase::Ready => {
                let status = self.reg.dma_status.read();
                self.reg.dma_status.write(status & DMA_ACK_MASK);
                if status & (1 << 12) != 0 {
                    // Preserve the fault bits before the W1C acknowledgement.
                    shared.write(format_args!(
                        "eth{}: fatal DMA status={status:08x}",
                        self.port
                    ));
                    return Err("DMA fatal bus error");
                }
                if self.pending {
                    let desc = self.tx_descriptors[self.index].0[3].read();
                    if desc & OWN == 0 {
                        tau::asm::fence();
                        shared.write(format_args!(
                            "eth{}: TX completed descriptor={desc:08x} DMA={status:08x} irqs={}",
                            self.port, self.irq_count
                        ));
                        shared.ethernet_tx_done[usize::from(self.port)] =
                            Some(if desc & (1 << 15) != 0 {
                                Err(TxError::Descriptor)
                            } else {
                                Ok(())
                            });
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
                if self.rx_pending.is_some() {
                    let status = self.rx_descriptors[self.rx_index].0[3].read();
                    if let Some(done) = rx::completion(status) {
                        tau::asm::fence();
                        shared.ethernet_rx_done[usize::from(self.port)] = Some(done);
                        self.rx_pending = None;
                        self.rx_index = (self.rx_index + 1) % RING_LEN;
                    }
                }
                if timer && !self.pending && self.ticks.is_multiple_of(10) {
                    self.phy_read(1)?;
                    if self.phy_read(1)? & 4 == 0 || self.phy_read(0x11)? & 0xe000 != self.link {
                        self.reg.dma_irq_enable.write(0u32);
                        self.reg.tx_control.write(self.reg.tx_control.read() & !1);
                        self.reg.rx_control.write(self.reg.rx_control.read() & !1);
                        self.reg.mac_config.write(self.reg.mac_config.read() & !3);
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
                self.reg.dma_status.read(),
                self.reg.tx_control.read(),
                self.reg.mtl_tx_queue0_debug.read()
            ));
            self.reg.dma_irq_enable.write(0u32);
            self.reg.tx_control.write(self.reg.tx_control.read() & !1);
            self.reg.rx_control.write(self.reg.rx_control.read() & !1);
            self.reg.mac_config.write(self.reg.mac_config.read() & !3);
            self.phase = Phase::Failed;
            if self.rx_pending.take().is_some() {
                shared.ethernet_rx_done[usize::from(self.port)] =
                    Some(Err(RxError::BufferUnavailable));
            }
            if self.pending {
                // Stopping the channel alone does not prove all bus accesses
                // have drained. Do not promise the caller buffer ownership.
                self.pending = false;
                shared.ethernet_tx_done[usize::from(self.port)] =
                    Some(Err(TxError::BufferUnavailable));
            }
        }
        // Interrupts never enqueue timers: exactly one outstanding per MAC.
        if timer && !matches!(self.phase, Phase::Failed) {
            shared.sleep(self.timer_issuer(), Duration::from_millis(100));
        }
    }
}
