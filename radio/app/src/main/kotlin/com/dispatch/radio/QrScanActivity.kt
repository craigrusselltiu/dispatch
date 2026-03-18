package com.dispatch.radio

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.util.Log
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors

/**
 * QR code scanner activity (dispatch-ct2.2).
 *
 * Uses CameraX + ML Kit Barcode Scanning to read a QR code displayed by the
 * console TUI. The QR encodes a WebSocket URL: ws://host:port/?psk=<key>
 *
 * On successful scan, returns host, port, and PSK to the caller via extras.
 */
class QrScanActivity : AppCompatActivity() {

    companion object {
        const val EXTRA_PSK = "extra_psk"
        const val EXTRA_HOST = "extra_host"
        const val EXTRA_PORT = "extra_port"
        private const val TAG = "QrScan"
    }

    private var scanned = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_qr_scan)

        findViewById<Button>(R.id.btn_cancel).setOnClickListener {
            setResult(Activity.RESULT_CANCELED)
            finish()
        }

        startCamera()
    }

    @androidx.camera.core.ExperimentalGetImage
    private fun startCamera() {
        val cameraProviderFuture = ProcessCameraProvider.getInstance(this)
        cameraProviderFuture.addListener({
            val cameraProvider = cameraProviderFuture.get()

            val preview = Preview.Builder().build().also {
                it.surfaceProvider = findViewById<PreviewView>(R.id.preview_view).surfaceProvider
            }

            val scanner = BarcodeScanning.getClient()
            val analysis = ImageAnalysis.Builder()
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()

            analysis.setAnalyzer(Executors.newSingleThreadExecutor()) { imageProxy ->
                val mediaImage = imageProxy.image
                if (mediaImage != null && !scanned) {
                    val inputImage = InputImage.fromMediaImage(
                        mediaImage, imageProxy.imageInfo.rotationDegrees
                    )
                    scanner.process(inputImage)
                        .addOnSuccessListener { barcodes ->
                            for (barcode in barcodes) {
                                if (barcode.format == Barcode.FORMAT_QR_CODE) {
                                    barcode.rawValue?.let { handleQrValue(it) }
                                }
                            }
                        }
                        .addOnCompleteListener { imageProxy.close() }
                } else {
                    imageProxy.close()
                }
            }

            try {
                cameraProvider.unbindAll()
                cameraProvider.bindToLifecycle(
                    this, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis
                )
            } catch (e: Exception) {
                Log.e(TAG, "Camera bind failed", e)
            }
        }, ContextCompat.getMainExecutor(this))
    }

    /**
     * Parse scanned QR value. Expected format: ws://host:port/?psk=<key>
     */
    private fun handleQrValue(value: String) {
        if (scanned) return
        scanned = true

        try {
            val uri = Uri.parse(value)
            val host = uri.host
            val port = uri.port
            val psk = uri.getQueryParameter("psk")

            if (host != null && port > 0 && !psk.isNullOrEmpty()) {
                val data = Intent().apply {
                    putExtra(EXTRA_HOST, host)
                    putExtra(EXTRA_PORT, port)
                    putExtra(EXTRA_PSK, psk)
                }
                setResult(Activity.RESULT_OK, data)
                finish()
                return
            }
        } catch (e: Exception) {
            Log.w(TAG, "Failed to parse QR URI: $value", e)
        }

        // Fallback: treat entire value as PSK
        val data = Intent().apply {
            putExtra(EXTRA_PSK, value)
        }
        setResult(Activity.RESULT_OK, data)
        finish()
    }
}
