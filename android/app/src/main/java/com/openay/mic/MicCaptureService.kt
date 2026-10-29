package com.openay.mic

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioManager
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.NoiseSuppressor
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import com.openay.mic.logic.CallMute
import org.json.JSONObject

/**
 * Foreground service driving the native capture engine (NativeBridge).
 *
 * Started with [captureIntent]; an explicit [ACTION_STOP] intent stops the
 * native session and the service. While running, a lightweight coroutine
 * refreshes the notification with live counters every 2 s.
 *
 * Transport "bt" routes through [BluetoothLink]: the RFCOMM socket lives in
 * Kotlin (the NDK has no BT socket APIs), the service connects + pumps, and
 * native only sees the pipe's write fd via nativeStartBt.
 *
 * Session-scoped extras: [EXTRA_VOICE_FX] attaches platform echo
 * cancellation / noise suppression to the capture session; [EXTRA_MUTE_ON_CALL]
 * mutes the stream whenever the device enters a call/communication audio
 * mode (silence keeps streaming so timing and liveness continue). UDP
 * capture holds a high-performance Wi-Fi lock so the radio does not doze at
 * screen-off.
 */
class MicCaptureService : Service() {

    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    // statsJob/btJob/lastXruns are written from the main thread AND from the
    // BT connect/pump continuations on Dispatchers.IO: @Volatile gives the
    // cross-thread visibility (all mutations are single-assignment style, no
    // read-modify-write).
    @Volatile private var statsJob: Job? = null
    // Previous session's xrun count; a refresh seeing a higher value logs a
    // warning (increase tracking only — the notification text is untouched).
    @Volatile private var lastXruns = 0
    // Bluetooth link + its connect/pump coroutine (transport "bt" only).
    // btLink is touched from the main thread and the IO pump/coroutines.
    @Volatile private var btLink: BluetoothLink? = null
    @Volatile private var btJob: Job? = null

    // Voice processing effects bound to the native capture session; created
    // per session, released on stop.
    private var echoCanceler: AcousticEchoCanceler? = null
    private var noiseSuppressor: NoiseSuppressor? = null
    // Held while UDP capture runs; keeps the Wi-Fi radio at high perf.
    private var wifiLock: WifiManager.WifiLock? = null
    // Call/communication mode watcher that drives the native mute flag.
    // Polled (not listened): the mode-change listener interface does not
    // exist on every supported OS level; getMode() does.
    private var modePollJob: Job? = null
    private var muteOnCall = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent == null || intent.action == ACTION_STOP) {
            Log.i(TAG, "Stop requested (action=${intent?.action ?: "null"})")
            stopCaptureAndSelf()
            return START_NOT_STICKY
        }

        val transport = intent.getStringExtra(EXTRA_TRANSPORT) ?: DEFAULT_TRANSPORT
        val host = intent.getStringExtra(EXTRA_HOST).orEmpty()
        val port = intent.getIntExtra(EXTRA_PORT, -1)
        val codec = intent.getStringExtra(EXTRA_CODEC) ?: DEFAULT_CODEC
        val frameMs = intent.getIntExtra(EXTRA_FRAME_MS, DEFAULT_FRAME_MS)
        val device = intent.getStringExtra(EXTRA_DEVICE).orEmpty()
        val voiceFx = intent.getBooleanExtra(EXTRA_VOICE_FX, false)
        muteOnCall = intent.getBooleanExtra(EXTRA_MUTE_ON_CALL, false)

        if (transport == TRANSPORT_BT) {
            if (device.isBlank()) {
                Log.e(TAG, "BT transport without a device address — refusing to start")
                stopSelf()
                return START_NOT_STICKY
            }
        } else if (port <= 0 || port > 65535 || host.isBlank()) {
            Log.e(TAG, "Invalid capture parameters (host='$host', port=$port) — refusing to start")
            stopSelf()
            return START_NOT_STICKY
        }

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
            != PackageManager.PERMISSION_GRANTED
        ) {
            Log.e(TAG, "RECORD_AUDIO permission missing — refusing to capture")
            stopSelf()
            return START_NOT_STICKY
        }

        // Tolerate being started while already running: tear the old session
        // down completely first (watcher, effects, lock are session-scoped —
        // a restart with different extras must not inherit them).
        if (nativeRunning()) {
            Log.i(TAG, "Capture already running — restarting with new parameters")
            stopModeWatcher()
            releaseVoiceEffects()
            releaseWifiLock()
            nativeStop()
        }

        ensureNotificationChannel()

        // Promote to foreground BEFORE opening the mic: Android 12+ rejects
        // startForeground() when the calling app is not in a foreground state
        // (e.g. service started by adb or a system restart), and the
        // microphone FGS type must be active before capture begins. A
        // rejection is logged and answered with a clean stop — never a crash.
        val target = if (transport == TRANSPORT_BT) device else "$host:$port"
        try {
            ServiceCompat.startForeground(
                this,
                NOTIFICATION_ID,
                buildNotification("OpenAY Mic · $transport/$codec → $target"),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            )
        } catch (e: Exception) {
            Log.e(TAG, "startForeground rejected — app not in foreground state", e)
            stopSelf()
            return START_NOT_STICKY
        }

        if (transport == TRANSPORT_BT) {
            startBtCapture(device, codec, frameMs, voiceFx)
            return START_NOT_STICKY
        }

        // Switching BT -> wired: the previous BT link (socket + pump) must be
        // torn down explicitly. The nativeStop() above already closed the
        // pipe's write fd, so the pump has EOF-exited and this is quiet —
        // without it the idle RFCOMM socket would survive, leaking a fd and
        // leaving the desktop console showing a stale BT device.
        if (transport != TRANSPORT_BT) stopBtLink()

        val started = nativeStart(transport, host, port, codec, frameMs, voiceFx)
        if (!started) {
            val lastError = lastErrorFromStats()
            Log.e(TAG, "nativeStart($transport, $host, $port, $codec, ${frameMs}ms) failed: $lastError")
            // Foreground promotion already happened, so surface the failure
            // with a plain notification update and stop.
            notifyFailure("start failed: $lastError")
            stopSelf()
            return START_NOT_STICKY
        }

        // Fresh xrun baseline for the new session: the stats refresh only
        // warns about increases that happen while this session runs.
        lastXruns = 0

        if (voiceFx) attachVoiceEffects()
        if (muteOnCall) startModeWatcher()
        if (transport == DEFAULT_TRANSPORT) acquireWifiLock()

        statsJob = serviceScope.launch {
            while (isActive) {
                delay(STATS_INTERVAL_MS)
                updateStatsNotification()
            }
        }

        return START_NOT_STICKY
    }

    /**
     * BT transport: connect the RFCOMM socket, create the pipe, start native
     * with the write fd, then pump pipe -> socket. Runs on Dispatchers.IO —
     * a cold RFCOMM connect can take seconds and must not block the main
     * thread (the wired transports connect inside nativeStart instead).
     */
    private fun startBtCapture(device: String, codec: String, frameMs: Int, voiceFx: Boolean) {
        stopBtLink()  // tear down any in-flight/previous link before the new one
        val link = BluetoothLink()
        btLink = link
        btJob = serviceScope.launch(Dispatchers.IO) {
            val fd = try {
                link.connect(applicationContext, device)
            } catch (e: BluetoothLinkException) {
                Log.w(TAG, "BT connect to $device failed: ${e.message}")
                if (btLink === link) {
                    // Close first: the link may hold a connected socket even
                    // after connect() threw (see BluetoothLink.connect()).
                    link.close()
                    btLink = null
                    notifyFailure("BT connect failed: ${e.message}")
                    stopSelf()
                }
                return@launch
            }
            if (btLink !== link) {
                // Superseded while connecting; native never saw the fd and
                // we are its only owner in this window.
                ParcelFileDescriptor.adoptFd(fd).close()
                return@launch
            }
            // Ownership: from nativeStartBt on, the fd belongs to native
            // (closed by the engine sink). Only an exception BEFORE that
            // call leaves it ours to close.
            val started = try {
                NativeBridge.nativeStartBt(fd, device, codec, frameMs, voiceFx)
            } catch (e: UnsatisfiedLinkError) {
                Log.e(TAG, "Native library unavailable", e)
                ParcelFileDescriptor.adoptFd(fd).close()
                if (btLink === link) {
                    // Native never took the fd; the link's socket + pipe read
                    // end still need closing (write fd handled above).
                    link.close()
                    btLink = null
                    stopSelf()
                }
                return@launch
            }
            if (!started) {
                // Native consumed (closed) the fd even on failure; the
                // reason is readable via StatsJson. The link's socket and
                // the pipe read end are still ours to close.
                val lastError = lastErrorFromStats()
                Log.e(TAG, "nativeStartBt($device, $codec, ${frameMs}ms) failed: $lastError")
                if (btLink === link) {
                    link.close()
                    btLink = null
                    notifyFailure("start failed: $lastError")
                    stopSelf()
                }
                return@launch
            }

            // Fresh xrun baseline for the new session (same as the wired path).
            lastXruns = 0

            // Pump failure (socket drop / IO error): stop capture cleanly —
            // native records send errors once the pipe write side starts
            // failing, and the stats path surfaces them.
            link.startPump(serviceScope) { reason ->
                if (btLink === link) {
                    Log.w(TAG, "BT link down: $reason — stopping capture")
                    stopBtSession("BT link down: $reason")
                }
            }

            statsJob = serviceScope.launch {
                while (isActive) {
                    delay(STATS_INTERVAL_MS)
                    updateStatsNotification()
                }
            }
        }
    }

    /**
     * Attaches platform echo cancellation / noise suppression to the native
     * capture session. Best effort per effect: an unavailable or refused
     * effect is logged and skipped (the stream runs unprocessed).
     */
    private fun attachVoiceEffects() {
        val sessionId = try {
            NativeBridge.nativeGetAudioSessionId()
        } catch (_: UnsatisfiedLinkError) {
            0
        }
        if (sessionId <= 0) {
            Log.w(TAG, "voice FX requested but no audio session id available (API < 28?)")
            return
        }
        if (AcousticEchoCanceler.isAvailable()) {
            AcousticEchoCanceler.create(sessionId)?.let { aec ->
                if (!aec.enabled) aec.enabled = true
                echoCanceler = aec
                Log.i(TAG, "acoustic echo cancellation enabled (session $sessionId)")
            }
        }
        if (NoiseSuppressor.isAvailable()) {
            NoiseSuppressor.create(sessionId)?.let { ns ->
                if (!ns.enabled) ns.enabled = true
                noiseSuppressor = ns
                Log.i(TAG, "noise suppressor enabled (session $sessionId)")
            }
        }
    }

    /** BT link failure path: stop native, surface the reason, stop the service. */
    private fun stopBtSession(reason: String) {
        statsJob?.cancel()
        statsJob = null
        lastXruns = 0
        // Native first: the engine closes the pipe's write fd, which ends the
        // pump (EOF) before the socket is torn down.
        if (nativeRunning()) nativeStop()
        stopBtLink()
        notifyFailure(reason)
        stopSelf()
    }

    /** Tears down the BT link (socket + pump); native is untouched here. */
    private fun stopBtLink() {
        btLink?.close()
        btLink = null
        btJob?.cancel()
        btJob = null
    }

    /**
     * Polls the audio mode while capturing and drives the native mute flag
     * on every change. The first iteration applies the current mode, so a
     * capture started during an ongoing call is muted immediately.
     */
    private fun startModeWatcher() {
        if (modePollJob != null) return
        val am = getSystemService(AudioManager::class.java)
        if (am == null) {
            Log.w(TAG, "no AudioManager — call auto-mute unavailable")
            return
        }
        modePollJob = serviceScope.launch {
            var lastMode = Int.MIN_VALUE
            while (isActive) {
                val mode = am.mode
                if (mode != lastMode) {
                    lastMode = mode
                    val shouldMute = CallMute.shouldMuteForCall(mode)
                    try {
                        NativeBridge.nativeSetMuted(shouldMute)
                    } catch (_: UnsatisfiedLinkError) {
                        // Native side gone; the stop path cleans the rest up.
                    }
                    Log.d(TAG, "audio mode $mode -> muted=$shouldMute")
                }
                delay(CallMute.POLL_INTERVAL_MS)
            }
        }
    }

    private fun stopModeWatcher() {
        modePollJob?.cancel()
        modePollJob = null
        try {
            NativeBridge.nativeSetMuted(false)
        } catch (_: UnsatisfiedLinkError) {
        }
    }

    private fun acquireWifiLock() {
        if (wifiLock?.isHeld == true) return
        val wm = applicationContext.getSystemService(WifiManager::class.java) ?: return
        val mode = if (Build.VERSION.SDK_INT >= 29) {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } else {
            @Suppress("DEPRECATION")
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        wifiLock = wm.createWifiLock(mode, "openay_mic_wifi").apply {
            setReferenceCounted(false)
            acquire()
        }
        Log.i(TAG, "Wi-Fi lock acquired (mode=$mode)")
    }

    private fun releaseWifiLock() {
        wifiLock?.let { lock ->
            if (lock.isHeld) lock.release()
            Log.i(TAG, "Wi-Fi lock released")
        }
        wifiLock = null
    }

    private fun releaseVoiceEffects() {
        echoCanceler?.let { if (it.enabled) it.enabled = false }
        echoCanceler?.release()
        echoCanceler = null
        noiseSuppressor?.let { if (it.enabled) it.enabled = false }
        noiseSuppressor?.release()
        noiseSuppressor = null
    }

    /** Refreshes the notification with sent-packets / ring overruns / xruns. */
    private fun updateStatsNotification() {
        try {
            val json = JSONObject(NativeBridge.nativeGetStats())
            val sent = json.optInt("sent", 0)
            val overruns = json.optInt("ring_overruns", 0)
            val xruns = json.optInt("xruns", 0)
            if (xruns > lastXruns) {
                Log.w(
                    TAG,
                    "xruns increased: $lastXruns -> $xruns (+${xruns - lastXruns}) sent=$sent"
                )
            }
            lastXruns = xruns
            val lastError = json.optString("last_error")
            val text = buildString {
                append("sent=$sent · overruns=$overruns · xruns=$xruns")
                if (json.optString("peer_link") == "lost") append(" · LINK LOST")
                if (lastError.isNotBlank()) append(" · error: $lastError")
            }
            try {
                NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, buildNotification(text))
            } catch (e: SecurityException) {
                Log.w(TAG, "POST_NOTIFICATIONS not granted — notification not updated")
            }
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Native library unavailable while refreshing stats", e)
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse stats: ${e.message}")
        }
    }

    private fun lastErrorFromStats(): String = try {
        JSONObject(NativeBridge.nativeGetStats()).optString("last_error").ifBlank { "unknown error" }
    } catch (e: Exception) {
        "unknown error (${e.message})"
    }

    /** Plain notification update used to surface start/link failures. */
    private fun notifyFailure(text: String) {
        try {
            NotificationManagerCompat.from(this).notify(
                NOTIFICATION_ID,
                buildNotification("OpenAY Mic · $text")
            )
        } catch (e: SecurityException) {
            Log.w(TAG, "POST_NOTIFICATIONS not granted — failure not shown")
        }
    }

    private fun nativeStart(
        transport: String,
        host: String,
        port: Int,
        codec: String,
        frameMs: Int,
        sessionForEffects: Boolean
    ): Boolean = try {
        NativeBridge.nativeStart(transport, host, port, codec, frameMs, sessionForEffects)
    } catch (e: UnsatisfiedLinkError) {
        Log.e(TAG, "Native library unavailable", e)
        false
    }

    private fun nativeRunning(): Boolean = try {
        NativeBridge.nativeIsRunning()
    } catch (e: UnsatisfiedLinkError) {
        Log.e(TAG, "Native library unavailable", e)
        false
    }

    private fun nativeStop() {
        try {
            NativeBridge.nativeStop()
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "Native library unavailable", e)
        }
    }

    private fun stopCaptureAndSelf() {
        statsJob?.cancel()
        statsJob = null
        lastXruns = 0
        stopModeWatcher()
        releaseVoiceEffects()
        releaseWifiLock()
        // Native first: for a BT session the engine closes the pipe's write
        // fd, which ends the pump (EOF) before the socket is torn down.
        if (nativeRunning()) nativeStop()
        stopBtLink()
        stopSelf()
    }

    private fun ensureNotificationChannel() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Capture status", NotificationManager.IMPORTANCE_LOW).apply {
                description = "OpenAY Mic capture progress"
            }
        )
    }

    private fun buildNotification(text: String): Notification {
        val stopIntent = Intent(this, MicCaptureService::class.java).setAction(ACTION_STOP)
        val stopPendingIntent = PendingIntent.getService(
            this,
            0,
            stopIntent,
            PendingIntent.FLAG_IMMUTABLE
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification_mic)
            .setContentTitle("OpenAY Mic")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .addAction(R.drawable.ic_notification_mic, "Stop", stopPendingIntent)
            .build()
    }

    override fun onDestroy() {
        // Tear down in order: stop the UI refresh loop, cancel the scope,
        // release session resources, stop the native session, tear down the
        // BT link (after native: the engine closing the pipe's write fd ends
        // the pump first), and only then notify the framework.
        statsJob?.cancel()
        statsJob = null
        lastXruns = 0
        stopModeWatcher()
        releaseVoiceEffects()
        releaseWifiLock()
        serviceScope.cancel()
        if (nativeRunning()) nativeStop()
        stopBtLink()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "MicCaptureService"
        private const val CHANNEL_ID = "openay_capture"
        private const val NOTIFICATION_ID = 41
        private const val STATS_INTERVAL_MS = 2_000L

        private const val DEFAULT_TRANSPORT = "udp"
        private const val DEFAULT_CODEC = "pcm"
        private const val DEFAULT_FRAME_MS = 10

        const val ACTION_STOP = "com.openay.mic.action.STOP"

        /** Transport value for the Bluetooth RFCOMM (SPP) link. */
        const val TRANSPORT_BT = "bt"

        private const val EXTRA_TRANSPORT = "transport"
        private const val EXTRA_HOST = "host"
        private const val EXTRA_PORT = "port"
        private const val EXTRA_CODEC = "codec"
        private const val EXTRA_FRAME_MS = "frame_ms"
        private const val EXTRA_DEVICE = "device"
        private const val EXTRA_VOICE_FX = "voice_fx"
        private const val EXTRA_MUTE_ON_CALL = "mute_on_call"

        /**
         * Builds the intent that starts capture with the given parameters.
         * `device` (bonded MAC address) is only read for transport "bt",
         * which ignores host/port.
         */
        fun captureIntent(
            context: Context,
            transport: String,
            host: String,
            port: Int,
            codec: String,
            frameMs: Int,
            voiceFx: Boolean,
            muteOnCall: Boolean,
            device: String = ""
        ): Intent = Intent(context, MicCaptureService::class.java).apply {
            putExtra(EXTRA_TRANSPORT, transport)
            putExtra(EXTRA_HOST, host)
            putExtra(EXTRA_PORT, port)
            putExtra(EXTRA_CODEC, codec)
            putExtra(EXTRA_FRAME_MS, frameMs)
            putExtra(EXTRA_VOICE_FX, voiceFx)
            putExtra(EXTRA_MUTE_ON_CALL, muteOnCall)
            putExtra(EXTRA_DEVICE, device)
        }
    }
}
