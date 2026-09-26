SECTIONS
{
	/* one page for rw data */
	. = __CONTEXT + 0x1000;

	.text :
	{
		KEEP(*(.text.init))
		*(.text .text.*)
		. = ALIGN(0x1000);
	}

	.rodata :
	{
		*(.rodata .rodata.*)
		*(.srodata .srodata.*)
		. = ALIGN(0x1000);
	}

	.data :
	{
		KEEP(*(.data .data.*))
		ASSERT(SIZEOF(.data) == 0, "Error: .data section is not empty");
	}

	.sdata :
	{
		KEEP(*(.sdata .sdata.*))
		ASSERT(SIZEOF(.sdata) == 0, "Error: .sdata section is not empty");
	}

	.bss :
	{
		KEEP(*(.bss .bss.*))
		ASSERT(SIZEOF(.bss) == 0, "Error: .bss section is not empty");
	}

	.sbss :
	{
		KEEP(*(.sbss .sbss.*))
		ASSERT(SIZEOF(.sbss) == 0, "Error: .sbss section is not empty");
	}

    ASSERT(SIZEOF(.text) + SIZEOF(.rodata) <= 0xb000, "Error: Total section size exceeds 44 KiB!")
	/DISCARD/ : { *(.comment) *(.gnu*) *(.note*) *(.eh_frame*) }
}
