package com.dispatch.radio

import android.Manifest
import android.animation.ObjectAnimator
import android.animation.ValueAnimator
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.view.KeyEvent
import android.view.View
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import com.dispatch.radio.model.Agent
import com.dispatch.radio.model.callsignColor
import com.dispatch.radio.ui.AudioLevelView
import com.google.gson.Gson
import com.google.gson.JsonObject

/**
 * Single-activity entry point for Dispatch Radio (dispatch-88k.1).
 *
 * Volume Down -> push-to-talk via SpeechRecognizer (dispatch-88k.2)
 * Volume Up   -> target cycling / quick dispatch (dispatch-88k.4)
 *
 * Integrates: WebSocket client (dispatch-88k.5), UI (dispatch-88k.6),
 *             settings (dispatch-88k.7), haptics (dispatch-88k.8).
 */
class MainActivity : AppCompatActivity() {

    private lateinit var settings: RadioSettings
    private lateinit var haptics: HapticFeedback
    private lateinit var volumeUpHandler: VolumeUpHandler
    private lateinit var pttManager: PushToTalkManager
    private var continuousManager: ContinuousListenManager? = null
    private var wsClient: RadioWebSocketClient? = null

    // App state synced from console
    private var agents: List<Agent> = emptyList()
    private var currentSlot: Int = -1
    private var queuedTasks: Int = 0

    // Chat log (dispatch-chat)
    private var chatMessageCount: Int = 0

    // Status dot blink animator (REC-light pulse)
    private var statusBlinkAnimator: ObjectAnimator? = null

    // UI views
    private lateinit var tvConnDot: TextView
    private lateinit var tvConnStatus: TextView
    private lateinit var flListening: View
    private lateinit var tvListeningLabel: TextView
    private lateinit var tvPartial: TextView
    private lateinit var audioLevelView: AudioLevelView
    private lateinit var svChat: ScrollView
    private lateinit var llChat: LinearLayout

    private val gson = Gson()

    companion object {
        private const val SETTINGS_REQUEST = 1001
        private const val MAX_CHAT_MESSAGES = 100
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        settings = RadioSettings(this)
        haptics = HapticFeedback(this)

        bindViews()
        applyScreenOnFlag()
        startStatusBlink()

        // Request microphone permission for speech recognition
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
            != PackageManager.PERMISSION_GRANTED) {
            ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.RECORD_AUDIO), 100)
        }

        volumeUpHandler = VolumeUpHandler(
            context = this,
            haptics = haptics,
            onCycleTarget = { agent ->
                currentSlot = agent.slot
                wsClient?.send("""{"type":"set_target","slot":${agent.slot}}""")
            },
            onQuickDispatch = { tool ->
                wsClient?.send("""{"type":"dispatch","tool":"$tool"}""")
            }
        )

        pttManager = PushToTalkManager(
            context = this,
            locale = settings.speechLocale,
            onListeningStart = {
                haptics.listeningStart()
                wsClient?.send("""{"type":"radio_status","state":"listening"}""")
                tvListeningLabel.text = "LISTENING"
                flListening.visibility = View.VISIBLE
                tvPartial.text = ""
                audioLevelView.level = 0f
            },
            onPartialResult = { partial ->
                tvPartial.text = partial
            },
            onFinalResult = { transcript ->
                flListening.visibility = View.INVISIBLE
                audioLevelView.level = 0f
                haptics.sendConfirm()
                wsClient?.send("""{"type":"radio_status","state":"idle"}""")
                handleTranscript(transcript)
            },
            onEmptyTranscript = {
                flListening.visibility = View.INVISIBLE
                haptics.emptyTranscript()
                wsClient?.send("""{"type":"radio_status","state":"idle"}""")
            },
            onError = {
                flListening.visibility = View.INVISIBLE
                wsClient?.send("""{"type":"radio_status","state":"idle"}""")
            }
        )

        initContinuousManager()
        initWebSocket()

        findViewById<android.widget.TextView>(R.id.btn_settings).setOnClickListener {
            @Suppress("DEPRECATION")
            startActivityForResult(Intent(this, SettingsActivity::class.java), SETTINGS_REQUEST)
        }

        // Register bridge so the AccessibilityService can forward volume keys
        // when the activity is backgrounded or the screen is off (dispatch-ct2.7)
        VolumeKeyBridge.onKeyEvent = { event ->
            when (event.action) {
                KeyEvent.ACTION_DOWN -> onKeyDown(event.keyCode, event)
                KeyEvent.ACTION_UP -> onKeyUp(event.keyCode, event)
                else -> false
            }
        }
    }

    private fun initContinuousManager() {
        continuousManager?.destroy()
        continuousManager = ContinuousListenManager(
            context = this,
            locale = settings.speechLocale,
            onListeningStart = {
                wsClient?.send("""{"type":"radio_status","state":"listening"}""")
                tvListeningLabel.text = "CONTINUOUS"
                flListening.visibility = View.VISIBLE
                tvPartial.text = ""
            },
            onPartialResult = { partial ->
                tvPartial.text = partial
            },
            onFinalResult = { transcript ->
                haptics.sendConfirm()
                tvPartial.text = ""
                audioLevelView.level = 0f
                handleTranscript(transcript)
                // Listening panel stays visible — recognizer auto-restarts
            },
            onEmptyTranscript = {
                // No speech in this cycle — recognizer auto-restarts, no action needed
            },
            onError = {
                // Recoverable — recognizer auto-restarts
            },
            onRmsChanged = { level ->
                audioLevelView.level = level
            }
        )
    }

    private fun bindViews() {
        tvConnDot = findViewById(R.id.tv_conn_dot)
        tvConnStatus = findViewById(R.id.tv_conn_status)
        flListening = findViewById(R.id.fl_listening)
        tvListeningLabel = findViewById(R.id.tv_listening_label)
        tvPartial = findViewById(R.id.tv_partial)
        audioLevelView = findViewById(R.id.audio_level_view)
        svChat = findViewById(R.id.sv_chat)
        llChat = findViewById(R.id.ll_chat)
    }

    private fun applyScreenOnFlag() {
        if (settings.keepScreenOn) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    private fun initWebSocket() {
        haptics.enabled = settings.hapticEnabled
        try {
            wsClient = RadioWebSocketClient(
                host = settings.consoleHost,
                port = settings.consolePort,
                psk = settings.psk,
                listener = object : RadioWebSocketClient.Listener {
                    override fun onConnected() = setConnected(true)
                    override fun onDisconnected() = setConnected(false)
                    override fun onMessage(text: String) = handleMessage(text)
                },
                certFingerprint = settings.certFingerprint,
            )
            wsClient?.connect()
        } catch (_: Exception) {
            wsClient = null
        }
    }

    override fun onResume() {
        super.onResume()
        VolumeKeyBridge.isActivityInForeground = true
    }

    override fun onPause() {
        super.onPause()
        VolumeKeyBridge.isActivityInForeground = false
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (event.keyCode == KeyEvent.KEYCODE_VOLUME_UP || event.keyCode == KeyEvent.KEYCODE_VOLUME_DOWN) {
            return when (event.action) {
                KeyEvent.ACTION_DOWN -> onKeyDown(event.keyCode, event)
                KeyEvent.ACTION_UP -> onKeyUp(event.keyCode, event)
                else -> true
            }
        }
        return super.dispatchKeyEvent(event)
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        return when (keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> volumeUpHandler.onKeyDown(agents, currentSlot)
            KeyEvent.KEYCODE_VOLUME_DOWN -> {
                if (event.repeatCount == 0) {
                    if (settings.continuousListening) {
                        toggleContinuousListening()
                    } else {
                        pttManager.startListening()
                    }
                }
                true
            }
            else -> super.onKeyDown(keyCode, event)
        }
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        return when (keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> volumeUpHandler.onKeyUp(agents, currentSlot)
            KeyEvent.KEYCODE_VOLUME_DOWN -> {
                if (!settings.continuousListening) {
                    pttManager.stopListening()
                }
                true
            }
            else -> super.onKeyUp(keyCode, event)
        }
    }

    private fun toggleContinuousListening() {
        val manager = continuousManager ?: return
        if (manager.isActive) {
            manager.stop()
            haptics.sendConfirm()
            flListening.visibility = View.INVISIBLE
            audioLevelView.level = 0f
            wsClient?.send("""{"type":"radio_status","state":"idle"}""")
        } else {
            haptics.listeningStart()
            manager.start()
        }
    }

    // dispatch-h62: send raw transcripts to the orchestrator LLM instead of
    // parsing commands locally. The orchestrator decides what to do.
    private fun handleTranscript(transcript: String) {
        val msg = """{"type":"send","text":${gson.toJson(transcript)},"auto":true}"""
        wsClient?.send(msg)
    }

    private fun handleMessage(text: String) {
        val json = runCatching { gson.fromJson(text, JsonObject::class.java) }.getOrNull() ?: return
        when (json.get("type")?.asString) {
            "agents" -> {
                val slots = json.getAsJsonArray("slots") ?: return
                agents = slots.mapNotNull { el ->
                    val obj = el.asJsonObject
                    val status = obj.get("status")?.takeUnless { it.isJsonNull }?.asString ?: return@mapNotNull null
                    Agent(
                        slot = obj.get("slot")?.takeUnless { it.isJsonNull }?.asInt ?: return@mapNotNull null,
                        callsign = obj.get("callsign")?.takeUnless { it.isJsonNull }?.asString ?: return@mapNotNull null,
                        tool = obj.get("tool")?.takeUnless { it.isJsonNull }?.asString ?: "",
                        status = status,
                        task = obj.get("task")?.takeUnless { it.isJsonNull }?.asString
                    )
                }
                currentSlot = json.get("target")?.takeUnless { it.isJsonNull }?.asInt ?: currentSlot
                queuedTasks = json.get("queued_tasks")?.takeUnless { it.isJsonNull }?.asInt ?: 0
                // UI state updated — agents/target/queued tracked internally for volume key handling
            }
            "target_changed" -> {
                currentSlot = json.get("slot")?.asInt ?: currentSlot
            }
            "ack" -> {
                // Acknowledged by orchestrator
            }
            "dispatched" -> {
                haptics.dispatchConfirm()
            }
            // dispatch-chat: handle chat messages pushed by the console
            "chat" -> {
                val sender = json.get("sender")?.asString ?: return
                val chatText = json.get("text")?.asString ?: return
                addChatMessage(sender, chatText)
            }
        }
    }

    // dispatch-chat: add a message to the scrollable chat log
    private fun addChatMessage(sender: String, text: String) {
        // Trim old messages if over the cap
        if (chatMessageCount >= MAX_CHAT_MESSAGES) {
            llChat.removeViewAt(0)
            chatMessageCount--
        }

        val displayName = if (sender == "Dispatcher") "Console" else sender

        val colorInt = when (sender) {
            "You" -> getColor(R.color.green)
            "Dispatcher" -> getColor(R.color.magenta)
            "System" -> getColor(R.color.dim_grey)
            else -> callsignColor(sender) // Distinct color per agent callsign
        }

        val tv = TextView(this).apply {
            this.text = "$displayName: $text"
            textSize = 11f
            typeface = android.graphics.Typeface.MONOSPACE
            setTextColor(colorInt)
            setPadding(0, 2, 0, 2)
        }
        llChat.addView(tv)
        chatMessageCount++
        svChat.post { svChat.fullScroll(View.FOCUS_DOWN) }
    }

    private fun setConnected(connected: Boolean) {
        val color = if (connected) R.color.green else R.color.red
        val statusText = if (connected) "CONNECTED" else "DISCONNECTED"
        tvConnDot.setTextColor(getColor(color))
        tvConnStatus.setTextColor(getColor(color))
        tvConnStatus.text = statusText
        startStatusBlink()
    }

    /** Pulse the status dot alpha like a REC indicator light (~1s cycle). */
    private fun startStatusBlink() {
        statusBlinkAnimator?.cancel()
        tvConnDot.alpha = 1f
        statusBlinkAnimator = ObjectAnimator.ofFloat(tvConnDot, "alpha", 1f, 0f, 1f).apply {
            duration = 1000L
            repeatCount = ValueAnimator.INFINITE
            start()
        }
    }

    @Suppress("DEPRECATION")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == SETTINGS_REQUEST && resultCode == RESULT_OK) {
            // Reload settings and reconnect
            settings = RadioSettings(this)
            try {
                wsClient?.disconnect()
                pttManager.destroy()
                continuousManager?.destroy()
                initWebSocket()
                pttManager = PushToTalkManager(
                    context = this,
                    locale = settings.speechLocale,
                    onListeningStart = {
                        haptics.listeningStart()
                        wsClient?.send("""{"type":"radio_status","state":"listening"}""")
                        tvListeningLabel.text = "LISTENING"
                        flListening.visibility = View.VISIBLE
                        tvPartial.text = ""
                    },
                    onPartialResult = { partial -> tvPartial.text = partial },
                    onFinalResult = { transcript ->
                        flListening.visibility = View.INVISIBLE
                        haptics.sendConfirm()
                        wsClient?.send("""{"type":"radio_status","state":"idle"}""")
                        handleTranscript(transcript)
                    },
                    onEmptyTranscript = {
                        flListening.visibility = View.INVISIBLE
                        haptics.emptyTranscript()
                        wsClient?.send("""{"type":"radio_status","state":"idle"}""")
                    },
                    onError = {
                        flListening.visibility = View.INVISIBLE
                        wsClient?.send("""{"type":"radio_status","state":"idle"}""")
                    }
                )
                initContinuousManager()
            } catch (_: Exception) {
                // Settings changed but reconnect failed — app stays running
            }
        }
    }

    override fun onDestroy() {
        super.onDestroy()
        statusBlinkAnimator?.cancel()
        VolumeKeyBridge.onKeyEvent = null
        VolumeKeyBridge.isActivityInForeground = false
        pttManager.destroy()
        continuousManager?.destroy()
        wsClient?.disconnect()
    }
}
