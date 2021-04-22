#[link_section = ".exception_vectors.current_el0_synchronous"]
#[export_name = "e_0_c_el0_sync"]
#[naked]
unsafe extern "C" fn e_0_c_el0_sync() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_el0_irq"]
#[export_name = "e_1_c_el0_irq"]
#[naked]
unsafe extern "C" fn e_1_c_el0_irq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_el0_fiq"]
#[export_name = "e_2_c_el0_fiq"]
#[naked]
unsafe extern "C" fn e_2_c_el0_fiq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_el0_serror"]
#[export_name = "e_3_c_el0_serror"]
#[naked]
unsafe extern "C" fn e_3_c_el0_serror() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_elx_synchronous"]
#[export_name = "e_4_c_elx_sync"]
#[naked]
unsafe extern "C" fn e_4_c_elx_sync() -> ! {
    asm! {
        "add x0, x0, #1",
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_elx_irq"]
#[export_name = "e_5_c_elx_irq"]
#[naked]
unsafe extern "C" fn e_5_c_elx_irq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_elx_fiq"]
#[export_name = "e_6_c_elx_fiq"]
#[naked]
unsafe extern "C" fn e_6_c_elx_fiq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.current_elx_serror"]
#[export_name = "e_7_c_elx_serror"]
#[naked]
unsafe extern "C" fn e_7_c_elx_serror() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch64_synchronous"]
#[export_name = "e_8_l_64_sync"]
#[naked]
unsafe extern "C" fn e_8_l_64_sync() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch64_irq"]
#[export_name = "e_9_l_64_irq"]
#[naked]
unsafe extern "C" fn e_9_l_64_irq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch64_fiq"]
#[export_name = "e_a_l_64_fiq"]
#[naked]
unsafe extern "C" fn e_a_l_64_fiq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch64_serror"]
#[export_name = "e_b_l_64_serror"]
#[naked]
unsafe extern "C" fn e_b_l_64_serror() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch32_synchronous"]
#[export_name = "e_c_l_32_sync"]
#[naked]
unsafe extern "C" fn e_c_l_32_sync() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch32_irq"]
#[export_name = "e_d_l_32_irq"]
#[naked]
unsafe extern "C" fn e_d_l_32_irq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch32_fiq"]
#[export_name = "e_e_l_32_fiq"]
#[naked]
unsafe extern "C" fn e_e_l_32_fiq() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}

#[link_section = ".exception_vectors.lower_aarch32_serror"]
#[export_name = "e_f_l_32_serror"]
#[naked]
unsafe extern "C" fn e_f_l_32_serror() -> ! {
    asm! {
        "eret",
        options(noreturn)
    }
}
