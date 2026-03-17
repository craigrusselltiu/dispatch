package com.dispatch.radio

import com.dispatch.radio.model.Agent

/**
 * Manages target cycling on Volume Up (short press).
 *
 * Advances to the next occupied slot (non-empty status), wrapping around.
 * Returns the new target slot, or null if there are no occupied slots.
 */
class TargetCycler {

    /**
     * Advance to the next occupied slot after [currentSlot].
     * Occupied means status != "empty".
     *
     * Returns the newly selected [Agent], or null if no occupied agents exist.
     */
    fun cycle(agents: List<Agent>, currentSlot: Int): Agent? {
        val occupied = agents.filter { it.status != "empty" }
        if (occupied.isEmpty()) return null

        val currentIndex = occupied.indexOfFirst { it.slot == currentSlot }
        val nextIndex = (currentIndex + 1) % occupied.size
        return occupied[nextIndex]
    }
}
