// WebSocket protocol message types for the Dispatch console.
//
// Inbound: messages received from the radio (or any WebSocket client).
// Outbound: messages sent by the console to connected clients.

use serde::{Deserialize, Serialize};

/// NATO phonetic alphabet, used as the default agent callsign list.
pub const NATO_DEFAULTS: [&str; 26] = [
    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
    "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar", "Papa",
    "Quebec", "Romeo", "Sierra", "Tango", "Uniform", "Victor", "Whiskey",
    "X-ray", "Yankee", "Zulu",
];

/// Callsign for a 1-indexed slot number, given the configured callsign list.
pub fn callsign_for_slot(slot: u32, callsigns: &[String]) -> &str {
    callsigns.get((slot as usize).saturating_sub(1))
        .map(|s| s.as_str())
        .unwrap_or("Agent")
}

/// Resolve a callsign to its 0-indexed slot. Case-insensitive.
pub fn callsign_to_slot(callsign: &str, callsigns: &[String]) -> Option<usize> {
    let upper = callsign.to_uppercase();
    callsigns.iter().position(|n| n.to_uppercase() == upper)
}

// --- Inbound messages (radio → console) ---

/// Flat deserialization of all inbound message types.
/// The `type` field is used to discriminate; unused fields are ignored.
/// Unknown `type` values are silently ignored by the handler.
#[derive(Debug, Deserialize)]
pub struct RawInbound {
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Optional correlation sequence number.
    pub seq: Option<u64>,
    /// Used by: set_target, send, dispatch, terminate, rename.
    pub slot: Option<u32>,
    /// Used by: send.
    pub text: Option<String>,
    /// Used by: send — auto-dispatch to idle/new agent.
    pub auto: Option<bool>,
    /// Used by: dispatch — tool to launch (e.g. "claude-code").
    pub tool: Option<String>,
    /// Used by: rename — new callsign for the agent.
    pub callsign: Option<String>,
    /// Used by: radio_status — "listening" | "idle".
    pub state: Option<String>,
}

// --- Outbound messages (console → radio) ---

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundMsg {
    /// Response to list_agents.
    Agents {
        slots: Vec<SlotInfo>,
        target: Option<u32>,
        queued_tasks: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Response to set_target.
    TargetChanged {
        slot: u32,
        callsign: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Response to send.
    Ack {
        slot: u32,
        callsign: String,
        task: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        auto_dispatched: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Response to dispatch.
    Dispatched {
        slot: u32,
        callsign: String,
        tool: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Response to terminate.
    Terminated {
        slot: u32,
        callsign: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Response to rename.
    Renamed {
        slot: u32,
        callsign: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// Unsolicited chat message pushed to all connected clients.
    Chat {
        /// Sender label (e.g. "Dispatcher", "Alpha", "System").
        sender: String,
        /// Chat message text.
        text: String,
    },
    /// Sent on protocol errors (unknown slot, no target, all busy, etc.).
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
}

/// Agent slot state as reported to the radio.
#[derive(Debug, Serialize, Clone)]
pub struct SlotInfo {
    pub slot: u32,
    pub callsign: Option<String>,
    pub tool: Option<String>,
    /// "busy" | "idle" | "empty"
    pub status: &'static str,
    pub task: Option<String>,
    /// Short repo directory name (dispatch-2dc).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}
