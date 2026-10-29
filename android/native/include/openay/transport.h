// OpenAY Mic — POSIX transports (UDP datagram + TCP stream framing).
//
// Portability: only <sys/socket.h>-family POSIX APIs plus std::thread; this
// compiles on Linux hosts and Android NDK (bionic) unchanged. No exceptions
// cross the public API: every failure returns false / nullptr and logs to
// stderr with the errno string.
#ifndef OPENAY_TRANSPORT_H
#define OPENAY_TRANSPORT_H

#include <cstdint>
#include <memory>
#include <string>
#include <sys/types.h>
#include <unistd.h>
#include <vector>

#include "openay/protocol.h"
#include "openay/stats.h"

namespace openay {

// ---------------------------------------------------------------------------
// BytePipe — abstract byte-stream seam.
//
// Extension point for transports that cannot be expressed as a POSIX socket
// created by this library — most importantly the Bluetooth RFCOMM (SPP)
// bridge wired from Kotlin through JNI on Android. RFCOMM is a byte stream
// with the same framing/resync rules as TCP, so the BytePipe implementation
// (FdBytePipe below) reuses TcpConn's framing logic once a JNI file
// descriptor is handed over.
//
// Implementations must be safe to call from a single receiving thread plus a
// single sending thread (or externally synchronized).
// ---------------------------------------------------------------------------
class BytePipe {
public:
    virtual ~BytePipe() = default;

    // Push up to `size` bytes into the pipe. Returns false on transport
    // error or closed pipe; callers should treat false as terminal.
    virtual bool Push(const uint8_t* data, size_t size) = 0;

    // Pull up to `max_size` bytes out of the pipe. Returns the number of
    // bytes read (> 0), 0 on clean end-of-stream, or -1 on error.
    virtual ssize_t Pull(uint8_t* data, size_t max_size) = 0;
};

// ---------------------------------------------------------------------------
// UDP (Wi-Fi) transport: one datagram = one packet.
// ---------------------------------------------------------------------------

class UdpSender {
public:
    // Resolves `host`, creates the socket and connect()s it (UDP connect only
    // pins the peer; no bytes are sent). On failure the sender is invalid,
    // errno is logged to stderr, and Send() returns false.
    UdpSender(const std::string& host, uint16_t port);
    ~UdpSender();
    UdpSender(const UdpSender&) = delete;
    UdpSender& operator=(const UdpSender&) = delete;

    bool Valid() const { return fd_ >= 0; }
    bool Send(const Packet& packet);  // one datagram per packet

    // Receives one datagram from the connected peer, waiting up to
    // timeout_ms (0 = pure poll). Returns true only for a well-formed
    // packet; false on timeout, malformed datagram, or error. This is how
    // the sender side hears the receiver's control traffic (pings, acks).
    bool Recv(Packet* out, int timeout_ms);

private:
    int fd_ = -1;
};

class UdpReceiver {
public:
    explicit UdpReceiver(uint16_t port);  // binds 127.0.0.1 (loopback)
    ~UdpReceiver();
    UdpReceiver(const UdpReceiver&) = delete;
    UdpReceiver& operator=(const UdpReceiver&) = delete;

    bool Bind();

    // Blocks up to timeout_ms (poll). Returns true only for a well-formed
    // packet (stats.received/seq fields updated). Returns false on timeout
    // AND on malformed datagrams — malformed datagrams are counted in
    // stats().malformed and dropped, never fatal.
    bool Recv(Packet* out, int timeout_ms);

    const PacketStats& stats() const { return stats_; }

private:
    int fd_ = -1;
    uint16_t port_ = 0;
    PacketStats stats_;
    SeqTracker seq_;
};

// ---------------------------------------------------------------------------
// TCP (USB via adb forward) transport: packets concatenated on a byte
// stream; 6-byte header, then exactly payload_len bytes. On a bad header the
// receiver scans forward up to 64 KiB for the next 0xA7 and resumes; if none
// is found the connection is a hard failure (false + stderr).
// ---------------------------------------------------------------------------

class TcpConn {
public:
    ~TcpConn();
    // Move leaves the source without a valid fd so its destructor cannot
    // close the transferred descriptor.
    TcpConn(TcpConn&& other) noexcept
        : fd_(other.fd_), eof_(other.eof_), stats_(std::move(other.stats_)),
          seq_(std::move(other.seq_)) {
        other.fd_ = -1;
    }
    TcpConn& operator=(TcpConn&& other) noexcept {
        if (this != &other) {
            if (fd_ >= 0) close(fd_);
            fd_ = other.fd_;
            other.fd_ = -1;
            eof_ = other.eof_;
            stats_ = std::move(other.stats_);
            seq_ = std::move(other.seq_);
        }
        return *this;
    }
    TcpConn(const TcpConn&) = delete;
    TcpConn& operator=(const TcpConn&) = delete;

    bool Send(const Packet& packet);
    bool Recv(Packet* out, int timeout_ms);
    const PacketStats& stats() const { return stats_; }
    bool Eof() const { return eof_; }   // peer closed the stream
    bool Valid() const { return fd_ >= 0; }  // connection established

private:
    friend class TcpServer;
    friend class TcpClient;
    explicit TcpConn(int fd);  // takes ownership of fd

    bool ReadExact(uint8_t* dst, size_t n, int timeout_ms);
    // Keeps bytes consumed from the stream by an aborted ReadExact so the
    // next call resumes mid-packet instead of losing them (TCP is a byte
    // stream: a poll timeout can land inside a packet header).
    void RetainPartial(const uint8_t* data, size_t n);

    int fd_ = -1;
    bool eof_ = false;
    // Bytes consumed from the socket but not yet consumed by a complete
    // ReadExact (bounded by one maximum packet: header + payload).
    std::vector<uint8_t> pend_;
    PacketStats stats_;
    SeqTracker seq_;
    // Scan forward (up to 64 KiB) for the next 0xA7 and rebuild the header
    // buffer: hdr_out[0] is the magic byte; any header bytes that were
    // already buffered in `already` follow it. *have_out receives how many
    // bytes after the magic are already in hdr_out; the caller reads the
    // remainder.
    bool Resync(uint8_t* hdr_out, size_t* have_out, const uint8_t* already,
                size_t already_n, int timeout_ms);
};

class TcpServer {
public:
    explicit TcpServer(uint16_t port);  // listens on 127.0.0.1 (loopback)
    ~TcpServer();
    TcpServer(const TcpServer&) = delete;
    TcpServer& operator=(const TcpServer&) = delete;

    bool Listen();
    // Blocks up to timeout_ms for an inbound connection. Returns nullptr on
    // timeout or accept error (logged).
    std::unique_ptr<TcpConn> Accept(int timeout_ms);

private:
    int fd_ = -1;
    uint16_t port_ = 0;
};

class TcpClient {
public:
    // Resolves `host`, connects (blocking). On failure the client is
    // invalid, errno is logged to stderr, and Send/Recv return false.
    TcpClient(const std::string& host, uint16_t port);
    ~TcpClient();
    TcpClient(const TcpClient&) = delete;
    TcpClient& operator=(const TcpClient&) = delete;

    bool Valid() const { return conn_.Valid(); }
    bool Send(const Packet& packet) { return conn_.Send(packet); }
    bool Recv(Packet* out, int timeout_ms) { return conn_.Recv(out, timeout_ms); }
    const PacketStats& stats() const { return conn_.stats(); }
    bool Eof() const { return conn_.Eof(); }

private:
    TcpConn conn_;
};

// ---------------------------------------------------------------------------
// FdBytePipe — BytePipe over an owned file descriptor: the Bluetooth bridge.
//
// Native owns no Bluetooth socket code (the NDK has none): Kotlin connects
// the RFCOMM (SPP) socket, creates a full-duplex local socket pair, and
// hands native one end's fd; a Kotlin pump coroutine forwards bytes between
// the other end and the socket in BOTH directions. Push() is the
// phone->desktop side — the same full-write loop as TcpConn::Send, so the
// wire bytes are identical to the TCP framing and the desktop's stream
// decoder works unchanged. Recv() is the desktop->phone side — the same
// 6-byte header rules as TcpConn::Recv (64 KiB bad-magic resync, partial
// reads retained across calls) — so the phone hears the desktop's control
// traffic (liveness pings, hello-ack) exactly like a TCP client would.
//
// Ownership: the fd is OWNED. The constructor takes it over and the
// destructor closes it exactly once — the JNI layer takes ownership of the
// fd handed over by nativeStartBt, and the Kotlin side must detachFd() that
// end before the call (it never closes it afterwards). A negative fd
// is accepted but inert: Valid() is false and Push/Recv fail immediately.
//
// SIGPIPE: socket fds take the send(MSG_NOSIGNAL) path; a non-socket fd
// (e.g. a host test pipe) falls back to write() permanently after the first
// ENOTSOCK. write() to a broken pipe is safe on Android because ART blocks
// SIGPIPE process-wide (writes return EPIPE instead of raising); host tests
// use socketpairs, which keep the MSG_NOSIGNAL path.
//
// Threading: Push/Recv target the pipeline's single network thread (same
// contract as BytePipe: one sender thread, or externally synchronized).
// ---------------------------------------------------------------------------
class FdBytePipe : public BytePipe {
public:
    // Takes ownership of fd. Out-of-line: sets a send timeout so a stalled
    // link becomes a reportable error instead of a blocked network thread.
    explicit FdBytePipe(int fd);
    ~FdBytePipe() override;

    FdBytePipe(const FdBytePipe&) = delete;
    FdBytePipe& operator=(const FdBytePipe&) = delete;

    bool Valid() const { return fd_ >= 0; }
    int fd() const { return fd_; }

    // Write-all loop; false on error or closed pipe (terminal for the link).
    bool Push(const uint8_t* data, size_t size) override;

    // read() passthrough; the raw seam the framed Recv() below is built on.
    ssize_t Pull(uint8_t* data, size_t max_size) override;

    // Receives one framed packet from the fd (the desktop->phone direction),
    // waiting up to timeout_ms (0 = pure poll). The SAME 6-byte header rules
    // as TcpConn::Recv: a poll timeout may land inside a packet, so bytes
    // consumed by an aborted read are retained in `pend_` and served first
    // on the next call; a bad magic byte scans forward up to 64 KiB for the
    // next 0xA7 (same resync as TCP); more than 4 consecutive bad headers
    // abandons the connection (false). Returns false on timeout, EOF (peer
    // closed the stream — Eof() then reports true), error, or invalid fd.
    bool Recv(Packet* out, int timeout_ms);

    // True once the desktop closed its end of the stream (clean EOF seen by
    // Recv). Terminal: Recv keeps returning false.
    bool Eof() const { return eof_; }
    const PacketStats& stats() const { return stats_; }

private:
    bool ReadExact(uint8_t* dst, size_t n, int timeout_ms);
    // Keeps bytes consumed from the stream by an aborted ReadExact so the
    // next call resumes mid-packet instead of losing them (a stream may
    // split a packet across the poll-timeout boundary).
    void RetainPartial(const uint8_t* data, size_t n);
    // Scan forward (up to 64 KiB) for the next 0xA7 and rebuild the header
    // buffer: hdr_out[0] is the magic byte; any header bytes that were
    // already buffered in `already` follow it. *have_out receives how many
    // bytes after the magic are already in hdr_out; the caller reads the
    // remainder.
    bool Resync(uint8_t* hdr_out, size_t* have_out, const uint8_t* already,
                size_t already_n, int timeout_ms);

    int fd_ = -1;
    bool socket_ = true;  // false once send() reports ENOTSOCK (pipe fd)
    bool eof_ = false;
    // Bytes consumed from the fd but not yet part of a complete packet
    // (bounded by one maximum packet: header + payload).
    std::vector<uint8_t> pend_;
    PacketStats stats_;
    SeqTracker seq_;
};

}  // namespace openay

#endif  // OPENAY_TRANSPORT_H
