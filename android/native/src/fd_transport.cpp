// fd transport implementation: BytePipe over an owned file descriptor — the
// native write side of the Bluetooth RFCOMM (SPP) bridge (see transport.h).
//
// The wire bytes are byte-for-byte what TcpConn::Send puts on a TCP stream
// (EncodePacket's 6-byte header + payload, written in full), so the desktop
// decoder is shared between the TCP and Bluetooth transports.
#include "openay/transport.h"

#include <sys/socket.h>
#include <unistd.h>

#include <cerrno>
#include <cstdio>

#include "internal_log.h"

namespace openay {

FdBytePipe::~FdBytePipe() {
    if (fd_ >= 0) close(fd_);  // exactly once: the fd is owned (see transport.h)
}

bool FdBytePipe::Push(const uint8_t* data, size_t size) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: FdBytePipe::Push on invalid fd\n");
        return false;
    }
    // Full-write loop, mirroring TcpConn::Send. send(MSG_NOSIGNAL) is tried
    // first so socket fds never raise SIGPIPE; the production fd is a pipe
    // (Kotlin's ParcelFileDescriptor.createPipe), which reports ENOTSOCK and
    // flips the pipe to write() permanently. write() on Android is SIGPIPE
    // safe because ART blocks the signal process-wide (EPIPE comes back).
    size_t off = 0;
    while (off < size) {
        ssize_t w;
        if (socket_) {
            w = send(fd_, data + off, size - off, MSG_NOSIGNAL);
            if (w < 0 && errno == ENOTSOCK) {
                socket_ = false;
                continue;
            }
        } else {
            w = write(fd_, data + off, size - off);
        }
        if (w < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno(socket_ ? "send" : "write");
            return false;
        }
        off += static_cast<size_t>(w);
    }
    return true;
}

ssize_t FdBytePipe::Pull(uint8_t* data, size_t max_size) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: FdBytePipe::Pull on invalid fd\n");
        return -1;
    }
    for (;;) {
        const ssize_t r = read(fd_, data, max_size);
        if (r < 0 && errno == EINTR) continue;
        return r;  // > 0 bytes, 0 on clean EOF, -1 on error
    }
}

}  // namespace openay
