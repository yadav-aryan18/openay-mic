// OpenAY Mic — capture engine facade (portable).
//
// Single-threaded control surface for the app's JNI layer:
//   Configure(transport, host, port, codec, frame_ms) -> Start -> Stop
//   StatsJson() emits the exact JSON object the Kotlin UI consumes.
//
// Platform wiring (one portable .cpp, no platform files):
//   __ANDROID__ : IAudioSource = OboeSource (real device capture)
//   otherwise   : IAudioSource = TestSource (host tests / tone-udp tool)
//
// All public methods are safe to call from multiple threads (mutex-guarded);
// none of them ever runs on the audio RT thread. StatsJson reads the
// pipeline's atomics, so it is cheap enough for the UI's polling loop.
#ifndef OPENAY_CAPTURE_ENGINE_H
#define OPENAY_CAPTURE_ENGINE_H

#include <memory>
#include <mutex>
#include <string>

#include "openay/capture_pipeline.h"

namespace openay {

class CaptureEngine {
public:
    CaptureEngine() = default;
    ~CaptureEngine();  // stops any running stream
    CaptureEngine(const CaptureEngine&) = delete;
    CaptureEngine& operator=(const CaptureEngine&) = delete;

    // Re-configures the engine (stopping a running stream first). Returns
    // false on any error; the reason is recorded and surfaces in StatsJson's
    // "last_error".
    bool Configure(TransportType transport, const std::string& host,
                   uint16_t port, CodecType codec, int frame_ms);
    bool Start();
    void Stop();
    bool IsRunning() const;

    // Exact hand-serialized JSON object (fixed vocabulary, nothing escaped):
    // {"running":true,"transport":"udp","host":"10.0.2.2","port":41700,
    //  "codec":"opus","frame_ms":10,"sharing":"exclusive","sample_rate":48000,
    //  "sent":1234,"bytes":1186560,"ring_overruns":0,"encode_errors":0,
    //  "send_errors":0,"xruns":0,"callback_us_p50":0,"last_error":"",
    //  "level_peak":40}
    std::string StatsJson() const;

    // Input level peak (0..32767) since the last call, consumed and reset;
    // 0 when not running. Note StatsJson() also consumes the peak, so each
    // poll interval is metered exactly once.
    uint16_t ExchangeLevelPeak();

private:
    void StopLocked();

    mutable std::mutex mu_;
    std::unique_ptr<CapturePipeline> pipeline_;  // may exist even if configure failed
    std::unique_ptr<IAudioSource> source_;
    bool running_ = false;  // guarded by mu_
    TransportType transport_ = TransportType::Udp;
    std::string host_;
    uint16_t port_ = 0;
    CodecType codec_ = CodecType::Pcm;
    int frame_ms_ = 10;
    std::string sharing_ = "exclusive";  // updated from the live source
};

}  // namespace openay

#endif  // OPENAY_CAPTURE_ENGINE_H
