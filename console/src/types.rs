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

pub const NATO: &[&str] = &[
    "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT", "GOLF", "HOTEL", "INDIA", "JULIET",
    "KILO", "LIMA", "MIKE", "NOVEMBER", "OSCAR", "PAPA", "QUEBEC", "ROMEO", "SIERRA", "TANGO",
    "UNIFORM", "VICTOR", "WHISKEY", "X-RAY", "YANKEE", "ZULU",
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
    /// Task assigned to an agent slot.
    TaskAssigned { id: String, agent: String, slot: usize },
    /// Task completed.
    TaskComplete { id: String, agent: String },
    /// Worktree merged successfully.
    Merged { id: String },
    /// Merge conflict.
    MergeConflict { id: String },
    /// Agent dispatched into a slot.
    Dispatched { agent: String, slot: usize, tool: String },
    /// Agent terminated.
    Terminated { agent: String, slot: usize },
    /// Orchestrator reasoning text.
    OrchestratorText { text: String },
    /// Tool call issued by orchestrator.
    ToolCallIssued { name: String },
    /// Tool result sent back to orchestrator.
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

/// Active overlay.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Overlay {
    None,
    Help,
    ConnectionInfo,
    ConfirmQuit,
    ConfirmTerminate,
    RepoSelect,
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
    #[allow(dead_code)]
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
    pub last_output_at: Instant,     // when screen content last changed
    // Scrollback (dispatch-ct2.4): lines scrolled back from bottom
    pub scroll_offset: usize,
}

impl SlotState {
    pub fn display_name(&self) -> &str {
        self.custom_name.as_deref().unwrap_or(&self.callsign)
    }
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
    /// Shared input buffer for overlays.
    pub ws_state: crate::ws_server::SharedState,
    pub pane_rows: u16,
    pub pane_cols: u16,
    pub tools: std::collections::HashMap<String, String>,
    // Ticker: LED-style scrolling marquee
    pub ticker_queue: std::collections::VecDeque<String>,
    pub ticker_current: String,
    pub ticker_offset: usize,
    pub ticker_frame_counter: u8,
    /// Workspace mode: single-repo or multi-repo.
    pub workspace: Workspace,
    /// Currently highlighted repo in the RepoSelect overlay.
    pub repo_select_idx: usize,
    // Scrollback config
    pub scrollback_lines: u32,
    // Orchestrator log view
    pub view_mode: ViewMode,
    pub orch_log: Vec<OrchestratorEvent>,
    pub orch_scroll: usize, // scroll offset from bottom
    // Persistent LLM orchestrator
    pub orchestrator: Option<orchestrator::Orchestrator>,
    // Voice messages received before orchestrator is ready.
    pub pending_voice: Vec<String>,
    // Broadcast channel for pushing chat messages to radio clients
    pub chat_tx: tokio::sync::broadcast::Sender<String>,
}
