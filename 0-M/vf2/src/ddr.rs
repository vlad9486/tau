// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// Upstream license: GPL-2.0-or-later.
// See LICENSE for the full license text.

// JH7110 DDR sequence translated from starfive-tech/u-boot c4c67bb.

use super::{
    mmio::{read, write, update},
    delay_us,
    ddr_data::*,
};

const CRG: usize = 0x1302_0000;
const CTRL: usize = 0x1570_0000;
const PHY: usize = 0x1300_0000;
const PHY_BASE: usize = 0x800 * 4;
const PHY_AC_BASE: usize = 0x1000 * 4;
const SIZE_8G: u32 = 1 << 27;

fn write_words(base: usize, values: &[u32]) {
    for (index, &value) in values.iter().enumerate() {
        write(base + index * 4, value);
    }
}

fn apply(base: usize, secondary: usize, data: &[Cfg], size: u32, phy: bool) {
    for cfg in data {
        if cfg.flags & size == 0 {
            continue;
        }
        let address = if phy {
            base + ((if cfg.flags & (1 << 31) != 0 {
                0x1000
            } else {
                0x800
            }) + cfg.offset as usize)
                * 4
        } else {
            (if cfg.flags & (1 << 31) != 0 {
                secondary
            } else {
                base
            }) + cfg.offset as usize
        };
        if cfg.flags & 1 != 0 {
            write(address, (read(address) & cfg.mask) | cfg.value);
        } else if cfg.flags & 2 != 0 || !phy {
            write(address, cfg.value);
        } else {
            write(address, read(address).wrapping_add(cfg.value));
        }
    }
}

fn bus_clock(div: u32) {
    update(CRG + 0xac, 0x3f << 24, div << 24);
}

fn reset_pulse(id: u32) {
    let bit = 1u32 << (id % 32);
    let address = CRG + 0x2f8 + (id / 32) as usize * 4;
    update(address, bit, bit);
    update(address, bit, 0);
    let status = address + 0x10;
    while read(status) & bit == 0 {
        core::hint::spin_loop();
    }
}

fn wait_bits(address: usize, mask: u32, set: bool) {
    while (read(address) & mask != 0) != set {
        core::hint::spin_loop();
    }
}

pub fn init() {
    // The VF2 DDR node selects 2133 MT/s. This build selects the user's
    // 8 GiB board and the vendor driver's default LPDDR4 type.
    bus_clock(0);
    super::clock::pll1();
    delay_us(100);
    bus_clock(1);
    for id in [39, 40, 38] {
        reset_pulse(id);
    }

    write_words(PHY + PHY_BASE, &LPDDR4_TRAIN_DATA);
    let util = &LPDDR4_PHY_DATA;
    // U-Boot passes PHY_AC_BASE (0x1000 words) to ddr_phy_util(), while
    // ddr_phy_train() receives PHY_BASE (0x800 words). These are distinct
    // hardware windows. Writing util data at PHY would overwrite controls.
    write_words(PHY + PHY_AC_BASE + 1792 * 4, &util[1792..]);
    write_words(PHY + PHY_AC_BASE, &util[..1792]);
    apply(PHY, 0, &LPDDR4_START_CFG, SIZE_8G, true);
    write(PHY, 1);
    bus_clock(0);

    apply(CTRL, CTRL + 0x1000, &LPDDR4_CSR_CFG, SIZE_8G, false);
    wait_bits(CTRL + 0x504, 0x8000_0000, true);
    write(CTRL + 0x504, 0);
    write(CTRL + 0x50c, 0);
    delay_us(300);
    write(CTRL + 0x50c, 1);
    delay_us(3000);
    write(CTRL + 0x10, 0x3c);
    write(CTRL + 0x14, 1);
    delay_us(4);
    apply(CTRL, CTRL + 0x1000, &LPDDR4_CSR_CFG1, SIZE_8G, false);
    delay_us(4);
    write(CTRL + 0x10, 0x11);
    write(CTRL + 0x14, 1);
    write(CTRL + 0x10, 0x20);
    write(CTRL + 0x14, 1);
    delay_us(4);
    write(CTRL + 0x10, 0x21);
    write(CTRL + 0x14, 1);
    write(CTRL + 0x514, 0);
    wait_bits(CTRL + 0x518, 2, true);
    while read(CTRL + 0x518) & 2 != 0 {
        let command = read(PHY + 4);
        if command & 0x20 != 0 {
            match command & 0x1f {
                0 => bus_clock(0),
                1 => bus_clock(3),
                2 => bus_clock(1),
                _ => {}
            }
            write(PHY + 8, 1);
            wait_bits(PHY + 8, 1, false);
        }
        delay_us(1);
    }
    let _ = read(PHY + (2048 + 83) * 4);
    let value = read(PHY + (2048 + 84) * 4);
    write(PHY + (2048 + 84) * 4, value & 0xf800_0000);
    apply(PHY + PHY_BASE, CTRL + 0x1000, &DDR_CSR_CFG2, SIZE_8G, false);
    apply(CTRL, CTRL + 0x1000, &LPDDR4_CSR_CFG3, SIZE_8G, false);
}
