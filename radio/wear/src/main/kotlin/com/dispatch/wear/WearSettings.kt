package com.dispatch.wear

import android.content.Context
import android.content.SharedPreferences

class WearSettings(context: Context) {

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

    /** TLS certificate fingerprint (SHA-256 hex). */
    var certFingerprint: String?
        get() = prefs.getString(KEY_CERT_FP, null)
        set(value) = prefs.edit().putString(KEY_CERT_FP, value).apply()

    companion object {
        private const val PREFS_NAME = "dispatch_wear"
        private const val KEY_HOST = "console_host"
        private const val KEY_PORT = "console_port"
        private const val KEY_PSK = "psk"
        private const val KEY_CERT_FP = "cert_fingerprint"
        private const val DEFAULT_HOST = "192.168.1.1"
        private const val DEFAULT_PORT = 9800
    }
}
