// dispatch: Console TUI for Dispatch
//
// dispatch-bgz.2: embedded terminal per slot (portable-pty + vt100)
// dispatch-e0k.1: PTY with claude via portable-pty + vt100 + ratatui
// dispatch-e0k.2: keyboard input forwarding to PTY
// dispatch-e0k.3: bd create integration from Rust
// dispatch-bgz.4: modal input model (command mode / input mode)
// dispatch-bgz.5: full command mode keybindings
// dispatch-bgz.8: WebSocket protocol (ws_server + protocol modules)
// dispatch-bgz.9: beads task lifecycle (create, assign, close, reopen)
// dispatch-bgz.10: pane info strip and header bar
// dispatch-bgz.11: standby pane (empty slot display + queued task list)
// dispatch-bgz.12: config file and CLI subcommands
// dispatch-bgz.1: quad-pane TUI layout with multi-page support
//
// Layout:
//   Header bar  : DISPATCH title, radio state, PSK, agent count, PAGE X/Y, clock
//   Quad pane   : 2x2 grid; each pane has info strip + terminal area
//   Footer bar  : mode indicator, target, navigation hints

mod config;
mod protocol;
mod ws_server;

use clap::{Parser, Subcommand};
use std::{
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
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
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

const PTY_ROWS: u16 = 20;
const PTY_COLS: u16 = 80;
const TASK_POLL_SECS: u64 = 5;

const NATO: &[&str] = &[
    "ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT", "GOLF", "HOTEL", "INDIA", "JULIET",
    "KILO", "LIMA", "MIKE", "NOVEMBER", "OSCAR", "PAPA", "QUEBEC", "ROMEO", "SIERRA", "TANGO",
    "UNIFORM", "VICTOR", "WHISKEY", "X-RAY", "YANKEE", "ZULU",
];

// ── types ─────────────────────────────────────────────────────────────────────

/// Input mode for the console (dispatch-bgz.4).
///
/// - `Command`: default mode; keystrokes control the console (navigation, quit, etc.).
/// - `Input`: keystrokes are forwarded directly to the active PTY.
///   The only key the console intercepts in this mode is Escape.
///   A double-Escape sends one literal Escape byte to the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Command,
    Input,
}

/// Active overlay shown on top of the quad pane (dispatch-bgz.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Overlay {
    None,
    Help,
    TaskList,
    ConfirmQuit,
    ConfirmTerminate,
    DispatchSlot, // for 'N' -- dispatch into specific slot
}

#[derive(Clone, Copy)]
#[allow(dead_code)] // Connected variant used when WebSocket is wired up (dispatch-bgz.7)
enum RadioState {
    Connected,
    Disconnected,
}

struct AgentSlot {
    callsign: String,
    tool: String,
    task_id: Option<String>,
    dispatch_time: Instant,
    dispatch_wall_str: String, // "14:20"
}

/// A task ready to be dispatched, fetched from `bd ready --json` (dispatch-bgz.11).
#[derive(Clone)]
struct QueuedTask {
    id: String,
    title: String,
}

struct App {
    /// All agent slots (length = max_agents). Index = global slot number - 1.
    slots: Vec<Option<AgentSlot>>,
    current_page: usize,
    total_pages: usize,
    /// 0-indexed into the current page's 4 slots (0-3) for the targeted pane.
    target: usize,
    mode: Mode,
    /// Whether the previous key in Input mode was Escape (for double-Escape passthrough).
    last_was_escape: bool,
    radio_state: RadioState,
    psk: String,
    psk_expanded: bool,
    active_count: usize,
    max_agents: usize,
    /// Active overlay (dispatch-bgz.5).
    overlay: Overlay,
    /// Input buffer for the 'N' dispatch-into-slot prompt (dispatch-bgz.5).
    input_buf: String,
    /// Open/unblocked tasks from beads, displayed in the last standby pane (dispatch-bgz.11).
    queued_tasks: Vec<QueuedTask>,
    /// WebSocket protocol state shared with the WS server thread (dispatch-bgz.8).
    ws_state: ws_server::SharedState,
}

impl App {
    fn new(psk: String, max_agents: usize, task_id: Option<String>, ws_state: ws_server::SharedState) -> Self {
        let wall = Local::now().format("%H:%M").to_string();
        let alpha = AgentSlot {
            callsign: NATO[0].to_string(),
            tool: "claude-code".to_string(),
            task_id,
            dispatch_time: Instant::now(),
            dispatch_wall_str: wall,
        };
        let mut slots: Vec<Option<AgentSlot>> = (0..max_agents).map(|_| None).collect();
        slots[0] = Some(alpha);
        let total_pages = (max_agents + 3) / 4;
        App {
            slots,
            current_page: 0,
            total_pages,
            target: 0,
            mode: Mode::Command,
            last_was_escape: false,
            radio_state: RadioState::Disconnected,
            psk,
            psk_expanded: false,
            active_count: 1,
            max_agents,
            overlay: Overlay::None,
            input_buf: String::new(),
            queued_tasks: Vec::new(),
            ws_state,
        }
    }

    /// Returns the callsign of the WS-protocol-targeted agent (slot from last set_target message).
    fn ws_target_callsign(&self) -> Option<String> {
        let st = self.ws_state.lock().ok()?;
        let slot = st.target?;
        let idx = (slot as usize).saturating_sub(1);
        st.slots.get(idx)?.as_ref().map(|a| a.callsign.clone())
    }

    /// Global slot index for the currently targeted pane.
    fn global_target(&self) -> usize {
        self.current_page * 4 + self.target
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
        // Only applies to the last page
        if global_idx / 4 != self.total_pages - 1 {
            return false;
        }
        // No later slot on the last page is also empty
        for i in (global_idx + 1)..self.slots.len() {
            if self.slots[i].is_none() {
                return false;
            }
        }
        true
    }
}

// ── per-slot PTY (dispatch-bgz.2) ─────────────────────────────────────────────

/// One PTY per dispatched agent: master handle for resize, writer for input,
/// shared screen for render, and an exit flag set by the reader thread.
struct SlotPty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    screen: Arc<Mutex<vt100::Parser>>,
    child_exited: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
}

impl SlotPty {
    /// Open a PTY and spawn `cmd`; falls back to shell if `cmd` is not found.
    fn spawn(rows: u16, cols: u16, cmd: &str) -> Option<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .ok()?;

        let _ = pair
            .slave
            .spawn_command(CommandBuilder::new(cmd))
            .or_else(|_| {
                let shell = if cfg!(windows) { "cmd" } else { "bash" };
                pair.slave.spawn_command(CommandBuilder::new(shell))
            })
            .ok()?;

        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let screen_w = Arc::clone(&screen);
        let mut reader = pair.master.try_clone_reader().ok()?;
        let exited = Arc::new(AtomicBool::new(false));
        let exited_w = Arc::clone(&exited);

        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        screen_w.lock().unwrap().process(&buf[..n]);
                    }
                }
            }
            exited_w.store(true, Ordering::Relaxed);
        });

        let writer = pair.master.take_writer().ok()?;
        let master = pair.master;
        Some(SlotPty { master, writer, screen, child_exited: exited, rows, cols })
    }

    fn screen_lines(&self) -> Vec<Line<'static>> {
        let parser = self.screen.lock().unwrap();
        screen_to_lines(parser.screen())
    }

    fn is_exited(&self) -> bool {
        self.child_exited.load(Ordering::Relaxed)
    }

    /// Resize the PTY and update the vt100 parser dimensions.
    fn resize(&mut self, rows: u16, cols: u16) {
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.screen.lock().unwrap().set_size(rows, cols);
        self.rows = rows;
        self.cols = cols;
    }

    fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn format_runtime(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}m{:02}s", s / 60, s % 60)
}

/// Run `bd create "{prompt}" -t task --json` and return the task ID.
fn bd_create_task(prompt: &str) -> Option<String> {
    let output = Command::new("bd")
        .args(["create", prompt, "-t", "task", "--json"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // bd returns a JSON array; extract the first id field
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    v.get(0)?.get("id")?.as_str().map(|s| s.to_owned())
}

/// Run `bd update {id} --claim --assignee {callsign} --status in_progress --json`.
fn bd_assign_task(id: &str, callsign: &str) -> bool {
    Command::new("bd")
        .args([
            "update",
            id,
            "--claim",
            "--assignee",
            callsign,
            "--status",
            "in_progress",
            "--json",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `bd close {id} --reason "Completed" --json`.
fn bd_close_task(id: &str) -> bool {
    Command::new("bd")
        .args(["close", id, "--reason", "Completed", "--json"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `bd update {id} --status open --json` to reopen an abandoned task.
fn bd_reopen_task(id: &str) -> bool {
    Command::new("bd")
        .args(["update", id, "--status", "open", "--json"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `bd ready --json` and return open/unblocked tasks for the queue display (dispatch-bgz.11).
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

    let arr = match v.as_array() {
        Some(a) => a,
        None => return vec![],
    };

    arr.iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?.to_owned();
            let title = item.get("title")?.as_str()?.to_owned();
            Some(QueuedTask { id, title })
        })
        .collect()
}

/// Map a crossterm KeyEvent to the bytes that should be sent to the PTY.
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
                // Ctrl+A..Z -> 0x01..0x1a
                if c.is_ascii_alphabetic() {
                    let b = (c.to_ascii_lowercase() as u8) - b'a' + 1;
                    vec![b]
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

/// Convert a vt100 Cell color to ratatui Color.
fn vt100_color_to_ratatui(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Render the vt100 screen into ratatui Lines.
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
        app.active_count,
        app.max_agents,
        app.current_page + 1,
        app.total_pages,
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
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(status_line), inner);
}

/// Build the 4-line info strip for one pane.
/// `local_idx`: 0-3 position on current page; `global_idx`: index into app.slots.
fn pane_info_strip(local_idx: usize, global_idx: usize, app: &App) -> Text<'static> {
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
                    format!("[{}] {}", slot_num, agent.callsign),
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

/// Build the standby body lines for an empty pane (dispatch-bgz.11).
///
/// The last standby slot on the last page shows queued tasks; all others show dispatch shortcuts.
fn standby_body(global_idx: usize, app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    if app.is_last_standby(global_idx) {
        // Last standby on the last page: show queued task count and truncated titles
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
        // Regular standby slot: show dispatch shortcuts
        lines.push(Line::from(Span::styled(
            " Dispatch new agent:",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  [c] claude-code",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  [g] gh copilot",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
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

    // Split inner: 4 lines for info strip, rest for terminal / standby content
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(inner);

    let info = pane_info_strip(local_idx, global_idx, app);
    f.render_widget(Paragraph::new(info), chunks[0]);

    if let Some(lines) = vt_lines {
        // Active pane: show PTY content
        f.render_widget(Paragraph::new(Text::from(lines)), chunks[1]);
    } else {
        // Standby pane: show dispatch shortcuts or queued task list (dispatch-bgz.11)
        let body = standby_body(global_idx, app);
        f.render_widget(Paragraph::new(body), chunks[1]);
    }
}

fn render_panes(
    f: &mut Frame,
    area: Rect,
    app: &App,
    vt_lines: Vec<Option<Vec<Line<'static>>>>,
) {
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

    // top-left=local0, top-right=local1, bottom-left=local2, bottom-right=local3
    // global_idx = current_page * 4 + local_idx (dispatch-bgz.2)
    let page_start = app.current_page * 4;
    let vl = |g: usize| vt_lines.get(g).and_then(|v| v.clone());
    render_pane(f, left_rows[0],  0, page_start,     app, vl(page_start));
    render_pane(f, right_rows[0], 1, page_start + 1, app, vl(page_start + 1));
    render_pane(f, left_rows[1],  2, page_start + 2, app, vl(page_start + 2));
    render_pane(f, right_rows[1], 3, page_start + 3, app, vl(page_start + 3));
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let global = app.global_target();
    let target_callsign = match &app.slots[global] {
        Some(a) => a.callsign.clone(),
        None => format!("[{}]", global + 1),
    };

    let content = match app.mode {
        Mode::Command => {
            let radio_label = match app.radio_state {
                RadioState::Connected => "RADIO CONNECTED",
                RadioState::Disconnected => "RADIO IDLE",
            };
            // Show WS radio target if set (dispatch-bgz.8)
            let ws_target_str = match app.ws_target_callsign() {
                Some(cs) => format!(" │ WS→{} ", cs),
                None => String::new(),
            };
            Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(Color::Cyan)),
                Span::styled(radio_label, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(" │ TARGET: {} ", target_callsign),
                    Style::default().fg(Color::White),
                ),
                Span::styled(ws_target_str, Style::default().fg(Color::Cyan)),
                Span::styled(
                    "│ i/Enter input │ 1-4 slot │ Tab cycle │ ]/[ page │ n/N dispatch │ x term │ t tasks │ p psk │ q quit │ ? help",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        }
        Mode::Input => Line::from(vec![
            Span::styled(
                format!(" -- INPUT ({}) --", target_callsign),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "                              ESC exit │ ESC ESC send Esc to PTY",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    };

    f.render_widget(Paragraph::new(content), area);
}

// ── overlay rendering (dispatch-bgz.5) ───────────────────────────────────────

/// Return a centered rect with the given absolute width and height.
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
    let block = Block::default()
        .title(" HELP ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(block),
        r,
    );
}

fn render_task_list_overlay(f: &mut Frame, area: Rect, app: &App) {
    // Show only active (non-empty) slots across all pages
    let active: Vec<(usize, &AgentSlot)> = app.slots.iter().enumerate()
        .filter_map(|(i, s)| s.as_ref().map(|a| (i, a)))
        .collect();
    let height = (active.len() + 4).max(6) as u16;
    let r = centered_rect(54, height, area);
    f.render_widget(Clear, r);
    let mut lines = vec![Line::default()];
    if active.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no active agents)",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for (global_idx, a) in &active {
        let slot_num = global_idx + 1;
        let task = a.task_id.as_deref().unwrap_or("no task");
        lines.push(Line::from(vec![
            Span::styled(
                format!("  [{}]  {}", slot_num, a.callsign),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", task),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  Press any key to close",
        Style::default().fg(Color::DarkGray),
    )));
    let block = Block::default()
        .title(" TASK LIST ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(block),
        r,
    );
}

fn render_confirm_overlay(f: &mut Frame, area: Rect, title: &str, body: &str) {
    let r = centered_rect(50, 7, area);
    f.render_widget(Clear, r);
    let lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  {}", body),
            Style::default().fg(Color::White),
        )),
        Line::default(),
        Line::from(vec![
            Span::styled("  y ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("confirm    ", Style::default().fg(Color::DarkGray)),
            Span::styled("n / Esc ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::DarkGray)),
        ]),
        Line::default(),
    ];
    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(block),
        r,
    );
}

fn render_dispatch_overlay(f: &mut Frame, area: Rect, app: &App) {
    let r = centered_rect(50, 7, area);
    f.render_widget(Clear, r);
    let total_slots = app.slots.len();
    let lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("  Slot number (1-{}):", total_slots),
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
    let block = Block::default()
        .title(" DISPATCH INTO SLOT ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(block),
        r,
    );
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::RegeneratePsk) => {
            let psk = config::regenerate_psk();
            println!("{psk}");
            return Ok(());
        }
        Some(Commands::ShowPsk) => {
            let cfg = config::load_or_create();
            println!("{}", cfg.auth.psk);
            return Ok(());
        }
        Some(Commands::Config) => {
            println!("{}", config::config_path().display());
            return Ok(());
        }
        None => {}
    }

    // Load (or create) config on startup.
    let cfg = config::load_or_create();

    // dispatch-bgz.8: Start the WebSocket server; share state with the App for UI reads.
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

    // dispatch-bgz.9: beads task lifecycle
    // Slot 1 callsign is Alpha by convention (NATO phonetic alphabet, dispatch order)
    const CALLSIGN: &str = "Alpha";
    const PROMPT: &str = "PoC session: validate PTY + vt100 + ratatui";

    // 1. Create a typed task
    let task_id = bd_create_task(PROMPT);

    // 2. Assign the task to this agent slot
    if let Some(id) = &task_id {
        bd_assign_task(id, CALLSIGN);
    }

    // dispatch-bgz.2: per-slot PTY Vec (one entry per slot in app.slots); slot 0 gets a PTY on startup
    let max_agents = cfg.terminal.max_agents as usize;
    let mut ptys: Vec<Option<SlotPty>> = (0..max_agents).map(|_| None).collect();
    ptys[0] = SlotPty::spawn(PTY_ROWS, PTY_COLS, "claude");

    // 3. Deliver prefixed prompt to slot 0's agent
    if let Some(id) = &task_id {
        if let Some(pty) = ptys[0].as_mut() {
            let prefixed = format!("[Dispatch task {id}] {PROMPT}\r");
            pty.write(prefixed.as_bytes());
        }
    }

    // Background thread: periodically fetch queued tasks from beads (dispatch-bgz.11)
    let (tasks_tx, tasks_rx) = mpsc::channel::<Vec<QueuedTask>>();
    thread::spawn(move || loop {
        let tasks = bd_fetch_queued();
        let _ = tasks_tx.send(tasks);
        thread::sleep(Duration::from_secs(TASK_POLL_SECS));
    });

    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(
        cfg.auth.psk.clone(),
        cfg.terminal.max_agents as usize,
        task_id.clone(),
        ws_state,
    );

    // Track exit reason to determine task close vs reopen (dispatch-bgz.9)
    let mut agent_terminated = false;

    loop {
        // dispatch-bgz.2: check for exited PTY slots; clear slot when done
        let mut slot0_exited = false;
        for i in 0..ptys.len() {
            if ptys[i].as_ref().map(|p| p.is_exited()).unwrap_or(false) {
                ptys[i] = None;
                app.slots[i] = None;
                if app.active_count > 0 {
                    app.active_count -= 1;
                }
                if i == 0 {
                    slot0_exited = true;
                }
            }
        }
        if slot0_exited {
            break;
        }

        // Pull latest queued tasks from background thread (non-blocking, dispatch-bgz.11)
        while let Ok(tasks) = tasks_rx.try_recv() {
            app.queued_tasks = tasks;
        }

        // dispatch-bgz.2: snapshot VT screen for each active slot
        let vt_lines: Vec<Option<Vec<Line<'static>>>> =
            ptys.iter().map(|p| p.as_ref().map(|p| p.screen_lines())).collect();

        terminal.draw(|f| {
            let full = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // header bar
                    Constraint::Min(0),    // quad pane
                    Constraint::Length(1), // footer bar
                ])
                .split(full);

            render_header(f, chunks[0], &app);
            render_panes(f, chunks[1], &app, vt_lines);
            render_footer(f, chunks[2], &app);

            // Overlays rendered on top of everything (dispatch-bgz.5)
            match app.overlay {
                Overlay::None => {}
                Overlay::Help => render_help_overlay(f, full),
                Overlay::TaskList => render_task_list_overlay(f, full, &app),
                Overlay::ConfirmQuit => render_confirm_overlay(
                    f,
                    full,
                    "QUIT",
                    "Agents are running. Really quit?",
                ),
                Overlay::ConfirmTerminate => {
                    let global = app.global_target();
                    let callsign = match &app.slots[global] {
                        Some(a) => a.callsign.clone(),
                        None => format!("slot {}", global + 1),
                    };
                    render_confirm_overlay(
                        f,
                        full,
                        "TERMINATE",
                        &format!("Terminate {}?", callsign),
                    );
                }
                Overlay::DispatchSlot => render_dispatch_overlay(f, full, &app),
            }
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                // ----------------------------------------------------------------
                // dispatch-bgz.2: resize all active PTYs when the terminal resizes
                // ----------------------------------------------------------------
                Event::Resize(cols, rows) => {
                    // Each pane is ~half the terminal width/height minus borders and info strip
                    let pane_cols = (cols / 2).saturating_sub(2).max(20);
                    let pane_rows = (rows.saturating_sub(4) / 2).saturating_sub(6).max(5);
                    for pty in ptys.iter_mut().flatten() {
                        pty.resize(pane_rows, pane_cols);
                    }
                }

                Event::Key(key) => match app.mode {
                    // ----------------------------------------------------------------
                    // Input mode: keystrokes go to the PTY.
                    // Escape is the only key the console intercepts.
                    // Double-Escape passes one literal Escape byte to the PTY.
                    // ----------------------------------------------------------------
                    Mode::Input => {
                        if key.code == KeyCode::Esc {
                            if app.last_was_escape {
                                // Double-Escape: send one literal Escape to PTY, stay in Input mode.
                                if let Some(pty) = ptys[app.global_target()].as_mut() {
                                    pty.write(b"\x1b");
                                }
                                app.last_was_escape = false;
                            } else {
                                // First Escape: wait to see if a second follows.
                                app.last_was_escape = true;
                            }
                            continue;
                        }

                        if app.last_was_escape {
                            // Single Escape then non-Escape: exit input mode.
                            app.mode = Mode::Command;
                            app.last_was_escape = false;
                            continue;
                        }

                        // dispatch-bgz.2: forward input to the targeted slot's PTY
                        if let Some(pty) = ptys[app.global_target()].as_mut() {
                            let bytes = key_to_pty_bytes(&key);
                            if !bytes.is_empty() {
                                pty.write(&bytes);
                            }
                        }
                    }

                    // ----------------------------------------------------------------
                    // Command mode: keystrokes control the console.
                    // ----------------------------------------------------------------
                    Mode::Command => {
                        // If an overlay is active, route keys to the overlay handler.
                        if app.overlay != Overlay::None {
                            match app.overlay {
                                Overlay::Help | Overlay::TaskList => {
                                    // Any key dismisses these overlays.
                                    app.overlay = Overlay::None;
                                }
                                Overlay::ConfirmQuit => match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        agent_terminated = true;
                                        break;
                                    }
                                    _ => {
                                        app.overlay = Overlay::None;
                                    }
                                },
                                Overlay::ConfirmTerminate => match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        // dispatch-bgz.2: drop the slot's PTY on terminate
                                        let global = app.global_target();
                                        ptys[global] = None;
                                        app.slots[global] = None;
                                        if app.active_count > 0 {
                                            app.active_count -= 1;
                                        }
                                        app.overlay = Overlay::None;
                                    }
                                    _ => {
                                        app.overlay = Overlay::None;
                                    }
                                },
                                Overlay::DispatchSlot => match key.code {
                                    KeyCode::Esc => {
                                        app.input_buf.clear();
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Backspace => {
                                        app.input_buf.pop();
                                    }
                                    KeyCode::Enter => {
                                        if let Ok(n) = app.input_buf.trim().parse::<usize>() {
                                            let total = app.slots.len();
                                            if n >= 1 && n <= total {
                                                let global = n - 1;
                                                let page = global / 4;
                                                let local_idx = global % 4;
                                                // Auto-navigate to the page and target slot
                                                app.current_page = page;
                                                app.target = local_idx;
                                                // Dispatch: fill slot if empty
                                                if app.slots[global].is_none() {
                                                    let callsign = NATO
                                                        .get(global % NATO.len())
                                                        .unwrap_or(&"AGENT");
                                                    let wall = Local::now().format("%H:%M").to_string();
                                                    app.slots[global] = Some(AgentSlot {
                                                        callsign: callsign.to_string(),
                                                        tool: "claude-code".to_string(),
                                                        task_id: None,
                                                        dispatch_time: Instant::now(),
                                                        dispatch_wall_str: wall,
                                                    });
                                                    app.active_count += 1;
                                                    // dispatch-bgz.2: spawn PTY for the new slot
                                                    ptys[global] = SlotPty::spawn(PTY_ROWS, PTY_COLS, "claude");
                                                }
                                            }
                                        }
                                        app.input_buf.clear();
                                        app.overlay = Overlay::None;
                                    }
                                    KeyCode::Char(c) if c.is_ascii_digit() => {
                                        app.input_buf.push(c);
                                    }
                                    _ => {}
                                },
                                Overlay::None => unreachable!(),
                            }
                        } else {
                            match key.code {
                                // Quit — confirm if any agents are running
                                KeyCode::Char('q') => {
                                    if app.active_count > 0 {
                                        app.overlay = Overlay::ConfirmQuit;
                                    } else {
                                        break;
                                    }
                                }

                                // Enter input mode
                                KeyCode::Enter | KeyCode::Char('i') => {
                                    app.mode = Mode::Input;
                                    app.last_was_escape = false;
                                }

                                // Select target slot (1-4 on current page)
                                KeyCode::Char('1') => app.target = 0,
                                KeyCode::Char('2') => app.target = 1,
                                KeyCode::Char('3') => app.target = 2,
                                KeyCode::Char('4') => app.target = 3,

                                // Cycle target: Tab = next slot across all pages
                                KeyCode::Tab => {
                                    let total = app.total_pages * 4;
                                    let global = app.current_page * 4 + app.target;
                                    let next = (global + 1) % total;
                                    app.current_page = next / 4;
                                    app.target = next % 4;
                                }

                                // Cycle target: Shift+Tab = prev slot across all pages
                                KeyCode::BackTab => {
                                    let total = app.total_pages * 4;
                                    let global = app.current_page * 4 + app.target;
                                    let prev = (global + total - 1) % total;
                                    app.current_page = prev / 4;
                                    app.target = prev % 4;
                                }

                                // Page navigation: ] or Shift+Right
                                KeyCode::Char(']') => {
                                    if app.current_page + 1 < app.total_pages {
                                        app.current_page += 1;
                                    }
                                }
                                KeyCode::Right
                                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                                {
                                    if app.current_page + 1 < app.total_pages {
                                        app.current_page += 1;
                                    }
                                }

                                // Page navigation: [ or Shift+Left
                                KeyCode::Char('[') => {
                                    if app.current_page > 0 {
                                        app.current_page -= 1;
                                    }
                                }
                                KeyCode::Left
                                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                                {
                                    if app.current_page > 0 {
                                        app.current_page -= 1;
                                    }
                                }

                                // Dispatch into first empty slot (across all pages)
                                KeyCode::Char('n') => {
                                    if let Some(global) =
                                        app.slots.iter().position(|s| s.is_none())
                                    {
                                        let callsign = NATO
                                            .get(global % NATO.len())
                                            .unwrap_or(&"AGENT");
                                        let wall = Local::now().format("%H:%M").to_string();
                                        app.slots[global] = Some(AgentSlot {
                                            callsign: callsign.to_string(),
                                            tool: "claude-code".to_string(),
                                            task_id: None,
                                            dispatch_time: Instant::now(),
                                            dispatch_wall_str: wall,
                                        });
                                        app.active_count += 1;
                                        // Auto-navigate to the page containing the new slot
                                        app.current_page = global / 4;
                                        app.target = global % 4;
                                        // dispatch-bgz.2: spawn PTY for the new slot
                                        ptys[global] = SlotPty::spawn(PTY_ROWS, PTY_COLS, "claude");
                                    }
                                }

                                // Dispatch into specific slot (shows prompt)
                                KeyCode::Char('N') => {
                                    app.input_buf.clear();
                                    app.overlay = Overlay::DispatchSlot;
                                }

                                // Terminate target agent (confirm first)
                                KeyCode::Char('x') => {
                                    if app.slots[app.global_target()].is_some() {
                                        app.overlay = Overlay::ConfirmTerminate;
                                    }
                                }

                                // Rename target agent (stub — opens input mode for now)
                                KeyCode::Char('R') => {
                                    // Stub: rename not yet implemented
                                }

                                // Task list overlay
                                KeyCode::Char('t') => {
                                    app.overlay = Overlay::TaskList;
                                }

                                // Toggle PSK expansion
                                KeyCode::Char('p') => {
                                    app.psk_expanded = !app.psk_expanded;
                                }

                                // Help overlay
                                KeyCode::Char('?') => {
                                    app.overlay = Overlay::Help;
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

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // 5. Task lifecycle close/reopen based on exit reason (dispatch-bgz.9)
    if let Some(id) = &task_id {
        if agent_terminated {
            // Agent was killed by user — reopen task so another agent can pick it up
            bd_reopen_task(id);
            println!("Agent terminated. Task {id} reopened as open.");
        } else {
            // Agent's PTY closed naturally — mark task complete
            bd_close_task(id);
            println!("Session complete. Task {id} closed.");
        }
    }

    Ok(())
}
