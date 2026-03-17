package com.dispatch.radio

import android.content.Context
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager

/**
 * Haptic feedback patterns for radio events.
 * Detailed patterns are implemented in dispatch-88k.8.
 */
class HapticFeedback(context: Context) {

    private val vibrator: Vibrator = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        (context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as VibratorManager)
            .defaultVibrator
    } else {
        @Suppress("DEPRECATION")
        context.getSystemService(Context.VIBRATOR_SERVICE) as Vibrator
    }

    private var enabled = true

    fun setEnabled(enabled: Boolean) {
        this.enabled = enabled
    }

    /** Short single pulse — used on target change. */
    fun shortPulse() {
        if (!enabled) return
        vibrate(50)
    }

    /** Confirm pulse — used after send. */
    fun confirmPulse() {
        if (!enabled) return
        vibrate(100)
    }

    /** Double pulse — used on empty transcript. */
    fun doublePulse() {
        if (!enabled) return
        vibratePattern(longArrayOf(0, 60, 80, 60))
    }

    private fun vibrate(ms: Long) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            vibrator.vibrate(VibrationEffect.createOneShot(ms, VibrationEffect.DEFAULT_AMPLITUDE))
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(ms)
        }
    }

    private fun vibratePattern(pattern: LongArray) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            vibrator.vibrate(VibrationEffect.createWaveform(pattern, -1))
        } else {
            @Suppress("DEPRECATION")
            vibrator.vibrate(pattern, -1)
        }
    }
}
