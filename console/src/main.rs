// dispatch: Console TUI for Dispatch
//
// dispatch-e0k.1: PTY with claude via portable-pty + vt100 + ratatui
// dispatch-e0k.2: keyboard input forwarding to PTY
// dispatch-e0k.3: bd create integration from Rust
// dispatch-bgz.4: modal input model (command mode / input mode)
// dispatch-bgz.9: beads task lifecycle (create, assign, close, reopen)
// dispatch-bgz.10: pane info strip and header bar
// dispatch-bgz.12: config file and CLI subcommands
//
// Layout:
//   Header bar  : DISPATCH title, radio state, PSK, agent count, PAGE X/Y, clock
//   Quad pane   : 2x2 grid; each pane has info strip + terminal area
//   Footer bar  : mode indicator, target, navigation hints

mod config;

use clap::{Parser, Subcommand};
use std::{
    io::{self, Read, Write},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
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
    widgets::{Block, Borders, Paragraph},
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

struct App {
    /// Four visible slots for the current page. None = empty.
    slots: [Option<AgentSlot>; 4],
    current_page: usize,
    total_pages: usize,
    /// 0-indexed into slots[] for the currently targeted pane.
    target: usize,
    mode: Mode,
    /// Whether the previous key in Input mode was Escape (for double-Escape passthrough).
    last_was_escape: bool,
    radio_state: RadioState,
    psk: String,
    psk_expanded: bool,
    active_count: usize,
    max_agents: usize,
}

impl App {
    fn new(psk: String, max_agents: usize, task_id: Option<String>) -> Self {
        let wall = Local::now().format("%H:%M").to_string();
        let alpha = AgentSlot {
            callsign: NATO[0].to_string(),
            tool: "claude-code".to_string(),
            task_id,
            dispatch_time: Instant::now(),
            dispatch_wall_str: wall,
        };
        App {
            slots: [Some(alpha), None, None, None],
            current_page: 0,
            total_pages: 1,
            target: 0,
            mode: Mode::Command,
            last_was_escape: false,
            radio_state: RadioState::Disconnected,
            psk,
            psk_expanded: false,
            active_count: 1,
            max_agents,
        }
    }

    fn slot_number(&self, idx: usize) -> usize {
        self.current_page * 4 + idx + 1
    }

    fn psk_display(&self) -> String {
        if self.psk_expanded || self.psk.len() <= 4 {
            self.psk.clone()
        } else {
            format!("{}...", &self.psk[..4])
        }
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
fn pane_info_strip(slot_idx: usize, app: &App) -> Text<'static> {
    let slot_num = app.slot_number(slot_idx);
    let is_target = app.target == slot_idx;

    let marker_str = if is_target { "▸ " } else { "  " };
    let marker_style = if is_target {
        match app.mode {
            Mode::Command => Style::default().fg(Color::Cyan),
            Mode::Input => Style::default().fg(Color::Green),
        }
    } else {
        Style::default()
    };

    match &app.slots[slot_idx] {
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

fn render_pane(
    f: &mut Frame,
    area: Rect,
    slot_idx: usize,
    app: &App,
    vt_lines: Option<Vec<Line<'static>>>,
) {
    let is_target = app.target == slot_idx;
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

    // Split inner: 4 lines for info strip, rest for terminal
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(inner);

    let info = pane_info_strip(slot_idx, app);
    f.render_widget(Paragraph::new(info), chunks[0]);

    if let Some(lines) = vt_lines {
        f.render_widget(Paragraph::new(Text::from(lines)), chunks[1]);
    }
}

fn render_panes(f: &mut Frame, area: Rect, app: &App, vt_lines: Vec<Line<'static>>) {
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

    // top-left=slot0, top-right=slot1, bottom-left=slot2, bottom-right=slot3
    render_pane(f, left_rows[0], 0, app, Some(vt_lines));
    render_pane(f, right_rows[0], 1, app, None);
    render_pane(f, left_rows[1], 2, app, None);
    render_pane(f, right_rows[1], 3, app, None);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let target_callsign = match &app.slots[app.target] {
        Some(a) => a.callsign.clone(),
        None => format!("[{}]", app.slot_number(app.target)),
    };

    let content = match app.mode {
        Mode::Command => {
            let radio_label = match app.radio_state {
                RadioState::Connected => "RADIO CONNECTED",
                RadioState::Disconnected => "RADIO IDLE",
            };
            Line::from(vec![
                Span::styled(" ▸ ", Style::default().fg(Color::Cyan)),
                Span::styled(radio_label, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(" │ TARGET: {} │ ", target_callsign),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    "i/Enter input │ 1-4 select │ p psk │ q quit",
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

    // Create the PTY for slot 0 (Alpha)
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: PTY_ROWS,
            cols: PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("failed to open PTY");

    let cmd = CommandBuilder::new("claude");
    let _child = pair
        .slave
        .spawn_command(cmd)
        .or_else(|_| {
            // Fallback: open a shell for testing without claude installed
            let shell = if cfg!(windows) { "cmd" } else { "bash" };
            pair.slave.spawn_command(CommandBuilder::new(shell))
        })
        .expect("failed to spawn process");

    // PTY reader thread: feed bytes into vt100 parser; signal when PTY closes
    let screen: Arc<Mutex<vt100::Parser>> = Arc::new(Mutex::new(vt100::Parser::new(
        PTY_ROWS,
        PTY_COLS,
        0, // scrollback
    )));
    let screen_writer = Arc::clone(&screen);
    let mut pty_reader = pair.master.try_clone_reader().expect("clone reader");
    let child_exited = Arc::new(AtomicBool::new(false));
    let child_exited_writer = Arc::clone(&child_exited);

    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    screen_writer.lock().unwrap().process(&buf[..n]);
                }
            }
        }
        child_exited_writer.store(true, Ordering::Relaxed);
    });

    // PTY writer (dispatch-e0k.2): keyboard input forwarding
    let mut pty_writer = pair.master.take_writer().expect("take writer");

    // 3. Deliver prefixed prompt to the agent
    // Format: "[Dispatch task {id}] {prompt_text}\r"
    if let Some(id) = &task_id {
        let prefixed = format!("[Dispatch task {id}] {PROMPT}\r");
        let _ = pty_writer.write_all(prefixed.as_bytes());
        let _ = pty_writer.flush();
    }

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
    );

    // Track exit reason to determine task close vs reopen (dispatch-bgz.9)
    let mut agent_terminated = false;

    loop {
        // Break if the child process exited (agent completed its work)
        if child_exited.load(Ordering::Relaxed) {
            break;
        }

        // Snapshot VT screen for slot 0 (Alpha's PTY)
        let vt_lines = {
            let parser = screen.lock().unwrap();
            screen_to_lines(parser.screen())
        };

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
            render_panes(f, chunks[1], &app, vt_lines.clone());
            render_footer(f, chunks[2], &app);
        })?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match app.mode {
                    // ----------------------------------------------------------------
                    // Input mode: keystrokes go to the PTY.
                    // Escape is the only key the console intercepts.
                    // Double-Escape passes one literal Escape byte to the PTY.
                    // ----------------------------------------------------------------
                    Mode::Input => {
                        if key.code == KeyCode::Esc {
                            if app.last_was_escape {
                                // Double-Escape: send one literal Escape to PTY, stay in Input mode.
                                if app.target == 0 {
                                    let _ = pty_writer.write_all(b"\x1b");
                                    let _ = pty_writer.flush();
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

                        // Forward to PTY only for slot 0 (the only slot with a PTY in PoC)
                        if app.target == 0 {
                            let bytes = key_to_pty_bytes(&key);
                            if !bytes.is_empty() {
                                let _ = pty_writer.write_all(&bytes);
                                let _ = pty_writer.flush();
                            }
                        }
                    }

                    // ----------------------------------------------------------------
                    // Command mode: keystrokes control the console.
                    // ----------------------------------------------------------------
                    Mode::Command => match key.code {
                        KeyCode::Char('q') => {
                            agent_terminated = true;
                            break;
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

                        // Toggle PSK expansion
                        KeyCode::Char('p') => {
                            app.psk_expanded = !app.psk_expanded;
                        }

                        _ => {}
                    },
                }
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
