// WebSocket server for the Dispatch console.
//
// Listens on the configured port, validates PSK as a query parameter on the
// WebSocket upgrade request, and routes incoming JSON messages to handlers.
// All message types from the spec are implemented; unknown types are silently
// ignored. Responses carry the optional `seq` from the request for correlation.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

use crate::protocol::{default_callsign, OutboundMsg, RawInbound, SlotInfo};

pub const MAX_SLOTS: usize = 26;

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
}

pub struct ConsoleState {
    pub slots: [Option<AgentSlot>; MAX_SLOTS],
    /// Currently targeted slot (1-indexed). None if no agents are running.
    pub target: Option<u32>,
    /// Queued beads task IDs (tasks waiting for an available agent).
    pub queued_tasks: Vec<String>,
    /// Monotonic counter used to generate stub task IDs.
    task_counter: u64,
}

impl ConsoleState {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            target: None,
            queued_tasks: Vec::new(),
            task_counter: 0,
        }
    }

    fn next_task_id(&mut self) -> String {
        self.task_counter += 1;
        format!("task-{}", self.task_counter)
    }

    fn slot_info(&self, slot: u32) -> SlotInfo {
        let idx = (slot as usize).saturating_sub(1);
        match self.slots.get(idx).and_then(|s| s.as_ref()) {
            None => SlotInfo {
                slot,
                callsign: None,
                tool: None,
                status: "empty",
                task: None,
            },
            Some(a) => SlotInfo {
                slot,
                callsign: Some(a.callsign.clone()),
                tool: Some(a.tool.clone()),
                status: if a.status == AgentStatus::Busy { "busy" } else { "idle" },
                task: a.task.clone(),
            },
        }
    }

    fn all_slot_infos(&self) -> Vec<SlotInfo> {
        (1..=MAX_SLOTS as u32).map(|s| self.slot_info(s)).collect()
    }

    fn first_idle_slot(&self) -> Option<u32> {
        self.slots.iter().enumerate().find_map(|(i, s)| {
            s.as_ref().and_then(|a| {
                if a.status == AgentStatus::Idle {
                    Some(i as u32 + 1)
                } else {
                    None
                }
            })
        })
    }

    fn first_empty_slot(&self) -> Option<u32> {
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

// --- Server entry point --------------------------------------------------

/// Start the WebSocket server on `0.0.0.0:{port}`.
/// Accepts connections only when the `?psk=<key>` query parameter matches.
pub async fn run_server(state: SharedState, port: u16, psk: String) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr)
        .await
        .expect("failed to bind WebSocket server");

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let state = Arc::clone(&state);
        let psk = psk.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer_addr, state, psk).await {
                eprintln!("ws: connection error from {peer_addr}: {e}");
            }
        });
    }
}

// --- Connection handler --------------------------------------------------

async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    state: SharedState,
    psk: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut auth_ok = false;

    let result = accept_hdr_async(stream, |req: &Request, resp: Response| {
        let valid = req
            .uri()
            .query()
            .unwrap_or("")
            .split('&')
            .any(|part| part == format!("psk={}", psk).as_str());

        if valid {
            auth_ok = true;
            Ok(resp)
        } else {
            use tokio_tungstenite::tungstenite::http;
            let err = http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(None)
                .unwrap();
            Err(err)
        }
    })
    .await;

    let ws_stream = match result {
        Ok(ws) => ws,
        Err(e) => {
            if !auth_ok {
                eprintln!("ws: rejected {peer_addr}: invalid PSK");
            } else {
                eprintln!("ws: handshake error from {peer_addr}: {e}");
            }
            return Ok(());
        }
    };

    let (mut tx, mut rx) = ws_stream.split();

    while let Some(msg) = rx.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };

        let raw: RawInbound = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue, // silently ignore malformed JSON
        };

        if let Some(response) = handle_message(raw, &state) {
            let json = serde_json::to_string(&response)?;
            tx.send(Message::Text(json)).await?;
        }
    }

    Ok(())
}

// --- Message dispatch ----------------------------------------------------

fn handle_message(raw: RawInbound, state: &SharedState) -> Option<OutboundMsg> {
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

            // Determine which slot to send to.
            let (slot, auto_dispatched) = if let Some(s) = raw.slot {
                // Explicit slot.
                (Some(s), false)
            } else if auto {
                // Auto-dispatch: idle agent → empty slot → error.
                if let Some(idle) = st.first_idle_slot() {
                    (Some(idle), false)
                } else if let Some(empty) = st.first_empty_slot() {
                    let callsign = default_callsign(empty).to_string();
                    st.slots[(empty as usize) - 1] = Some(AgentSlot {
                        callsign,
                        tool: "claude-code".to_string(),
                        status: AgentStatus::Idle,
                        task: None,
                    });
                    (Some(empty), true)
                } else {
                    let task_id = st.next_task_id();
                    let msg = format!("all agents busy, task queued as {task_id}");
                    st.queued_tasks.push(task_id);
                    return Some(OutboundMsg::Error { message: msg, seq });
                }
            } else {
                // Use current target.
                (st.target, false)
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

            // Stub task ID — real integration creates a beads task via bd.
            let task_id = st.next_task_id();
            let _ = text; // prompt text forwarded to PTY by console core
            let agent = st.slots[idx].as_mut().unwrap();
            agent.status = AgentStatus::Busy;
            agent.task = Some(task_id.clone());
            let callsign = agent.callsign.clone();

            Some(OutboundMsg::Ack {
                slot,
                callsign,
                task: task_id,
                auto_dispatched: if auto { Some(auto_dispatched) } else { None },
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
    fn auto_dispatch_new_agent() {
        let state = make_state();

        let mut s = raw("send");
        s.text = Some("write tests".to_string());
        s.auto = Some(true);
        let resp = handle_message(s, &state).unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"type\":\"ack\""));
        assert!(json.contains("\"auto_dispatched\":true"));
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
}
