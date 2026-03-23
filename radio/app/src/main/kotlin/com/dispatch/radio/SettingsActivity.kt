package com.dispatch.radio

import android.os.Bundle
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Settings screen (dispatch-88k.7).
 *
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

        loadSettings()

        btnSave.setOnClickListener { saveSettings() }
        btnReset.setOnClickListener { resetToDefaults() }
        btnDiscover.setOnClickListener { onDiscoverClicked() }
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
    }

    override fun onDestroy() {
        super.onDestroy()
        discovery.stopDiscovery()
    }

    private fun saveSettings() {
        // Stop discovery and cancel pending callbacks before saving
        discovery.stopDiscovery()
        etHost.removeCallbacks(null)

        settings.consoleHost = etHost.text.toString().trim().ifEmpty { "192.168.1.1" }
        settings.consolePort = etPort.text.toString().trim().toIntOrNull() ?: 9800
        settings.psk = etPsk.text.toString().trim()
        settings.speechLocale = etLocale.text.toString().trim().ifEmpty { "en-US" }
        settings.hapticEnabled = cbHaptic.isChecked
        settings.keepScreenOn = cbScreenOn.isChecked
        settings.continuousListening = cbContinuous.isChecked

        setResult(RESULT_OK)
        finish()
    }
}
