use tau_regs::{RegisterWrite, RegisterSetField};

#[allow(non_camel_case_types)]
#[derive(Default)]
pub struct SPSR_EL1(u64);

impl RegisterWrite for SPSR_EL1 {
    #[inline(always)]
    fn set_value(self) {
        unsafe {
            asm!("msr SPSR_EL1, {reg:x}", reg = in(reg) self.0, options(nomem, nostack));
        }
    }
}

impl RegisterSetField<SPSR_EL1_m::M> for SPSR_EL1 {
    fn set_field(self, field: SPSR_EL1_m::M) -> Self {
        SPSR_EL1((self.0 & !0b1111) | (field as u64))
    }
}

impl RegisterSetField<SPSR_EL1_m::D> for SPSR_EL1 {
    fn set_field(self, field: SPSR_EL1_m::D) -> Self {
        if field.0 {
            SPSR_EL1(self.0 | (1 << 9))
        } else {
            SPSR_EL1(self.0 & !(1 << 9))
        }
    }
}

impl RegisterSetField<SPSR_EL1_m::A> for SPSR_EL1 {
    fn set_field(self, field: SPSR_EL1_m::A) -> Self {
        if field.0 {
            SPSR_EL1(self.0 | (1 << 8))
        } else {
            SPSR_EL1(self.0 & !(1 << 8))
        }
    }
}

impl RegisterSetField<SPSR_EL1_m::I> for SPSR_EL1 {
    fn set_field(self, field: SPSR_EL1_m::I) -> Self {
        if field.0 {
            SPSR_EL1(self.0 | (1 << 7))
        } else {
            SPSR_EL1(self.0 & !(1 << 7))
        }
    }
}

impl RegisterSetField<SPSR_EL1_m::F> for SPSR_EL1 {
    fn set_field(self, field: SPSR_EL1_m::F) -> Self {
        if field.0 {
            SPSR_EL1(self.0 | (1 << 6))
        } else {
            SPSR_EL1(self.0 & !(1 << 6))
        }
    }
}

#[allow(non_snake_case)]
pub mod SPSR_EL1_m {
    pub enum M {
        EL0t = 0b0000,
        EL1t = 0b0100,
        EL1h = 0b0101,
    }

    pub struct D(pub bool);

    pub struct A(pub bool);

    pub struct I(pub bool);

    pub struct F(pub bool);
}
