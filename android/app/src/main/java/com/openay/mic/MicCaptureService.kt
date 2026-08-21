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
import android.os.Build
import android.os.IBinder
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
import org.json.JSONObject

/**
 * Foreground service driving the native capture engine (NativeBridge).
 *
 * Started with [captureIntent]; an explicit [ACTION_STOP] intent stops the
 * native session and the service. While running, a lightweight coroutine
 * refreshes the notification with live counters every 2 s.
 */
class MicCaptureService : Service() {

    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var statsJob: Job? = null

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

        if (port <= 0 || port > 65535 || host.isBlank()) {
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

        // Tolerate being started while already running: stop the old session first.
        if (nativeRunning()) {
            Log.i(TAG, "Capture already running — restarting with new parameters")
            nativeStop()
        }

        ensureNotificationChannel()

        // Promote to foreground BEFORE opening the mic: Android 12+ rejects
        // startForeground() when the calling app is not in a foreground state
        // (e.g. service started by adb or a system restart), and the
        // microphone FGS type must be active before capture begins. A
        // rejection is logged and answered with a clean stop — never a crash.
        try {
            ServiceCompat.startForeground(
                this,
                NOTIFICATION_ID,
                buildNotification("OpenAY Mic · $transport/$codec → $host:$port"),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            )
        } catch (e: Exception) {
            Log.e(TAG, "startForeground rejected — app not in foreground state", e)
            stopSelf()
            return START_NOT_STICKY
        }

        val started = nativeStart(transport, host, port, codec, frameMs)
        if (!started) {
            val lastError = lastErrorFromStats()
            Log.e(TAG, "nativeStart($transport, $host, $port, $codec, ${frameMs}ms) failed: $lastError")
            // Foreground promotion already happened, so surface the failure
            // with a plain notification update and stop.
            try {
                NotificationManagerCompat.from(this).notify(
                    NOTIFICATION_ID,
                    buildNotification("OpenAY Mic · start failed: $lastError")
                )
            } catch (e: SecurityException) {
                Log.w(TAG, "POST_NOTIFICATIONS not granted — failure not shown")
            }
            stopSelf()
            return START_NOT_STICKY
        }

        statsJob = serviceScope.launch {
            while (isActive) {
                delay(STATS_INTERVAL_MS)
                updateStatsNotification()
            }
        }

        return START_NOT_STICKY
    }

    /** Refreshes the notification with sent-packets / ring overruns / xruns. */
    private fun updateStatsNotification() {
        try {
            val json = JSONObject(NativeBridge.nativeGetStats())
            val sent = json.optInt("sent", 0)
            val overruns = json.optInt("ring_overruns", 0)
            val xruns = json.optInt("xruns", 0)
            val lastError = json.optString("last_error")
            val text = buildString {
                append("sent=$sent · overruns=$overruns · xruns=$xruns")
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

    private fun nativeStart(transport: String, host: String, port: Int, codec: String, frameMs: Int): Boolean = try {
        NativeBridge.nativeStart(transport, host, port, codec, frameMs)
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
        if (nativeRunning()) nativeStop()
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
        // stop the native session, and only then notify the framework.
        statsJob?.cancel()
        statsJob = null
        serviceScope.cancel()
        if (nativeRunning()) nativeStop()
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

        private const val EXTRA_TRANSPORT = "transport"
        private const val EXTRA_HOST = "host"
        private const val EXTRA_PORT = "port"
        private const val EXTRA_CODEC = "codec"
        private const val EXTRA_FRAME_MS = "frame_ms"

        /** Builds the intent that starts capture with the given parameters. */
        fun captureIntent(
            context: Context,
            transport: String,
            host: String,
            port: Int,
            codec: String,
            frameMs: Int
        ): Intent = Intent(context, MicCaptureService::class.java).apply {
            putExtra(EXTRA_TRANSPORT, transport)
            putExtra(EXTRA_HOST, host)
            putExtra(EXTRA_PORT, port)
            putExtra(EXTRA_CODEC, codec)
            putExtra(EXTRA_FRAME_MS, frameMs)
        }
    }
}
