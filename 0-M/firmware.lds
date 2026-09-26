ENTRY(_start)

SECTIONS
{
	. = __FIRMWARE;

	.text : ALIGN(16)
	{
		KEEP(*(.text.init))
		*(.text .text.*)
	}

	.rodata : ALIGN(16)
	{
		*(.rodata .rodata.*)
		*(.srodata .srodata.*)
	}

	.data : ALIGN(16)
	{
		PROVIDE(__global_pointer$ = . + 0x800);
		*(.data.boot)
		*(.data .data.*)
		*(.sdata .sdata.*)
	}

	.bss (NOLOAD) : ALIGN(16)
	{
		__BSS_START = .;
		*(.bss .bss.*)
		*(.sbss .sbss.*)
		. = ALIGN(16);
		__STACK_BOTTOM = .;
		. += __MAX_HARTS * __STACK_SIZE;
		__STACK_TOP = .;
		__BSS_END = .;
	}

	__FIRMWARE_END = .;
	ASSERT(__FIRMWARE_END <= __DTB, "firmware overlaps its DTB")

	. = __DTB;
	.dtb : ALIGN(16)
	{
		__DTB_START = .;
		KEEP(*(.dtb))
		__DTB_END = .;
	}
	ASSERT(__DTB_END <= __PAYLOAD, "DTB overlaps its payload")

	. = __PAYLOAD;
	.payload : ALIGN(16)
	{
		__PAYLOAD_START = .;
		KEEP(*(.payload))
		__PAYLOAD_END = .;
	}
	ASSERT(__PAYLOAD_END <= __FIRMWARE + __MAX_IMAGE_SIZE,
	       "raw image exceeds configured size limit")

	/DISCARD/ : { *(.comment) *(.gnu*) *(.note*) *(.eh_frame*) }
}
