// proctest: checks processes and signals in the Linux compatibility layer
// from a real Linux program (built static with musl): fork, exec, wait,
// vfork/posix_spawn, scripts, signals and their handlers, poll and select,
// close-on-exec and symbolic links. The kernel self-tests run it; it exits 0
// when everything passed.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/select.h>
#include <sys/timerfd.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;
static int failures;

#define CHECK(cond, what)                                                        \
    do {                                                                         \
        if (!(cond)) {                                                           \
            printf("FAIL %s: %s (errno %d)\n", __func__, what, errno);           \
            fflush(stdout);                                                      \
            failures++;                                                          \
            return;                                                              \
        }                                                                        \
    } while (0)

static char self[256];

// Runs `argv` with its output captured into `out`; returns the wait status.
static int run(char *const argv[], char *out, size_t size) {
    int fds[2];
    if (pipe(fds) != 0) return -1;
    pid_t pid = fork();
    if (pid == 0) {
        dup2(fds[1], 1);
        close(fds[0]);
        close(fds[1]);
        execv(argv[0], argv);
        _exit(127);
    }
    close(fds[1]);
    size_t got = 0;
    ssize_t n;
    while (got + 1 < size && (n = read(fds[0], out + got, size - 1 - got)) > 0) got += n;
    out[got] = 0;
    close(fds[0]);
    int status = -1;
    waitpid(pid, &status, 0);
    return status;
}

static void forking(void) {
    int shared = 1;
    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid == 0) {
        shared = 2; // the parent's copy stays 1
        _exit(shared == 2 ? 42 : 1);
    }
    int status;
    CHECK(waitpid(pid, &status, 0) == pid, "waitpid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 42, "child's exit code");
    CHECK(shared == 1, "memory is copied, not shared");
    CHECK(waitpid(-1, &status, WNOHANG) == -1 && errno == ECHILD, "no children left");
    printf("PASS fork and wait\n");
}

// fork shares memory copy-on-write: a write by either side, its own or the
// kernel's for a system call (read() below), stays its own.
static char cow[3 * 4096] __attribute__((aligned(4096)));

static void copy_on_write(void) {
    memset(cow, 'a', sizeof cow);
    int go[2];
    CHECK(pipe(go) == 0, "pipe");
    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid == 0) {
        close(go[1]);
        // The kernel writes into the shared page here.
        int ok = read(go[0], cow + 4096, 1) == 1 && cow[4096] == 'x';
        ok = ok && cow[0] == 'a';  // the parent's write isn't seen
        cow[8192] = 'c';
        _exit(ok && cow[8192] == 'c' ? 42 : 1);
    }
    close(go[0]);
    cow[0] = 'p';
    CHECK(write(go[1], "x", 1) == 1, "write to the child");
    close(go[1]);
    int status;
    CHECK(waitpid(pid, &status, 0) == pid, "waitpid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 42, "the child saw its own writes only");
    CHECK(cow[0] == 'p' && cow[4096] == 'a' && cow[8192] == 'a', "the parent sees its own writes only");
    printf("PASS copy-on-write fork\n");
}

static void executing(void) {
    char out[256];
    char *echo[] = {"/bin/echo", "hello", "from", "echo", NULL};
    int st = run(echo, out, sizeof out);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "echo ran");
    CHECK(strcmp(out, "hello from echo\n") == 0, "echo's output");
    char *me[] = {self, "child-args", "a b", "c", NULL};
    st = run(me, out, sizeof out);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 7, "exec of this program");
    CHECK(strcmp(out, "3:a b|c\n") == 0, "arguments arrive");
    char *missing[] = {"/bin/no-such-program", NULL};
    st = run(missing, out, sizeof out);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 127, "a missing program");
    printf("PASS exec\n");
}

static void scripts(void) {
    mkdir("/tmp", 0755);
    FILE *f = fopen("/tmp/pt-script", "w");
    CHECK(f, "write a script");
    fputs("#!/bin/sh\necho script \"$1\"\nfor i in 1 2 3; do printf $i; done\necho\nexit 5\n", f);
    fclose(f);
    chmod("/tmp/pt-script", 0755);
    char out[256];
    char *argv[] = {"/tmp/pt-script", "arg", NULL};
    int st = run(argv, out, sizeof out);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 5, "the script's exit code");
    CHECK(strcmp(out, "script arg\n123\n") == 0, "the script's output");
    char *pipeline[] = {"/bin/sh", "-c", "seq 1 100 | awk '{s+=$1} END {print s}'", NULL};
    st = run(pipeline, out, sizeof out);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0 && strcmp(out, "5050\n") == 0, "a shell pipeline");
    unlink("/tmp/pt-script");
    printf("PASS scripts and the shell\n");
}

static void spawning(void) {
    pid_t pid;
    char *argv[] = {"/bin/true", NULL};
    CHECK(posix_spawn(&pid, "/bin/true", NULL, NULL, argv, environ) == 0, "posix_spawn");
    int status;
    CHECK(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0, "spawned child");
    volatile int touched = 0;
    pid = vfork();
    if (pid == 0) {
        touched = 1; // vfork shares memory until _exit
        _exit(3);
    }
    CHECK(pid > 0, "vfork");
    CHECK(waitpid(pid, &status, 0) == pid && WEXITSTATUS(status) == 3, "vfork child");
    CHECK(touched == 1, "vfork shares memory");
    printf("PASS posix_spawn and vfork\n");
}

static volatile sig_atomic_t got_usr1, got_chld, info_pid;

static void on_usr1(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    got_usr1 = sig;
    info_pid = si->si_pid;
}

static void on_chld(int sig) {
    (void)sig;
    got_chld++;
}

static void signals(void) {
    struct sigaction sa = {0};
    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO;
    CHECK(sigaction(SIGUSR1, &sa, NULL) == 0, "sigaction");
    double before = 1.5;
    CHECK(kill(getpid(), SIGUSR1) == 0, "kill");
    CHECK(got_usr1 == SIGUSR1 && info_pid == getpid(), "the handler ran, with siginfo");
    CHECK(before * 2 == 3.0, "SIMD registers survive a handler");

    sigset_t set, old;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    got_usr1 = 0;
    CHECK(sigprocmask(SIG_BLOCK, &set, &old) == 0, "block");
    raise(SIGUSR1);
    CHECK(got_usr1 == 0, "blocked signals wait");
    sigset_t pending;
    sigpending(&pending);
    CHECK(sigismember(&pending, SIGUSR1), "sigpending");
    CHECK(sigprocmask(SIG_SETMASK, &old, NULL) == 0, "unblock");
    CHECK(got_usr1 == SIGUSR1, "delivered once unblocked");

    struct sigaction chld = {0};
    chld.sa_handler = on_chld;
    chld.sa_flags = SA_RESTART;
    sigaction(SIGCHLD, &chld, NULL);
    pid_t pid = fork();
    if (pid == 0) _exit(0);
    int status;
    waitpid(pid, &status, 0);
    for (int i = 0; i < 100 && !got_chld; i++) usleep(1000);
    CHECK(got_chld >= 1, "SIGCHLD");
    signal(SIGCHLD, SIG_DFL);

    pid = fork();
    if (pid == 0) {
        for (;;) pause();
    }
    usleep(20000);
    CHECK(kill(pid, SIGTERM) == 0, "kill a child");
    CHECK(waitpid(pid, &status, 0) == pid, "wait for it");
    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM, "ended by SIGTERM");
    printf("PASS signals\n");
}

static void interruptions(void) {
    int fds[2];
    CHECK(pipe(fds) == 0, "pipe");
    struct sigaction sa = {0};
    sa.sa_sigaction = on_usr1;
    sa.sa_flags = SA_SIGINFO; // no SA_RESTART
    sigaction(SIGUSR1, &sa, NULL);
    pid_t parent = getpid();
    pid_t pid = fork();
    if (pid == 0) {
        usleep(30000);
        kill(parent, SIGUSR1);
        usleep(30000);
        write(fds[1], "x", 1);
        _exit(0);
    }
    char c;
    got_usr1 = 0;
    ssize_t n = read(fds[0], &c, 1);
    CHECK(n == -1 && errno == EINTR && got_usr1, "a signal interrupts read");
    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    sigaction(SIGUSR1, &sa, NULL);
    CHECK(read(fds[0], &c, 1) == 1 && c == 'x', "read again");
    waitpid(pid, NULL, 0);

    struct pollfd pfd = {fds[0], POLLIN, 0};
    CHECK(poll(&pfd, 1, 20) == 0, "poll times out");
    write(fds[1], "y", 1);
    CHECK(poll(&pfd, 1, 1000) == 1 && (pfd.revents & POLLIN), "poll sees data");
    fd_set rd;
    FD_ZERO(&rd);
    FD_SET(fds[0], &rd);
    struct timeval tv = {1, 0};
    CHECK(select(fds[0] + 1, &rd, NULL, NULL, &tv) == 1 && FD_ISSET(fds[0], &rd), "select");
    read(fds[0], &c, 1);

    signal(SIGPIPE, SIG_IGN);
    close(fds[0]);
    CHECK(write(fds[1], "z", 1) == -1 && errno == EPIPE, "EPIPE once the reader is gone");
    close(fds[1]);
    signal(SIGPIPE, SIG_DFL);
    printf("PASS interrupted calls, poll and select\n");
}

static long ms_since(struct timespec *t0) {
    struct timespec t1;
    clock_gettime(CLOCK_MONOTONIC, &t1);
    return (t1.tv_sec - t0->tv_sec) * 1000 + (t1.tv_nsec - t0->tv_nsec) / 1000000;
}

static void event_loops(void) {
    int ev = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
    CHECK(ev >= 0, "eventfd");
    uint64_t v;
    CHECK(read(ev, &v, 8) == -1 && errno == EAGAIN, "an empty eventfd");
    v = 3;
    write(ev, &v, 8);
    v = 4;
    write(ev, &v, 8);
    CHECK(read(ev, &v, 8) == 8 && v == 7, "eventfd adds up");

    int tf = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
    CHECK(tf >= 0, "timerfd");
    struct itimerspec its = {{0, 20 * 1000000}, {0, 30 * 1000000}};
    CHECK(timerfd_settime(tf, 0, &its, NULL) == 0, "arm the timer");
    int ep = epoll_create1(EPOLL_CLOEXEC);
    CHECK(ep >= 0, "epoll_create1");
    struct epoll_event e1 = {EPOLLIN, {.u32 = 1}}, e2 = {EPOLLIN, {.u32 = 2}};
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, tf, &e1) == 0 && epoll_ctl(ep, EPOLL_CTL_ADD, ev, &e2) == 0, "epoll_ctl");
    struct epoll_event out[4];
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int n = epoll_wait(ep, out, 4, 1000);
    long ms = ms_since(&t0);
    CHECK(n == 1 && out[0].data.u32 == 1, "epoll reports the timer");
    CHECK(ms >= 25 && ms < 500, "after about 30 ms");
    uint64_t expirations;
    CHECK(read(tf, &expirations, 8) == 8 && expirations >= 1, "the timer's expirations");
    struct itimerspec off = {{0, 0}, {0, 0}};
    timerfd_settime(tf, 0, &off, NULL);

    pid_t pid = fork();
    if (pid == 0) {
        usleep(30000);
        v = 1;
        write(ev, &v, 8);
        _exit(0);
    }
    clock_gettime(CLOCK_MONOTONIC, &t0);
    n = epoll_wait(ep, out, 4, 2000);
    ms = ms_since(&t0);
    CHECK(n == 1 && out[0].data.u32 == 2, "epoll wakes when another process writes the eventfd");
    CHECK(ms < 1000, "promptly");
    waitpid(pid, NULL, 0);
    CHECK(epoll_wait(ep, out, 4, 0) == 1, "level-triggered: still readable");
    read(ev, &v, 8);
    CHECK(epoll_wait(ep, out, 4, 0) == 0, "nothing once read");
    close(ep);
    close(tf);
    close(ev);
    printf("PASS eventfd, timerfd and epoll (%ld ms)\n", ms);
}

static void close_on_exec(void) {
    int keep = open("/dev/null", O_RDONLY);
    int drop = open("/dev/null", O_RDONLY | O_CLOEXEC);
    CHECK(keep >= 0 && drop >= 0, "open");
    CHECK(fcntl(drop, F_GETFD) == FD_CLOEXEC && fcntl(keep, F_GETFD) == 0, "F_GETFD");
    char a[16], b[16], out[64];
    snprintf(a, sizeof a, "%d", keep);
    snprintf(b, sizeof b, "%d", drop);
    char *argv[] = {self, "child-fds", a, b, NULL};
    int st = run(argv, out, sizeof out);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "only close-on-exec descriptors close");
    close(keep);
    close(drop);
    printf("PASS close-on-exec\n");
}

static void links(void) {
    mkdir("/tmp", 0755);
    unlink("/tmp/pt-link");
    CHECK(symlink("/bin/busybox", "/tmp/pt-link") == 0, "symlink");
    char target[64] = {0};
    CHECK(readlink("/tmp/pt-link", target, sizeof target - 1) > 0 && strcmp(target, "/bin/busybox") == 0, "readlink");
    struct stat st;
    CHECK(lstat("/tmp/pt-link", &st) == 0 && S_ISLNK(st.st_mode), "lstat");
    CHECK(stat("/tmp/pt-link", &st) == 0 && S_ISREG(st.st_mode), "stat follows");
    CHECK(lstat("/bin/sh", &st) == 0 && S_ISLNK(st.st_mode), "/bin/sh is a link");
    CHECK(access("/usr/bin/awk", X_OK) == 0, "BusyBox commands are where Debian has them");
    CHECK(unlink("/tmp/pt-link") == 0 && lstat("/tmp/pt-link", &st) != 0, "unlink a link");
    printf("PASS symbolic links\n");
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "child-args") == 0) {
        printf("%d:%s|%s\n", argc - 1, argv[2], argv[3]);
        return 7;
    }
    if (argc >= 4 && strcmp(argv[1], "child-fds") == 0) {
        int keep = atoi(argv[2]), drop = atoi(argv[3]);
        return fcntl(keep, F_GETFD) >= 0 && fcntl(drop, F_GETFD) == -1 && errno == EBADF ? 0 : 1;
    }
    ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
    if (n <= 0) {
        printf("FAIL: /proc/self/exe\n");
        return 1;
    }
    self[n] = 0;
    setvbuf(stdout, NULL, _IONBF, 0);
    forking();
    copy_on_write();
    executing();
    scripts();
    spawning();
    signals();
    interruptions();
    event_loops();
    close_on_exec();
    links();
    if (failures) {
        printf("proc-test: %d failures\n", failures);
        return 1;
    }
    printf("proc-test passed\n");
    return 0;
}
