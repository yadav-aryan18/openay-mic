// Internal logging helpers shared by the transport implementations.
#ifndef OPENAY_INTERNAL_LOG_H
#define OPENAY_INTERNAL_LOG_H

#include <cerrno>
#include <cstdio>
#include <cstring>

namespace openay {
namespace detail {

inline void LogErrno(const char* what) {
    fprintf(stderr, "openay: %s: %s\n", what, strerror(errno));
}

inline void Log(const char* msg) { fprintf(stderr, "openay: %s\n", msg); }

}  // namespace detail
}  // namespace openay

#endif  // OPENAY_INTERNAL_LOG_H
