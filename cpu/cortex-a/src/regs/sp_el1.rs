use tau_regs::{RegisterWrite, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct SP_EL1(u64);

impl RegisterWrite for SP_EL1 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr SP_EL1, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterSetField<*mut u64> for SP_EL1 {
    fn set_field(self, field: *mut u64) -> Self {
        SP_EL1(field as _)
    }
}
