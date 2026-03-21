package com.dispatch.radio

import android.content.Context
import android.os.Handler
import android.os.Looper
import com.dispatch.radio.model.Agent

/**
 * Handles Volume Up key events for agent status and quick dispatch.
 *
 * Hold (key down): immediately show agent status overlay listing all agents
 *   with their busy/idle state. Dismissed on key up.
 *
 * Long press (key down held > 1s): status overlay dismissed, agent type picker
 *   overlay shown for quick dispatch.
 *
 * Usage:
 *   - Call [onKeyDown] from Activity.onKeyDown for KEYCODE_VOLUME_UP
 *   - Call [onKeyUp] from Activity.onKeyUp for KEYCODE_VOLUME_UP
 *   - Both return true to consume the event and suppress system volume UI
 */
class VolumeUpHandler(
    private val context: Context,
    private val haptics: HapticFeedback,
    private val onQuickDispatch: (tool: String) -> Unit
) {
    companion object {
        private const val LONG_PRESS_MS = 1000L
    }

    private val handler = Handler(Looper.getMainLooper())
    private var longPressTriggered = false
    private var isKeyDown = false
    private var statusOverlay: AgentStatusOverlay? = null

    private val longPressRunnable = Runnable {
        longPressTriggered = true
        statusOverlay?.dismiss()
        statusOverlay = null
        // Overlay requires a foreground activity — skip when backgrounded (dispatch-ct2.7)
        if (VolumeKeyBridge.isActivityInForeground) {
            QuickDispatchOverlay(context).show { tool ->
                onQuickDispatch(tool)
            }
        }
    }

    /** Call from Activity.onKeyDown for KEYCODE_VOLUME_UP. Returns true to consume. */
    fun onKeyDown(agents: List<Agent>): Boolean {
        if (isKeyDown) return true // Ignore key-repeat events while held
        isKeyDown = true
        longPressTriggered = false
        handler.postDelayed(longPressRunnable, LONG_PRESS_MS)
        if (VolumeKeyBridge.isActivityInForeground) {
            statusOverlay = AgentStatusOverlay(context).also { it.show(agents) }
        }
        return true
    }

    /** Call from Activity.onKeyUp for KEYCODE_VOLUME_UP. Returns true to consume. */
    fun onKeyUp(): Boolean {
        isKeyDown = false
        handler.removeCallbacks(longPressRunnable)
        if (!longPressTriggered) {
            haptics.shortPulse()
        }
        statusOverlay?.dismiss()
        statusOverlay = null
        return true
    }
}
