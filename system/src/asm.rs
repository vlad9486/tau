use core::{arch, hint};

#[inline(always)]
pub fn fence() {
    unsafe { arch::asm!("fence") };
}

#[inline(always)]
pub fn pause(x: usize) {
    for _ in 0..(x << 20) {
        hint::spin_loop();
    }
}
