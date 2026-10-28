// nettest: checks sockets in the Linux compatibility layer from a real
// Linux program (built static with musl): local sockets of every type,
// passing descriptors (SCM_RIGHTS), and TCP and UDP over the loopback
// interface, blocking and not. The kernel self-tests run it.

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stddef.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
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

static int send_fd(int sock, int fd, const char *data) {
    char ctl[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = {(void *)data, strlen(data)};
    struct msghdr m = {0};
    m.msg_iov = &iov;
    m.msg_iovlen = 1;
    m.msg_control = ctl;
    m.msg_controllen = sizeof ctl;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET;
    c->cmsg_type = SCM_RIGHTS;
    c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(int));
    return sendmsg(sock, &m, 0);
}

static int recv_fd(int sock, char *buf, size_t size) {
    char ctl[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = {buf, size};
    struct msghdr m = {0};
    m.msg_iov = &iov;
    m.msg_iovlen = 1;
    m.msg_control = ctl;
    m.msg_controllen = sizeof ctl;
    ssize_t n = recvmsg(sock, &m, MSG_CMSG_CLOEXEC);
    if (n < 0) return -1;
    buf[n] = 0;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (!c || c->cmsg_type != SCM_RIGHTS) return -2;
    int fd;
    memcpy(&fd, CMSG_DATA(c), sizeof(int));
    return fd;
}

static void pairs_and_rights(void) {
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv) == 0, "socketpair");
    CHECK(write(sv[0], "ping", 4) == 4, "write");
    char buf[64] = {0};
    CHECK(read(sv[1], buf, sizeof buf) == 4 && memcmp(buf, "ping", 4) == 0, "read");

    // A pipe's read end travels to a child, which reads what we write.
    int pfd[2];
    pipe(pfd);
    pid_t pid = fork();
    if (pid == 0) {
        close(sv[0]);
        char msg[32];
        int fd = recv_fd(sv[1], msg, sizeof msg - 1);
        char data[16] = {0};
        int ok = fd >= 0 && strcmp(msg, "here") == 0 && read(fd, data, sizeof data) == 5 &&
                 strcmp(data, "hello") == 0 && (fcntl(fd, F_GETFD) & FD_CLOEXEC);
        _exit(ok ? 0 : 1);
    }
    CHECK(send_fd(sv[0], pfd[0], "here") == 4, "sendmsg with a descriptor");
    close(pfd[0]);
    write(pfd[1], "hello", 5);
    int st;
    waitpid(pid, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "the child got the descriptor and read through it");
    close(pfd[1]);
    close(sv[0]);
    CHECK(read(sv[1], buf, sizeof buf) == 0, "end of file once the peer is gone");
    close(sv[1]);

    CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) == 0, "seqpacket pair");
    send(sv[0], "one", 3, 0);
    send(sv[0], "three", 5, 0);
    CHECK(recv(sv[1], buf, sizeof buf, 0) == 3 && recv(sv[1], buf, sizeof buf, 0) == 5, "message boundaries");
    close(sv[0]);
    close(sv[1]);
    printf("PASS socketpair and SCM_RIGHTS\n");
}

static void named(void) {
    const char *path = "/tmp/nettest.sock";
    unlink(path);
    int srv = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un a = {AF_UNIX};
    strcpy(a.sun_path, path);
    CHECK(bind(srv, (void *)&a, sizeof a) == 0 && listen(srv, 4) == 0, "bind and listen");
    pid_t pid = fork();
    if (pid == 0) {
        int c = socket(AF_UNIX, SOCK_STREAM, 0);
        if (connect(c, (void *)&a, sizeof a) != 0) _exit(2);
        write(c, "from the client", 15);
        char b[8];
        _exit(read(c, b, sizeof b) == 2 && memcmp(b, "ok", 2) == 0 ? 0 : 3);
    }
    int c = accept4(srv, NULL, NULL, SOCK_CLOEXEC);
    CHECK(c >= 0, "accept");
    char buf[32] = {0};
    CHECK(read(c, buf, sizeof buf) == 15 && strcmp(buf, "from the client") == 0, "read from the client");
    struct ucred cred;
    socklen_t len = sizeof cred;
    CHECK(getsockopt(c, SOL_SOCKET, SO_PEERCRED, &cred, &len) == 0 && cred.pid == pid, "SO_PEERCRED");
    write(c, "ok", 2);
    int st;
    waitpid(pid, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0, "the client");
    close(c);
    close(srv);

    // The abstract namespace, with datagrams.
    int d1 = socket(AF_UNIX, SOCK_DGRAM, 0), d2 = socket(AF_UNIX, SOCK_DGRAM, 0);
    struct sockaddr_un ab = {AF_UNIX};
    memcpy(ab.sun_path, "\0nettest-abstract", 17);
    socklen_t ablen = offsetof(struct sockaddr_un, sun_path) + 17;
    CHECK(bind(d1, (void *)&ab, ablen) == 0, "bind an abstract name");
    CHECK(sendto(d2, "dgram", 5, 0, (void *)&ab, ablen) == 5, "sendto");
    CHECK(recv(d1, buf, sizeof buf, 0) == 5, "recv a datagram");
    close(d1);
    close(d2);
    printf("PASS named and abstract local sockets\n");
}

static void tcp(void) {
    int srv = socket(AF_INET, SOCK_STREAM, 0);
    int one = 1;
    setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in a = {AF_INET, htons(40123), {htonl(INADDR_LOOPBACK)}};
    CHECK(bind(srv, (void *)&a, sizeof a) == 0, "bind");
    CHECK(listen(srv, 4) == 0, "listen");
    int c = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
    int r = connect(c, (void *)&a, sizeof a);
    CHECK(r == 0 || errno == EINPROGRESS, "non-blocking connect");
    struct pollfd pfd = {c, POLLOUT, 0};
    CHECK(poll(&pfd, 1, 5000) == 1, "connected (poll)");
    int err = -1;
    socklen_t len = sizeof err;
    CHECK(getsockopt(c, SOL_SOCKET, SO_ERROR, &err, &len) == 0 && err == 0, "SO_ERROR");
    struct pollfd lp = {srv, POLLIN, 0};
    CHECK(poll(&lp, 1, 5000) == 1, "a connection waits");
    struct sockaddr_in peer;
    len = sizeof peer;
    int s = accept(srv, (void *)&peer, &len);
    CHECK(s >= 0 && peer.sin_addr.s_addr == htonl(INADDR_LOOPBACK), "accept");
    setsockopt(s, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    CHECK(send(c, "over tcp", 8, 0) == 8, "send");
    char buf[32] = {0};
    struct pollfd sp = {s, POLLIN, 0};
    poll(&sp, 1, 5000);
    CHECK(recv(s, buf, sizeof buf, 0) == 8 && strcmp(buf, "over tcp") == 0, "recv");
    CHECK(recv(c, buf, sizeof buf, 0) == -1 && errno == EAGAIN, "nothing yet (non-blocking)");
    int ep = epoll_create1(0);
    struct epoll_event e = {EPOLLIN, {.fd = c}};
    epoll_ctl(ep, EPOLL_CTL_ADD, c, &e);
    write(s, "back", 4);
    struct epoll_event out;
    CHECK(epoll_wait(ep, &out, 1, 5000) == 1 && out.data.fd == c, "epoll sees the reply");
    CHECK(read(c, buf, sizeof buf) == 4, "read the reply");
    close(s);
    for (int i = 0; i < 100 && read(c, buf, sizeof buf) != 0; i++) usleep(10000);
    CHECK(read(c, buf, sizeof buf) == 0, "end of file after close");
    close(c);
    close(ep);
    close(srv);
    printf("PASS TCP over loopback\n");
}

static void udp(void) {
    int a = socket(AF_INET, SOCK_DGRAM, 0), b = socket(AF_INET, SOCK_DGRAM, 0);
    struct sockaddr_in addr = {AF_INET, htons(40124), {htonl(INADDR_LOOPBACK)}};
    CHECK(bind(a, (void *)&addr, sizeof addr) == 0, "bind");
    CHECK(sendto(b, "datagram", 8, 0, (void *)&addr, sizeof addr) == 8, "sendto");
    struct pollfd pfd = {a, POLLIN, 0};
    CHECK(poll(&pfd, 1, 5000) == 1, "poll");
    char buf[32] = {0};
    struct sockaddr_in from;
    socklen_t len = sizeof from;
    CHECK(recvfrom(a, buf, sizeof buf, 0, (void *)&from, &len) == 8 && strcmp(buf, "datagram") == 0, "recvfrom");
    CHECK(from.sin_family == AF_INET && ntohs(from.sin_port) != 0, "the sender's address");
    struct addrinfo *res = NULL;
    CHECK(getaddrinfo("localhost", "80", NULL, &res) == 0 && res, "getaddrinfo(localhost)");
    freeaddrinfo(res);
    close(a);
    close(b);
    printf("PASS UDP and name lookup\n");
}

static void shared_memory(void) {
    int fd = memfd_create("nettest", MFD_CLOEXEC);
    CHECK(fd >= 0, "memfd_create");
    CHECK(ftruncate(fd, 3 * 4096) == 0, "ftruncate");
    char *m = mmap(NULL, 3 * 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    CHECK(m != MAP_FAILED, "mmap MAP_SHARED");
    strcpy(m + 4096, "parent");
    pid_t pid = fork();
    if (pid == 0) {
        _exit(strcmp(m + 4096, "parent") == 0 && (strcpy(m + 8192, "child"), 1) ? 0 : 1);
    }
    int st;
    waitpid(pid, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0 && strcmp(m + 8192, "child") == 0, "both processes see the same memory");
    char buf[8] = {0};
    CHECK(pread(fd, buf, 5, 8192) == 5 && strcmp(buf, "child") == 0, "read through the descriptor");

    // The memory travels to another process over a socket, as Wayland buffers do.
    int sv[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    pid = fork();
    if (pid == 0) {
        char msg[8];
        int got = recv_fd(sv[1], msg, sizeof msg - 1);
        char *c = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, got, 0);
        if (c == MAP_FAILED) _exit(2);
        strcpy(c, "drawn by the client");
        _exit(0);
    }
    send_fd(sv[0], fd, "buf");
    waitpid(pid, &st, 0);
    CHECK(WIFEXITED(st) && WEXITSTATUS(st) == 0 && strcmp(m, "drawn by the client") == 0, "a passed memfd maps to the same pages");
    close(sv[0]);
    close(sv[1]);
    munmap(m, 3 * 4096);
    close(fd);

    int *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    CHECK(shared != MAP_FAILED, "shared anonymous memory");
    *shared = 1;
    pid = fork();
    if (pid == 0) {
        *shared = 42;
        _exit(0);
    }
    waitpid(pid, NULL, 0);
    CHECK(*shared == 42, "shared across fork");

    int s1 = shm_open("/nettest-shm", O_CREAT | O_RDWR | O_EXCL, 0600);
    CHECK(s1 >= 0 && ftruncate(s1, 4096) == 0, "shm_open");
    int *a = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, s1, 0);
    int s2 = shm_open("/nettest-shm", O_RDWR, 0);
    int *b = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, s2, 0);
    *a = 7;
    CHECK(*b == 7, "two opens of one /dev/shm object");
    CHECK(shm_unlink("/nettest-shm") == 0 && shm_open("/nettest-shm", O_RDWR, 0) < 0, "shm_unlink");

    char *g = mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    strcpy(g + 4096, "moved");
    char *h = mremap(g, 8192, 64 << 20, MREMAP_MAYMOVE);
    CHECK(h != MAP_FAILED && strcmp(h + 4096, "moved") == 0, "mremap keeps the contents");
    h[(64 << 20) - 1] = 1;
    printf("PASS shared memory\n");
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    mkdir("/tmp", 0755);
    pairs_and_rights();
    named();
    tcp();
    udp();
    shared_memory();
    if (failures) {
        printf("net-test: %d failures\n", failures);
        return 1;
    }
    printf("net-test passed\n");
    return 0;
}
