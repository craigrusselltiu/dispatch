// WebSocket protocol message types for the Dispatch console.
//
// Inbound: messages received from the radio (or any WebSocket client).
// Outbound: messages sent by the console to connected clients.

use serde::{Deserialize, Serialize};

/// NATO phonetic callsigns, 1-indexed (slot 1 → Alpha, ..., slot 26 → Zulu).
pub const NATO: [&str; 26] = [
    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel",
    "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar", "Papa",
    "Quebec", "Romeo", "Sierra", "Tango", "Uniform", "Victor", "Whiskey",
    "X-ray", "Yankee", "Zulu",
];

/// Default callsign for a 1-indexed slot number.
pub fn default_callsign(slot: u32) -> &'static str {
    NATO[(slot as usize).saturating_sub(1).min(25)]
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
