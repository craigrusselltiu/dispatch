// App state and core logic for the dispatch console.

use std::{
    io::Write,
    time::Instant,
};

use chrono::Local;
use dispatch_core::{protocol, tools};

use crate::types::*;
use crate::pty::dispatch_slot;
use crate::util::repo_name_from_path;
use crate::ws_server;

impl App {
    pub fn new(
        psk: String,
        port: u16,
        ws_state: ws_server::SharedState,
        pane_rows: u16,
        pane_cols: u16,
        tools: std::collections::HashMap<String, String>,
        workspace: Workspace,
        scrollback_lines: u32,
        chat_tx: tokio::sync::broadcast::Sender<String>,
        agent_msg_tx: std::sync::mpsc::Sender<(usize, String)>,
        callsigns: Vec<String>,
        user_callsign: String,
        console_name: String,
    ) -> Self {
        let slot_count = callsigns.len();
        let slots: Vec<Option<SlotState>> = (0..slot_count).map(|_| None).collect();
        App {
            slots,
            callsigns,
            current_page: 0,
            target: 0,
            mode: Mode::Command,
            esc_exit_time: None,
            radio_state: RadioState::Disconnected,
            psk,
            port,
            psk_expanded: false,
            overlay: Overlay::None,
            ws_state,
            pane_rows,
            pane_cols,
            tools,
            ticker_tape: String::new(),
            ticker_tape_chars: 0,
            ticker_offset: 0,
            ticker_frame_counter: 0,
            workspace,
            repo_select_idx: 0,
            scrollback_lines,
            view_mode: ViewMode::Agents,
            orch_log: std::collections::VecDeque::new(),
            orch_scroll: 0,
            orchestrator: None,
            pending_voice: Vec::new(),
            chat_tx,
            agent_msg_tx,
            status_blink_frame: 0,
            user_callsign,
            console_name,
        }
    }

    /// Push an event to the orchestrator log (dispatch-6nm).
    pub fn push_orch(&mut self, kind: OrchestratorEventKind) {
        let time = Local::now().format("%H:%M:%S").to_string();
        self.orch_log.push_back(OrchestratorEvent { time, kind });
        // Cap at 500 entries to bound memory.
        if self.orch_log.len() > 500 {
            self.orch_log.pop_front();
            self.orch_scroll = self.orch_scroll.saturating_sub(1);
        }
    }

    /// Push a chat message to all connected radio clients (dispatch-chat).
    pub fn push_chat(&self, sender: &str, text: &str) {
        let msg = protocol::OutboundMsg::Chat {
            sender: sender.to_string(),
            text: text.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = self.chat_tx.send(json);
        }
    }

    /// Broadcast current agent state to all connected radio clients.
    /// Called whenever agent slots change so the radio stays in sync.
    pub fn broadcast_agents(&self) {
        let st = self.ws_state.lock().unwrap();
        let msg = protocol::OutboundMsg::Agents {
            slots: st.all_slot_infos(),
            target: st.target,
            queued_tasks: st.queued_tasks.len() as u32,
            user_callsign: Some(st.user_callsign.clone()),
            console_name: Some(st.console_name.clone()),
            seq: None,
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = self.chat_tx.send(json);
        }
    }

    pub fn global_idx(&self, local_idx: usize) -> usize {
        self.current_page * SLOTS_PER_PAGE + local_idx
    }

    pub fn target_global(&self) -> usize {
        self.global_idx(self.target)
    }

    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Total pages: determined by the configured slot count (callsigns list length).
    pub fn total_pages(&self) -> usize {
        (self.slots.len() + SLOTS_PER_PAGE - 1) / SLOTS_PER_PAGE
    }

    pub fn psk_display(&self) -> String {
        if self.psk_expanded {
            self.psk.clone()
        } else if self.psk.len() >= 4 {
            format!("{}...", &self.psk[..4])
        } else {
            "****".to_string()
        }
    }

    pub fn tool_cmd(&self, tool_key: &str) -> &str {
        self.tools
            .get(tool_key)
            .map(|s| s.as_str())
            .unwrap_or("claude")
    }

    /// Whether we're in multi-repo mode (dispatch-sa1).
    pub fn is_multi_repo(&self) -> bool {
        matches!(self.workspace, Workspace::MultiRepo { .. })
    }

    /// Get the list of repos (dispatch-sa1). Single-repo returns a one-element vec.
    pub fn repo_list(&self) -> Vec<&str> {
        match &self.workspace {
            Workspace::SingleRepo { root } => vec![root.as_str()],
            Workspace::MultiRepo { repos, .. } => repos.iter().map(|s| s.as_str()).collect(),
        }
    }

    /// Default repo root: first repo in list (dispatch-sa1).
    pub fn default_repo_root(&self) -> &str {
        match &self.workspace {
            Workspace::SingleRepo { root } => root.as_str(),
            Workspace::MultiRepo { repos, .. } => repos.first().map(|s| s.as_str()).unwrap_or("."),
        }
    }

    /// Next unused callsign from the configured list (dynamic assignment).
    pub fn next_callsign(&self) -> Option<String> {
        let used: std::collections::HashSet<String> = self.slots.iter()
            .filter_map(|s| s.as_ref().map(|slot| slot.display_name().to_uppercase()))
            .collect();
        self.callsigns.iter()
            .find(|cs| !used.contains(&cs.to_uppercase()))
            .cloned()
    }

    /// Re-scan child directories for git repos in multi-repo mode (dispatch-sa1).
    pub fn rescan_repos(&mut self) {
        if let Workspace::MultiRepo { parent, repos } = &mut self.workspace {
            *repos = crate::util::scan_child_repos(parent);
        }
    }

    /// Advance the status blink frame counter.
    /// Called once per render loop (~16ms). Produces a ~1s cycle (60 frames).
    pub fn tick_status_blink(&mut self) {
        self.status_blink_frame = self.status_blink_frame.wrapping_add(1);
    }

    /// Whether the status indicator dot should be "on" (visible) this frame.
    /// On for ~70% of the cycle, off for ~30% — mimics a recording indicator light.
    pub fn status_blink_on(&self) -> bool {
        (self.status_blink_frame % 60) < 42
    }

    /// Append a message to the continuous ticker tape.
    /// New content flows in from the right after any existing content.
    pub fn push_ticker(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.ticker_tape.is_empty() || self.ticker_offset > self.ticker_tape_chars + 300 {
            // Tape is empty or all content has scrolled off-screen; start fresh.
            self.ticker_tape = msg;
            self.ticker_tape_chars = self.ticker_tape.chars().count();
            self.ticker_offset = 0;
            self.ticker_frame_counter = 0;
        } else {
            // Append with separator so it flows in after existing content.
            self.ticker_tape.push_str("  ///  ");
            self.ticker_tape.push_str(&msg);
            self.ticker_tape_chars = self.ticker_tape.chars().count();
        }
    }

    /// Advance the ticker by one frame (~16ms). Scrolls one char every 3 frames (~50ms).
    pub fn tick_ticker(&mut self) {
        if self.ticker_tape.is_empty() {
            return;
        }
        self.ticker_frame_counter = self.ticker_frame_counter.wrapping_add(1);
        if self.ticker_frame_counter % 3 == 0 {
            self.ticker_offset += 1;
            // Clear tape once everything has scrolled off-screen.
            if self.ticker_offset > self.ticker_tape_chars + 300 {
                self.ticker_tape.clear();
                self.ticker_tape_chars = 0;
                self.ticker_offset = 0;
            }
        }
    }

    /// Build the visible ticker string for a given display width.
    /// Content scrolls right-to-left: enters from the right edge, exits left.
    /// Tape char at index i appears at screen position (width - offset + i).
    pub fn ticker_display(&self, width: usize) -> String {
        if self.ticker_tape.is_empty() {
            return " ".repeat(width);
        }
        let mut line = vec![' '; width];
        // Skip chars already off-screen to the left.
        let start = self.ticker_offset.saturating_sub(width);
        for (idx, ch) in self.ticker_tape.chars().enumerate().skip(start) {
            let pos = width as isize - self.ticker_offset as isize + idx as isize;
            if pos >= width as isize {
                break; // Rest is off-screen to the right.
            }
            if pos >= 0 {
                line[pos as usize] = ch;
            }
        }
        line.into_iter().collect()
    }

    // ── orchestrator tool execution (dispatch-x94) ──────────────────────────

    /// Execute a tool call from the orchestrator agent. Returns the result.
    pub fn execute_tool(&mut self, call: &tools::ToolCall) -> tools::ToolResult {
        match call {
            tools::ToolCall::Dispatch { repo: _, prompt, callsign: requested_callsign } => {
                // Dynamic callsign assignment: agents go into the next
                // available slot rather than a fixed slot per callsign.
                let (slot_idx, callsign_for_prompt) = if let Some(cs) = requested_callsign.as_deref() {
                    // Check if an agent with this callsign is already active.
                    let active_idx = self.slots.iter().enumerate().find_map(|(i, s)| {
                        s.as_ref().and_then(|slot| {
                            if slot.display_name().eq_ignore_ascii_case(cs) {
                                Some(i)
                            } else {
                                None
                            }
                        })
                    });

                    if let Some(idx) = active_idx {
                        // Agent exists — must be idle (no active task).
                        match &self.slots[idx] {
                            Some(slot) if slot.task_id.is_some() => {
                                return tools::ToolResult::Error {
                                    message: format!("{} (slot {}) is busy", cs, idx + 1),
                                };
                            }
                            _ => (Some(idx), cs.to_string()),
                        }
                    } else {
                        // Callsign not active — assign to first empty slot.
                        match self.slots.iter().position(|s| s.is_none()) {
                            Some(idx) => (Some(idx), cs.to_string()),
                            None => return tools::ToolResult::Error {
                                message: "no available slots".to_string(),
                            },
                        }
                    }
                } else {
                    // No callsign requested: find an idle slot or empty slot.
                    let idle = self.slots.iter().enumerate().find_map(|(i, s)| {
                        match s {
                            Some(slot) if slot.task_id.is_none() => Some(i),
                            _ => None,
                        }
                    });

                    if let Some(idx) = idle {
                        let cs = self.slots[idx].as_ref().unwrap().display_name().to_string();
                        (Some(idx), cs)
                    } else {
                        // Empty slot + next available callsign from the pool.
                        match self.slots.iter().position(|s| s.is_none()) {
                            Some(idx) => {
                                let cs = self.next_callsign()
                                    .unwrap_or_else(|| format!("Agent-{}", idx + 1));
                                (Some(idx), cs)
                            }
                            None => return tools::ToolResult::Error {
                                message: "no available slots".to_string(),
                            },
                        }
                    }
                };

                let slot_idx = match slot_idx {
                    Some(i) => i,
                    None => return tools::ToolResult::Error {
                        message: "no available slots".to_string(),
                    },
                };

                let target_repo = self.default_repo_root().to_string();

                let full_prompt = format!("Your callsign is {}. {}", callsign_for_prompt, prompt);

                // Spawn PTY if slot is empty. Agent creates its own worktree.
                if self.slots[slot_idx].is_none() {
                    let cmd = self.tool_cmd("claude-code").to_string();
                    match dispatch_slot(
                        slot_idx, "claude-code", &cmd, self.pane_rows, self.pane_cols,
                        None, self.scrollback_lines,
                        repo_name_from_path(&target_repo), &target_repo,
                        Some(&full_prompt),
                        self.agent_msg_tx.clone(),
                        &callsign_for_prompt,
                    ) {
                        Some(slot) => { self.slots[slot_idx] = Some(slot); }
                        None => return tools::ToolResult::Error {
                            message: "failed to spawn agent PTY".to_string(),
                        },
                    }
                }

                let callsign = {
                    let slot = self.slots[slot_idx].as_mut().unwrap();
                    slot.task_id = Some(prompt.clone());
                    *slot.last_output_at.lock().unwrap() = Instant::now();
                    slot.idle = false;
                    slot.display_name().to_string()
                };

                self.push_orch(OrchestratorEventKind::Dispatched {
                    agent: callsign.clone(), slot: slot_idx + 1, tool: "claude-code".to_string(),
                });
                self.push_ticker(format!(
                    "DISPATCH: {} (slot {})", callsign, slot_idx + 1
                ));
                self.push_chat("System", &format!("Dispatched agent {} to slot {}.", callsign, slot_idx + 1));

                // Sync ws_state.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    st.slots[slot_idx] = Some(ws_server::AgentSlot {
                        callsign: callsign.clone(),
                        tool: "claude-code".to_string(),
                        status: ws_server::AgentStatus::Busy,
                        task: None,
                        repo: Some(repo_name_from_path(&target_repo).to_string()),
                    });
                }
                self.broadcast_agents();

                tools::ToolResult::Dispatched {
                    slot: (slot_idx + 1) as u32,
                    callsign,
                    task_id: "none".to_string(),
                }
            }

            tools::ToolCall::Terminate { agent } => {
                let (slot_occupied, callsigns): (Vec<bool>, Vec<Option<String>>) = self.slots
                    .iter()
                    .map(|s| match s {
                        Some(slot) => (true, Some(slot.display_name().to_string())),
                        None => (false, None),
                    })
                    .unzip();

                let idx = match tools::resolve_agent(agent, &slot_occupied, &callsigns) {
                    Some(i) => i,
                    None => return tools::ToolResult::Error {
                        message: format!("agent '{}' not found", agent),
                    },
                };

                let callsign = self.slots[idx].as_ref().unwrap().display_name().to_string();
                crate::pty::terminate_slot(&mut self.slots[idx]);

                self.push_orch(OrchestratorEventKind::Terminated {
                    agent: callsign.clone(), slot: idx + 1,
                });
                self.push_ticker(format!("TERMINATED: {} (slot {})", callsign, idx + 1));
                self.push_chat("System", &format!("Terminated agent {} (slot {}).", callsign, idx + 1));

                // Sync ws_state.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    st.slots[idx] = None;
                    if st.target == Some((idx + 1) as u32) {
                        st.target = None;
                    }
                }
                self.broadcast_agents();

                tools::ToolResult::Terminated {
                    slot: (idx + 1) as u32,
                    callsign,
                }
            }

            // Agents merge their own branches; this acknowledges the completion.
            tools::ToolCall::Merge { agent } => {
                self.push_orch(OrchestratorEventKind::Merged { id: agent.clone() });
                self.push_ticker(format!("MERGED: {}", agent));
                self.push_chat("System", &format!("{} has merged to remote.", agent));
                tools::ToolResult::Merged {
                    agent: agent.clone(),
                    success: true,
                    message: format!("{} merged by agent", agent),
                }
            }

            tools::ToolCall::ListAgents => {
                let agents: Vec<tools::AgentInfo> = self.slots.iter().enumerate()
                    .filter_map(|(i, s)| {
                        s.as_ref().map(|slot| {
                            let status = if slot.task_id.is_some() && !slot.idle {
                                "working".to_string()
                            } else {
                                "idle".to_string()
                            };
                            tools::AgentInfo {
                                slot: (i + 1) as u32,
                                callsign: slot.display_name().to_string(),
                                tool: slot.tool.clone(),
                                status,
                                task: slot.task_id.clone(),
                                repo: Some(slot.repo_name.clone()),
                            }
                        })
                    })
                    .collect();
                tools::ToolResult::Agents { agents }
            }

            tools::ToolCall::ListRepos => {
                let repos = self.repo_list().iter().map(|path| {
                    tools::RepoInfo {
                        name: repo_name_from_path(path).to_string(),
                        path: path.to_string(),
                    }
                }).collect();
                tools::ToolResult::Repos { repos }
            }

            tools::ToolCall::MessageAgent { agent, text } => {
                let (slot_occupied, callsigns): (Vec<bool>, Vec<Option<String>>) = self.slots
                    .iter()
                    .map(|s| match s {
                        Some(slot) => (true, Some(slot.display_name().to_string())),
                        None => (false, None),
                    })
                    .unzip();

                let idx = match tools::resolve_agent(agent, &slot_occupied, &callsigns) {
                    Some(i) => i,
                    None => return tools::ToolResult::Error {
                        message: format!("agent '{}' not found", agent),
                    },
                };

                let slot = self.slots[idx].as_mut().unwrap();
                let agent_name = slot.display_name().to_string();
                let msg = format!("{}\r", text);
                let _ = slot.writer.write_all(msg.as_bytes());
                let _ = slot.writer.flush();
                *slot.last_output_at.lock().unwrap() = Instant::now();
                slot.idle = false;

                self.push_chat("System", &format!("[to {}] {}", agent_name, text));

                tools::ToolResult::MessageSent {
                    agent: agent_name,
                    slot: (idx + 1) as u32,
                }
            }
        }
    }

}
