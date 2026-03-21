package com.dispatch.radio

import android.content.Context
import android.content.SharedPreferences

/**
 * Persistent settings for Dispatch Radio (dispatch-88k.7).
 */
class RadioSettings(context: Context) {

    private val prefs: SharedPreferences =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    var consoleHost: String
        get() = prefs.getString(KEY_HOST, DEFAULT_HOST) ?: DEFAULT_HOST
        set(value) = prefs.edit().putString(KEY_HOST, value).apply()

    var consolePort: Int
        get() = prefs.getInt(KEY_PORT, DEFAULT_PORT)
        set(value) = prefs.edit().putInt(KEY_PORT, value).apply()

    var psk: String
        get() = prefs.getString(KEY_PSK, "") ?: ""
        set(value) = prefs.edit().putString(KEY_PSK, value).apply()

    var hapticEnabled: Boolean
        get() = prefs.getBoolean(KEY_HAPTIC, true)
        set(value) = prefs.edit().putBoolean(KEY_HAPTIC, value).apply()

    var confirmBeforeSend: Boolean
        get() = prefs.getBoolean(KEY_CONFIRM, false)
        set(value) = prefs.edit().putBoolean(KEY_CONFIRM, value).apply()

    var keepScreenOn: Boolean
        get() = prefs.getBoolean(KEY_SCREEN_ON, true)
        set(value) = prefs.edit().putBoolean(KEY_SCREEN_ON, value).apply()

    var speechLocale: String
        get() = prefs.getString(KEY_LOCALE, DEFAULT_LOCALE) ?: DEFAULT_LOCALE
        set(value) = prefs.edit().putString(KEY_LOCALE, value).apply()

    var continuousListening: Boolean
        get() = prefs.getBoolean(KEY_CONTINUOUS, false)
        set(value) = prefs.edit().putBoolean(KEY_CONTINUOUS, value).apply()

    /** TLS certificate fingerprint (SHA-256 hex). Set via QR scan. */
    var certFingerprint: String?
        get() = prefs.getString(KEY_CERT_FP, null)
        set(value) = prefs.edit().putString(KEY_CERT_FP, value).apply()

    companion object {
        private const val PREFS_NAME = "dispatch_radio"
        private const val KEY_HOST = "console_host"
        private const val KEY_PORT = "console_port"
        private const val KEY_PSK = "psk"
        private const val KEY_HAPTIC = "haptic_enabled"
        private const val KEY_CONFIRM = "confirm_before_send"
        private const val KEY_SCREEN_ON = "keep_screen_on"
        private const val KEY_LOCALE = "speech_locale"
        private const val KEY_CONTINUOUS = "continuous_listening"
        private const val KEY_CERT_FP = "cert_fingerprint"
        private const val DEFAULT_HOST = "192.168.1.1"
        private const val DEFAULT_PORT = 9800
        private const val DEFAULT_LOCALE = "en-US"
    }
}
