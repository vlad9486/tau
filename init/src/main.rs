#![no_std]
#![no_main]
#![feature(asm)]
#![feature(naked_functions)]

#[cfg(any(feature = "rpi3",))]
mod aarch64;

#[panic_handler]
fn handler(info: &core::panic::PanicInfo) -> ! {
    if cfg!(feature = "rpi3") {
        use core::fmt::Write as _;

        let mut state = tau_hw::State;
        state.init_uart();
        let _ = state.stdout().write_fmt(format_args!("{:?}\n", info));
    }

    loop {}
}
