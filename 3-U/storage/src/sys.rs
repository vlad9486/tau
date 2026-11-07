// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use core::{fmt, num::NonZero, time::Duration};

pub fn sleep(_id: u8, delay: Duration, freq: u64) {
    let tick = ((delay.as_nanos() * u128::from(freq)) / 1_000_000_000) as usize;
    let deadline = unsafe { NonZero::new_unchecked(tau::asm::read_time().wrapping_add(tick)) };
    loop {
        match tau::Ubi::respond_keep(0, [0, deadline.get()]) {
            Ok([]) => break,
            Err(_err) => {
                let eps = 100;
                if tau::asm::read_time() + eps >= deadline.get() {
                    break;
                }
            }
        }
    }
}

// TODO: system logger
pub fn write(_args: fmt::Arguments<'_>, freq: u64) {
    let _nanos = ((tau::asm::read_time() as u128) * 1_000_000_000) / u128::from(freq);
}

// TODO: handle error
pub fn wait_interrupt() {
    // TODO: handle timeout
    let _ = tau::Ubi::respond_keep::<_, 0>(0, [0, 0]);
}
