use super::{
    scheduler::{Shared, DriverState},
    register::Register,
};

pub struct State {
    reg: &'static Reg,
}

#[repr(C, align(0x1000))]
struct Reg {
    _h0: [Register<u32, u32>; 0x4000],
}

impl State {
    pub fn new(config: tau::DtbProps<'_>) -> Option<Self> {
        let area = config.find_reg()?;
        let reg = area.r::<Reg>();

        reg._h0[0xc4d].write((1u32 << 15) | (1 << 14) | (1 << 6) | (1 << 0));

        Some(State { reg })
    }
}

impl DriverState for State {
    fn handle(&mut self, shared: &mut Shared, event: tau::Event) {
        match event {
            tau::Event::Timeout => {
                shared.write(format_args!("here"));
            }
            tau::Event::Interrupt { id } => {
                let _ = &self.reg;
                shared.write(format_args!("interrupt {id}"));
            }
            _ => {}
        }
    }
}
