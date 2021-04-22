use tau_cpu::{
    RegisterRead as _, RegisterWrite as _, RegisterGetField as _, RegisterSetField as _,
    cortex_a::{asm, regs},
};

#[link_section = ".text.boot"]
#[export_name = "_start"]
#[naked]
unsafe extern "C" fn _start() -> ! {
    asm! {
        "mrs x1, MPIDR_EL1",
        "and x1, x1, 0b11",
        "mov x2, 0",
        "cmp x1, x2",
        "b.ne _wfe",
        "b _start_core0",
        options(noreturn)
    }
}

#[export_name = "_start_core0"]
#[naked]
unsafe extern "C" fn _start_core0() -> ! {
    asm! {
        "adrp x0, __END",
        "mov sp, x0",
        "b _start_inner",
        options(noreturn)
    }
}

#[export_name = "_wfe"]
#[naked]
unsafe extern "C" fn _wfe() -> ! {
    asm! {
        "wfe",
        "b _wfe",
        options(noreturn)
    }
}

#[export_name = "_start_inner"]
extern "C" fn _start_inner() -> ! {
    match regs::CurrentEL::get_value().get_field() {
        regs::EL::_0 => panic!("cannot work in user space"),
        regs::EL::_1 => {
            use core::{cell::UnsafeCell, mem, ptr};

            extern "C" {
                static __START: UnsafeCell<u64>;
                static __END: UnsafeCell<u64>;
                static __BSS_START: UnsafeCell<u64>;
                static __BSS_END: UnsafeCell<u64>;
            }

            let (start, end) = unsafe {
                let mut s_bss = __BSS_START.get();
                let e_bss = __BSS_END.get();
                while s_bss < e_bss {
                    // NOTE(volatile) to prevent this from being transformed into intrinsic
                    ptr::write_volatile(s_bss, mem::zeroed());
                    s_bss = s_bss.offset(1);
                }

                (__START.get() as u64, __END.get() as u64)
            };

            super::main::main(start, end)
        },
        regs::EL::_2 => el2_to_el1(),
        regs::EL::_3 => el3_to_el2(),
    }
}

#[inline(always)]
fn el3_to_el2() -> ! {
    // TODO: check if it is necessary
    {
        // Enable timer counter registers for EL1.
        regs::CNTHCTL_EL2::default()
            .set_field(regs::CNTHCTL_EL2_m::EL1PCEN(true))
            .set_field(regs::CNTHCTL_EL2_m::EL1PCTEN(true))
            .set_value();

        // No offset for reading the counters.
        regs::CNTVOFF_EL2::default().set_value();
    }

    regs::HCR_EL2::default()
        .set_field(regs::HCR_EL2_m::RW::EL1IsAarch64)
        .set_value();
    regs::SCR_EL3::default()
        .set_field(0b0101_1011_0001)
        .set_value();
    regs::SPSR_EL3::default()
        .set_field(regs::SPSR_EL3_m::D(true))
        .set_field(regs::SPSR_EL3_m::A(true))
        .set_field(regs::SPSR_EL3_m::I(true))
        .set_field(regs::SPSR_EL3_m::F(true))
        .set_field(regs::SPSR_EL3_m::M::EL2h)
        .set_value();
    regs::ELR_EL3::default().set_field(_start_core0).set_value();

    asm::eret()
}

#[inline(always)]
fn el2_to_el1() -> ! {
    // TODO: check if it is necessary
    {
        // Enable timer counter registers for EL1.
        regs::CNTHCTL_EL2::default()
            .set_field(regs::CNTHCTL_EL2_m::EL1PCEN(true))
            .set_field(regs::CNTHCTL_EL2_m::EL1PCTEN(true))
            .set_value();

        // No offset for reading the counters.
        regs::CNTVOFF_EL2::default().set_value();
    }

    regs::HCR_EL2::default()
        .set_field(regs::HCR_EL2_m::RW::EL1IsAarch64)
        .set_value();
    regs::SPSR_EL2::default()
        .set_field(regs::SPSR_EL2_m::D(true))
        .set_field(regs::SPSR_EL2_m::A(true))
        .set_field(regs::SPSR_EL2_m::I(true))
        .set_field(regs::SPSR_EL2_m::F(true))
        .set_field(regs::SPSR_EL2_m::M::EL1h)
        .set_value();
    regs::ELR_EL2::default().set_field(_start_core0).set_value();

    asm::eret()
}
