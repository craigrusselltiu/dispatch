package com.dispatch.radio

import com.dispatch.radio.model.Agent
import org.junit.Assert.assertEquals
import org.junit.Test

class VoiceCommandParserTest {

    private val agents = listOf(
        Agent(slot = 1, callsign = "Alpha",   tool = "claude-code", status = "busy", task = "bd-001"),
        Agent(slot = 2, callsign = "Bravo",   tool = "claude-code", status = "idle", task = null),
        Agent(slot = 3, callsign = "Charlie", tool = "copilot",     status = "idle", task = null),
        Agent(slot = 4, callsign = "Delta",   tool = "claude-code", status = "empty", task = null),
    )

    private val parser = VoiceCommandParser(agents)

    // --- Dispatch ---

    @Test
    fun `dispatch claude code`() =
        assertEquals(Command.Dispatch("claude-code"), parser.parse("dispatch claude code"))

    @Test
    fun `new copilot`() =
        assertEquals(Command.Dispatch("copilot"), parser.parse("new copilot"))

    @Test
    fun `spin up claude code`() =
        assertEquals(Command.Dispatch("claude-code"), parser.parse("spin up claude code"))

    @Test
    fun `dispatch cloud code alias`() =
        assertEquals(Command.Dispatch("claude-code"), parser.parse("dispatch cloud code"))

    @Test
    fun `dispatch claud code alias`() =
        assertEquals(Command.Dispatch("claude-code"), parser.parse("dispatch claud code"))

    @Test
    fun `dispatch github copilot alias`() =
        assertEquals(Command.Dispatch("copilot"), parser.parse("new github copilot"))

    @Test
    fun `dispatch co pilot alias`() =
        assertEquals(Command.Dispatch("copilot"), parser.parse("dispatch co pilot"))

    // --- Terminate ---

    @Test
    fun `terminate alpha`() =
        assertEquals(Command.Terminate("Alpha"), parser.parse("terminate alpha"))

    @Test
    fun `kill bravo`() =
        assertEquals(Command.Terminate("Bravo"), parser.parse("kill bravo"))

    @Test
    fun `shut down charlie`() =
        assertEquals(Command.Terminate("Charlie"), parser.parse("shut down charlie"))

    @Test
    fun `terminate is case-insensitive`() =
        assertEquals(Command.Terminate("Alpha"), parser.parse("Terminate ALPHA"))

    // --- Set Target ---

    @Test
    fun `switch to bravo`() =
        assertEquals(Command.SetTarget("Bravo"), parser.parse("switch to bravo"))

    @Test
    fun `target charlie`() =
        assertEquals(Command.SetTarget("Charlie"), parser.parse("target charlie"))

    @Test
    fun `set target is case-insensitive`() =
        assertEquals(Command.SetTarget("Alpha"), parser.parse("Switch To Alpha"))

    // --- SendTo (callsign addressing) ---

    @Test
    fun `callsign at start routes to that agent`() =
        assertEquals(Command.SendTo("Alpha", "refactor the auth module"), parser.parse("alpha refactor the auth module"))

    @Test
    fun `callsign with comma routes to that agent`() =
        assertEquals(Command.SendTo("Bravo", "fix the login bug"), parser.parse("bravo, fix the login bug"))

    @Test
    fun `callsign with comma no space routes to that agent`() =
        assertEquals(Command.SendTo("Charlie", "add unit tests"), parser.parse("charlie,add unit tests"))

    @Test
    fun `callsign is case-insensitive`() =
        assertEquals(Command.SendTo("Alpha", "write a test"), parser.parse("ALPHA write a test"))

    // --- SendToTarget (fallthrough) ---

    @Test
    fun `unrecognized utterance sends to current target`() =
        assertEquals(Command.SendToTarget("refactor the payment module"), parser.parse("refactor the payment module"))

    @Test
    fun `empty agents list always sends to target`() {
        val emptyParser = VoiceCommandParser(emptyList())
        assertEquals(Command.SendToTarget("alpha do something"), emptyParser.parse("alpha do something"))
    }

    @Test
    fun `empty agent slots are not matched as callsigns`() {
        // Delta is "empty", should not be matched
        assertEquals(Command.SendToTarget("delta do something"), parser.parse("delta do something"))
    }

    @Test
    fun `utterance preserves original casing for SendToTarget`() =
        assertEquals(Command.SendToTarget("Write Tests for Auth"), parser.parse("Write Tests for Auth"))

    @Test
    fun `utterance preserves original text for SendTo`() =
        assertEquals(Command.SendTo("Alpha", "Write Tests for Auth"), parser.parse("alpha Write Tests for Auth"))
}
