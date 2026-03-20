// Type definitions, enums, and constants for the dispatch console.

use std::{
    io::Write,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Instant,
};

use dispatch_core::orchestrator;

/// Total number of agent slots (maps to NATO alphabet A-Z).
pub const MAX_SLOTS: usize = 26;
/// Slots per page (2x2 grid).
pub const SLOTS_PER_PAGE: usize = 4;
pub const TASK_POLL_SECS: u64 = 5;

pub const NATO: &[&str] = &[
    "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT", "GOLF", "HOTEL", "INDIA", "JULIET",
    "KILO", "LIMA", "MIKE", "NOVEMBER", "OSCAR", "PAPA", "QUEBEC", "ROMEO", "SIERRA", "TANGO",
    "UNIFORM", "VICTOR", "WHISKEY", "X-RAY", "YANKEE", "ZULU",
];

// Reserved words that cannot be used as custom callsigns (dispatch-bgz.3).
pub const RESERVED_CALLSIGNS: &[&str] = &[
    "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT", "GOLF", "HOTEL", "INDIA", "JULIET",
    "KILO", "LIMA", "MIKE", "NOVEMBER", "OSCAR", "PAPA", "QUEBEC", "ROMEO", "SIERRA", "TANGO",
    "UNIFORM", "VICTOR", "WHISKEY", "X-RAY", "YANKEE", "ZULU",
    "STANDBY", "DISPATCH", "IDLE",
];

/// Input mode for the console (dispatch-bgz.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Command,
    Input,
}

/// Which view is shown in the main area (dispatch-6nm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Default 2x2 agent grid.
    Agents,
    /// Orchestrator conversation log.
    Orchestrator,
}

/// A timestamped orchestrator event for the log view (dispatch-6nm).
#[derive(Clone)]
pub struct OrchestratorEvent {
    pub time: String,
    pub kind: OrchestratorEventKind,
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum OrchestratorEventKind {
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
pub enum Workspace {
    /// Launched inside a git repo — original single-repo behavior.
    SingleRepo { root: String },
    /// Launched from a non-repo directory — children contain git repos.
    MultiRepo { parent: String, repos: Vec<String> },
}

/// Active overlay (dispatch-sa1, dispatch-bgz.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
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
pub enum RadioState {
    Connected,
    Disconnected,
}

/// Per-slot PTY and display state (dispatch-bgz.2).
/// Not Send — only used on the main thread.
pub struct SlotState {
    pub callsign: String,            // NATO default (slot-bound)
    pub custom_name: Option<String>, // user rename (dispatch-bgz.3)
    pub tool: String,
    pub task_id: Option<String>,
    pub repo_name: String,             // short repo dir name for grid header (dispatch-2dc)
    pub repo_root: String,             // absolute repo root for this slot (dispatch-sa1)
    pub dispatch_time: Instant,
    pub dispatch_wall_str: String,
    // PTY
    pub screen: Arc<Mutex<vt100::Parser>>,
    pub writer: Box<dyn Write + Send>,
    pub child_exited: Arc<AtomicBool>,
    pub child_pid: Option<u32>,
    // Keep master alive for resize (dispatch-bgz.6)
    pub master: Box<dyn portable_pty::MasterPty>,
    // Task completion detection (dispatch-1lc.2)
    pub last_output_at: Instant,     // when screen content last changed
    pub last_screen_hash: u64,       // hash of screen to detect changes
    pub idle_since: Option<Instant>, // when idle prompt was first seen (for 500ms debounce)
    // Scrollback (dispatch-ct2.4): lines scrolled back from bottom
    pub scroll_offset: usize,
}

impl SlotState {
    pub fn display_name(&self) -> &str {
        self.custom_name.as_deref().unwrap_or(&self.callsign)
    }
}

/// A task ready to be dispatched (dispatch-bgz.11).
#[derive(Clone)]
pub struct QueuedTask {
    pub id: String,
    pub title: String,
}

/// A task entry for the task list overlay (dispatch-1lc.4).
#[derive(Clone)]
pub struct TaskEntry {
    pub id: String,
    pub title: String,
    pub status: String,        // "open", "in_progress", "closed"
    pub agent: Option<String>, // agent display name if currently in a slot
    pub deps: Vec<String>,     // dependency IDs from -> arrows (dispatch-1lc.3)
}

/// A recorded prompt for the history log (dispatch-ct2.8).
#[derive(Clone)]
pub struct PromptEntry {
    pub time: String,
    pub source: PromptSource,
    pub target: String,
    pub text: String,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum PromptSource {
    Voice,
    Keyboard,
}

pub struct App {
    pub slots: [Option<SlotState>; MAX_SLOTS],
    pub current_page: usize,
    /// 0-indexed into the current page's 4 visible slots.
    pub target: usize,
    pub mode: Mode,
    pub esc_exit_time: Option<Instant>,
    pub radio_state: RadioState,
    pub psk: String,
    pub port: u16,
    pub psk_expanded: bool,
    pub overlay: Overlay,
    /// Shared input buffer for DispatchSlot and Rename overlays.
    pub input_buf: String,
    pub queued_tasks: Vec<QueuedTask>,
    pub ws_state: crate::ws_server::SharedState,
    pub pane_rows: u16,
    pub pane_cols: u16,
    pub tools: std::collections::HashMap<String, String>,
    pub completion_timeout: std::time::Duration,
    // Ticker (dispatch-ami): LED-style scrolling marquee
    pub ticker_queue: std::collections::VecDeque<String>,
    pub ticker_current: String,
    pub ticker_offset: usize,
    pub ticker_frame_counter: u8,
    /// Task IDs with unresolved merge conflicts (dispatch-xje).
    pub conflict_tasks: Vec<String>,
    /// Absolute path to the target repo root (dispatch-xje).
    /// In single-repo mode: the git repo root. In multi-repo mode: the parent directory.
    pub repo_root: String,
    /// Workspace mode: single-repo or multi-repo (dispatch-sa1).
    pub workspace: Workspace,
    /// Currently highlighted repo in the RepoSelect overlay (dispatch-sa1).
    pub repo_select_idx: usize,
    // Task list overlay cache (dispatch-1lc.4): loaded when overlay opens
    pub task_list_data: Vec<TaskEntry>,
    // Scrollback config (dispatch-ct2.4)
    pub scrollback_lines: u32,
    // Orchestrator log view (dispatch-6nm)
    pub view_mode: ViewMode,
    pub orch_log: Vec<OrchestratorEvent>,
    pub orch_scroll: usize, // scroll offset from bottom
    // TLS cert fingerprint for QR pairing (dispatch-ct2.6)
    pub tls_fingerprint: String,
    // Prompt history and logging (dispatch-ct2.8)
    pub prompt_history: Vec<PromptEntry>,
    pub input_line_buf: String,       // shadow buffer tracking keyboard input in input mode
    pub history_scroll: usize,        // selected index in the prompt history overlay
    // Persistent LLM orchestrator (dispatch-h62)
    pub orchestrator: Option<orchestrator::Orchestrator>,
    // dispatch-guj: voice messages received before orchestrator is ready.
    pub pending_voice: Vec<String>,
    // Broadcast channel for pushing chat messages to radio clients (dispatch-chat)
    pub chat_tx: tokio::sync::broadcast::Sender<String>,
}
