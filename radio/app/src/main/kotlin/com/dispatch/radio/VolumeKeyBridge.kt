package com.dispatch.radio

import android.view.KeyEvent

/**
 * Bridge between [VolumeKeyAccessibilityService] and [MainActivity].
 *
 * When the activity is in the foreground it handles volume keys via onKeyDown/onKeyUp
 * directly. When backgrounded or the screen is off, the AccessibilityService intercepts
 * volume keys and forwards them through this bridge so the activity's existing PTT and
 * target-cycling logic still fires.
 */
object VolumeKeyBridge {

    @Volatile
    var isActivityInForeground = false

    /**
     * Key event handler registered by [MainActivity].
     * Returns true if the event was consumed.
     */
    var onKeyEvent: ((KeyEvent) -> Boolean)? = null
}
