package com.dispatch.radio

import android.app.AlertDialog
import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.view.Gravity
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import com.dispatch.radio.model.Agent

/**
 * Shows a hold-to-view overlay listing all active agent statuses.
 *
 * Each line displays the callsign on the left and its status on the right,
 * colored RED for busy and YELLOW for idle. Agents are listed in slot order
 * (earliest dispatched first). The caller dismisses when the button is released.
 */
class AgentStatusOverlay(private val context: Context) {

    companion object {
        private val MONO_BOLD = Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)
    }

    private var dialog: AlertDialog? = null

    fun show(agents: List<Agent>) {
        dismiss()

        val active = agents.filter { it.status != "empty" }.sortedBy { it.slot }

        val layout = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 24, 48, 24)
        }

        if (active.isEmpty()) {
            layout.addView(TextView(context).apply {
                text = "No agents online"
                setTextColor(Color.GRAY)
                textSize = 16f
                typeface = Typeface.MONOSPACE
            })
        } else {
            for (agent in active) {
                val row = LinearLayout(context).apply {
                    orientation = LinearLayout.HORIZONTAL
                    setPadding(0, 12, 0, 12)
                }

                row.addView(TextView(context).apply {
                    text = agent.callsign
                    setTextColor(Color.WHITE)
                    textSize = 18f
                    typeface = MONO_BOLD
                    layoutParams = LinearLayout.LayoutParams(
                        0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f
                    )
                })

                val statusColor = when (agent.status) {
                    "busy" -> Color.RED
                    "idle" -> Color.YELLOW
                    else -> Color.GRAY
                }

                row.addView(TextView(context).apply {
                    text = agent.status.replaceFirstChar { it.uppercase() }
                    setTextColor(statusColor)
                    textSize = 18f
                    typeface = MONO_BOLD
                    gravity = Gravity.END
                })

                layout.addView(row)
            }
        }

        dialog = AlertDialog.Builder(context, R.style.Theme_DispatchRadio_Dialog)
            .setTitle("AGENT STATUS")
            .setView(layout)
            .setCancelable(false)
            .create()

        // Prevent the dialog from stealing input focus so volume key events
        // continue reaching Activity.dispatchKeyEvent. Without this flag the
        // dialog window captures key events, the activity never sees KEY_UP,
        // and the overlay flickers as focus bounces between windows.
        dialog?.window?.setFlags(
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE
        )

        dialog?.show()
    }

    fun dismiss() {
        dialog?.dismiss()
        dialog = null
    }
}
