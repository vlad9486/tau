ENTRY(_start);

SECTIONS
{
    . = 0x80000;

    __START = .;

    .text :
    {
        KEEP(*(.text.boot)) *(.text .text.*)
    }

    .exception_vectors ALIGN(0x800) :
    {
        __EXCEPTION_VECTORS = .;
        KEEP(*(.exception_vectors.current_el0_synchronous))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_el0_irq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_el0_fiq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_el0_serror))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_elx_synchronous))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_elx_irq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_elx_fiq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.current_elx_serror))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch64_synchronous))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch64_irq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch64_fiq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch64_serror))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch32_synchronous))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch32_irq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch32_fiq))
        . = ALIGN(0x80);
        KEEP(*(.exception_vectors.lower_aarch32_serror))
        . = ALIGN(0x80);
    }

    .rodata :
    {
        *(.rodata .rodata.*)
    }

    .data :
    {
        *(.data .data.*)
    }

    .bss ALIGN(8) :
    {
        __BSS_START = .;
        *(.bss .bss.*)
        *(COMMON)
        __BSS_END = .;
    }

    . = ALIGN(0x1000);
    . += 0x1000;

    __END = .;

    /DISCARD/ : { *(.comment) *(.gnu*) *(.note*) *(.eh_frame*) }
}
