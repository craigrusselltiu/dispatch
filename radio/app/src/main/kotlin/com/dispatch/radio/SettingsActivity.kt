package com.dispatch.radio

import android.app.AlertDialog
import android.graphics.Typeface
import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.widget.AdapterView
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.Spinner
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Settings screen (dispatch-88k.7).
 *
 * - Connection profile saving and switching (dropdown selector)
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
    private lateinit var spinnerProfiles: Spinner
    private lateinit var btnSaveProfile: Button
    private lateinit var btnDeleteProfile: Button

    /** Values loaded from settings, used to detect manual edits. */
    private var loadedHost = ""
    private var loadedPort = ""

    private val profileSelectionListener = object : AdapterView.OnItemSelectedListener {
        override fun onItemSelected(parent: AdapterView<*>?, view: View?, position: Int, id: Long) {
            val profiles = settings.listProfiles()
            if (profiles.isEmpty() || position >= profiles.size) return
            val profile = profiles[position]
            etHost.setText(profile.host)
            etPort.setText(profile.port.toString())
            etPsk.setText(profile.psk)
            settings.activeProfile = profile.name
            loadedHost = profile.host
            loadedPort = profile.port.toString()
        }
        override fun onNothingSelected(parent: AdapterView<*>?) {}
    }

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
        spinnerProfiles = findViewById(R.id.spinner_profiles)
        btnSaveProfile = findViewById(R.id.btn_save_profile)
        btnDeleteProfile = findViewById(R.id.btn_delete_profile)

        loadSettings()
        refreshProfileDropdown()

        btnSave.setOnClickListener { saveSettings() }
        btnReset.setOnClickListener { resetToDefaults() }
        btnDiscover.setOnClickListener { onDiscoverClicked() }
        btnSaveProfile.setOnClickListener { onSaveProfileClicked() }
        btnDeleteProfile.setOnClickListener { onDeleteProfileClicked() }
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
                // Auto-populate TLS cert fingerprint from mDNS TXT record.
                if (console.certFingerprint != null) {
                    settings.certFingerprint = console.certFingerprint
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

    // -- Connection Profiles --------------------------------------------------

    private fun onSaveProfileClicked() {
        val container = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 24, 48, 0)
        }

        val input = EditText(this).apply {
            hint = "profile name"
            inputType = android.text.InputType.TYPE_CLASS_TEXT
            setTextColor(getColor(R.color.white))
            setHintTextColor(getColor(R.color.dim_grey))
            typeface = Typeface.MONOSPACE
            background = getDrawable(R.drawable.input_background)
            setPadding(24, 24, 24, 24)
            settings.activeProfile?.let { setText(it) }
        }

        val hint = TextView(this).apply {
            text = "Letters, numbers, and dashes only."
            textSize = 10f
            typeface = Typeface.MONOSPACE
            setTextColor(getColor(R.color.dim_grey))
            setPadding(0, 12, 0, 0)
        }

        container.addView(input)
        container.addView(hint)

        val dialog = AlertDialog.Builder(this, R.style.Theme_DispatchRadio_Dialog)
            .setTitle("SAVE PROFILE")
            .setView(container)
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
                refreshProfileDropdown()
            }
            .setNegativeButton("CANCEL", null)
            .show()

        styleDialogButtons(dialog)
    }

    private fun onDeleteProfileClicked() {
        val profiles = settings.listProfiles()
        if (profiles.isEmpty()) return
        val position = spinnerProfiles.selectedItemPosition
        if (position < 0 || position >= profiles.size) return
        val profile = profiles[position]

        val dialog = AlertDialog.Builder(this, R.style.Theme_DispatchRadio_Dialog)
            .setTitle("DELETE PROFILE")
            .setMessage("Delete \"${profile.name}\"?")
            .setPositiveButton("DELETE") { _, _ ->
                settings.deleteProfile(profile.name)
                refreshProfileDropdown()
            }
            .setNegativeButton("CANCEL", null)
            .show()

        dialog.getButton(AlertDialog.BUTTON_POSITIVE)?.apply {
            setTextColor(getColor(R.color.red))
            typeface = Typeface.MONOSPACE
            isAllCaps = true
        }
        dialog.getButton(AlertDialog.BUTTON_NEGATIVE)?.apply {
            setTextColor(getColor(R.color.dim_grey))
            typeface = Typeface.MONOSPACE
            isAllCaps = true
        }
    }

    private fun refreshProfileDropdown() {
        // Detach listener to prevent auto-loading during adapter/selection changes
        spinnerProfiles.onItemSelectedListener = null

        val profiles = settings.listProfiles()
        val active = settings.activeProfile

        if (profiles.isEmpty()) {
            val adapter = profileAdapter(listOf("NO SAVED PROFILES"), dimmed = true)
            spinnerProfiles.adapter = adapter
            spinnerProfiles.isEnabled = false
            btnDeleteProfile.isEnabled = false
            btnDeleteProfile.alpha = 0.4f
        } else {
            val adapter = profileAdapter(profiles.map { it.name }, dimmed = false)
            spinnerProfiles.adapter = adapter
            spinnerProfiles.isEnabled = true
            btnDeleteProfile.isEnabled = true
            btnDeleteProfile.alpha = 1.0f

            if (active != null) {
                val idx = profiles.indexOfFirst { it.name == active }
                if (idx >= 0) spinnerProfiles.setSelection(idx)
            }
        }

        // Re-attach listener after pending layout passes to skip initial callbacks
        spinnerProfiles.post {
            spinnerProfiles.onItemSelectedListener = profileSelectionListener
        }
    }

    private fun profileAdapter(items: List<String>, dimmed: Boolean): ArrayAdapter<String> {
        val textColor = if (dimmed) R.color.dim_grey else R.color.white
        val adapter = object : ArrayAdapter<String>(
            this, android.R.layout.simple_spinner_item, items
        ) {
            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                return (super.getView(position, convertView, parent) as TextView).apply {
                    setTextColor(getColor(textColor))
                    typeface = Typeface.MONOSPACE
                    textSize = 12f
                    setPadding(24, 20, 24, 20)
                }
            }
            override fun getDropDownView(position: Int, convertView: View?, parent: ViewGroup): View {
                return (super.getDropDownView(position, convertView, parent) as TextView).apply {
                    setTextColor(getColor(textColor))
                    setBackgroundColor(getColor(R.color.background))
                    typeface = Typeface.MONOSPACE
                    textSize = 12f
                    setPadding(24, 28, 24, 28)
                }
            }
        }
        adapter.setDropDownViewResource(android.R.layout.simple_spinner_dropdown_item)
        return adapter
    }

    /** Style dialog buttons with proper contrast colors. */
    private fun styleDialogButtons(dialog: AlertDialog) {
        dialog.getButton(AlertDialog.BUTTON_POSITIVE)?.apply {
            setTextColor(getColor(R.color.green))
            typeface = Typeface.MONOSPACE
            isAllCaps = true
        }
        dialog.getButton(AlertDialog.BUTTON_NEGATIVE)?.apply {
            setTextColor(getColor(R.color.dim_grey))
            typeface = Typeface.MONOSPACE
            isAllCaps = true
        }
    }

    // -- Settings load/save ---------------------------------------------------

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
        refreshProfileDropdown()
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
