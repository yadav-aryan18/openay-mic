package com.openay.mic

object NativeBridge {
    init { System.loadLibrary("openaymic") }

    /**
     * Starts the capture engine.
     *
     * @param transport "udp"|"tcp"|"bt" (bt goes through [nativeStartBt])
     * @param codec     "pcm"|"opus"
     * @param frameMs   5|10
     * @param sessionForEffects allocate an audio session id for the capture
     *   stream (API 28+) so [nativeGetAudioSessionId] can return it and the
     *   app can attach echo-cancellation/noise-suppression effects.
     */
    external fun nativeStart(
        transport: String,
        host: String,
        port: Int,
        codec: String,
        frameMs: Int,
        sessionForEffects: Boolean
    ): Boolean

    /**
     * transport "bt": fd is the WRITE end of a pipe Kotlin created — ownership
     * passes to native on entry (native closes it exactly once, on stop /
     * re-configure / teardown; Kotlin must detachFd() before the call and
     * never close it afterwards). deviceAddress is display-only (StatsJson
     * "host"); codec/frameMs as nativeStart. sessionForEffects as nativeStart
     * (the BT capture stream gets the same effects handling).
     */
    external fun nativeStartBt(
        fd: Int,
        deviceAddress: String,
        codec: String,
        frameMs: Int,
        sessionForEffects: Boolean
    ): Boolean
    external fun nativeStop(): Boolean
    external fun nativeIsRunning(): Boolean
    /** Mutes the capture: digital silence keeps streaming (timing + control live). */
    external fun nativeSetMuted(muted: Boolean)
    /** Audio session id of the live capture stream (0 = none/unsupported). */
    external fun nativeGetAudioSessionId(): Int
    /** JSON: {"running":bool,"transport":str,"codec":str,"frame_ms":int,"sharing":str,"sample_rate":int,"sent":int,"bytes":int,"ring_overruns":int,"encode_errors":int,"send_errors":int,"xruns":int,"last_error":str,"level_peak":int,"peer_link":"unknown|alive|lost","muted":bool,"audio_session":int} */
    external fun nativeGetStats(): String
}
