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
// dispatch-1lc.4: task list overlay — full-screen plan view with status groups and agent assignments
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
mod protocol;
mod ws_server;

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
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
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

/// Active overlay (dispatch-bgz.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Overlay {
    None,
    Help,
    TaskList,
    ConfirmQuit,
    ConfirmTerminate,
    DispatchSlot,
    Rename,
}

#[derive(Clone, Copy)]
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
    worktree_path: Option<String>, // git worktree path for this task (dispatch-xje)
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
    status: String,      // "open", "in_progress", "closed"
    agent: Option<String>, // agent display name if currently in a slot
}

struct App {
    slots: [Option<SlotState>; MAX_SLOTS],
    current_page: usize,
    /// 0-indexed into the current page's 4 visible slots.
    target: usize,
    mode: Mode,
    last_was_escape: bool,
    radio_state: RadioState,
    psk: String,
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
    repo_root: String,
    // Task list overlay cache (dispatch-1lc.4): loaded when overlay opens
    task_list_data: Vec<TaskEntry>,
}

impl App {
    fn new(
        psk: String,
        ws_state: ws_server::SharedState,
        pane_rows: u16,
        pane_cols: u16,
        tools: std::collections::HashMap<String, String>,
        completion_timeout: Duration,
        repo_root: String,
    ) -> Self {
        App {
            slots: std::array::from_fn(|_| None),
            current_page: 0,
            target: 0,
            mode: Mode::Command,
            last_was_escape: false,
            radio_state: RadioState::Disconnected,
            psk,
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
            task_list_data: Vec::new(),
        }
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
        if self.psk_expanded || self.psk.len() <= 4 {
            self.psk.clone()
        } else {
            format!("{}...", &self.psk[..4])
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

    fn ws_target_callsign(&self) -> Option<String> {
        let st = self.ws_state.lock().ok()?;
        let slot = st.target?;
        let idx = (slot as usize).saturating_sub(1);
        st.slots.get(idx)?.as_ref().map(|a| a.callsign.clone())
    }

    fn tool_cmd(&self, tool_key: &str) -> &str {
        self.tools
            .get(tool_key)
            .map(|s| s.as_str())
            .unwrap_or("claude")
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
}

// ── PTY helpers (dispatch-bgz.2, dispatch-bgz.6) ──────────────────────────────

/// Open a PTY and spawn a process. Returns a SlotState on success.
/// `cwd` sets the working directory for the PTY (dispatch-xje: worktree path).
fn dispatch_slot(
    global_idx: usize,
    tool_key: &str,
    tool_cmd: &str,
    pane_rows: u16,
    pane_cols: u16,
    cwd: Option<&str>,
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

    let screen = Arc::new(Mutex::new(vt100::Parser::new(pane_rows, pane_cols, 0)));
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
        worktree_path: None,
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

fn bd_create_task(prompt: &str) -> Option<String> {
    let output = Command::new("bd")
        .args(["create", prompt, "-t", "task", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v.get(0)?.get("id")?.as_str().map(|s| s.to_owned())
}

fn bd_assign_task(id: &str, callsign: &str) -> bool {
    Command::new("bd")
        .args([
            "update", id, "--claim", "--assignee", callsign,
            "--status", "in_progress", "--json",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bd_close_task(id: &str) -> bool {
    Command::new("bd")
        .args(["close", id, "--reason", "Completed", "--json"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bd_reopen_task(id: &str) -> bool {
    Command::new("bd")
        .args(["update", id, "--status", "open", "--json"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn bd_fetch_queued() -> Vec<QueuedTask> {
    let output = match Command::new("bd").args(["ready", "--json"]).output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };
    if !output.status.success() {
        return vec![];
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?.to_owned();
            let title = item.get("title")?.as_str()?.to_owned();
            Some(QueuedTask { id, title })
        })
        .collect()
}

/// Fetch all tasks for the task list overlay (dispatch-1lc.4).
/// Cross-references with active agent slots to annotate in-progress tasks.
fn bd_fetch_task_list(slots: &[Option<SlotState>; MAX_SLOTS]) -> Vec<TaskEntry> {
    // Build a map of task_id -> agent display name from active slots.
    let mut slot_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for slot in slots.iter().flatten() {
        if let Some(id) = &slot.task_id {
            slot_map.insert(id.clone(), slot.display_name().to_string());
        }
    }

    let output = match Command::new("bd").args(["list", "--json"]).output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };
    if !output.status.success() {
        return vec![];
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    v.as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?.to_owned();
            let title = item.get("title")?.as_str()?.to_owned();
            let status = item.get("status")?.as_str()?.to_owned();
            let agent = slot_map.get(&id).cloned();
            Some(TaskEntry { id, title, status, agent })
        })
        .collect()
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

// ── worktree helpers (dispatch-xje) ───────────────────────────────────────────

/// Create a git worktree for `task_id` at `.dispatch/.worktrees/{task_id}` on
/// branch `task/{task_id}`. Returns the absolute worktree path on success.
/// If the worktree already exists (task reassignment), returns the existing path.
fn create_worktree(task_id: &str, repo_root: &str) -> Option<String> {
    let worktrees_dir = format!("{}/.dispatch/.worktrees", repo_root);
    let worktree_path = format!("{}/{}", worktrees_dir, task_id);
    let branch = format!("task/{}", task_id);

    let _ = std::fs::create_dir_all(&worktrees_dir);

    // Reuse existing worktree on reassignment.
    if std::path::Path::new(&worktree_path).exists() {
        return Some(worktree_path);
    }

    let status = Command::new("git")
        .args(["worktree", "add", &worktree_path, "-b", &branch, "HEAD"])
        .current_dir(repo_root)
        .status()
        .ok()?;

    if status.success() { Some(worktree_path) } else { None }
}

/// Merge `task/{task_id}` into the current branch. On success, removes the
/// worktree and branch and returns true. On conflict, aborts and returns false.
fn merge_worktree(task_id: &str, repo_root: &str) -> bool {
    let branch = format!("task/{}", task_id);
    let worktree_path = format!("{}/.dispatch/.worktrees/{}", repo_root, task_id);

    let merged = Command::new("git")
        .args(["merge", &branch, "--no-ff", "-m", &format!("merge {}", branch)])
        .current_dir(repo_root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if merged {
        let _ = Command::new("git")
            .args(["worktree", "remove", &worktree_path, "--force"])
            .current_dir(repo_root)
            .status();
        let _ = Command::new("git")
            .args(["branch", "-d", &branch])
            .current_dir(repo_root)
            .status();
        true
    } else {
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(repo_root)
            .status();
        false
    }
}

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
    let right = format!(
        "   PSK: {}   AGENTS: {}/{}  PAGE {}/{}  {}",
        app.psk_display(),
        app.active_count(),
        app.slots.len(),
        app.current_page + 1,
        app.total_pages(),
        clock,
    );

    let status_line = Line::from(vec![
        Span::raw(" RADIO: "),
        radio_span,
        Span::styled(right, Style::default().fg(Color::White)),
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
    } else {
        f.render_widget(Paragraph::new(standby_body(global_idx, app)), chunks[1]);
    }
}

/// Render the 2×2 quad pane for the current page (dispatch-bgz.1).
fn render_panes(f: &mut Frame, area: Rect, app: &App) {
    let page_start = app.current_page * SLOTS_PER_PAGE;

    // Pre-compute vt lines for each visible slot (hold locks briefly, then release).
    let mut page_lines: [Option<Vec<Line<'static>>>; SLOTS_PER_PAGE] =
        [None, None, None, None];
    for local in 0..SLOTS_PER_PAGE {
        let g = page_start + local;
        if g < MAX_SLOTS {
            if let Some(slot) = &app.slots[g] {
                let parser = slot.screen.lock().unwrap();
                page_lines[local] = Some(screen_to_lines(parser.screen()));
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
            render_pane(f, areas[local], local, g, app, page_lines[local].take());
        }
    }
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
            let radio_label = match app.radio_state {
                RadioState::Connected => "RADIO CONNECTED",
                RadioState::Disconnected => "RADIO IDLE",
            };
            let ws_target_str = match app.ws_target_callsign() {
                Some(cs) => format!(" │ WS→{} ", cs),
                None => String::new(),
            };
            Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(Color::Cyan)),
                Span::styled(radio_label, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(" │ TARGET: {} ", target_name),
                    Style::default().fg(Color::White),
                ),
                Span::styled(ws_target_str, Style::default().fg(Color::Cyan)),
                Span::styled(
                    "│ i/Enter input │ 1-4 slot │ Tab cycle │ ]/[ page │ n/N dispatch │ x term │ R rename │ t tasks │ p psk │ q quit │ ? help",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
        Mode::Input => Line::from(vec![
            Span::styled(
                format!(" -- INPUT ({}) --", target_name),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "                              ESC exit │ ESC ESC send Esc to PTY",
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
    let r = centered_rect(52, 22, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::from(Span::styled(
            " COMMAND MODE KEYS ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(Span::raw("  Enter / i    Enter input mode")),
        Line::from(Span::raw("  1-4          Select slot on current page")),
        Line::from(Span::raw("  Tab          Next slot (all pages)")),
        Line::from(Span::raw("  Shift+Tab    Prev slot (all pages)")),
        Line::from(Span::raw("  ] / Shift+→  Next page")),
        Line::from(Span::raw("  [ / Shift+←  Prev page")),
        Line::from(Span::raw("  n            Dispatch into first empty slot")),
        Line::from(Span::raw("  N            Dispatch into specific slot")),
        Line::from(Span::raw("  x            Terminate target agent")),
        Line::from(Span::raw("  R            Rename target agent")),
        Line::from(Span::raw("  t            Task list overlay")),
        Line::from(Span::raw("  p            Toggle PSK visibility")),
        Line::from(Span::raw("  q            Quit (confirms if agents running)")),
        Line::from(Span::raw("  ?            This help screen")),
        Line::default(),
        Line::from(Span::styled(
            "  INPUT MODE",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw("  Esc          Return to command mode")),
        Line::from(Span::raw("  Esc Esc      Send literal Escape to PTY")),
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
            let prefix = format!("[ ] {}  ", t.id);
            let avail = inner_w.saturating_sub(prefix.len());
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
    // 3-row header + 1-row ticker + 1-row footer = 5 fixed rows; remaining split 2 ways vertically.
    // Each pane: 2 border rows + 4 info strip rows = 6 overhead.
    let pane_h = term_rows.saturating_sub(5) / 2;
    let rows = pane_h.saturating_sub(6).max(10);
    // Each pane is half the terminal width minus 2 for borders.
    let cols = (term_cols / 2).saturating_sub(2).max(20);
    (rows, cols)
}

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

    // Start the WebSocket server (dispatch-bgz.7).
    let ws_state: ws_server::SharedState = Arc::new(Mutex::new(ws_server::ConsoleState::new()));
    {
        let state = Arc::clone(&ws_state);
        let psk = cfg.auth.psk.clone();
        let port = cfg.server.port;
        thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .expect("tokio runtime")
                .block_on(ws_server::run_server(state, port, psk));
        });
    }

    // Determine initial pane size from the terminal.
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((160, 40));
    let (pane_rows, pane_cols) = compute_pane_size(term_rows, term_cols);

    let completion_timeout = Duration::from_secs(cfg.beads.completion_timeout_secs as u64);

    // Resolve repo root for worktree operations (dispatch-xje).
    let repo_root = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
        } else {
            None
        })
        .unwrap_or_else(|| ".".to_string());

    let mut app = App::new(
        cfg.auth.psk.clone(),
        ws_state,
        pane_rows,
        pane_cols,
        cfg.tools.clone(),
        completion_timeout,
        repo_root.clone(),
    );

    // Dispatch slot 0 (Alpha) with claude on startup (dispatch-bgz.6).
    // dispatch-xje: create task and worktree before spawning PTY so cwd is set.
    const PROMPT: &str = "PoC session: validate PTY + vt100 + ratatui";
    let startup_task_id = bd_create_task(PROMPT);
    let startup_worktree = startup_task_id
        .as_deref()
        .and_then(|id| create_worktree(id, &repo_root));
    let claude_cmd = app.tool_cmd("claude-code").to_string();
    if let Some(mut slot) = dispatch_slot(
        0, "claude-code", &claude_cmd, pane_rows, pane_cols,
        startup_worktree.as_deref(),
    ) {
        // dispatch-bgz.9: beads task lifecycle on startup
        if let Some(id) = &startup_task_id {
            bd_assign_task(id, &slot.callsign);
            let prefixed = format!("[Dispatch task {id}] {PROMPT}\r");
            let _ = slot.writer.write_all(prefixed.as_bytes());
            let _ = slot.writer.flush();
            slot.task_id = startup_task_id;
            slot.worktree_path = startup_worktree;
        }
        let name = slot.display_name().to_string();
        app.push_ticker(format!("DISPATCH: {} launched in slot 1 — claude-code ready", name));
        app.slots[0] = Some(slot);
    }

    // Channel for WsEvents from the WebSocket thread (dispatch-1lc.1).
    let (ws_event_tx, ws_event_rx) = mpsc::channel::<ws_server::WsEvent>();
    {
        let mut st = app.ws_state.lock().unwrap();
        st.event_tx = Some(ws_event_tx);
    }

    // Background thread: fetch queued tasks every TASK_POLL_SECS (dispatch-bgz.11).
    let (tasks_tx, tasks_rx) = mpsc::channel::<Vec<QueuedTask>>();
    thread::spawn(move || loop {
        let _ = tasks_tx.send(bd_fetch_queued());
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
                    let worktree_path = s.worktree_path.clone();
                    app.slots[i] = None;
                    if let Some(id) = task_id {
                        // dispatch-xje: merge worktree before closing; flag on conflict.
                        if worktree_path.is_some() {
                            if merge_worktree(&id, &app.repo_root) {
                                app.push_ticker(format!("TASK COMPLETE: {} merged {} — slot {} now standby", callsign, id, i + 1));
                            } else {
                                app.conflict_tasks.push(id.clone());
                                app.push_ticker(format!("MERGE CONFLICT: {} in {} — resolve manually", id, callsign));
                            }
                        } else {
                            app.push_ticker(format!("TASK COMPLETE: {} closed {} — slot {} now standby", callsign, id, i + 1));
                        }
                        bd_close_task(&id);
                    } else {
                        app.push_ticker(format!("AGENT EXITED: {} (slot {}) — standby", callsign, i + 1));
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
            if let Some(slot) = app.slots[i].as_mut() {
                slot.task_id = None;
                slot.idle_since = None;
            }
            bd_close_task(&task_id);

            // Pick up next available queued task and assign it to the idle slot.
            let next = bd_fetch_queued().into_iter().next();
            if let Some(qt) = next {
                if let Some(slot) = app.slots[i].as_mut() {
                    let callsign = slot.callsign.clone();
                    if bd_assign_task(&qt.id, &callsign) {
                        let prompt = format!("[Dispatch task {}] {}\r", qt.id, qt.title);
                        let _ = slot.writer.write_all(prompt.as_bytes());
                        let _ = slot.writer.flush();
                        slot.task_id = Some(qt.id.clone());
                        slot.last_output_at = Instant::now();
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
                app.push_ticker(format!("PLANNER: {} new task{} queued — {} total ready", added, if added == 1 { "" } else { "s" }, new_count));
            }
            app.queued_tasks = tasks;
        }

        // Advance ticker animation each frame (dispatch-ami).
        app.tick_ticker();

        // Process auto-dispatch events from the WebSocket thread (dispatch-1lc.1).
        while let Ok(event) = ws_event_rx.try_recv() {
            match event {
                ws_server::WsEvent::AutoDispatch { slot, prompt } => {
                    let g = (slot as usize).saturating_sub(1);
                    if g < MAX_SLOTS {
                        // Spawn a PTY if the slot is empty.
                        if app.slots[g].is_none() {
                            let cmd = app.tool_cmd("claude-code").to_string();
                            if let Some(s) = dispatch_slot(
                                g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                            ) {
                                let name = s.display_name().to_string();
                                app.slots[g] = Some(s);
                                app.push_ticker(format!("AUTO-DISPATCH: {} launched in slot {}", name, g + 1));
                            }
                        }
                        let task_id = bd_create_task(&prompt);
                        let mut ticker_msg: Option<String> = None;
                        if let Some(slot_state) = app.slots[g].as_mut() {
                            if let Some(ref id) = task_id {
                                bd_assign_task(id, &slot_state.callsign);
                                let prefixed = format!("[Dispatch task {id}] {prompt}\r");
                                let _ = slot_state.writer.write_all(prefixed.as_bytes());
                                let _ = slot_state.writer.flush();
                                ticker_msg = Some(format!("AUTO-DISPATCH: task {} assigned to {}", id, slot_state.callsign));
                            } else {
                                let with_enter = format!("{prompt}\r");
                                let _ = slot_state.writer.write_all(with_enter.as_bytes());
                                let _ = slot_state.writer.flush();
                            }
                            slot_state.task_id = task_id;
                        }
                        if let Some(msg) = ticker_msg {
                            app.push_ticker(msg);
                        }
                    }
                }
                ws_server::WsEvent::QueueTask { prompt } => {
                    // Create an open beads task; it will surface in the next
                    // queued-task poll and can be picked up when a slot frees.
                    if let Some(id) = bd_create_task(&prompt) {
                        app.push_ticker(format!("QUEUED: all agents busy — task {} waiting", id));
                    }
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
            render_panes(f, chunks[2], &app);
            render_footer(f, chunks[3], &app);

            match app.overlay {
                Overlay::None => {}
                Overlay::Help => render_help_overlay(f, full),
                Overlay::TaskList => render_task_list_overlay(f, full, &app),
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

                Event::Key(key) => match app.mode {
                    // Input mode: keystrokes forwarded to targeted PTY (dispatch-bgz.4)
                    Mode::Input => {
                        if key.code == KeyCode::Esc {
                            if app.last_was_escape {
                                let target_g = app.target_global();
                                if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                    let _ = slot.writer.write_all(b"\x1b");
                                    let _ = slot.writer.flush();
                                }
                                app.last_was_escape = false;
                            } else {
                                app.last_was_escape = true;
                            }
                            continue 'main;
                        }

                        if app.last_was_escape {
                            app.mode = Mode::Command;
                            app.last_was_escape = false;
                            continue 'main;
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
                                Overlay::Help | Overlay::TaskList => {
                                    app.overlay = Overlay::None;
                                }

                                Overlay::ConfirmQuit => match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        if app.active_count() == 0 {
                                            break 'main;
                                        }
                                        for i in 0..MAX_SLOTS {
                                            if let Some(task_id) = terminate_slot(&mut app.slots[i]) {
                                                bd_reopen_task(&task_id);
                                            }
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
                                        if let Some(task_id) = terminate_slot(&mut app.slots[target_g]) {
                                            bd_reopen_task(&task_id);
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
                                                    let cmd = app.tool_cmd("claude-code").to_string();
                                                    if let Some(slot) = dispatch_slot(
                                                        g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                                                    ) {
                                                        let name = slot.display_name().to_string();
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

                                Overlay::None => unreachable!(),
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('q') => {
                                    if app.active_count() > 0 {
                                        app.overlay = Overlay::ConfirmQuit;
                                    } else {
                                        break 'main;
                                    }
                                }

                                KeyCode::Enter | KeyCode::Char('i') => {
                                    app.mode = Mode::Input;
                                    app.last_was_escape = false;
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

                                KeyCode::Char(']') => {
                                    let total = app.total_pages();
                                    if app.current_page + 1 < total {
                                        app.current_page += 1;
                                    }
                                }
                                KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                    let total = app.total_pages();
                                    if app.current_page + 1 < total {
                                        app.current_page += 1;
                                    }
                                }

                                KeyCode::Char('[') => {
                                    if app.current_page > 0 {
                                        app.current_page -= 1;
                                    }
                                }
                                KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                                    if app.current_page > 0 {
                                        app.current_page -= 1;
                                    }
                                }

                                // Dispatch into first empty slot (dispatch-bgz.6)
                                KeyCode::Char('n') => {
                                    if let Some(g) = app.slots.iter().position(|s| s.is_none()) {
                                        let cmd = app.tool_cmd("claude-code").to_string();
                                        if let Some(slot) = dispatch_slot(
                                            g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                                        ) {
                                            let page = g / SLOTS_PER_PAGE;
                                            let local = g % SLOTS_PER_PAGE;
                                            let name = slot.display_name().to_string();
                                            app.push_ticker(format!("DISPATCH: {} launched in slot {}", name, g + 1));
                                            app.slots[g] = Some(slot);
                                            app.current_page = page;
                                            app.target = local;
                                        }
                                    }
                                }

                                KeyCode::Char('N') => {
                                    app.input_buf.clear();
                                    app.overlay = Overlay::DispatchSlot;
                                }

                                // Terminate target agent (dispatch-bgz.6)
                                KeyCode::Char('x') => {
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
                                    app.task_list_data = bd_fetch_task_list(&app.slots);
                                    app.overlay = Overlay::TaskList;
                                }
                                KeyCode::Char('p') => app.psk_expanded = !app.psk_expanded,
                                KeyCode::Char('?') => app.overlay = Overlay::Help,

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
