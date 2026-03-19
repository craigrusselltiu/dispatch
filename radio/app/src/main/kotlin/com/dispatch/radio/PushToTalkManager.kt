package com.dispatch.radio

import android.content.Context
import android.content.Intent
import android.os.Bundle
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

    /** Call from onKeyDown for KEYCODE_VOLUME_DOWN. */
    fun startListening() {
        if (listening) return
        listening = true
        lastPartial = ""
        resultDelivered = false

        if (recognizer == null) {
            recognizer = SpeechRecognizer.createSpeechRecognizer(context)
            recognizer?.setRecognitionListener(recognitionListener)
        }

        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
            putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            putExtra(RecognizerIntent.EXTRA_LANGUAGE, locale)
            putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            // No speech timeout while key is held
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS, Long.MAX_VALUE)
            putExtra(RecognizerIntent.EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS, Long.MAX_VALUE)
        }

        recognizer?.startListening(intent)
        onListeningStart()
    }

    /** Call from onKeyUp for KEYCODE_VOLUME_DOWN. */
    fun stopListening() {
        if (!listening) return
        listening = false
        recognizer?.stopListening()
        // onFinalResult will be called via the listener callback
    }

    fun destroy() {
        recognizer?.destroy()
        recognizer = null
        listening = false
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
            onPartialResult(partial)
        }

        override fun onResults(results: Bundle?) {
            if (resultDelivered) return
            resultDelivered = true

            val transcript = results
                ?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)
                ?.firstOrNull()
                ?: lastPartial

            if (transcript.isNullOrBlank()) {
                onEmptyTranscript()
            } else {
                onFinalResult(transcript)
            }
        }

        override fun onError(error: Int) {
            if (resultDelivered) return

            if (!listening) {
                resultDelivered = true
                if (lastPartial.isNotBlank()) {
                    onFinalResult(lastPartial)
                } else {
                    onEmptyTranscript()
                }
            } else {
                this@PushToTalkManager.onError(error)
            }
            listening = false
        }

        override fun onEvent(eventType: Int, params: Bundle?) {}
    }
}
