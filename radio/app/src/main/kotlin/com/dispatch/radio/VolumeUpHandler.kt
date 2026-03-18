package com.dispatch.radio

import android.content.Context
import android.os.Handler
import android.os.Looper
import com.dispatch.radio.model.Agent

/**
 * Handles Volume Up key events for target cycling and quick dispatch.
 *
 * Short press (key down, key up < 1s): cycle to next occupied slot,
 *   send set_target, display new callsign, short vibration.
 *
 * Long press (key down held > 1s): show agent type picker overlay,
 *   tap selection dispatches a new agent.
 *
 * Usage:
 *   - Call [onKeyDown] from Activity.onKeyDown for KEYCODE_VOLUME_UP
 *   - Call [onKeyUp] from Activity.onKeyUp for KEYCODE_VOLUME_UP
 *   - Both return true to consume the event and suppress system volume UI
 */
class VolumeUpHandler(
    private val context: Context,
    private val haptics: HapticFeedback,
    private val onCycleTarget: (agent: Agent) -> Unit,
    private val onQuickDispatch: (tool: String) -> Unit
) {
    companion object {
        private const val LONG_PRESS_MS = 1000L
    }

    private val handler = Handler(Looper.getMainLooper())
    private var longPressTriggered = false

    private val longPressRunnable = Runnable {
        longPressTriggered = true
        // Overlay requires a foreground activity — skip when backgrounded (dispatch-ct2.7)
        if (VolumeKeyBridge.isActivityInForeground) {
            QuickDispatchOverlay(context).show { tool ->
                onQuickDispatch(tool)
            }
        }
    }

    /** Call from Activity.onKeyDown for KEYCODE_VOLUME_UP. Returns true to consume. */
    fun onKeyDown(agents: List<Agent>, currentSlot: Int): Boolean {
        longPressTriggered = false
        handler.postDelayed(longPressRunnable, LONG_PRESS_MS)
        return true
    }

    /** Call from Activity.onKeyUp for KEYCODE_VOLUME_UP. Returns true to consume. */
    fun onKeyUp(agents: List<Agent>, currentSlot: Int): Boolean {
        handler.removeCallbacks(longPressRunnable)
        if (!longPressTriggered) {
            val next = TargetCycler().cycle(agents, currentSlot)
            if (next != null) {
                haptics.shortPulse()
                onCycleTarget(next)
            }
        }
        return true
    }
}
