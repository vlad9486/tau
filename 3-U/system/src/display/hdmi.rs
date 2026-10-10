// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::{MODE, Registers};

pub(super) struct Hdmi(Registers);

impl Hdmi {
    pub fn new() -> Self {
        Self(Registers::map(0x2959_0000, 0x4000))
    }

    // Innosilicon's published indices are spaced four bytes apart on JH7110.
    fn read(&self, index: usize) -> u32 {
        self.0.read(index * 4)
    }
    fn write(&self, index: usize, value: u32) {
        self.0.write(index * 4, value);
    }

    pub fn connected(&self) -> bool {
        self.read(0xc8) & (1 << 7) != 0
    }
    pub fn locked(&self) -> bool {
        self.read(0x1a9) & 1 != 0 && self.read(0x1af) & 1 != 0
    }

    pub fn prepare(&self) {
        self.write(0xc0, 0); // EDID interrupts masked.
        self.write(0xc2, 0); // HDCP interrupts masked.
        self.write(0xc8, 2); // Clear HPD flag, mask its IRQ; read-only HPD remains.
        self.write(0x00, 0x63); // Keep transmitter powered down while configuring.
        self.write(0x1b2, 0);
        self.write(0x1b0, self.read(0x1b0) | 4); // Bandgap.
        self.write(0x1cc, 0x0f); // PHY resistor calibration.
        // StarFive U-Boot's 24 MHz reference / 148.5 MHz pixel + TMDS table.
        // See tau-docs/visionfive2/reference/display/starfive-u-boot/.../sf_hdmi.c.
        for (index, value) in [
            (0x1a0, 0x01),
            (0x1aa, 0x0f),
            (0x1a1, 0x01),
            (0x1a2, 0xf0),
            (0x1a3, 0x63),
            (0x1a4, 0x15),
            (0x1a5, 0x41),
            (0x1a6, 0x42),
            (0x1ab, 0x01),
            (0x1ac, 0x14),
            (0x1ad, 0x01),
            (0x1d1, 0),
            (0x1d2, 0),
            (0x1d3, 0),
            (0x1aa, 0x0e),
            (0x1a0, 0x00),
        ] {
            self.write(index, value);
        }
    }

    fn word(&self, index: usize, value: u32) {
        self.write(index, value & 0xff);
        self.write(index + 1, (value >> 8) & 0xff);
    }

    pub fn enable(&self) {
        self.write(0x1b4, 7); // PHY LDOs.
        self.write(0x1be, 0x71); // Serializers.
        self.write(0x1bf, 2); // 1080p eye-diagram settings from vendor Linux.
        self.write(0x1c0, 0x22);
        self.write(0x01, 1); // SDR RGB444 with external DE.
        self.write(0x02, 0x30); // 8-bit RGB input and RGB output.
        self.write(0x03, 1); // No automatic CSC or R/B swap.
        self.write(0x04, 0x1c); // Full RGB range, no CSC or deep color.
        self.write(0x05, 0x82); // Clear AV mute, leave unused audio muted.
        self.write(0x52, 2); // HDMI mode, HDCP disabled.
        self.word(0x09, MODE.h_total);
        self.word(0x0b, MODE.h_total - MODE.width);
        self.word(0x0d, MODE.h_total - MODE.h_start);
        self.word(0x0f, MODE.h_end - MODE.h_start);
        self.word(0x11, MODE.v_total);
        self.write(0x13, MODE.v_total - MODE.height);
        self.write(0x14, MODE.v_total - MODE.v_start);
        self.write(0x15, MODE.v_end - MODE.v_start);
        self.write(0x08, 0x0d); // External progressive timing, positive H/V sync.
        // AVI packet: HDMI, RGB, 16:9, VIC 16; checksum over header + payload.
        self.write(0x9f, 6); // Select AVI packet buffer.
        for (index, value) in [
            (0xa0, 0x82),
            (0xa1, 2),
            (0xa2, 13),
            (0xa3, 0x2f),
            (0xa4, 0),
            (0xa5, 0x28),
            (0xa6, 0x08),
            (0xa7, 16),
        ] {
            self.write(index, value);
        }
        for index in 0xa8..=0xb0 {
            self.write(index, 0);
        }
        self.write(0xc9, 0x10); // Normal framebuffer input, not HDMI BIST bars.
        self.write(0x00, 0x61);
        self.write(0x1b2, 0x8f); // TMDS drivers.
    }

    pub fn sync(&self) {
        self.write(0xce, 0);
        self.write(0xce, 1);
    }

    pub fn disable(&self) {
        self.write(0x1b2, 0);
        self.write(0x00, 0x63);
        self.write(0x1be, 0);
        self.write(0x1b4, 0);
        self.write(0x1a0, 1);
        self.write(0x1aa, 1);
    }
}
