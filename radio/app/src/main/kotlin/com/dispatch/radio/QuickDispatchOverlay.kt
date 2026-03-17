package com.dispatch.radio

import android.app.AlertDialog
import android.content.Context

/**
 * Shows the agent type picker overlay for quick dispatch (Volume Up long press).
 *
 * Presents a dialog listing available tool types. Tapping a selection
 * invokes [onDispatch] with the chosen tool name, which the caller
 * should turn into a `dispatch` WebSocket message.
 */
class QuickDispatchOverlay(private val context: Context) {

    private val tools = listOf(
        "claude-code" to "Claude Code",
        "copilot" to "GitHub Copilot"
    )

    fun show(onDispatch: (tool: String) -> Unit) {
        val labels = tools.map { it.second }.toTypedArray()

        AlertDialog.Builder(context, R.style.Theme_DispatchRadio_Dialog)
            .setTitle("QUICK DISPATCH")
            .setItems(labels) { dialog, which ->
                dialog.dismiss()
                onDispatch(tools[which].first)
            }
            .setNegativeButton("CANCEL") { dialog, _ -> dialog.dismiss() }
            .show()
    }
}
