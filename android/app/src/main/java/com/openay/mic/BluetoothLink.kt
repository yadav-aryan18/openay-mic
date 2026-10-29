package com.openay.mic

import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothSocket
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.content.ContextCompat
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull

/** Connect/link failure carrying a UI-ready reason (notification + log). */
class BluetoothLinkException(message: String, cause: Throwable? = null) :
    Exception(message, cause)

/**
 * Bluetooth SPP (RFCOMM) link to the desktop console — the phone is the
 * CLIENT: it connects to the desktop's SPP service and pumps the native
 * engine's packets into it. Native owns no Bluetooth code, so the socket
 * lives here and the bridge to the engine is a full-duplex local socket
 * pair (one end owned by native, handed to NativeBridge.nativeStartBt):
 *
 *   native engine -> local end -> [pump] -> socket.outputStream -> desktop
 *   native engine <- local end <- [pump] <- socket.inputStream <- desktop
 *
 * Audio flows phone->desktop; the desktop->phone direction carries the
 * CONTROL plane (the desktop receiver's liveness pings and hello-ack),
 * which the native sink reads back from the same fd — so the phone
 * answers pings and reports receiver liveness exactly like over TCP.
 *
 * fd ownership (single-owner contract, no double close): [connect] detaches
 * the native end and returns its raw fd — from that moment NATIVE owns
 * it and closes it exactly once (engine stop / re-configure / teardown).
 * This class never touches it again; [close] releases only the pump end and
 * the socket.
 *
 * Teardown order matters (MicCaptureService): stop native FIRST — the engine
 * closes the native end, the phone->desktop pump's read() returns EOF and
 * exits on its own, and the desktop->phone pump's next write fails with
 * EPIPE and exits quietly (a stop in progress, not a link error); [close]
 * then gives the pumps a short flush window and closes the socket. On a
 * mid-run socket failure the pump closes the pump end itself, so native
 * writes fail fast (EPIPE) instead of blocking on a full buffer.
 */
class BluetoothLink {

    /** Bonded device summary for the picker. */
    data class BondedDevice(val name: String, val address: String)

    private var socket: BluetoothSocket? = null
    private var pumpEnd: ParcelFileDescriptor? = null
    private var pumpJob: Job? = null

    @Volatile private var closed = false

    // ---------- discovery (MainActivity picker) ----------

    /** Runtime permission gate: BLUETOOTH_CONNECT on S+, install-time before. */
    fun needsConnectPermission(context: Context): Boolean =
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S &&
            ContextCompat.checkSelfPermission(
                context, Manifest.permission.BLUETOOTH_CONNECT
            ) != PackageManager.PERMISSION_GRANTED

    /**
     * Bonded devices, picker-ready (name first, address second). Empty when
     * the adapter is missing/off or the connect permission is not granted —
     * the picker shows a hint instead.
     */
    @SuppressLint("MissingPermission")
    fun bondedDevices(context: Context): List<BondedDevice> {
        if (needsConnectPermission(context)) return emptyList()
        val adapter = adapter(context) ?: return emptyList()
        if (!adapter.isEnabled) return emptyList()
        return adapter.bondedDevices.orEmpty()
            .map { BondedDevice(it.name ?: "", it.address) }
            .sortedBy { it.name.lowercase() }
    }

    // ---------- connection (MicCaptureService) ----------

    /**
     * Connects to the desktop's SPP service and returns the pipe write fd for
     * NativeBridge.nativeStartBt. Blocking (a cold RFCOMM connect can take
     * seconds) — call from Dispatchers.IO. Fails with a
     * [BluetoothLinkException] whose message is fit for direct display
     * (permission missing / Bluetooth off / not paired / connect failed).
     */
    @SuppressLint("MissingPermission")
    suspend fun connect(context: Context, address: String): Int {
        if (closed) throw BluetoothLinkException("link already closed")
        if (needsConnectPermission(context)) {
            throw BluetoothLinkException("Bluetooth permission missing")
        }
        val adapter = adapter(context)
            ?: throw BluetoothLinkException("Bluetooth unavailable on this device")
        if (!adapter.isEnabled) throw BluetoothLinkException("Bluetooth is off")
        val device = try {
            adapter.getRemoteDevice(address)
        } catch (e: IllegalArgumentException) {
            throw BluetoothLinkException("Bad device address: $address", e)
        }
        // Create the socket BEFORE the try so both catch arms can close it —
        // connect() failing must not leak the (unconnected) socket.
        var s = device.createRfcommSocketToServiceRecord(SPP_UUID)
        try {
            // Blocking connect() to the desktop's SPP service; the caller is
            // on IO, so stalling here is fine (same model as native's TCP
            // connect, just in Kotlin because the NDK has no BT sockets).
            s.connect()
        } catch (e: SecurityException) {
            try { s.close() } catch (_: IOException) {}
            throw BluetoothLinkException("Bluetooth permission missing", e)
        } catch (e: IOException) {
            // Classic Android → BlueZ SPP quirk: the secure RFCOMM channel
            // can fail service discovery on some phone/PC pairings where the
            // insecure channel connects fine (encryption is irrelevant here —
            // the audio is one-way and the link is already paired). Retry
            // once with the insecure factory before giving up.
            try { s.close() } catch (_: IOException) {}
            var insecure: BluetoothSocket? = null
            try {
                insecure = device.createInsecureRfcommSocketToServiceRecord(SPP_UUID)
                insecure.connect()
            } catch (retry: Exception) {
                try { insecure?.close() } catch (_: IOException) {}
                if (retry is SecurityException) {
                    throw BluetoothLinkException("Bluetooth permission missing", retry)
                }
                throw BluetoothLinkException(
                    "Connect failed — is the console paired, in BT mode, and ON AIR?", e
                )
            }
            s = insecure
        }
        // The caller may have torn the link down while the blocking connect()
        // ran: self-clean instead of assigning state into a closed link (the
        // service's close already happened and will never revisit us).
        if (closed) {
            try { s.close() } catch (_: IOException) {}
            throw BluetoothLinkException("link already closed")
        }
        socket = s
        // Full-duplex local pair: [0] pump (Kotlin), [1] native. Native's
        // FdBytePipe writes phone audio into [1] and reads the desktop's
        // control traffic back from it — the exact wire behavior of a TCP
        // client socket, exercised unchanged by the host socketpair tests.
        val pair = try {
            ParcelFileDescriptor.createSocketPair()
        } catch (e: IOException) {
            try { s.close() } catch (_: IOException) {}
            socket = null
            throw BluetoothLinkException("Pipe setup failed", e)
        }
        if (closed) {
            // Same race, one step later: close everything we just made —
            // both ends, since the detach below has not happened yet.
            try { s.close() } catch (_: IOException) {}
            try { pair[0].close() } catch (_: IOException) {}
            try { pair[1].close() } catch (_: IOException) {}
            socket = null
            throw BluetoothLinkException("link already closed")
        }
        pumpEnd = pair[0]
        // Detach: the fd belongs to native from the moment we return it.
        return pair[1].detachFd()
    }

    /**
     * Starts the bidirectional pump on [scope]: native end -> socket output
     * (phone audio) and socket input -> native end (desktop control traffic).
     * [onDown] fires at most once, on the socket dropping or any IO error —
     * NOT on the clean EOF that follows a native stop. The handler runs on
     * Dispatchers.IO.
     */
    fun startPump(scope: CoroutineScope, onDown: (String) -> Unit) {
        val local = pumpEnd ?: return
        val s = socket ?: return
        pumpJob = scope.launch(Dispatchers.IO) {
            // The two directions fail independently and [onDown] must fire at
            // most once (the service stops the session on the first report).
            val reported = AtomicBoolean(false)
            fun linkDown(reason: String) {
                if (reported.compareAndSet(false, true)) {
                    // Link dropped mid-run: close the pump end so native
                    // writes fail fast (EPIPE) instead of blocking on a full
                    // buffer.
                    closePumpEnd()
                    onDown(reason)
                }
            }
            // phone -> desktop: native audio out.
            launch {
                try {
                    FileInputStream(local.fileDescriptor).use { nativeInput ->
                        s.outputStream.use { output ->
                            val buf = ByteArray(PUMP_BUFFER_BYTES)
                            while (true) {
                                // < 0 = the native end closed (a clean stop in
                                // progress) — exit quietly, like the old
                                // pipe pump did on EOF.
                                val n = nativeInput.read(buf)
                                if (n < 0) return@launch
                                output.write(buf, 0, n)
                            }
                        }
                    }
                } catch (e: Exception) {
                    if (closed) return@launch  // teardown race, not an error
                    linkDown(e.message ?: "socket I/O error")
                }
            }
            // desktop -> phone: control traffic in (the desktop receiver
            // sends liveness pings every second and answers hello). Blocking
            // read: while the desktop is up this returns ~1 ping/s; on a
            // socket drop it errors and is reported via linkDown.
            launch {
                try {
                    s.inputStream.use { input ->
                        FileOutputStream(local.fileDescriptor).use { nativeOutput ->
                            val buf = ByteArray(PUMP_BUFFER_BYTES)
                            while (true) {
                                val n = input.read(buf)
                                if (n < 0) {
                                    linkDown("desktop closed the stream")
                                    return@launch
                                }
                                try {
                                    nativeOutput.write(buf, 0, n)
                                } catch (_: IOException) {
                                    // The native end closed mid-write — a
                                    // native stop in progress, not a link
                                    // error (the phone->desktop pump reports
                                    // the same stop as a clean EOF above).
                                    return@launch
                                }
                            }
                        }
                    }
                } catch (e: Exception) {
                    if (closed) return@launch  // teardown race, not an error
                    linkDown(e.message ?: "socket I/O error")
                }
            }
        }
    }

    /**
     * Tears the link down (idempotent). Never touches the native-owned write
     * fd. Gives the pump a short window to flush the pipe tail into the
     * socket after a native stop; a wedged socket bounds that wait.
     */
    @Synchronized
    fun close() {
        if (closed) return
        closed = true
        pumpJob?.let { job ->
            if (!job.isCompleted) {
                runBlocking { withTimeoutOrNull(PUMP_FLUSH_MS) { job.join() } }
            }
        }
        pumpJob = null
        closePumpEnd()
        try {
            socket?.close()
        } catch (_: IOException) {
        }
        socket = null
    }

    private fun closePumpEnd() {
        try {
            pumpEnd?.close()
        } catch (_: IOException) {
        }
        pumpEnd = null
    }

    private fun adapter(context: Context): BluetoothAdapter? {
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        return manager?.adapter
    }

    companion object {
        /** Desktop SPP service (RFCOMM), standard Serial Port Profile UUID. */
        val SPP_UUID: UUID = UUID.fromString("00001101-0000-1000-8000-00805F9B34FB")

        private const val PUMP_BUFFER_BYTES = 4096
        private const val PUMP_FLUSH_MS = 200L
    }
}
