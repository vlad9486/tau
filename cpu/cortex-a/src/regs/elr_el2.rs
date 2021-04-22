use tau_regs::{RegisterWrite, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct ELR_EL2(u64);

impl RegisterWrite for ELR_EL2 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr ELR_EL2, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterSetField<unsafe extern "C" fn() -> !> for ELR_EL2 {
    fn set_field(self, field: unsafe extern "C" fn() -> !) -> Self {
        ELR_EL2(field as *const () as u64)
    }
}
