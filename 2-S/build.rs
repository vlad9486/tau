// SPDX-FileCopyrightText: 2025-2026 Vladyslav Melnyk
// SPDX-License-Identifier: GPL-3.0-or-later
// See LICENSE for the full license text.

use std::{collections::BTreeSet, env, fs, iter, path::PathBuf};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Modules {
    module: Vec<Module>,
}

impl Modules {
    pub fn generate(&self) -> String {
        let mut names = BTreeSet::new();
        let mut end = 0;
        let mut generated = String::from("pub const MODULES: &[StaticModule] = &[\n");
        for module in &self.module {
            module.validate(end);
            assert!(names.insert(&module.name), "duplicate module name");
            end = module
                .offset
                .checked_add(module.size)
                .expect("module range overflow");
            assert!(end <= 0x1e0, "modules and boot DTB must fit in 2 MiB");
            let mut name = module.name.clone();
            name.extend(
                iter::repeat(['\\', 'x', '0', '0'])
                    .take(20 - module.name.len())
                    .flatten(),
            );
            let user = !matches!(module.name.as_str(), "loader" | "supervisor");
            generated.push_str("    StaticModule {\n");
            generated.push_str(&format!("        name: *b\"{name}\",\n"));
            generated.push_str(&format!("        offset: {},\n", module.offset));
            generated.push_str(&format!("        pages: {},\n", module.size));
            generated.push_str(&format!("        user: {user},\n"));
            generated.push_str("    },\n");
        }
        generated.push_str("];\n");
        assert!(
            self.module
                .first()
                .is_some_and(|m| m.name == "loader" && m.offset == 0)
        );
        for name in ["loader", "supervisor", "system", "system-test"] {
            let module = self
                .module
                .iter()
                .find(|m| m.name == name)
                .expect("missing boot module");
            let prefix = if name == "system-test" {
                "SYSTEM".to_owned()
            } else {
                name.to_uppercase()
            };
            let cfg = match name {
                "system" => "#[cfg(not(feature = \"system-test\"))]\n",
                "system-test" => "#[cfg(feature = \"system-test\")]\n",
                _ => "",
            };
            generated.push_str(&format!(
                "{cfg}pub const {prefix}_OFFSET: usize = {};\n{cfg}pub const {prefix}_SIZE: usize = {};\n",
                module.offset, module.size,
            ));
        }
        generated.push_str(&format!(
            "pub const DTB_OFFSET: usize = {end};\n\
             pub const DTB_PAGES: usize = 16;\n\
             pub const HEAP_START: usize = {heap_start};\n\
             pub const HEAP_END: usize = {heap_end};\n",
            heap_start = end + 16,
            heap_end = end + 32,
        ));
        generated
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Module {
    name: String,
    offset: usize,
    size: usize,
}

impl Module {
    fn validate(&self, edge: usize) {
        assert!(!self.name.is_empty());
        assert!(self.name.len() <= 20);
        assert!(
            self.name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "invalid module name"
        );
        assert!(self.size > 0, "modules need nonzero page ranges");
        assert!(
            self.offset >= edge,
            "modules need non-overlapping page ranges"
        );
        assert!(
            self.name != "supervisor" || self.size <= 0xdf,
            "supervisor overlaps the allocator's virtual range"
        );
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let config_path = manifest_dir.join("modules.toml");
    println!("cargo:rerun-if-changed={}", config_path.display());

    let config = toml::from_str::<Modules>(&fs::read_to_string(config_path).unwrap()).unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("modules.rs"), config.generate()).unwrap();

    let loader_size = config.module[0].size << 12;
    println!("cargo:rustc-link-arg-bin=loader=--defsym=__LOADER_SIZE={loader_size}");
    for (bin, script) in [("loader", "loader.lds"), ("supervisor", "supervisor.lds")] {
        let script = manifest_dir.join(script);
        println!("cargo:rerun-if-changed={}", script.display());
        println!("cargo:rustc-link-arg-bin={bin}=-T{}", script.display());
    }

    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__WINDOW=0xffffffc000000000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__THREAD=0xffffffc000200000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__MODULE_CONTEXT=0xffffffc000210000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__MODULE=0xffffffc000220000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__SCHEDULER=0xffffffc000400000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__CONTEXT=0xffffffc000600000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__ALLOCATOR=0xffffffc0006e0000");
}
