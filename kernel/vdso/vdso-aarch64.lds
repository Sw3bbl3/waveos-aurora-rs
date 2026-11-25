/* The arm64 vDSO: one loadable segment holding the whole file, so the
   kernel can map the image as it is (file offsets equal addresses). The
   unwind tables stay (unlike x86's): unwinders find their way out of a
   signal handler through __kernel_rt_sigreturn's. */
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
    .eh_frame_hdr : { *(.eh_frame_hdr) } :text :eh_frame_hdr
    .eh_frame : { KEEP(*(.eh_frame)) } :text
    .text : { *(.text*) } :text
    /DISCARD/ : { *(.data*) *(.bss*) *(.note*) *(.comment) }
}

PHDRS {
    text PT_LOAD FILEHDR PHDRS FLAGS(5);
    dynamic PT_DYNAMIC FLAGS(4);
    eh_frame_hdr PT_GNU_EH_FRAME;
}

VERSION {
    LINUX_2.6.39 {
        global:
            __kernel_rt_sigreturn;
            __kernel_gettimeofday;
            __kernel_clock_gettime;
            __kernel_clock_getres;
        local: *;
    };
}
