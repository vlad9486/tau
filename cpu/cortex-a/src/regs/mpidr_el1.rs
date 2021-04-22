use tau_regs::RegisterRead;

#[allow(non_camel_case_types)]
pub struct MPIDR_EL1(u64);

impl RegisterRead for MPIDR_EL1 {
    #[inline(always)]
    fn get_value() -> Self {
        let reg;
        unsafe {
            asm!("mrs {reg:x}, MPIDR_EL1", reg = out(reg) reg, options(nomem, nostack));
        }
        MPIDR_EL1(reg)
    }
}

pub enum Aff0 {
    _0,
    _1,
    _2,
    _3,
}

impl MPIDR_EL1 {
    pub fn core(&self) -> Aff0 {
        match self.0 & 0x3 {
            0x0 => Aff0::_0,
            0x1 => Aff0::_1,
            0x2 => Aff0::_2,
            0x3 => Aff0::_3,
            _ => unreachable!(),
        }
    }
}
