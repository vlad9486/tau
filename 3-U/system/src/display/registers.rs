// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use super::Register;

/// A mapped bank of word-wide volatile registers, addressed by byte offset.
pub(super) struct Registers(&'static [Register<u32, u32>]);

impl Registers {
    pub fn map(base: usize, len: usize) -> Self {
        Self(tau::Area::new(base, len).sl())
    }

    pub fn read(&self, offset: usize) -> u32 {
        self.0[offset / 4].read()
    }

    pub fn write(&self, offset: usize, value: u32) {
        self.0[offset / 4].write(value);
        // Order device accesses even across different register banks.
        unsafe { core::arch::asm!("fence iorw, iorw") };
    }

    pub fn update(&self, offset: usize, mask: u32, value: u32) {
        self.write(offset, (self.read(offset) & !mask) | (value & mask));
    }
}
