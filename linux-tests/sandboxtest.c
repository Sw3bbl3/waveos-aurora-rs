// sandboxtest: the sandbox from a Linux program (built static with musl).
// The kernel self-tests run it in a sandbox that declares only `linux`
// (see kernel/src/sandbox/selftest.rs), with /SandboxOracle holding files
// it must never reach, an unconfined victim process (argv[1]), a local
// socket and a shared memory object of the system's, and a link to the
// oracle planted in /tmp by someone else. Every escape a Linux program
// might try must fail: paths, `..`, directory descriptors, links (soft
// and hard), renames racing each other, /proc, other apps' sockets and
// shared memory, signals, exec, the DRM device, WaveOS calls; what it
// was given (an inherited descriptor) and its own things must work.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>
#include "../libs/linux/include/waveos.h"

#define SECRET "/SandboxOracle/secret.txt"

static int failures;

#define CHECK(cond, what)                                              \
    do {                                                               \
        if (!(cond)) {                                                 \
            printf("FAIL %s: %s (errno %d)\n", __func__, what, errno); \
            failures++;                                                \
        }                                                              \
    } while (0)

// The call failed with `want`.
#define REFUSED(call, want, what) CHECK((call) == -1 && errno == (want), what)

static void paths(void) {
    struct stat st;
    REFUSED(open(SECRET, O_RDONLY), EACCES, "open the oracle");
    REFUSED(open(SECRET, O_WRONLY | O_TRUNC), EACCES, "open the oracle to write");
    REFUSED(stat(SECRET, &st), EACCES, "stat the oracle");
    REFUSED(access(SECRET, R_OK), EACCES, "access the oracle");
    REFUSED(truncate(SECRET, 0), EACCES, "truncate the oracle");
    REFUSED(chmod(SECRET, 0777), EACCES, "chmod the oracle");
    REFUSED(unlink(SECRET), EACCES, "unlink the oracle");
    REFUSED(rename(SECRET, "/tmp/stolen"), EACCES, "rename the oracle away");
    REFUSED(mkdir("/SandboxOracle/x", 0755), EACCES, "mkdir in the oracle");
    REFUSED(chdir("/SandboxOracle"), EACCES, "chdir to the oracle");
    REFUSED(open("/tmp/../../../../SandboxOracle/secret.txt", O_RDONLY), EACCES, "walk out with ..");
    // Listing what is not its own.
    int root = open("/", O_RDONLY | O_DIRECTORY);
    char buf[2048];
    if (root >= 0) {
        REFUSED(syscall(SYS_getdents64, root, buf, sizeof buf), EACCES, "list /");
        close(root);
    }
    REFUSED(open("/Documents/sandboxtest.txt", O_WRONLY | O_CREAT, 0644), EACCES, "make a file in Documents");
}

static void own_files(void) {
    // /tmp and the home folder are in the container.
    int fd = open("/tmp/sbx-own.txt", O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "a file in /tmp");
    if (fd >= 0) {
        CHECK(write(fd, "own", 3) == 3, "write it");
        close(fd);
    }
    const char *home = getenv("HOME");
    char p[256];
    snprintf(p, sizeof p, "%s/sbx-home.txt", home ? home : "/home/user");
    fd = open(p, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(fd >= 0, "a file in the home folder");
    if (fd >= 0) {
        CHECK(write(fd, "home", 4) == 4, "write it");
        close(fd);
    }
    CHECK(access("/usr/bin", X_OK) == 0 || access("/bin/sh", X_OK) == 0, "the Linux system tree is there");
}

static void descriptors(void) {
    // A directory descriptor is no way out.
    int dir = open("/tmp", O_RDONLY | O_DIRECTORY);
    CHECK(dir >= 0, "open /tmp");
    if (dir >= 0) {
        REFUSED(openat(dir, "../../../../../SandboxOracle/secret.txt", O_RDONLY), EACCES, "openat .. from a dirfd");
        REFUSED(fchdir(dir) == 0 ? open("../../../../SandboxOracle/secret.txt", O_RDONLY) : -1, EACCES,
                "relative from the working folder");
        close(dir);
    }
    // What it was given works: standard input is a file the harness opened.
    char b[32] = {0};
    CHECK(read(0, b, sizeof b - 1) > 0 && strncmp(b, "inherited", 9) == 0, "read the inherited descriptor");
    int again = open("/proc/self/fd/0", O_RDONLY);
    CHECK(again >= 0, "reopen it through /proc/self/fd");
    if (again >= 0) close(again);
    CHECK(open("/proc/1/root" SECRET, O_RDONLY) == -1 && (errno == ENOENT || errno == EACCES),
          "another process's /proc");
}

static void links(void) {
    // It may not plant links (the table is everyone's)...
    REFUSED(symlink(SECRET, "/tmp/sbx-mine"), EACCES, "make a link");
    // ...and one planted by someone else leads where it may not go.
    REFUSED(open("/tmp/sbx-planted", O_RDONLY), EACCES, "follow a planted link to the oracle");
    char t[256];
    ssize_t n = readlink("/tmp/sbx-planted", t, sizeof t);
    CHECK(n > 0, "read the planted link itself");
    // No hard links at all.
    REFUSED(link(SECRET, "/tmp/sbx-hard"), EPERM, "hard link to the oracle");
    REFUSED(link("/tmp/sbx-own.txt", "/tmp/sbx-hard"), EPERM, "hard link to its own file");
}

static volatile int racing = 1;

static void *renamer(void *arg) {
    (void)arg;
    while (racing) {
        rename("/tmp/race-a", "/tmp/race-b");
        rename("/tmp/race-b", "/tmp/race-a");
    }
    return NULL;
}

static void renames(void) {
    mkdir("/tmp/race-a", 0755);
    pthread_t t;
    pthread_create(&t, NULL, renamer, NULL);
    int got = 0;
    for (int i = 0; i < 400; i++) {
        int fd = open("/tmp/race-a/../../../../SandboxOracle/secret.txt", O_RDONLY);
        if (fd >= 0) {
            got++;
            close(fd);
        }
        if (rename(SECRET, "/tmp/race-a/stolen") == 0) got++;
        if (rename("/tmp/race-b", "/SandboxOracle/planted") == 0) got++;
    }
    racing = 0;
    pthread_join(t, NULL);
    CHECK(got == 0, "renames racing opened or moved the oracle");
}

static void sockets(void) {
    REFUSED(socket(AF_INET, SOCK_STREAM, 0), EACCES, "a network socket");
    REFUSED(socket(AF_INET6, SOCK_DGRAM, 0), EACCES, "a UDP socket");
    // The system's local socket is not in its namespace.
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    CHECK(s >= 0, "a local socket");
    struct sockaddr_un a = {.sun_family = AF_UNIX};
    memcpy(a.sun_path, "\0sbx-public", 11);
    REFUSED(connect(s, (struct sockaddr *)&a, offsetof(struct sockaddr_un, sun_path) + 11), ECONNREFUSED,
            "connect to another app's socket");
    close(s);
    // Its own: bound, connected and passing descriptors between its processes.
    int l = socket(AF_UNIX, SOCK_STREAM, 0);
    memcpy(a.sun_path, "\0sbx-own", 8);
    socklen_t len = offsetof(struct sockaddr_un, sun_path) + 8;
    CHECK(bind(l, (struct sockaddr *)&a, len) == 0 && listen(l, 4) == 0, "bind its own socket");
    pid_t child = fork();
    if (child == 0) {
        int c = socket(AF_UNIX, SOCK_STREAM, 0);
        if (connect(c, (struct sockaddr *)&a, len) != 0) _exit(1);
        int fd = open("/tmp/sbx-own.txt", O_RDONLY);
        char ctl[CMSG_SPACE(sizeof(int))] = {0};
        struct iovec iov = {"f", 1};
        struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
        struct cmsghdr *cm = CMSG_FIRSTHDR(&m);
        cm->cmsg_level = SOL_SOCKET;
        cm->cmsg_type = SCM_RIGHTS;
        cm->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cm), &fd, sizeof(int));
        _exit(sendmsg(c, &m, 0) == 1 ? 0 : 2);
    }
    int conn = accept(l, NULL, NULL);
    CHECK(conn >= 0, "accept from its own process");
    if (conn >= 0) {
        char ctl[CMSG_SPACE(sizeof(int))] = {0};
        char d;
        struct iovec iov = {&d, 1};
        struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
        CHECK(recvmsg(conn, &m, 0) == 1, "receive a descriptor");
        struct cmsghdr *cm = CMSG_FIRSTHDR(&m);
        int fd = -1;
        if (cm) memcpy(&fd, CMSG_DATA(cm), sizeof(int));
        char b[4] = {0};
        CHECK(fd >= 0 && read(fd, b, 3) == 3 && memcmp(b, "own", 3) == 0, "read the passed descriptor");
        close(conn);
    }
    int st = 0;
    waitpid(child, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "the child that passed it");
    close(l);
}

static void shared_memory(void) {
    REFUSED(open("/dev/shm/sbx-public", O_RDONLY), ENOENT, "another app's shared memory");
    int fd = open("/dev/shm/sbx-own", O_RDWR | O_CREAT, 0600);
    CHECK(fd >= 0, "its own shared memory");
    if (fd >= 0) {
        CHECK(ftruncate(fd, 4096) == 0, "size it");
        close(fd);
        CHECK(unlink("/dev/shm/sbx-own") == 0, "remove it");
    }
}

static void processes(const char *victim) {
    pid_t v = (pid_t)atoi(victim);
    REFUSED(kill(v, SIGTERM), EPERM, "signal another app's process");
    REFUSED(kill(v, 0), EPERM, "probe another app's process");
    pid_t child = fork();
    if (child == 0) {
        // No `spawn`: no other programs.
        execl("/bin/true", "true", (char *)NULL);
        _exit(errno == EACCES ? 0 : 1);
    }
    int st = 0;
    waitpid(child, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "exec refused without spawn");
    child = fork();
    if (child == 0) {
        pause();
        _exit(0);
    }
    CHECK(kill(child, SIGKILL) == 0, "signal its own child");
    waitpid(child, &st, 0);
}

static void devices(void) {
    REFUSED(open("/dev/dri/card0", O_RDWR), EACCES, "the DRM card");
    REFUSED(open("/dev/dsp", O_WRONLY), EACCES, "sound");
    REFUSED(syscall(WAVE_BASE + WAVE_NOTIFY, "t", 1L, "b", 1L), EACCES, "a notification");
    REFUSED(syscall(WAVE_BASE + WAVE_AUDIO_OPEN), EACCES, "a sound stream");
}

int main(int argc, char **argv) {
    paths();
    own_files();
    descriptors();
    links();
    renames();
    sockets();
    shared_memory();
    processes(argc > 1 ? argv[1] : "1");
    devices();
    if (failures == 0) printf("sandbox-test passed\n");
    else printf("sandbox-test: %d failed\n", failures);
    return failures;
}
