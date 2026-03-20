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

mod app;
mod config;
mod mdns;
mod pty;
mod task_file;
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
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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
    let (pane_rows, pane_cols) = util::compute_pane_size(term_rows, term_cols);

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
        completion_timeout,
        repo_root.clone(),
        workspace,
        cfg.terminal.scrollback_lines,
        tls_fingerprint,
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

    // Background thread: poll .dispatch/tasks.md for ready tasks (dispatch-1lc.3).
    // dispatch-sa1: in multi-repo mode, poll all repos.
    let (tasks_tx, tasks_rx) = mpsc::channel::<Vec<QueuedTask>>();
    let poll_repos: Vec<String> = app.repo_list().iter().map(|s| s.to_string()).collect();
    thread::spawn(move || loop {
        let mut all_tasks = Vec::new();
        for repo in &poll_repos {
            all_tasks.extend(task_file::fetch_ready_tasks(repo));
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
                        task_file::update_task_in_file(&slot_repo, &id, 'x', None);
                        // Dispatch newly unblocked tasks after completion.
                        app.dispatch_ready_tasks();
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
                util::compute_screen_hash(parser.screen())
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
                util::is_idle_prompt(parser.screen(), &slot.tool)
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
            task_file::update_task_in_file(&slot_repo, &task_id, 'x', None);

            // Dispatch newly unblocked tasks after completion.
            app.dispatch_ready_tasks();

            // Pick up next available queued task and assign it to the idle slot.
            let next = task_file::fetch_ready_tasks(&slot_repo).into_iter().next();
            if let Some(qt) = next {
                let mut assigned = false;
                let mut assigned_callsign = String::new();
                if let Some(slot) = app.slots[i].as_mut() {
                    let callsign = slot.callsign.clone();
                    if task_file::update_task_in_file(&slot_repo, &qt.id, '~', Some(&callsign)) {
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
                Overlay::TaskList => ui::render_task_list_overlay(f, full, &app),
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
                Overlay::DispatchSlot => ui::render_dispatch_overlay(f, full, &app),
                Overlay::Rename => ui::render_rename_overlay(f, full, &app),
                Overlay::RepoSelect => ui::render_repo_select_overlay(f, full, &app),
                Overlay::PromptHistory => ui::render_prompt_history_overlay(f, full, &app),
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
                                            if let Some(task_id) = pty::terminate_slot(&mut app.slots[i]) {
                                                let repo = slot_repo.unwrap_or_else(|| app.default_repo_root().to_string());
                                                task_file::update_task_in_file(&repo, &task_id, ' ', None);
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
                                        if let Some(task_id) = pty::terminate_slot(&mut app.slots[target_g]) {
                                            task_file::update_task_in_file(&slot_repo, &task_id, ' ', None);
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
                                                    if let Some(slot) = pty::dispatch_slot(
                                                        g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                                                        app.scrollback_lines, util::repo_name_from_path(&target_repo), &target_repo,
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
                                            } else if util::is_valid_callsign(&name) {
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
                                            if let Some(slot) = pty::dispatch_slot(
                                                g, "claude-code", &cmd, app.pane_rows, app.pane_cols, None,
                                                app.scrollback_lines, util::repo_name_from_path(&target_repo), &target_repo,
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
                                        app.task_list_data.extend(task_file::fetch_task_list_from_file(repo, &app.slots));
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
