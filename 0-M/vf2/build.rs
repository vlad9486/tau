// SPDX-FileCopyrightText: 2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use std::{env, path::PathBuf};

fn main() {
    let script = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("spl.lds");
    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rerun-if-changed=src/uart_early.S");
    println!("cargo:rerun-if-changed=src/start.S");
    println!(
        "cargo:rustc-link-arg-bin=tau-visionfive2-spl=-T{}",
        script.display()
    );
}
