package com.dispatch.radio

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject

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

    var keepScreenOn: Boolean
        get() = prefs.getBoolean(KEY_SCREEN_ON, true)
        set(value) = prefs.edit().putBoolean(KEY_SCREEN_ON, value).apply()

    var speechLocale: String
        get() = prefs.getString(KEY_LOCALE, DEFAULT_LOCALE) ?: DEFAULT_LOCALE
        set(value) = prefs.edit().putString(KEY_LOCALE, value).apply()

    var continuousListening: Boolean
        get() = prefs.getBoolean(KEY_CONTINUOUS, false)
        set(value) = prefs.edit().putBoolean(KEY_CONTINUOUS, value).apply()

    /** Clears all preferences and restores defaults. */
    fun resetToDefaults() {
        // Preserve profiles and active profile across resets
        val savedProfiles = prefs.getString(KEY_PROFILES, null)
        val savedActive = prefs.getString(KEY_ACTIVE_PROFILE, null)
        prefs.edit().clear().apply()
        savedProfiles?.let { prefs.edit().putString(KEY_PROFILES, it).apply() }
        savedActive?.let { prefs.edit().putString(KEY_ACTIVE_PROFILE, it).apply() }
    }

    /** TLS certificate fingerprint (SHA-256 hex). */
    var certFingerprint: String?
        get() = prefs.getString(KEY_CERT_FP, null)
        set(value) = prefs.edit().putString(KEY_CERT_FP, value).apply()

    // ── Connection Profiles ──────────────────────────────────────────────

    /** Name of the currently active profile, or null if none. */
    var activeProfile: String?
        get() = prefs.getString(KEY_ACTIVE_PROFILE, null)
        set(value) {
            if (value == null) prefs.edit().remove(KEY_ACTIVE_PROFILE).apply()
            else prefs.edit().putString(KEY_ACTIVE_PROFILE, value).apply()
        }

    /** A saved connection profile: name, host, port, and PSK. */
    data class ConnectionProfile(val name: String, val host: String, val port: Int, val psk: String)

    /** Returns all saved profiles sorted by name. */
    fun listProfiles(): List<ConnectionProfile> {
        val json = prefs.getString(KEY_PROFILES, null) ?: return emptyList()
        val arr = JSONArray(json)
        return (0 until arr.length()).map { i ->
            val obj = arr.getJSONObject(i)
            ConnectionProfile(
                name = obj.getString("name"),
                host = obj.getString("host"),
                port = obj.getInt("port"),
                psk = obj.getString("psk")
            )
        }.sortedBy { it.name }
    }

    /**
     * Save (or overwrite) a connection profile. If a profile with the same
     * name already exists it is replaced.
     */
    fun saveProfile(profile: ConnectionProfile) {
        val profiles = listProfiles().toMutableList()
        profiles.removeAll { it.name == profile.name }
        profiles.add(profile)
        writeProfiles(profiles)
    }

    /** Delete a profile by name. Clears activeProfile if it matches. */
    fun deleteProfile(name: String) {
        val profiles = listProfiles().toMutableList()
        profiles.removeAll { it.name == name }
        writeProfiles(profiles)
        if (activeProfile == name) activeProfile = null
    }

    /**
     * Load a profile: sets consoleHost, consolePort, and psk from the
     * saved profile and marks it as active.
     */
    fun loadProfile(name: String): Boolean {
        val profile = listProfiles().find { it.name == name } ?: return false
        consoleHost = profile.host
        consolePort = profile.port
        psk = profile.psk
        activeProfile = name
        return true
    }

    private fun writeProfiles(profiles: List<ConnectionProfile>) {
        val arr = JSONArray()
        for (p in profiles) {
            arr.put(JSONObject().apply {
                put("name", p.name)
                put("host", p.host)
                put("port", p.port)
                put("psk", p.psk)
            })
        }
        prefs.edit().putString(KEY_PROFILES, arr.toString()).apply()
    }

    companion object {
        private const val PREFS_NAME = "dispatch_radio"
        private const val KEY_HOST = "console_host"
        private const val KEY_PORT = "console_port"
        private const val KEY_PSK = "psk"
        private const val KEY_HAPTIC = "haptic_enabled"
        private const val KEY_SCREEN_ON = "keep_screen_on"
        private const val KEY_LOCALE = "speech_locale"
        private const val KEY_CONTINUOUS = "continuous_listening"
        private const val KEY_CERT_FP = "cert_fingerprint"
        private const val KEY_PROFILES = "connection_profiles"
        private const val KEY_ACTIVE_PROFILE = "active_profile"
        private const val DEFAULT_HOST = "192.168.1.1"
        private const val DEFAULT_PORT = 9800
        private const val DEFAULT_LOCALE = "en-US"
    }
}
