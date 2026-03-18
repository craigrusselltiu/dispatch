package com.dispatch.radio

import android.view.KeyEvent
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class VolumeKeyBridgeTest {

    @Before
    fun setUp() {
        VolumeKeyBridge.isActivityInForeground = false
        VolumeKeyBridge.onKeyEvent = null
    }

    @After
    fun tearDown() {
        VolumeKeyBridge.isActivityInForeground = false
        VolumeKeyBridge.onKeyEvent = null
    }

    @Test
    fun `foreground flag defaults to false`() {
        assertFalse(VolumeKeyBridge.isActivityInForeground)
    }

    @Test
    fun `onKeyEvent defaults to null`() {
        assertNull(VolumeKeyBridge.onKeyEvent)
    }

    @Test
    fun `callback receives forwarded key events`() {
        val received = mutableListOf<Int>()
        VolumeKeyBridge.onKeyEvent = { event ->
            received.add(event.keyCode)
            true
        }

        val event = KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_VOLUME_DOWN)
        val consumed = VolumeKeyBridge.onKeyEvent?.invoke(event) ?: false

        assertTrue(consumed)
        assertEquals(listOf(KeyEvent.KEYCODE_VOLUME_DOWN), received)
    }

    @Test
    fun `foreground flag can be toggled`() {
        VolumeKeyBridge.isActivityInForeground = true
        assertTrue(VolumeKeyBridge.isActivityInForeground)
        VolumeKeyBridge.isActivityInForeground = false
        assertFalse(VolumeKeyBridge.isActivityInForeground)
    }
}
