// linuxtest: checks the Linux compatibility layer from inside a real Linux
// program (built static with musl). The kernel self-tests run it; it prints
// one line per check and exits 0 when everything passed.

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <time.h>
#include <unistd.h>
#include <waveos.h>

static int failures;

#define CHECK(cond, what)                                                        \
    do {                                                                         \
        if (!(cond)) {                                                           \
            printf("FAIL %s: %s (errno %d)\n", __func__, what, errno);           \
            failures++;                                                          \
            return;                                                              \
        }                                                                        \
    } while (0)

static void start(int argc, char **argv) {
    CHECK(argc >= 1 && strstr(argv[0], "linuxtest"), "argv[0]");
    CHECK(getenv("PATH") && getenv("HOME"), "environment");
    printf("PASS start-up (argc %d, HOME=%s)\n", argc, getenv("HOME"));
}

static void files(void) {
    mkdir("/tmp", 0755);
    unlink("/tmp/lt/a.txt");
    unlink("/tmp/lt/b.txt");
    rmdir("/tmp/lt");
    CHECK(mkdir("/tmp/lt", 0755) == 0, "mkdir");
    CHECK(mkdir("/tmp/lt", 0755) == -1 && errno == EEXIST, "mkdir of a folder that exists");
    // mkdir -p starts at the root.
    CHECK(mkdir("/", 0755) == -1 && errno == EEXIST, "mkdir /");
    FILE *f = fopen("/tmp/lt/a.txt", "w");
    CHECK(f, "fopen for writing");
    for (int i = 0; i < 1000; i++) fprintf(f, "line %d\n", i);
    CHECK(fclose(f) == 0, "fclose");
    struct stat st;
    CHECK(stat("/tmp/lt/a.txt", &st) == 0 && S_ISREG(st.st_mode), "stat");
    CHECK(st.st_size == 8890, "file size");
    CHECK(rename("/tmp/lt/a.txt", "/tmp/lt/b.txt") == 0, "rename");
    int fd = open("/tmp/lt/b.txt", O_RDONLY);
    CHECK(fd >= 0, "open");
    char buf[16] = {0};
    CHECK(pread(fd, buf, 7, 7) == 7 && memcmp(buf, "line 1\n", 7) == 0, "pread");
    CHECK(lseek(fd, -9, SEEK_END) == 8881, "lseek");
    CHECK(read(fd, buf, 9) == 9 && memcmp(buf, "line 999\n", 9) == 0, "read at end");
    // Nobody else holds a lock on it (SQLite asks this before using WAL).
    struct flock lk = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 1};
    CHECK(fcntl(fd, F_GETLK, &lk) == 0 && lk.l_type == F_UNLCK, "F_GETLK finds no lock");
    close(fd);
    DIR *d = opendir("/tmp/lt");
    CHECK(d, "opendir");
    int seen = 0;
    for (struct dirent *e; (e = readdir(d));) seen |= strcmp(e->d_name, "b.txt") == 0;
    closedir(d);
    CHECK(seen, "readdir finds the file");
    CHECK(unlink("/tmp/lt/b.txt") == 0 && rmdir("/tmp/lt") == 0, "unlink and rmdir");
    CHECK(access("/tmp/lt", F_OK) != 0 && errno == ENOENT, "gone");
    char cwd[256];
    CHECK(chdir("/tmp") == 0 && getcwd(cwd, sizeof cwd) && strcmp(cwd, "/tmp") == 0, "chdir and getcwd");
    printf("PASS files and folders\n");
}

static void memory(void) {
    size_t big = 64 << 20;
    char *p = malloc(big);
    CHECK(p, "malloc 64 MiB");
    for (size_t i = 0; i < big; i += 4096) p[i] = (char)i;
    CHECK(p[big - 4096] == (char)(big - 4096), "touch it");
    free(p);
    char *m = mmap(0, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(m != MAP_FAILED, "mmap");
    memset(m, 7, 1 << 20);
    CHECK(mprotect(m, 1 << 20, PROT_READ) == 0, "mprotect");
    CHECK(m[12345] == 7, "read after mprotect");
    CHECK(munmap(m, 1 << 20) == 0, "munmap");
    char *r = mmap(0, 256 << 20, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    CHECK(r != MAP_FAILED, "reserve 256 MiB");
    CHECK(mprotect(r + (1 << 20), 1 << 20, PROT_READ | PROT_WRITE) == 0, "commit part of it");
    r[(1 << 20) + 5] = 1;
    munmap(r, 256 << 20);
    char *q = realloc(malloc(100), 8 << 20);
    CHECK(q, "realloc grows");
    q[(8 << 20) - 1] = 1;
    free(q);
    // Memory comes when touched: 8 GiB can be mapped (WebKit's JavaScript
    // engine asks for that), and pages read as zero again after
    // MADV_DONTNEED.
    size_t huge = (size_t)8 << 30;
    char *h = mmap(0, huge, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    CHECK(h != MAP_FAILED, "map 8 GiB");
    CHECK(h[0] == 0 && h[huge - 1] == 0, "untouched memory reads zero");
    h[4096] = 42;
    h[huge / 2] = 43;
    CHECK(h[4096] == 42 && h[huge / 2] == 43, "touched pages keep what was written");
    CHECK(madvise(h, 2 * 4096, MADV_DONTNEED) == 0 && h[4096] == 0, "MADV_DONTNEED gives zeros back");
    CHECK(munmap(h, huge) == 0, "unmap 8 GiB");
    printf("PASS memory\n");
}

// A process's credentials along with what it sends (SO_PASSCRED and
// SCM_CREDENTIALS), as WebKit's process launcher checks them.
static void credentials(void) {
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) == 0, "socketpair");
    int on = 1;
    CHECK(setsockopt(sv[0], SOL_SOCKET, SO_PASSCRED, &on, sizeof on) == 0, "SO_PASSCRED");
    CHECK(write(sv[1], "x", 1) == 1, "send");
    char byte;
    struct iovec iov = {&byte, 1};
    union {
        struct cmsghdr h;
        char buf[CMSG_SPACE(sizeof(struct ucred))];
    } control;
    struct msghdr msg = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = control.buf, .msg_controllen = sizeof control.buf};
    CHECK(recvmsg(sv[0], &msg, 0) == 1, "recvmsg");
    struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
    CHECK(c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_CREDENTIALS, "credentials come along");
    struct ucred cred;
    memcpy(&cred, CMSG_DATA(c), sizeof cred);
    CHECK(cred.pid == getpid() && cred.uid == getuid(), "the sender's");
    close(sv[0]);
    close(sv[1]);
    printf("PASS credentials\n");
}

// AVX, where the processor has it: the kernel enables it (XSAVE) and keeps
// each thread's YMM registers across switches.
static atomic_int avx_go;

static void *avx_other(void *arg) {
    (void)arg;
    static const unsigned char ones[32] = {[0 ... 31] = 0xEE};
    while (!atomic_load(&avx_go)) sched_yield();
    for (int i = 0; i < 200; i++) {
        __asm__ volatile("vmovdqu %0, %%ymm1" ::"m"(ones) : "xmm1");
        sched_yield();
    }
    return 0;
}

static void avx(void) {
    unsigned a, b, c, d;
    __asm__("cpuid" : "=a"(a), "=b"(b), "=c"(c), "=d"(d) : "a"(1), "c"(0));
    if (!(c & (1u << 28))) {
        printf("PASS avx (none on this processor)\n");
        return;
    }
    CHECK(c & (1u << 27), "the kernel enables XSAVE (OSXSAVE)");
    unsigned lo, hi;
    __asm__ volatile("xgetbv" : "=a"(lo), "=d"(hi) : "c"(0));
    CHECK((lo & 6) == 6, "XCR0 has SSE and AVX");
    unsigned char mine[32], back[32];
    for (int i = 0; i < 32; i++) mine[i] = (unsigned char)(i * 7 + 1);
    pthread_t t;
    CHECK(pthread_create(&t, 0, avx_other, 0) == 0, "thread");
    __asm__ volatile("vmovdqu %0, %%ymm1" ::"m"(mine) : "xmm1");
    atomic_store(&avx_go, 1);
    for (int i = 0; i < 200; i++) sched_yield();
    __asm__ volatile("vmovdqu %%ymm1, %0" : "=m"(back));
    pthread_join(t, 0);
    CHECK(memcmp(mine, back, 32) == 0, "YMM registers survive switches");
    printf("PASS avx\n");
}

static __thread int local = 5;
static atomic_int total;

static void *worker(void *arg) {
    for (int i = 0; i < 1000; i++) {
        local++;
        atomic_fetch_add(&total, 1);
    }
    return (void *)(long)(local + (long)arg);
}

static void threads(void) {
    pthread_t t[4];
    for (long i = 0; i < 4; i++) CHECK(pthread_create(&t[i], 0, worker, (void *)i) == 0, "pthread_create");
    for (long i = 0; i < 4; i++) {
        void *r;
        CHECK(pthread_join(t[i], &r) == 0, "pthread_join");
        CHECK((long)r == 1005 + i, "each thread has its own TLS");
    }
    CHECK(local == 5, "main thread's TLS untouched");
    CHECK(atomic_load(&total) == 4000, "shared counter");
    printf("PASS threads and TLS\n");
}

// Thread ids as glibc's pthread_create asks clone for them: one word, set
// for the new thread and cleared when it exits. The id must be there before
// the thread's first instruction (glibc takes locks with it), and once
// cleared it must stay cleared (pthread_join waits for that).
static _Atomic int tid_word;
static atomic_int ids_seen;

static int quick_thread(void *arg) {
    if (atomic_load(&tid_word) == (int)syscall(SYS_gettid)) atomic_fetch_add(&ids_seen, 1);
    syscall(SYS_exit, 0);
    return 0;
}

static void clone_ids(void) {
    static char stack[64 << 10] __attribute__((aligned(16)));
    const int flags = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
                      CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID;
    for (int i = 0; i < 50; i++) {
        atomic_store(&tid_word, 0);
        int tid = clone(quick_thread, stack + sizeof stack, flags, 0, &tid_word, 0, &tid_word);
        CHECK(tid > 0, "clone");
        int waited = 0;
        while (atomic_load(&tid_word) != 0 && waited++ < 5000) usleep(1000);
        CHECK(atomic_load(&tid_word) == 0, "the exited thread's id stays cleared");
    }
    CHECK(atomic_load(&ids_seen) == 50, "each new thread sees its id at once");
    printf("PASS clone thread ids\n");
}

static void clocks(void) {
    struct timespec a, b;
    CHECK(clock_gettime(CLOCK_MONOTONIC, &a) == 0, "monotonic");
    struct timespec nap = {0, 30 * 1000000};
    CHECK(nanosleep(&nap, 0) == 0, "nanosleep");
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    CHECK(ms >= 25 && ms < 2000, "slept about 30 ms");
    CHECK(time(0) > 1700000000, "wall clock");
    printf("PASS clocks (slept %ld ms)\n", ms);
}

static void machine(void) {
    struct utsname u;
    CHECK(uname(&u) == 0 && strcmp(u.sysname, "Linux") == 0, "uname");
    unsigned char r[16] = {0}, zero[16] = {0};
    int fd = open("/dev/urandom", O_RDONLY);
    CHECK(fd >= 0 && read(fd, r, 16) == 16 && memcmp(r, zero, 16) != 0, "/dev/urandom");
    close(fd);
    char exe[256] = {0};
    CHECK(readlink("/proc/self/exe", exe, sizeof exe - 1) > 0 && strstr(exe, "linuxtest"), "/proc/self/exe");
    CHECK(sysconf(_SC_NPROCESSORS_ONLN) >= 1, "processor count");
    // glibc finds the main thread's stack in /proc/self/maps.
    static char maps[65536];
    fd = open("/proc/self/maps", O_RDONLY);
    ssize_t mn = fd >= 0 ? read(fd, maps, sizeof maps - 1) : -1;
    close(fd);
    int local;
    unsigned long lo = 0, hi = 0, here = (unsigned long)&local;
    char *stack = mn > 0 ? strstr(maps, "[stack]") : NULL;
    if (stack) {
        while (stack > maps && stack[-1] != '\n') stack--;
        sscanf(stack, "%lx-%lx", &lo, &hi);
    }
    CHECK(lo <= here && here < hi && strstr(stack, "rw-p"), "/proc/self/maps shows the stack");
    int fds[2];
    CHECK(pipe(fds) == 0 && write(fds[1], "hi", 2) == 2, "pipe");
    char got[2];
    CHECK(read(fds[0], got, 2) == 2 && got[0] == 'h', "pipe read");
    printf("PASS machine (%s %s)\n", u.sysname, u.release);
}

// WaveOS's own calls: allowed ones work, the rest (here: exit) are refused.
static void wave(void) {
    CHECK(wave_call(WAVE_TIME_MS, 0, 0, 0, 0, 0) > 0, "WaveOS uptime call");
    CHECK(wave_call(0, 3, 0, 0, 0, 0) == -ENOSYS, "WaveOS exit is not reachable");
    struct wave_event e;
    CHECK(wave_next_event(&e, 0) == 0, "no events without a window");
    int log = open("/dev/kmsg", O_WRONLY);
    CHECK(log >= 0 && write(log, "linuxtest: /dev/kmsg\n", 21) == 21, "/dev/kmsg is the kernel log");
    close(log);
    printf("PASS WaveOS calls\n");
}

int main(int argc, char **argv) {
    start(argc, argv);
    files();
    memory();
    threads();
    credentials();
    avx();
    clone_ids();
    clocks();
    machine();
    wave();
    if (failures) {
        printf("linux-test: %d failures\n", failures);
        return 1;
    }
    printf("linux-test passed\n");
    return 0;
}
