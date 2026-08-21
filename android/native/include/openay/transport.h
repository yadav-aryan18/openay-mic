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
// Reserved as the extension point for transports that cannot be expressed as
// a POSIX socket created by this library — most importantly the Bluetooth
// RFCOMM (SPP) bridge that Phase 3 will wire from Kotlin through JNI on
// Android. RFCOMM is a byte stream with the same framing/resync rules as TCP,
// so a future BytePipe implementation can reuse TcpConn's framing logic once
// a JNI file descriptor is handed over; nothing in this file wires it yet.
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
    // Scan forward (up to 64 KiB) for the next 0xA7 and rebuild the header
    // buffer: hdr_out[0] is the magic byte; any header bytes that were
    // already buffered in `already` follow it. *have_out receives how many
    // bytes after the magic are already in hdr_out; the caller reads the
    // remainder.
    bool Resync(uint8_t* hdr_out, size_t* have_out, const uint8_t* already,
                size_t already_n, int timeout_ms);

    int fd_ = -1;
    bool eof_ = false;
    PacketStats stats_;
    SeqTracker seq_;
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

}  // namespace openay

#endif  // OPENAY_TRANSPORT_H
