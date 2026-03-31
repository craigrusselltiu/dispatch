// App state and core logic for the dispatch console.

use std::{
    io::Write,
    path::Path,
    process::Command,
    sync::Arc,
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use dispatch_core::{protocol, strike_team, tools};

use crate::pty::dispatch_slot;
use crate::types::*;
use crate::util::repo_name_from_path;
use crate::ws_server;

/// Prompt templates bundled at compile time.
const PLANNER_MD: &str = include_str!("../../docs/PLANNER.md");
const VERIFIER_MD: &str = include_str!("../../docs/VERIFIER.md");

/// Apply placeholder substitutions to a bundled prompt template.
fn render_prompt_template(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut text = template.to_string();
    for &(placeholder, value) in replacements {
        text = text.replace(placeholder, value);
    }
    text
}

fn resolve_repo_relative_path_case_insensitive(
    repo_root: &str,
    relative_path: &str,
) -> Option<String> {
    let trimmed = relative_path.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }

    let mut current = std::path::PathBuf::from(repo_root);
    let mut exact_parts = Vec::new();
    for component in Path::new(trimmed).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                let wanted = part.to_string_lossy();
                let entry = std::fs::read_dir(&current)
                    .ok()?
                    .filter_map(Result::ok)
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&wanted)
                    })?;
                let exact = entry.file_name().to_string_lossy().into_owned();
                current.push(&exact);
                exact_parts.push(exact);
            }
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }

    if exact_parts.is_empty() || !current.is_file() {
        return None;
    }

    Some(exact_parts.join("/"))
}

fn strike_team_source_candidates(source_file: &str) -> Vec<&'static str> {
    let normalized = source_file.trim().trim_matches('"').to_ascii_lowercase();

    // Match against the shared alias table: check the alias name itself and
    // each candidate path (so "spec.md" matches "the spec" entry).
    for &(alias, candidates) in tools::DOCUMENT_ALIASES {
        let alias_lower = alias.to_ascii_lowercase();
        // Strip leading "the " for bare-word matching (e.g. "spec" -> "the spec").
        let bare = alias_lower.strip_prefix("the ").unwrap_or(&alias_lower);
        if normalized.is_empty()
            || normalized == alias_lower
            || normalized == bare
            || normalized == format!("{}.md", bare)
            || normalized == format!("the {}", bare)
            // "specification" / "the specification" -> "the spec"
            || (bare == "spec"
                && (normalized == "specification" || normalized == "the specification"))
            || candidates
                .iter()
                .any(|c| c.to_ascii_lowercase() == normalized)
        {
            return candidates.to_vec();
        }
    }
    Vec::new()
}

fn resolve_strike_team_source_file(repo_root: &str, source_file: &str) -> Option<String> {
    if let Some(path) = resolve_repo_relative_path_case_insensitive(repo_root, source_file) {
        return Some(path);
    }

    for candidate in strike_team_source_candidates(source_file) {
        if let Some(path) = resolve_repo_relative_path_case_insensitive(repo_root, candidate) {
            return Some(path);
        }
    }

    None
}

impl App {
    pub fn new(
        psk: String,
        port: u16,
        ws_state: ws_server::SharedState,
        pane_rows: u16,
        pane_cols: u16,
        tools: std::collections::HashMap<String, String>,
        default_tool: String,
        merge_strategy: String,
        workspace: Workspace,
        scrollback_lines: u32,
        chat_tx: tokio::sync::broadcast::Sender<String>,
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
            default_tool,
            merge_strategy,
            ticker_items: Vec::new(),
            ticker_pending: std::collections::VecDeque::new(),
            ticker_frame_counter: 0,
            workspace,
            repo_select_idx: 0,
            scrollback_lines,
            view_mode: ViewMode::Agents,
            orch_log: std::collections::VecDeque::new(),
            orch_scroll: 0,
            orchestrator: None,
            orch_error: None,
            pending_voice: Vec::new(),
            chat_tx,
            status_blink_frame: 0,
            user_callsign,
            console_name,
            strike_teams: Vec::new(),
            user_initiated_turn: false,
            needs_redraw: true,
            cached_clock: String::new(),
            cached_clock_second: 0,
            frame_counter: 0,
            ticker_char_buf: Vec::new(),
            ticker_str_buf: String::new(),
        }
    }

    /// Push an event to the orchestrator log (dispatch-6nm).
    pub fn push_orch(&mut self, kind: OrchestratorEventKind) {
        let time = crate::util::local_time_hms();
        self.orch_log.push_back(OrchestratorEvent { time, kind });
        // Cap at 500 entries to bound memory.
        if self.orch_log.len() > 500 {
            self.orch_log.pop_front();
            self.orch_scroll = self.orch_scroll.saturating_sub(1);
        }
        self.needs_redraw = true;
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
            orchestrator_status: Some(st.orchestrator_status.clone()),
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

    /// Compute the current orchestrator status string and, if it changed,
    /// sync it into the shared WebSocket state and broadcast to radio clients.
    pub fn sync_orchestrator_status(&self) {
        let status = self.orchestrator_status_str().to_string();
        let changed = {
            let mut st = self.ws_state.lock().unwrap();
            if st.orchestrator_status != status {
                st.orchestrator_status = status;
                true
            } else {
                false
            }
        };
        if changed {
            self.broadcast_agents();
        }
    }

    /// Return the orchestrator status as a wire-protocol string.
    fn orchestrator_status_str(&self) -> &'static str {
        use dispatch_core::orchestrator;
        match &self.orchestrator {
            Some(o) if o.is_alive() => match o.state {
                orchestrator::OrchestratorState::Idle => "idle",
                orchestrator::OrchestratorState::Responding => "thinking",
                orchestrator::OrchestratorState::Dead => "dead",
            },
            Some(_) => "dead",
            None if self.orch_error.is_some() => "failed",
            None => "starting",
        }
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

    /// The root directory where dispatch was initialized.
    pub fn init_path(&self) -> &str {
        match &self.workspace {
            Workspace::SingleRepo { root } => root.as_str(),
            Workspace::MultiRepo { parent, .. } => parent.as_str(),
        }
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

    /// Resolve a repo name or path to a full repo root path. Matches against
    /// the short directory name (case-insensitive) first, then tries a path
    /// suffix match. Falls back to default_repo_root() if no match is found.
    pub fn resolve_repo(&self, name: &str) -> String {
        if name.is_empty() {
            return self.default_repo_root().to_string();
        }
        let repos = self.repo_list();
        // Exact match on short directory name (case-insensitive).
        if let Some(path) = repos
            .iter()
            .find(|p| repo_name_from_path(p).eq_ignore_ascii_case(name))
        {
            return path.to_string();
        }
        // Path suffix match (e.g. "GitHub/testament" matches ".../GitHub/testament").
        if let Some(path) = repos
            .iter()
            .find(|p| p.to_lowercase().ends_with(&name.to_lowercase()))
        {
            return path.to_string();
        }
        self.default_repo_root().to_string()
    }

    /// Next unused callsign from the configured list (dynamic assignment).
    pub fn next_callsign(&self) -> Option<String> {
        let used: std::collections::HashSet<String> = self
            .slots
            .iter()
            .filter_map(|s| s.as_ref().map(|slot| slot.display_name().to_uppercase()))
            .collect();
        self.callsigns
            .iter()
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
    /// Only marks a redraw when the blink visual state actually changes.
    pub fn tick_status_blink(&mut self) {
        let old_on = (self.status_blink_frame % 60) < 42;
        self.status_blink_frame = self.status_blink_frame.wrapping_add(1);
        let new_on = (self.status_blink_frame % 60) < 42;
        if old_on != new_on {
            self.needs_redraw = true;
        }
    }

    /// Whether the status indicator dot should be "on" (visible) this frame.
    /// On for ~70% of the cycle, off for ~30% — mimics a recording indicator light.
    pub fn status_blink_on(&self) -> bool {
        (self.status_blink_frame % 60) < 42
    }

    /// Push a new independently-scrolling ticker message.
    /// If an item is already scrolling, queue the new message so it waits
    /// rather than overlapping the current display.
    pub fn push_ticker(&mut self, msg: impl Into<String>) {
        let text = msg.into();
        if self.ticker_items.is_empty() && self.ticker_pending.is_empty() {
            let char_count = text.chars().count();
            self.ticker_items.push(TickerItem {
                text,
                char_count,
                offset: 0,
            });
        } else {
            self.ticker_pending.push_back(text);
        }
        self.needs_redraw = true;
    }

    /// Advance the ticker by one frame (~16ms). Scrolls one char every 3 frames (~50ms).
    pub fn tick_ticker(&mut self) {
        if self.ticker_items.is_empty() && self.ticker_pending.is_empty() {
            return;
        }
        self.needs_redraw = true;
        self.ticker_frame_counter = self.ticker_frame_counter.wrapping_add(1);
        if self.ticker_frame_counter % 3 == 0 {
            for item in &mut self.ticker_items {
                item.offset += 1;
            }
            // Remove items that have fully scrolled off the left edge.
            // An item is off-screen when its offset exceeds char_count + generous margin.
            self.ticker_items
                .retain(|item| item.offset <= item.char_count + 300);

            // Promote a pending item once the last active item has fully entered
            // the screen (scrolled past its own length + a small gap).
            if !self.ticker_pending.is_empty() {
                let can_promote = if let Some(last) = self.ticker_items.last() {
                    last.offset >= last.char_count + 3
                } else {
                    true
                };
                if can_promote {
                    if let Some(text) = self.ticker_pending.pop_front() {
                        let char_count = text.chars().count();
                        self.ticker_items.push(TickerItem {
                            text,
                            char_count,
                            offset: 0,
                        });
                    }
                }
            }
        }
    }

    /// Build the visible ticker string for a given display width.
    /// Each item scrolls independently right-to-left.
    /// Item char at index i appears at screen position (width - offset + i).
    /// Reuses internal buffers to avoid per-frame allocations.
    pub fn ticker_display(&mut self, width: usize) -> &str {
        self.ticker_str_buf.clear();
        if self.ticker_items.is_empty() {
            for _ in 0..width {
                self.ticker_str_buf.push(' ');
            }
            return &self.ticker_str_buf;
        }
        self.ticker_char_buf.clear();
        self.ticker_char_buf.resize(width, ' ');
        // Render older items first so newer items layer on top if they overlap.
        for item in &self.ticker_items {
            let start = item.offset.saturating_sub(width);
            for (idx, ch) in item.text.chars().enumerate().skip(start) {
                let pos = width as isize - item.offset as isize + idx as isize;
                if pos >= width as isize {
                    break;
                }
                if pos >= 0 {
                    self.ticker_char_buf[pos as usize] = ch;
                }
            }
        }
        self.ticker_str_buf.extend(self.ticker_char_buf.iter());
        &self.ticker_str_buf
    }

    /// Get cached clock string (HH:MM), updated at most once per second.
    pub fn clock_display(&mut self) -> &str {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Refresh clock string only when the minute changes (every 60s).
        if now_secs / 60 != self.cached_clock_second / 60 || self.cached_clock.is_empty() {
            self.cached_clock = crate::util::local_time_hm();
            self.cached_clock_second = now_secs;
        }
        &self.cached_clock
    }

    // ── orchestrator tool execution (dispatch-x94) ──────────────────────────

    /// Execute a tool call from the orchestrator agent. Returns the result.
    pub fn execute_tool(&mut self, call: &tools::ToolCall) -> tools::ToolResult {
        match call {
            tools::ToolCall::Dispatch {
                repo,
                prompt,
                callsign: requested_callsign,
                tool: requested_tool,
            } => {
                // Dynamic callsign assignment: agents go into the next
                // available slot rather than a fixed slot per callsign.
                let (slot_idx, callsign_for_prompt) =
                    if let Some(cs) = requested_callsign.as_deref() {
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
                                None => {
                                    return tools::ToolResult::Error {
                                        message: "no available slots".to_string(),
                                    }
                                }
                            }
                        }
                    } else {
                        // No callsign requested: find an idle slot or empty slot.
                        let idle = self.slots.iter().enumerate().find_map(|(i, s)| match s {
                            Some(slot) if slot.task_id.is_none() => Some(i),
                            _ => None,
                        });

                        if let Some(idx) = idle {
                            let cs = self.slots[idx].as_ref().unwrap().display_name().to_string();
                            (Some(idx), cs)
                        } else {
                            // Empty slot + next available callsign from the pool.
                            match self.slots.iter().position(|s| s.is_none()) {
                                Some(idx) => {
                                    let cs = self
                                        .next_callsign()
                                        .unwrap_or_else(|| format!("Agent-{}", idx + 1));
                                    (Some(idx), cs)
                                }
                                None => {
                                    return tools::ToolResult::Error {
                                        message: "no available slots".to_string(),
                                    }
                                }
                            }
                        }
                    };

                let slot_idx = match slot_idx {
                    Some(i) => i,
                    None => {
                        return tools::ToolResult::Error {
                            message: "no available slots".to_string(),
                        }
                    }
                };

                let target_repo = self.resolve_repo(repo);

                let full_prompt = format!("Your callsign is {}. {}", callsign_for_prompt, prompt);

                // Resolve which tool to use for this dispatch.
                let effective_tool = requested_tool
                    .as_deref()
                    .unwrap_or(&self.default_tool)
                    .to_string();

                // Spawn PTY if slot is empty. Agent creates its own worktree.
                if self.slots[slot_idx].is_none() {
                    let cmd = self.tool_cmd(&effective_tool).to_string();
                    match dispatch_slot(
                        slot_idx,
                        &effective_tool,
                        &cmd,
                        self.pane_rows,
                        self.pane_cols,
                        None,
                        self.scrollback_lines,
                        repo_name_from_path(&target_repo),
                        &target_repo,
                        Some(&full_prompt),
                        &callsign_for_prompt,
                        &self.merge_strategy,
                        None,
                    ) {
                        Some(slot) => {
                            self.slots[slot_idx] = Some(slot);
                        }
                        None => {
                            return tools::ToolResult::Error {
                                message: "failed to spawn agent PTY".to_string(),
                            }
                        }
                    }
                } else {
                    // Existing idle agent: write the prompt to the PTY so it
                    // receives the new task (the agent process is still alive).
                    let slot = self.slots[slot_idx].as_mut().unwrap();
                    if let Some(ref sw) = slot.shared_writer {
                        // Copilot: type char-by-char on a background thread
                        // to avoid blocking the main TUI loop.
                        let w = Arc::clone(sw);
                        let ts = Arc::clone(&slot.last_output_at);
                        let prompt = full_prompt.clone();
                        thread::spawn(move || {
                            crate::pty::type_to_copilot(&w, &prompt, &ts);
                        });
                    } else {
                        let msg = format!("{}\r", full_prompt);
                        let _ = slot.writer.write_all(msg.as_bytes());
                        let _ = slot.writer.flush();
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
                    agent: callsign.clone(),
                    slot: slot_idx + 1,
                    tool: effective_tool.clone(),
                });
                self.push_ticker(format!("DISPATCH: {} (slot {})", callsign, slot_idx + 1));
                self.push_chat(
                    "System",
                    &format!(
                        "Dispatched {} to slot {}: {}",
                        callsign,
                        slot_idx + 1,
                        prompt
                    ),
                );

                // Sync ws_state.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    st.slots[slot_idx] = Some(ws_server::AgentSlot {
                        callsign: callsign.clone(),
                        tool: effective_tool,
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
                // Gate: only allow terminate when the current orchestrator turn
                // was triggered by authentic user voice/text input from the radio.
                // Prevents the LLM from hallucinating a fake MIC message and
                // self-authorizing destructive actions.
                if !self.user_initiated_turn {
                    self.push_ticker(
                        "BLOCKED: orchestrator tried to terminate without user input".to_string(),
                    );
                    self.push_chat(
                        "System",
                        &format!(
                            "Terminate {} blocked: no voice/text input from Dispatch.",
                            agent
                        ),
                    );
                    return tools::ToolResult::Error {
                        message: format!(
                            "terminate '{}' rejected: destructive actions require a preceding \
                             voice/text command from Dispatch. This turn was not triggered by \
                             user input.",
                            agent
                        ),
                    };
                }

                let (slot_occupied, callsigns): (Vec<bool>, Vec<Option<String>>) = self
                    .slots
                    .iter()
                    .map(|s| match s {
                        Some(slot) => (true, Some(slot.display_name().to_string())),
                        None => (false, None),
                    })
                    .unzip();

                let idx = match tools::resolve_agent(agent, &slot_occupied, &callsigns) {
                    Some(i) => i,
                    None => {
                        return tools::ToolResult::Error {
                            message: format!("agent '{}' not found", agent),
                        }
                    }
                };

                let callsign = self.slots[idx].as_ref().unwrap().display_name().to_string();
                crate::pty::terminate_slot(&mut self.slots[idx]);

                self.push_orch(OrchestratorEventKind::Terminated {
                    agent: callsign.clone(),
                    slot: idx + 1,
                });
                self.push_ticker(format!("TERMINATED: {} (slot {})", callsign, idx + 1));
                self.push_chat(
                    "System",
                    &format!("Terminated agent {} (slot {}).", callsign, idx + 1),
                );

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

            // Agents merge their own branches; this tool acknowledges the
            // completion and generates the system merge notification.
            tools::ToolCall::Merge { agent } => {
                if self.merge_strategy == "pr" {
                    self.push_orch(OrchestratorEventKind::PrCreated { id: agent.clone() });
                    self.push_ticker(format!("PR CREATED: {}", agent));
                    self.push_chat("System", &format!("{} has created a PR.", agent));
                } else {
                    self.push_orch(OrchestratorEventKind::Merged { id: agent.clone() });
                    self.push_ticker(format!("MERGED: {}", agent));
                    self.push_chat("System", &format!("{} has merged to remote.", agent));
                }

                tools::ToolResult::Merged {
                    agent: agent.clone(),
                    success: true,
                    message: format!("{} merged by agent", agent),
                }
            }

            tools::ToolCall::ListAgents => {
                let agents: Vec<tools::AgentInfo> = self
                    .slots
                    .iter()
                    .enumerate()
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
                let repos = self
                    .repo_list()
                    .iter()
                    .map(|path| tools::RepoInfo {
                        name: repo_name_from_path(path).to_string(),
                        path: path.to_string(),
                    })
                    .collect();
                tools::ToolResult::Repos { repos }
            }

            tools::ToolCall::StrikeTeam {
                source_file,
                name,
                repo,
            } => {
                let target_repo = self.resolve_repo(repo);
                let resolved_source_file = match resolve_strike_team_source_file(
                    &target_repo,
                    source_file,
                ) {
                    Some(path) => path,
                    None => {
                        let repo_name = repo_name_from_path(&target_repo);
                        let message = if source_file.trim().is_empty() {
                            format!(
                                "could not resolve a default strike team document in repo '{}' (expected something like docs/SPEC.md)",
                                repo_name
                            )
                        } else {
                            format!(
                                "could not resolve strike team source file '{}' in repo '{}'",
                                source_file, repo_name
                            )
                        };
                        return tools::ToolResult::Error { message };
                    }
                };
                let st_name = name
                    .as_deref()
                    .unwrap_or_else(|| {
                        // Default name: source filename without extension.
                        resolved_source_file
                            .rsplit('/')
                            .next()
                            .and_then(|f| f.rsplit('\\').next())
                            .and_then(|f| f.strip_suffix(".md"))
                            .unwrap_or(&resolved_source_file)
                    })
                    .to_string();

                // Build planner prompt from docs/PLANNER.md template.
                let planner_prompt = render_prompt_template(
                    PLANNER_MD,
                    &[
                        ("{source_file}", &resolved_source_file),
                        ("{name}", &st_name),
                    ],
                );

                // Dispatch planner agent to repo root (no worktree).
                let callsign_for_prompt = self
                    .next_callsign()
                    .unwrap_or_else(|| "Planner".to_string());
                let slot_idx = match self.slots.iter().position(|s| s.is_none()) {
                    Some(i) => i,
                    None => {
                        return tools::ToolResult::Error {
                            message: "no available slots for planner agent".to_string(),
                        }
                    }
                };

                let effective_tool = self.default_tool.clone();
                let cmd = self.tool_cmd(&effective_tool).to_string();
                let full_prompt = format!(
                    "Your callsign is {}. {}",
                    callsign_for_prompt, planner_prompt
                );

                match dispatch_slot(
                    slot_idx,
                    &effective_tool,
                    &cmd,
                    self.pane_rows,
                    self.pane_cols,
                    Some(&target_repo),
                    self.scrollback_lines,
                    repo_name_from_path(&target_repo),
                    &target_repo,
                    Some(&full_prompt),
                    &callsign_for_prompt,
                    &self.merge_strategy,
                    None,
                ) {
                    Some(slot) => {
                        self.slots[slot_idx] = Some(slot);
                    }
                    None => {
                        return tools::ToolResult::Error {
                            message: "failed to spawn planner agent PTY".to_string(),
                        }
                    }
                }

                {
                    let slot = self.slots[slot_idx].as_mut().unwrap();
                    slot.task_id = Some(format!("strike-team-planner:{}", st_name));
                    *slot.last_output_at.lock().unwrap() = Instant::now();
                    slot.idle = false;
                }

                let phase_agent_callsign = self.slots[slot_idx]
                    .as_ref()
                    .unwrap()
                    .display_name()
                    .to_string();

                // Initialize strike team state.
                let task_file_path = format!("{}/.dispatch/tasks-{}.md", target_repo, st_name);
                self.strike_teams.push(strike_team::StrikeTeamState {
                    name: st_name.clone(),
                    source_file: resolved_source_file.clone(),
                    repo: target_repo.clone(),
                    phase: strike_team::StrikeTeamPhase::Planning,
                    tasks: Vec::new(),
                    task_file_path,
                    phase_agent_callsign: Some(phase_agent_callsign.clone()),
                });

                self.push_orch(OrchestratorEventKind::Dispatched {
                    agent: phase_agent_callsign.clone(),
                    slot: slot_idx + 1,
                    tool: effective_tool.clone(),
                });
                self.push_ticker(format!("STRIKE TEAM: planning {}...", st_name));
                self.push_chat(
                    "System",
                    &format!(
                        "Strike Team '{}': planner dispatched to slot {}.",
                        st_name,
                        slot_idx + 1
                    ),
                );

                // Sync ws_state.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    st.slots[slot_idx] = Some(ws_server::AgentSlot {
                        callsign: phase_agent_callsign,
                        tool: effective_tool,
                        status: ws_server::AgentStatus::Busy,
                        task: None,
                        repo: Some(repo_name_from_path(&target_repo).to_string()),
                    });
                }
                self.broadcast_agents();

                tools::ToolResult::StrikeTeamAcknowledged {
                    name: st_name,
                    source_file: resolved_source_file,
                    repo: target_repo,
                }
            }

            tools::ToolCall::MessageAgent { agent, text } => {
                let (slot_occupied, callsigns): (Vec<bool>, Vec<Option<String>>) = self
                    .slots
                    .iter()
                    .map(|s| match s {
                        Some(slot) => (true, Some(slot.display_name().to_string())),
                        None => (false, None),
                    })
                    .unzip();

                let idx = match tools::resolve_agent(agent, &slot_occupied, &callsigns) {
                    Some(i) => i,
                    None => {
                        return tools::ToolResult::Error {
                            message: format!("agent '{}' not found", agent),
                        }
                    }
                };

                let slot = self.slots[idx].as_mut().unwrap();
                let agent_name = slot.display_name().to_string();
                if let Some(ref sw) = slot.shared_writer {
                    // Copilot: type char-by-char on a background thread
                    // to avoid blocking the main TUI loop.
                    let w = Arc::clone(sw);
                    let ts = Arc::clone(&slot.last_output_at);
                    let text = text.clone();
                    thread::spawn(move || {
                        crate::pty::type_to_copilot(&w, &text, &ts);
                    });
                } else {
                    let msg = format!("{}\r", text);
                    let _ = slot.writer.write_all(msg.as_bytes());
                    let _ = slot.writer.flush();
                }
                *slot.last_output_at.lock().unwrap() = Instant::now();
                slot.idle = false;
                // Set task_id so idle detection tracks the follow-up work.
                // Without this, the agent stays in a limbo state where idle
                // detection never fires and AGENT_IDLE is never sent.
                if slot.task_id.is_none() {
                    slot.task_id = Some(text.clone());
                }

                self.push_chat("System", &format!("[to {}] {}", agent_name, text));

                // Sync ws_state so the radio shows the agent as busy.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    if let Some(agent) = &mut st.slots[idx] {
                        agent.status = ws_server::AgentStatus::Busy;
                    }
                }
                self.broadcast_agents();

                tools::ToolResult::MessageSent {
                    agent: agent_name,
                    slot: (idx + 1) as u32,
                }
            }
        }
    }

    // ── strike team state machine ────────────────────────────────────────────

    /// Advance the strike team state machine. Called each frame from the main loop.
    pub fn tick_strike_team(&mut self) {
        // Collect indices and phases to avoid borrow conflicts.
        let work: Vec<(usize, strike_team::StrikeTeamPhase)> = self
            .strike_teams
            .iter()
            .enumerate()
            .map(|(i, st)| (i, st.phase.clone()))
            .collect();

        for (st_idx, phase) in &work {
            match phase {
                strike_team::StrikeTeamPhase::Planning => {
                    self.tick_strike_team_planning(*st_idx);
                }
                strike_team::StrikeTeamPhase::Executing => {
                    self.strike_team_dispatch_ready(*st_idx);
                }
                strike_team::StrikeTeamPhase::Verifying => {
                    self.tick_strike_team_verifying(*st_idx);
                }
                strike_team::StrikeTeamPhase::Complete | strike_team::StrikeTeamPhase::Aborted => {
                    // Terminal — will be cleaned up below.
                }
            }
        }

        // Remove completed/aborted strike teams so the vec doesn't grow forever.
        self.strike_teams.retain(|st| {
            st.phase != strike_team::StrikeTeamPhase::Complete
                && st.phase != strike_team::StrikeTeamPhase::Aborted
        });
    }

    fn tick_strike_team_planning(&mut self, st_idx: usize) {
        let cs = match self.strike_teams.get(st_idx) {
            Some(st) => match &st.phase_agent_callsign {
                Some(cs) => cs.clone(),
                None => return,
            },
            None => return,
        };

        // Planner is idle when its slot exists but task_id has been cleared.
        let planner_idle = self.slots.iter().any(|s| {
            s.as_ref().map_or(false, |slot| {
                slot.display_name().eq_ignore_ascii_case(&cs) && slot.task_id.is_none()
            })
        });
        // Planner exited when its slot is gone entirely.
        let planner_gone = !self.slots.iter().any(|s| {
            s.as_ref()
                .map_or(false, |slot| slot.display_name().eq_ignore_ascii_case(&cs))
        });

        if planner_idle || planner_gone {
            // Parse the task file and transition to Executing.
            let st = &mut self.strike_teams[st_idx];
            st.phase_agent_callsign = None;
            let task_file = st.task_file_path.clone();
            match std::fs::read_to_string(&task_file) {
                Ok(contents) => {
                    st.tasks = strike_team::parse_task_file(&contents);
                    let task_count = st.tasks.len();
                    if task_count == 0 {
                        st.phase = strike_team::StrikeTeamPhase::Aborted;
                        let name = st.name.clone();
                        self.push_ticker(format!("STRIKE TEAM: {} aborted — no tasks found", name));
                    } else {
                        st.phase = strike_team::StrikeTeamPhase::Executing;
                        let name = st.name.clone();
                        self.push_ticker(format!("STRIKE TEAM: plan ready, {} tasks", task_count));
                        self.push_chat(
                            "System",
                            &format!(
                                "Strike Team '{}': plan ready with {} tasks.",
                                name, task_count
                            ),
                        );
                    }
                }
                Err(_) => {
                    st.phase = strike_team::StrikeTeamPhase::Aborted;
                    let name = st.name.clone();
                    self.push_ticker(format!(
                        "STRIKE TEAM: {} aborted — task file not found",
                        name
                    ));
                }
            }

            // Terminate the planner agent to free the slot.
            if planner_idle {
                if let Some(idx) = self.slots.iter().position(|s| {
                    s.as_ref()
                        .map_or(false, |slot| slot.display_name().eq_ignore_ascii_case(&cs))
                }) {
                    let callsign = self.slots[idx].as_ref().unwrap().display_name().to_string();
                    crate::pty::terminate_slot(&mut self.slots[idx]);
                    self.push_orch(OrchestratorEventKind::Terminated {
                        agent: callsign,
                        slot: idx + 1,
                    });
                    {
                        let mut wst = self.ws_state.lock().unwrap();
                        wst.slots[idx] = None;
                    }
                    self.broadcast_agents();
                }
            }
        }
    }

    fn tick_strike_team_verifying(&mut self, st_idx: usize) {
        let cs = match self.strike_teams.get(st_idx) {
            Some(st) => match &st.phase_agent_callsign {
                Some(cs) => cs.clone(),
                None => return,
            },
            None => return,
        };

        let verifier_idle = self.slots.iter().any(|s| {
            s.as_ref().map_or(false, |slot| {
                slot.display_name().eq_ignore_ascii_case(&cs) && slot.task_id.is_none()
            })
        });
        let verifier_gone = !self.slots.iter().any(|s| {
            s.as_ref()
                .map_or(false, |slot| slot.display_name().eq_ignore_ascii_case(&cs))
        });

        if verifier_idle || verifier_gone {
            // Terminate the verifier to free the slot.
            if verifier_idle {
                if let Some(idx) = self.slots.iter().position(|s| {
                    s.as_ref()
                        .map_or(false, |slot| slot.display_name().eq_ignore_ascii_case(&cs))
                }) {
                    let callsign = self.slots[idx].as_ref().unwrap().display_name().to_string();
                    crate::pty::terminate_slot(&mut self.slots[idx]);
                    self.push_orch(OrchestratorEventKind::Terminated {
                        agent: callsign,
                        slot: idx + 1,
                    });
                    {
                        let mut wst = self.ws_state.lock().unwrap();
                        wst.slots[idx] = None;
                    }
                    self.broadcast_agents();
                }
            }
            self.strike_team_complete(st_idx);
        }
    }

    /// Dispatch agents for all ready tasks (pending with all deps done).
    /// Runs `git pull --ff-only` first to pick up prior merges.
    fn strike_team_dispatch_ready(&mut self, st_idx: usize) {
        let (repo, ready_ids, task_file_path) = {
            let st = match self.strike_teams.get(st_idx) {
                Some(st) => st,
                None => return,
            };
            let ready = strike_team::ready_tasks(&st.tasks);
            let ids: Vec<String> = ready.iter().map(|t| t.id.clone()).collect();
            (st.repo.clone(), ids, st.task_file_path.clone())
        };

        if ready_ids.is_empty() {
            // Check if all tasks are complete.
            let st = &self.strike_teams[st_idx];
            if strike_team::is_complete(&st.tasks) {
                let summary = strike_team::summary(&st.tasks);
                let name = st.name.clone();
                self.push_ticker(format!(
                    "STRIKE TEAM: tasks complete ({}), dispatching verifier",
                    summary
                ));
                self.push_chat(
                    "System",
                    &format!(
                        "Strike Team '{}': tasks complete ({}), verifying.",
                        name, summary
                    ),
                );
                self.strike_team_dispatch_verifier(st_idx);
            }
            return;
        }

        // git pull --ff-only in repo root to pick up prior merges.
        match Command::new("git")
            .args(["pull", "--ff-only"])
            .current_dir(&repo)
            .output()
        {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                self.push_ticker(format!("STRIKE TEAM: git pull failed — {}", stderr.trim()));
            }
            Err(e) => {
                self.push_ticker(format!("STRIKE TEAM: git pull error — {}", e));
            }
            _ => {}
        }

        for task_id in &ready_ids {
            // Find an available slot.
            let slot_idx = match self.slots.iter().position(|s| s.is_none()) {
                Some(i) => i,
                None => break, // No more slots — wait for next tick.
            };

            let (prompt, title, source_file, st_name) = {
                let st = &self.strike_teams[st_idx];
                let task = st.tasks.iter().find(|t| t.id == *task_id).unwrap();
                (
                    task.prompt.clone(),
                    task.title.clone(),
                    st.source_file.clone(),
                    st.name.clone(),
                )
            };

            let callsign = self
                .next_callsign()
                .unwrap_or_else(|| format!("Agent-{}", slot_idx + 1));
            let effective_tool = self.default_tool.clone();
            let cmd = self.tool_cmd(&effective_tool).to_string();
            let task_num = task_id.strip_prefix('T').unwrap_or(task_id);
            let commit_prefix = format!("{}-t{}", st_name, task_num);
            let full_prompt = format!(
                "Your callsign is {}. COMMIT PREFIX: you MUST prefix ALL git commit messages with \
                 '{}: ' (example: '{}: implement feature X'). \
                 Source document: {} -- read it for full context. {}",
                callsign, commit_prefix, commit_prefix, source_file, prompt
            );

            // In PR mode, strike team task agents only push their branch --
            // the verifier consolidates all branches into one PR later.
            let task_strategy = if self.merge_strategy == "pr" {
                "pr-strike-team"
            } else {
                &self.merge_strategy
            };

            match dispatch_slot(
                slot_idx,
                &effective_tool,
                &cmd,
                self.pane_rows,
                self.pane_cols,
                None,
                self.scrollback_lines,
                repo_name_from_path(&repo),
                &repo,
                Some(&full_prompt),
                &callsign,
                task_strategy,
                Some(&commit_prefix),
            ) {
                Some(slot) => {
                    self.slots[slot_idx] = Some(slot);
                }
                None => continue,
            }

            {
                let slot = self.slots[slot_idx].as_mut().unwrap();
                slot.task_id = Some(format!("strike:{}", task_id));
                *slot.last_output_at.lock().unwrap() = Instant::now();
                slot.idle = false;
            }

            let actual_callsign = self.slots[slot_idx]
                .as_ref()
                .unwrap()
                .display_name()
                .to_string();

            // Update task state: status=active, agent=callsign.
            {
                let st = &mut self.strike_teams[st_idx];
                strike_team::assign_task(&mut st.tasks, task_id, &actual_callsign);
                // Write updated task file.
                let contents = strike_team::write_task_file(&st.tasks);
                let _ = std::fs::write(&task_file_path, &contents);
            }

            self.push_orch(OrchestratorEventKind::Dispatched {
                agent: actual_callsign.clone(),
                slot: slot_idx + 1,
                tool: effective_tool.clone(),
            });
            self.push_ticker(format!("STRIKE TEAM: {} -> {}", task_id, actual_callsign));
            self.push_chat(
                "System",
                &format!(
                    "Strike Team: {} ({}) -> {} (slot {}).",
                    task_id,
                    title,
                    actual_callsign,
                    slot_idx + 1
                ),
            );

            // Sync ws_state.
            {
                let mut wst = self.ws_state.lock().unwrap();
                wst.slots[slot_idx] = Some(ws_server::AgentSlot {
                    callsign: actual_callsign,
                    tool: effective_tool,
                    status: ws_server::AgentStatus::Busy,
                    task: None,
                    repo: Some(repo_name_from_path(&repo).to_string()),
                });
            }
            self.broadcast_agents();
        }
    }

    /// Called when an agent goes idle (10s no output). If the agent is working
    /// on a strike team task, mark the task done, terminate the agent to free
    /// the slot, and write the updated task file.
    pub fn strike_team_on_agent_idle(&mut self, callsign: &str) {
        // Find which strike team owns this agent.
        let st_idx = match self.strike_teams.iter().position(|st| {
            st.phase == strike_team::StrikeTeamPhase::Executing
                && strike_team::task_for_agent(&st.tasks, callsign).is_some()
        }) {
            Some(i) => i,
            None => return,
        };

        // Find the task assigned to this callsign.
        let task_id = match strike_team::task_for_agent(&self.strike_teams[st_idx].tasks, callsign)
        {
            Some(t) => t.id.clone(),
            None => return,
        };

        // Mark task done.
        let st = &mut self.strike_teams[st_idx];
        strike_team::complete_task(&mut st.tasks, &task_id);
        let contents = strike_team::write_task_file(&st.tasks);
        let _ = std::fs::write(&st.task_file_path, &contents);

        let name = st.name.clone();
        self.push_ticker(format!("STRIKE TEAM: {} done ({})", task_id, callsign));
        self.push_chat(
            "System",
            &format!("Strike Team '{}': {} done ({}).", name, task_id, callsign),
        );

        // Terminate the agent to free the slot for the next wave.
        if let Some(idx) = self.slots.iter().position(|s| {
            s.as_ref().map_or(false, |slot| {
                slot.display_name().eq_ignore_ascii_case(callsign)
            })
        }) {
            crate::pty::terminate_slot(&mut self.slots[idx]);
            self.push_orch(OrchestratorEventKind::Terminated {
                agent: callsign.to_string(),
                slot: idx + 1,
            });
            {
                let mut wst = self.ws_state.lock().unwrap();
                wst.slots[idx] = None;
            }
            self.broadcast_agents();
        }
    }

    /// Dispatch a verifier agent after all tasks are done. Transitions to Verifying.
    fn strike_team_dispatch_verifier(&mut self, st_idx: usize) {
        let (repo, source_file, task_file_path, name, consolidation_instructions) = {
            let st = match self.strike_teams.get(st_idx) {
                Some(st) => st,
                None => return,
            };

            // In PR mode, build consolidation instructions listing every
            // successfully-completed task branch for the verifier to merge.
            let consolidation = if self.merge_strategy == "pr" {
                let mut branches = Vec::new();
                let mut merge_cmds = Vec::new();
                let mut cleanup_cmds = Vec::new();
                for t in st.tasks.iter().filter(|t| {
                    t.status == dispatch_core::strike_team::TaskStatus::Done
                        && t.agent.is_some()
                }) {
                    let agent = t.agent.as_ref().unwrap();
                    branches.push(format!(
                        "- `origin/dispatch/{}` ({}: {})",
                        agent, t.id, t.title,
                    ));
                    merge_cmds.push(format!(
                        "git merge origin/dispatch/{} --no-ff -m \"Merge {}: {}\"",
                        agent, t.id, t.title,
                    ));
                    cleanup_cmds.push(format!(
                        "git push origin --delete dispatch/{}",
                        agent,
                    ));
                }

                format!(
                    "\n\n## PR Consolidation\n\n\
                     Before verifying, consolidate all task branches into yours. \
                     The strike team name is: {}\n\n\
                     Task branches:\n{}\n\n\
                     Fetch and merge each one:\n```bash\ngit fetch origin\n{}\n```\n\n\
                     If any merge conflicts occur, resolve them manually and commit.\n\n\
                     After verification and any fixes, clean up remote task branches:\n\
                     ```bash\n{}\n```\n",
                    st.name,
                    branches.join("\n"),
                    merge_cmds.join("\n"),
                    cleanup_cmds.join("\n"),
                )
            } else {
                String::new()
            };

            (
                st.repo.clone(),
                st.source_file.clone(),
                st.task_file_path.clone(),
                st.name.clone(),
                consolidation,
            )
        };

        let slot_idx = match self.slots.iter().position(|s| s.is_none()) {
            Some(i) => i,
            None => {
                // No slot available -- tick will retry next frame.
                return;
            }
        };

        let callsign = self
            .next_callsign()
            .unwrap_or_else(|| "Verifier".to_string());
        let effective_tool = self.default_tool.clone();
        let cmd = self.tool_cmd(&effective_tool).to_string();

        let verifier_prompt = format!(
            "Your callsign is {}. {}",
            callsign,
            render_prompt_template(
                VERIFIER_MD,
                &[
                    ("{source_file}", &source_file),
                    ("{task_file_path}", &task_file_path),
                    ("{consolidation_instructions}", &consolidation_instructions),
                ],
            )
        );

        // In PR mode the verifier uses the special pr-verifier workflow
        // which creates the single consolidated PR.
        let verifier_strategy = if self.merge_strategy == "pr" {
            "pr-verifier"
        } else {
            &self.merge_strategy
        };

        match dispatch_slot(
            slot_idx,
            &effective_tool,
            &cmd,
            self.pane_rows,
            self.pane_cols,
            None,
            self.scrollback_lines,
            repo_name_from_path(&repo),
            &repo,
            Some(&verifier_prompt),
            &callsign,
            verifier_strategy,
            None,
        ) {
            Some(slot) => {
                self.slots[slot_idx] = Some(slot);
            }
            None => return,
        }

        {
            let slot = self.slots[slot_idx].as_mut().unwrap();
            slot.task_id = Some(format!("strike-team-verifier:{}", name));
            *slot.last_output_at.lock().unwrap() = Instant::now();
            slot.idle = false;
        }

        let actual_callsign = self.slots[slot_idx]
            .as_ref()
            .unwrap()
            .display_name()
            .to_string();

        {
            let st = &mut self.strike_teams[st_idx];
            st.phase = strike_team::StrikeTeamPhase::Verifying;
            st.phase_agent_callsign = Some(actual_callsign.clone());
        }

        self.push_orch(OrchestratorEventKind::Dispatched {
            agent: actual_callsign.clone(),
            slot: slot_idx + 1,
            tool: effective_tool.clone(),
        });
        self.push_ticker(format!("STRIKE TEAM: verifier -> {}", actual_callsign));
        self.push_chat(
            "System",
            &format!(
                "Strike Team '{}': verifier dispatched to slot {}.",
                name,
                slot_idx + 1
            ),
        );

        // Sync ws_state.
        {
            let mut wst = self.ws_state.lock().unwrap();
            wst.slots[slot_idx] = Some(ws_server::AgentSlot {
                callsign: actual_callsign,
                tool: effective_tool,
                status: ws_server::AgentStatus::Busy,
                task: None,
                repo: Some(repo_name_from_path(&repo).to_string()),
            });
        }
        self.broadcast_agents();
    }

    /// Called when the verifier agent finishes. Transitions to Complete.
    fn strike_team_complete(&mut self, st_idx: usize) {
        let st = match self.strike_teams.get_mut(st_idx) {
            Some(st) if st.phase == strike_team::StrikeTeamPhase::Verifying => st,
            _ => return,
        };

        let summary = strike_team::summary(&st.tasks);
        let name = st.name.clone();
        st.phase = strike_team::StrikeTeamPhase::Complete;
        st.phase_agent_callsign = None;

        self.push_ticker(format!("STRIKE TEAM: complete ({})", summary));
        self.push_chat(
            "System",
            &format!("Strike Team '{}': complete ({}).", name, summary),
        );
        if let Some(orch) = &mut self.orchestrator {
            orch.send_message(&format!(
                "[D-{}:EVENT] STRIKE_TEAM_COMPLETE name={} result={}",
                orch.nonce(),
                name,
                summary
            ));
        }
    }

    /// Abort all active strike teams. Transitions to Aborted phase so no new
    /// tasks are dispatched. Active agents continue but nothing new is started.
    pub fn abort_strike_team(&mut self) {
        for st in &mut self.strike_teams {
            if st.phase != strike_team::StrikeTeamPhase::Complete
                && st.phase != strike_team::StrikeTeamPhase::Aborted
            {
                st.phase = strike_team::StrikeTeamPhase::Aborted;
                let name = st.name.clone();
                let summary = strike_team::summary(&st.tasks);
                self.ticker_pending
                    .push_back(format!("STRIKE TEAM: {} aborted ({})", name, summary));
                // Note: push_chat/push_ticker require &mut self which we can't use
                // inside the loop, so we use ticker_pending directly above and
                // push chat messages after the loop.
            }
        }
    }

    /// Called when an agent process exits unexpectedly. If the agent was working
    /// on a strike team task, mark the task as failed. If the verifier exits,
    /// complete the strike team anyway (verification is best-effort).
    pub fn strike_team_on_agent_exit(&mut self, slot_idx: usize) {
        let callsign = match &self.slots[slot_idx] {
            Some(s) => s.display_name().to_string(),
            None => return,
        };

        // Find which strike team this agent belongs to.
        // Check verifiers first, then executing tasks.
        let verifier_st_idx = self.strike_teams.iter().position(|st| {
            st.phase == strike_team::StrikeTeamPhase::Verifying
                && st
                    .phase_agent_callsign
                    .as_deref()
                    .map_or(false, |cs| cs.eq_ignore_ascii_case(&callsign))
        });

        if let Some(st_idx) = verifier_st_idx {
            let name = self.strike_teams[st_idx].name.clone();
            self.push_ticker(format!("STRIKE TEAM: verifier exited ({})", callsign));
            self.push_chat(
                "System",
                &format!("Strike Team '{}': verifier exited, completing.", name),
            );
            self.strike_team_complete(st_idx);
            return;
        }

        let executing_st_idx = self.strike_teams.iter().position(|st| {
            st.phase == strike_team::StrikeTeamPhase::Executing
                && strike_team::task_for_agent(&st.tasks, &callsign).is_some()
        });

        if let Some(st_idx) = executing_st_idx {
            let st = &mut self.strike_teams[st_idx];
            let task_id = match strike_team::task_for_agent(&st.tasks, &callsign) {
                Some(t) => t.id.clone(),
                None => return,
            };

            strike_team::fail_task(&mut st.tasks, &task_id);
            let contents = strike_team::write_task_file(&st.tasks);
            let _ = std::fs::write(&st.task_file_path, &contents);

            let name = st.name.clone();
            self.push_ticker(format!("STRIKE TEAM: {} failed ({})", task_id, callsign));
            self.push_chat(
                "System",
                &format!("Strike Team '{}': {} failed ({}).", name, task_id, callsign),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo {
        path: std::path::PathBuf,
    }

    impl TempRepo {
        fn new(label: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("dispatch-{label}-{unique}"));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, relative: &str) {
            let full = self.path.join(relative);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, "test").unwrap();
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn resolve_strike_team_source_defaults_to_repo_spec() {
        let repo = TempRepo::new("spec-default");
        repo.write("docs/SPEC.md");

        let resolved = resolve_strike_team_source_file(&repo.path.to_string_lossy(), "");
        assert_eq!(resolved.as_deref(), Some("docs/SPEC.md"));
    }

    #[test]
    fn resolve_strike_team_source_canonicalizes_case() {
        let repo = TempRepo::new("spec-case");
        repo.write("docs/SPEC.md");

        let resolved =
            resolve_strike_team_source_file(&repo.path.to_string_lossy(), "docs/spec.md");
        assert_eq!(resolved.as_deref(), Some("docs/SPEC.md"));
    }

    #[test]
    fn resolve_strike_team_source_accepts_architecture_alias() {
        let repo = TempRepo::new("arch-alias");
        repo.write("docs/ARCHITECTURE.md");

        let resolved =
            resolve_strike_team_source_file(&repo.path.to_string_lossy(), "architecture");
        assert_eq!(resolved.as_deref(), Some("docs/ARCHITECTURE.md"));
    }
}
