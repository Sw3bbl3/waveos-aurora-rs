/* One loadable segment holding the whole file, so the kernel can map the
   image as it is (file offsets equal addresses). */
SECTIONS {
    . = SIZEOF_HEADERS;
    .hash : { *(.hash) } :text
    .gnu.hash : { *(.gnu.hash) }
    .dynsym : { *(.dynsym) }
    .dynstr : { *(.dynstr) }
    .gnu.version : { *(.gnu.version) }
    .gnu.version_d : { *(.gnu.version_d) }
    .gnu.version_r : { *(.gnu.version_r) }
    .dynamic : { *(.dynamic) } :text :dynamic
    .rodata : { *(.rodata*) } :text
    .text : { *(.text*) } :text
    /DISCARD/ : { *(.data*) *(.bss*) *(.eh_frame*) *(.note*) *(.comment) }
}

PHDRS {
    text PT_LOAD FILEHDR PHDRS FLAGS(5);
    dynamic PT_DYNAMIC FLAGS(4);
}

VERSION {
    LINUX_2.6 {
        global:
            __vdso_clock_gettime;
            __vdso_gettimeofday;
            __vdso_time;
            clock_gettime;
            gettimeofday;
            time;
        local: *;
    };
}
