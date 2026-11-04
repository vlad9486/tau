// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

#![no_std]
#![cfg_attr(
    feature = "nightly",
    feature(custom_test_frameworks),
    test_runner(tau::tester::test_runner),
    feature(strict_provenance_lints),
    warn(implicit_provenance_casts)
)]

pub mod csr;

pub mod sbi;

pub mod space;

pub mod llfree;

pub mod scheduler;

pub mod matter;

pub mod state;
