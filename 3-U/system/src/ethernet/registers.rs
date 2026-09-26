//! DWMAC4/5 register layout used by the Ethernet driver.

use crate::register::Register;

#[repr(C)]
pub(super) struct Registers {
    pub mac_config: Register<u32, u32>, // 0x0000
    _reserved_0004: [u32; 0x1],
    pub mac_packet_filter: Register<u32, u32>, // 0x0008
    _reserved_000c: [u32; 0x19],
    pub mac_tx_flow_control: Register<u32, u32>, // 0x0070
    _reserved_0074: [u32; 0x7],
    pub mac_rx_flow_control: Register<u32, u32>, // 0x0090
    _reserved_0094: [u32; 0x3],
    pub mac_rx_queue_control0: Register<u32, u32>, // 0x00a0
    pub mac_rx_queue_control1: Register<u32, u32>, // 0x00a4
    _reserved_00a8: [u32; 0x3],
    pub mac_irq_enable: Register<u32, u32>, // 0x00b4
    _reserved_00b8: [u32; 0x6],
    pub mac_lpi_control_status: Register<u32, u32>, // 0x00d0
    _reserved_00d4: [u32; 0xf],
    pub mac_version: Register<u32, u32>, // 0x0110
    _reserved_0114: [u32; 0x2],
    pub mac_hw_feature0: Register<u32, u32>, // 0x011c
    pub mac_hw_feature1: Register<u32, u32>, // 0x0120
    pub mac_hw_feature2: Register<u32, u32>, // 0x0124
    _reserved_0128: [u32; 0x36],
    pub mdio_addr: Register<u32, u32>, // 0x0200
    pub mdio_data: Register<u32, u32>, // 0x0204
    _reserved_0208: [u32; 0x3e],
    pub mac_address0_high: Register<u32, u32>, // 0x0300
    pub mac_address0_low: Register<u32, u32>,  // 0x0304
    _reserved_0308: [u32; 0x101],
    pub mmc_rx_irq_mask: Register<u32, u32>, // 0x070c
    pub mmc_tx_irq_mask: Register<u32, u32>, // 0x0710
    _reserved_0714: [u32; 0x3b],
    pub mmc_rx_ipc_irq_mask: Register<u32, u32>, // 0x0800
    _reserved_0804: [u32; 0x10b],
    pub mtl_rx_queue_dma_map0: Register<u32, u32>, // 0x0c30
    _reserved_0c34: [u32; 0x33],
    pub mtl_tx_queue0_operation_mode: Register<u32, u32>, // 0x0d00
    _reserved_0d04: [u32; 0x1],
    pub mtl_tx_queue0_debug: Register<u32, u32>, // 0x0d08
    _reserved_0d0c: [u32; 0x3],
    pub mtl_queue0_irq_control_status: Register<u32, u32>, // 0x0d18
    _reserved_0d1c: [u32; 0x5],
    pub mtl_rx_queue0_operation_mode: Register<u32, u32>, // 0x0d30
    _reserved_0d34: [u32; 0xb3],
    pub dma_mode: Register<u32, u32>,        // 0x1000
    pub dma_sysbus_mode: Register<u32, u32>, // 0x1004
    _reserved_1008: [u32; 0x3e],
    pub dma_channel0_control: Register<u32, u32>, // 0x1100
    pub tx_control: Register<u32, u32>,           // 0x1104
    pub rx_control: Register<u32, u32>,           // 0x1108
    _reserved_110c: [u32; 0x1],
    pub tx_descriptor_list_high: Register<u32, u32>, // 0x1110
    pub tx_descriptor_list_low: Register<u32, u32>,  // 0x1114
    pub rx_descriptor_list_high: Register<u32, u32>, // 0x1118
    pub rx_descriptor_list_low: Register<u32, u32>,  // 0x111c
    pub tx_tail: Register<u32, u32>,                 // 0x1120
    _reserved_1124: [u32; 0x1],
    pub rx_tail: Register<u32, u32>,        // 0x1128
    pub tx_ring_length: Register<u32, u32>, // 0x112c
    pub rx_ring_length: Register<u32, u32>, // 0x1130
    pub dma_irq_enable: Register<u32, u32>, // 0x1134
    pub rx_watchdog: Register<u32, u32>,    // 0x1138
    _reserved_113c: [u32; 0x9],
    pub dma_status: Register<u32, u32>, // 0x1160
}

const _: () = {
    use core::mem::{offset_of, size_of};

    assert!(offset_of!(Registers, mac_config) == 0x0000);
    assert!(offset_of!(Registers, mac_packet_filter) == 0x0008);
    assert!(offset_of!(Registers, mac_tx_flow_control) == 0x0070);
    assert!(offset_of!(Registers, mac_rx_flow_control) == 0x0090);
    assert!(offset_of!(Registers, mac_rx_queue_control0) == 0x00a0);
    assert!(offset_of!(Registers, mac_rx_queue_control1) == 0x00a4);
    assert!(offset_of!(Registers, mac_irq_enable) == 0x00b4);
    assert!(offset_of!(Registers, mac_lpi_control_status) == 0x00d0);
    assert!(offset_of!(Registers, mac_version) == 0x0110);
    assert!(offset_of!(Registers, mac_hw_feature0) == 0x011c);
    assert!(offset_of!(Registers, mac_hw_feature1) == 0x0120);
    assert!(offset_of!(Registers, mac_hw_feature2) == 0x0124);
    assert!(offset_of!(Registers, mdio_addr) == 0x0200);
    assert!(offset_of!(Registers, mdio_data) == 0x0204);
    assert!(offset_of!(Registers, mac_address0_high) == 0x0300);
    assert!(offset_of!(Registers, mac_address0_low) == 0x0304);
    assert!(offset_of!(Registers, mmc_rx_irq_mask) == 0x070c);
    assert!(offset_of!(Registers, mmc_tx_irq_mask) == 0x0710);
    assert!(offset_of!(Registers, mmc_rx_ipc_irq_mask) == 0x0800);
    assert!(offset_of!(Registers, mtl_rx_queue_dma_map0) == 0x0c30);
    assert!(offset_of!(Registers, mtl_tx_queue0_operation_mode) == 0x0d00);
    assert!(offset_of!(Registers, mtl_tx_queue0_debug) == 0x0d08);
    assert!(offset_of!(Registers, mtl_queue0_irq_control_status) == 0x0d18);
    assert!(offset_of!(Registers, mtl_rx_queue0_operation_mode) == 0x0d30);
    assert!(offset_of!(Registers, dma_mode) == 0x1000);
    assert!(offset_of!(Registers, dma_sysbus_mode) == 0x1004);
    assert!(offset_of!(Registers, dma_channel0_control) == 0x1100);
    assert!(offset_of!(Registers, tx_control) == 0x1104);
    assert!(offset_of!(Registers, rx_control) == 0x1108);
    assert!(offset_of!(Registers, tx_descriptor_list_high) == 0x1110);
    assert!(offset_of!(Registers, tx_descriptor_list_low) == 0x1114);
    assert!(offset_of!(Registers, rx_descriptor_list_high) == 0x1118);
    assert!(offset_of!(Registers, rx_descriptor_list_low) == 0x111c);
    assert!(offset_of!(Registers, tx_tail) == 0x1120);
    assert!(offset_of!(Registers, rx_tail) == 0x1128);
    assert!(offset_of!(Registers, tx_ring_length) == 0x112c);
    assert!(offset_of!(Registers, rx_ring_length) == 0x1130);
    assert!(offset_of!(Registers, dma_irq_enable) == 0x1134);
    assert!(offset_of!(Registers, rx_watchdog) == 0x1138);
    assert!(offset_of!(Registers, dma_status) == 0x1160);
    assert!(size_of::<Registers>() == 0x1164);
};
