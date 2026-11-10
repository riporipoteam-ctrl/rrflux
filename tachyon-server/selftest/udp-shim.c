// udp-shim.c — LD_PRELOAD workaround for THIS SANDBOX ONLY.
//
// The Muse VM's netstack returns EPERM for sendto()/sendmsg() on unconnected
// UDP sockets (even on loopback), while connect()+send() on the same socket
// is allowed. LiteNetLib (like most UDP stacks) sends via sendto() on one
// unconnected socket, so nothing can leave the process here.
//
// This shim reroutes datagram sends through connect()+send() on the caller's
// own fd, remembering the fd's current peer and re-pointing with connect()
// when the destination changes. It never disconnects (AF_UNSPEC): on this
// netstack a disconnect leaves the socket deaf to later replies.
//
// Wire bytes are unchanged. Used only by selftest/run-selftest.sh inside
// this sandbox; the Tachyon server is plain stock LiteNetLib and needs no
// shim anywhere else.
//
// Caveats (acceptable for the loopback self-test, never for production):
//  - each fd is left connected to its most recent peer, so the kernel
//    filters inbound datagrams to that peer — the self-test topology is
//    strictly 1 server <-> 1 client, so nothing else exists to filter out;
//  - connect+send is not atomic across threads sending to different peers
//    on the same fd (the self-test never changes a fd's peer once set).

#define _GNU_SOURCE
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <netinet/in.h>
#include <stddef.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <dlfcn.h>
#include <errno.h>
#include <unistd.h>

typedef ssize_t (*sendto_fn)(int, const void *, size_t, int, const struct sockaddr *, socklen_t);
typedef ssize_t (*sendmsg_fn)(int, const struct msghdr *, int);
typedef ssize_t (*send_fn)(int, const void *, size_t, int);
typedef int (*connect_fn)(int, const struct sockaddr *, socklen_t);
typedef int (*close_fn)(int);

static sendto_fn real_sendto;
static sendmsg_fn real_sendmsg;
static send_fn real_send;
static connect_fn real_connect;
static close_fn real_close;

__attribute__((constructor)) static void shim_init(void) {
    real_sendto = (sendto_fn)dlsym(RTLD_NEXT, "sendto");
    real_sendmsg = (sendmsg_fn)dlsym(RTLD_NEXT, "sendmsg");
    real_send = (send_fn)dlsym(RTLD_NEXT, "send");
    real_connect = (connect_fn)dlsym(RTLD_NEXT, "connect");
    real_close = (close_fn)dlsym(RTLD_NEXT, "close");
}

static int shim_enabled(void) {
    const char *v = getenv("TACHYON_UDP_SHIM");
    return v && v[0] == '1';
}

static int shim_debug(void) {
    const char *v = getenv("TACHYON_UDP_SHIM_DEBUG");
    return v && v[0] == '1';
}

// ---- per-fd current-peer table -------------------------------------------
#define MAX_TRACKED_FD 4096
static struct sockaddr_storage peer_of[MAX_TRACKED_FD];
static socklen_t peer_len_of[MAX_TRACKED_FD];
static char peer_set[MAX_TRACKED_FD];

static int same_peer(int fd, const struct sockaddr *dest, socklen_t destlen) {
    if (fd < 0 || fd >= MAX_TRACKED_FD || !peer_set[fd]) return 0;
    if (peer_len_of[fd] != destlen) return 0;
    return memcmp(&peer_of[fd], dest, destlen) == 0;
}

static void remember_peer(int fd, const struct sockaddr *dest, socklen_t destlen) {
    if (fd < 0 || fd >= MAX_TRACKED_FD || destlen > (socklen_t)sizeof(struct sockaddr_storage)) return;
    memcpy(&peer_of[fd], dest, destlen);
    peer_len_of[fd] = destlen;
    peer_set[fd] = 1;
}

static int is_datagram(int fd) {
    int type = 0;
    socklen_t len = sizeof(type);
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &len) != 0) return 0;
    return type == SOCK_DGRAM;
}

// Returns 1 when the fd is now connected to dest and a plain send applies.
static int ensure_connected(int fd, const struct sockaddr *dest, socklen_t destlen) {
    if (same_peer(fd, dest, destlen)) return 1;
    if (real_connect(fd, dest, destlen) != 0) return 0;
    remember_peer(fd, dest, destlen);
    if (shim_debug())
        fprintf(stderr, "[shim] fd=%d connected to family=%d\n", fd, (int)dest->sa_family);
    return 1;
}

int connect(int fd, const struct sockaddr *dest, socklen_t destlen) {
    int rc = real_connect(fd, dest, destlen);
    if (rc == 0 && shim_enabled() && dest != NULL && dest->sa_family != AF_UNSPEC)
        remember_peer(fd, dest, destlen);
    return rc;
}

int close(int fd) {
    if (fd >= 0 && fd < MAX_TRACKED_FD) peer_set[fd] = 0;
    return real_close(fd);
}

ssize_t sendto(int fd, const void *buf, size_t len, int flags,
               const struct sockaddr *dest, socklen_t destlen) {
    if (!shim_enabled() || dest == NULL || !is_datagram(fd))
        return real_sendto(fd, buf, len, flags, dest, destlen);
    if (!ensure_connected(fd, dest, destlen))
        return real_sendto(fd, buf, len, flags, dest, destlen);
    return real_send(fd, buf, len, flags);
}

ssize_t sendmsg(int fd, const struct msghdr *msg, int flags) {
    if (!shim_enabled() || msg == NULL || msg->msg_name == NULL || !is_datagram(fd))
        return real_sendmsg(fd, msg, flags);
    if (!ensure_connected(fd, (const struct sockaddr *)msg->msg_name, msg->msg_namelen))
        return real_sendmsg(fd, msg, flags);
    struct msghdr copy = *msg;
    copy.msg_name = NULL;
    copy.msg_namelen = 0;
    return real_sendmsg(fd, &copy, flags);
}
