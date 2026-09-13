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
import java.io.IOException
import java.util.UUID
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
 * lives here and the bridge to the engine is a plain pipe:
 *
 *   native engine -> pipe WRITE fd (owned by native, handed to
 *   NativeBridge.nativeStartBt) -> [pump] -> socket.outputStream -> desktop
 *
 * Audio is strictly one-way phone->desktop; socket.inputStream is never
 * read. The desktop SPP server sends nothing, so leaving it unread costs
 * nothing and cannot stall the pump.
 *
 * fd ownership (single-owner contract, no double close): [connect] detaches
 * the pipe's write end and returns its raw fd — from that moment NATIVE owns
 * it and closes it exactly once (engine stop / re-configure / teardown).
 * This class never touches it again; [close] releases only the read end and
 * the socket.
 *
 * Teardown order matters (MicCaptureService): stop native FIRST — the engine
 * closes the write fd, the pump's read() returns EOF and exits on its own;
 * [close] then gives the pump a short flush window and closes the socket.
 * On a mid-run socket failure the pump closes the read end itself, so native
 * writes fail fast (EPIPE) instead of blocking on a full pipe.
 */
class BluetoothLink {

    /** Bonded device summary for the picker. */
    data class BondedDevice(val name: String, val address: String)

    private var socket: BluetoothSocket? = null
    private var pipeRead: ParcelFileDescriptor? = null
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
        val s = device.createRfcommSocketToServiceRecord(SPP_UUID)
        try {
            // Blocking connect() to the desktop's SPP service; the caller is
            // on IO, so stalling here is fine (same model as native's TCP
            // connect, just in Kotlin because the NDK has no BT sockets).
            s.connect()
        } catch (e: SecurityException) {
            try { s.close() } catch (_: IOException) {}
            throw BluetoothLinkException("Bluetooth permission missing", e)
        } catch (e: IOException) {
            try { s.close() } catch (_: IOException) {}
            throw BluetoothLinkException(
                "Connect failed — is the console paired and its SPP service running?", e
            )
        }
        // The caller may have torn the link down while the blocking connect()
        // ran: self-clean instead of assigning state into a closed link (the
        // service's close already happened and will never revisit us).
        if (closed) {
            try { s.close() } catch (_: IOException) {}
            throw BluetoothLinkException("link already closed")
        }
        socket = s
        val pipe = try {
            ParcelFileDescriptor.createPipe()  // [0] read (pump), [1] write (native)
        } catch (e: IOException) {
            try { s.close() } catch (_: IOException) {}
            socket = null
            throw BluetoothLinkException("Pipe setup failed", e)
        }
        if (closed) {
            // Same race, one step later: close everything we just made —
            // both pipe ends, since the detach below has not happened yet.
            try { s.close() } catch (_: IOException) {}
            try { pipe[0].close() } catch (_: IOException) {}
            try { pipe[1].close() } catch (_: IOException) {}
            socket = null
            throw BluetoothLinkException("link already closed")
        }
        pipeRead = pipe[0]
        // Detach: the fd belongs to native from the moment we return it.
        return pipe[1].detachFd()
    }

    /**
     * Starts the pump (pipe read end -> socket output) on [scope]. [onDown]
     * fires at most once, on the socket dropping or any IO error — NOT on the
     * clean EOF that follows a native stop. The handler runs on
     * Dispatchers.IO.
     */
    fun startPump(scope: CoroutineScope, onDown: (String) -> Unit) {
        val read = pipeRead ?: return
        val s = socket ?: return
        pumpJob = scope.launch(Dispatchers.IO) {
            try {
                FileInputStream(read.fileDescriptor).use { input ->
                    s.outputStream.use { output ->
                        val buf = ByteArray(PUMP_BUFFER_BYTES)
                        while (true) {
                            val n = input.read(buf)
                            if (n < 0) return@launch  // native closed the write fd: clean stop
                            output.write(buf, 0, n)
                        }
                    }
                }
            } catch (e: Exception) {
                if (closed) return@launch  // teardown race, not an error
                // Link dropped mid-run: close the read end so native writes
                // fail fast (EPIPE) instead of blocking on a full pipe.
                closeReadEnd()
                onDown(e.message ?: "socket I/O error")
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
        closeReadEnd()
        try {
            socket?.close()
        } catch (_: IOException) {
        }
        socket = null
    }

    private fun closeReadEnd() {
        try {
            pipeRead?.close()
        } catch (_: IOException) {
        }
        pipeRead = null
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
