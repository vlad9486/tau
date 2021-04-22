use tau_regs::RegisterWrite;

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct CNTVOFF_EL2(u64);

impl RegisterWrite for CNTVOFF_EL2 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr CNTVOFF_EL2, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}
