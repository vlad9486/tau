OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY { sram (rwx) : ORIGIN = 0x08000000, LENGTH = 0x00040000 }
SECTIONS {
    .text : { KEEP(*(.text.init)) *(.text .text.*) } > sram
    .rodata : ALIGN(8) { *(.rodata .rodata.*) } > sram
    .data : ALIGN(8) { PROVIDE(__global_pointer$ = . + 0x800); *(.sdata .sdata.* .data .data.*) } > sram
    .bss (NOLOAD) : ALIGN(8) {
        __bss_start = .;
        *(.sbss .sbss.* .bss .bss.*)
        . = ALIGN(8);
        __bss_end = .;
    } > sram
    /DISCARD/ : { *(.eh_frame*) *(.comment*) }
    ASSERT(. <= ORIGIN(sram) + LENGTH(sram), "SPL exceeds SRAM")
}
