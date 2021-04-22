use tau_regs::{RegisterWrite, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct VBAR_EL1(u64);

impl RegisterWrite for VBAR_EL1 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr VBAR_EL1, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterSetField<*mut [u8; 0x800]> for VBAR_EL1 {
    fn set_field(self, field: *mut [u8; 0x800]) -> Self {
        VBAR_EL1(field as u64)
    }
}
