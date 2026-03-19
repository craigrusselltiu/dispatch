// Message handler and console state for the Dispatch console.
//
// Contains the pure-logic message routing extracted from ws_server.rs.
// No networking, TLS, or async dependencies -- just state + JSON.

use std::sync::{mpsc, Arc, Mutex};

use crate::protocol::{default_callsign, OutboundMsg, RawInbound, SlotInfo};

pub const MAX_SLOTS: usize = 26;

// --- Event channel -------------------------------------------------------

/// Events sent from the WebSocket handler to the main TUI thread so that
/// PTY operations (which must happen on the main thread) can be executed.
pub enum WsEvent {
    /// Voice transcript from the radio — forwarded to the orchestrator (dispatch-h62).
    VoiceTranscript { text: String },
}

// --- Agent state ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum AgentStatus {
    Idle,
    Busy,
}

#[derive(Debug, Clone)]
pub struct AgentSlot {
    pub callsign: String,
    pub tool: String,
    pub status: AgentStatus,
    /// Current beads task ID, if busy.
    pub task: Option<String>,
    /// Short repo directory name (dispatch-2dc).
    pub repo: Option<String>,
}

pub struct ConsoleState {
    pub slots: [Option<AgentSlot>; MAX_SLOTS],
    /// Currently targeted slot (1-indexed). None if no agents are running.
    pub target: Option<u32>,
    /// Queued beads task IDs (tasks waiting for an available agent).
    pub queued_tasks: Vec<String>,
    /// Monotonic counter used to generate stub task IDs.
    task_counter: u64,
    /// Sender for PTY events; set by the main thread after startup.
    pub event_tx: Option<mpsc::Sender<WsEvent>>,
}

impl ConsoleState {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            target: None,
            queued_tasks: Vec::new(),
            task_counter: 0,
            event_tx: None,
        }
    }

    pub fn next_task_id(&mut self) -> String {
        self.task_counter += 1;
        format!("task-{}", self.task_counter)
    }

    pub fn slot_info(&self, slot: u32) -> SlotInfo {
        let idx = (slot as usize).saturating_sub(1);
        match self.slots.get(idx).and_then(|s| s.as_ref()) {
            None => SlotInfo {
                slot,
                callsign: None,
                tool: None,
                status: "empty",
                task: None,
                repo: None,
            },
            Some(a) => SlotInfo {
                slot,
                callsign: Some(a.callsign.clone()),
                tool: Some(a.tool.clone()),
                status: if a.status == AgentStatus::Busy { "busy" } else { "idle" },
                task: a.task.clone(),
                repo: a.repo.clone(),
            },
        }
    }

    pub fn all_slot_infos(&self) -> Vec<SlotInfo> {
        (1..=MAX_SLOTS as u32).map(|s| self.slot_info(s)).collect()
    }

    pub fn first_empty_slot(&self) -> Option<u32> {
        self.slots.iter().enumerate().find_map(|(i, s)| {
            if s.is_none() {
                Some(i as u32 + 1)
            } else {
                None
            }
        })
    }
}

pub type SharedState = Arc<Mutex<ConsoleState>>;

// --- Message dispatch ----------------------------------------------------

pub fn handle_message(raw: RawInbound, state: &SharedState) -> Option<OutboundMsg> {
    let seq = raw.seq;

    match raw.msg_type.as_str() {
        "list_agents" => {
            let st = state.lock().unwrap();
            Some(OutboundMsg::Agents {
                slots: st.all_slot_infos(),
                target: st.target,
                queued_tasks: st.queued_tasks.len() as u32,
                seq,
            })
        }

        "set_target" => {
            let slot = raw.slot?;
            let mut st = state.lock().unwrap();
            let idx = (slot as usize).saturating_sub(1);
            if idx >= MAX_SLOTS {
                return Some(OutboundMsg::Error {
                    message: format!("invalid slot {slot}"),
                    seq,
                });
            }
            let callsign = match &st.slots[idx] {
                Some(a) => a.callsign.clone(),
                None => {
                    return Some(OutboundMsg::Error {
                        message: format!("slot {slot} is empty"),
                        seq,
                    })
                }
            };
            st.target = Some(slot);
            Some(OutboundMsg::TargetChanged { slot, callsign, seq })
        }

        "send" => {
            let text = raw.text.as_deref().unwrap_or("").to_string();
            let auto = raw.auto.unwrap_or(false);
            let mut st = state.lock().unwrap();

            // dispatch-h62: when auto is true, forward the raw transcript to the
            // orchestrator LLM which decides what to do via tool calls. The old
            // deterministic routing (word count, idle slots, etc.) is replaced.
            if auto {
                if let Some(tx) = &st.event_tx {
                    let _ = tx.send(WsEvent::VoiceTranscript { text });
                }
                return Some(OutboundMsg::Ack {
                    slot: 0,
                    callsign: "Orchestrator".to_string(),
                    task: "routing".to_string(),
                    auto_dispatched: Some(true),
                    seq,
                });
            }

            // Explicit slot or current target — direct send (non-auto path unchanged).
            let slot = if let Some(s) = raw.slot {
                Some(s)
            } else {
                st.target
            };

            let slot = match slot {
                Some(s) => s,
                None => {
                    return Some(OutboundMsg::Error {
                        message: "no target agent; use set_target first or send with auto:true"
                            .to_string(),
                        seq,
                    })
                }
            };

            let idx = (slot as usize).saturating_sub(1);
            if idx >= MAX_SLOTS {
                return Some(OutboundMsg::Error {
                    message: format!("invalid slot {slot}"),
                    seq,
                });
            }

            if st.slots[idx].is_none() {
                return Some(OutboundMsg::Error {
                    message: format!("slot {slot} is empty"),
                    seq,
                });
            }

            // Stub task ID — real beads task created by the main thread via bd.
            let task_id = st.next_task_id();
            let agent = st.slots[idx].as_mut().unwrap();
            agent.status = AgentStatus::Busy;
            agent.task = Some(task_id.clone());
            let callsign = agent.callsign.clone();

            Some(OutboundMsg::Ack {
                slot,
                callsign,
                task: task_id,
                auto_dispatched: None,
                seq,
            })
        }

        "dispatch" => {
            let tool = raw.tool.as_deref().unwrap_or("claude-code").to_string();
            let mut st = state.lock().unwrap();

            let slot = if let Some(s) = raw.slot {
                s
            } else {
                match st.first_empty_slot() {
                    Some(s) => s,
                    None => {
                        return Some(OutboundMsg::Error {
                            message: "no empty slots available".to_string(),
                            seq,
                        })
                    }
                }
            };

            let idx = (slot as usize).saturating_sub(1);
            if idx >= MAX_SLOTS {
                return Some(OutboundMsg::Error {
                    message: format!("invalid slot {slot}"),
                    seq,
                });
            }

            let callsign = default_callsign(slot).to_string();
            st.slots[idx] = Some(AgentSlot {
                callsign: callsign.clone(),
                tool: tool.clone(),
                status: AgentStatus::Idle,
                task: None,
                repo: None,
            });

            Some(OutboundMsg::Dispatched { slot, callsign, tool, seq })
        }

        "terminate" => {
            let slot = raw.slot?;
            let mut st = state.lock().unwrap();
            let idx = (slot as usize).saturating_sub(1);
            if idx >= MAX_SLOTS {
                return Some(OutboundMsg::Error {
                    message: format!("invalid slot {slot}"),
                    seq,
                });
            }
            let callsign = match st.slots[idx].take() {
                Some(a) => a.callsign,
                None => {
                    return Some(OutboundMsg::Error {
                        message: format!("slot {slot} is empty"),
                        seq,
                    })
                }
            };
            if st.target == Some(slot) {
                st.target = None;
            }
            Some(OutboundMsg::Terminated { slot, callsign, seq })
        }

        "rename" => {
            let slot = raw.slot?;
            let callsign = raw.callsign.as_deref().unwrap_or("").to_string();
            let mut st = state.lock().unwrap();
            let idx = (slot as usize).saturating_sub(1);
            if idx >= MAX_SLOTS {
                return Some(OutboundMsg::Error {
                    message: format!("invalid slot {slot}"),
                    seq,
                });
            }
            match st.slots[idx].as_mut() {
                Some(a) => {
                    a.callsign = callsign.clone();
                    Some(OutboundMsg::Renamed { slot, callsign, seq })
                }
                None => Some(OutboundMsg::Error {
                    message: format!("slot {slot} is empty"),
                    seq,
                }),
            }
        }

        "radio_status" => {
            // Consumed for state tracking; no response needed.
            let _state_str = raw.state.as_deref().unwrap_or("idle");
            None
        }

        _ => {
            // Unknown type: silently ignored per spec.
            None
        }
    }
}

// --- Unit tests ----------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> SharedState {
        Arc::new(Mutex::new(ConsoleState::new()))
    }

    fn raw(type_: &str) -> RawInbound {
        RawInbound {
            msg_type: type_.to_string(),
            seq: None,
            slot: None,
            text: None,
            auto: None,
            tool: None,
            callsign: None,
            state: None,
        }
    }

    #[test]
    fn list_agents_empty() {
        let state = make_state();
        let msg = raw("list_agents");
        let resp = handle_message(msg, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"agents\""));
        assert!(json.contains("\"queued_tasks\":0"));
    }

    #[test]
    fn dispatch_and_list() {
        let state = make_state();

        let mut msg = raw("dispatch");
        msg.tool = Some("claude-code".to_string());
        let resp = handle_message(msg, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"dispatched\""));
        assert!(json.contains("\"slot\":1"));
        assert!(json.contains("\"callsign\":\"Alpha\""));

        // list_agents should now show slot 1 idle
        let resp2 = handle_message(raw("list_agents"), &state).unwrap();
        let json2 = serde_json::to_string(&resp2).unwrap();
        assert!(json2.contains("\"status\":\"idle\""));
    }

    #[test]
    fn set_target_and_send() {
        let state = make_state();

        // Dispatch first
        let mut d = raw("dispatch");
        d.tool = Some("claude-code".to_string());
        handle_message(d, &state);

        // Set target to slot 1
        let mut st = raw("set_target");
        st.slot = Some(1);
        let resp = handle_message(st, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"target_changed\""));

        // Send to current target
        let mut s = raw("send");
        s.text = Some("hello".to_string());
        let resp2 = handle_message(s, &state).unwrap();
        let json2 = serde_json::to_string(&resp2).unwrap();
        assert!(json2.contains("\"type\":\"ack\""));
        assert!(json2.contains("\"slot\":1"));
    }

    #[test]
    fn terminate_clears_target() {
        let state = make_state();

        let mut d = raw("dispatch");
        d.tool = Some("claude-code".to_string());
        handle_message(d, &state);

        let mut st = raw("set_target");
        st.slot = Some(1);
        handle_message(st, &state);

        let mut t = raw("terminate");
        t.slot = Some(1);
        handle_message(t, &state);

        let locked = state.lock().unwrap();
        assert!(locked.target.is_none());
        assert!(locked.slots[0].is_none());
    }

    #[test]
    fn rename_agent() {
        let state = make_state();

        let mut d = raw("dispatch");
        d.tool = Some("claude-code".to_string());
        handle_message(d, &state);

        let mut r = raw("rename");
        r.slot = Some(1);
        r.callsign = Some("Maverick".to_string());
        let resp = handle_message(r, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"renamed\""));
        assert!(json.contains("\"callsign\":\"Maverick\""));

        let locked = state.lock().unwrap();
        assert_eq!(locked.slots[0].as_ref().unwrap().callsign, "Maverick");
    }

    #[test]
    fn auto_sends_voice_transcript() {
        // dispatch-h62: auto:true now sends VoiceTranscript to orchestrator.
        let state = make_state();
        let (tx, rx) = std::sync::mpsc::channel();
        state.lock().unwrap().event_tx = Some(tx);

        let mut s = raw("send");
        s.text = Some("write tests".to_string());
        s.auto = Some(true);
        let resp = handle_message(s, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"ack\""));
        assert!(json.contains("\"callsign\":\"Orchestrator\""));

        match rx.try_recv().unwrap() {
            WsEvent::VoiceTranscript { text } => {
                assert_eq!(text, "write tests");
            }
        }
    }

    #[test]
    fn seq_echo() {
        let state = make_state();
        let mut msg = raw("list_agents");
        msg.seq = Some(42);
        let resp = handle_message(msg, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"seq\":42"));
    }

    #[test]
    fn unknown_type_silently_ignored() {
        let state = make_state();
        let msg = raw("some_future_type");
        let resp = handle_message(msg, &state);
        assert!(resp.is_none());
    }

    #[test]
    fn radio_status_no_response() {
        let state = make_state();
        let mut msg = raw("radio_status");
        msg.state = Some("listening".to_string());
        let resp = handle_message(msg, &state);
        assert!(resp.is_none());
    }

    #[test]
    fn error_on_set_target_empty_slot() {
        let state = make_state();
        let mut msg = raw("set_target");
        msg.slot = Some(1);
        let resp = handle_message(msg, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"error\""));
    }

    #[test]
    fn auto_always_sends_voice_transcript() {
        // dispatch-h62: all auto:true prompts (short or long) go to orchestrator.
        let state = make_state();
        let (tx, rx) = std::sync::mpsc::channel();
        state.lock().unwrap().event_tx = Some(tx);

        // Short prompt
        let mut s = raw("send");
        s.text = Some("fix the login bug".to_string());
        s.auto = Some(true);
        let resp = handle_message(s, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"callsign\":\"Orchestrator\""));

        match rx.try_recv().unwrap() {
            WsEvent::VoiceTranscript { text } => {
                assert_eq!(text, "fix the login bug");
            }
        }

        // Long prompt — same behavior (no word-count routing)
        let mut s2 = raw("send");
        s2.text = Some(
            "refactor the entire authentication system to use OAuth2 with JWT tokens \
             and add refresh token rotation plus session management with Redis backend"
                .to_string(),
        );
        s2.auto = Some(true);
        handle_message(s2, &state);

        match rx.try_recv().unwrap() {
            WsEvent::VoiceTranscript { text } => {
                assert!(text.contains("refactor"));
            }
        }
    }
}
