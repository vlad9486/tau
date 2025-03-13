SECTIONS
{
	__ST = .;

	.text :
	{
		KEEP(*(.text.init))
		*(.text .text.*)
		. = ALIGN(0x1000);
	}

	__RO = .;

	.rodata :
	{
		*(.rodata .rodata.*)
		*(.srodata .srodata.*)
		. = ALIGN(0x1000);
	}

	__HP = .;

	.got :
	{
		KEEP(*(.got .got.*))
		ASSERT(SIZEOF(.got) == 0, "Error: .got section is not empty");
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

    ASSERT(SIZEOF(.text) + SIZEOF(.rodata) <= 0x4000, "Error: Total section size exceeds 16 KiB!")
	/DISCARD/ : { *(.comment) *(.gnu*) *(.note*) *(.eh_frame*) }
}
