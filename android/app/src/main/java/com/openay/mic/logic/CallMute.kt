package com.openay.mic.logic

import android.media.AudioManager

/**
 * Call auto-mute decision: capture is muted whenever the device audio is in
 * any non-normal mode (ringing, in-call, communication), because the
 * microphone would otherwise stream call audio to the desktop.
 *
 * The service polls the audio mode instead of registering a mode-change
 * listener: the listener interface is not available on all supported OS
 * levels, while [AudioManager.getMode] is (API 1).
 */
object CallMute {
    /** How often the service re-checks the audio mode while capturing. */
    const val POLL_INTERVAL_MS = 250L

    fun shouldMuteForCall(mode: Int): Boolean = mode != AudioManager.MODE_NORMAL
}
