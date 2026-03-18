package com.dispatch.wear

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AgentSlotTest {

    @Test
    fun agentSlot_holdsValues() {
        val agent = AgentSlot(
            slot = 1,
            callsign = "Alpha",
            tool = "claude-code",
            status = "busy",
            task = "t1.1"
        )
        assertEquals(1, agent.slot)
        assertEquals("Alpha", agent.callsign)
        assertEquals("claude-code", agent.tool)
        assertEquals("busy", agent.status)
        assertEquals("t1.1", agent.task)
    }

    @Test
    fun agentSlot_nullTask() {
        val agent = AgentSlot(
            slot = 2,
            callsign = "Bravo",
            tool = "copilot",
            status = "idle",
            task = null
        )
        assertNull(agent.task)
    }

    @Test
    fun filterOccupied_excludesEmpty() {
        val agents = listOf(
            AgentSlot(1, "Alpha", "claude-code", "busy", null),
            AgentSlot(2, "Bravo", "", "empty", null),
            AgentSlot(3, "Charlie", "copilot", "idle", null)
        )
        val occupied = agents.filter { it.status != "empty" }
        assertEquals(2, occupied.size)
        assertEquals("Alpha", occupied[0].callsign)
        assertEquals("Charlie", occupied[1].callsign)
    }

    @Test
    fun cycleLogic_forwardWrap() {
        val occupied = listOf(
            AgentSlot(1, "Alpha", "claude-code", "busy", null),
            AgentSlot(3, "Charlie", "copilot", "idle", null),
            AgentSlot(5, "Echo", "claude-code", "busy", null)
        )
        // Current is Charlie (index 1), forward should give Echo (index 2)
        val currentIndex = occupied.indexOfFirst { it.slot == 3 }
        val nextIndex = (currentIndex + 1).mod(occupied.size)
        assertEquals(2, nextIndex)
        assertEquals("Echo", occupied[nextIndex].callsign)

        // Current is Echo (index 2), forward should wrap to Alpha (index 0)
        val wrapIndex = (2 + 1).mod(occupied.size)
        assertEquals(0, wrapIndex)
        assertEquals("Alpha", occupied[wrapIndex].callsign)
    }

    @Test
    fun cycleLogic_backwardWrap() {
        val occupied = listOf(
            AgentSlot(1, "Alpha", "claude-code", "busy", null),
            AgentSlot(3, "Charlie", "copilot", "idle", null),
            AgentSlot(5, "Echo", "claude-code", "busy", null)
        )
        // Current is Alpha (index 0), backward should wrap to Echo (index 2)
        val prevIndex = (0 + (-1)).mod(occupied.size)
        assertEquals(2, prevIndex)
        assertEquals("Echo", occupied[prevIndex].callsign)
    }

    @Test
    fun cycleLogic_notFound_defaultsToFirst() {
        val occupied = listOf(
            AgentSlot(1, "Alpha", "claude-code", "busy", null),
            AgentSlot(3, "Charlie", "copilot", "idle", null)
        )
        // Current slot not in occupied list
        val currentIndex = occupied.indexOfFirst { it.slot == 99 }
        assertEquals(-1, currentIndex)
        // When not found, default to index 0
        val nextIndex = if (currentIndex < 0) 0 else (currentIndex + 1).mod(occupied.size)
        assertEquals(0, nextIndex)
        assertEquals("Alpha", occupied[nextIndex].callsign)
    }
}
