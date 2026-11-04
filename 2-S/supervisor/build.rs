// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use std::{env, path::PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let script = manifest_dir.join("supervisor.lds");
    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rustc-link-arg-bin=supervisor=-T{}", script.display());

    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__WINDOW=0xffffffc000000000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__MODULE_CONTEXT=0xffffffc000210000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__MODULE=0xffffffc000220000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__SCHEDULER=0xffffffc000400000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__CONTEXT=0xffffffc000600000");
}
