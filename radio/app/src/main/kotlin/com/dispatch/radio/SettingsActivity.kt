package com.dispatch.radio

import android.Manifest
import android.accessibilityservice.AccessibilityServiceInfo
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.provider.Settings
import android.view.WindowManager
import android.view.accessibility.AccessibilityManager
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat

/**
 * Settings screen (dispatch-88k.7).
 *
 * - mDNS console discovery (dispatch-ct2.1)
 * - Console IP + port
 * - Pre-shared key (manual entry or QR scan)
 * - Haptic feedback toggle (default on)
 * - Confirm before send toggle (default off)
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
    private lateinit var cbConfirm: CheckBox
    private lateinit var cbScreenOn: CheckBox
    private lateinit var cbContinuous: CheckBox
    private lateinit var btnSave: Button
    private lateinit var btnScanQr: Button
    private lateinit var btnDiscover: Button
    private lateinit var tvDiscoverStatus: TextView
    private lateinit var btnAccessibility: Button
    private lateinit var tvAccessibilityStatus: TextView

    private val qrScanLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == RESULT_OK) {
            val data = result.data ?: return@registerForActivityResult
            data.getStringExtra(QrScanActivity.EXTRA_HOST)?.let { etHost.setText(it) }
            val port = data.getIntExtra(QrScanActivity.EXTRA_PORT, -1)
            if (port > 0) etPort.setText(port.toString())
            data.getStringExtra(QrScanActivity.EXTRA_PSK)?.let { etPsk.setText(it) }
            // Store TLS cert fingerprint from QR (dispatch-ct2.6)
            data.getStringExtra(QrScanActivity.EXTRA_CERT_FP)?.let {
                settings.certFingerprint = it
            }
        }
    }

    private val cameraPermissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { granted ->
        if (granted) launchQrScanner()
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
        cbConfirm = findViewById(R.id.cb_confirm)
        cbScreenOn = findViewById(R.id.cb_screen_on)
        cbContinuous = findViewById(R.id.cb_continuous)
        btnSave = findViewById(R.id.btn_save)
        btnScanQr = findViewById(R.id.btn_scan_qr)
        btnDiscover = findViewById(R.id.btn_discover)
        tvDiscoverStatus = findViewById(R.id.tv_discover_status)
        btnAccessibility = findViewById(R.id.btn_accessibility)
        tvAccessibilityStatus = findViewById(R.id.tv_accessibility_status)

        loadSettings()
        refreshAccessibilityStatus()

        btnSave.setOnClickListener { saveSettings() }
        btnScanQr.setOnClickListener { onScanQrClicked() }
        btnDiscover.setOnClickListener { onDiscoverClicked() }
        btnAccessibility.setOnClickListener {
            startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
        }
    }

    override fun onResume() {
        super.onResume()
        refreshAccessibilityStatus()
    }

    private fun isAccessibilityServiceEnabled(): Boolean {
        val am = getSystemService(ACCESSIBILITY_SERVICE) as AccessibilityManager
        return am.getEnabledAccessibilityServiceList(AccessibilityServiceInfo.FEEDBACK_GENERIC)
            .any { it.resolveInfo.serviceInfo.name == VolumeKeyAccessibilityService::class.java.name }
    }

    private fun refreshAccessibilityStatus() {
        val enabled = isAccessibilityServiceEnabled()
        tvAccessibilityStatus.text = if (enabled) "ENABLED" else "DISABLED"
        tvAccessibilityStatus.setTextColor(
            getColor(if (enabled) R.color.green else R.color.dim_grey)
        )
    }

    private fun onDiscoverClicked() {
        tvDiscoverStatus.text = "SCANNING..."
        btnDiscover.isEnabled = false
        discovery.startDiscovery(object : ConsoleDiscovery.Listener {
            override fun onConsoleFound(console: ConsoleDiscovery.Console) {
                discovery.stopDiscovery()
                etHost.setText(console.host)
                etPort.setText(console.port.toString())
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

    private fun onScanQrClicked() {
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA)
            == PackageManager.PERMISSION_GRANTED
        ) {
            launchQrScanner()
        } else {
            cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
        }
    }

    private fun launchQrScanner() {
        qrScanLauncher.launch(Intent(this, QrScanActivity::class.java))
    }

    private fun loadSettings() {
        etHost.setText(settings.consoleHost)
        etPort.setText(settings.consolePort.toString())
        etPsk.setText(settings.psk)
        etLocale.setText(settings.speechLocale)
        cbHaptic.isChecked = settings.hapticEnabled
        cbConfirm.isChecked = settings.confirmBeforeSend
        cbScreenOn.isChecked = settings.keepScreenOn
        cbContinuous.isChecked = settings.continuousListening
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
        settings.confirmBeforeSend = cbConfirm.isChecked
        settings.keepScreenOn = cbScreenOn.isChecked
        settings.continuousListening = cbContinuous.isChecked

        setResult(RESULT_OK)
        finish()
    }
}
