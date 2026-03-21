package com.dispatch.radio

import android.content.Context
import com.dispatch.radio.model.Agent

/**
 * Handles Volume Up key events for agent status overlay.
 *
 * Hold (key down): immediately show agent status overlay listing all agents
 *   with their busy/idle state. Stays visible for the entire hold duration.
 *
 * Release (key up): dismiss the overlay, short vibration.
 *
 * Usage:
 *   - Call [onKeyDown] from Activity.onKeyDown for KEYCODE_VOLUME_UP
 *   - Call [onKeyUp] from Activity.onKeyUp for KEYCODE_VOLUME_UP
 *   - Both return true to consume the event and suppress system volume UI
 */
class VolumeUpHandler(
    private val context: Context,
    private val haptics: HapticFeedback,
) {
    private var isKeyDown = false
    private var statusOverlay: AgentStatusOverlay? = null

    /** Call from Activity.onKeyDown for KEYCODE_VOLUME_UP. Returns true to consume. */
    fun onKeyDown(agents: List<Agent>): Boolean {
        if (isKeyDown) return true // Ignore key-repeat events while held
        isKeyDown = true
        if (VolumeKeyBridge.isActivityInForeground) {
            statusOverlay = AgentStatusOverlay(context).also { it.show(agents) }
        }
        return true
    }

    /** Call from Activity.onKeyUp for KEYCODE_VOLUME_UP. Returns true to consume. */
    fun onKeyUp(): Boolean {
        isKeyDown = false
        haptics.shortPulse()
        statusOverlay?.dismiss()
        statusOverlay = null
        return true
    }
}
