// OpenAY Mic — JNI bridge (Android only; excluded from host builds).
//
// Exposes the capture engine to Kotlin's com.openay.mic.NativeBridge:
//   nativeStart(String transport, String host, int port, String codec, int frameMs)
//   nativeStop() / nativeIsRunning() / nativeGetStats()
//
// The engine is a process-wide singleton guarded by one std::mutex. All calls
// arrive from the app's service thread (never from the audio RT thread), so
// taking the mutex here is cheap and safe.
#include <jni.h>

#include <mutex>
#include <string>

#include "openay/capture_engine.h"

#include <android/log.h>

namespace {

constexpr const char* kTag = "openaymic";

std::mutex g_engine_mu;
openay::CaptureEngine* g_engine = nullptr;

std::string JString(JNIEnv* env, jstring s) {
    if (!s) return {};
    const char* utf = env->GetStringUTFChars(s, nullptr);
    if (!utf) return {};
    std::string out(utf);
    env->ReleaseStringUTFChars(s, utf);
    return out;
}

}  // namespace

extern "C" {

JNIEXPORT jboolean JNICALL
Java_com_openay_mic_NativeBridge_nativeStart(JNIEnv* env, jobject,
                                             jstring jtransport, jstring jhost,
                                             jint port, jstring jcodec,
                                             jint frame_ms) {
    std::lock_guard<std::mutex> lk(g_engine_mu);
    if (!g_engine) g_engine = new openay::CaptureEngine();

    const std::string transport = JString(env, jtransport);
    const std::string host = JString(env, jhost);
    const std::string codec = JString(env, jcodec);

    openay::TransportType tt;
    if (transport == "udp") {
        tt = openay::TransportType::Udp;
    } else if (transport == "tcp") {
        tt = openay::TransportType::Tcp;
    } else {
        __android_log_print(ANDROID_LOG_ERROR, kTag,
                            "nativeStart: unknown transport '%s'", transport.c_str());
        return JNI_FALSE;
    }

    openay::CodecType ct;
    if (codec == "pcm") {
        ct = openay::CodecType::Pcm;
    } else if (codec == "opus") {
        ct = openay::CodecType::Opus;
    } else {
        __android_log_print(ANDROID_LOG_ERROR, kTag,
                            "nativeStart: unknown codec '%s'", codec.c_str());
        return JNI_FALSE;
    }

    // A running stream is replaced: Start() is an explicit new configuration.
    if (g_engine->IsRunning()) g_engine->Stop();

    if (!g_engine->Configure(tt, host, static_cast<uint16_t>(port), ct,
                             static_cast<int>(frame_ms))) {
        // Reason is recorded in the engine; the app reads it via StatsJson.
        __android_log_print(ANDROID_LOG_ERROR, kTag,
                            "nativeStart: configure failed (%s:%d, %s, %d ms)",
                            host.c_str(), static_cast<int>(port), codec.c_str(),
                            static_cast<int>(frame_ms));
        return JNI_FALSE;
    }
    const bool ok = g_engine->Start();
    if (!ok) {
        __android_log_print(ANDROID_LOG_ERROR, kTag, "nativeStart: start failed");
    }
    return ok ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT jboolean JNICALL
Java_com_openay_mic_NativeBridge_nativeStop(JNIEnv*, jobject) {
    std::lock_guard<std::mutex> lk(g_engine_mu);
    if (!g_engine) return JNI_FALSE;
    g_engine->Stop();
    return JNI_TRUE;
}

JNIEXPORT jboolean JNICALL
Java_com_openay_mic_NativeBridge_nativeIsRunning(JNIEnv*, jobject) {
    std::lock_guard<std::mutex> lk(g_engine_mu);
    return g_engine && g_engine->IsRunning() ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT jstring JNICALL
Java_com_openay_mic_NativeBridge_nativeGetStats(JNIEnv* env, jobject) {
    std::lock_guard<std::mutex> lk(g_engine_mu);
    if (!g_engine) g_engine = new openay::CaptureEngine();
    const std::string json = g_engine->StatsJson();
    return env->NewStringUTF(json.c_str());
}

}  // extern "C"
