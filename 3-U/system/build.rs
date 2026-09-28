// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

fn main() {
    println!("cargo:rustc-link-arg-bin=system=-eMANIFEST");
}
