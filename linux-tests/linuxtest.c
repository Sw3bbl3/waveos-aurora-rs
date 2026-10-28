// linuxtest: checks the Linux compatibility layer from inside a real Linux
// program (built static with musl). The kernel self-tests run it; it prints
// one line per check and exits 0 when everything passed.

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <time.h>
#include <unistd.h>

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
    printf("PASS memory\n");
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
    int fds[2];
    CHECK(pipe(fds) == 0 && write(fds[1], "hi", 2) == 2, "pipe");
    char got[2];
    CHECK(read(fds[0], got, 2) == 2 && got[0] == 'h', "pipe read");
    printf("PASS machine (%s %s)\n", u.sysname, u.release);
}

int main(int argc, char **argv) {
    start(argc, argv);
    files();
    memory();
    threads();
    clocks();
    machine();
    if (failures) {
        printf("linux-test: %d failures\n", failures);
        return 1;
    }
    printf("linux-test passed\n");
    return 0;
}
