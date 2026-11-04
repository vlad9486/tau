// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

pub trait Testable {
    fn run(&self);
}

pub fn test_runner(_tests: &[&dyn Testable]) {}
