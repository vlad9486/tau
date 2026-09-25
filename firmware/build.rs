use std::{env, fs, path::PathBuf};

use serde::Deserialize;

const PAYLOAD_ENV: &str = "TAU_SBI_PAYLOAD";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardConfig {
    name: String,
    firmware: u64,
    dtb: u64,
    payload: u64,
    uart: u64,
    clint: u64,
    reset: Option<u64>,
    stack_size: u64,
    max_image_size: u64,
}

#[derive(Deserialize)]
struct Config {
    board: Vec<BoardConfig>,
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let cargo_manifest = manifest_dir.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", cargo_manifest.display());

    let workspace = manifest_dir.parent().unwrap();
    let linker_script = manifest_dir.join("firmware.lds");
    println!("cargo:rerun-if-changed={}", linker_script.display());
    println!("cargo:rerun-if-env-changed={PAYLOAD_ENV}");

    let payload = match env::var_os(PAYLOAD_ENV) {
        Some(path) => {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            };
            let path = path.canonicalize().unwrap_or_else(|error| {
                panic!("cannot open {} from {PAYLOAD_ENV}: {error}", path.display())
            });
            assert!(path.is_file(), "{} must name a file", path.display());
            println!("cargo:rerun-if-changed={}", path.display());
            path
        }
        None => {
            let path = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("empty-payload.bin");
            fs::write(&path, []).unwrap();
            println!("cargo:warning={PAYLOAD_ENV} is not set; embedding an empty payload");
            path
        }
    };
    println!("cargo:rustc-env=TAU_SBI_PAYLOAD_PATH={}", payload.display());

    let config_path = manifest_dir.join("config.toml");
    let source = fs::read_to_string(manifest_dir.join("config.toml")).unwrap();
    let Config { board } = toml::from_str(&source).unwrap();
    println!("cargo:rerun-if-changed={}", config_path.display());

    for config in board {
        let bin = format!("tau-{}", config.name);
        assert!(
            config.firmware < config.dtb && config.dtb < config.payload,
            "{}: image addresses must increase",
            config_path.display()
        );
        assert!(
            config.stack_size > 0,
            "{}: stack size must be positive",
            config_path.display()
        );
        assert!(
            config.max_image_size > config.payload - config.firmware,
            "{}: payload lies outside image limit",
            config_path.display()
        );
        config
            .firmware
            .checked_add(config.max_image_size)
            .expect("image limit overflows address space");

        println!(
            "cargo:rustc-link-arg-bin={bin}=-T{}",
            linker_script.display()
        );
        for (value, symbol) in [
            (config.firmware, "__FIRMWARE"),
            (config.dtb, "__DTB"),
            (config.payload, "__PAYLOAD"),
            (config.uart, "__UART"),
            (config.clint, "__CLINT"),
            (config.stack_size, "__STACK_SIZE"),
            (config.max_image_size, "__MAX_IMAGE_SIZE"),
        ] {
            println!("cargo:rustc-link-arg-bin={bin}=--defsym={symbol}=0x{value:x}");
        }
        if let Some(reset) = config.reset {
            println!("cargo:rustc-link-arg-bin={bin}=--defsym=__RESET=0x{reset:x}");
        }
    }
}
