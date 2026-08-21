package com.openay.mic

object NativeBridge {
    init { System.loadLibrary("openaymic") }
    /** transport: "udp"|"tcp"; codec: "pcm"|"opus"; frameMs: 5|10 */
    external fun nativeStart(transport: String, host: String, port: Int, codec: String, frameMs: Int): Boolean
    external fun nativeStop(): Boolean
    external fun nativeIsRunning(): Boolean
    /** JSON: {"running":bool,"transport":str,"codec":str,"frame_ms":int,"sharing":str,"sample_rate":int,"sent":int,"bytes":int,"ring_overruns":int,"encode_errors":int,"send_errors":int,"xruns":int,"last_error":str} */
    external fun nativeGetStats(): String
}
