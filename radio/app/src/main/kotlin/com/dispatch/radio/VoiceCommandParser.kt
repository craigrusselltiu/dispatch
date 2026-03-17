package com.dispatch.radio

import com.dispatch.radio.model.Agent

/** Structured result of parsing a voice utterance. */
sealed class Command {
    data class Dispatch(val tool: String) : Command()
    data class Terminate(val callsign: String) : Command()
    data class SetTarget(val callsign: String) : Command()
    data class SendTo(val callsign: String, val text: String) : Command()
    data class SendToTarget(val text: String) : Command()
}

/**
 * Parses a voice utterance into a [Command].
 *
 * Parse order (from SPEC §Voice Commands §Parser Design):
 * 1. Normalize: lowercase + trim
 * 2. Command prefixes: dispatch, terminate, set_target
 * 3. Callsign addressing at utterance start (optional comma)
 * 4. Default: send to current target
 */
class VoiceCommandParser(private val agents: List<Agent>) {

    fun parse(utterance: String): Command {
        val normalized = utterance.trim().lowercase()

        // 2a. Dispatch: (dispatch|new|spin up) + tool
        DISPATCH_PREFIXES.forEach { prefix ->
            if (normalized.startsWith(prefix)) {
                val rest = normalized.removePrefix(prefix).trim()
                val tool = matchTool(rest)
                if (tool != null) return Command.Dispatch(tool)
            }
        }

        // 2b. Terminate: (terminate|kill|shut down) + callsign
        TERMINATE_PREFIXES.forEach { prefix ->
            if (normalized.startsWith(prefix)) {
                val rest = normalized.removePrefix(prefix).trim()
                val callsign = matchCallsign(rest)
                if (callsign != null) return Command.Terminate(callsign)
            }
        }

        // 2c. Set target: (switch to|target) + callsign
        SET_TARGET_PREFIXES.forEach { prefix ->
            if (normalized.startsWith(prefix)) {
                val rest = normalized.removePrefix(prefix).trim()
                val callsign = matchCallsign(rest)
                if (callsign != null) return Command.SetTarget(callsign)
            }
        }

        // 3. Callsign addressing at utterance start (optional comma)
        val callsignPrefix = matchLeadingCallsign(normalized)
        if (callsignPrefix != null) {
            val (callsign, remainder) = callsignPrefix
            return Command.SendTo(callsign, remainder.trim())
        }

        // 4. Default: send to current target
        return Command.SendToTarget(utterance.trim())
    }

    /** Returns the canonical tool name if [text] matches a known alias, else null. */
    private fun matchTool(text: String): String? {
        for ((canonical, aliases) in TOOL_ALIASES) {
            if (aliases.any { text == it || text.startsWith("$it ") }) return canonical
        }
        return null
    }

    /** Returns the original-case callsign if [text] starts with a known callsign, else null. */
    private fun matchCallsign(text: String): String? {
        val occupied = agents.filter { it.status != "empty" }
        return occupied.firstOrNull { agent ->
            val cs = agent.callsign.lowercase()
            text == cs || text.startsWith("$cs ") || text.startsWith("$cs,")
        }?.callsign
    }

    /**
     * Checks whether [normalized] begins with a callsign (optionally followed by a comma).
     * Returns (callsign, remaining text) or null.
     */
    private fun matchLeadingCallsign(normalized: String): Pair<String, String>? {
        val occupied = agents.filter { it.status != "empty" }
        for (agent in occupied) {
            val cs = agent.callsign.lowercase()
            when {
                normalized.startsWith("$cs, ") ->
                    return agent.callsign to normalized.removePrefix("$cs, ")
                normalized.startsWith("$cs,") ->
                    return agent.callsign to normalized.removePrefix("$cs,")
                normalized.startsWith("$cs ") ->
                    return agent.callsign to normalized.removePrefix("$cs ")
            }
        }
        return null
    }

    companion object {
        private val DISPATCH_PREFIXES = listOf("dispatch ", "new ", "spin up ")
        private val TERMINATE_PREFIXES = listOf("terminate ", "kill ", "shut down ")
        private val SET_TARGET_PREFIXES = listOf("switch to ", "target ")

        /** Canonical tool name -> list of recognized spoken aliases (all lowercase). */
        val TOOL_ALIASES: Map<String, List<String>> = mapOf(
            "claude-code" to listOf("claude code", "cloud code", "claud code"),
            "copilot" to listOf("copilot", "co-pilot", "co pilot", "github copilot"),
        )
    }
}
