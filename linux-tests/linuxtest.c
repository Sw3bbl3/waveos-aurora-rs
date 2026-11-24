// linuxtest: checks the Linux compatibility layer from inside a real Linux
// program (built static with musl). The kernel self-tests run it; it prints
// one line per check and exits 0 when everything passed.

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysinfo.h>
#include <sys/timerfd.h>
#include <sys/utsname.h>
#include <sys/wait.h>
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

// The auxiliary vector: /proc/self/auxv holds what the stack had, and
// AT_PLATFORM names this architecture (arm64 also gets AT_MINSIGSTKSZ).
static void auxv(void) {
    unsigned long words[128];
    int fd = open("/proc/self/auxv", O_RDONLY);
    CHECK(fd >= 0, "open /proc/self/auxv");
    long n = read(fd, words, sizeof words);
    close(fd);
    CHECK(n >= 16 && n % 16 == 0 && words[n / 8 - 2] == AT_NULL, "/proc/self/auxv ends with AT_NULL");
    int seen = 0;
    for (long i = 0; i < n / 8; i += 2) {
        if (words[i] == AT_PAGESZ || words[i] == AT_RANDOM || words[i] == AT_HWCAP) {
            CHECK(words[i + 1] == getauxval(words[i]), "/proc/self/auxv agrees with the stack");
            seen++;
        }
    }
    CHECK(seen == 3, "AT_PAGESZ, AT_RANDOM and AT_HWCAP in /proc/self/auxv");
    const char *platform = (const char *)getauxval(AT_PLATFORM);
#if defined(__aarch64__)
    CHECK(platform && strcmp(platform, "aarch64") == 0, "AT_PLATFORM");
    CHECK(getauxval(51) >= 4720, "AT_MINSIGSTKSZ");
#else
    CHECK(platform && strcmp(platform, "x86_64") == 0, "AT_PLATFORM");
#endif
    printf("PASS auxiliary vector (%ld entries, platform %s)\n", n / 16, platform);
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

// A file removed while open stays readable and writable through its
// descriptor, and nothing new gets its inode meanwhile (on AuroraFS a new
// file or folder used to be given it, and a write or ftruncate through the
// old descriptor then landed in that, corrupting a folder).
static void removed_while_open(void) {
    mkdir("/tmp", 0755);
    mkdir("/tmp/lto", 0755);
    int fd = open("/tmp/lto/gone", O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "create");
    CHECK(write(fd, "kept after unlink", 17) == 17, "write");
    struct stat before;
    CHECK(fstat(fd, &before) == 0, "fstat");
    CHECK(unlink("/tmp/lto/gone") == 0, "unlink while open");
    CHECK(access("/tmp/lto/gone", F_OK) != 0 && errno == ENOENT, "the name is gone");
    // New files and folders take other inodes.
    for (int i = 0; i < 8; i++) {
        char p[64];
        snprintf(p, sizeof p, "/tmp/lto/d%d", i);
        CHECK(mkdir(p, 0755) == 0, "mkdir after unlink");
        struct stat st;
        CHECK(stat(p, &st) == 0 && st.st_ino != before.st_ino, "a new folder has its own inode");
    }
    char buf[32] = {0};
    CHECK(pread(fd, buf, 17, 0) == 17 && memcmp(buf, "kept after unlink", 17) == 0, "read after unlink");
    CHECK(pwrite(fd, "more", 4, 17) == 4, "write after unlink");
    CHECK(ftruncate(fd, 5) == 0, "ftruncate after unlink");
    struct stat after;
    CHECK(fstat(fd, &after) == 0 && after.st_size == 5, "size after ftruncate");
    CHECK(fsync(fd) == 0 && fdatasync(fd) == 0, "fsync and fdatasync");
    CHECK(close(fd) == 0, "close");
    for (int i = 0; i < 8; i++) {
        char p[64];
        snprintf(p, sizeof p, "/tmp/lto/d%d", i);
        DIR *d = opendir(p);
        CHECK(d, "the folders are intact");
        closedir(d);
        CHECK(rmdir(p) == 0, "rmdir");
    }
    // A folder that can't be read says so, rather than looking empty (rm -rf
    // used to report "Directory not empty" for it). Root may read anything.
    if (geteuid() != 0) {
        CHECK(mkdir("/tmp/lto/locked", 0755) == 0 && mkdir("/tmp/lto/locked/inside", 0755) == 0, "mkdir locked");
        CHECK(chmod("/tmp/lto/locked", 0300) == 0, "chmod 0300");
        DIR *d = opendir("/tmp/lto/locked");
        int refused = !d && errno == EACCES;
        if (d) {
            errno = 0;
            struct dirent *e = readdir(d);
            refused = !e && errno == EACCES;
            closedir(d);
        }
        CHECK(chmod("/tmp/lto/locked", 0755) == 0, "chmod back");
        CHECK(refused, "listing an unreadable folder fails with EACCES");
        CHECK(rmdir("/tmp/lto/locked/inside") == 0 && rmdir("/tmp/lto/locked") == 0, "remove locked");
    }
    // A folder can't go inside itself.
    CHECK(mkdir("/tmp/lto/a", 0755) == 0 && mkdir("/tmp/lto/a/b", 0755) == 0, "mkdir a/b");
    CHECK(rename("/tmp/lto/a", "/tmp/lto/a/b/a") == -1 && errno == EINVAL, "rename into itself");
    CHECK(rmdir("/tmp/lto/a/b") == 0 && rmdir("/tmp/lto/a") == 0 && rmdir("/tmp/lto") == 0, "clean up");
    printf("PASS files removed while open, fsync\n");
}

// A child writes to shared memory `w`: does the parent see it?
static int fork_and_wait_sees(unsigned char *w) {
    w[0] = 1;
    pid_t pid = fork();
    if (pid == 0) {
        w[0] = 99;
        _exit(0);
    }
    int status;
    return pid > 0 && waitpid(pid, &status, 0) == pid && w[0] == 99;
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
    // mincore: mapped pages answer, unmapped ones are ENOMEM (EGL tells a
    // Wayland display from other pointers this way).
    unsigned char vec[4];
    char *mc = mmap(0, 4 * 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(mc != MAP_FAILED && mincore(mc, 4 * 4096, vec) == 0, "mincore of mapped pages");
    CHECK(munmap(mc + 2 * 4096, 2 * 4096) == 0, "unmap half");
    errno = 0;
    CHECK(mincore(mc, 4 * 4096, vec) == -1 && errno == ENOMEM, "mincore over unmapped pages");
    munmap(mc, 2 * 4096);
    // Code written through one shared mapping of a deleted file and run
    // through another (GStreamer's ORC makes its code this way).
    mkdir("/tmp", 0755);
    char tmpl[] = "/tmp/orcexec.XXXXXX";
    int fd = mkstemp(tmpl);
    CHECK(fd >= 0 && unlink(tmpl) == 0 && ftruncate(fd, 65536) == 0, "a deleted 64 KiB file");
    unsigned char *x = mmap(0, 65536, PROT_READ | PROT_EXEC, MAP_SHARED, fd, 0);
    unsigned char *w = mmap(0, 65536, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(x != MAP_FAILED && w != MAP_FAILED, "map it executable and writable");
#if defined(__aarch64__)
    static const unsigned char code[] = {0x40, 0x05, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6}; // mov w0, #42; ret
#else
    static const unsigned char code[] = {0xb8, 42, 0, 0, 0, 0xc3}; // mov eax, 42; ret
#endif
    memcpy(w + 8192, code, sizeof code);
#if defined(__aarch64__)
    // What __clear_cache does (no compiler runtime here): clean the written
    // line to where instructions are fetched from, drop the stale one.
    __asm__ volatile("dc cvau, %0\n dsb ish\n ic ivau, %1\n dsb ish\n isb" ::"r"(w + 8192), "r"(x + 8192) : "memory");
#endif
    CHECK(memcmp(x + 8192, code, sizeof code) == 0, "what one mapping writes, the other sees");
    CHECK(((int (*)(void))(x + 8192))() == 42, "and runs");
    CHECK(fork_and_wait_sees(w), "a child writes through the mapping it inherited");
    munmap(x, 65536);
    munmap(w, 65536);
    close(fd);
    printf("PASS memory\n");
}

#ifndef MADV_COLD
#define MADV_COLD 20
#endif
#ifndef MADV_PAGEOUT
#define MADV_PAGEOUT 21
#endif

// A field of /proc/self/status, in kB (-1 if missing).
static long status_kb(const char *field) {
    static char buf[4096];
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    size_t n = fread(buf, 1, sizeof buf - 1, f);
    fclose(f);
    buf[n] = 0;
    char *at = strstr(buf, field);
    return at ? strtol(at + strlen(field), NULL, 10) : -1;
}

static unsigned char byte_at(size_t i, unsigned seed) {
    return (unsigned char)((i * 2654435761u + seed) >> 13);
}

static int same(const unsigned char *p, size_t len, unsigned seed, size_t flipped) {
    for (size_t i = 0; i < len; i++) {
        unsigned char want = byte_at(i, seed);
        if (i < flipped) want ^= 0xFF;
        if (p[i] != want) return 0;
    }
    return 1;
}

// Swapping as Linux programs see it: the figures in /proc and sysinfo,
// MADV_PAGEOUT (pages out now, back on touch, VmSwap counting them),
// mlock (locked pages stay), fork (each side keeps its own copy of what
// the other writes after) and shared memory (a write seen across fork),
// all with the pages swapped out in between.
static void swapping(void) {
    static char text[4096];
    FILE *f = fopen("/proc/meminfo", "r");
    CHECK(f, "/proc/meminfo");
    text[fread(text, 1, sizeof text - 1, f)] = 0;
    fclose(f);
    CHECK(strstr(text, "MemAvailable:") && strstr(text, "SwapTotal:") && strstr(text, "SwapFree:") &&
              strstr(text, "Zswap:"),
          "/proc/meminfo has memory, swap and compressed figures");
    f = fopen("/proc/pressure/memory", "r");
    CHECK(f, "/proc/pressure/memory");
    text[fread(text, 1, sizeof text - 1, f)] = 0;
    fclose(f);
    CHECK(strncmp(text, "some avg10=", 11) == 0 && strstr(text, "\nfull avg10="), "/proc/pressure/memory");
    struct sysinfo si;
    CHECK(sysinfo(&si) == 0 && si.totalram > 0 && si.totalswap > 0 && si.freeswap <= si.totalswap, "sysinfo swap");

    size_t len = 256 * 4096;
    unsigned char *p = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(p != MAP_FAILED, "mmap");
    for (size_t i = 0; i < len; i++) p[i] = byte_at(i, 1);
    long s0 = status_kb("VmSwap:");
    CHECK(s0 >= 0 && status_kb("VmRSS:") >= 1024, "/proc/self/status: VmSwap and VmRSS");
    CHECK(madvise(p, len, MADV_PAGEOUT) == 0, "MADV_PAGEOUT");
    long s1 = status_kb("VmSwap:");
    CHECK(s1 >= s0 + 1024, "VmSwap counts the pages swapped out");
    CHECK(same(p, len, 1, 0), "swapped-out pages come back as they were");
    CHECK(status_kb("VmSwap:") <= s1 - 1024, "touched pages are in memory again");

    CHECK(mlock(p, 64 * 4096) == 0 && status_kb("VmLck:") >= 256, "mlock (VmLck)");
    s0 = status_kb("VmSwap:");
    CHECK(madvise(p, len, MADV_PAGEOUT) == 0, "MADV_PAGEOUT with some pages locked");
    s1 = status_kb("VmSwap:");
    CHECK(s1 - s0 >= 192 * 4 && s1 - s0 <= 192 * 4 + 64, "locked pages are not swapped out");
    static unsigned char core[256];
    CHECK(mincore(p, len, core) == 0, "mincore");
    int in_memory = 0;
    for (int i = 64; i < 256; i++) in_memory += core[i] & 1;
    CHECK((core[0] & 1) && (core[63] & 1) && in_memory < 64, "mincore: locked pages resident, swapped-out ones not");
    CHECK(munlock(p, 64 * 4096) == 0 && status_kb("VmLck:") == 0, "munlock");
    CHECK(same(p, len, 1, 0), "after mlock and MADV_PAGEOUT");
    CHECK(madvise(p, len, MADV_COLD) == 0 && madvise(p, len, MADV_WILLNEED) == 0, "MADV_COLD and MADV_WILLNEED");

    // fork with the pages out: the child writes half and swaps again; the
    // parent's pages stay as they were.
    CHECK(madvise(p, len, MADV_PAGEOUT) == 0, "MADV_PAGEOUT before fork");
    pid_t c = fork();
    CHECK(c >= 0, "fork");
    if (c == 0) {
        int ok = same(p, len, 1, 0);
        for (size_t i = 0; i < len / 2; i++) p[i] ^= 0xFF;
        madvise(p, len, MADV_PAGEOUT);
        _exit(ok && same(p, len, 1, len / 2) ? 0 : 1);
    }
    int st = -1;
    CHECK(waitpid(c, &st, 0) == c && WIFEXITED(st) && WEXITSTATUS(st) == 0, "the child's copy, swapped");
    CHECK(madvise(p, len, MADV_PAGEOUT) == 0 && same(p, len, 1, 0), "the parent's copy, swapped");
    munmap(p, len);

    // Shared memory: what the child writes, the parent reads, swapped out
    // by both in between.
    unsigned char *s = mmap(0, len, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(s != MAP_FAILED, "mmap shared");
    for (size_t i = 0; i < len; i++) s[i] = byte_at(i, 2);
    CHECK(madvise(s, len, MADV_PAGEOUT) == 0, "MADV_PAGEOUT shared");
    c = fork();
    CHECK(c >= 0, "fork (shared)");
    if (c == 0) {
        int ok = same(s, len, 2, 0);
        for (size_t i = 0; i < len / 4; i++) s[i] ^= 0xFF;
        madvise(s, len, MADV_PAGEOUT);
        _exit(ok ? 0 : 1);
    }
    CHECK(waitpid(c, &st, 0) == c && WIFEXITED(st) && WEXITSTATUS(st) == 0, "the child read shared memory");
    CHECK(same(s, len, 2, len / 4), "the parent reads what the child wrote to shared memory");
    munmap(s, len);
    printf("PASS swapping (MADV_PAGEOUT, mlock, fork, shared memory, /proc figures)\n");
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

#if !defined(__aarch64__)
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
#endif

#if defined(__aarch64__)
// On Arm the same check for the NEON registers (always there): each
// thread's v1 survives switches.
static void *neon_other(void *arg) {
    (void)arg;
    static const unsigned char ones[16] = {[0 ... 15] = 0xEE};
    while (!atomic_load(&avx_go)) sched_yield();
    for (int i = 0; i < 200; i++) {
        __asm__ volatile("ld1 {v1.16b}, [%0]" ::"r"(ones) : "v1", "memory");
        sched_yield();
    }
    return 0;
}

static void avx(void) {
    unsigned char mine[16], back[16];
    for (int i = 0; i < 16; i++) mine[i] = (unsigned char)(i * 7 + 1);
    pthread_t t;
    CHECK(pthread_create(&t, 0, neon_other, 0) == 0, "thread");
    __asm__ volatile("ld1 {v1.16b}, [%0]" ::"r"(mine) : "v1", "memory");
    atomic_store(&avx_go, 1);
    for (int i = 0; i < 200; i++) sched_yield();
    __asm__ volatile("st1 {v1.16b}, [%0]" ::"r"(back) : "memory");
    pthread_join(t, 0);
    CHECK(memcmp(mine, back, 16) == 0, "NEON registers survive switches");
    printf("PASS avx (NEON on this processor)\n");
}
#else
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
#endif

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
    // The vDSO answers without a system call, and agrees with the kernel:
    // its time always lies between two of the kernel's taken around it,
    // for both clocks, again and again until real time has crossed a whole
    // second (where a clock pieced together from seconds and nanoseconds
    // read apart would jump).
    CHECK(getauxval(AT_SYSINFO_EHDR) != 0, "a vDSO");
    struct timespec k1, v, k2, first;
    syscall(SYS_clock_gettime, CLOCK_REALTIME, &first);
    long rounds = 0, real_sec = first.tv_sec;
    // (A bound on rounds only in case the clock stood still: under hardware
    // virtualization a million rounds take well under two seconds.)
    while (real_sec < first.tv_sec + 2 && rounds < 100000000) {
        for (int clock = CLOCK_REALTIME; clock <= CLOCK_MONOTONIC; clock++) {
            syscall(SYS_clock_gettime, clock, &k1);
            clock_gettime(clock, &v);
            syscall(SYS_clock_gettime, clock, &k2);
            long long n1 = k1.tv_sec * 1000000000LL + k1.tv_nsec, nv = v.tv_sec * 1000000000LL + v.tv_nsec,
                      n2 = k2.tv_sec * 1000000000LL + k2.tv_nsec;
            if (!(n1 <= nv && nv <= n2)) {
                printf("    clock %d, check %ld: kernel %lld, vDSO %lld, kernel %lld\n", clock, rounds, n1, nv, n2);
            }
            CHECK(n1 <= nv && nv <= n2, "the vDSO's time lies between the kernel's");
            if (clock == CLOCK_REALTIME) real_sec = k2.tv_sec;
        }
        rounds++;
    }
    CHECK(time(0) >= first.tv_sec + 2, "real time moved on through a whole second");
    printf("PASS clocks (slept %ld ms; vDSO checked against the kernel %ld times over a second)\n", ms, rounds);
}

// From <linux/futex.h> (musl ships no kernel headers).
#define FUTEX_WAIT_PRIVATE 128
#define FUTEX_WAKE_PRIVATE 129
#define FUTEX_CMP_REQUEUE_PRIVATE 132
#define FUTEX_WAKE_OP_PRIVATE 133
#define FUTEX_OP(op, oparg, cmp, cmparg) (((op & 0xf) << 28) | ((cmp & 0xf) << 24) | ((oparg & 0xfff) << 12) | (cmparg & 0xfff))
#define FUTEX_OP_SET 0
#define FUTEX_OP_CMP_EQ 0

static atomic_int futex_a, futex_b, futex_ready;

static void *futex_waiter(void *arg) {
    (void)arg;
    atomic_fetch_add(&futex_ready, 1);
    while (syscall(SYS_futex, &futex_a, FUTEX_WAIT_PRIVATE, 0, NULL) != 0 && errno == EINTR) {
    }
    return NULL;
}

static void futexes(void) {
    pthread_t t[2];
    for (int i = 0; i < 2; i++) CHECK(pthread_create(&t[i], NULL, futex_waiter, NULL) == 0, "pthread_create");
    while (futex_ready < 2) usleep(1000);
    // Moves both waiters from A to B (waking none), then wakes them on B.
    long moved = 0;
    for (int i = 0; i < 200 && moved < 2; i++) {
        usleep(5000);
        moved += syscall(SYS_futex, &futex_a, FUTEX_CMP_REQUEUE_PRIVATE, 0, (void *)2L, &futex_b, 0);
    }
    CHECK(moved == 2, "FUTEX_CMP_REQUEUE moves the waiters");
    CHECK(syscall(SYS_futex, &futex_a, FUTEX_WAKE_PRIVATE, 2, NULL) == 0, "none left on A");
    CHECK(syscall(SYS_futex, &futex_b, FUTEX_WAKE_PRIVATE, 2, NULL) == 2, "both woken on B");
    for (int i = 0; i < 2; i++) pthread_join(t[i], NULL);
    CHECK(syscall(SYS_futex, &futex_a, FUTEX_CMP_REQUEUE_PRIVATE, 0, (void *)1L, &futex_b, 7) == -1 &&
              errno == EAGAIN,
          "FUTEX_CMP_REQUEUE checks the value");
    // WAKE_OP: B = 5 (it was 0, so the comparison "== 0" holds).
    int op = FUTEX_OP(FUTEX_OP_SET, 5, FUTEX_OP_CMP_EQ, 0);
    CHECK(syscall(SYS_futex, &futex_a, FUTEX_WAKE_OP_PRIVATE, 1, (void *)1L, &futex_b, op) == 0 && futex_b == 5,
          "FUTEX_WAKE_OP");
    printf("PASS futexes\n");
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
    // The processors as /proc and /sys describe them (cpuid's flags).
    char *info = malloc(1 << 16);
    fd = open("/proc/cpuinfo", O_RDONLY);
    ssize_t in = fd >= 0 ? read(fd, info, (1 << 16) - 1) : -1;
    close(fd);
    CHECK(in > 0, "/proc/cpuinfo");
    info[in] = 0;
#if defined(__aarch64__)
    CHECK(strstr(info, "Features\t: fp asimd") && strstr(info, "CPU implementer"), "cpuinfo Features");
    // ID registers read from user mode trap and are emulated, as on Linux
    // (HWCAP_CPUID says so), with the same features as the HWCAPs.
    unsigned long isar0, midr, hwcap = getauxval(AT_HWCAP);
    CHECK(hwcap & (1UL << 11), "HWCAP_CPUID");
    __asm__ volatile("mrs %0, midr_el1" : "=r"(midr));
    __asm__ volatile("mrs %0, id_aa64isar0_el1" : "=r"(isar0));
    CHECK(midr != 0, "mrs MIDR_EL1");
    CHECK((((isar0 >> 4) & 0xF) >= 1) == !!(hwcap & (1UL << 3)), "mrs ID_AA64ISAR0_EL1 agrees with HWCAP_AES");
#else
    CHECK(strstr(info, "flags\t\t: fpu") && strstr(info, " sse2") && strstr(info, "model name"), "cpuinfo flags");
#endif
    free(info);
    char online[32] = {0};
    fd = open("/sys/devices/system/cpu/online", O_RDONLY);
    CHECK(fd >= 0 && read(fd, online, sizeof online - 1) > 0 && online[0] == '0', "/sys/devices/system/cpu/online");
    close(fd);
    CHECK(sysconf(_SC_NPROCESSORS_CONF) == sysconf(_SC_NPROCESSORS_ONLN), "configured processors are online");
    // close_range closes a run of descriptors (or marks them close-on-exec).
    int a = dup(1), b = dup(1);
    CHECK(a > 2 && b > a && syscall(436 /* close_range */, a, b, 4) == 0, "close_range CLOEXEC");
    CHECK(fcntl(a, F_GETFD) == FD_CLOEXEC && fcntl(b, F_GETFD) == FD_CLOEXEC, "marked close-on-exec");
    CHECK(syscall(436 /* close_range */, a, b, 0) == 0 && fcntl(a, F_GETFD) == -1 && fcntl(b, F_GETFD) == -1, "close_range");
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
    // The scheduling attributes (SCHED_OTHER at nice 0) and the memory
    // policy (the default, one node), as glib and WebKit ask.
    unsigned int attr[14];
    memset(attr, 0xff, sizeof attr);
    CHECK(syscall(SYS_sched_getattr, 0, attr, sizeof attr, 0) == 0 && attr[0] == 56 && attr[1] == 0, "sched_getattr");
    int policy = -1;
    unsigned long nodes = ~0UL;
    CHECK(syscall(SYS_get_mempolicy, &policy, &nodes, 64, 0, 0) == 0 && policy == 0 && nodes == 0, "get_mempolicy");
    CHECK(syscall(SYS_get_mempolicy, 0, &nodes, 64, 0, 4) == 0 && nodes == 1, "get_mempolicy: the allowed nodes");
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

    // A window's events through a descriptor poll() can wait on, and the
    // FRAME event once a present is on screen.
    int ev = wave_event_fd(O_CLOEXEC);
    CHECK(ev >= 0, "wave_event_fd");
    struct pollfd pfd = {ev, POLLIN, 0};
    CHECK(poll(&pfd, 1, 0) == 0, "no window events yet");
    long win = wave_win_create(120, 80, "linuxtest", 9, 0);
    CHECK(win > 0, "a window");
    struct wave_surface s;
    CHECK(wave_win_surface(win, &s) == 0 && s.addr, "its surface");
    for (uint32_t i = 0; i < s.stride * s.height; i++) ((uint32_t *)s.addr)[i] = 0xFF2060A0;
    CHECK(wave_win_set_cursor(win, WAVE_CURSOR_TEXT) == 0, "a pointer shape");
    CHECK(wave_win_set_cursor(win, 99) == -EINVAL, "an unknown pointer shape");
    CHECK(wave_win_present(win, 0, 0, s.width, s.height) == 0, "present");
    int framed = 0;
    for (int i = 0; i < 50 && !framed; i++) {
        pfd.revents = 0;
        if (poll(&pfd, 1, 100) == 1 && (pfd.revents & POLLIN)) {
            uint64_t n = 0;
            CHECK(read(ev, &n, 8) == 8 && n >= 1, "read counts the events");
            while (wave_next_event(&e, 0) == 1) framed |= e.kind == WAVE_EV_FRAME;
        }
    }
    CHECK(framed, "a FRAME event after the present, through poll");
    CHECK(wave_win_close(win) == 0, "close the window");
    close(ev);
    printf("PASS WaveOS calls\n");
}

// OSS sound: /dev/dsp plays 48 kHz 16-bit stereo through a WaveOS sound
// stream (GStreamer's osssink, so WebKit's media, uses it).
static void sound(void) {
    int dsp = open("/dev/dsp", O_WRONLY);
    if (dsp < 0) {
        printf("SKIP sound (no /dev/dsp, errno %d)\n", errno);
        return;
    }
    int v = 0x10; // AFMT_S16_LE
    CHECK(ioctl(dsp, 0xC0045005, &v) == 0 && v == 0x10, "OSS: 16-bit samples");
    v = 2;
    CHECK(ioctl(dsp, 0xC0045006, &v) == 0 && v == 2, "OSS: stereo");
    v = 44100;
    CHECK(ioctl(dsp, 0xC0045002, &v) == 0 && v == 48000, "OSS: the rate it plays (48 kHz)");
    int space[4]; // fragments, fragstotal, fragsize, bytes
    CHECK(ioctl(dsp, 0x8010500C, space) == 0 && space[2] > 0 && space[3] > 0, "OSS: room to write");
    static short silence[4800 * 2]; // 100 ms
    CHECK(write(dsp, silence, sizeof silence) == (ssize_t)sizeof silence, "OSS: write");
    int delay = -1;
    // Queued, and what the mixer has put in the device's ring ahead of it (up to 100 ms).
    CHECK(ioctl(dsp, 0x80045017, &delay) == 0 && delay >= 0 && delay <= (int)sizeof silence + 19200, "OSS: delay");
    CHECK(ioctl(dsp, 0x5001, 0) == 0, "OSS: sync");
    CHECK(ioctl(dsp, 0x80045017, &delay) == 0 && delay == 0, "OSS: everything played");
    close(dsp);
    printf("PASS sound (OSS)\n");
}

// Sleeps and timeouts are kept to the nanosecond, not the millisecond tick
// (frame and sound pacing depend on it): nanosleep never wakes early and
// doesn't spin out its last millisecond; epoll_pwait2's sub-millisecond
// timeout is waited for, not rounded down to nothing; a timerfd keeps its
// period.
#ifndef SYS_epoll_pwait2
#define SYS_epoll_pwait2 441
#endif
static long us_between(struct timespec *a, struct timespec *b) {
    return (b->tv_sec - a->tv_sec) * 1000000 + (b->tv_nsec - a->tv_nsec) / 1000;
}

static void precise_timers(void) {
    struct timespec a, b, nap = {0, 1500000};
    long late = 0;
    for (int i = 0; i < 20; i++) {
        clock_gettime(CLOCK_MONOTONIC, &a);
        CHECK(nanosleep(&nap, 0) == 0, "nanosleep");
        clock_gettime(CLOCK_MONOTONIC, &b);
        long us = us_between(&a, &b);
        CHECK(us >= 1500, "nanosleep never wakes early");
        late += us - 1500;
    }
    int ep = epoll_create1(0);
    CHECK(ep >= 0, "epoll_create1");
    struct epoll_event ev;
    struct timespec t = {0, 500000};
    clock_gettime(CLOCK_MONOTONIC, &a);
    long n = syscall(SYS_epoll_pwait2, ep, &ev, 1, &t, 0, 8);
    clock_gettime(CLOCK_MONOTONIC, &b);
    CHECK(n == 0 && us_between(&a, &b) >= 500, "epoll_pwait2 waits out a 0.5 ms timeout");
    close(ep);
    int tfd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(tfd >= 0, "timerfd_create");
    struct itimerspec its = {{0, 4000000}, {0, 4000000}};
    CHECK(timerfd_settime(tfd, 0, &its, 0) == 0, "timerfd_settime");
    unsigned long long ticks = 0, v;
    clock_gettime(CLOCK_MONOTONIC, &a);
    while (ticks < 10) {
        CHECK(read(tfd, &v, 8) == 8, "timerfd read");
        ticks += v;
    }
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = us_between(&a, &b) / 1000;
    CHECK(ms >= 39 && ms < 400, "a 4 ms timerfd ticks ten times in about 40 ms");
    close(tfd);
    printf("PASS precise timers (1.5 ms nanosleep: %ld us late on average; ten 4 ms ticks in %ld ms)\n", late / 20, ms);
}

// ALSA: the card (/dev/snd/controlC0) and its playback device, driven with
// the kernel's ioctls as libasound does: parameters refined and set, frames
// written and through the mapped buffer (committed with SYNC_PTR), the
// start threshold, poll, delay, an underrun and recovering from it.
struct snd_interval {
    unsigned int min, max, flags;
};
struct snd_hw_params {
    unsigned int flags;
    unsigned int masks[3][8];
    unsigned int mres[5][8];
    struct snd_interval intervals[12];
    struct snd_interval ires[9];
    unsigned int rmask, cmask, info, msbits, rate_num, rate_den;
    unsigned long fifo_size;
    unsigned char reserved[64];
};
_Static_assert(sizeof(struct snd_hw_params) == 608, "snd_pcm_hw_params");
struct snd_sw_params {
    int tstamp_mode;
    unsigned int period_step, sleep_min;
    unsigned long avail_min, xfer_align, start_threshold, stop_threshold, silence_threshold, silence_size, boundary;
    unsigned int proto, tstamp_type;
    unsigned char reserved[56];
};
_Static_assert(sizeof(struct snd_sw_params) == 136, "snd_pcm_sw_params");
struct snd_sync_ptr {
    unsigned int flags;
    union {
        struct {
            int state, pad1;
            unsigned long hw_ptr;
            struct timespec tstamp;
            int suspended_state;
            struct timespec audio_tstamp;
        } status;
        unsigned char reserved[64];
    } s;
    union {
        struct {
            unsigned long appl_ptr, avail_min;
        } control;
        unsigned char reserved[64];
    } c;
};
_Static_assert(sizeof(struct snd_sync_ptr) == 136, "snd_pcm_sync_ptr");
struct snd_xferi {
    long result;
    void *buf;
    unsigned long frames;
};
#define PCM_HW_REFINE _IOWR('A', 0x10, struct snd_hw_params)
#define PCM_HW_PARAMS _IOWR('A', 0x11, struct snd_hw_params)
#define PCM_WRITEI _IOW('A', 0x50, struct snd_xferi)
#define PCM_SYNC_PTR _IOWR('A', 0x23, struct snd_sync_ptr)
enum { P_CHANNELS = 10, P_RATE = 11, P_PERIOD_SIZE = 13, P_PERIODS = 15, P_BUFFER_SIZE = 17 };

static void hw_any(struct snd_hw_params *p) {
    memset(p, 0, sizeof *p);
    memset(p->masks, 0xff, sizeof p->masks);
    for (int i = 0; i < 12; i++) p->intervals[i] = (struct snd_interval){0, ~0u, 0};
    p->rmask = ~0u;
}
static void hw_set(struct snd_hw_params *p, int param, unsigned v) {
    p->intervals[param - 8] = (struct snd_interval){v, v, 0};
}
static int hw_setup(int pcm, int access, struct snd_hw_params *p) {
    hw_any(p);
    memset(p->masks, 0, sizeof p->masks);
    p->masks[0][0] = 1u << access;
    p->masks[1][0] = 1u << 2; // S16_LE
    p->masks[2][0] = 1;
    hw_set(p, P_CHANNELS, 2);
    hw_set(p, P_RATE, 48000);
    hw_set(p, P_PERIOD_SIZE, 1024);
    hw_set(p, P_PERIODS, 4);
    return ioctl(pcm, PCM_HW_PARAMS, p);
}
static int sw_setup(int pcm, unsigned long start) {
    struct snd_sw_params sw = {0};
    sw.avail_min = 1024;
    sw.start_threshold = start;
    sw.stop_threshold = 4096;
    return ioctl(pcm, _IOWR('A', 0x13, struct snd_sw_params), &sw) == 0 && sw.boundary >= 4096 ? 0 : -1;
}

static void alsa(void) {
    int ctl = open("/dev/snd/controlC0", O_RDONLY);
    if (ctl < 0) {
        printf("SKIP ALSA (no sound card, errno %d)\n", errno);
        return;
    }
    int ver = 0, dev = -1;
    CHECK(ioctl(ctl, _IOR('U', 0x00, int), &ver) == 0 && ver >> 16 == 2, "ALSA: control version");
    unsigned char card[376];
    CHECK(ioctl(ctl, _IOR('U', 0x01, card), card) == 0 && strcmp((char *)card + 24, "WaveOS") == 0, "ALSA: card info");
    CHECK(ioctl(ctl, _IOR('U', 0x30, int), &dev) == 0 && dev == 0, "ALSA: the card's first PCM");
    CHECK(ioctl(ctl, _IOR('U', 0x30, int), &dev) == 0 && dev == -1, "ALSA: and no other");
    close(ctl);
    int pcm = open("/dev/snd/pcmC0D0p", O_RDWR | O_NONBLOCK);
    CHECK(pcm >= 0, "ALSA: open the PCM");
    CHECK(ioctl(pcm, _IOR('A', 0x00, int), &ver) == 0 && ver >= 0x2000e, "ALSA: PCM version");
    struct snd_hw_params p;
    hw_any(&p);
    CHECK(ioctl(pcm, PCM_HW_REFINE, &p) == 0, "ALSA: refine");
    CHECK(p.intervals[P_RATE - 8].min == 48000 && p.intervals[P_RATE - 8].max == 48000, "ALSA: 48 kHz");
    CHECK(p.intervals[P_CHANNELS - 8].min == 1 && p.intervals[P_CHANNELS - 8].max == 2, "ALSA: one or two channels");
    hw_any(&p);
    hw_set(&p, P_RATE, 44100);
    CHECK(ioctl(pcm, PCM_HW_REFINE, &p) == -1 && errno == EINVAL, "ALSA: a rate it doesn't take is refused");
    CHECK(hw_setup(pcm, 3, &p) == 0, "ALSA: hw_params (written, 16-bit stereo)");
    CHECK(p.intervals[P_BUFFER_SIZE - 8].min == 4096 && p.intervals[P_BUFFER_SIZE - 8].max == 4096, "ALSA: buffer size");
    CHECK(sw_setup(pcm, 2048) == 0, "ALSA: sw_params");
    CHECK(ioctl(pcm, _IO('A', 0x40)) == 0, "ALSA: prepare");
    static short tone[4096 * 2];
    for (int i = 0; i < 4096; i++) tone[2 * i] = tone[2 * i + 1] = (i % 109 - 54) * 100;
    struct snd_xferi x = {0, tone, 1024};
    unsigned char st[152];
    CHECK(ioctl(pcm, PCM_WRITEI, &x) == 0 && x.result == 1024, "ALSA: write");
    CHECK(ioctl(pcm, _IOR('A', 0x20, st), st) == 0 && *(int *)st == 2, "ALSA: below the start threshold: prepared");
    x = (struct snd_xferi){0, tone, 3072};
    CHECK(ioctl(pcm, PCM_WRITEI, &x) == 0 && x.result == 3072, "ALSA: write the rest of the buffer");
    CHECK(ioctl(pcm, _IOR('A', 0x20, st), st) == 0 && *(int *)st == 3, "ALSA: past the start threshold: running");
    x = (struct snd_xferi){0, tone, 1024};
    int r = ioctl(pcm, PCM_WRITEI, &x);
    CHECK((r == -1 && errno == EAGAIN) || (r == 0 && x.result > 0), "ALSA: a full buffer takes no more (EAGAIN)");
    struct pollfd pfd = {pcm, POLLOUT, 0};
    CHECK(poll(&pfd, 1, 3000) == 1 && pfd.revents & POLLOUT, "ALSA: poll says when there is room again");
    long delay = -1;
    CHECK(ioctl(pcm, _IOR('A', 0x21, long), &delay) == 0 && delay >= 0 && delay <= 4096 + 9600, "ALSA: delay");
    // Nothing more written: it runs dry (XRUN); writing says so; prepare recovers.
    struct timespec dry = {0, 600 * 1000000};
    nanosleep(&dry, 0);
    x = (struct snd_xferi){0, tone, 1024};
    CHECK(ioctl(pcm, PCM_WRITEI, &x) == -1 && errno == EPIPE, "ALSA: an underrun is EPIPE");
    CHECK(ioctl(pcm, _IO('A', 0x40)) == 0, "ALSA: prepare after the underrun");
    CHECK(ioctl(pcm, _IO('A', 0x43)) == 0 && ioctl(pcm, _IO('A', 0x12)) == 0, "ALSA: drop, hw_free");
    // The mapped buffer, committed through SYNC_PTR (the status page isn't mappable).
    CHECK(hw_setup(pcm, 0, &p) == 0, "ALSA: hw_params (mapped)");
    CHECK(sw_setup(pcm, 1) == 0, "ALSA: sw_params (mapped)");
    short *buf = mmap(0, 4096 * 4, PROT_READ | PROT_WRITE, MAP_SHARED, pcm, 0);
    CHECK(buf != MAP_FAILED, "ALSA: map the buffer");
    CHECK(mmap(0, 4096, PROT_READ, MAP_SHARED, pcm, 0x80000000) == MAP_FAILED, "ALSA: the status page isn't mapped");
    CHECK(ioctl(pcm, _IO('A', 0x40)) == 0, "ALSA: prepare (mapped)");
    memcpy(buf, tone, 2048 * 4);
    struct snd_sync_ptr sp = {0};
    sp.c.control.appl_ptr = 2048;
    sp.c.control.avail_min = 1024;
    CHECK(ioctl(pcm, PCM_SYNC_PTR, &sp) == 0 && sp.c.control.appl_ptr == 2048 && sp.s.status.state == 3,
          "ALSA: SYNC_PTR commits the mapped frames and starts");
    unsigned long hw = 0;
    for (int i = 0; i < 100 && hw == 0; i++) {
        struct timespec nap = {0, 20 * 1000000};
        nanosleep(&nap, 0);
        sp.flags = 1 | 2 | 4;
        CHECK(ioctl(pcm, PCM_SYNC_PTR, &sp) == 0, "ALSA: SYNC_PTR (hwsync)");
        hw = sp.s.status.hw_ptr;
    }
    CHECK(hw > 0 && hw <= 2048, "ALSA: the mixer takes the mapped frames");
    CHECK(ioctl(pcm, _IO('A', 0x44)) == 0, "ALSA: drain");
    munmap(buf, 4096 * 4);
    close(pcm);
    printf("PASS ALSA (refine, write, start threshold, poll, delay %ld frames, underrun, mapped buffer)\n", delay);
}

int main(int argc, char **argv) {
    start(argc, argv);
    auxv();
    files();
    removed_while_open();
    memory();
    swapping();
    threads();
    credentials();
    avx();
    clone_ids();
    clocks();
    futexes();
    machine();
    wave();
    sound();
    precise_timers();
    alsa();
    if (failures) {
        printf("linux-test: %d failures\n", failures);
        return 1;
    }
    printf("linux-test passed\n");
    return 0;
}
