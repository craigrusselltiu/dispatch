package com.dispatch.radio

import android.content.Intent
import android.os.Bundle
import android.view.KeyEvent
import android.view.View
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import com.dispatch.radio.model.Agent
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
 *             settings (dispatch-88k.7), haptics (dispatch-88k.8),
 *             voice command parser (dispatch-88k.3).
 */
class MainActivity : AppCompatActivity() {

    private lateinit var settings: RadioSettings
    private lateinit var haptics: HapticFeedback
    private lateinit var volumeUpHandler: VolumeUpHandler
    private lateinit var pttManager: PushToTalkManager
    private var continuousManager: ContinuousListenManager? = null
    private lateinit var wsClient: RadioWebSocketClient

    // App state synced from console
    private var agents: List<Agent> = emptyList()
    private var currentSlot: Int = -1
    private var queuedTasks: Int = 0

    // UI views
    private lateinit var tvConnDot: TextView
    private lateinit var tvConnStatus: TextView
    private lateinit var tvTarget: TextView
    private lateinit var tvTargetDetail: TextView
    private lateinit var flListening: View
    private lateinit var tvListeningLabel: TextView
    private lateinit var tvPartial: TextView
    private lateinit var audioLevelView: AudioLevelView
    private lateinit var tvLastDispatch: TextView
    private lateinit var tvLastTaskId: TextView
    private lateinit var llAgents: LinearLayout
    private lateinit var tvQueued: TextView

    private val gson = Gson()

    companion object {
        private const val SETTINGS_REQUEST = 1001
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        settings = RadioSettings(this)
        haptics = HapticFeedback(this)

        bindViews()
        applyScreenOnFlag()

        volumeUpHandler = VolumeUpHandler(
            context = this,
            haptics = haptics,
            onCycleTarget = { agent ->
                currentSlot = agent.slot
                refreshTarget()
                wsClient.send("""{"type":"set_target","slot":${agent.slot}}""")
            },
            onQuickDispatch = { tool ->
                wsClient.send("""{"type":"dispatch","tool":"$tool"}""")
            }
        )

        pttManager = PushToTalkManager(
            context = this,
            locale = settings.speechLocale,
            onListeningStart = {
                haptics.listeningStart()
                wsClient.send("""{"type":"radio_status","state":"listening"}""")
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
                wsClient.send("""{"type":"radio_status","state":"idle"}""")
                handleTranscript(transcript)
            },
            onEmptyTranscript = {
                flListening.visibility = View.INVISIBLE
                haptics.emptyTranscript()
                wsClient.send("""{"type":"radio_status","state":"idle"}""")
            },
            onError = {
                flListening.visibility = View.INVISIBLE
                wsClient.send("""{"type":"radio_status","state":"idle"}""")
            }
        )

        initContinuousManager()
        initWebSocket()

        findViewById<android.widget.Button>(R.id.btn_settings).setOnClickListener {
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
                wsClient.send("""{"type":"radio_status","state":"listening"}""")
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
        tvTarget = findViewById(R.id.tv_target)
        tvTargetDetail = findViewById(R.id.tv_target_detail)
        flListening = findViewById(R.id.fl_listening)
        tvListeningLabel = findViewById(R.id.tv_listening_label)
        tvPartial = findViewById(R.id.tv_partial)
        audioLevelView = findViewById(R.id.audio_level_view)
        tvLastDispatch = findViewById(R.id.tv_last_dispatch)
        tvLastTaskId = findViewById(R.id.tv_last_task_id)
        llAgents = findViewById(R.id.ll_agents)
        tvQueued = findViewById(R.id.tv_queued)
    }

    private fun applyScreenOnFlag() {
        if (settings.keepScreenOn) {
            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }

    private fun initWebSocket() {
        haptics.setEnabled(settings.hapticEnabled)
        wsClient = RadioWebSocketClient(
            host = settings.consoleHost,
            port = settings.consolePort,
            psk = settings.psk,
            listener = object : RadioWebSocketClient.Listener {
                override fun onConnected() = setConnected(true)
                override fun onDisconnected() = setConnected(false)
                override fun onMessage(text: String) = handleMessage(text)
            }
        )
        wsClient.connect()
    }

    override fun onResume() {
        super.onResume()
        VolumeKeyBridge.isActivityInForeground = true
    }

    override fun onPause() {
        super.onPause()
        VolumeKeyBridge.isActivityInForeground = false
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
            wsClient.send("""{"type":"radio_status","state":"idle"}""")
        } else {
            haptics.listeningStart()
            manager.start()
        }
    }

    private fun handleTranscript(transcript: String) {
        val command = CommandParser.parse(transcript, agents)
        val msg = when (command) {
            is Command.Dispatch -> {
                haptics.dispatchConfirm()
                showLastDispatch("DISPATCH ${command.tool.uppercase()}", null)
                """{"type":"dispatch","tool":"${command.tool}"}"""
            }
            is Command.Terminate -> {
                val agent = agents.firstOrNull { it.slot == command.slot }
                showLastDispatch("TERMINATE ${agent?.callsign?.uppercase() ?: command.slot}", null)
                """{"type":"terminate","slot":${command.slot}}"""
            }
            is Command.SetTarget -> {
                val agent = agents.firstOrNull { it.slot == command.slot }
                currentSlot = command.slot
                refreshTarget()
                showLastDispatch("TARGET -> ${agent?.callsign?.uppercase() ?: command.slot}", null)
                """{"type":"set_target","slot":${command.slot}}"""
            }
            is Command.SendTo -> {
                val agent = agents.firstOrNull { it.slot == command.slot }
                showLastDispatch("-> ${agent?.callsign?.uppercase() ?: command.slot}: \"${command.text}\"", null)
                """{"type":"send","text":${gson.toJson(command.text)},"slot":${command.slot}}"""
            }
            is Command.SendToTarget -> {
                val target = agents.firstOrNull { it.slot == currentSlot }
                showLastDispatch("-> ${target?.callsign?.uppercase() ?: "TARGET"}: \"${command.text}\"", null)
                """{"type":"send","text":${gson.toJson(command.text)}}"""
            }
        }
        wsClient.send(msg)
    }

    private fun handleMessage(text: String) {
        val json = runCatching { gson.fromJson(text, JsonObject::class.java) }.getOrNull() ?: return
        when (json.get("type")?.asString) {
            "agents" -> {
                val slots = json.getAsJsonArray("slots") ?: return
                agents = slots.mapNotNull { el ->
                    val obj = el.asJsonObject
                    val status = obj.get("status")?.asString ?: return@mapNotNull null
                    Agent(
                        slot = obj.get("slot")?.asInt ?: return@mapNotNull null,
                        callsign = obj.get("callsign")?.asString ?: return@mapNotNull null,
                        tool = obj.get("tool")?.asString ?: "",
                        status = status,
                        task = obj.get("task")?.let { if (it.isJsonNull) null else it.asString }
                    )
                }
                currentSlot = json.get("target")?.asInt ?: currentSlot
                queuedTasks = json.get("queued_tasks")?.asInt ?: 0
                refreshAgentList()
                refreshTarget()
                tvQueued.text = queuedTasks.toString()
            }
            "target_changed" -> {
                currentSlot = json.get("slot")?.asInt ?: currentSlot
                refreshTarget()
            }
            "ack" -> {
                val taskId = json.get("task")?.let { if (it.isJsonNull) null else it.asString }
                tvLastTaskId.text = if (taskId != null) "task $taskId" else ""
            }
            "dispatched" -> {
                haptics.dispatchConfirm()
            }
        }
    }

    private fun setConnected(connected: Boolean) {
        val color = if (connected) R.color.green else R.color.red
        val statusText = if (connected) "CONNECTED" else "DISCONNECTED"
        tvConnDot.setTextColor(getColor(color))
        tvConnStatus.setTextColor(getColor(color))
        tvConnStatus.text = statusText
    }

    private fun refreshTarget() {
        val agent = agents.firstOrNull { it.slot == currentSlot }
        if (agent != null) {
            tvTarget.text = "[${agent.slot}] ${agent.callsign.uppercase()}"
            tvTargetDetail.text = buildString {
                if (agent.tool.isNotEmpty()) append(agent.tool.uppercase())
                if (agent.task != null) append(" | ${agent.task}")
            }
        } else {
            tvTarget.text = "NONE"
            tvTargetDetail.text = ""
        }
    }

    private fun refreshAgentList() {
        llAgents.removeAllViews()
        val active = agents.filter { it.status != "empty" }
        for (agent in active) {
            val tv = TextView(this).apply {
                val initial = greekInitial(agent.callsign)
                text = initial
                textSize = 18f
                fontFamily = "monospace"
                setPadding(8, 8, 8, 8)
                setTextColor(
                    if (agent.slot == currentSlot) getColor(R.color.cyan)
                    else getColor(R.color.dim_grey)
                )
                if (agent.slot == currentSlot) {
                    text = "▸$initial"
                }
            }
            llAgents.addView(tv)
        }
    }

    private fun showLastDispatch(line: String, taskId: String?) {
        tvLastDispatch.text = line
        tvLastTaskId.text = if (taskId != null) "task $taskId" else ""
    }

    /** Maps NATO callsign to Greek letter initial for the agent row display. */
    private fun greekInitial(callsign: String): String = when (callsign.lowercase()) {
        "alpha" -> "α"; "bravo" -> "β"; "charlie" -> "χ"; "delta" -> "δ"
        "echo" -> "ε"; "foxtrot" -> "φ"; "golf" -> "γ"; "hotel" -> "η"
        "india" -> "ι"; "juliet" -> "J"; "kilo" -> "κ"; "lima" -> "λ"
        "mike" -> "μ"; "november" -> "ν"; "oscar" -> "ο"; "papa" -> "π"
        "quebec" -> "Q"; "romeo" -> "ρ"; "sierra" -> "σ"; "tango" -> "τ"
        "uniform" -> "υ"; "victor" -> "ν"; "whiskey" -> "ω"; "x-ray" -> "ξ"
        "yankee" -> "ψ"; "zulu" -> "ζ"
        else -> callsign.take(1).uppercase()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == SETTINGS_REQUEST && resultCode == RESULT_OK) {
            // Reconnect with new settings
            wsClient.disconnect()
            pttManager.destroy()
            continuousManager?.destroy()
            initWebSocket()
            pttManager = PushToTalkManager(
                context = this,
                locale = settings.speechLocale,
                onListeningStart = {
                    haptics.listeningStart()
                    wsClient.send("""{"type":"radio_status","state":"listening"}""")
                    tvListeningLabel.text = "LISTENING"
                    flListening.visibility = View.VISIBLE
                    tvPartial.text = ""
                },
                onPartialResult = { partial -> tvPartial.text = partial },
                onFinalResult = { transcript ->
                    flListening.visibility = View.INVISIBLE
                    haptics.sendConfirm()
                    wsClient.send("""{"type":"radio_status","state":"idle"}""")
                    handleTranscript(transcript)
                },
                onEmptyTranscript = {
                    flListening.visibility = View.INVISIBLE
                    haptics.emptyTranscript()
                    wsClient.send("""{"type":"radio_status","state":"idle"}""")
                },
                onError = {
                    flListening.visibility = View.INVISIBLE
                    wsClient.send("""{"type":"radio_status","state":"idle"}""")
                }
            )
            initContinuousManager()
        }
    }

    override fun onDestroy() {
        super.onDestroy()
        VolumeKeyBridge.onKeyEvent = null
        VolumeKeyBridge.isActivityInForeground = false
        pttManager.destroy()
        continuousManager?.destroy()
        wsClient.disconnect()
    }
}
