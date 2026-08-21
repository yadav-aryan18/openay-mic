package com.openay.mic

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import android.os.SystemClock
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Snackbar
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import com.openay.mic.logic.LinkDelta
import com.openay.mic.logic.linkDelta
import com.openay.mic.logic.smoothLevel
import com.openay.mic.ui.ChainCard
import com.openay.mic.ui.ChipOption
import com.openay.mic.ui.ChipRow
import com.openay.mic.ui.ErrorPanel
import com.openay.mic.ui.LampDot
import com.openay.mic.ui.NetworkCard
import com.openay.mic.ui.OnAirToggle
import com.openay.mic.ui.SettingsRow
import com.openay.mic.ui.TransportSegments
import com.openay.mic.ui.theme.Cream
import com.openay.mic.ui.theme.Dim
import com.openay.mic.ui.theme.OpenAyDataStyles
import com.openay.mic.ui.theme.OpenAyTheme
import com.openay.mic.ui.theme.PanelRaised
import com.openay.mic.ui.theme.WordmarkStyle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.json.JSONObject

/**
 * Poll cadence for native stats. Each read consumes its own metering
 * interval, so 120–160 ms keeps the level ring smooth (140 ms chosen).
 */
private const val POLL_INTERVAL_MS = 140L

/**
 * Exponential smoothing constant for the level ring — ~100 ms time constant
 * at 140 ms polls (1 - exp(-140/100) ≈ 0.75).
 */
private const val LEVEL_SMOOTH_ALPHA = 0.75f

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        // Rack background from the very first frame (no light flash pre-Compose).
        window.setBackgroundDrawable(ColorDrawable(0xFF14120F.toInt()))
        setContent {
            OpenAyTheme {
                CaptureScreen()
            }
        }
    }
}

/**
 * The single scrollable rack screen: wordmark header, THE CHAIN hero, ON AIR
 * toggle, transport/codec/frame controls, network card. All live state comes
 * from the 140 ms native stats poll; config persists in SharedPreferences.
 */
@Composable
private fun CaptureScreen() {
    val context = LocalContext.current
    val snackbarHostState = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val prefs = remember { AppPrefs(context) }

    // --- persisted configuration ---
    var transport by remember { mutableStateOf(prefs.transport) }
    var host by remember { mutableStateOf(prefs.host) }
    var portText by remember { mutableStateOf(prefs.port) }
    var codec by remember { mutableStateOf(prefs.codec) }
    var frameMs by remember { mutableStateOf(prefs.frameMs) }

    // --- live state ---
    var running by remember { mutableStateOf(false) }
    var stats by remember { mutableStateOf<MicStats?>(null) }
    var nativeError by remember { mutableStateOf<String?>(null) }
    var lastError by remember { mutableStateOf<String?>(null) }
    var link by remember { mutableStateOf(LinkDelta.IDLE) }
    var level by remember { mutableFloatStateOf(0f) }
    var uptimeSeconds by remember { mutableLongStateOf(0L) }
    var sessionStartElapsed by remember { mutableLongStateOf(0L) }

    val reducedMotion = rememberReducedMotion()

    // ---------- capture lifecycle ----------

    fun showSnackbar(message: String) {
        scope.launch { snackbarHostState.showSnackbar(message) }
    }

    fun startCapture() {
        if (nativeError != null) return
        val port = portText.trim().toIntOrNull()
        if (port == null || port !in 1..65535) {
            showSnackbar("Port must be 1–65535")
            return
        }
        if (host.isBlank()) {
            showSnackbar("Host must not be empty")
            return
        }
        val intent = MicCaptureService.captureIntent(context, transport, host, port, codec, frameMs)
        ContextCompat.startForegroundService(context, intent)
    }

    fun stopCapture() {
        context.stopService(Intent(context, MicCaptureService::class.java))
        running = false
    }

    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) {
            startCapture()
        } else {
            showSnackbar("Microphone permission denied — capture not started")
        }
    }

    fun onTogglePress() {
        when {
            running -> stopCapture()
            nativeError != null -> showSnackbar("Native library unavailable — cannot start")
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO)
                == PackageManager.PERMISSION_GRANTED -> startCapture()
            else -> permissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    // ---------- native stats polling (self-recovers via nativeIsRunning) ----------

    LaunchedEffect(Unit) {
        var prevSent = 0L
        var prevSendErr = 0L
        var prevAt = 0L
        while (true) {
            try {
                val alive = NativeBridge.nativeIsRunning()
                if (alive && !running) {
                    sessionStartElapsed = SystemClock.elapsedRealtime()
                }
                running = alive
                if (alive) {
                    val s = parseMicStats(NativeBridge.nativeGetStats())
                    if (s != null) {
                        stats = s
                        lastError = s.lastError.takeIf { it.isNotBlank() }
                        val now = SystemClock.elapsedRealtime()
                        if (prevAt > 0L) {
                            link = linkDelta(prevSent, prevSendErr, s.sent, s.sendErrors, now - prevAt)
                        }
                        prevSent = s.sent
                        prevSendErr = s.sendErrors
                        prevAt = now
                        level = smoothLevel(level, s.levelPeak.toFloat(), LEVEL_SMOOTH_ALPHA)
                        uptimeSeconds = (now - sessionStartElapsed) / 1000L
                    }
                } else {
                    link = LinkDelta.IDLE
                    stats = null
                    lastError = null
                    level = 0f
                    uptimeSeconds = 0L
                }
            } catch (e: UnsatisfiedLinkError) {
                if (nativeError == null) {
                    nativeError = "Native library failed to load: ${e.message}"
                }
                running = false
            }
            delay(POLL_INTERVAL_MS)
        }
    }

    // ---------- UI ----------

    Scaffold(
        containerColor = MaterialTheme.colorScheme.background,
        snackbarHost = {
            SnackbarHost(snackbarHostState) { data ->
                Snackbar(
                    snackbarData = data,
                    containerColor = PanelRaised,
                    contentColor = Cream,
                    shape = RoundedCornerShape(2.dp),
                )
            }
        }
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            // Wordmark header with running lamp dot
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                LampDot(running = running)
                Text("OPENAY MIC", style = WordmarkStyle, color = Cream)
            }

            nativeError?.let { ErrorPanel(it, Modifier.fillMaxWidth()) }

            ChainCard(
                running = running,
                level = level,
                link = link,
                consoleHost = host,
                consolePort = portText,
                reducedMotion = reducedMotion,
                modifier = Modifier.fillMaxWidth(),
            )

            lastError?.let { ErrorPanel(it, Modifier.fillMaxWidth()) }

            // ON AIR lamp + mono status line
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                OnAirToggle(
                    running = running,
                    enabled = nativeError == null || running,
                    reducedMotion = reducedMotion,
                    onClick = ::onTogglePress,
                )
                Text(
                    "${transportLabel(transport)} · " +
                        "${if (host.isBlank()) "0.0.0.0" else host}:${portText.ifBlank { "0" }} · " +
                        "${codecLabel(codec)} ${frameMs}MS",
                    style = OpenAyDataStyles.micro,
                    color = Dim.copy(alpha = 0.85f),
                    maxLines = 1,
                    softWrap = false,
                    overflow = TextOverflow.Ellipsis,
                )
            }

            SettingsRow(label = "TRANSPORT") {
                TransportSegments(
                    selected = transport,
                    onSelect = {
                        transport = it
                        prefs.transport = it
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            SettingsRow(label = "CODEC") {
                ChipRow(
                    options = listOf(ChipOption("RAW PCM", "pcm"), ChipOption("OPUS", "opus")),
                    selected = codec,
                    onSelect = {
                        codec = it
                        prefs.codec = it
                    },
                )
            }

            SettingsRow(label = "FRAME") {
                ChipRow(
                    options = listOf(ChipOption("5 MS", "5"), ChipOption("10 MS", "10")),
                    selected = frameMs.toString(),
                    onSelect = {
                        frameMs = it.toInt()
                        prefs.frameMs = it.toInt()
                    },
                )
            }

            NetworkCard(
                host = host,
                onHostChange = {
                    host = it
                    prefs.host = it
                },
                port = portText,
                onPortChange = {
                    portText = it.filter(Char::isDigit)
                    prefs.port = portText
                },
                sent = stats?.sent ?: 0L,
                lost = link.lost,
                uptimeSeconds = uptimeSeconds,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

/** Fields the UI drives off the native stats JSON (full schema in NativeBridge). */
private data class MicStats(
    val sent: Long = 0L,
    val sendErrors: Long = 0L,
    val lastError: String = "",
    val levelPeak: Int = 0,
)

private fun parseMicStats(json: String): MicStats? = try {
    val o = JSONObject(json)
    MicStats(
        sent = o.optLong("sent"),
        sendErrors = o.optLong("send_errors"),
        lastError = o.optString("last_error"),
        levelPeak = o.optInt("level_peak"),
    )
} catch (_: Exception) {
    null
}

/**
 * Config persistence — plain SharedPreferences, kept over DataStore: values
 * are tiny, written synchronously on user edit, and the service contract
 * never reads them (everything travels via intent extras).
 */
private class AppPrefs(context: Context) {
    private val sp = context.applicationContext
        .getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    var transport: String
        get() = sp.getString(KEY_TRANSPORT, DEFAULT_TRANSPORT) ?: DEFAULT_TRANSPORT
        set(value) = sp.edit().putString(KEY_TRANSPORT, value).apply()

    var host: String
        get() = sp.getString(KEY_HOST, DEFAULT_HOST) ?: DEFAULT_HOST
        set(value) = sp.edit().putString(KEY_HOST, value).apply()

    var port: String
        get() = sp.getString(KEY_PORT, DEFAULT_PORT) ?: DEFAULT_PORT
        set(value) = sp.edit().putString(KEY_PORT, value).apply()

    var codec: String
        get() = sp.getString(KEY_CODEC, DEFAULT_CODEC) ?: DEFAULT_CODEC
        set(value) = sp.edit().putString(KEY_CODEC, value).apply()

    var frameMs: Int
        get() = sp.getInt(KEY_FRAME_MS, DEFAULT_FRAME_MS)
        set(value) = sp.edit().putInt(KEY_FRAME_MS, value).apply()

    private companion object {
        const val PREFS_NAME = "openay_mic_settings"
        const val KEY_TRANSPORT = "transport"
        const val KEY_HOST = "host"
        const val KEY_PORT = "port"
        const val KEY_CODEC = "codec"
        const val KEY_FRAME_MS = "frame_ms"
        const val DEFAULT_TRANSPORT = "udp"
        const val DEFAULT_HOST = "10.0.2.2"
        const val DEFAULT_PORT = "41700"
        const val DEFAULT_CODEC = "pcm"
        const val DEFAULT_FRAME_MS = 10
    }
}

/**
 * System reduced-motion: animator duration scale == 0. When set, the cable
 * pulses and the power-on stagger are dropped (the level ring stays live —
 * it is instrument data, not decoration).
 */
@Composable
private fun rememberReducedMotion(): Boolean {
    val resolver = LocalContext.current.contentResolver
    return remember {
        try {
            Settings.Global.getFloat(resolver, Settings.Global.ANIMATOR_DURATION_SCALE, 1f) == 0f
        } catch (_: Exception) {
            false
        }
    }
}

private fun transportLabel(transport: String): String = when (transport) {
    "tcp" -> "USB"
    else -> "UDP"
}

private fun codecLabel(codec: String): String = when (codec) {
    "opus" -> "OPUS"
    else -> "PCM"
}