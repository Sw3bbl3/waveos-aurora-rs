/* Aster kernel link layout: higher half, top 2 GiB (-mcmodel=kernel). */
ENTRY(_start)

KERNEL_BASE = 0xFFFFFFFF80000000;

SECTIONS
{
    . = KERNEL_BASE;
    __kernel_start = .;

    .text : ALIGN(4K)
    {
        *(.text._start)
        *(.text .text.*)
    }

    /* Page-aligned boundaries: the kernel maps .text read-only and
       executable, .rodata and .got read-only, the rest no-execute
       (arch::protect). The .got is filled in at link time. */
    . = ALIGN(4K);
    __text_end = .;

    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    }

    .got :
    {
        *(.got .got.*)
    }

    . = ALIGN(4K);
    __rodata_end = .;

    .data : ALIGN(4K)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(4K)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame*)
        *(.note .note.*)
        *(.comment)
    }
}
