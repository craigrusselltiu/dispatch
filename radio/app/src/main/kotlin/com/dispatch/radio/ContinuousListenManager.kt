package com.dispatch.radio

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer

/**
 * Continuous listening mode with voice-activity detection (dispatch-ct2.3).
 *
 * Uses Android SpeechRecognizer's built-in silence detection as VAD.
 * Automatically restarts recognition after each utterance so the user
 * can speak multiple commands without holding Volume Down.
 *
 * Toggle on/off with [start] and [stop]. Volume Down tap acts as toggle
 * when continuous mode is enabled in settings.
 */
class ContinuousListenManager(
    private val context: Context,
    private val locale: String = "en-US",
    val onListeningStart: () -> Unit,
    val onPartialResult: (String) -> Unit,
    val onFinalResult: (String) -> Unit,
    val onEmptyTranscript: () -> Unit,
    val onError: (Int) -> Unit,
    val onRmsChanged: (Float) -> Unit
) {

    private var recognizer: SpeechRecognizer? = null
    private var active = false
    private var lastPartial = ""
    private val handler = Handler(Looper.getMainLooper())

    val isActive: Boolean get() = active

    /** Start continuous listening. Recognizer auto-restarts after each utterance. */
    fun start() {
        if (active) return
        active = true
        ensureRecognizer()
        startRecognition()
        onListeningStart()
    }

    /** Stop continuous listening. */
    fun stop() {
        if (!active) return
        active = false
        handler.removeCallbacksAndMessages(null)
        recognizer?.stopListening()
    }

    fun destroy() {
        active = false
        handler.removeCallbacksAndMessages(null)
        recognizer?.destroy()
        recognizer = null
    }

    private fun ensureRecognizer() {
        if (recognizer == null) {
            recognizer = SpeechRecognizer.createSpeechRecognizer(context)
            recognizer?.setRecognitionListener(recognitionListener)
        }
    }

    private fun startRecognition() {
        if (!active) return
        lastPartial = ""
        ensureRecognizer()

        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, locale)
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            // VAD: let the recognizer detect end-of-speech via silence
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, 3500L)
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS, 3000L)
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_MINIMUM_LENGTH_MILLIS, 500L)
        }

        recognizer?.startListening(intent)
    }

    /** Restart recognition after a brief pause to avoid rapid-fire restarts. */
    private fun restartAfterDelay() {
        if (!active) return
        handler.postDelayed({
            if (active) {
                startRecognition()
                onListeningStart()
            }
        }, RESTART_DELAY_MS)
    }

    private val recognitionListener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {}
        override fun onBeginningOfSpeech() {}

        override fun onRmsChanged(rmsdB: Float) {
            // Normalize RMS to 0.0-1.0 range. SpeechRecognizer reports roughly -2 to 10 dB.
            val normalized = ((rmsdB + 2f) / 12f).coerceIn(0f, 1f)
            onRmsChanged(normalized)
        }

        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}

        override fun onPartialResults(partialResults: Bundle?) {
            val partial = partialResults
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull() ?: return
            lastPartial = partial
            onPartialResult(partial)
        }

        override fun onResults(results: Bundle?) {
            val transcript = results
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull()
                ?: lastPartial

            if (transcript.isNullOrBlank()) {
                onEmptyTranscript()
            } else {
                onFinalResult(transcript)
            }

            // Auto-restart for next utterance
            restartAfterDelay()
        }

        override fun onError(error: Int) {
            when (error) {
                // No speech detected — normal in continuous mode, just restart
                SpeechRecognizer.ERROR_NO_MATCH,
                SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> {
                    restartAfterDelay()
                }
                else -> {
                    if (active) {
                        onError(error)
                        // Try to recover by restarting
                        restartAfterDelay()
                    }
                }
            }
        }

        override fun onEvent(eventType: Int, params: Bundle?) {}
    }

    companion object {
        private const val RESTART_DELAY_MS = 300L
    }
}
