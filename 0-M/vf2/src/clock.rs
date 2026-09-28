// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// Upstream license: GPL-2.0-or-later.
// See LICENSE for the full license text.

// JH7110 clock values from starfive-tech/u-boot c4c67bb.

use super::mmio::update;

const SYSCON: usize = 0x1303_0000;
const CRG: usize = 0x1302_0000;

fn field(address: usize, mask: u32, shift: u32, value: u32) {
    update(address, mask, value << shift);
}

fn pll(
    pd: usize,
    dac: usize,
    dacbit: u32,
    dsm: u32,
    fb: usize,
    fbshift: u32,
    pre: usize,
    prediv: u32,
    fbdiv: u32,
) {
    field(SYSCON + pd, 1 << 27, 27, 1);
    field(SYSCON + dac, 1 << dacbit, dacbit, 1);
    field(SYSCON + dac, 1 << dsm, dsm, 1);
    field(SYSCON + pre, 0x3f, 0, prediv);
    field(SYSCON + fb, 0xfff << fbshift, fbshift, fbdiv);
    field(SYSCON + pd, 3 << 28, 28, 0);
    field(SYSCON + pd, 1 << 27, 27, 0);
}

pub fn early() {
    // PLL0: 1 GHz; PLL2: 1.188 GHz. Then select the vendor's root clocks.
    pll(0x20, 0x18, 24, 25, 0x1c, 0, 0x24, 3, 125);
    pll(0x30, 0x2c, 15, 16, 0x2c, 17, 0x34, 2, 99);
    for (offset, mask) in [
        (0x00, 1 << 24),
        (0x14, 1 << 24),
        (0x10, 1 << 24),
        (0x180, 1 << 31),
        (0x168, 1 << 24),
    ] {
        update(CRG + offset, mask, mask);
    }
    update(0x1700_0004, 1 << 24, 1 << 24);
    // PLL2 / 3 / 2 feeds AHB at 198 MHz; APB is 49.5 MHz.
    update(CRG + 7 * 4, 0x3, 3);
    update(CRG + 8 * 4, 0x3, 2);
    update(CRG + 11 * 4, 0xf, 4);
    update(CRG + 9 * 4, 1 << 31, 1 << 31);
    update(CRG + 12 * 4, 1 << 31, 1 << 31);
}

pub fn pll1() {
    // LPDDR4 2133 mode: 1066 MHz DDR PLL.
    pll(0x28, 0x24, 15, 16, 0x24, 17, 0x2c, 12, 533);
}

fn gpio(gpio: usize, input: usize, output: usize, enable: u32) {
    let base = 0x1304_0000;
    let off = (gpio / 4) * 4;
    let shift = (gpio % 4) * 8;
    update(base + off, 0x3f << shift, enable << shift);
    update(base + 0x40 + off, 0x7f << shift, (output as u32) << shift);
    let inoff = (input / 4) * 4;
    let inshift = (input % 4) * 8;
    update(
        base + 0x80 + inoff,
        0x7f << inshift,
        (gpio as u32 + 2) << inshift,
    );
}

pub fn sdio1() {
    gpio_output(10, 55, 0);
    pin_drive(10, 2, true);
    gpio(9, 44, 57, 19);
    pin_drive(9, 1, false);
    gpio(11, 45, 58, 20);
    pin_drive(11, 1, false);
    gpio(12, 46, 59, 21);
    pin_drive(12, 1, false);
    gpio(7, 47, 60, 22);
    pin_drive(7, 1, false);
    gpio(8, 48, 61, 23);
    pin_drive(8, 1, false);
    // Clock IDs 92 and 94 in starfive-jh7110-clkgen.h.
    // AXI_CFG0 is 396 MHz, so /8 gives 49.5 MHz to the SD controller.
    update(CRG + 92 * 4, 1 << 31, 1 << 31);
    update(CRG + 94 * 4, (1 << 31) | 0xf, (1 << 31) | 8);
    // Reset 65 is active high.
    update(CRG + 0x300, 1 << 1, 0);
}

fn gpio_output(gpio: usize, output: usize, enable: u32) {
    let base = 0x1304_0000;
    let off = (gpio / 4) * 4;
    let shift = (gpio % 4) * 8;
    update(base + off, 0x3f << shift, enable << shift);
    update(base + 0x40 + off, 0x7f << shift, (output as u32) << shift);
}

fn pin_drive(gpio: usize, strength: u32, slew: bool) {
    update(0x1304_0000 + 0x120 + gpio * 4, 0x6, strength << 1);
    if slew {
        update(0x1304_0000 + 0x120 + gpio * 4, 1 << 5, 1 << 5);
    }
}

pub fn uart0_rx() {
    // TX, gates, resets and the UART registers were initialized before Rust.
    // Configure only the RX pin here; do not clear the working TX FIFO.
    let base = 0x1304_0000;
    let input = 14usize;
    update(base + 4, 0x3f << 16, 1 << 16);
    update(
        base + 0x80 + (input / 4) * 4,
        0x7f << ((input % 4) * 8),
        8 << ((input % 4) * 8),
    );
}
