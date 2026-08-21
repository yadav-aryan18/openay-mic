package com.openay.mic.logic

import java.util.Locale
import kotlin.math.roundToInt

/** Per-poll deltas derived from two consecutive stats reads. */
data class LinkDelta(val sentPerSec: Int, val lost: Int) {
    companion object {
        /** Idle strip — no stream, no readout. */
        val IDLE = LinkDelta(0, 0)
    }
}

/**
 * Computes the per-second send rate and the lost count from two consecutive
 * stats snapshots. Counters are monotone u64 surfaced as JSON ints; a
 * backwards move (counter reset) is clamped to 0 instead of wrapping.
 *
 * @param intervalMs wall time between the two reads (poll cadence).
 */
fun linkDelta(
    prevSent: Long,
    prevLost: Long,
    curSent: Long,
    curLost: Long,
    intervalMs: Long,
): LinkDelta {
    val dSent = (curSent - prevSent).coerceAtLeast(0L)
    val dLost = (curLost - prevLost).coerceAtLeast(0L)
    val sentPerSec = if (intervalMs > 0) (dSent * 1000.0 / intervalMs).roundToInt() else 0
    return LinkDelta(
        sentPerSec = sentPerSec,
        lost = dLost.coerceAtMost(Int.MAX_VALUE.toLong()).toInt(),
    )
}

/**
 * One step of exponential level smoothing (the ring's ~100 ms ease).
 * Alpha is clamped to 0..1.
 */
fun smoothLevel(prev: Float, target: Float, alpha: Float): Float {
    val a = alpha.coerceIn(0f, 1f)
    return prev + (target - prev) * a
}

/** Session uptime as mm:ss, rolling to h:mm:ss past an hour. */
fun formatUptime(totalSeconds: Long): String {
    val s = totalSeconds.coerceAtLeast(0L)
    val h = s / 3600
    val m = (s % 3600) / 60
    val sec = s % 60
    return if (h > 0) {
        String.format(Locale.US, "%d:%02d:%02d", h, m, sec)
    } else {
        String.format(Locale.US, "%02d:%02d", m, sec)
    }
}

/** Thousands-grouped count, e.g. 1240 -> "1,240". */
fun formatCount(value: Long): String {
    val digits = value.coerceAtLeast(0L).toString()
    return digits.reversed().chunked(3).joinToString(",").reversed()
}