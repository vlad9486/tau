// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::{Error, Pmic, Registers, spin};

pub(super) struct Platform {
    sys: Registers,
    syscon: Registers,
    vout: Registers,
    pmu: Registers,
    gpio: Registers,
    pmic: Pmic,
}

const GATE: u32 = 1 << 31;
const VOUT_POWER: u32 = 1 << 4;
const VOUT_RESETS: u32 = 7 | (1 << 9); // DC AXI/AHB/core and HDMI.

impl Platform {
    pub fn new() -> Self {
        Self {
            sys: Registers::map(0x1302_0000, 0x1000),
            syscon: Registers::map(0x1303_0000, 0x1000),
            vout: Registers::map(0x295c_0000, 0x1000),
            pmu: Registers::map(0x1703_0000, 0x1000),
            gpio: Registers::map(0x1304_0000, 0x1000),
            pmic: Pmic::new(),
        }
    }

    fn reset(
        &self,
        bank: &Registers,
        control: usize,
        status: usize,
        mask: u32,
        assert: bool,
    ) -> Result<(), Error> {
        bank.update(control, mask, if assert { mask } else { 0 });
        // Assert bits are active high; status bits report deasserted state.
        spin(100_000, || {
            bank.read(status) & mask == if assert { 0 } else { mask }
        })
        .map_err(|_| Error::Reset)
    }

    fn pin(&self, gpio: usize, input: usize, output: u32, enable: u32, pad: u32) {
        let offset = (gpio / 4) * 4;
        let shift = (gpio % 4) * 8;
        self.gpio.update(offset, 0x3f << shift, enable << shift);
        self.gpio
            .update(0x40 + offset, 0x7f << shift, output << shift);
        let shift = (input % 4) * 8;
        self.gpio.update(
            0x80 + (input / 4) * 4,
            0x7f << shift,
            (gpio as u32 + 2) << shift,
        );
        self.gpio.write(0x120 + gpio * 4, pad);
    }

    pub fn supplies(&self) -> Result<(), Error> {
        // Board I2C5 on GPIO19/20. Preserve unrelated pinmux fields and rails.
        self.gpio.update(0x29c, 0xe000_0000, 0);
        self.gpio.update(0x2a0, 7, 0);
        self.pin(19, 79, 0, 42, 9);
        self.pin(20, 80, 0, 43, 9);
        self.sys.update(143 * 4, GATE, GATE); // I2C5 APB gate.
        self.reset(&self.sys, 0x300, 0x310, 1 << 17, false)?; // Reset 81.
        self.pmic.enable_1p8()
    }

    pub fn power_on(&self) -> Result<(), Error> {
        self.pmic.enable_0p9()?;
        if !self.powered() {
            self.pmu.write(0x0c, VOUT_POWER);
            for encourage in [0xff, 0x05, 0x50] {
                self.pmu.write(0x44, encourage);
            }
        }
        Ok(())
    }

    pub fn powered(&self) -> bool {
        self.pmu.read(0x80) & VOUT_POWER != 0
    }

    pub fn configure(&self) -> Result<(), Error> {
        // Tau SPL configures PLL2 = 24 MHz * 99 / 2 = 1.188 GHz.
        // Do not retune a shared PLL used by other devices.
        if (self.syscon.read(0x2c) >> 17) & 0xfff != 99
            || self.syscon.read(0x34) & 0x3f != 2
            || self.syscon.read(0x30) & ((3 << 28) | (1 << 27)) != 0
        {
            return Err(Error::Clock);
        }
        self.sys.update(59 * 4, 7, 3); // VOUT AXI = PLL2 / 3 = 396 MHz.
        for clock in [58, 60, 61, 62] {
            self.sys.update(clock * 4, GATE, GATE);
        }
        self.reset(&self.sys, 0x2f8, 0x308, 1 << 26, false)?; // NOC display AXI.
        self.reset(&self.sys, 0x2fc, 0x30c, 1 << 11, false)?; // VOUT source.
        self.vout.update(0, 0x1f, 4); // VOUT APB = AHB / 4.
        self.vout.update(4, 0x3f, 8); // Pixel divider: 1.188 GHz / 8 = 148.5 MHz.
        for clock in [4, 5, 6, 17] {
            self.vout.update(clock * 4, GATE, GATE);
        }
        self.vout.update(7 * 4, GATE | (1 << 24), GATE); // DC pixel0: divider source.
        self.vout.update(9 * 4, GATE | (1 << 24), GATE); // LCD/HDMI DPI: pixel0.
        // The vendor probe enables all three HDMI clocks even for video-only
        // output. Supply internal MCLK/BCLK sources; keep audio muted in HDMI.
        self.sys.update(16 * 4, 0x1f, 24); // Audio root: PLL2 / 24.
        self.sys.update(17 * 4, 0x1f, 2); // Internal MCLK: 24.75 MHz.
        self.sys.update(18 * 4, 1 << 24, 0); // Select internal MCLK.
        self.sys.update(63 * 4, GATE, GATE); // Top HDMI MCLK.
        self.sys.update(162 * 4, GATE | 0x1f, GATE | 8); // BCLK master.
        self.sys.update(165 * 4, 1 << 24, 0); // Select internal BCLK.
        for clock in [15, 16] {
            self.vout.update(clock * 4, GATE, GATE);
        }
        self.reset(&self.vout, 0x48, 0x4c, VOUT_RESETS, true)?;
        self.reset(&self.vout, 0x48, 0x4c, VOUT_RESETS, false)?;
        self.pin(15, 8, 0, 1, 1); // HDMI HPD input.
        Ok(())
    }
}
