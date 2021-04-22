use tau_regs::{RegisterRead, RegisterWrite, RegisterGetField, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct HCR_EL2(u64);

impl RegisterRead for HCR_EL2 {
    #[inline(always)]
    fn get_value() -> Self {
        let reg;
        unsafe {
            asm!("mrs {reg:x}, HCR_EL2", reg = out(reg) reg, options(nomem, nostack));
        }
        HCR_EL2(reg)
    }
}

impl RegisterWrite for HCR_EL2 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr HCR_EL2, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterGetField<self::HCR_EL2_m::RW> for HCR_EL2 {
    fn get_field(&self) -> self::HCR_EL2_m::RW {
        match (self.0 >> 31) & 0x1 {
            0 => self::HCR_EL2_m::RW::AllLowerELsAreAarch32,
            1 => self::HCR_EL2_m::RW::EL1IsAarch64,
            _ => unreachable!(),
        }
    }
}

impl RegisterSetField<self::HCR_EL2_m::RW> for HCR_EL2 {
    fn set_field(self, field: self::HCR_EL2_m::RW) -> Self {
        match field {
            self::HCR_EL2_m::RW::AllLowerELsAreAarch32 => HCR_EL2(self.0 & !(1 << 31)),
            self::HCR_EL2_m::RW::EL1IsAarch64 => HCR_EL2(self.0 | (1 << 31)),
        }
    }
}

#[allow(non_snake_case)]
pub mod HCR_EL2_m {
    #[allow(non_camel_case_types)]
    pub enum RW {
        AllLowerELsAreAarch32,
        EL1IsAarch64,
    }
}
