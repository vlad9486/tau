pub fn main(start: u64, end: u64) -> ! {
    use core::fmt::Write as _;
    use tau_cpu::{cortex_a::regs, RegisterWrite, RegisterSetField};
    use tau_hw::State;

    let mut state = State;
    state.init_uart();
    state
        .stdout()
        .write_fmt(format_args!("{:016x}-{:016x}\n", start, end))
        .unwrap();

    extern "C" {
        static __EXCEPTION_VECTORS: core::cell::UnsafeCell<[u8; 0x800]>;
    }

    regs::VBAR_EL1::default().set_field(unsafe { __EXCEPTION_VECTORS.get() }).set_value();
    unsafe { asm!("ISB SY", options(nostack)) }

    let output: u64;
    unsafe {
        asm! {
            "mov x0, #9",
            "svc #0",
            "mov {reg}, x0",
            reg = out(reg) output,
            options(nomem, nostack)
        }
    }
    state
        .stdout()
        .write_fmt(format_args!("{}\n", output))
        .unwrap();

    loop {}
}
