// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::{FRAMEBUFFER_PHYS, MODE, Registers};

pub(super) struct Controller(Registers);

impl Controller {
    pub fn new() -> Self {
        Self(Registers::map(0x2940_0000, 0x3000))
    }

    pub fn identity(&self) -> (u32, u32) {
        (self.0.read(0x24), self.0.read(0x30))
    }

    pub fn stop(&self) {
        self.0.write(0x14, 0); // Mask all host interrupts; this driver polls.
        self.0.write(0x1ccc, 0); // Stop both panels before changing state.
        self.0.write(0x1418, 0x111);
        self.0.write(0x141c, 0x111);
        self.0.write(0x1cc0, 0);
        self.0.write(0x1cc4, 0);
    }

    pub fn start(&self) {
        let reg = &self.0;
        self.stop();
        // Disable overlays and cursors. Fresh VOUT reset supplies defaults for
        // the unused scaling, color conversion and compression blocks.
        for offset in [0x1540, 0x1544, 0x1548, 0x154c, 0x1468, 0x24e8] {
            reg.write(offset, 0);
        }
        reg.write(0x1cc0, 1 << 12); // Primary 0 shadow registers.
        reg.write(0x2518, 1); // Hold panel configuration until the commit below.
        reg.write(0x1400, FRAMEBUFFER_PHYS as u32);
        reg.write(0x1408, MODE.stride() as u32);
        reg.write(0x1800, 0); // Linear origin.
        reg.write(0x1810, MODE.size());
        reg.write(0x24d8, 0);
        reg.write(0x24e0, MODE.size()); // Exclusive bottom-right.
        reg.write(0x1518, 6 << 26); // ARGB8888, ARGB swizzle, linear, no scaling.
        reg.write(0x2510, 1 << 1); // Disable alpha blending.
        reg.write(0x1ce8, 0x0f61); // Vendor scanout watermark.
        reg.write(0x1cc0, (1 << 13) | (1 << 12)); // Enable primary 0 on panel 0.
        let (horizontal, hsync) = MODE.horizontal();
        let (vertical, vsync) = MODE.vertical();
        reg.write(0x1430, horizontal);
        reg.write(0x1438, hsync);
        reg.write(0x1440, vertical);
        reg.write(0x1448, vsync);
        reg.write(0x1410, 0); // No dithering.
        reg.write(0x14b0, 0); // No panel rotation.
        reg.write(0x1cd0, 0xd2); // RGB888, DPI output (DP enable bit 3 clear).
        reg.write(0x14b8, 5); // DPI RGB888.
        reg.write(0x14d0, 0x52); // Vendor RGB output configuration.
        reg.write(0x1a28, 0); // Disable panel 0 degamma.
        reg.write(0x1418, 0x1111); // Enable DE, data, clock and panel; no gamma.
        reg.write(0x2518, 0); // Commit panel shadow configuration.
        reg.write(0x1ccc, 1); // Start panel 0 only.
    }
}
