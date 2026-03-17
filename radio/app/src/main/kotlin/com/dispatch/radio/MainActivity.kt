package com.dispatch.radio

import android.os.Bundle
import android.view.KeyEvent
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import com.dispatch.radio.model.Agent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel

/**
 * Single-activity entry point for Dispatch Radio.
 *
 * Volume key handling:
 *   Volume Down -> push-to-talk (dispatch-88k.2)
 *   Volume Up   -> target cycling / quick dispatch (dispatch-88k.4)
 *
 * WebSocket client injected via [wsClient] (dispatch-88k.5).
 * UI updates driven by state observers (dispatch-88k.6).
 */
class MainActivity : AppCompatActivity() {

    private val scope = CoroutineScope(Dispatchers.Main + SupervisorJob())

    // Shared app state — populated by WebSocket sync (dispatch-88k.5)
    private var agents: List<Agent> = emptyList()
    private var currentSlot: Int = -1

    private lateinit var haptics: HapticFeedback
    private lateinit var volumeUpHandler: VolumeUpHandler

    // Placeholder view — full UI implemented in dispatch-88k.6
    private lateinit var tvTarget: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        tvTarget = findViewById(R.id.tv_target)

        haptics = HapticFeedback(this)

        volumeUpHandler = VolumeUpHandler(
            context = this,
            haptics = haptics,
            onCycleTarget = { agent ->
                currentSlot = agent.slot
                displayTarget(agent)
                sendSetTarget(agent.slot)
            },
            onQuickDispatch = { tool ->
                sendDispatch(tool)
            }
        )
    }

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        return when (keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> {
                volumeUpHandler.onKeyDown(agents, currentSlot)
            }
            KeyEvent.KEYCODE_VOLUME_DOWN -> {
                // Push-to-talk — implemented in dispatch-88k.2
                true
            }
            else -> super.onKeyDown(keyCode, event)
        }
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        return when (keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP -> {
                volumeUpHandler.onKeyUp(agents, currentSlot)
            }
            KeyEvent.KEYCODE_VOLUME_DOWN -> {
                // Push-to-talk — implemented in dispatch-88k.2
                true
            }
            else -> super.onKeyUp(keyCode, event)
        }
    }

    /** Called by WebSocket client when agent list is updated (dispatch-88k.5). */
    fun onAgentListUpdated(updatedAgents: List<Agent>, targetSlot: Int) {
        agents = updatedAgents
        currentSlot = targetSlot
        val target = agents.firstOrNull { it.slot == currentSlot }
        if (target != null) displayTarget(target)
    }

    private fun displayTarget(agent: Agent) {
        tvTarget.text = "[${agent.slot}] ${agent.callsign.uppercase()}"
    }

    private fun sendSetTarget(slot: Int) {
        // Delegate to WebSocket client (dispatch-88k.5)
        // wsClient.sendSetTarget(slot)
    }

    private fun sendDispatch(tool: String) {
        // Delegate to WebSocket client (dispatch-88k.5)
        // wsClient.sendDispatch(tool)
    }

    override fun onDestroy() {
        super.onDestroy()
        scope.cancel()
    }
}
