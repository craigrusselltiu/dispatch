package com.dispatch.wear

import android.app.AlertDialog
import android.content.Intent
import android.os.Bundle
import android.view.InputDevice
import android.view.MotionEvent
import android.widget.LinearLayout
import android.widget.TextView
import androidx.fragment.app.FragmentActivity
import org.json.JSONObject

/**
 * Wear OS companion for Dispatch. Shows agent status at a glance,
 * crown rotation cycles targets, tap triggers quick dispatch.
 */
class WearMainActivity : FragmentActivity() {

    private lateinit var settings: WearSettings
    private lateinit var wsClient: WearWebSocketClient

    private var agents: List<AgentSlot> = emptyList()
    private var currentSlot: Int = -1

    private lateinit var tvConnDot: TextView
    private lateinit var tvConnStatus: TextView
    private lateinit var tvTarget: TextView
    private lateinit var tvTargetDetail: TextView
    private lateinit var llAgents: LinearLayout
    private lateinit var tvDispatch: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_wear_main)

        settings = WearSettings(this)
        bindViews()
        setupInteraction()
        initWebSocket()
    }

    private fun bindViews() {
        tvConnDot = findViewById(R.id.tv_conn_dot)
        tvConnStatus = findViewById(R.id.tv_conn_status)
        tvTarget = findViewById(R.id.tv_target)
        tvTargetDetail = findViewById(R.id.tv_target_detail)
        llAgents = findViewById(R.id.ll_agents)
        tvDispatch = findViewById(R.id.tv_dispatch)
    }

    private fun setupInteraction() {
        // Tap dispatch area to trigger quick dispatch
        tvDispatch.setOnClickListener { showDispatchPicker() }

        // Long press anywhere opens settings
        findViewById<androidx.wear.widget.BoxInsetLayout>(R.id.root).setOnLongClickListener {
            startActivity(Intent(this, WearSettingsActivity::class.java))
            true
        }

        // Request focus for rotary input
        findViewById<androidx.wear.widget.BoxInsetLayout>(R.id.root).requestFocus()
    }

    private fun initWebSocket() {
        wsClient = WearWebSocketClient(
            host = settings.consoleHost,
            port = settings.consolePort,
            psk = settings.psk,
            listener = object : WearWebSocketClient.Listener {
                override fun onConnected() = setConnected(true)
                override fun onDisconnected() = setConnected(false)
                override fun onMessage(text: String) = handleMessage(text)
            }
        )
        wsClient.connect()
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        if (event.action == MotionEvent.ACTION_SCROLL &&
            event.isFromSource(InputDevice.SOURCE_ROTARY_ENCODER)
        ) {
            val delta = event.getAxisValue(MotionEvent.AXIS_SCROLL)
            cycleTarget(if (delta > 0f) 1 else -1)
            return true
        }
        return super.onGenericMotionEvent(event)
    }

    private fun cycleTarget(direction: Int) {
        val occupied = agents.filter { it.status != "empty" }
        if (occupied.isEmpty()) return

        val currentIndex = occupied.indexOfFirst { it.slot == currentSlot }
        val nextIndex = when {
            currentIndex < 0 -> 0
            else -> (currentIndex + direction).mod(occupied.size)
        }
        val next = occupied[nextIndex]
        currentSlot = next.slot
        refreshTarget()
        wsClient.send("""{"type":"set_target","slot":${next.slot}}""")
    }

    private fun showDispatchPicker() {
        val tools = arrayOf("Claude Code", "Copilot")
        val toolIds = arrayOf("claude-code", "copilot")

        AlertDialog.Builder(this)
            .setTitle("DISPATCH")
            .setItems(tools) { dialog, which ->
                dialog.dismiss()
                wsClient.send("""{"type":"dispatch","tool":"${toolIds[which]}"}""")
            }
            .setNegativeButton("CANCEL") { dialog, _ -> dialog.dismiss() }
            .show()
    }

    private fun handleMessage(text: String) {
        val json = runCatching { JSONObject(text) }.getOrNull() ?: return
        when (json.optString("type")) {
            "agents" -> {
                val slots = json.optJSONArray("slots") ?: return
                agents = (0 until slots.length()).mapNotNull { i ->
                    val obj = slots.optJSONObject(i) ?: return@mapNotNull null
                    AgentSlot(
                        slot = obj.optInt("slot", -1),
                        callsign = obj.optString("callsign", ""),
                        tool = obj.optString("tool", ""),
                        status = obj.optString("status", "empty"),
                        task = if (obj.isNull("task")) null else obj.optString("task")
                    )
                }
                currentSlot = json.optInt("target", currentSlot)
                refreshTarget()
                refreshAgentList()
            }
            "target_changed" -> {
                currentSlot = json.optInt("slot", currentSlot)
                refreshTarget()
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
            tvTarget.text = agent.callsign.uppercase()
            tvTargetDetail.text = buildString {
                if (agent.tool.isNotEmpty()) append(agent.tool.uppercase())
                append(" | ${agent.status}")
            }
        } else {
            tvTarget.text = getString(R.string.no_target)
            tvTargetDetail.text = ""
        }
    }

    private fun refreshAgentList() {
        llAgents.removeAllViews()
        val active = agents.filter { it.status != "empty" }
        for (agent in active) {
            val tv = TextView(this).apply {
                text = agent.callsign.take(1).uppercase()
                textSize = 14f
                setTypeface(android.graphics.Typeface.MONOSPACE)
                setPadding(6, 0, 6, 0)
                setTextColor(
                    if (agent.slot == currentSlot) getColor(R.color.cyan)
                    else getColor(R.color.dim_grey)
                )
            }
            llAgents.addView(tv)
        }
    }

    override fun onResume() {
        super.onResume()
        // Reconnect with potentially updated settings
        wsClient.disconnect()
        initWebSocket()
    }

    override fun onDestroy() {
        super.onDestroy()
        wsClient.disconnect()
    }
}

data class AgentSlot(
    val slot: Int,
    val callsign: String,
    val tool: String,
    val status: String,
    val task: String?
)
