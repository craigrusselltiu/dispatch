package com.dispatch.wear

import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import androidx.fragment.app.FragmentActivity

/**
 * Minimal settings screen for the Wear companion.
 * Configures console host, port, and PSK.
 */
class WearSettingsActivity : FragmentActivity() {

    private lateinit var settings: WearSettings

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_wear_settings)

        settings = WearSettings(this)

        val etHost = findViewById<EditText>(R.id.et_host)
        val etPort = findViewById<EditText>(R.id.et_port)
        val etPsk = findViewById<EditText>(R.id.et_psk)
        val btnSave = findViewById<Button>(R.id.btn_save)

        etHost.setText(settings.consoleHost)
        etPort.setText(settings.consolePort.toString())
        etPsk.setText(settings.psk)

        btnSave.setOnClickListener {
            settings.consoleHost = etHost.text.toString().trim()
            settings.consolePort = etPort.text.toString().toIntOrNull() ?: 9800
            settings.psk = etPsk.text.toString().trim()
            finish()
        }
    }
}
