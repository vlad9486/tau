fn main() {
    println!("cargo:rustc-link-arg-bin=loader=-T./build/loader.lds");
    println!("cargo:rustc-link-arg-bin=supervisor=-T./build/supervisor.lds");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__WINDOW=0xffffffc000000000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__THREAD=0xffffffc000200000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__MODULE=0xffffffc000210000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__SCHEDULER=0xffffffc000400000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__CONTEXT=0xffffffc000600000");
    println!("cargo:rustc-link-arg-bin=supervisor=--defsym=__ALLOCATOR=0xffffffc0006e0000");
}
