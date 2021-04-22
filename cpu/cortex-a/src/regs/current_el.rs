use tau_regs::{RegisterRead, RegisterGetField};

pub struct CurrentEL(u64);

impl RegisterRead for CurrentEL {
    #[inline(always)]
    fn get_value() -> Self {
        let reg;
        unsafe {
            asm!("mrs {reg:x}, CurrentEL", reg = out(reg) reg, options(nomem, nostack));
        }
        CurrentEL(reg)
    }
}

pub enum EL {
    _0,
    _1,
    _2,
    _3,
}

impl RegisterGetField<EL> for CurrentEL {
    fn get_field(&self) -> EL {
        match (self.0 & 0xc) >> 2 {
            0x0 => EL::_0,
            0x1 => EL::_1,
            0x2 => EL::_2,
            0x3 => EL::_3,
            _ => unreachable!(),
        }
    }
}
