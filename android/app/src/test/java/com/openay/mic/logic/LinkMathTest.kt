package com.openay.mic.logic

import org.junit.Assert.assertEquals
import org.junit.Test

class LinkMathTest {

    // ---- linkDelta: per-second rate + lost deltas ----

    @Test
    fun linkDelta_convertsIntervalCountsToPerSecond() {
        val d = linkDelta(prevSent = 0, prevLost = 0, curSent = 60, curLost = 2, intervalMs = 500)
        assertEquals(120, d.sentPerSec)
        assertEquals(2, d.lost)
    }

    @Test
    fun linkDelta_typicalPhonePollCadence() {
        // 20 packets over a 140 ms poll window -> ~143/s
        val d = linkDelta(prevSent = 100, prevLost = 0, curSent = 120, curLost = 1, intervalMs = 140)
        assertEquals(143, d.sentPerSec)
        assertEquals(1, d.lost)
    }

    @Test
    fun linkDelta_zeroIntervalYieldsZeroRateButKeepsLost() {
        val d = linkDelta(prevSent = 10, prevLost = 0, curSent = 20, curLost = 3, intervalMs = 0)
        assertEquals(0, d.sentPerSec)
        assertEquals(3, d.lost)
    }

    @Test
    fun linkDelta_clampsCountersThatResetBackwards() {
        val d = linkDelta(prevSent = 100, prevLost = 5, curSent = 10, curLost = 0, intervalMs = 1000)
        assertEquals(0, d.sentPerSec)
        assertEquals(0, d.lost)
    }

    @Test
    fun linkDelta_roundsPartialPacketsDown() {
        // 10 packets in 333 ms = 30.03/s -> 30
        val d = linkDelta(prevSent = 0, prevLost = 0, curSent = 10, curLost = 0, intervalMs = 333)
        assertEquals(30, d.sentPerSec)
    }

    // ---- smoothLevel: exponential easing for the mic ring ----

    @Test
    fun smoothLevel_noMovementAtAlphaZero() {
        assertEquals(40f, smoothLevel(40f, 90f, 0f), 1e-4f)
    }

    @Test
    fun smoothLevel_jumpsToTargetAtAlphaOne() {
        assertEquals(90f, smoothLevel(40f, 90f, 1f), 1e-4f)
    }

    @Test
    fun smoothLevel_easesFractionally() {
        assertEquals(52.5f, smoothLevel(40f, 90f, 0.25f), 1e-4f)
    }

    @Test
    fun smoothLevel_clampsOutOfRangeAlpha() {
        assertEquals(90f, smoothLevel(40f, 90f, 2f), 1e-4f)
        assertEquals(40f, smoothLevel(40f, 90f, -1f), 1e-4f)
    }

    @Test
    fun smoothLevel_convergesOverRepeatedSteps() {
        var l = 0f
        repeat(20) { l = smoothLevel(l, 100f, 0.75f) }
        assertEquals(100f, l, 0.05f)
    }

    @Test
    fun smoothLevel_decaysTowardZero() {
        var l = 80f
        repeat(6) { l = smoothLevel(l, 0f, 0.5f) }
        assertEquals(1.25f, l, 1e-4f)
    }

    // ---- formatUptime ----

    @Test
    fun formatUptime_secondsAndMinutes() {
        assertEquals("00:00", formatUptime(0))
        assertEquals("00:12", formatUptime(12))
        assertEquals("01:40", formatUptime(100))
        assertEquals("59:59", formatUptime(3599))
    }

    @Test
    fun formatUptime_rollsToHours() {
        assertEquals("1:00:00", formatUptime(3600))
        assertEquals("1:00:01", formatUptime(3601))
        assertEquals("25:12:07", formatUptime(25L * 3600 + 12 * 60 + 7))
    }

    @Test
    fun formatUptime_ignoresNegative() {
        assertEquals("00:00", formatUptime(-5))
    }

    // ---- formatCount ----

    @Test
    fun formatCount_groupsThousands() {
        assertEquals("0", formatCount(0))
        assertEquals("999", formatCount(999))
        assertEquals("1,240", formatCount(1240))
        assertEquals("1,000,000", formatCount(1_000_000))
    }
}