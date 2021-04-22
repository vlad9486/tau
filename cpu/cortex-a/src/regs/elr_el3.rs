use tau_regs::{RegisterWrite, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct ELR_EL3(u64);

impl RegisterWrite for ELR_EL3 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr ELR_EL3, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterSetField<unsafe extern "C" fn() -> !> for ELR_EL3 {
    fn set_field(self, field: unsafe extern "C" fn() -> !) -> Self {
        ELR_EL3(field as *const () as u64)
    }
}
