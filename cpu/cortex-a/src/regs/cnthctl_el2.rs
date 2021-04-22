use tau_regs::{RegisterRead, RegisterWrite, RegisterGetField, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct CNTHCTL_EL2(u64);

impl RegisterRead for CNTHCTL_EL2 {
    #[inline(always)]
    fn get_value() -> Self {
        let reg;
        unsafe {
            asm!("mrs {reg:x}, CNTHCTL_EL2", reg = out(reg) reg, options(nomem, nostack));
        }
        CNTHCTL_EL2(reg)
    }
}

impl RegisterWrite for CNTHCTL_EL2 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr CNTHCTL_EL2, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterGetField<CNTHCTL_EL2_m::EL1PCTEN> for CNTHCTL_EL2 {
    fn get_field(&self) -> CNTHCTL_EL2_m::EL1PCTEN {
        CNTHCTL_EL2_m::EL1PCTEN(self.0 & (1 << 0) != 0)
    }
}

impl RegisterSetField<CNTHCTL_EL2_m::EL1PCTEN> for CNTHCTL_EL2 {
    fn set_field(self, field: CNTHCTL_EL2_m::EL1PCTEN) -> Self {
        if field.0 {
            CNTHCTL_EL2(self.0 | (1 << 0))
        } else {
            CNTHCTL_EL2(self.0 & !(1 << 0))
        }
    }
}

impl RegisterGetField<CNTHCTL_EL2_m::EL1PCEN> for CNTHCTL_EL2 {
    fn get_field(&self) -> CNTHCTL_EL2_m::EL1PCEN {
        CNTHCTL_EL2_m::EL1PCEN(self.0 & (1 << 1) != 0)
    }
}

impl RegisterSetField<CNTHCTL_EL2_m::EL1PCEN> for CNTHCTL_EL2 {
    fn set_field(self, field: CNTHCTL_EL2_m::EL1PCEN) -> Self {
        if field.0 {
            CNTHCTL_EL2(self.0 | (1 << 1))
        } else {
            CNTHCTL_EL2(self.0 & !(1 << 1))
        }
    }
}

#[allow(non_snake_case)]
pub mod CNTHCTL_EL2_m {
    #[allow(non_camel_case_types)]
    pub struct EL1PCTEN(pub bool);

    #[allow(non_camel_case_types)]
    pub struct EL1PCEN(pub bool);
}
