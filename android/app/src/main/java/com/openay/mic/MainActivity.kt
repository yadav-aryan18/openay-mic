package com.openay.mic

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.json.JSONObject

/** Polling interval for native stats / isRunning (ms). */
private const val POLL_INTERVAL_MS = 500L

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            MaterialTheme(colorScheme = darkColorScheme()) {
                CaptureScreen()
            }
        }
    }
}

/**
 * Main capture screen with settings (transport, host, port, codec, frame
 * duration), a Start/Stop button, and live stats from the native engine.
 */
@Composable
private fun CaptureScreen() {
    val context = LocalContext.current
    val snackbarHostState = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()

    // --- persisted configuration ---
    var transport by rememberSaveable { mutableStateOf("udp") }
    var host by rememberSaveable { mutableStateOf("10.0.2.2") }
    var portText by rememberSaveable { mutableStateOf("41700") }
    var codec by rememberSaveable { mutableStateOf("pcm") }
    var frameMs by rememberSaveable { mutableStateOf(10) }

    // --- live state ---
    var running by remember { mutableStateOf(false) }
    var stats by remember { mutableStateOf<Map<String, Any>?>(null) }
    var nativeError by remember { mutableStateOf<String?>(null) }

    // ---------- helper functions ----------

    fun parseStats(json: String): Map<String, Any>? = try {
        val obj = JSONObject(json)
        obj.keys().asSequence().associateWith { key -> obj.get(key) }
    } catch (_: Exception) {
        null
    }

    fun startCapture() {
        if (nativeError != null) return
        val port = portText.trim().toIntOrNull()
        if (port == null || port !in 1..65535) {
            scope.launch { snackbarHostState.showSnackbar("Port must be 1–65535") }
            return
        }
        if (host.isBlank()) {
            scope.launch { snackbarHostState.showSnackbar("Host must not be empty") }
            return
        }
        val intent = MicCaptureService.captureIntent(
            context, transport, host, port, codec, frameMs
        )
        ContextCompat.startForegroundService(context, intent)
    }

    fun stopCapture() {
        context.stopService(Intent(context, MicCaptureService::class.java))
        running = false
    }

    // ---------- permission launcher ----------
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) {
            startCapture()
        } else {
            scope.launch {
                snackbarHostState.showSnackbar("Microphone permission denied — capture not started")
            }
        }
    }

    // ---------- polling loop ----------
    LaunchedEffect(Unit) {
        while (true) {
            try {
                val alive = NativeBridge.nativeIsRunning()
                running = alive
                if (alive) {
                    val json = NativeBridge.nativeGetStats()
                    stats = parseStats(json)
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

    Scaffold(snackbarHost = { SnackbarHost(snackbarHostState) }) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp)
        ) {
            // Title
            Text(
                "OpenAY Mic",
                style = MaterialTheme.typography.headlineMedium,
                fontWeight = FontWeight.Bold
            )

            // Native load error banner
            nativeError?.let { err ->
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = MaterialTheme.colorScheme.errorContainer
                    ),
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text(
                        err,
                        color = MaterialTheme.colorScheme.onErrorContainer,
                        modifier = Modifier.padding(12.dp)
                    )
                }
            }

            // ---- Transport ----
            SettingsSection("Transport") {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    SegmentedButton(
                        selected = transport == "udp",
                        onClick = { transport = "udp" },
                        shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2)
                    ) { Text("UDP") }
                    SegmentedButton(
                        selected = transport == "tcp",
                        onClick = { transport = "tcp" },
                        shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2)
                    ) { Text("TCP") }
                }
            }

            // ---- Destination ----
            SettingsSection("Destination") {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedTextField(
                        value = host,
                        onValueChange = { host = it },
                        label = { Text("Host") },
                        singleLine = true,
                        modifier = Modifier.weight(1f)
                    )
                    OutlinedTextField(
                        value = portText,
                        onValueChange = { portText = it.filter(Char::isDigit) },
                        label = { Text("Port") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                        modifier = Modifier.weight(0.6f)
                    )
                }
            }

            // ---- Codec ----
            SettingsSection("Codec") {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    SegmentedButton(
                        selected = codec == "pcm",
                        onClick = { codec = "pcm" },
                        shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2)
                    ) { Text("PCM") }
                    SegmentedButton(
                        selected = codec == "opus",
                        onClick = { codec = "opus" },
                        shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2)
                    ) { Text("Opus") }
                }
            }

            // ---- Frame duration ----
            SettingsSection("Frame") {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
                    SegmentedButton(
                        selected = frameMs == 5,
                        onClick = { frameMs = 5 },
                        shape = SegmentedButtonDefaults.itemShape(index = 0, count = 2)
                    ) { Text("5 ms") }
                    SegmentedButton(
                        selected = frameMs == 10,
                        onClick = { frameMs = 10 },
                        shape = SegmentedButtonDefaults.itemShape(index = 1, count = 2)
                    ) { Text("10 ms") }
                }
            }

            // ---- Start / Stop ----
            Button(
                onClick = {
                    if (running) {
                        stopCapture()
                    } else if (nativeError != null) {
                        scope.launch {
                            snackbarHostState.showSnackbar("Native library unavailable — cannot start")
                        }
                    } else if (ContextCompat.checkSelfPermission(
                            context, Manifest.permission.RECORD_AUDIO
                        ) == PackageManager.PERMISSION_GRANTED
                    ) {
                        startCapture()
                    } else {
                        permissionLauncher.launch(Manifest.permission.RECORD_AUDIO)
                    }
                },
                enabled = nativeError == null || running,
                modifier = Modifier
                    .fillMaxWidth()
                    .height(56.dp),
                colors = if (running) {
                    ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError
                    )
                } else {
                    ButtonDefaults.buttonColors()
                }
            ) {
                Text(
                    if (running) "Stop" else "Start",
                    style = MaterialTheme.typography.titleMedium
                )
            }

            // ---- Status ----
            SettingsSection("Status") {
                // Running indicator
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    Box(
                        modifier = Modifier
                            .size(10.dp)
                            .background(
                                if (running) Color(0xFF4CAF50) else MaterialTheme.colorScheme.outline,
                                CircleShape
                            )
                    )
                    Text(
                        if (running) "Capture active" else "Idle",
                        style = MaterialTheme.typography.titleMedium
                    )
                }

                Spacer(Modifier.height(8.dp))

                // Stats card
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(
                        modifier = Modifier.padding(16.dp),
                        verticalArrangement = Arrangement.spacedBy(6.dp)
                    ) {
                        Text("Live stats", style = MaterialTheme.typography.titleMedium)
                        HorizontalDivider()

                        if (stats == null && !running) {
                            Text(
                                "No capture session yet",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant
                            )
                        } else {
                            StatRow("running", if (running) "true" else "false")
                            listOf("transport", "codec", "sharing", "sent", "bytes",
                                "ring_overruns", "xruns").forEach { key ->
                                stats?.get(key)?.let { value ->
                                    StatRow(key, value.toString())
                                }
                            }
                            val lastError = stats?.get("last_error")?.toString()
                                ?.takeUnless { it.isBlank() || it == "null" }
                            if (lastError != null) {
                                StatRow("last_error", lastError, highlight = true)
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun SettingsSection(
    title: String,
    content: @Composable ColumnScope.() -> Unit
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            title,
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.primary
        )
        content()
    }
}

@Composable
private fun StatRow(
    label: String,
    value: String,
    highlight: Boolean = false
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween
    ) {
        Text(
            label,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant
        )
        Text(
            value,
            style = MaterialTheme.typography.bodyMedium,
            fontWeight = FontWeight.SemiBold,
            color = if (highlight) MaterialTheme.colorScheme.error
            else MaterialTheme.colorScheme.onSurface
        )
    }
}