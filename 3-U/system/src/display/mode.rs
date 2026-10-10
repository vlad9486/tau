// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

pub(super) struct Mode {
    pub width: u32,
    pub height: u32,
    pub h_start: u32,
    pub h_end: u32,
    pub h_total: u32,
    pub v_start: u32,
    pub v_end: u32,
    pub v_total: u32,
}

// CTA VIC 16, 148.5 MHz pixel clock, positive H/V sync.
pub(super) const MODE: Mode = Mode {
    width: 1920,
    height: 1080,
    h_start: 2008,
    h_end: 2052,
    h_total: 2200,
    v_start: 1084,
    v_end: 1089,
    v_total: 1125,
};

impl Mode {
    pub const fn stride(&self) -> usize {
        (self.width as usize * 4).div_ceil(128) * 128
    }

    pub const fn bytes(&self) -> usize {
        self.stride() * self.height as usize
    }

    pub const fn size(&self) -> u32 {
        self.width | (self.height << 15)
    }

    pub const fn horizontal(&self) -> (u32, u32) {
        (
            self.width | (self.h_total << 16),
            self.h_start | (self.h_end << 15) | (1 << 30),
        )
    }

    pub const fn vertical(&self) -> (u32, u32) {
        (
            self.height | (self.v_total << 16),
            self.v_start | (self.v_end << 15) | (1 << 30),
        )
    }
}
