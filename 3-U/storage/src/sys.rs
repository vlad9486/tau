// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{fmt, num::NonZero, ptr, sync::atomic::Ordering, time::Duration};

use super::MANIFEST;

pub fn sleep(_id: u8, delay: Duration, freq: u64) {
    let tick = ((delay.as_nanos() * u128::from(freq)) / 1_000_000_000) as usize;
    let deadline = unsafe { NonZero::new_unchecked(tau::asm::read_time().wrapping_add(tick)) };
    let slot = MANIFEST.wait_slots.get(0).unwrap();
    slot.occupied.store(true, Ordering::SeqCst);
    let _ = tau::Ubi::wait(ptr::from_ref(slot), Some(deadline));
}

// TODO: system logger
pub fn write(_args: fmt::Arguments<'_>, freq: u64) {
    let _nanos = ((tau::asm::read_time() as u128) * 1_000_000_000) / u128::from(freq);
}

// TODO: handle error
pub fn wait_interrupt() {
    let slot = MANIFEST.wait_slots.get(1).unwrap();
    slot.occupied.store(true, Ordering::SeqCst);
    let _ = tau::Ubi::wait(ptr::from_ref(slot), None);
}
