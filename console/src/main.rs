// dispatch: Console TUI for Dispatch
//
// dispatch-e0k.1: PTY with claude via portable-pty + vt100 + ratatui
// dispatch-e0k.2: keyboard input forwarding to PTY
// dispatch-e0k.3: bd create integration from Rust
// dispatch-bgz.1: quad-pane TUI layout with multi-page support
// dispatch-bgz.2: embedded terminal per slot (portable-pty + vt100)
// dispatch-bgz.3: agent naming (NATO phonetic alphabet, slot-bound, custom rename)
// dispatch-bgz.4: modal input model (command mode / input mode)
// dispatch-bgz.5: full command mode keybindings
// dispatch-bgz.6: PTY management (dispatch, terminate, resize, prompt injection)
// dispatch-bgz.7: WebSocket server with PSK authentication
// dispatch-bgz.8: WebSocket protocol (ws_server + protocol modules)
// dispatch-bgz.9: beads task lifecycle (create, assign, close, reopen)
// dispatch-bgz.10: pane info strip and header bar
// dispatch-bgz.11: standby pane (empty slot display + queued task list)
// dispatch-bgz.12: config file and CLI subcommands
// dispatch-ami: LED-style scrolling ticker line between header and panes
// dispatch-1lc.1: task queuing — auto-dispatch unaddressed prompts from radio
// dispatch-1lc.2: idle agent pickup — idle prompt detection, inactivity timeout, auto task pickup
// dispatch-xje: git worktree-per-task isolation
// dispatch-1lc.3: task dependencies — -> arrow syntax in .dispatch/tasks.md, file-based task ops
// dispatch-1lc.4: task list overlay — full-screen plan view with status groups and agent assignments
// dispatch-1lc.3: task dependencies — -> arrow syntax in .dispatch/tasks.md, dependency-aware dispatch
// dispatch-ct2.4: terminal scrollback in panes — PgUp/PgDn in command mode, configurable buffer
// dispatch-sa1: multi-repo support — detect non-repo parent, scan children for git repos
// dispatch-ct2.8: prompt history — log voice/keyboard prompts to file, browsable history overlay
//
// Layout:
//   Header bar  : DISPATCH title, radio state, PSK, agent count, PAGE X/Y, clock
//   Ticker bar  : single-line LED marquee scrolling right-to-left (dispatch-ami)
//   Quad pane   : 2x2 grid; each pane has info strip + terminal area
//   Footer bar  : mode indicator, target, navigation hints
//
// Pages: slots 1-4 on page 1, 5-8 on page 2, etc. (max 26 slots / 7 pages).
// All PTYs run regardless of visible page. Each slot owns its own PTY.

mod config;
mod mdns;
mod ws_server;

use dispatch_core::{orchestrator, tasks, tools};

use clap::{Parser, Subcommand};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::{self, Read, Write},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use chrono::Local;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "dispatch", about = "Dispatch console TUI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new pre-shared key and save it to config
    RegeneratePsk,
    /// Print the current pre-shared key
    ShowPsk,
    /// Print the config file path
    Config,
}

// ── constants ────────────────────────────────────────────────────────────────

/// Total number of agent slots (maps to NATO alphabet A-Z).
const MAX_SLOTS: usize = 26;
/// Slots per page (2×2 grid).
const SLOTS_PER_PAGE: usize = 4;
const TASK_POLL_SECS: u64 = 5;

const NATO: &[&str] = &[
    "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT", "GOLF", "HOTEL", "INDIA", "JULIET",
    "KILO", "LIMA", "MIKE", "NOVEMBER", "OSCAR", "PAPA", "QUEBEC", "ROMEO", "SIERRA", "TANGO",
    "UNIFORM", "VICTOR", "WHISKEY", "X-RAY", "YANKEE", "ZULU",
];

// Reserved words that cannot be used as custom callsigns (dispatch-bgz.3).
const RESERVED_CALLSIGNS: &[&str] = &[
    "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT", "GOLF", "HOTEL", "INDIA", "JULIET",
    "KILO", "LIMA", "MIKE", "NOVEMBER", "OSCAR", "PAPA", "QUEBEC", "ROMEO", "SIERRA", "TANGO",
    "UNIFORM", "VICTOR", "WHISKEY", "X-RAY", "YANKEE", "ZULU",
    "STANDBY", "DISPATCH", "IDLE",
];

// ── types ─────────────────────────────────────────────────────────────────────

/// Input mode for the console (dispatch-bgz.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Command,
    Input,
}

/// Which view is shown in the main area (dispatch-6nm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// Default 2x2 agent grid.
    Agents,
    /// Orchestrator conversation log.
    Orchestrator,
}

/// A timestamped orchestrator event for the log view (dispatch-6nm).
#[derive(Clone)]
struct OrchestratorEvent {
    time: String,
    kind: OrchestratorEventKind,
}

#[derive(Clone)]
#[allow(dead_code)]
enum OrchestratorEventKind {
    /// Voice transcript received from radio.
    VoiceTranscript { text: String },
    /// Task created in tasks.md.
    TaskCreated { id: String, title: String },
    /// Task assigned to an agent slot.
    TaskAssigned { id: String, agent: String, slot: usize },
    /// Task completed (idle detected or timeout).
    TaskComplete { id: String, agent: String },
    /// Worktree merged successfully.
    Merged { id: String },
    /// Merge conflict.
    MergeConflict { id: String },
    /// Agent dispatched into a slot.
    Dispatched { agent: String, slot: usize, tool: String },
    /// Agent terminated.
    Terminated { agent: String, slot: usize },
    /// All agents busy, task queued.
    Queued { id: String },
    /// Orchestrator reasoning text (dispatch-h62).
    OrchestratorText { text: String },
    /// Tool call issued by orchestrator (dispatch-h62).
    ToolCallIssued { name: String },
    /// Tool result sent back to orchestrator (dispatch-h62).
    ToolResultSent { name: String, success: bool },
}

/// Workspace mode: single repo or multi-repo parent directory (dispatch-sa1).
#[derive(Debug, Clone)]
enum Workspace {
    /// Launched inside a git repo — original single-repo behavior.
    SingleRepo { root: String },
    /// Launched from a non-repo directory — children contain git repos.
    MultiRepo { parent: String, repos: Vec<String> },
}

/// Active overlay (dispatch-sa1, dispatch-bgz.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Overlay {
    None,
    Help,
    TaskList,
    ConnectionInfo,
    ConfirmQuit,
    ConfirmTerminate,
    DispatchSlot,
    Rename,
    RepoSelect,      // dispatch-sa1: pick which repo to dispatch into
    PromptHistory,    // dispatch-ct2.8: browsable prompt history
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum RadioState {
    Connected,
    Disconnected,
}

/// Per-slot PTY and display state (dispatch-bgz.2).
/// Not Send — only used on the main thread.
struct SlotState {
    callsign: String,            // NATO default (slot-bound)
    custom_name: Option<String>, // user rename (dispatch-bgz.3)
    tool: String,
    task_id: Option<String>,
    repo_name: String,             // short repo dir name for grid header (dispatch-2dc)
    repo_root: String,             // absolute repo root for this slot (dispatch-sa1)
    dispatch_time: Instant,
    dispatch_wall_str: String,
    // PTY
    screen: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child_exited: Arc<AtomicBool>,
    child_pid: Option<u32>,
    // Keep master alive for resize (dispatch-bgz.6)
    master: Box<dyn portable_pty::MasterPty>,
    // Task completion detection (dispatch-1lc.2)
    last_output_at: Instant,     // when screen content last changed
    last_screen_hash: u64,       // hash of screen to detect changes
    idle_since: Option<Instant>, // when idle prompt was first seen (for 500ms debounce)
    // Scrollback (dispatch-ct2.4): lines scrolled back from bottom
    scroll_offset: usize,
}

impl SlotState {
    fn display_name(&self) -> &str {
        self.custom_name.as_deref().unwrap_or(&self.callsign)
    }
}

/// A task ready to be dispatched (dispatch-bgz.11).
#[derive(Clone)]
struct QueuedTask {
    id: String,
    title: String,
}

/// A task entry for the task list overlay (dispatch-1lc.4).
#[derive(Clone)]
struct TaskEntry {
    id: String,
    title: String,
    status: String,        // "open", "in_progress", "closed"
    agent: Option<String>, // agent display name if currently in a slot
    deps: Vec<String>,     // dependency IDs from -> arrows (dispatch-1lc.3)
}

/// A recorded prompt for the history log (dispatch-ct2.8).
#[derive(Clone)]
struct PromptEntry {
    time: String,
    source: PromptSource,
    target: String,
    text: String,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum PromptSource {
    Voice,
    Keyboard,
}

struct App {
    slots: [Option<SlotState>; MAX_SLOTS],
    current_page: usize,
    /// 0-indexed into the current page's 4 visible slots.
    target: usize,
    mode: Mode,
    esc_exit_time: Option<Instant>,
    radio_state: RadioState,
    psk: String,
    port: u16,
    psk_expanded: bool,
    overlay: Overlay,
    /// Shared input buffer for DispatchSlot and Rename overlays.
    input_buf: String,
    queued_tasks: Vec<QueuedTask>,
    ws_state: ws_server::SharedState,
    pane_rows: u16,
    pane_cols: u16,
    tools: std::collections::HashMap<String, String>,
    completion_timeout: Duration,
    // Ticker (dispatch-ami): LED-style scrolling marquee
    ticker_queue: std::collections::VecDeque<String>,
    ticker_current: String,
    ticker_offset: usize,
    ticker_frame_counter: u8,
    /// Task IDs with unresolved merge conflicts (dispatch-xje).
    conflict_tasks: Vec<String>,
    /// Absolute path to the target repo root (dispatch-xje).
    /// In single-repo mode: the git repo root. In multi-repo mode: the parent directory.
    repo_root: String,
    /// Workspace mode: single-repo or multi-repo (dispatch-sa1).
    workspace: Workspace,
    /// Currently highlighted repo in the RepoSelect overlay (dispatch-sa1).
    repo_select_idx: usize,
    // Task list overlay cache (dispatch-1lc.4): loaded when overlay opens
    task_list_data: Vec<TaskEntry>,
    // Scrollback config (dispatch-ct2.4)
    scrollback_lines: u32,
    // Orchestrator log view (dispatch-6nm)
    view_mode: ViewMode,
    orch_log: Vec<OrchestratorEvent>,
    orch_scroll: usize, // scroll offset from bottom
    // TLS cert fingerprint for QR pairing (dispatch-ct2.6)
    tls_fingerprint: String,
    // Prompt history and logging (dispatch-ct2.8)
    prompt_history: Vec<PromptEntry>,
    input_line_buf: String,       // shadow buffer tracking keyboard input in input mode
    history_scroll: usize,        // selected index in the prompt history overlay
    // Persistent LLM orchestrator (dispatch-h62)
    orchestrator: Option<orchestrator::Orchestrator>,
    // Broadcast channel for pushing chat messages to radio clients (dispatch-chat)
    chat_tx: tokio::sync::broadcast::Sender<String>,
}

impl App {
    fn new(
        psk: String,
        port: u16,
        ws_state: ws_server::SharedState,
        pane_rows: u16,
        pane_cols: u16,
        tools: std::collections::HashMap<String, String>,
        completion_timeout: Duration,
        repo_root: String,
        workspace: Workspace,
        scrollback_lines: u32,
        tls_fingerprint: String,
        chat_tx: tokio::sync::broadcast::Sender<String>,
    ) -> Self {
        App {
            slots: std::array::from_fn(|_| None),
            current_page: 0,
            target: 0,
            mode: Mode::Command,
            esc_exit_time: None,
            radio_state: RadioState::Disconnected,
            psk,
            port,
            psk_expanded: false,
            overlay: Overlay::None,
            input_buf: String::new(),
            queued_tasks: Vec::new(),
            ws_state,
            pane_rows,
            pane_cols,
            tools,
            completion_timeout,
            ticker_queue: std::collections::VecDeque::new(),
            ticker_current: String::new(),
            ticker_offset: 0,
            ticker_frame_counter: 0,
            conflict_tasks: Vec::new(),
            repo_root,
            workspace,
            repo_select_idx: 0,
            task_list_data: Vec::new(),
            scrollback_lines,
            view_mode: ViewMode::Agents,
            orch_log: Vec::new(),
            orch_scroll: 0,
            tls_fingerprint,
            prompt_history: Vec::new(),
            input_line_buf: String::new(),
            history_scroll: 0,
            orchestrator: None,
            chat_tx,
        }
    }

    /// Push an event to the orchestrator log (dispatch-6nm).
    fn push_orch(&mut self, kind: OrchestratorEventKind) {
        let time = Local::now().format("%H:%M:%S").to_string();
        self.orch_log.push(OrchestratorEvent { time, kind });
        // Cap at 500 entries to bound memory.
        if self.orch_log.len() > 500 {
            self.orch_log.remove(0);
            self.orch_scroll = self.orch_scroll.saturating_sub(1);
        }
    }

    /// Push a chat message to all connected radio clients (dispatch-chat).
    fn push_chat(&self, sender: &str, text: &str) {
        let msg = dispatch_core::protocol::OutboundMsg::Chat {
            sender: sender.to_string(),
            text: text.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = self.chat_tx.send(json);
        }
    }

    /// Record a prompt to in-memory history and append to the log file (dispatch-ct2.8).
    fn log_prompt(&mut self, source: PromptSource, target: &str, text: &str) {
        let time = Local::now().format("%H:%M:%S").to_string();
        let entry = PromptEntry {
            time: time.clone(),
            source,
            target: target.to_string(),
            text: text.to_string(),
        };
        self.prompt_history.push(entry);

        // Append to .dispatch/prompt_history.log
        let repo = self.default_repo_root().to_string();
        let dispatch_dir = format!("{}/.dispatch", repo);
        let _ = std::fs::create_dir_all(&dispatch_dir);
        let log_path = format!("{}/prompt_history.log", dispatch_dir);
        let label = match source {
            PromptSource::Voice => "VOICE",
            PromptSource::Keyboard => "KEYBOARD",
        };
        let line = format!("[{}] {} -> {}: \"{}\"\n", time, label, target, text);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    }

    fn global_idx(&self, local_idx: usize) -> usize {
        self.current_page * SLOTS_PER_PAGE + local_idx
    }

    fn target_global(&self) -> usize {
        self.global_idx(self.target)
    }

    fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Total pages needed: enough to show all active slots plus at least one standby.
    fn total_pages(&self) -> usize {
        let last_active = self
            .slots
            .iter()
            .rposition(|s| s.is_some())
            .map(|i| i + 1)
            .unwrap_or(0);
        let needed = (last_active + SLOTS_PER_PAGE).max(SLOTS_PER_PAGE);
        ((needed + SLOTS_PER_PAGE - 1) / SLOTS_PER_PAGE).min(MAX_SLOTS / SLOTS_PER_PAGE + 1)
    }

    fn psk_display(&self) -> String {
        if self.psk_expanded {
            self.psk.clone()
        } else if self.psk.len() >= 4 {
            format!("{}...", &self.psk[..4])
        } else {
            "****".to_string()
        }
    }

    /// True if `global_idx` is the last empty slot on the last page (dispatch-bgz.11).
    fn is_last_standby(&self, global_idx: usize) -> bool {
        if self.slots[global_idx].is_some() {
            return false;
        }
        let total = self.total_pages();
        let page = global_idx / SLOTS_PER_PAGE;
        if page != total - 1 {
            return false;
        }
        let local = global_idx % SLOTS_PER_PAGE;
        for i in (local + 1)..SLOTS_PER_PAGE {
            let g = page * SLOTS_PER_PAGE + i;
            if g < MAX_SLOTS && self.slots[g].is_none() {
                return false;
            }
        }
        true
    }

    fn tool_cmd(&self, tool_key: &str) -> &str {
        self.tools
            .get(tool_key)
            .map(|s| s.as_str())
            .unwrap_or("claude")
    }

    /// Whether we're in multi-repo mode (dispatch-sa1).
    fn is_multi_repo(&self) -> bool {
        matches!(self.workspace, Workspace::MultiRepo { .. })
    }

    /// Get the list of repos (dispatch-sa1). Single-repo returns a one-element vec.
    fn repo_list(&self) -> Vec<&str> {
        match &self.workspace {
            Workspace::SingleRepo { root } => vec![root.as_str()],
            Workspace::MultiRepo { repos, .. } => repos.iter().map(|s| s.as_str()).collect(),
        }
    }

    /// Default repo root: first repo in list (dispatch-sa1).
    fn default_repo_root(&self) -> &str {
        match &self.workspace {
            Workspace::SingleRepo { root } => root.as_str(),
            Workspace::MultiRepo { repos, .. } => repos.first().map(|s| s.as_str()).unwrap_or("."),
        }
    }

    /// Re-scan child directories for git repos in multi-repo mode (dispatch-sa1).
    fn rescan_repos(&mut self) {
        if let Workspace::MultiRepo { parent, repos } = &mut self.workspace {
            *repos = scan_child_repos(parent);
        }
    }

    /// Queue a message on the ticker (dispatch-ami).
    fn push_ticker(&mut self, msg: impl Into<String>) {
        self.ticker_queue.push_back(msg.into());
    }

    /// Advance the ticker state by one frame (dispatch-ami).
    /// Call once per render loop iteration (~16ms). Scrolls one character every 3 frames (~50ms).
    fn tick_ticker(&mut self) {
        self.ticker_frame_counter = self.ticker_frame_counter.wrapping_add(1);
        let advance = self.ticker_frame_counter % 3 == 0;

        if self.ticker_current.is_empty() {
            // Load next message from queue if available.
            if let Some(msg) = self.ticker_queue.pop_front() {
                self.ticker_current = msg;
                self.ticker_offset = 0;
                self.ticker_frame_counter = 0;
            }
            return;
        }

        if advance {
            // Count display characters (not bytes) for offset tracking.
            let char_len = self.ticker_current.chars().count();
            self.ticker_offset += 1;
            // Message is fully scrolled off when offset > char_len + display_width.
            // Use 200 as a conservative maximum terminal width estimate.
            if self.ticker_offset > char_len + 200 {
                self.ticker_current = String::new();
                self.ticker_offset = 0;
                // Load next message immediately if queued.
                if let Some(msg) = self.ticker_queue.pop_front() {
                    self.ticker_current = msg;
                    self.ticker_frame_counter = 0;
                }
            }
        }
    }

    /// Build the visible ticker string for a display width (dispatch-ami).
    /// The message scrolls right-to-left: starts fully off the right edge, moves left.
    fn ticker_display(&self, width: usize) -> String {
        if self.ticker_current.is_empty() {
            return " ".repeat(width);
        }
        let chars: Vec<char> = self.ticker_current.chars().collect();
        // Total virtual width: display area + message length (message starts off right edge).
        // offset 0 = message starts just off-screen to the right.
        // offset N = message has moved N chars to the left.
        let virtual_start = width as isize - self.ticker_offset as isize;
        let mut line = vec![' '; width];
        for (i, &ch) in chars.iter().enumerate() {
            let pos = virtual_start + i as isize;
            if pos >= 0 && (pos as usize) < width {
                line[pos as usize] = ch;
            }
        }
        line.into_iter().collect()
    }

    // ── orchestrator tool execution (dispatch-x94) ──────────────────────────

    /// Execute a tool call from the orchestrator agent. Returns the result.
    pub fn execute_tool(&mut self, call: &tools::ToolCall) -> tools::ToolResult {
        match call {
            tools::ToolCall::Dispatch { repo: _, prompt } => {
                // Find an idle slot (has PTY but no task) or an empty slot.
                let slot_idx = self.slots.iter().enumerate().find_map(|(i, s)| {
                    match s {
                        Some(slot) if slot.task_id.is_none() => Some(i),
                        _ => None,
                    }
                }).or_else(|| {
                    self.slots.iter().position(|s| s.is_none())
                });

                let slot_idx = match slot_idx {
                    Some(i) => i,
                    None => return tools::ToolResult::Error {
                        message: "no available slots".to_string(),
                    },
                };

                let target_repo = self.default_repo_root().to_string();

                // Create task in tasks.md.
                let task_id = match create_task_in_file(&target_repo, prompt) {
                    Some(id) => id,
                    None => return tools::ToolResult::Error {
                        message: "failed to create task".to_string(),
                    },
                };

                // Determine callsign before spawn so it can be included in the prompt.
                let callsign_for_prompt = dispatch_core::protocol::default_callsign((slot_idx + 1) as u32).to_string();
                let full_prompt = format!("Your callsign is {}. Your task ID is {}. {}", callsign_for_prompt, task_id, prompt);

                // Spawn PTY if slot is empty. Agent creates its own worktree (dispatch-bka).
                if self.slots[slot_idx].is_none() {
                    let cmd = self.tool_cmd("claude-code").to_string();
                    match dispatch_slot(
                        slot_idx, "claude-code", &cmd, self.pane_rows, self.pane_cols,
                        None, self.scrollback_lines,
                        repo_name_from_path(&target_repo), &target_repo,
                        Some(&full_prompt),
                    ) {
                        Some(slot) => { self.slots[slot_idx] = Some(slot); }
                        None => return tools::ToolResult::Error {
                            message: "failed to spawn agent PTY".to_string(),
                        },
                    }
                }

                // Assign task to slot.
                let callsign = {
                    let slot = self.slots[slot_idx].as_mut().unwrap();
                    update_task_in_file(&target_repo, &task_id, '~', Some(&slot.callsign));
                    slot.task_id = Some(task_id.clone());
                    slot.last_output_at = Instant::now();
                    slot.display_name().to_string()
                };

                self.push_orch(OrchestratorEventKind::TaskAssigned {
                    id: task_id.clone(), agent: callsign.clone(), slot: slot_idx + 1,
                });
                self.push_ticker(format!(
                    "DISPATCH: {} -> {} (slot {})", task_id, callsign, slot_idx + 1
                ));
                self.push_chat("Dispatcher", &format!("Dispatched agent {}.", callsign));

                // Sync ws_state.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    st.slots[slot_idx] = Some(ws_server::AgentSlot {
                        callsign: callsign.clone(),
                        tool: "claude-code".to_string(),
                        status: ws_server::AgentStatus::Busy,
                        task: Some(task_id.clone()),
                        repo: Some(repo_name_from_path(&target_repo).to_string()),
                    });
                }

                tools::ToolResult::Dispatched {
                    slot: (slot_idx + 1) as u32,
                    callsign,
                    task_id,
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
                let slot_repo = self.slots[idx].as_ref().unwrap().repo_root.clone();
                let task_id = terminate_slot(&mut self.slots[idx]);

                // Reopen task if assigned.
                if let Some(ref id) = task_id {
                    update_task_in_file(&slot_repo, id, ' ', None);
                }

                self.push_orch(OrchestratorEventKind::Terminated {
                    agent: callsign.clone(), slot: idx + 1,
                });
                self.push_ticker(format!("TERMINATED: {} (slot {})", callsign, idx + 1));
                self.push_chat("Dispatcher", &format!("Terminated agent {}.", callsign));

                // Sync ws_state.
                {
                    let mut st = self.ws_state.lock().unwrap();
                    st.slots[idx] = None;
                    if st.target == Some((idx + 1) as u32) {
                        st.target = None;
                    }
                }

                tools::ToolResult::Terminated {
                    slot: (idx + 1) as u32,
                    callsign,
                }
            }

            // dispatch-bka: agents now merge their own branches, so this
            // validates the merge actually happened before acknowledging.
            tools::ToolCall::Merge { task_id } => {
                let target_repo = self.default_repo_root().to_string();
                let branch = format!("task/{}", task_id);

                // Check if the task branch still exists (agent should have
                // deleted it after merging).
                let branch_exists = Command::new("git")
                    .args(["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
                    .current_dir(&target_repo)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);

                if branch_exists {
                    // Branch still exists — agent did not merge and clean up.
                    // Check if it has any commits ahead of HEAD (i.e. actual work).
                    let has_commits = Command::new("git")
                        .args(["log", "--oneline", &format!("HEAD..{}", branch)])
                        .current_dir(&target_repo)
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false);

                    let message = if has_commits {
                        format!("task/{} branch exists with unmerged commits — agent did not merge into main", task_id)
                    } else {
                        format!("task/{} branch exists but has no commits — agent did not make any changes", task_id)
                    };

                    self.push_ticker(format!("MERGE FAILED: task/{}", task_id));
                    self.push_chat("Dispatcher", &message);
                    return tools::ToolResult::Merged {
                        task_id: task_id.clone(),
                        success: false,
                        message,
                    };
                }

                // Branch doesn't exist — check if main has a merge commit for
                // this task (evidence the agent actually merged).
                let has_merge_commit = Command::new("git")
                    .args(["log", "--oneline", "--grep", &format!("task/{}", task_id), "-1"])
                    .current_dir(&target_repo)
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);

                if !has_merge_commit {
                    let message = format!(
                        "task/{} branch does not exist and no merge commit found — agent did not make any changes",
                        task_id
                    );
                    self.push_ticker(format!("MERGE FAILED: task/{}", task_id));
                    self.push_chat("Dispatcher", &message);
                    return tools::ToolResult::Merged {
                        task_id: task_id.clone(),
                        success: false,
                        message,
                    };
                }

                // Merge verified — update task and report success.
                update_task_in_file(&target_repo, task_id, 'x', None);
                self.push_orch(OrchestratorEventKind::Merged { id: task_id.clone() });
                self.push_ticker(format!("MERGED: task/{}", task_id));
                self.push_chat("Dispatcher", &format!("task/{} merged.", task_id));
                tools::ToolResult::Merged {
                    task_id: task_id.clone(),
                    success: true,
                    message: format!("task/{} merged by agent", task_id),
                }
            }

            tools::ToolCall::ListAgents => {
                let agents: Vec<tools::AgentInfo> = self.slots.iter().enumerate()
                    .filter_map(|(i, s)| {
                        s.as_ref().map(|slot| tools::AgentInfo {
                            slot: (i + 1) as u32,
                            callsign: slot.display_name().to_string(),
                            tool: slot.tool.clone(),
                            status: if slot.task_id.is_some() { "busy".to_string() } else { "idle".to_string() },
                            task: slot.task_id.clone(),
                            repo: Some(slot.repo_name.clone()),
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
                slot.last_output_at = Instant::now();

                self.push_chat("Dispatcher", &format!("Message to {}: {}", agent_name, text));

                tools::ToolResult::MessageSent {
                    agent: agent_name,
                    slot: (idx + 1) as u32,
                }
            }
        }
    }
}

/// Extract the short directory name from a repo root path (dispatch-2dc).
fn repo_name_from_path(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// Scan immediate children of `parent` for git repos. Returns sorted list of
/// absolute paths to directories that contain a `.git` entry (dispatch-sa1).
fn scan_child_repos(parent: &str) -> Vec<String> {
    let mut repos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join(".git").exists() {
                if let Some(s) = path.to_str() {
                    repos.push(s.to_string());
                }
            }
        }
    }
    repos.sort();
    repos
}

// ── PTY helpers (dispatch-bgz.2, dispatch-bgz.6) ──────────────────────────────

/// Open a PTY and spawn a process. Returns a SlotState on success.
/// `cwd` sets the working directory for the PTY (dispatch-xje: worktree path).
/// `initial_prompt` is passed as a CLI argument so the agent starts working immediately.
fn dispatch_slot(
    global_idx: usize,
    tool_key: &str,
    tool_cmd: &str,
    pane_rows: u16,
    pane_cols: u16,
    cwd: Option<&str>,
    scrollback_lines: u32,
    repo_name: &str,
    repo_root: &str,
    initial_prompt: Option<&str>,
) -> Option<SlotState> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: pane_rows,
            cols: pane_cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;

    let parts: Vec<&str> = tool_cmd.split_whitespace().collect();
    let mut cmd = if parts.is_empty() {
        CommandBuilder::new("claude")
    } else {
        let mut c = CommandBuilder::new(parts[0]);
        for arg in &parts[1..] {
            c.arg(arg);
        }
        c
    };
    // Inject agent instructions from docs/AGENTS.md as system prompt.
    let agents_md_path = format!("{}/docs/AGENTS.md", repo_root);
    if let Ok(instructions) = std::fs::read_to_string(&agents_md_path) {
        cmd.arg("--system-prompt");
        cmd.arg(&instructions);
    }
    cmd.arg("--dangerously-skip-permissions");
    if let Some(prompt) = initial_prompt {
        cmd.arg(prompt);
    }
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .or_else(|_| {
            let shell = if cfg!(windows) { "cmd" } else { "bash" };
            pair.slave.spawn_command(CommandBuilder::new(shell))
        })
        .ok()?;

    let child_pid = child.process_id();

    let screen = Arc::new(Mutex::new(vt100::Parser::new(pane_rows, pane_cols, (scrollback_lines as usize).min(10_000))));
    let screen_w = Arc::clone(&screen);
    let child_exited = Arc::new(AtomicBool::new(false));
    let child_exited_w = Arc::clone(&child_exited);
    let mut pty_reader = pair.master.try_clone_reader().ok()?;

    thread::spawn(move || {
        let mut child = child;
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    screen_w.lock().unwrap().process(&buf[..n]);
                }
            }
        }
        let _ = child.wait();
        child_exited_w.store(true, Ordering::Relaxed);
    });

    let writer = pair.master.take_writer().ok()?;
    let callsign = NATO[global_idx % NATO.len()].to_string();
    let wall = Local::now().format("%H:%M").to_string();

    let now = Instant::now();
    Some(SlotState {
        callsign,
        custom_name: None,
        tool: tool_key.to_string(),
        task_id: None,
        repo_name: repo_name.to_string(),
        repo_root: repo_root.to_string(),
        dispatch_time: now,
        dispatch_wall_str: wall,
        screen,
        writer,
        child_exited,
        child_pid,
        master: pair.master,
        last_output_at: now,
        last_screen_hash: 0,
        idle_since: None,
        scroll_offset: 0,
    })
}

/// Kill a child process by PID (dispatch-bgz.6).
fn kill_child_pid(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();
    }
}

/// Terminate a slot: kill child, clear slot, return task_id for beads reopen.
fn terminate_slot(slot: &mut Option<SlotState>) -> Option<String> {
    if let Some(s) = slot.as_ref() {
        if let Some(pid) = s.child_pid {
            kill_child_pid(pid);
        }
    }
    let task_id = slot.as_ref().and_then(|s| s.task_id.clone());
    *slot = None;
    task_id
}

/// Resize all active PTYs to the new pane size (dispatch-bgz.6).
fn resize_all_slots(slots: &mut [Option<SlotState>; MAX_SLOTS], new_size: PtySize) {
    for slot in slots.iter_mut().flatten() {
        let _ = slot.master.resize(new_size);
        let mut parser = slot.screen.lock().unwrap();
        *parser = vt100::Parser::new(new_size.rows, new_size.cols, 0);
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn format_runtime(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}m{:02}s", s / 60, s % 60)
}

/// Validate a custom callsign (dispatch-bgz.3).
fn is_valid_callsign(name: &str) -> bool {
    if name.is_empty() || name.len() > 20 {
        return false;
    }
    if name.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let upper = name.to_uppercase();
    !RESERVED_CALLSIGNS.contains(&upper.as_str())
}

// ── .dispatch/tasks.md parsing (dispatch-1lc.3) ─────────────────────────────
//
// Task format:
//   - [ ] t1: Description                    (open, no deps)
//   - [ ] t2: Description -> t1              (open, blocked by t1)
//   - [~] t3: Description | agent: Alpha     (in progress)
//   - [x] t4: Description                    (done)
//
// A task is "ready" when status is [ ] and all -> deps are [x].

use tasks::{ParsedTask, parse_task_line};

fn tasks_md_path(repo_root: &str) -> String {
    format!("{}/.dispatch/tasks.md", repo_root)
}

/// Read and parse .dispatch/tasks.md. Returns (all lines, parsed tasks).
fn parse_tasks_md(repo_root: &str) -> (Vec<String>, Vec<ParsedTask>) {
    let path = tasks_md_path(repo_root);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return (vec![], vec![]),
    };
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut tasks = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(task) = parse_task_line(line, idx) {
            tasks.push(task);
        }
    }
    (lines, tasks)
}

/// Reconstruct a task line from parsed components.
fn format_task_line(task: &ParsedTask) -> String {
    let mut line = format!("{}- [{}] {}: {}", task.prefix, task.status, task.id, task.title);
    if !task.deps.is_empty() {
        line.push_str(&format!(" -> {}", task.deps.join(", ")));
    }
    if let Some(agent) = &task.agent {
        line.push_str(&format!(" | agent: {}", agent));
    }
    line
}

/// Fetch tasks ready for dispatch: status [ ] with all -> deps marked [x].
fn fetch_ready_tasks(repo_root: &str) -> Vec<QueuedTask> {
    let (_, tasks) = parse_tasks_md(repo_root);
    let done: std::collections::HashSet<&str> = tasks
        .iter()
        .filter(|t| t.status == 'x')
        .map(|t| t.id.as_str())
        .collect();
    tasks
        .iter()
        .filter(|t| t.status == ' ' && t.deps.iter().all(|d| done.contains(d.as_str())))
        .map(|t| QueuedTask { id: t.id.clone(), title: t.title.clone() })
        .collect()
}

/// Update a task's status and agent annotation in .dispatch/tasks.md.
fn update_task_in_file(repo_root: &str, id: &str, new_status: char, agent: Option<&str>) -> bool {
    let (mut lines, tasks) = parse_tasks_md(repo_root);
    let task = match tasks.iter().find(|t| t.id == id) {
        Some(t) => t,
        None => return false,
    };
    let updated = ParsedTask {
        id: task.id.clone(),
        title: task.title.clone(),
        status: new_status,
        deps: task.deps.clone(),
        agent: agent.map(|s| s.to_string()),
        line_idx: task.line_idx,
        prefix: task.prefix.clone(),
    };
    lines[task.line_idx] = format_task_line(&updated);
    let path = tasks_md_path(repo_root);
    std::fs::write(&path, lines.join("\n") + "\n").is_ok()
}

/// Create a new task entry in .dispatch/tasks.md. Returns the generated ID.
fn create_task_in_file(repo_root: &str, prompt: &str) -> Option<String> {
    let dispatch_dir = format!("{}/.dispatch", repo_root);
    let _ = std::fs::create_dir_all(&dispatch_dir);

    let (lines, tasks) = parse_tasks_md(repo_root);

    // Next sequential ID: find highest top-level t{N} and increment.
    let max_num = tasks
        .iter()
        .filter_map(|t| {
            let num = t.id.strip_prefix('t')?;
            if num.contains('.') { return None; }
            num.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    let new_id = format!("t{}", max_num + 1);
    let new_line = format!("- [ ] {}: {}", new_id, prompt);

    let path = tasks_md_path(repo_root);
    let content = if lines.is_empty() {
        format!("# Tasks\n\n{}\n", new_line)
    } else {
        let mut c = lines.join("\n");
        if !c.ends_with('\n') {
            c.push('\n');
        }
        c.push_str(&new_line);
        c.push('\n');
        c
    };
    std::fs::write(&path, &content).ok()?;
    Some(new_id)
}

/// Fetch all tasks for the task list overlay (dispatch-1lc.3, dispatch-1lc.4).
/// Cross-references with active agent slots to annotate in-progress tasks.
fn fetch_task_list_from_file(
    repo_root: &str,
    slots: &[Option<SlotState>; MAX_SLOTS],
) -> Vec<TaskEntry> {
    let mut slot_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for slot in slots.iter().flatten() {
        if let Some(id) = &slot.task_id {
            slot_map.insert(id.clone(), slot.display_name().to_string());
        }
    }

    let (_, tasks) = parse_tasks_md(repo_root);
    tasks
        .iter()
        .map(|t| {
            let status = match t.status {
                '~' => "in_progress",
                'x' => "closed",
                _ => "open",
            };
            let agent = slot_map.get(&t.id).cloned().or_else(|| t.agent.clone());
            TaskEntry {
                id: t.id.clone(),
                title: t.title.clone(),
                status: status.to_string(),
                agent,
                deps: t.deps.clone(),
            }
        })
        .collect()
}


/// Dispatch ready tasks from .dispatch/tasks.md to available agents. Returns the
/// number dispatched. Called after task completion to fill newly unblocked tasks.
fn dispatch_ready_tasks(app: &mut App) -> usize {
    let ready = fetch_ready_tasks(&app.repo_root);
    let pane_rows = app.pane_rows;
    let pane_cols = app.pane_cols;
    let repo_root = app.repo_root.clone();
    let tool_cmd = app.tool_cmd("claude-code").to_string();
    let scrollback = app.scrollback_lines;
    let short_repo = repo_name_from_path(&repo_root).to_string();
    let mut dispatched = 0;

    for task in ready {
        // Find an idle slot (has PTY but no task) or an empty slot.
        let slot_idx = app.slots.iter().enumerate().find_map(|(i, s)| {
            match s {
                Some(slot) if slot.task_id.is_none() => Some(i),
                _ => None,
            }
        }).or_else(|| {
            app.slots.iter().position(|s| s.is_none())
        });

        let slot_idx = match slot_idx {
            Some(i) => i,
            None => break, // No available slots; remaining tasks stay queued.
        };

        // Spawn PTY if slot is empty. Agent creates its own worktree (dispatch-bka).
        if app.slots[slot_idx].is_none() {
            let prompt = format!("Your task ID is {}. {}", task.id, task.title);
            if let Some(slot) = dispatch_slot(
                slot_idx, "claude-code", &tool_cmd, pane_rows, pane_cols,
                None, scrollback, &short_repo, &repo_root,
                Some(&prompt),
            ) {
                app.slots[slot_idx] = Some(slot);
            } else {
                continue;
            }
        }

        // Assign the task to the slot.
        let display_name = {
            let slot = app.slots[slot_idx].as_mut().unwrap();
            update_task_in_file(&repo_root, &task.id, '~', Some(&slot.callsign));
            slot.task_id = Some(task.id.clone());
            slot.last_output_at = Instant::now();
            slot.display_name().to_string()
        };
        app.push_orch(OrchestratorEventKind::TaskAssigned { id: task.id.clone(), agent: display_name.clone(), slot: slot_idx + 1 });
        app.push_ticker(format!(
            "DISPATCH: {} -> {} (slot {})",
            task.id, display_name, slot_idx + 1
        ));
        dispatched += 1;
    }
    dispatched
}

// ── task completion detection (dispatch-1lc.2) ────────────────────────────────

/// Extract the text content of a single screen row, trimming trailing spaces.
fn screen_row_text(screen: &vt100::Screen, row: u16) -> String {
    let mut s = String::new();
    for col in 0..screen.size().1 {
        if let Some(cell) = screen.cell(row, col) {
            let ch = cell.contents();
            s.push_str(if ch.is_empty() { " " } else { &ch });
        }
    }
    s.trim_end().to_string()
}

/// Hash all screen content to detect changes without storing the full buffer.
fn compute_screen_hash(screen: &vt100::Screen) -> u64 {
    let mut hasher = DefaultHasher::new();
    for row in 0..screen.size().0 {
        screen_row_text(screen, row).hash(&mut hasher);
    }
    hasher.finish()
}

/// Return true if the last non-blank row of the screen matches the idle prompt
/// pattern for the given tool.
///
/// claude-code idle: last non-blank row is exactly ">" or "> "
fn is_idle_prompt(screen: &vt100::Screen, tool: &str) -> bool {
    if tool != "claude-code" {
        return false;
    }
    let (rows, _) = screen.size();
    for r in (0..rows).rev() {
        let text = screen_row_text(screen, r);
        if !text.is_empty() {
            return text == ">" || text == "> ";
        }
    }
    false
}

// dispatch-bka: worktree creation and merging are now handled by agents
// themselves (see docs/AGENTS.md). The console no longer runs blocking git
// worktree/merge commands on the main thread.

fn key_to_pty_bytes(key: &KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                if c.is_ascii_alphabetic() {
                    vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
                } else {
                    let mut buf = [0u8; 4];
                    c.encode_utf8(&mut buf).as_bytes().to_vec()
                }
            } else {
                let mut buf = [0u8; 4];
                c.encode_utf8(&mut buf).as_bytes().to_vec()
            }
        }
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => vec![],
        },
        _ => vec![],
    }
}

fn vt100_color_to_ratatui(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

fn screen_to_lines(screen: &vt100::Screen) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in 0..screen.size().0 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_text = String::new();
        let mut current_style = Style::default();

        for col in 0..screen.size().1 {
            let cell = screen.cell(row, col).unwrap();
            let mut style = Style::default();
            if let Some(fg) = vt100_color_to_ratatui(cell.fgcolor()) {
                style = style.fg(fg);
            }
            if let Some(bg) = vt100_color_to_ratatui(cell.bgcolor()) {
                style = style.bg(bg);
            }
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }

            let ch = cell.contents();
            let ch = if ch.is_empty() { " ".to_string() } else { ch };

            if style == current_style {
                current_text.push_str(&ch);
            } else {
                if !current_text.is_empty() {
                    spans.push(Span::styled(current_text.clone(), current_style));
                    current_text.clear();
                }
                current_text = ch;
                current_style = style;
            }
        }
        if !current_text.is_empty() {
            spans.push(Span::styled(current_text, current_style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

// ── rendering ─────────────────────────────────────────────────────────────────

/// Render the LED-style scrolling ticker line (dispatch-ami).
fn render_ticker(f: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let text = app.ticker_display(width);
    let style = Style::default().fg(Color::Yellow);
    f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let radio_span = match app.radio_state {
        RadioState::Connected => Span::styled(
            "● CONNECTED",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        RadioState::Disconnected => Span::styled(
            "● DISCONNECTED",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    };

    let clock = Local::now().format("%H:%M").to_string();
    // dispatch-sa1: show repo count in multi-repo mode.
    let workspace_indicator = if app.is_multi_repo() {
        format!("  REPOS: {}  ", app.repo_list().len())
    } else {
        String::new()
    };
    // dispatch-h62: orchestrator status indicator.
    let orch_indicator = match &app.orchestrator {
        Some(o) if o.is_alive() => match o.state {
            orchestrator::OrchestratorState::Idle => "  ORCH: IDLE",
            orchestrator::OrchestratorState::Responding => "  ORCH: THINKING",
            orchestrator::OrchestratorState::Dead => "  ORCH: DEAD",
        },
        Some(_) => "  ORCH: DEAD",
        None => "  ORCH: READY",
    };
    let right = format!(
        "PSK: {}  AGENTS: {}/{}{}{}  PAGE {}/{}  {}",
        app.psk_display(),
        app.active_count(),
        MAX_SLOTS,
        workspace_indicator,
        orch_indicator,
        app.current_page + 1,
        app.total_pages(),
        clock,
    );

    // Build left and right portions, pad gap to right-align, and truncate to fit.
    let left_text = " RADIO: ";
    let radio_text = match app.radio_state {
        RadioState::Connected => "● CONNECTED",
        RadioState::Disconnected => "● DISCONNECTED",
    };
    let inner_width = area.width.saturating_sub(2) as usize; // minus border chars
    let left_len = left_text.len() + radio_text.len();
    // Truncate right side if it doesn't fit.
    let max_right = inner_width.saturating_sub(left_len + 1);
    let right_truncated: String = right.chars().take(max_right).collect();
    let used = left_len + right_truncated.len();
    let gap = if inner_width > used { inner_width - used } else { 1 };
    let right_padded = format!("{}{}", " ".repeat(gap), right_truncated);

    let status_line = Line::from(vec![
        Span::raw(left_text),
        radio_span,
        Span::styled(right_padded, Style::default().fg(Color::White)),
    ]);

    let block = Block::default()
        .title(Span::styled(
            " DISPATCH ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(status_line), inner);
}

fn pane_info_strip(global_idx: usize, local_idx: usize, app: &App) -> Text<'static> {
    let slot_num = global_idx + 1;
    let is_target = app.target == local_idx;

    let marker_str = if is_target { "▸ " } else { "  " };
    let marker_style = if is_target {
        match app.mode {
            Mode::Command => Style::default().fg(Color::Cyan),
            Mode::Input => Style::default().fg(Color::Green),
        }
    } else {
        Style::default()
    };

    match &app.slots[global_idx] {
        None => {
            let line1 = Line::from(vec![
                Span::styled(marker_str.to_string(), marker_style),
                Span::styled(
                    format!("[{}] -- STANDBY --", slot_num),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let sep = Line::from(Span::styled(
                "┄".repeat(40),
                Style::default().fg(Color::DarkGray),
            ));
            Text::from(vec![line1, Line::default(), Line::default(), sep])
        }
        Some(agent) => {
            let line1 = Line::from(vec![
                Span::styled(marker_str.to_string(), marker_style),
                Span::styled(
                    format!("[{}] {}", slot_num, agent.display_name()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]);
            let task_span = match &agent.task_id {
                Some(id) => Span::styled(id.clone(), Style::default().fg(Color::Yellow)),
                None => Span::styled("idle", Style::default().fg(Color::DarkGray)),
            };
            let line2 = Line::from(vec![
                Span::styled(
                    format!("  {} | ", agent.tool.to_uppercase()),
                    Style::default().fg(Color::DarkGray),
                ),
                task_span,
                Span::styled(
                    format!(" | {}", agent.repo_name),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);
            let runtime = format_runtime(agent.dispatch_time.elapsed());
            let line3 = Line::from(Span::styled(
                format!("  dispatched {} | {}", agent.dispatch_wall_str, runtime),
                Style::default().fg(Color::DarkGray),
            ));
            let sep = Line::from(Span::styled(
                "┄".repeat(40),
                Style::default().fg(Color::DarkGray),
            ));
            Text::from(vec![line1, line2, line3, sep])
        }
    }
}

fn standby_body(global_idx: usize, app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    if app.is_last_standby(global_idx) {
        lines.push(Line::from(Span::styled(
            format!(" Queued tasks: {}", app.queued_tasks.len()),
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(""));
        for task in app.queued_tasks.iter().take(6) {
            let title_truncated = if task.title.len() > 24 {
                format!("{}...", &task.title[..21])
            } else {
                task.title.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {}  \"{}\"", task.id, title_truncated),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if app.queued_tasks.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (none)",
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            " Dispatch new agent:",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  [c] claude-code",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  [g] gh copilot",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
    }

    lines
}

fn render_pane(
    f: &mut Frame,
    area: Rect,
    local_idx: usize,
    global_idx: usize,
    app: &App,
    vt_lines: Option<Vec<Line<'static>>>,
    scrolled: bool,
) {
    let is_target = app.target == local_idx;
    let border_style = if is_target {
        match app.mode {
            Mode::Command => Style::default().fg(Color::Cyan),
            Mode::Input => Style::default().fg(Color::Green),
        }
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(inner);

    f.render_widget(Paragraph::new(pane_info_strip(global_idx, local_idx, app)), chunks[0]);

    if let Some(lines) = vt_lines {
        f.render_widget(Paragraph::new(Text::from(lines)), chunks[1]);
        // dispatch-ct2.4: show scroll indicator when not at bottom
        if scrolled {
            let indicator = Span::styled(
                " SCROLL ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            );
            let x = chunks[1].right().saturating_sub(9);
            let y = chunks[1].bottom().saturating_sub(1);
            if x >= chunks[1].x && y >= chunks[1].y {
                f.render_widget(Paragraph::new(Line::from(indicator)), Rect::new(x, y, 8, 1));
            }
        }
    } else {
        f.render_widget(Paragraph::new(standby_body(global_idx, app)), chunks[1]);
    }
}

/// Render the 2×2 quad pane for the current page (dispatch-bgz.1).
fn render_panes(f: &mut Frame, area: Rect, app: &App) {
    let page_start = app.current_page * SLOTS_PER_PAGE;

    // Pre-compute vt lines for each visible slot (hold locks briefly, then release).
    // dispatch-ct2.4: set scrollback offset before reading, then restore to 0.
    let mut page_lines: [Option<Vec<Line<'static>>>; SLOTS_PER_PAGE] =
        [None, None, None, None];
    let mut page_scrolled: [bool; SLOTS_PER_PAGE] = [false; SLOTS_PER_PAGE];
    for local in 0..SLOTS_PER_PAGE {
        let g = page_start + local;
        if g < MAX_SLOTS {
            if let Some(slot) = &app.slots[g] {
                let mut parser = slot.screen.lock().unwrap();
                parser.set_scrollback(slot.scroll_offset);
                page_lines[local] = Some(screen_to_lines(parser.screen()));
                page_scrolled[local] = slot.scroll_offset > 0;
                parser.set_scrollback(0);
            }
        }
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[0]);
    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[1]);

    // top-left=0, top-right=1, bottom-left=2, bottom-right=3
    let areas = [left_rows[0], right_rows[0], left_rows[1], right_rows[1]];
    for local in 0..SLOTS_PER_PAGE {
        let g = page_start + local;
        if g < MAX_SLOTS {
            render_pane(f, areas[local], local, g, app, page_lines[local].take(), page_scrolled[local]);
        }
    }
}

/// Render the orchestrator conversation log view (dispatch-6nm).
/// Replaces the panes area when ViewMode::Orchestrator is active.
fn render_orchestrator(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(Span::styled(
            " ORCHESTRATOR ",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.orch_log.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            " No events yet. The orchestrator log will show voice transcripts, reasoning, and tool calls.",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(empty, inner);
        return;
    }

    // Build lines from events.
    let mut lines: Vec<Line<'static>> = Vec::new();
    for ev in &app.orch_log {
        let (icon, style, body) = match &ev.kind {
            OrchestratorEventKind::VoiceTranscript { text } => (
                "MIC",
                Style::default().fg(Color::Green),
                format!("\"{}\"", text),
            ),
            OrchestratorEventKind::TaskCreated { id, title } => (
                "TASK",
                Style::default().fg(Color::Cyan),
                format!("created {}: {}", id, truncate(title, 60)),
            ),
            OrchestratorEventKind::TaskAssigned { id, agent, slot } => (
                "ASSIGN",
                Style::default().fg(Color::Yellow),
                format!("{} -> {} (slot {})", id, agent, slot),
            ),
            OrchestratorEventKind::TaskComplete { id, agent } => (
                "DONE",
                Style::default().fg(Color::Green),
                format!("{} completed by {}", id, agent),
            ),
            OrchestratorEventKind::Merged { id } => (
                "MERGE",
                Style::default().fg(Color::Green),
                format!("{} merged to main", id),
            ),
            OrchestratorEventKind::MergeConflict { id } => (
                "CONFLICT",
                Style::default().fg(Color::Red),
                format!("{} has merge conflicts", id),
            ),
            OrchestratorEventKind::Dispatched { agent, slot, tool } => (
                "DISPATCH",
                Style::default().fg(Color::Cyan),
                format!("{} in slot {} ({})", agent, slot, tool),
            ),
            OrchestratorEventKind::Terminated { agent, slot } => (
                "TERM",
                Style::default().fg(Color::Red),
                format!("{} (slot {})", agent, slot),
            ),
            OrchestratorEventKind::Queued { id } => (
                "QUEUE",
                Style::default().fg(Color::Yellow),
                format!("{} waiting for available agent", id),
            ),
            // dispatch-h62: orchestrator LLM events
            OrchestratorEventKind::OrchestratorText { text } => (
                "LLM",
                Style::default().fg(Color::Magenta),
                truncate(text, 120).to_string(),
            ),
            OrchestratorEventKind::ToolCallIssued { name } => (
                "TOOL",
                Style::default().fg(Color::Yellow),
                format!("-> {}", name),
            ),
            OrchestratorEventKind::ToolResultSent { name, success } => (
                "RESULT",
                if *success { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) },
                format!("<- {} {}", name, if *success { "ok" } else { "error" }),
            ),
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", ev.time),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(format!("{:<8} ", icon), style.add_modifier(Modifier::BOLD)),
            Span::styled(body, Style::default().fg(Color::White)),
        ]));
    }

    // Apply scroll from bottom.
    let visible = inner.height as usize;
    let total = lines.len();
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.orch_scroll.min(max_scroll);
    let start = total.saturating_sub(visible + scroll);
    let end = (start + visible).min(total);
    let visible_lines: Vec<Line<'static>> = lines[start..end].to_vec();

    let paragraph = Paragraph::new(Text::from(visible_lines));
    f.render_widget(paragraph, inner);

    // Scroll indicator on the right edge.
    if max_scroll > 0 {
        let pct = if scroll == 0 { 100 } else { ((max_scroll - scroll) * 100) / max_scroll };
        let indicator = format!(" {}% ", pct);
        let indicator_area = Rect {
            x: inner.x + inner.width.saturating_sub(indicator.len() as u16 + 1),
            y: area.y,
            width: indicator.len() as u16,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(indicator, Style::default().fg(Color::DarkGray))),
            indicator_area,
        );
    }
}

/// Truncate a string to `max` chars, appending "..." if trimmed.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}

/// Strip ```action ... ``` and <tool_call>...</tool_call> blocks from text,
/// returning only the prose/reasoning portion for chat display (dispatch-chat).
fn strip_action_blocks(text: &str) -> String {
    let mut result = text.to_string();
    // Remove ```action ... ``` blocks
    while let Some(start) = result.find("```action") {
        if let Some(end_fence) = result[start + 9..].find("```") {
            let end = start + 9 + end_fence + 3;
            result.replace_range(start..end, "");
        } else {
            break;
        }
    }
    // Remove <tool_call>...</tool_call> blocks
    while let Some(start) = result.find("<tool_call>") {
        if let Some(end) = result.find("</tool_call>") {
            result.replace_range(start..end + "</tool_call>".len(), "");
        } else {
            break;
        }
    }
    result
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let target_g = app.target_global();
    let target_name = app
        .slots
        .get(target_g)
        .and_then(|s| s.as_ref())
        .map(|a| a.display_name().to_string())
        .unwrap_or_else(|| format!("[{}]", target_g + 1));

    // dispatch-xje: show merge conflict notice when present.
    if !app.conflict_tasks.is_empty() {
        let ids = app.conflict_tasks.join(", ");
        let line = Line::from(vec![
            Span::styled(
                " MERGE CONFLICT: ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(ids, Style::default().fg(Color::Yellow)),
            Span::styled(
                " -- resolve manually, then run: git worktree remove .dispatch/.worktrees/<id>",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let content = match app.mode {
        Mode::Command => {
            let view_indicator = match app.view_mode {
                ViewMode::Agents => "",
                ViewMode::Orchestrator => "ORCH ",
            };
            let hints = if app.view_mode == ViewMode::Orchestrator {
                " o:back  ?:help  q:quit"
            } else {
                " Enter:input  n:new  x:kill  t:tasks  o:orch  ?:help  q:quit"
            };
            Line::from(vec![
                Span::styled(
                    format!(" {} ▸ {} ", view_indicator, target_name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled("│", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    hints,
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
        Mode::Input => Line::from(vec![
            Span::styled(
                format!(" INPUT [{}] ", target_name),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
            Span::styled(
                " ESC:exit  ESC ESC:send Esc to PTY",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    };

    f.render_widget(Paragraph::new(content), area);
}

// ── overlays ──────────────────────────────────────────────────────────────────

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn render_help_overlay(f: &mut Frame, area: Rect) {
    let r = centered_rect(52, 27, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::from(Span::styled(
            " COMMAND MODE KEYS ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::raw("  Enter        Enter input mode")),
        Line::from(Span::raw("  1-4          Select slot on current page")),
        Line::from(Span::raw("  Tab          Next slot (all pages)")),
        Line::from(Span::raw("  Shift+Tab    Prev slot (all pages)")),
        Line::from(Span::raw("  → / ←        Next / prev page")),
        Line::from(Span::raw("  PgUp / PgDn  Scroll output")),
        Line::from(Span::raw("  ↑ / ↓        Scroll orchestrator view")),
        Line::from(Span::raw("  n            Dispatch (repo select in multi-repo)")),
        Line::from(Span::raw("  N            Dispatch into specific slot")),
        Line::from(Span::raw("  k            Kill target agent")),
        Line::from(Span::raw("  R            Rename target agent")),
        Line::from(Span::raw("  S            Rescan repos (multi-repo mode)")),
        Line::from(Span::raw("  t            Task list overlay")),
        Line::from(Span::raw("  h            Prompt history")),
        Line::from(Span::raw("  o            Toggle orchestrator view")),
        Line::from(Span::raw("  p            Toggle PSK visibility")),
        Line::from(Span::raw("  x            Show connection info")),
        Line::from(Span::raw("  q            Quit (confirms if agents running)")),
        Line::from(Span::raw("  ?            This help screen")),
        Line::default(),
        Line::from(Span::styled(
            "  INPUT MODE",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  Esc          Return to command mode (immediate)")),
        Line::from(Span::raw("  Esc Esc      Send literal Escape to PTY (quick double-tap)")),
        Line::default(),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" HELP ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        r,
    );
}

/// Detect the machine's local network IP by connecting a UDP socket.
/// No data is sent; this just determines the outgoing interface address.
fn local_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}

/// Render a connection info overlay showing address, port, and PSK (dispatch-b54).
fn render_connection_info_overlay(f: &mut Frame, area: Rect, app: &App) {
    let host = local_ip().unwrap_or_else(|| "127.0.0.1".to_string());

    let lines = vec![
        Line::from(Span::styled(
            " CONNECTION INFO ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("  Address:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(&host),
        ]),
        Line::from(vec![
            Span::styled("  Port:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}", app.port)),
        ]),
        Line::from(vec![
            Span::styled("  PSK:      ", Style::default().fg(Color::DarkGray)),
            Span::raw(&app.psk),
        ]),
        Line::default(),
        Line::from(Span::styled(
            "  Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let content_height = lines.len() as u16 + 2;
    let r = centered_rect(46, content_height, area);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" CONNECTION ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        r,
    );
}

fn render_task_list_overlay(f: &mut Frame, area: Rect, app: &App) {
    // Full-screen overlay (dispatch-1lc.4)
    let r = centered_rect(area.width.saturating_sub(2), area.height.saturating_sub(2), area);
    f.render_widget(Clear, r);

    let in_progress: Vec<&TaskEntry> = app
        .task_list_data
        .iter()
        .filter(|t| t.status == "in_progress")
        .collect();
    let queued: Vec<&TaskEntry> = app
        .task_list_data
        .iter()
        .filter(|t| t.status == "open")
        .collect();
    let completed: Vec<&TaskEntry> = app
        .task_list_data
        .iter()
        .filter(|t| t.status == "closed")
        .collect();

    let total = app.task_list_data.len();
    let done_count = completed.len();

    // Inner width for truncating titles (subtract border + padding).
    let inner_w = r.width.saturating_sub(4) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Progress summary line.
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            format!("  Tasks: {}/{} complete", done_count, total),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(
                "   {} active  {} queued  {} done",
                in_progress.len(),
                queued.len(),
                done_count
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    lines.push(Line::default());

    // IN PROGRESS section.
    lines.push(Line::from(Span::styled(
        "  IN PROGRESS",
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )));
    if in_progress.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (none)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for t in &in_progress {
            let agent_str = t
                .agent
                .as_deref()
                .map(|a| format!(" <- {}", a))
                .unwrap_or_default();
            let prefix = format!("  [~] {}  ", t.id);
            let avail = inner_w.saturating_sub(prefix.len() + agent_str.len());
            let title = if t.title.len() > avail && avail > 3 {
                format!("{}...", &t.title[..avail - 3])
            } else {
                t.title.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("[~] ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{}  ", t.id),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::styled(title, Style::default().fg(Color::White)),
                Span::styled(agent_str, Style::default().fg(Color::Cyan)),
            ]));
        }
    }
    lines.push(Line::default());

    // QUEUED section.
    lines.push(Line::from(Span::styled(
        "  QUEUED",
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
    )));
    if queued.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (none)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for t in &queued {
            let dep_str = if t.deps.is_empty() {
                String::new()
            } else {
                format!(" -> {}", t.deps.join(", "))
            };
            let prefix = format!("[ ] {}  ", t.id);
            let avail = inner_w.saturating_sub(prefix.len() + dep_str.len());
            let title = if t.title.len() > avail && avail > 3 {
                format!("{}...", &t.title[..avail - 3])
            } else {
                t.title.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("[ ] ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}  ", t.id),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                ),
                Span::styled(title, Style::default().fg(Color::DarkGray)),
                Span::styled(dep_str, Style::default().fg(Color::Red)),
            ]));
        }
    }
    lines.push(Line::default());

    // COMPLETED section (most recent first, limited to avoid flooding).
    lines.push(Line::from(Span::styled(
        "  COMPLETED",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )));
    let max_completed = (r.height as usize).saturating_sub(lines.len() + 4);
    let show_completed: Vec<&TaskEntry> = completed.iter().rev().take(max_completed).cloned().collect();
    if show_completed.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (none)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for t in &show_completed {
            let prefix = format!("[x] {}  ", t.id);
            let avail = inner_w.saturating_sub(prefix.len());
            let title = if t.title.len() > avail && avail > 3 {
                format!("{}...", &t.title[..avail - 3])
            } else {
                t.title.clone()
            };
            lines.push(Line::from(vec![
                Span::styled("[x] ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}  ", t.id),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(title, Style::default().fg(Color::DarkGray)),
            ]));
        }
        if done_count > max_completed {
            lines.push(Line::from(Span::styled(
                format!("    ... and {} more", done_count - max_completed),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  Press any key to close",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" TASK LIST ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        r,
    );
}

fn render_confirm_overlay(f: &mut Frame, area: Rect, title: &str, body: &str) {
    let r = centered_rect(50, 7, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::default(),
        Line::from(Span::styled(format!("  {}", body), Style::default().fg(Color::White))),
        Line::default(),
        Line::from(vec![
            Span::styled("  y ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("confirm    ", Style::default().fg(Color::DarkGray)),
            Span::styled("n / Esc ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ]),
        Line::default(),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(format!(" {} ", title))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        ),
        r,
    );
}

fn render_dispatch_overlay(f: &mut Frame, area: Rect, app: &App) {
    let r = centered_rect(50, 7, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  Slot number (1-{}):", MAX_SLOTS),
            Style::default().fg(Color::White),
        )),
        Line::from(Span::styled(
            format!("  > {}_", app.input_buf),
            Style::default().fg(Color::Green),
        )),
        Line::default(),
        Line::from(Span::styled(
            "  Enter confirm    Esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
        Line::default(),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" DISPATCH INTO SLOT ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        r,
    );
}

/// Render the repo selection overlay for multi-repo mode (dispatch-sa1).
fn render_repo_select_overlay(f: &mut Frame, area: Rect, app: &App) {
    let repos = app.repo_list();
    let height = (repos.len() as u16 + 5).min(area.height.saturating_sub(4));
    let r = centered_rect(60, height, area);
    f.render_widget(Clear, r);
    let mut lines = vec![Line::default()];
    for (i, repo) in repos.iter().enumerate() {
        let name = repo_name_from_path(repo);
        let marker = if i == app.repo_select_idx { ">" } else { " " };
        let style = if i == app.repo_select_idx {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        lines.push(Line::from(Span::styled(
            format!("  {} {}  {}", marker, i + 1, name),
            style,
        )));
    }
    if repos.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no git repos found in child directories)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  Enter select    j/k navigate    r rescan    Esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::default());
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" SELECT REPO ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        r,
    );
}

/// Render the prompt history overlay (dispatch-ct2.8).
fn render_prompt_history_overlay(f: &mut Frame, area: Rect, app: &App) {
    let max_h = area.height.saturating_sub(4).min(30);
    let max_w = area.width.saturating_sub(4).min(80);
    let r = centered_rect(max_w, max_h, area);
    f.render_widget(Clear, r);

    let inner_height = max_h.saturating_sub(4) as usize; // borders + hint line
    let mut lines: Vec<Line<'static>> = Vec::new();

    if app.prompt_history.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "  (no prompts recorded yet)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Show entries centered around the selected one
        let total = app.prompt_history.len();
        let start = if app.history_scroll + inner_height / 2 >= total {
            total.saturating_sub(inner_height)
        } else {
            app.history_scroll.saturating_sub(inner_height / 2)
        };
        let end = (start + inner_height).min(total);

        for i in start..end {
            let entry = &app.prompt_history[i];
            let label = match entry.source {
                PromptSource::Voice => "MIC",
                PromptSource::Keyboard => "KBD",
            };
            let selected = i == app.history_scroll;
            let marker = if selected { ">" } else { " " };
            let text = truncate(&entry.text, (max_w as usize).saturating_sub(22));
            let style = if selected {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", marker), style),
                Span::styled(
                    format!("{} ", entry.time),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<3} ", label),
                    match entry.source {
                        PromptSource::Voice => Style::default().fg(Color::Green),
                        PromptSource::Keyboard => Style::default().fg(Color::Cyan),
                    },
                ),
                Span::styled(
                    format!("{:<8} ", truncate(&entry.target, 8)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(text, style),
            ]));
        }
    }

    // Pad remaining height
    while lines.len() < inner_height {
        lines.push(Line::default());
    }

    // Hint line
    lines.push(Line::from(Span::styled(
        "  j/k navigate    Enter re-send    g/G top/bottom    Esc close",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(Span::styled(
                    " PROMPT HISTORY ",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        ),
        r,
    );
}

fn render_rename_overlay(f: &mut Frame, area: Rect, app: &App) {
    let r = centered_rect(52, 8, area);
    f.render_widget(Clear, r);
    let target_g = app.target_global();
    let current = app
        .slots
        .get(target_g)
        .and_then(|s| s.as_ref())
        .map(|a| a.display_name().to_string())
        .unwrap_or_default();
    let lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  Current: {}", current),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            format!("  > {}_", app.input_buf),
            Style::default().fg(Color::Green),
        )),
        Line::default(),
        Line::from(Span::styled(
            "  Enter confirm    Esc cancel    empty = reset to NATO",
            Style::default().fg(Color::DarkGray),
        )),
        Line::default(),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .title(" RENAME AGENT ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        r,
    );
}

// ── main ──────────────────────────────────────────────────────────────────────

/// Compute PTY dimensions from terminal size.
fn compute_pane_size(term_rows: u16, term_cols: u16) -> (u16, u16) {
    // Cap to sane values (guards against bogus size reports on some terminals).
    let term_rows = term_rows.min(500);
    let term_cols = term_cols.min(1000);
    // 3-row header + 1-row ticker + 1-row footer = 5 fixed rows; remaining split 2 ways vertically.
    // Each pane: 2 border rows + 4 info strip rows = 6 overhead.
    let pane_h = term_rows.saturating_sub(5) / 2;
    let rows = pane_h.saturating_sub(6).max(10);
    // Each pane is half the terminal width minus 2 for borders.
    let cols = (term_cols / 2).saturating_sub(2).max(20);
    (rows, cols)
}

// ── tests for tasks.md parsing (dispatch-1lc.3) ──────────────────────────────

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::RegeneratePsk) => {
            println!("{}", config::regenerate_psk());
            return Ok(());
        }
        Some(Commands::ShowPsk) => {
            println!("{}", config::load_or_create().auth.psk);
            return Ok(());
        }
        Some(Commands::Config) => {
            println!("{}", config::config_path().display());
            return Ok(());
        }
        None => {}
    }

    let cfg = config::load_or_create();

    // Load or generate TLS certificate (dispatch-ct2.6).
    let tls = config::load_or_create_tls();
    let tls_fingerprint = tls.fingerprint.clone();

    // Broadcast channel for pushing chat messages to all connected radio clients (dispatch-chat).
    let (chat_tx, _) = tokio::sync::broadcast::channel::<String>(256);

    // Start the WebSocket server with TLS (dispatch-bgz.7, dispatch-ct2.6).
    let ws_state: ws_server::SharedState = Arc::new(Mutex::new(ws_server::ConsoleState::new()));
    {
        let state = Arc::clone(&ws_state);
        let psk = cfg.auth.psk.clone();
        let port = cfg.server.port;
        let acceptor = tls.acceptor;
        let chat_tx_ws = chat_tx.clone();
        thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .expect("tokio runtime")
                .block_on(ws_server::run_server(state, port, psk, acceptor, chat_tx_ws));
        });
    }

    // Advertise via mDNS so the radio can discover us (dispatch-ct2.1).
    let _mdns = mdns::advertise(cfg.server.port);

    // Determine initial pane size from the terminal.
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((160, 40));
    let (pane_rows, pane_cols) = compute_pane_size(term_rows, term_cols);

    let completion_timeout = Duration::from_secs(cfg.beads.completion_timeout_secs as u64);

    // Resolve repo root and workspace mode (dispatch-xje, dispatch-sa1).
    let git_toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
        } else {
            None
        });

    let (repo_root, workspace) = if let Some(root) = git_toplevel {
        // Inside a git repo — single-repo mode (backwards compatible).
        (root.clone(), Workspace::SingleRepo { root })
    } else {
        // Not in a git repo — scan children for repos (dispatch-sa1).
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| ".".to_string());
        let repos = scan_child_repos(&cwd);
        (cwd.clone(), Workspace::MultiRepo { parent: cwd, repos })
    };

    let mut app = App::new(
        cfg.auth.psk.clone(),
        cfg.server.port,
        ws_state,
        pane_rows,
        pane_cols,
        cfg.tools.clone(),
        completion_timeout,
        repo_root.clone(),
        workspace,
        cfg.terminal.scrollback_lines,
        tls_fingerprint,
        chat_tx,
    );

    // dispatch-h62: orchestrator is spawned lazily on first voice transcript
    // to avoid the heavy claude process consuming memory while idle.
    app.push_ticker("ORCHESTRATOR: ready (will start on first voice input)".to_string());

    // dispatch-sa1: show multi-repo indicator if applicable.
    if app.is_multi_repo() {
        let repo_count = app.repo_list().len();
        app.push_ticker(format!("MULTI-REPO: detected {} repos", repo_count));
    }

    // Channel for WsEvents from the WebSocket thread (dispatch-1lc.1).
    let (ws_event_tx, ws_event_rx) = mpsc::channel::<ws_server::WsEvent>();
    {
        let mut st = app.ws_state.lock().unwrap();
        st.event_tx = Some(ws_event_tx);
    }

    // Background thread: poll .dispatch/tasks.md for ready tasks (dispatch-1lc.3).
    // dispatch-sa1: in multi-repo mode, poll all repos.
    let (tasks_tx, tasks_rx) = mpsc::channel::<Vec<QueuedTask>>();
    let poll_repos: Vec<String> = app.repo_list().iter().map(|s| s.to_string()).collect();
    thread::spawn(move || loop {
        let mut all_tasks = Vec::new();
        for repo in &poll_repos {
            all_tasks.extend(fetch_ready_tasks(repo));
        }
        let _ = tasks_tx.send(all_tasks);
        thread::sleep(Duration::from_secs(TASK_POLL_SECS));
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut quit_requested = false;

    'main: loop {
        // Close any slots whose child exited naturally (dispatch-bgz.9, dispatch-xje).
        for i in 0..MAX_SLOTS {
            if let Some(s) = &app.slots[i] {
                if s.child_exited.load(Ordering::Relaxed) {
                    let callsign = s.display_name().to_string();
                    let task_id = s.task_id.clone();
                    let slot_repo = s.repo_root.clone();
                    app.slots[i] = None;
                    // Sync ws_state so the handler knows this slot is empty (dispatch-boa).
                    {
                        let mut st = app.ws_state.lock().unwrap();
                        st.slots[i] = None;
                    }
                    if let Some(id) = task_id {
                        app.push_orch(OrchestratorEventKind::TaskComplete { id: id.clone(), agent: callsign.clone() });
                        app.push_chat(&callsign, &format!("Task {} complete.", id));
                        // dispatch-h62: notify orchestrator of completion so it can decide next steps.
                        if let Some(orch) = &mut app.orchestrator {
                            orch.send_message(&format!("[EVENT] TASK_COMPLETE agent={} task={}", callsign, id));
                        }
                        // dispatch-bka: agent merges its own branch before exiting.
                        app.push_ticker(format!("TASK COMPLETE: {} closed {} — slot {} now standby", callsign, id, i + 1));
                        update_task_in_file(&slot_repo, &id, 'x', None);
                        // Dispatch newly unblocked tasks after completion.
                        dispatch_ready_tasks(&mut app);
                    } else {
                        app.push_ticker(format!("AGENT EXITED: {} (slot {}) — standby", callsign, i + 1));
                        if let Some(orch) = &mut app.orchestrator {
                            orch.send_message(&format!("[EVENT] AGENT_EXITED agent={} slot={}", callsign, i + 1));
                        }
                    }
                }
            }
        }

        // Idle agent pickup: detect task completion via idle prompt or inactivity
        // timeout, then assign the next queued task (dispatch-1lc.2).
        let now = Instant::now();
        let mut completed: Vec<(usize, String)> = Vec::new();
        for i in 0..MAX_SLOTS {
            let slot = match app.slots[i].as_mut() {
                Some(s) if s.task_id.is_some() => s,
                _ => continue,
            };

            // Update screen hash to track last output time.
            let hash = {
                let parser = slot.screen.lock().unwrap();
                compute_screen_hash(parser.screen())
            };
            if hash != slot.last_screen_hash {
                slot.last_screen_hash = hash;
                slot.last_output_at = now;
                slot.idle_since = None;
                // dispatch-ct2.4: snap back to bottom on new output
                slot.scroll_offset = 0;
            }

            // Layer 1: idle prompt detection with 500ms debounce.
            let idle_prompt = {
                let parser = slot.screen.lock().unwrap();
                is_idle_prompt(parser.screen(), &slot.tool)
            };
            if idle_prompt {
                match slot.idle_since {
                    None => slot.idle_since = Some(now),
                    Some(t) if now.duration_since(t) >= Duration::from_millis(500) => {
                        completed.push((i, slot.task_id.clone().unwrap()));
                    }
                    _ => {}
                }
            } else {
                slot.idle_since = None;
            }

            // Layer 2: inactivity timeout.
            if app.completion_timeout.as_secs() > 0
                && now.duration_since(slot.last_output_at) >= app.completion_timeout
                && slot.idle_since.is_none() // avoid double-completing
                && !completed.iter().any(|(idx, _)| *idx == i)
            {
                completed.push((i, slot.task_id.clone().unwrap()));
            }
        }

        for (i, task_id) in completed {
            let agent_name = app.slots[i].as_ref().map(|s| s.display_name().to_string()).unwrap_or_default();
            let slot_repo = app.slots[i].as_ref().map(|s| s.repo_root.clone()).unwrap_or_else(|| app.default_repo_root().to_string());
            app.push_orch(OrchestratorEventKind::TaskComplete { id: task_id.clone(), agent: agent_name.clone() });
            app.push_chat(&agent_name, &format!("Task {} complete.", task_id));
            // dispatch-h62: notify orchestrator of idle-detected completion.
            if let Some(orch) = &mut app.orchestrator {
                orch.send_message(&format!("[EVENT] TASK_COMPLETE agent={} task={}", agent_name, task_id));
            }
            if let Some(slot) = app.slots[i].as_mut() {
                slot.task_id = None;
                slot.idle_since = None;
            }
            // Sync ws_state so the WebSocket handler knows this slot is idle
            // and can accept follow-up tasks (dispatch-boa).
            {
                let mut st = app.ws_state.lock().unwrap();
                if let Some(ref mut agent) = st.slots[i] {
                    agent.status = ws_server::AgentStatus::Idle;
                    agent.task = None;
                }
            }
            update_task_in_file(&slot_repo, &task_id, 'x', None);

            // Dispatch newly unblocked tasks after completion.
            dispatch_ready_tasks(&mut app);

            // Pick up next available queued task and assign it to the idle slot.
            let next = fetch_ready_tasks(&slot_repo).into_iter().next();
            if let Some(qt) = next {
                let mut assigned = false;
                let mut assigned_callsign = String::new();
                if let Some(slot) = app.slots[i].as_mut() {
                    let callsign = slot.callsign.clone();
                    if update_task_in_file(&slot_repo, &qt.id, '~', Some(&callsign)) {
                        let prompt = format!("Your task ID is {}. {}\r", qt.id, qt.title);
                        let _ = slot.writer.write_all(prompt.as_bytes());
                        let _ = slot.writer.flush();
                        slot.task_id = Some(qt.id.clone());
                        slot.last_output_at = Instant::now();
                        assigned = true;
                        assigned_callsign = callsign;
                    }
                }
                if assigned {
                    app.push_orch(OrchestratorEventKind::TaskAssigned { id: qt.id.clone(), agent: assigned_callsign.clone(), slot: i + 1 });
                    // Sync ws_state for the new task assignment (dispatch-boa).
                    let mut st = app.ws_state.lock().unwrap();
                    if let Some(ref mut agent) = st.slots[i] {
                        agent.status = ws_server::AgentStatus::Busy;
                        agent.task = Some(qt.id.clone());
                    }
                }
                app.queued_tasks.retain(|t| t.id != qt.id);
            }
        }

        if quit_requested && app.active_count() == 0 {
            break;
        }

        while let Ok(tasks) = tasks_rx.try_recv() {
            let prev_count = app.queued_tasks.len();
            let new_count = tasks.len();
            if new_count > prev_count {
                let added = new_count - prev_count;
                app.push_ticker(format!("TASKS: {} new task{} queued — {} total ready", added, if added == 1 { "" } else { "s" }, new_count));
            }
            app.queued_tasks = tasks;
        }

        // Advance ticker animation each frame (dispatch-ami).
        app.tick_ticker();

        // Process events from the WebSocket thread (dispatch-1lc.1, dispatch-h62).
        while let Ok(event) = ws_event_rx.try_recv() {
            let ws_server::WsEvent::VoiceTranscript { text } = event;
            app.radio_state = RadioState::Connected;
            app.push_orch(OrchestratorEventKind::VoiceTranscript { text: text.clone() });
            app.push_chat("You", &text);
            // Lazy-spawn orchestrator on first voice input.
            if app.orchestrator.is_none() {
                let repos: Vec<&str> = app.repo_list().iter().copied().collect();
                let tool_defs = tools::tool_definitions();
                let system_prompt = orchestrator::build_system_prompt(&repos, &tool_defs);
                let orch_cwd = app.default_repo_root().to_string();
                match orchestrator::spawn(&system_prompt, &orch_cwd) {
                    Some(orch) => {
                        app.orchestrator = Some(orch);
                        app.push_ticker("ORCHESTRATOR: online".to_string());
                    }
                    None => {
                        app.push_ticker("ORCHESTRATOR: failed to spawn".to_string());
                    }
                }
            }
            if let Some(orch) = &mut app.orchestrator {
                orch.send_message(&format!("[MIC] {}", text));
            }
        }

        // dispatch-h62: poll orchestrator output and execute tool calls.
        // Collect all pending outputs first to avoid borrow conflicts.
        let mut orch_outputs: Vec<orchestrator::OrchestratorOutput> = Vec::new();
        if let Some(orch) = &mut app.orchestrator {
            while let Some(output) = orch.try_recv() {
                orch_outputs.push(output);
            }
        }
        for output in orch_outputs {
            match output {
                orchestrator::OrchestratorOutput::Text(text) => {
                    app.push_orch(OrchestratorEventKind::OrchestratorText { text: text.clone() });

                    // dispatch-chat: forward orchestrator reasoning to radio (strip action blocks).
                    let chat_text = strip_action_blocks(&text);
                    let chat_text = chat_text.trim();
                    if !chat_text.is_empty() {
                        app.push_chat("Dispatcher", chat_text);
                    }

                    // Parse and execute any tool calls in the response.
                    let calls = orchestrator::parse_all_tool_calls(&text);
                    for call in &calls {
                        let call_name = match call {
                            tools::ToolCall::Dispatch { .. } => "dispatch",
                            tools::ToolCall::Terminate { .. } => "terminate",
                            tools::ToolCall::Merge { .. } => "merge",
                            tools::ToolCall::ListAgents => "list_agents",
                            tools::ToolCall::ListRepos => "list_repos",
                            tools::ToolCall::MessageAgent { .. } => "message_agent",
                        };
                        app.push_orch(OrchestratorEventKind::ToolCallIssued {
                            name: call_name.to_string(),
                        });

                        let result = app.execute_tool(call);
                        let success = !matches!(result, tools::ToolResult::Error { .. });
                        app.push_orch(OrchestratorEventKind::ToolResultSent {
                            name: call_name.to_string(),
                            success,
                        });

                        // Send all results back so the orchestrator knows what happened.
                        let result_text = tools::format_tool_result(None, &result);
                        if let Some(orch) = &mut app.orchestrator {
                            orch.send_message(&result_text);
                        }
                    }
                }
                orchestrator::OrchestratorOutput::TurnComplete => {
                    // Orchestrator finished responding, now idle.
                }
                orchestrator::OrchestratorOutput::Exited => {
                    app.push_ticker("ORCHESTRATOR: process exited — manual mode only".to_string());
                    app.orchestrator = None;
                }
            }
        }

        terminal.draw(|f| {
            let full = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(full);

            render_header(f, chunks[0], &app);
            render_ticker(f, chunks[1], &app);
            // Clear the main content area to prevent visual artifacts when switching views.
            f.render_widget(Clear, chunks[2]);
            match app.view_mode {
                ViewMode::Agents => render_panes(f, chunks[2], &app),
                ViewMode::Orchestrator => render_orchestrator(f, chunks[2], &app),
            }
            render_footer(f, chunks[3], &app);

            match app.overlay {
                Overlay::None => {}
                Overlay::Help => render_help_overlay(f, full),
                Overlay::TaskList => render_task_list_overlay(f, full, &app),
                Overlay::ConnectionInfo => render_connection_info_overlay(f, full, &app),
                Overlay::ConfirmQuit => render_confirm_overlay(
                    f, full, "QUIT", "Agents are running. Really quit?",
                ),
                Overlay::ConfirmTerminate => {
                    let target_g = app.target_global();
                    let name = app.slots.get(target_g)
                        .and_then(|s| s.as_ref())
                        .map(|a| a.display_name().to_string())
                        .unwrap_or_else(|| format!("slot {}", target_g + 1));
                    render_confirm_overlay(f, full, "TERMINATE", &format!("Terminate {}?", name));
                }
                Overlay::DispatchSlot => render_dispatch_overlay(f, full, &app),
                Overlay::Rename => render_rename_overlay(f, full, &app),
                Overlay::RepoSelect => render_repo_select_overlay(f, full, &app),
                Overlay::PromptHistory => render_prompt_history_overlay(f, full, &app),
            }
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                // Terminal resize (dispatch-bgz.6)
                Event::Resize(new_cols, new_rows) => {
                    let (new_pane_rows, new_pane_cols) = compute_pane_size(new_rows, new_cols);
                    app.pane_rows = new_pane_rows;
                    app.pane_cols = new_pane_cols;
                    resize_all_slots(
                        &mut app.slots,
                        PtySize { rows: new_pane_rows, cols: new_pane_cols, pixel_width: 0, pixel_height: 0 },
                    );
                }

                Event::Key(key) if key.kind == KeyEventKind::Press => match app.mode {
                    // Input mode: keystrokes forwarded to targeted PTY (dispatch-bgz.4)
                    Mode::Input => {
                        // dispatch-qwd: Esc immediately exits input mode
                        if key.code == KeyCode::Esc {
                            app.mode = Mode::Command;
                            app.esc_exit_time = Some(Instant::now());
                            app.input_line_buf.clear(); // dispatch-ct2.8
                            continue 'main;
                        }

                        // dispatch-ct2.8: shadow-track keyboard input for history
                        match key.code {
                            KeyCode::Enter => {
                                let text = app.input_line_buf.trim().to_string();
                                if !text.is_empty() {
                                    let target_g = app.target_global();
                                    let target_name = app.slots.get(target_g)
                                        .and_then(|s| s.as_ref())
                                        .map(|s| s.display_name().to_string())
                                        .unwrap_or_else(|| format!("slot-{}", target_g + 1));
                                    app.log_prompt(PromptSource::Keyboard, &target_name, &text);
                                }
                                app.input_line_buf.clear();
                            }
                            KeyCode::Backspace => { app.input_line_buf.pop(); }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                app.input_line_buf.push(c);
                            }
                            _ => {}
                        }

                        let target_g = app.target_global();
                        if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                            let bytes = key_to_pty_bytes(&key);
                            if !bytes.is_empty() {
                                let _ = slot.writer.write_all(&bytes);
                                let _ = slot.writer.flush();
                            }
                        }
                    }

                    // Command mode (dispatch-bgz.5)
                    Mode::Command => {
                        if app.overlay != Overlay::None {
                            match app.overlay {
                                Overlay::Help | Overlay::TaskList | Overlay::ConnectionInfo => {
                                    app.overlay = Overlay::None;
                                }

                                Overlay::ConfirmQuit => match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        if app.active_count() == 0 {
                                            break 'main;
                                        }
                                        for i in 0..MAX_SLOTS {
                                            let slot_repo = app.slots[i].as_ref().map(|s| s.repo_root.clone());
                                            if let Some(task_id) = terminate_slot(&mut app.slots[i]) {
                                                let repo = slot_repo.unwrap_or_else(|| app.default_repo_root().to_string());
                                                update_task_in_file(&repo, &task_id, ' ', None);
                                            }
                                        }
                                        // dispatch-h62: kill orchestrator on quit.
                                        if let Some(orch) = &mut app.orchestrator {
                                            orch.kill();
                                        }
                                        quit_requested = true;
                                        app.overlay = Overlay::None;
                                    }
                                    _ => app.overlay = Overlay::None,
                                },

                                Overlay::ConfirmTerminate => match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        let target_g = app.target_global();
                                        let callsign = app.slots[target_g].as_ref().map(|s| s.display_name().to_string()).unwrap_or_default();
                                        let slot_repo = app.slots[target_g].as_ref().map(|s| s.repo_root.clone()).unwrap_or_else(|| app.default_repo_root().to_string());
                                        if !callsign.is_empty() {
                                            app.push_orch(OrchestratorEventKind::Terminated { agent: callsign.clone(), slot: target_g + 1 });
                                        }
                                        if let Some(task_id) = terminate_slot(&mut app.slots[target_g]) {
                                            update_task_in_file(&slot_repo, &task_id, ' ', None);
                                            app.push_ticker(format!("TERMINATED: {} (slot {}) — task {} reopened", callsign, target_g + 1, task_id));
                                        } else if !callsign.is_empty() {
                                            app.push_ticker(format!("TERMINATED: {} (slot {})", callsign, target_g + 1));
                                        }
                                        app.overlay = Overlay::None;
                                    }
                                    _ => app.overlay = Overlay::None,
                                },

                                Overlay::DispatchSlot => match key.code {
                                    KeyCode::Esc => {
                                        app.input_buf.clear();
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Backspace => { app.input_buf.pop(); }
                                    KeyCode::Enter => {
                                        if let Ok(n) = app.input_buf.trim().parse::<usize>() {
                                            if n >= 1 && n <= MAX_SLOTS {
                                                let g = n - 1;
                                                let page = g / SLOTS_PER_PAGE;
                                                let local = g % SLOTS_PER_PAGE;
                                                app.current_page = page;
                                                app.target = local;
                                                if app.slots[g].is_none() {
                                                    let target_repo = app.default_repo_root().to_string();
                                                    let cmd = app.tool_cmd("claude-code").to_string();
                                                    if let Some(slot) = dispatch_slot(
                                                        g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                                                        app.scrollback_lines, repo_name_from_path(&target_repo), &target_repo,
                                                        None,
                                                    ) {
                                                        let name = slot.display_name().to_string();
                                                        app.push_orch(OrchestratorEventKind::Dispatched { agent: name.clone(), slot: g + 1, tool: "claude-code".to_string() });
                                                        app.push_ticker(format!("DISPATCH: {} launched in slot {}", name, g + 1));
                                                        app.slots[g] = Some(slot);
                                                    }
                                                }
                                            }
                                        }
                                        app.input_buf.clear();
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Char(c) if c.is_ascii_digit() => {
                                        if app.input_buf.len() < 2 {
                                            app.input_buf.push(c);
                                        }
                                    }
                                    _ => {}
                                },

                                // Rename overlay (dispatch-bgz.3)
                                Overlay::Rename => match key.code {
                                    KeyCode::Esc => {
                                        app.input_buf.clear();
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Backspace => { app.input_buf.pop(); }
                                    KeyCode::Enter => {
                                        let name = app.input_buf.trim().to_string();
                                        let target_g = app.target_global();
                                        if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                            if name.is_empty() {
                                                slot.custom_name = None; // reset to NATO
                                            } else if is_valid_callsign(&name) {
                                                slot.custom_name = Some(name);
                                            }
                                        }
                                        app.input_buf.clear();
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Char(c) if !c.is_control() => {
                                        if app.input_buf.len() < 20 {
                                            app.input_buf.push(c);
                                        }
                                    }
                                    _ => {}
                                },

                                // Repo selection overlay (dispatch-sa1)
                                Overlay::RepoSelect => match key.code {
                                    KeyCode::Esc => {
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Char('j') | KeyCode::Down => {
                                        let count = app.repo_list().len();
                                        if count > 0 && app.repo_select_idx < count - 1 {
                                            app.repo_select_idx += 1;
                                        }
                                    }
                                    KeyCode::Char('k') | KeyCode::Up => {
                                        if app.repo_select_idx > 0 {
                                            app.repo_select_idx -= 1;
                                        }
                                    }
                                    KeyCode::Char('r') => {
                                        // Re-scan child directories for repos.
                                        app.rescan_repos();
                                        app.repo_select_idx = 0;
                                    }
                                    KeyCode::Enter => {
                                        let repos = app.repo_list().iter().map(|s| s.to_string()).collect::<Vec<_>>();
                                        if let Some(selected_repo) = repos.get(app.repo_select_idx).cloned() {
                                            app.overlay = Overlay::None;
                                            // Dispatch into the first empty slot, targeting the selected repo.
                                            if let Some(g) = app.slots.iter().position(|s| s.is_none()) {
                                                let cmd = app.tool_cmd("claude-code").to_string();
                                                if let Some(slot) = dispatch_slot(
                                                    g, "claude-code", &cmd, app.pane_rows, app.pane_cols,
                                                    Some(&selected_repo), app.scrollback_lines,
                                                    repo_name_from_path(&selected_repo), &selected_repo,
                                                    None,
                                                ) {
                                                    let page = g / SLOTS_PER_PAGE;
                                                    let local = g % SLOTS_PER_PAGE;
                                                    let name = slot.display_name().to_string();
                                                    app.push_orch(OrchestratorEventKind::Dispatched { agent: name.clone(), slot: g + 1, tool: "claude-code".to_string() });
                                                    app.push_ticker(format!("DISPATCH: {} launched in slot {} — repo {}", name, g + 1, repo_name_from_path(&selected_repo)));
                                                    app.slots[g] = Some(slot);
                                                    app.current_page = page;
                                                    app.target = local;
                                                }
                                            }
                                        }
                                    }
                                    KeyCode::Char(c) if c.is_ascii_digit() => {
                                        // Quick-select by number.
                                        let n = c.to_digit(10).unwrap_or(0) as usize;
                                        let repos = app.repo_list();
                                        if n >= 1 && n <= repos.len() {
                                            app.repo_select_idx = n - 1;
                                        }
                                    }
                                    _ => {}
                                },

                                // Prompt history overlay (dispatch-ct2.8)
                                Overlay::PromptHistory => match key.code {
                                    KeyCode::Esc | KeyCode::Char('h') => {
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Char('j') | KeyCode::Down => {
                                        if !app.prompt_history.is_empty() && app.history_scroll + 1 < app.prompt_history.len() {
                                            app.history_scroll += 1;
                                        }
                                    }
                                    KeyCode::Char('k') | KeyCode::Up => {
                                        app.history_scroll = app.history_scroll.saturating_sub(1);
                                    }
                                    KeyCode::Char('G') => {
                                        if !app.prompt_history.is_empty() {
                                            app.history_scroll = app.prompt_history.len() - 1;
                                        }
                                    }
                                    KeyCode::Char('g') => {
                                        app.history_scroll = 0;
                                    }
                                    KeyCode::Enter => {
                                        // Re-send the selected prompt to the current target
                                        if let Some(entry) = app.prompt_history.get(app.history_scroll).cloned() {
                                            let target_g = app.target_global();
                                            if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                                let with_enter = format!("{}\r", entry.text);
                                                let _ = slot.writer.write_all(with_enter.as_bytes());
                                                let _ = slot.writer.flush();
                                            }
                                            app.overlay = Overlay::None;
                                        }
                                    }
                                    _ => {}
                                },

                                Overlay::None => unreachable!(),
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('q') => {
                                    if app.active_count() > 0 {
                                        app.overlay = Overlay::ConfirmQuit;
                                    } else {
                                        if let Some(orch) = &mut app.orchestrator {
                                            orch.kill();
                                        }
                                        break 'main;
                                    }
                                }

                                KeyCode::Enter => {
                                    // dispatch-ct2.4: reset scroll when entering input mode
                                    let target_g = app.target_global();
                                    if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                        slot.scroll_offset = 0;
                                    }
                                    app.mode = Mode::Input;
                                    app.esc_exit_time = None;
                                    app.input_line_buf.clear(); // dispatch-ct2.8
                                }

                                KeyCode::Char('1') => app.target = 0,
                                KeyCode::Char('2') => app.target = 1,
                                KeyCode::Char('3') => app.target = 2,
                                KeyCode::Char('4') => app.target = 3,

                                KeyCode::Tab => {
                                    let total = app.total_pages() * SLOTS_PER_PAGE;
                                    let global = app.current_page * SLOTS_PER_PAGE + app.target;
                                    let next = (global + 1) % total;
                                    app.current_page = next / SLOTS_PER_PAGE;
                                    app.target = next % SLOTS_PER_PAGE;
                                }

                                KeyCode::BackTab => {
                                    let total = app.total_pages() * SLOTS_PER_PAGE;
                                    let global = app.current_page * SLOTS_PER_PAGE + app.target;
                                    let prev = (global + total - 1) % total;
                                    app.current_page = prev / SLOTS_PER_PAGE;
                                    app.target = prev % SLOTS_PER_PAGE;
                                }

                                KeyCode::Right => {
                                    let total = app.total_pages();
                                    if app.current_page + 1 < total {
                                        app.current_page += 1;
                                    }
                                }

                                KeyCode::Left => {
                                    if app.current_page > 0 {
                                        app.current_page -= 1;
                                    }
                                }

                                // Dispatch into first empty slot (dispatch-bgz.6)
                                // dispatch-sa1: in multi-repo mode, open repo selector first.
                                KeyCode::Char('n') => {
                                    if app.is_multi_repo() {
                                        app.repo_select_idx = 0;
                                        app.overlay = Overlay::RepoSelect;
                                    } else {
                                        let target_repo = app.default_repo_root().to_string();
                                        if let Some(g) = app.slots.iter().position(|s| s.is_none()) {
                                            let cmd = app.tool_cmd("claude-code").to_string();
                                            if let Some(slot) = dispatch_slot(
                                                g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                                                app.scrollback_lines, repo_name_from_path(&target_repo), &target_repo,
                                                None,
                                            ) {
                                                let page = g / SLOTS_PER_PAGE;
                                                let local = g % SLOTS_PER_PAGE;
                                                let name = slot.display_name().to_string();
                                                app.push_orch(OrchestratorEventKind::Dispatched { agent: name.clone(), slot: g + 1, tool: "claude-code".to_string() });
                                                app.push_ticker(format!("DISPATCH: {} launched in slot {}", name, g + 1));
                                                app.slots[g] = Some(slot);
                                                app.current_page = page;
                                                app.target = local;
                                            }
                                        }
                                    }
                                }

                                KeyCode::Char('N') => {
                                    app.input_buf.clear();
                                    app.overlay = Overlay::DispatchSlot;
                                }

                                // Terminate target agent (dispatch-bgz.6)
                                KeyCode::Char('k') => {
                                    let target_g = app.target_global();
                                    if app.slots[target_g].is_some() {
                                        app.overlay = Overlay::ConfirmTerminate;
                                    }
                                }

                                // Rename target agent (dispatch-bgz.3)
                                KeyCode::Char('R') => {
                                    let target_g = app.target_global();
                                    if app.slots[target_g].is_some() {
                                        app.input_buf.clear();
                                        app.overlay = Overlay::Rename;
                                    }
                                }

                                KeyCode::Char('t') => {
                                    // dispatch-sa1: aggregate tasks from all repos in multi-repo mode.
                                    app.task_list_data = Vec::new();
                                    for repo in &app.repo_list().iter().map(|s| s.to_string()).collect::<Vec<_>>() {
                                        app.task_list_data.extend(fetch_task_list_from_file(repo, &app.slots));
                                    }
                                    app.overlay = Overlay::TaskList;
                                }
                                // Prompt history overlay (dispatch-ct2.8)
                                KeyCode::Char('h') => {
                                    app.history_scroll = 0;
                                    app.overlay = Overlay::PromptHistory;
                                }

                                KeyCode::Char('p') => app.psk_expanded = !app.psk_expanded,
                                KeyCode::Char('x') => app.overlay = Overlay::ConnectionInfo,
                                KeyCode::Char('?') => app.overlay = Overlay::Help,

                                // Toggle orchestrator view (dispatch-6nm)
                                KeyCode::Char('o') => {
                                    app.view_mode = match app.view_mode {
                                        ViewMode::Agents => ViewMode::Orchestrator,
                                        ViewMode::Orchestrator => ViewMode::Agents,
                                    };
                                    app.orch_scroll = 0;
                                }

                                // Rescan repos in multi-repo mode (dispatch-sa1)
                                KeyCode::Char('S') if app.is_multi_repo() => {
                                    let old_count = app.repo_list().len();
                                    app.rescan_repos();
                                    let new_count = app.repo_list().len();
                                    app.push_ticker(format!("RESCAN: {} repos detected (was {})", new_count, old_count));
                                }

                                // Orchestrator scroll (dispatch-6nm)
                                KeyCode::Up if app.view_mode == ViewMode::Orchestrator => {
                                    app.orch_scroll = app.orch_scroll.saturating_add(1);
                                }
                                KeyCode::Down if app.view_mode == ViewMode::Orchestrator => {
                                    app.orch_scroll = app.orch_scroll.saturating_sub(1);
                                }

                                // PgUp/PgDn: orchestrator scroll or pane scrollback
                                KeyCode::PageUp => {
                                    if app.view_mode == ViewMode::Orchestrator {
                                        app.orch_scroll = app.orch_scroll.saturating_add(10);
                                    } else {
                                        // Scrollback (dispatch-ct2.4)
                                        let target_g = app.target_global();
                                        if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                            let half = (app.pane_rows as usize) / 2;
                                            slot.scroll_offset = slot.scroll_offset.saturating_add(half);
                                        }
                                    }
                                }
                                KeyCode::PageDown => {
                                    if app.view_mode == ViewMode::Orchestrator {
                                        app.orch_scroll = app.orch_scroll.saturating_sub(10);
                                    } else {
                                        // Scrollback (dispatch-ct2.4)
                                        let target_g = app.target_global();
                                        if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                            let half = (app.pane_rows as usize) / 2;
                                            slot.scroll_offset = slot.scroll_offset.saturating_sub(half);
                                        }
                                    }
                                }

                                // dispatch-qwd: double-Esc sends literal Escape to PTY
                                KeyCode::Esc => {
                                    if let Some(t) = app.esc_exit_time.take() {
                                        if t.elapsed() < Duration::from_millis(300) {
                                            let target_g = app.target_global();
                                            if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                                let _ = slot.writer.write_all(b"\x1b");
                                                let _ = slot.writer.flush();
                                            }
                                        }
                                    }
                                }

                                _ => {}
                            }
                        }
                    }
                },

                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
