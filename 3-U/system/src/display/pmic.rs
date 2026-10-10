// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! Minimal polling transactions to AXP15060 on the board's DW I2C5 bus.

use super::{Error, Registers, spin};

pub(super) struct Pmic(Registers);

const DATA: usize = 0x10;
const RAW_INTR: usize = 0x34;
const ABORT: u32 = 1 << 6;
const STOP: u32 = 1 << 9;

impl Pmic {
    pub fn new() -> Self {
        Self(Registers::map(0x1205_0000, 0x1000))
    }

    fn wait(&self, cond: impl Fn() -> bool) -> Result<(), Error> {
        spin(5_000_000, cond).map_err(|_| Error::Pmic)
    }

    fn begin(&self) -> Result<(), Error> {
        self.0.write(0x6c, 0);
        self.wait(|| self.0.read(0x9c) & 1 == 0)?; // ENABLE_STATUS.
        self.0.write(0x00, 0x63); // Master, standard speed, restart, no slave.
        self.0.write(0x04, 0x36); // PMIC's 7-bit I2C address.
        // Conservative counters work with both 24 and 49.5 MHz input clocks.
        self.0.write(0x14, 250);
        self.0.write(0x18, 300);
        self.0.write(0x7c, 15); // SDA hold.
        self.0.write(0x30, 0); // Interrupts masked; do not enable PLIC IRQ 50.
        let _ = self.0.read(0x40); // Clear stale interrupts.
        self.0.write(0x6c, 1);
        self.wait(|| self.0.read(0x9c) & 1 != 0)
    }

    fn end(&self) -> Result<(), Error> {
        let result = self.wait(|| self.0.read(RAW_INTR) & (STOP | ABORT) != 0);
        let abort = self.0.read(RAW_INTR) & ABORT != 0;
        self.0.write(0x6c, 0);
        self.wait(|| self.0.read(0x9c) & 1 == 0)?;
        let _ = self.0.read(0x40);
        result?;
        if abort { Err(Error::Pmic) } else { Ok(()) }
    }

    fn read(&self, address: u32) -> Result<u32, Error> {
        self.begin()?;
        self.0.write(DATA, address);
        self.0.write(DATA, (1 << 8) | (1 << 9) | (1 << 10)); // Read, stop, restart.
        let result = self.wait(|| self.0.read(0x78) != 0 || self.0.read(RAW_INTR) & ABORT != 0);
        // Always finish/disable on errors so later devices can use the bus.
        let value = if self.0.read(0x78) != 0 {
            self.0.read(DATA) & 0xff
        } else {
            0
        };
        let end = self.end();
        result?;
        end?;
        Ok(value)
    }

    fn write(&self, address: u32, value: u32) -> Result<(), Error> {
        self.begin()?;
        self.0.write(DATA, address);
        self.0.write(DATA, value | (1 << 9));
        self.end()
    }

    fn update(&self, address: u32, mask: u32, value: u32) -> Result<(), Error> {
        self.write(address, (self.read(address)? & !mask) | value)
    }

    pub fn enable_1p8(&self) -> Result<(), Error> {
        // AXP15060 ALDO3: 0.7 V + selector * 0.1 V, enable bit 2.
        self.update(0x21, 0x1f, 11)?;
        self.update(0x11, 1 << 2, 1 << 2)
    }

    pub fn enable_0p9(&self) -> Result<(), Error> {
        // ALDO5 has the same selector encoding, enable bit 4.
        self.update(0x23, 0x1f, 2)?;
        self.update(0x11, 1 << 4, 1 << 4)?;
        if self.read(0x11)? & 0x14 != 0x14 {
            return Err(Error::Pmic);
        }
        Ok(())
    }
}
