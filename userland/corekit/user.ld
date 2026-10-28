/* Link layout for WaveOS Aurora user programs. */
ENTRY(_start)

SECTIONS
{
    . = 0x400000;

    .text : ALIGN(4K)
    {
        *(.text._start)
        *(.text .text.*)
    }

    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(4K)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /* Says "built for WaveOS": files without it run as Linux programs. */
    .waveos_note : ALIGN(4)
    {
        KEEP(*(.waveos_note))
    }

    /DISCARD/ :
    {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
