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
    private val locale: String = "en-AU",
    val onListeningStart: () -> Unit,
    val onPartialResult: (String) -> Unit,
    val onFinalResult: (String) -> Unit,
    val onEmptyTranscript: () -> Unit,
    val onError: (Int) -> Unit
) {

    private var recognizer: SpeechRecognizer? = null
    private var listening = false
    private var lastPartial = ""
    private var resultDelivered = false

    /** Transcript accumulated across recognizer auto-restarts within a single PTT hold. */
    private var accumulatedTranscript = ""

    private val handler = Handler(Looper.getMainLooper())

    /** Call from onKeyDown for KEYCODE_VOLUME_DOWN. */
    fun startListening() {
        if (listening) return
        listening = true
        lastPartial = ""
        resultDelivered = false
        accumulatedTranscript = ""

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
        putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
        // Suppress silence-based cutoff while key is held
        putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, 60000L)
        putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS, 60000L)
        putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_MINIMUM_LENGTH_MILLIS, 60000L)
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
        override fun onRmsChanged(rmsdB: Float) {}
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

            if (listening) {
                // Recognizer auto-completed while button still held.
                // Accumulate what we have and restart to keep capturing speech.
                accumulatedTranscript = combinedTranscript(transcript)
                lastPartial = ""
                recognizer?.startListening(buildRecognizerIntent())
            } else {
                // Button released — deliver the final combined result.
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

            if (listening) {
                // Recognizer errored while button still held.
                // Save any partial transcript and restart.
                if (lastPartial.isNotBlank()) {
                    accumulatedTranscript = combinedTranscript(lastPartial)
                    lastPartial = ""
                }
                recognizer?.startListening(buildRecognizerIntent())
            } else {
                // Button released — deliver whatever we have.
                deliverIfPending()
            }
        }

        override fun onEvent(eventType: Int, params: Bundle?) {}
    }

    companion object {
        /** Grace period for the safety-net delivery after stopListening(). */
        private const val STOP_SAFETY_DELAY_MS = 500L
    }
}
