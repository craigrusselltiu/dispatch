package com.dispatch.radio

import android.accessibilityservice.AccessibilityService
import android.view.KeyEvent
import android.view.accessibility.AccessibilityEvent

/**
 * AccessibilityService that intercepts volume button events when the screen is off
 * or the app is backgrounded (dispatch-ct2.7).
 *
 * When [MainActivity] is in the foreground, key events pass through to the activity's
 * onKeyDown/onKeyUp as normal. When the activity is NOT in the foreground, this service
 * consumes volume key events and forwards them via [VolumeKeyBridge] so PTT and
 * target cycling continue to work hands-free.
 *
 * The user must enable this service in Android Settings > Accessibility.
 */
class VolumeKeyAccessibilityService : AccessibilityService() {

    override fun onKeyEvent(event: KeyEvent): Boolean {
        if (VolumeKeyBridge.isActivityInForeground) return false

        val keyCode = event.keyCode
        if (keyCode != KeyEvent.KEYCODE_VOLUME_DOWN && keyCode != KeyEvent.KEYCODE_VOLUME_UP) {
            return false
        }

        return VolumeKeyBridge.onKeyEvent?.invoke(event) ?: false
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        // Not used — we only need key event filtering
    }

    override fun onInterrupt() {
        // Required override — nothing to clean up
    }
}
