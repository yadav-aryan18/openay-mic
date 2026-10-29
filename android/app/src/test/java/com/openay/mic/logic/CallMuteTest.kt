package com.openay.mic.logic

import android.media.AudioManager
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CallMuteTest {

    @Test
    fun `normal mode does not mute`() {
        assertFalse(CallMute.shouldMuteForCall(AudioManager.MODE_NORMAL))
    }

    @Test
    fun `ringtone in-call and communication modes mute`() {
        assertTrue(CallMute.shouldMuteForCall(AudioManager.MODE_RINGTONE))
        assertTrue(CallMute.shouldMuteForCall(AudioManager.MODE_IN_CALL))
        assertTrue(CallMute.shouldMuteForCall(AudioManager.MODE_IN_COMMUNICATION))
    }

    @Test
    fun `poll interval keeps mute latency bounded`() {
        // The mute path is poll-based; 250 ms keeps the window between a
        // call starting and the capture going silent comfortably under
        // half a second.
        assertTrue(CallMute.POLL_INTERVAL_MS in 1..500)
    }
}
