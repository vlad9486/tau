fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    for (bin, script) in [("loader", "loader.lds"), ("supervisor", "supervisor.lds")] {
        let script = manifest_dir.join(script);
        println!("cargo:rerun-if-changed={}", script.display());
        println!("cargo:rustc-link-arg-bin={bin}=-T{}", script.display());
    }
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__WINDOW=0xffffffc000000000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__THREAD=0xffffffc000200000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__MODULE=0xffffffc000210000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__SCHEDULER=0xffffffc000400000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__CONTEXT=0xffffffc000600000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__ALLOCATOR=0xffffffc0006e0000");
}
