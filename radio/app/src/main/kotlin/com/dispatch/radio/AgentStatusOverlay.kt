package com.dispatch.radio

import android.app.AlertDialog
import android.content.Context
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView
import android.graphics.Typeface
import com.dispatch.radio.model.Agent
import com.dispatch.radio.model.callsignColor

/**
 * Shows a hold-to-view overlay listing all active agent statuses.
 *
 * Each row shows a colored status dot, the callsign in its unique color,
 * and the status text on the right. Agents are listed in slot order
 * (earliest dispatched first). The caller dismisses when the button is released.
 */
class AgentStatusOverlay(private val context: Context) {

    private var dialog: AlertDialog? = null

    fun show(agents: List<Agent>, orchestratorStatus: String?) {
        dismiss()

        val font = Typeface.MONOSPACE
        val active = agents.filter { it.status != "empty" }.sortedBy { it.slot }
        val dp = context.resources.displayMetrics.density

        // --- Title bar with rule ---
        val titleLayout = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            setPadding((24 * dp).toInt(), (20 * dp).toInt(), (24 * dp).toInt(), 0)
        }

        titleLayout.addView(TextView(context).apply {
            text = "AGENT STATUS"
            setTextColor(Color.WHITE)
            textSize = 16f
            typeface = font
            letterSpacing = 0.15f
        })

        // Thin horizontal rule under title
        titleLayout.addView(View(context).apply {
            setBackgroundColor(0xFF333333.toInt())
            layoutParams = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, (1 * dp).toInt()
            ).apply { topMargin = (12 * dp).toInt() }
        })

        // --- Content ---
        val layout = LinearLayout(context).apply {
            orientation = LinearLayout.VERTICAL
            setPadding((24 * dp).toInt(), (8 * dp).toInt(), (24 * dp).toInt(), (16 * dp).toInt())
        }

        // --- Orchestrator status (pinned at top) ---
        if (orchestratorStatus != null) {
            val orchColor = when (orchestratorStatus) {
                "idle" -> 0xFFFFAA00.toInt()       // Amber
                "thinking" -> 0xFFFF3333.toInt()    // Red
                "dead" -> 0xFFFF3333.toInt()        // Red
                "failed" -> 0xFFFF3333.toInt()      // Red
                "starting" -> 0xFF666666.toInt()    // Dim gray
                else -> 0xFF666666.toInt()
            }

            val orchRow = LinearLayout(context).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(0, (10 * dp).toInt(), 0, (10 * dp).toInt())
            }

            // Status indicator dot
            orchRow.addView(View(context).apply {
                val size = (8 * dp).toInt()
                layoutParams = LinearLayout.LayoutParams(size, size).apply {
                    marginEnd = (12 * dp).toInt()
                }
                background = GradientDrawable().apply {
                    shape = GradientDrawable.OVAL
                    setColor(orchColor)
                }
            })

            // Label
            orchRow.addView(TextView(context).apply {
                text = "ORCHESTRATOR"
                setTextColor(Color.WHITE)
                textSize = 13f
                typeface = font
                layoutParams = LinearLayout.LayoutParams(
                    0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f
                )
            })

            // Status value
            orchRow.addView(TextView(context).apply {
                text = orchestratorStatus.uppercase()
                setTextColor(orchColor)
                textSize = 13f
                typeface = font
                gravity = Gravity.END
            })

            layout.addView(orchRow)

            // Divider after orchestrator row
            layout.addView(View(context).apply {
                setBackgroundColor(0xFF333333.toInt())
                layoutParams = LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT, (1 * dp).toInt()
                ).apply {
                    topMargin = (2 * dp).toInt()
                    bottomMargin = (4 * dp).toInt()
                }
            })
        }

        if (active.isEmpty()) {
            layout.addView(TextView(context).apply {
                text = "No agents online"
                setTextColor(0xFF666666.toInt())
                textSize = 14f
                typeface = font
                setPadding(0, (16 * dp).toInt(), 0, (8 * dp).toInt())
            })
        } else {
            for ((i, agent) in active.withIndex()) {
                val statusColor = when (agent.status) {
                    "working" -> 0xFFFF3333.toInt()
                    "idle" -> 0xFFFFAA00.toInt()
                    else -> 0xFF666666.toInt()
                }

                val row = LinearLayout(context).apply {
                    orientation = LinearLayout.HORIZONTAL
                    gravity = Gravity.CENTER_VERTICAL
                    setPadding(0, (12 * dp).toInt(), 0, (12 * dp).toInt())
                }

                // Status indicator dot
                val dot = View(context).apply {
                    val size = (8 * dp).toInt()
                    layoutParams = LinearLayout.LayoutParams(size, size).apply {
                        marginEnd = (12 * dp).toInt()
                    }
                    background = GradientDrawable().apply {
                        shape = GradientDrawable.OVAL
                        setColor(statusColor)
                    }
                }
                row.addView(dot)

                // Callsign + repo in a vertical stack
                val nameCol = LinearLayout(context).apply {
                    orientation = LinearLayout.VERTICAL
                    layoutParams = LinearLayout.LayoutParams(
                        0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f
                    )
                }
                nameCol.addView(TextView(context).apply {
                    text = agent.callsign.uppercase()
                    setTextColor(callsignColor(agent.callsign))
                    textSize = 16f
                    typeface = font
                })
                if (!agent.repo.isNullOrEmpty()) {
                    nameCol.addView(TextView(context).apply {
                        text = agent.repo
                        setTextColor(0xFF666666.toInt())
                        textSize = 11f
                        typeface = font
                    })
                }
                row.addView(nameCol)

                // Status label
                row.addView(TextView(context).apply {
                    text = agent.status.uppercase()
                    setTextColor(statusColor)
                    textSize = 13f
                    typeface = font
                    gravity = Gravity.END
                })

                layout.addView(row)

                // Divider between rows (not after last)
                if (i < active.size - 1) {
                    layout.addView(View(context).apply {
                        setBackgroundColor(0xFF1A1A1A.toInt())
                        layoutParams = LinearLayout.LayoutParams(
                            LinearLayout.LayoutParams.MATCH_PARENT, (1 * dp).toInt()
                        )
                    })
                }
            }
        }

        // --- Summary line ---
        val working = active.count { it.status == "working" }
        val idle = active.count { it.status == "idle" }
        if (active.isNotEmpty()) {
            layout.addView(View(context).apply {
                setBackgroundColor(0xFF333333.toInt())
                layoutParams = LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT, (1 * dp).toInt()
                ).apply { topMargin = (4 * dp).toInt() }
            })

            layout.addView(TextView(context).apply {
                text = "${active.size} ONLINE  /  $working WORKING  /  $idle IDLE"
                setTextColor(0xFF666666.toInt())
                textSize = 11f
                typeface = font
                setPadding(0, (12 * dp).toInt(), 0, 0)
            })
        }

        dialog = AlertDialog.Builder(context, R.style.Theme_DispatchRadio_Dialog)
            .setCustomTitle(titleLayout)
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
