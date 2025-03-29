use core::arch;

#[inline(always)]
pub fn fence() {
    unsafe { arch::asm!("fence") };
}
