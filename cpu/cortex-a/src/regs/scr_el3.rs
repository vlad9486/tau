use tau_regs::{RegisterWrite, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct SCR_EL3(u64);

impl RegisterWrite for SCR_EL3 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr SCR_EL3, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterSetField<u64> for SCR_EL3 {
    fn set_field(self, field: u64) -> Self {
        SCR_EL3(field)
    }
}
