// The vDSO WaveOS maps into every arm64 Linux program: clock_gettime,
// gettimeofday and clock_getres without a system call, read from the
// generic timer's virtual count (CNTVCT_EL0) and the kernel's time page
// (`compat::linux::vdso`), which sits one page below this image; and
// __kernel_rt_sigreturn, where signal handlers return (x30 points at it
// when the program set no SA_RESTORER). Clocks it can't read here (the
// CPU-time clocks, or no usable counter) go to the kernel as usual.
//
// The names and the LINUX_2.6.39 version are arm64 Linux's: glibc and
// musl look them up by both.

typedef unsigned long u64;
typedef long s64;
typedef unsigned int u32;

// Must match `Vvar` in kernel/src/compat/linux/vdso.rs.
struct vvar {
    u32 seq; // odd while the kernel is changing the page
    u32 ok;  // the counter is the clock
    u64 tsc_base;
    u64 mult; // nanoseconds per tick, times 2^32
    s64 offset_ns;
    s64 real_offset_ns; // CLOCK_REALTIME minus CLOCK_MONOTONIC
};

struct ts {
    s64 sec, nsec;
};
struct tv {
    s64 sec, usec;
};

extern const char __ehdr_start[] __attribute__((visibility("hidden")));

static const volatile struct vvar *page(void) {
    return (const volatile struct vvar *)(__ehdr_start - 4096);
}

static inline u64 counter(void) {
    u64 v;
    // The ISB keeps the read from being taken before earlier instructions.
    __asm__ volatile("isb\n\tmrs %0, cntvct_el0" : "=r"(v)::"memory");
    return v;
}

static long sys2(long n, long a, long b) {
    register long x8 __asm__("x8") = n;
    register long x0 __asm__("x0") = a;
    register long x1 __asm__("x1") = b;
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1) : "memory");
    return x0;
}

// arm64's numbers for the calls the fallbacks make.
#define NR_CLOCK_GETTIME 113
#define NR_CLOCK_GETRES 114
#define NR_GETTIMEOFDAY 169

// Nanoseconds on CLOCK_MONOTONIC (or REALTIME); 0 if the kernel must answer.
static int now(u64 *ns, int real) {
    const volatile struct vvar *v = page();
    for (;;) {
        u32 seq = v->seq;
        if (seq & 1) continue;
        __asm__ volatile("dmb ishld" ::: "memory");
        if (!v->ok) return 0;
        u64 d = counter() - v->tsc_base;
        s64 t = (s64)(u64)(((unsigned __int128)d * v->mult) >> 32) + v->offset_ns;
        if (t < 0) t = 0;
        if (real) t += v->real_offset_ns;
        __asm__ volatile("dmb ishld" ::: "memory");
        if (v->seq == seq) {
            *ns = (u64)t;
            return 1;
        }
    }
}

// 1: real time, 0: monotonic, -1: a clock the kernel answers.
static int kind(long clock) {
    switch (clock) {
    case 0: case 5: case 11: // REALTIME, REALTIME_COARSE, TAI
        return 1;
    case 1: case 4: case 6: case 7: // MONOTONIC, _RAW, _COARSE, BOOTTIME
        return 0;
    default:
        return -1;
    }
}

int __kernel_clock_gettime(long clock, struct ts *t) {
    int real = kind(clock);
    u64 ns;
    if (real < 0 || !now(&ns, real)) return (int)sys2(NR_CLOCK_GETTIME, clock, (long)t);
    t->sec = (s64)(ns / 1000000000);
    t->nsec = (s64)(ns % 1000000000);
    return 0;
}

int __kernel_gettimeofday(struct tv *tv, void *tz) {
    u64 ns;
    if (!now(&ns, 1)) return (int)sys2(NR_GETTIMEOFDAY, (long)tv, (long)tz);
    if (tv) {
        tv->sec = (s64)(ns / 1000000000);
        tv->usec = (s64)(ns % 1000000000 / 1000);
    }
    if (tz) ((u32 *)tz)[0] = ((u32 *)tz)[1] = 0;
    return 0;
}

// The kernel's clocks all count in nanoseconds.
int __kernel_clock_getres(long clock, struct ts *res) {
    if (kind(clock) < 0) return (int)sys2(NR_CLOCK_GETRES, clock, (long)res);
    if (res) {
        res->sec = 0;
        res->nsec = 1;
    }
    return 0;
}

// Where a handler returns: rt_sigreturn (139) with sp at the frame. These
// two instructions, exactly, are what unwinders (libgcc, LLVM's
// libunwind) recognise as a signal frame; the CFI says where the
// interrupted registers are, as Linux's does (`sigreturn.S`).
__asm__(
    ".text\n"
    ".globl __kernel_rt_sigreturn\n"
    ".type __kernel_rt_sigreturn, %function\n"
    "    .cfi_startproc\n"
    "    .cfi_signal_frame\n"
    "    .cfi_def_cfa x29, 0\n"
    "    .cfi_offset x29, 0 * 8\n"
    "    .cfi_offset x30, 1 * 8\n"
    "    nop\n"
    "__kernel_rt_sigreturn:\n"
    "    mov x8, #139\n"
    "    svc #0\n"
    "    .cfi_endproc\n"
    ".size __kernel_rt_sigreturn, . - __kernel_rt_sigreturn\n");
