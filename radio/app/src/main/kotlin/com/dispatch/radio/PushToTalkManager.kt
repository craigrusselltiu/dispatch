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
 * Manages push-to-talk via Android SpeechRecognizer (dispatch-88k.2).
 *
 * Volume Down key down: start recognition, emit [onListeningStart]
 * While held: partial results emitted via [onPartialResult]
 * Volume Down key up: stop recognition, final result emitted via [onFinalResult]
 * Empty transcript: [onEmptyTranscript] fires (caller should double-pulse vibrate)
 *
 * If the recognizer auto-completes while the button is still held (some Android
 * implementations ignore the silence-length extras), the transcript is accumulated
 * and recognition restarts transparently so the user's speech is not cut off.
 */
class PushToTalkManager(
    private val context: Context,
    private val locale: String = "en-US",
    val onListeningStart: () -> Unit,
    val onPartialResult: (String) -> Unit,
    val onFinalResult: (String) -> Unit,
    val onEmptyTranscript: () -> Unit,
    val onError: (Int) -> Unit,
    val onRmsChanged: (Float) -> Unit = {}
) {

    private var recognizer: SpeechRecognizer? = null
    private var listening = false
    private var lastPartial = ""
    private var resultDelivered = false

    /** Transcript accumulated across recognizer auto-restarts within a single PTT hold. */
    private var accumulatedTranscript = ""

    /** Number of times the recognizer has auto-restarted within the current PTT hold. */
    private var restartCount = 0

    /** Timestamp (ms) when the current PTT hold started. */
    private var holdStartTime = 0L

    private val handler = Handler(Looper.getMainLooper())

    /** Call from onKeyDown for KEYCODE_VOLUME_DOWN. */
    fun startListening() {
        if (listening) return
        listening = true
        lastPartial = ""
        resultDelivered = false
        accumulatedTranscript = ""
        restartCount = 0
        holdStartTime = System.currentTimeMillis()

        ensureRecognizer()
        recognizer?.startListening(buildRecognizerIntent())
        onListeningStart()
    }

    /** Call from onKeyUp for KEYCODE_VOLUME_DOWN. */
    fun stopListening() {
        if (!listening) return
        listening = false
        recognizer?.stopListening()

        // Safety net: if the recognizer already auto-stopped (and the restart hasn't
        // produced a callback yet), deliver whatever we've accumulated after a brief wait.
        handler.postDelayed(::deliverIfPending, STOP_SAFETY_DELAY_MS)
    }

    fun destroy() {
        handler.removeCallbacksAndMessages(null)
        recognizer?.destroy()
        recognizer = null
        listening = false
    }

    private fun ensureRecognizer() {
        if (recognizer == null) {
            recognizer = SpeechRecognizer.createSpeechRecognizer(context)
            recognizer?.setRecognitionListener(recognitionListener)
        }
    }

    private fun buildRecognizerIntent() = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
        putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
        putExtra(RecognizerIntent.EXTRA_LANGUAGE, locale)
        putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
        // Suppress silence-based cutoff while key is held
        putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, 60000L)
        putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS, 60000L)
        // Keep minimum length low so the recognizer starts delivering partial results
        // immediately rather than buffering audio for a long expected utterance.
        // The silence-length extras above already prevent auto-stop.
        putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_MINIMUM_LENGTH_MILLIS, 50L)
    }

    /** Whether the recognizer can auto-restart (within hold-duration and restart-count limits). */
    private fun canRestart(): Boolean {
        if (restartCount >= MAX_RESTARTS) return false
        if (System.currentTimeMillis() - holdStartTime >= MAX_HOLD_DURATION_MS) return false
        return true
    }

    /** Combine accumulated transcript with the current segment. */
    private fun combinedTranscript(current: String?): String {
        val cur = current?.trim() ?: ""
        return if (accumulatedTranscript.isBlank()) cur
        else if (cur.isBlank()) accumulatedTranscript
        else "$accumulatedTranscript $cur"
    }

    /** Deliver the final combined transcript if not already delivered. */
    private fun deliverIfPending() {
        if (resultDelivered) return
        resultDelivered = true
        handler.removeCallbacksAndMessages(null)

        val full = combinedTranscript(lastPartial)
        if (full.isBlank()) {
            onEmptyTranscript()
        } else {
            onFinalResult(full)
        }
    }

    private val recognitionListener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {}
        override fun onBeginningOfSpeech() {}

        override fun onRmsChanged(rmsdB: Float) {
            // Normalize RMS to 0.0-1.0 range. SpeechRecognizer reports roughly -2 to 10 dB.
            val normalized = ((rmsdB + 2f) / 12f).coerceIn(0f, 1f)
            this@PushToTalkManager.onRmsChanged(normalized)
        }

        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}

        override fun onPartialResults(partialResults: Bundle?) {
            val partial = partialResults
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull() ?: return
            lastPartial = partial
            onPartialResult(combinedTranscript(partial))
        }

        override fun onResults(results: Bundle?) {
            if (resultDelivered) return

            val transcript = results
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull()
                ?: lastPartial

            if (listening && canRestart()) {
                // Recognizer auto-completed while button still held.
                // Accumulate what we have and restart to keep capturing speech.
                restartCount++
                accumulatedTranscript = combinedTranscript(transcript)
                lastPartial = ""
                recognizer?.startListening(buildRecognizerIntent())
            } else {
                // Button released or restart limit reached — deliver the final combined result.
                listening = false
                resultDelivered = true
                handler.removeCallbacksAndMessages(null)
                val full = combinedTranscript(transcript)
                if (full.isNullOrBlank()) {
                    onEmptyTranscript()
                } else {
                    onFinalResult(full)
                }
            }
        }

        override fun onError(error: Int) {
            if (resultDelivered) return

            if (listening && canRestart()) {
                // Recognizer errored while button still held.
                // Save any partial transcript and restart.
                restartCount++
                if (lastPartial.isNotBlank()) {
                    accumulatedTranscript = combinedTranscript(lastPartial)
                    lastPartial = ""
                }
                recognizer?.startListening(buildRecognizerIntent())
            } else {
                // Button released or restart limit reached — deliver whatever we have.
                listening = false
                deliverIfPending()
            }
        }

        override fun onEvent(eventType: Int, params: Bundle?) {}
    }

    companion object {
        /** Grace period for the safety-net delivery after stopListening(). */
        private const val STOP_SAFETY_DELAY_MS = 500L

        /** Maximum number of auto-restarts within a single PTT hold. */
        private const val MAX_RESTARTS = 5

        /** Maximum duration (ms) for a single PTT hold before auto-delivering. */
        private const val MAX_HOLD_DURATION_MS = 120_000L
    }
}
