// dispatch: Console TUI for Dispatch
//
// Quad-pane TUI with embedded terminals for AI coding agents.
// Orchestrator sends prompts to agents via `dispatch` tool; agents create
// their own git worktrees, work, commit, merge, and push.
//
// Layout:
//   Header bar  : DISPATCH title, radio state, PSK, agent count, PAGE X/Y, clock
//   Ticker bar  : single-line LED marquee scrolling right-to-left
//   Quad pane   : 2x2 grid; each pane has info strip + terminal area
//   Footer bar  : mode indicator, target, navigation hints
//
// Pages: slots 1-4 on page 1, 5-8 on page 2, etc. (max 26 slots / 7 pages).
// All PTYs run regardless of visible page. Each slot owns its own PTY.

mod app;
mod config;
mod mdns;
mod pty;
mod types;
mod ui;
mod util;
mod ws_server;

use dispatch_core::{orchestrator, tools};

use clap::{Parser, Subcommand};
use std::{
    io::{self, Write},
    process::Command,
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::PtySize;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::Clear,
    Terminal,
};

use types::*;

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

// ── main ──────────────────────────────────────────────────────────────────────

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
    let _tls_fingerprint = tls.fingerprint.clone();

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
    let (pane_rows, pane_cols) = util::compute_pane_size(term_rows, term_cols);

    // Resolve repo root and workspace mode (dispatch-sa1).
    let git_toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() {
            String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
        } else {
            None
        });

    let (_repo_root, workspace) = if let Some(root) = git_toplevel {
        // Inside a git repo — single-repo mode (backwards compatible).
        (root.clone(), Workspace::SingleRepo { root })
    } else {
        // Not in a git repo — scan children for repos (dispatch-sa1).
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| ".".to_string());
        let repos = util::scan_child_repos(&cwd);
        (cwd.clone(), Workspace::MultiRepo { parent: cwd, repos })
    };

    let mut app = App::new(
        cfg.auth.psk.clone(),
        cfg.server.port,
        ws_state,
        pane_rows,
        pane_cols,
        cfg.tools.clone(),
        workspace,
        cfg.terminal.scrollback_lines,
        chat_tx,
    );

    // dispatch-guj: eagerly spawn orchestrator in background so it's warm
    // by the time the first voice message arrives (eliminates first-message lag).
    let orch_repos: Vec<String> = app.repo_list().iter().map(|s| s.to_string()).collect();
    let orch_cwd = app.default_repo_root().to_string();
    let (orch_ready_tx, orch_ready_rx) = mpsc::channel::<orchestrator::Orchestrator>();
    thread::spawn(move || {
        let repo_refs: Vec<&str> = orch_repos.iter().map(|s| s.as_str()).collect();
        let tool_defs = tools::tool_definitions();
        let system_prompt = orchestrator::build_system_prompt(&repo_refs, &tool_defs);
        if let Some(orch) = orchestrator::spawn(&system_prompt, &orch_cwd) {
            let _ = orch_ready_tx.send(orch);
        }
    });
    app.push_ticker("ORCHESTRATOR: starting...".to_string());

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
                    app.slots[i] = None;
                    // Sync ws_state so the handler knows this slot is empty (dispatch-boa).
                    {
                        let mut st = app.ws_state.lock().unwrap();
                        st.slots[i] = None;
                    }
                    if let Some(id) = task_id {
                        app.push_orch(OrchestratorEventKind::TaskComplete { id: id.clone(), agent: callsign.clone() });
                        app.push_chat(&callsign, &format!("Task {} complete.", id));
                        // Notify orchestrator of completion so it can decide next steps.
                        if let Some(orch) = &mut app.orchestrator {
                            orch.send_message(&format!("[EVENT] TASK_COMPLETE agent={} task={}", callsign, id));
                        }
                        app.push_ticker(format!("TASK COMPLETE: {} closed {} — slot {} now standby", callsign, id, i + 1));
                    } else {
                        app.push_ticker(format!("AGENT EXITED: {} (slot {}) — standby", callsign, i + 1));
                        if let Some(orch) = &mut app.orchestrator {
                            orch.send_message(&format!("[EVENT] AGENT_EXITED agent={} slot={}", callsign, i + 1));
                        }
                    }
                }
            }
        }

        if quit_requested && app.active_count() == 0 {
            break;
        }

        // Advance ticker animation each frame (dispatch-ami).
        app.tick_ticker();

        // dispatch-guj: pick up background-spawned orchestrator when ready.
        if app.orchestrator.is_none() {
            if let Ok(orch) = orch_ready_rx.try_recv() {
                app.orchestrator = Some(orch);
                app.push_ticker("ORCHESTRATOR: online".to_string());
                // Flush any voice messages that arrived before orchestrator was ready.
                let pending: Vec<String> = app.pending_voice.drain(..).collect();
                if let Some(orch) = &mut app.orchestrator {
                    for msg in pending {
                        orch.send_message(&format!("[MIC] {}", msg));
                    }
                }
            }
        }

        // Process events from the WebSocket thread (dispatch-1lc.1, dispatch-h62).
        while let Ok(event) = ws_event_rx.try_recv() {
            let ws_server::WsEvent::VoiceTranscript { text } = event;
            app.radio_state = RadioState::Connected;
            app.push_orch(OrchestratorEventKind::VoiceTranscript { text: text.clone() });
            app.push_chat("You", &text);
            if let Some(orch) = &mut app.orchestrator {
                orch.send_message(&format!("[MIC] {}", text));
            } else {
                app.pending_voice.push(text);
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
                    let chat_text = util::strip_action_blocks(&text);
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

            ui::render_header(f, chunks[0], &app);
            ui::render_ticker(f, chunks[1], &app);
            // Clear the main content area to prevent visual artifacts when switching views.
            f.render_widget(Clear, chunks[2]);
            match app.view_mode {
                ViewMode::Agents => ui::render_panes(f, chunks[2], &app),
                ViewMode::Orchestrator => ui::render_orchestrator(f, chunks[2], &app),
            }
            ui::render_footer(f, chunks[3], &app);

            match app.overlay {
                Overlay::None => {}
                Overlay::Help => ui::render_help_overlay(f, full),
                Overlay::ConnectionInfo => ui::render_connection_info_overlay(f, full, &app),
                Overlay::ConfirmQuit => ui::render_confirm_overlay(
                    f, full, "QUIT", "Agents are running. Really quit?",
                ),
                Overlay::ConfirmTerminate => {
                    let target_g = app.target_global();
                    let name = app.slots.get(target_g)
                        .and_then(|s| s.as_ref())
                        .map(|a| a.display_name().to_string())
                        .unwrap_or_else(|| format!("slot {}", target_g + 1));
                    ui::render_confirm_overlay(f, full, "TERMINATE", &format!("Terminate {}?", name));
                }
                Overlay::RepoSelect => ui::render_repo_select_overlay(f, full, &app),
            }
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                // Terminal resize (dispatch-bgz.6)
                Event::Resize(new_cols, new_rows) => {
                    let (new_pane_rows, new_pane_cols) = util::compute_pane_size(new_rows, new_cols);
                    app.pane_rows = new_pane_rows;
                    app.pane_cols = new_pane_cols;
                    pty::resize_all_slots(
                        &mut app.slots,
                        PtySize { rows: new_pane_rows, cols: new_pane_cols, pixel_width: 0, pixel_height: 0 },
                    );
                }

                Event::Key(key) if key.kind == KeyEventKind::Press => match app.mode {
                    // Input mode: keystrokes forwarded to targeted PTY (dispatch-bgz.4)
                    Mode::Input => {
                        // Esc immediately exits input mode
                        if key.code == KeyCode::Esc {
                            app.mode = Mode::Command;
                            app.esc_exit_time = Some(Instant::now());
                            continue 'main;
                        }

                        let target_g = app.target_global();
                        if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                            let bytes = pty::key_to_pty_bytes(&key);
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
                                Overlay::Help | Overlay::ConnectionInfo => {
                                    app.overlay = Overlay::None;
                                }

                                Overlay::ConfirmQuit => match key.code {
                                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                                        if app.active_count() == 0 {
                                            break 'main;
                                        }
                                        for i in 0..MAX_SLOTS {
                                            pty::terminate_slot(&mut app.slots[i]);
                                        }
                                        // Kill orchestrator on quit.
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
                                        if !callsign.is_empty() {
                                            app.push_orch(OrchestratorEventKind::Terminated { agent: callsign.clone(), slot: target_g + 1 });
                                        }
                                        let task_id = pty::terminate_slot(&mut app.slots[target_g]);
                                        if task_id.is_some() {
                                            app.push_ticker(format!("TERMINATED: {} (slot {})", callsign, target_g + 1));
                                        } else if !callsign.is_empty() {
                                            app.push_ticker(format!("TERMINATED: {} (slot {})", callsign, target_g + 1));
                                        }
                                        app.overlay = Overlay::None;
                                    }
                                    _ => app.overlay = Overlay::None,
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
                                                if let Some(slot) = pty::dispatch_slot(
                                                    g, "claude-code", &cmd, app.pane_rows, app.pane_cols,
                                                    Some(&selected_repo), app.scrollback_lines,
                                                    util::repo_name_from_path(&selected_repo), &selected_repo,
                                                    None,
                                                ) {
                                                    let page = g / SLOTS_PER_PAGE;
                                                    let local = g % SLOTS_PER_PAGE;
                                                    let name = slot.display_name().to_string();
                                                    app.push_orch(OrchestratorEventKind::Dispatched { agent: name.clone(), slot: g + 1, tool: "claude-code".to_string() });
                                                    app.push_ticker(format!("DISPATCH: {} launched in slot {} — repo {}", name, g + 1, util::repo_name_from_path(&selected_repo)));
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
                                    // Reset scroll when entering input mode
                                    let target_g = app.target_global();
                                    if let Some(Some(slot)) = app.slots.get_mut(target_g) {
                                        slot.scroll_offset = 0;
                                    }
                                    app.mode = Mode::Input;
                                    app.esc_exit_time = None;
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

                                // Terminate target agent
                                KeyCode::Char('k') => {
                                    let target_g = app.target_global();
                                    if app.slots[target_g].is_some() {
                                        app.overlay = Overlay::ConfirmTerminate;
                                    }
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

                                // Orchestrator scroll
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
