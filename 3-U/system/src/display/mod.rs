// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

//! VisionFive 2 CPU-rendered ARGB8888 scanout, fixed at 1920x1080/60 Hz.
//! One task owns the framebuffer; completions mean drawing finished, not vblank.

use super::register::Register;
use super::scheduler::{DriverState, Shared, spin};

mod controller;
mod drawing;
mod hdmi;
mod mode;
mod platform;
mod pmic;
mod registers;
mod state;

use self::controller::Controller;
use self::hdmi::Hdmi;
use self::mode::MODE;
use self::platform::Platform;
use self::pmic::Pmic;
use self::registers::Registers;

pub use self::drawing::Task;
pub use self::state::{Error, State};

// Also recorded in 0-M/boards/vf2's FDT reservation table. Never access the
// cached alias: VOUT is not coherent with the U74 caches.
const FRAMEBUFFER_PHYS: usize = 0x7100_0000;
const FRAMEBUFFER_RESERVATION: usize = 0x80_0000;
const UNCACHED_OFFSET: usize = 0x4_0000_0000;
const TIMER_ISSUER: u8 = 5;

const _: () = assert!(MODE.bytes() <= FRAMEBUFFER_RESERVATION);
const _: () = assert!(FRAMEBUFFER_PHYS + MODE.bytes() <= u32::MAX as usize);
