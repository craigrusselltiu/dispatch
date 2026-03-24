package com.dispatch.radio

import android.app.AlertDialog
import android.graphics.Typeface
import android.os.Bundle
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Settings screen (dispatch-88k.7).
 *
 * - Connection profile saving and switching
 * - mDNS console discovery (dispatch-ct2.1)
 * - Console IP + port
 * - Pre-shared key (manual entry)
 * - Haptic feedback toggle (default on)
 * - Keep screen on toggle (default on)
 * - Speech recognition locale (default en-US)
 */
class SettingsActivity : AppCompatActivity() {

    private lateinit var settings: RadioSettings
    private lateinit var discovery: ConsoleDiscovery

    private lateinit var etHost: EditText
    private lateinit var etPort: EditText
    private lateinit var etPsk: EditText
    private lateinit var etLocale: EditText
    private lateinit var cbHaptic: CheckBox
    private lateinit var cbScreenOn: CheckBox
    private lateinit var cbContinuous: CheckBox
    private lateinit var btnSave: Button
    private lateinit var btnReset: Button
    private lateinit var btnDiscover: Button
    private lateinit var tvDiscoverStatus: TextView
    private lateinit var tvActiveProfile: TextView
    private lateinit var btnSaveProfile: Button
    private lateinit var btnLoadProfile: Button
    private lateinit var llProfiles: LinearLayout

    /** Values loaded from settings, used to detect manual edits. */
    private var loadedHost = ""
    private var loadedPort = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_settings)

        settings = RadioSettings(this)
        discovery = ConsoleDiscovery(this)

        etHost = findViewById(R.id.et_host)
        etPort = findViewById(R.id.et_port)
        etPsk = findViewById(R.id.et_psk)
        etLocale = findViewById(R.id.et_locale)
        cbHaptic = findViewById(R.id.cb_haptic)
        cbScreenOn = findViewById(R.id.cb_screen_on)
        cbContinuous = findViewById(R.id.cb_continuous)
        btnSave = findViewById(R.id.btn_save)
        btnReset = findViewById(R.id.btn_reset)
        btnDiscover = findViewById(R.id.btn_discover)
        tvDiscoverStatus = findViewById(R.id.tv_discover_status)
        tvActiveProfile = findViewById(R.id.tv_active_profile)
        btnSaveProfile = findViewById(R.id.btn_save_profile)
        btnLoadProfile = findViewById(R.id.btn_load_profile)
        llProfiles = findViewById(R.id.ll_profiles)

        loadSettings()
        refreshProfileList()

        btnSave.setOnClickListener { saveSettings() }
        btnReset.setOnClickListener { resetToDefaults() }
        btnDiscover.setOnClickListener { onDiscoverClicked() }
        btnSaveProfile.setOnClickListener { onSaveProfileClicked() }
        btnLoadProfile.setOnClickListener { onLoadProfileClicked() }
    }

    private fun onDiscoverClicked() {
        // Capture whether the user has manually edited the address fields before
        // starting discovery, so we don't overwrite their changes with a stale
        // mDNS result from the old console.
        val userEditedAddress = etHost.text.toString().trim() != loadedHost ||
            etPort.text.toString().trim() != loadedPort

        tvDiscoverStatus.text = "SCANNING..."
        btnDiscover.isEnabled = false
        discovery.startDiscovery(object : ConsoleDiscovery.Listener {
            override fun onConsoleFound(console: ConsoleDiscovery.Console) {
                discovery.stopDiscovery()
                if (!userEditedAddress) {
                    etHost.setText(console.host)
                    etPort.setText(console.port.toString())
                }
                tvDiscoverStatus.text = "FOUND: ${console.name} (${console.host}:${console.port})"
                btnDiscover.isEnabled = true
            }

            override fun onDiscoveryStopped() {
                btnDiscover.isEnabled = true
            }
        })

        // Timeout after 5 seconds if nothing found
        etHost.postDelayed({
            if (!btnDiscover.isEnabled) {
                discovery.stopDiscovery()
                tvDiscoverStatus.text = "NO CONSOLE FOUND"
                btnDiscover.isEnabled = true
            }
        }, 5000)
    }

    // ── Connection Profiles ──────────────────────────────────────────────

    private fun onSaveProfileClicked() {
        val input = EditText(this).apply {
            hint = "profile name"
            inputType = android.text.InputType.TYPE_CLASS_TEXT
            setTextColor(getColor(R.color.white))
            setHintTextColor(getColor(R.color.dim_grey))
            typeface = Typeface.MONOSPACE
            background = getDrawable(R.drawable.input_background)
            setPadding(24, 24, 24, 24)
            // Pre-fill with active profile name if one is set
            settings.activeProfile?.let { setText(it) }
        }

        AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog)
            .setTitle("Save connection profile")
            .setView(input)
            .setPositiveButton("SAVE") { _, _ ->
                val name = input.text.toString().trim().lowercase()
                    .replace(Regex("[^a-z0-9-]"), "-")
                    .replace(Regex("-+"), "-")
                    .trim('-')
                if (name.isEmpty()) return@setPositiveButton

                val host = etHost.text.toString().trim().ifEmpty { "192.168.1.1" }
                val port = etPort.text.toString().trim().toIntOrNull() ?: 9800
                val psk = etPsk.text.toString().trim()

                settings.saveProfile(
                    RadioSettings.ConnectionProfile(name, host, port, psk)
                )
                settings.activeProfile = name
                refreshProfileList()
            }
            .setNegativeButton("CANCEL", null)
            .show()
    }

    private fun onLoadProfileClicked() {
        val profiles = settings.listProfiles()
        if (profiles.isEmpty()) {
            tvActiveProfile.text = "NO SAVED PROFILES"
            tvActiveProfile.setTextColor(getColor(R.color.dim_grey))
            return
        }

        val names = profiles.map { it.name }.toTypedArray()
        AlertDialog.Builder(this, android.R.style.Theme_DeviceDefault_Dialog)
            .setTitle("Load connection profile")
            .setItems(names) { _, which ->
                val profile = profiles[which]
                etHost.setText(profile.host)
                etPort.setText(profile.port.toString())
                etPsk.setText(profile.psk)
                settings.activeProfile = profile.name
                loadedHost = profile.host
                loadedPort = profile.port.toString()
                refreshProfileList()
            }
            .setNegativeButton("CANCEL", null)
            .show()
    }

    private fun refreshProfileList() {
        val active = settings.activeProfile
        if (active != null) {
            tvActiveProfile.text = "ACTIVE: $active"
            tvActiveProfile.setTextColor(getColor(R.color.green))
        } else {
            tvActiveProfile.text = "NO ACTIVE PROFILE"
            tvActiveProfile.setTextColor(getColor(R.color.dim_grey))
        }

        llProfiles.removeAllViews()
        val profiles = settings.listProfiles()
        for (profile in profiles) {
            val row = LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                setPadding(0, 4, 0, 4)
            }

            val label = TextView(this).apply {
                text = "${profile.name}  ${profile.host}:${profile.port}"
                textSize = 11f
                typeface = Typeface.MONOSPACE
                setTextColor(
                    if (profile.name == active) getColor(R.color.green)
                    else getColor(R.color.white)
                )
                layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
            }

            val btnDelete = TextView(this).apply {
                text = "DEL"
                textSize = 11f
                typeface = Typeface.MONOSPACE
                setTextColor(getColor(R.color.red))
                setPadding(16, 0, 0, 0)
                setOnClickListener {
                    settings.deleteProfile(profile.name)
                    refreshProfileList()
                }
            }

            row.addView(label)
            row.addView(btnDelete)
            llProfiles.addView(row)
        }
    }

    // ── Settings load/save ───────────────────────────────────────────────

    private fun loadSettings() {
        etHost.setText(settings.consoleHost)
        etPort.setText(settings.consolePort.toString())
        loadedHost = settings.consoleHost
        loadedPort = settings.consolePort.toString()
        etPsk.setText(settings.psk)
        etLocale.setText(settings.speechLocale)
        cbHaptic.isChecked = settings.hapticEnabled
        cbScreenOn.isChecked = settings.keepScreenOn
        cbContinuous.isChecked = settings.continuousListening
    }

    private fun resetToDefaults() {
        settings.resetToDefaults()
        loadSettings()
        refreshProfileList()
    }

    override fun onDestroy() {
        super.onDestroy()
        discovery.stopDiscovery()
    }

    private fun saveSettings() {
        // Stop discovery and cancel pending callbacks before saving
        discovery.stopDiscovery()
        etHost.removeCallbacks(null)

        val newHost = etHost.text.toString().trim().ifEmpty { "192.168.1.1" }
        val newPort = etPort.text.toString().trim().toIntOrNull() ?: 9800
        val newPsk = etPsk.text.toString().trim()

        settings.consoleHost = newHost
        settings.consolePort = newPort
        settings.psk = newPsk
        settings.speechLocale = etLocale.text.toString().trim().ifEmpty { "en-US" }
        settings.hapticEnabled = cbHaptic.isChecked
        settings.keepScreenOn = cbScreenOn.isChecked
        settings.continuousListening = cbContinuous.isChecked

        // Clear active profile if connection fields changed from what the profile had
        val active = settings.activeProfile
        if (active != null) {
            val profile = settings.listProfiles().find { it.name == active }
            if (profile != null &&
                (profile.host != newHost || profile.port != newPort || profile.psk != newPsk)) {
                settings.activeProfile = null
            }
        }

        setResult(RESULT_OK)
        finish()
    }
}
