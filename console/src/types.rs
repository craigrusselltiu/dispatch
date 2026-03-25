// Type definitions, enums, and constants for the dispatch console.

use std::{
    io::Write,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Instant,
};

use dispatch_core::{orchestrator, strike_team};

/// Slots per page (2x2 grid).
pub const SLOTS_PER_PAGE: usize = 4;

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
    /// Status message from an agent.
    AgentMessage { agent: String, text: String },
}

/// A single ticker message with its own scroll position.
pub struct TickerItem {
    pub text: String,
    pub char_count: usize,
    /// How many character positions it has scrolled from the right edge.
    pub offset: usize,
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
    pub callsign: String,            // dynamically assigned from pool
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
    pub last_output_at: Arc<Mutex<Instant>>,  // updated by PTY reader on output
    /// Shared writer for copilot agents (background thread typing). None for claude.
    pub shared_writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    pub idle: bool,                           // true when no output for idle threshold
    // Scrollback (dispatch-ct2.4): lines scrolled back from bottom
    pub scroll_offset: usize,
    // File-based agent messaging: absolute path to the message file and read offset.
    pub msg_file: String,
    pub msg_offset: u64,
}

impl SlotState {
    pub fn display_name(&self) -> &str {
        self.custom_name.as_deref().unwrap_or(&self.callsign)
    }
}

pub struct App {
    pub slots: Vec<Option<SlotState>>,
    /// Configured agent callsigns (drives slot count).
    pub callsigns: Vec<String>,
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
    /// Which tool key to use by default when dispatching agents (e.g. "claude" or "copilot").
    pub default_tool: String,
    /// How agents finalize work: "pr" (create pull request) or "merge" (merge to main).
    pub merge_strategy: String,
    // Ticker: LED-style scrolling marquee (independent items)
    pub ticker_items: Vec<TickerItem>,
    pub ticker_pending: std::collections::VecDeque<String>,
    pub ticker_frame_counter: u8,
    /// Workspace mode: single-repo or multi-repo.
    pub workspace: Workspace,
    /// Currently highlighted repo in the RepoSelect overlay.
    pub repo_select_idx: usize,
    // Scrollback config
    pub scrollback_lines: u32,
    // Orchestrator log view
    pub view_mode: ViewMode,
    pub orch_log: std::collections::VecDeque<OrchestratorEvent>,
    pub orch_scroll: usize, // scroll offset from bottom
    // Persistent LLM orchestrator
    pub orchestrator: Option<orchestrator::Orchestrator>,
    /// Error message if the orchestrator failed to spawn.
    pub orch_error: Option<String>,
    // Voice messages received before orchestrator is ready.
    pub pending_voice: Vec<String>,
    // Broadcast channel for pushing chat messages to radio clients
    pub chat_tx: tokio::sync::broadcast::Sender<String>,
    // Status indicator blink frame counter (pulsing REC-light effect)
    pub status_blink_frame: u16,
    /// Display name for the user (default: "Dispatch").
    pub user_callsign: String,
    /// Display name for the console/orchestrator (default: "Console").
    pub console_name: String,
    /// Active strike team state (None when no strike team is running).
    pub strike_team: Option<strike_team::StrikeTeamState>,
    /// Whether the current orchestrator turn was triggered by user voice/text input.
    /// Snapshotted before collecting orchestrator outputs each frame. Used to gate
    /// destructive actions (terminate) so the LLM cannot self-hallucinate user input
    /// and act on it.
    pub user_initiated_turn: bool,
    /// Dirty flag: set when something visual changed, cleared after draw.
    /// Skips terminal.draw() when clean to avoid 60fps idle CPU burn.
    pub needs_redraw: bool,
    /// Cached clock string (updated once per second, not every frame).
    pub cached_clock: String,
    /// Second value when cached_clock was last updated.
    pub cached_clock_second: u64,
    /// Frame counter for throttling periodic work (agent message polling).
    pub frame_counter: u32,
}
