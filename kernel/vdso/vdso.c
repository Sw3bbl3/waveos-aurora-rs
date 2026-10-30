// The vDSO WaveOS maps into every Linux program: clock_gettime,
// gettimeofday and time without a system call, read from the time-stamp
// counter and the kernel's time page (`compat::linux::vdso`), which sits
// one page below this image. Clocks it can't read here (the CPU-time
// clocks, or no usable TSC) go to the kernel as usual.

typedef unsigned long u64;
typedef long s64;
typedef unsigned int u32;

// Must match `Vvar` in kernel/src/compat/linux/vdso.rs.
struct vvar {
    u32 seq; // odd while the kernel is changing the page
    u32 ok;  // the TSC is the clock
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

static inline u64 rdtsc(void) {
    u32 lo, hi;
    __asm__ volatile("lfence\n\trdtsc" : "=a"(lo), "=d"(hi)::"memory");
    return (u64)hi << 32 | lo;
}

static long sys2(long n, long a, long b) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b) : "rcx", "r11", "memory");
    return r;
}

// Nanoseconds on CLOCK_MONOTONIC (or REALTIME); 0 if the kernel must answer.
static int now(u64 *ns, int real) {
    const volatile struct vvar *v = page();
    for (;;) {
        u32 seq = v->seq;
        if (seq & 1) continue;
        __asm__ volatile("" ::: "memory");
        if (!v->ok) return 0;
        u64 d = rdtsc() - v->tsc_base;
        s64 t = (s64)(u64)(((unsigned __int128)d * v->mult) >> 32) + v->offset_ns;
        if (t < 0) t = 0;
        if (real) t += v->real_offset_ns;
        __asm__ volatile("" ::: "memory");
        if (v->seq == seq) {
            *ns = (u64)t;
            return 1;
        }
    }
}

int __vdso_clock_gettime(long clock, struct ts *t) {
    int real;
    switch (clock) {
    case 0: case 5: case 11: // REALTIME, REALTIME_COARSE, TAI
        real = 1;
        break;
    case 1: case 4: case 6: case 7: // MONOTONIC, _RAW, _COARSE, BOOTTIME
        real = 0;
        break;
    default:
        return (int)sys2(228, clock, (long)t);
    }
    u64 ns;
    if (!now(&ns, real)) return (int)sys2(228, clock, (long)t);
    t->sec = (s64)(ns / 1000000000);
    t->nsec = (s64)(ns % 1000000000);
    return 0;
}

int __vdso_gettimeofday(struct tv *tv, void *tz) {
    u64 ns;
    if (!now(&ns, 1)) return (int)sys2(96, (long)tv, (long)tz);
    if (tv) {
        tv->sec = (s64)(ns / 1000000000);
        tv->usec = (s64)(ns % 1000000000 / 1000);
    }
    if (tz) ((u32 *)tz)[0] = ((u32 *)tz)[1] = 0;
    return 0;
}

long __vdso_time(long *t) {
    u64 ns;
    if (!now(&ns, 1)) return sys2(201, (long)t, 0);
    long s = (long)(ns / 1000000000);
    if (t) *t = s;
    return s;
}

int clock_gettime(long, struct ts *) __attribute__((weak, alias("__vdso_clock_gettime")));
int gettimeofday(struct tv *, void *) __attribute__((weak, alias("__vdso_gettimeofday")));
long time(long *) __attribute__((weak, alias("__vdso_time")));
