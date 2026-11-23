// linuxfuzz: a system-call fuzzer for the Linux compatibility layer, from a
// real Linux program (built static with musl). The kernel self-tests run
// it; `linuxfuzz SEED CHILDREN CALLS` makes a longer or reproducible run
// (`linuxfuzz child SEED CALLS` runs one child alone).
//
// Each child is a process of its own group that calls random Linux system
// call numbers (and WaveOS calls through WAVE_BASE) with structured random
// arguments from a seeded generator — bad and good pointers, boundary
// lengths, bad and recycled descriptors, flags — on three threads, while a
// fourth closes descriptors and unmaps the buffers they use. A child may
// hurt only itself: calls that reach other processes or the machine
// (signals, ptrace and limits of other processes, the clock, mounts,
// reboot, fork and exec) are left out or pointed at the child itself, and
// paths stay inside /tmp/lfz (or are only read). The parent kills a child
// that does not finish in time; a child that cannot be killed, or a
// kernel panic, fails the test.
//
// `linuxfuzz SEED CHILDREN CALLS ORACLE` runs in a sandbox (the self-tests
// start it in one) and also aims path calls, mutating ones included, at
// the folder ORACLE, which it must never read or change: the kernel
// watches it and fails the test if any call gets there.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define WAVE_BASE 0x57410000L
#define SANDBOX "/tmp/lfz"

static uint64_t splitmix(uint64_t *s) {
    uint64_t z = (*s += 0x9E3779B97F4A7C15ull);
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
    return z ^ (z >> 31);
}

typedef struct {
    uint64_t s;
    char *scratch; // path strings: never handed to the kernel to write into
} rng;

static uint64_t rnd(rng *r) { return splitmix(&r->s); }
static uint64_t below(rng *r, uint64_t n) { return n ? rnd(r) % n : 0; }

#define REGIONS 6
#define FDS 24
static _Atomic uintptr_t region[REGIONS];
static _Atomic size_t region_len[REGIONS];
static _Atomic int fds[FDS];
static atomic_int stop;
static pid_t me;

static const uint64_t edges[] = {0, 1, 2, 3, 7, 8, 15, 16, 63, 64, 255, 256, 4095, 4096, 4097, 65535, 65536,
                                 1 << 20, 0x7fffffff, 0x80000000, 0xffffffff, 1ull << 32, 0x7fffffffffffffffull,
                                 0x8000000000000000ull, ~0ull, ~0ull - 1, (uint64_t)-4096};

static uint64_t integer(rng *r) {
    switch (below(r, 4)) {
    case 0: return rnd(r);
    case 1: return below(r, 256);
    default: return edges[below(r, sizeof edges / sizeof *edges)];
    }
}

static uint64_t length(rng *r) {
    switch (below(r, 6)) {
    case 0: case 1: case 2: return below(r, 4200);
    case 3: return below(r, 70000);
    default: return integer(r);
    }
}

static uint64_t buffer(rng *r, uint64_t need) {
    int i = below(r, REGIONS);
    uintptr_t base = region[i];
    size_t len = region_len[i];
    switch (below(r, 18)) {
    case 0: return 0;
    case 1: return 0x1000;
    case 2: return 0xffff800000000000ull + below(r, 1 << 30) * 8;
    case 3: return 0xffffffff80000000ull;
    case 4: return 0x800000000000ull - (need > 4096 ? 2048 : need / 2 + 1);
    case 5: return 0x8000000000000000ull | rnd(r);
    case 6: return (uint64_t)(uintptr_t)&buffer; // read-only code
    case 7: return rnd(r);
    case 8: return base + len - (need > 8192 ? 4096 : need / 2); // into the gap after it
    default: {
        uint64_t room = len > need ? len - need : 0;
        return base + below(r, room + 1);
    }
    }
}

static uint64_t fd(rng *r) {
    switch (below(r, 8)) {
    case 0: return (uint64_t)(int64_t)-100; // AT_FDCWD
    case 1: return below(r, 64);
    case 2: return edges[below(r, sizeof edges / sizeof *edges)];
    default: return (uint64_t)(int64_t)fds[below(r, FDS)];
    }
}

// Paths: inside the sandbox when the call may change something, else also
// a few to read; or a bad pointer. Random names never hold '/' or '.'.
static const char *rw_paths[] = {SANDBOX "/a", SANDBOX "/b", SANDBOX "/dir", SANDBOX "/dir/x", SANDBOX "/s",
                                 "a", "dir/b", SANDBOX "/a/b", SANDBOX};
static const char *ro_paths[] = {"/proc/self/maps", "/proc/self/status", "/dev/null", "/proc/self/fd",
                                 "/proc/self/exe", "/etc/passwd", "", "/proc/self/cmdline", "/dev/zero"};
static const char *cwd_paths[] = {SANDBOX, SANDBOX "/dir", "/proc", "/dev"};

// The folder outside the sandbox to aim at (argv[4]), if any.
static const char *oracle;

static uint64_t path_at(rng *r, int half, int mutating) {
    char *s = r->scratch + half * 8192;
    if (oracle && below(r, 4) == 0) {
        static const char *forms[] = {"%s/secret.txt", "%s", "/tmp/lfz/../../..%s/secret.txt", "%s/new",
                                      "%s/secret.txt/x", "%s/..", "%s//secret.txt/."};
        snprintf(s, 8192, forms[below(r, sizeof forms / sizeof *forms)], oracle);
        return (uint64_t)(uintptr_t)s;
    }
    switch (below(r, 10)) {
    case 0: case 1: case 2: case 3:
        strcpy(s, rw_paths[below(r, sizeof rw_paths / sizeof *rw_paths)]);
        return (uint64_t)(uintptr_t)s;
    case 4: case 5:
        if (!mutating) {
            strcpy(s, ro_paths[below(r, sizeof ro_paths / sizeof *ro_paths)]);
            return (uint64_t)(uintptr_t)s;
        }
        // fall through
    case 6: {
        static const char names[] = "abcdefghijklmnopqrstuvwxyz0123456789 -_~#%+=,;:@\x01\x7f\xc3\xa9\xff";
        size_t n = 1 + below(r, below(r, 4) == 0 ? 5000 : 200);
        strcpy(s, SANDBOX "/");
        size_t at = strlen(s);
        for (size_t i = 0; i < n && at < 8190; i++) s[at++] = names[below(r, sizeof names - 1)];
        s[at] = 0;
        return (uint64_t)(uintptr_t)s;
    }
    case 7: return buffer(r, 16) | 0xffff800000000000ull;
    default:
        strcpy(s, cwd_paths[below(r, sizeof cwd_paths / sizeof *cwd_paths)]);
        return (uint64_t)(uintptr_t)s;
    }
}

// A pid argument: ourselves, our own group, or nobody.
static uint64_t pid_arg(rng *r) {
    switch (below(r, 4)) {
    case 0: return me;
    case 1: return 0;
    case 2: return 0x7ffffff0;
    default: return (uint64_t)(int64_t)-0x7ffffff0;
    }
}

static uint64_t sockaddr(rng *r) {
    static __thread char sa[128];
    memset(sa, 0, sizeof sa);
    switch (below(r, 4)) {
    case 0: // AF_INET 127.0.0.1
        sa[0] = 2;
        sa[2] = below(r, 256);
        sa[3] = below(r, 256);
        sa[4] = 127;
        sa[7] = 1;
        break;
    case 1: // AF_UNIX in the sandbox
        sa[0] = 1;
        strcpy(sa + 2, below(r, 2) ? SANDBOX "/s" : SANDBOX "/t");
        break;
    case 2: // AF_INET6 ::1
        sa[0] = 10;
        sa[3] = below(r, 256);
        sa[23] = 1;
        break;
    default: return buffer(r, 128);
    }
    return (uint64_t)(uintptr_t)sa;
}

// Which arguments are paths, for the calls that take them (x86-64 numbers);
// bit 7: the call changes the filesystem.
struct path_call { int nr; unsigned char args, mutating; };
static const struct path_call path_calls[] = {
    {SYS_open, 1, 2}, {SYS_stat, 1, 0}, {SYS_lstat, 1, 0}, {SYS_access, 1, 0}, {SYS_truncate, 1, 1},
    {SYS_rename, 3, 1}, {SYS_mkdir, 1, 1}, {SYS_rmdir, 1, 1}, {SYS_creat, 1, 1}, {SYS_link, 3, 1},
    {SYS_unlink, 1, 1}, {SYS_symlink, 3, 1}, {SYS_readlink, 1, 0}, {SYS_chmod, 1, 1}, {SYS_chown, 1, 1},
    {SYS_lchown, 1, 1}, {SYS_utime, 1, 1}, {SYS_mknod, 1, 1}, {SYS_statfs, 1, 0}, {SYS_setxattr, 1, 1},
    {SYS_lsetxattr, 1, 1}, {SYS_getxattr, 1, 0}, {SYS_lgetxattr, 1, 0}, {SYS_listxattr, 1, 0},
    {SYS_removexattr, 1, 1}, {SYS_lremovexattr, 1, 1}, {SYS_llistxattr, 1, 0}, {SYS_utimes, 1, 1}, {SYS_inotify_add_watch, 2, 0}, {SYS_openat, 2, 2},
    {SYS_mkdirat, 2, 1}, {SYS_mknodat, 2, 1}, {SYS_fchownat, 2, 1}, {SYS_futimesat, 2, 1},
    {SYS_newfstatat, 2, 0}, {SYS_unlinkat, 2, 1}, {SYS_renameat, 10, 1}, {SYS_linkat, 10, 1},
    {SYS_symlinkat, 5, 1}, {SYS_readlinkat, 2, 0}, {SYS_fchmodat, 2, 1}, {SYS_faccessat, 2, 0},
    {SYS_utimensat, 2, 1}, {SYS_renameat2, 10, 1}, {SYS_statx, 2, 0}, {SYS_chdir, 1, 3},
#ifdef SYS_openat2
    {SYS_openat2, 2, 1},
#endif
#ifdef SYS_faccessat2
    {SYS_faccessat2, 2, 0},
#endif
};

// Calls never made: they reach beyond this child (the machine, other
// processes) or start programs and processes.
static int denied(long n) {
    switch (n) {
    case SYS_reboot: case SYS_sethostname: case SYS_setdomainname: case SYS_settimeofday:
    case SYS_clock_settime: case SYS_adjtimex: case SYS_clock_adjtime: case SYS_mount: case SYS_umount2:
    case SYS_pivot_root: case SYS_chroot: case SYS_swapon: case SYS_swapoff: case SYS_acct:
    case SYS_init_module: case SYS_delete_module: case SYS_finit_module: case SYS_kexec_load:
    case SYS_kexec_file_load: case SYS_iopl: case SYS_ioperm: case SYS_vhangup: case SYS_setsid:
    case SYS_setpgid: case SYS_execve: case SYS_execveat: case SYS_fork: case SYS_vfork: case SYS_clone:
    case SYS_sync: case SYS_quotactl: case SYS_perf_event_open: case SYS_bpf:
#ifdef SYS_clone3
    case SYS_clone3:
#endif
#ifdef SYS_move_mount
    case SYS_open_tree: case SYS_move_mount: case SYS_fsopen: case SYS_fsconfig: case SYS_fsmount:
    case SYS_fspick:
#endif
        return 1;
    }
    return 0;
}

static void one_call(rng *r) {
    long n;
    switch (below(r, 30)) {
    case 0: n = WAVE_BASE + below(r, 100); break;
    case 1: n = 460 + below(r, 600); break;
    default: n = below(r, 460);
    }
    if (denied(n)) return;
    // Rare: calls that end or wreck this child on purpose.
    if ((n == SYS_exit || n == SYS_exit_group || n == SYS_rt_sigreturn || n == SYS_arch_prctl ||
         n == SYS_munmap || n == SYS_mremap || n == SYS_brk || n == SYS_madvise) &&
        below(r, 200) != 0)
        return;
    if (n == WAVE_BASE + 45 && below(r, 50) != 0) return; // notifications: few
    // Signals end or derail a child quickly (all are blocked; see `child`):
    // these calls are made, but not often.
    if ((n == SYS_rt_sigaction || n == SYS_rt_sigprocmask || n == SYS_sigaltstack || n == SYS_kill ||
         n == SYS_tkill || n == SYS_tgkill || n == SYS_rt_sigsuspend || n == SYS_rt_sigqueueinfo ||
         n == SYS_rt_tgsigqueueinfo) &&
        below(r, 20) != 0)
        return;
    uint64_t a[6];
    for (int i = 0; i < 6; i++) {
        switch (below(r, 5)) {
        case 0: a[i] = integer(r); break;
        case 1: a[i] = fd(r); break;
        case 2: a[i] = length(r); break;
        default: a[i] = buffer(r, 4096); break;
        }
    }
    for (size_t k = 0; k < sizeof path_calls / sizeof *path_calls; k++) {
        if (path_calls[k].nr != n) continue;
        int mutating = path_calls[k].mutating;
        if (mutating == 2) {
            // open/openat: by the flags.
            uint64_t flags = n == SYS_open ? a[1] : a[2];
            mutating = (flags & (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC | O_APPEND)) != 0;
        }
        if (mutating == 3) { // chdir
            strcpy(r->scratch, cwd_paths[below(r, sizeof cwd_paths / sizeof *cwd_paths)]);
            a[0] = (uint64_t)(uintptr_t)r->scratch;
            break;
        }
        int half = 0;
        for (int i = 0; i < 6; i++)
            if (path_calls[k].args & (1 << i)) a[i] = path_at(r, half++, mutating);
        break;
    }
    switch (n) {
    // Signals, tracing and limits: only ourselves.
    case SYS_kill: case SYS_rt_sigqueueinfo: case SYS_sched_setattr: case SYS_migrate_pages: case SYS_move_pages:
        a[0] = pid_arg(r);
        break;
    case SYS_tkill: a[0] = below(r, 2) ? (uint64_t)syscall(SYS_gettid) : 0x7ffffff0; break;
    case SYS_tgkill: case SYS_rt_tgsigqueueinfo: a[0] = me; a[1] = below(r, 2) ? (uint64_t)syscall(SYS_gettid) : 0x7ffffff0; break;
    case SYS_ptrace: case SYS_sched_setaffinity: case SYS_sched_setscheduler: case SYS_sched_setparam:
        a[1] = pid_arg(r);
        if (n != SYS_ptrace) a[0] = pid_arg(r);
        break;
    case SYS_process_vm_readv: case SYS_process_vm_writev: case SYS_prlimit64: a[0] = pid_arg(r); break;
    case SYS_setpriority: a[1] = 0; break;
#ifdef SYS_pidfd_open
    case SYS_pidfd_open: a[0] = pid_arg(r); break;
#endif
    case SYS_connect: case SYS_bind: case SYS_sendto:
        if (n == SYS_sendto) a[4] = sockaddr(r);
        else a[1] = sockaddr(r);
        break;
    // Memory: sizes this machine has; changing only our buffers, mostly.
    case SYS_mmap: a[1] = below(r, 4) ? below(r, 64 << 20) : length(r); a[3] &= ~(uint64_t)MAP_POPULATE; break;
    case SYS_mprotect: case SYS_munmap: case SYS_madvise: case SYS_mlock: case SYS_munlock: case SYS_msync:
        if (below(r, 4)) {
            int i = below(r, REGIONS);
            a[0] = region[i];
            a[1] = below(r, region_len[i] + 4097);
        }
        break;
    case SYS_write: case SYS_pwrite64: case SYS_writev: case SYS_pwritev:
        if (a[2] > (1 << 21)) a[2] = 1 << 21;
        break;
    // Waiting: not too long (the child ends its blocked threads anyway).
    case SYS_poll: case SYS_epoll_wait: case SYS_epoll_pwait: a[2] = below(r, 20); break;
    }
    syscall(n, a[0], a[1], a[2], a[3], a[4], a[5]);
}

static void open_some(rng *r) {
    int slot = below(r, FDS), f = -1, pair[2];
    switch (below(r, 9)) {
    case 0: f = open(SANDBOX "/a", O_RDWR | O_CREAT, 0644); break;
    case 1: f = open("/dev/null", O_RDWR); break;
    case 2: f = open(SANDBOX, O_RDONLY | O_DIRECTORY); break;
    case 3:
        if (pipe2(pair, below(r, 2) ? O_NONBLOCK : 0) == 0) {
            int old = atomic_exchange(&fds[below(r, FDS)], pair[1]);
            if (old > 2) close(old);
            f = pair[0];
        }
        break;
    case 4:
        if (socketpair(AF_UNIX, below(r, 2) ? SOCK_STREAM : SOCK_DGRAM, 0, pair) == 0) {
            int old = atomic_exchange(&fds[below(r, FDS)], pair[1]);
            if (old > 2) close(old);
            f = pair[0];
        }
        break;
    case 5: f = eventfd(0, EFD_NONBLOCK); break;
    case 6: f = epoll_create1(0); break;
    case 7: f = socket(below(r, 2) ? AF_INET : AF_INET6, below(r, 2) ? SOCK_STREAM : SOCK_DGRAM, 0); break;
    default: f = memfd_create("lfz", 0); if (f >= 0) ftruncate(f, 8192); break;
    }
    if (f < 0) return;
    int old = atomic_exchange(&fds[slot], f);
    if (old > 2) close(old);
}

static void map_region(int i, rng *r) {
    size_t len = 4096 * (1 + below(r, 8));
    void *p = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) return;
    region_len[i] = len;
    region[i] = (uintptr_t)p;
}

struct worker { uint64_t seed; long calls; };
static atomic_int finished;

static void *chaos(void *arg) {
    rng r = {.s = (uint64_t)(uintptr_t)arg};
    while (!stop) {
        switch (below(&r, 4)) {
        case 0: {
            int i = below(&r, REGIONS);
            uintptr_t a = atomic_exchange(&region[i], 0);
            if (a) munmap((void *)a, region_len[i]);
            map_region(i, &r);
            break;
        }
        case 1: case 2: open_some(&r); break;
        default: sched_yield();
        }
    }
    return NULL;
}

static void *work(void *arg) {
    struct worker *w = arg;
    rng r = {.s = w->seed, .scratch = malloc(16384)};
    if (!r.scratch) return NULL;
    for (long i = 0; i < w->calls && !stop; i++) one_call(&r);
    finished++;
    return NULL;
}

static int child(uint64_t seed, long calls) {
    me = getpid();
    // Our own group, so `kill(0, …)` reaches no one else; quiet stdio.
    setpgid(0, 0);
    int null = open("/dev/null", O_RDWR);
    for (int i = 0; i < 3; i++) dup2(null, i);
    mkdir("/tmp", 0755);
    mkdir(SANDBOX, 0755);
    chdir(SANDBOX);
    rng r = {.s = seed};
    // Every signal blocked (the threads inherit it), so random handlers and
    // kill() rarely end the round at once; faults still do.
    sigset_t all;
    sigfillset(&all);
    sigprocmask(SIG_BLOCK, &all, NULL);
    for (int i = 0; i < FDS; i++) fds[i] = -1;
    for (int i = 0; i < REGIONS; i++) map_region(i, &r);
    for (int i = 0; i < FDS; i++) open_some(&r);
    pthread_t t[4];
    struct worker w[3];
    pthread_create(&t[3], NULL, chaos, (void *)(uintptr_t)rnd(&r));
    for (int i = 0; i < 3; i++) {
        w[i].seed = rnd(&r);
        w[i].calls = calls;
        pthread_create(&t[i], NULL, work, &w[i]);
    }
    // Then end, whatever the threads are doing (blocked in the kernel or
    // not): exit_group must take them all down.
    struct timespec start, now;
    clock_gettime(CLOCK_MONOTONIC, &start);
    do {
        usleep(5000);
        clock_gettime(CLOCK_MONOTONIC, &now);
    } while (finished < 3 && now.tv_sec - start.tv_sec < 2);
    stop = 1;
    _exit(0);
}

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

int main(int argc, char **argv) {
    if (argc >= 4 && strcmp(argv[1], "child") == 0) return child(strtoull(argv[2], NULL, 0), atol(argv[3]));
    uint64_t seed = argc > 1 ? strtoull(argv[1], NULL, 0) : 1;
    int children = argc > 2 ? atoi(argv[2]) : 8;
    long calls = argc > 3 ? atol(argv[3]) : 400;
    if (argc > 4) {
        oracle = argv[4];
        printf("linuxfuzz: in a sandbox, aiming at %s\n", oracle);
    }
    printf("linuxfuzz: seed %#llx, %d children of %ld calls on 3 threads\n", (unsigned long long)seed, children,
           calls);
    fflush(stdout);
    int failed = 0, killed = 0, crashed = 0;
    long t0 = now_ms();
    for (int k = 0; k < children; k++) {
        uint64_t s = seed;
        for (int j = 0; j <= k; j++) splitmix(&s);
        pid_t pid = fork();
        if (pid == 0) _exit(child(s, calls));
        if (pid < 0) {
            printf("FAIL fork for child %d\n", k);
            failed++;
            continue;
        }
        int status = 0;
        long deadline = now_ms() + 20000;
        pid_t got = 0;
        while ((got = waitpid(pid, &status, WNOHANG)) == 0 && now_ms() < deadline) usleep(5000);
        if (got == 0) {
            killed++;
            kill(pid, SIGKILL);
            deadline = now_ms() + 10000;
            while ((got = waitpid(pid, &status, WNOHANG)) == 0 && now_ms() < deadline) usleep(5000);
            if (got == 0) {
                printf("FAIL child %d (seed %#llx) could not be killed\n", k, (unsigned long long)s);
                failed++;
                continue;
            }
        }
        if (WIFSIGNALED(status) && WTERMSIG(status) != SIGKILL) crashed++;
        printf("child %d seed %#llx: %s %d\n", k, (unsigned long long)s, WIFSIGNALED(status) ? "signal" : "exit",
               WIFSIGNALED(status) ? WTERMSIG(status) : WEXITSTATUS(status));
        fflush(stdout);
    }
    // Clean the sandbox up with what still works.
    if (fork() == 0) {
        execl("/bin/sh", "sh", "-c", "rm -rf " SANDBOX, (char *)NULL);
        _exit(127);
    }
    wait(NULL);
    printf("linuxfuzz: %d children in %ld ms (%d killed after 20 s, %d ended by a signal)\n", children, now_ms() - t0,
           killed, crashed);
    if (failed == 0) printf("linuxfuzz passed\n");
    return failed;
}
