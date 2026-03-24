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
// Pages: slots 1-4 on page 1, 5-8 on page 2, etc. Slot count is config-driven.
// All PTYs run regardless of visible page. Each slot owns its own PTY.

mod app;
mod config;
mod mdns;
mod pty;
mod types;
mod ui;
mod util;
mod ws_server;

use base64::Engine;
use dispatch_core::{orchestrator, tools};

use clap::{Parser, Subcommand};
use std::{
    io::{self, Write},
    process::Command,
    sync::{atomic::Ordering, Arc, Mutex, mpsc},
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
    /// Open the config file in VS Code
    EditConfig,
    /// Manage console configuration channels (save/load/list/delete/show)
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
}

#[derive(Subcommand)]
enum ChannelAction {
    /// Save the current config as a named channel
    Save {
        /// Channel name (e.g. "home", "work", "laptop")
        name: String,
    },
    /// Load a saved channel (overwrites config.toml -- restart to apply)
    Load {
        /// Channel name to load
        name: String,
    },
    /// List all saved channels
    List,
    /// Delete a saved channel
    Delete {
        /// Channel name to delete
        name: String,
    },
    /// Show the contents of a saved channel
    Show {
        /// Channel name to display
        name: String,
    },
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
        Some(Commands::EditConfig) => {
            let path = config::config_path();
            // Ensure the config file exists before opening.
            let _ = config::load_or_create();
            let status = Command::new("code")
                .arg(&path)
                .status()
                .expect("failed to launch VS Code — is `code` on your PATH?");
            if !status.success() {
                eprintln!("VS Code exited with status {}", status);
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(Commands::Channel { action }) => {
            match action {
                ChannelAction::Save { name } => config::channel_save(&name),
                ChannelAction::Load { name } => config::channel_load(&name),
                ChannelAction::List => config::channel_list(),
                ChannelAction::Delete { name } => config::channel_delete(&name),
                ChannelAction::Show { name } => config::channel_show(&name),
            }
            return Ok(());
        }
        None => {}
    }

    let cfg = config::load_or_create();
    let callsigns = cfg.callsigns();

    // Load or generate TLS certificate (dispatch-ct2.6).
    let tls = config::load_or_create_tls();
    let tls_fingerprint = tls.fingerprint.clone();

    // Broadcast channel for pushing chat messages to all connected radio clients (dispatch-chat).
    let (chat_tx, _) = tokio::sync::broadcast::channel::<String>(256);

    // Channel for WsEvents from the WebSocket thread (dispatch-1lc.1).
    // Created before the server starts to prevent a race where early
    // connections could arrive before event_tx is set.
    let (ws_event_tx, ws_event_rx) = mpsc::channel::<ws_server::WsEvent>();

    // Start the WebSocket server with TLS (dispatch-bgz.7, dispatch-ct2.6).
    let ws_state: ws_server::SharedState = Arc::new(Mutex::new({
        let mut state = ws_server::ConsoleState::new(callsigns.clone());
        state.user_callsign = cfg.identity.user_callsign.clone();
        state.console_name = cfg.identity.console_name.clone();
        state.default_tool = cfg.default_tool_key().to_string();
        state.event_tx = Some(ws_event_tx);
        state
    }));
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
    // Include TLS fingerprint in TXT records so the radio can pin the certificate.
    let _mdns = mdns::advertise(cfg.server.port, Some(&tls_fingerprint));

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
        cfg.default_tool_key().to_string(),
        cfg.merge_strategy().to_string(),
        workspace,
        cfg.terminal.scrollback_lines,
        chat_tx,
        callsigns.clone(),
        cfg.identity.user_callsign.clone(),
        cfg.identity.console_name.clone(),
    );

    // Clear stale agent message and image files from previous sessions.
    for repo in &app.repo_list() {
        util::clean_dispatch_dirs(repo);
    }

    // dispatch-guj: eagerly spawn orchestrator in background so it's warm
    // by the time the first voice message arrives (eliminates first-message lag).
    let orch_repos: Vec<String> = app.repo_list().iter().map(|s| s.to_string()).collect();
    let orch_cwd = app.default_repo_root().to_string();
    let orch_callsigns = callsigns;
    let orch_user_callsign = cfg.identity.user_callsign.clone();
    let orch_console_name = cfg.identity.console_name.clone();
    let orch_default_tool = cfg.default_tool_key().to_string();
    let orch_merge_strategy = cfg.merge_strategy().to_string();
    let orch_cmd = cfg.tools.get(&orch_default_tool).cloned().unwrap_or_else(|| orch_default_tool.clone());
    let (orch_ready_tx, orch_ready_rx) = mpsc::channel::<Result<orchestrator::Orchestrator, String>>();
    {
        let tx = orch_ready_tx.clone();
        let repos = orch_repos.clone();
        let cwd = orch_cwd.clone();
        let cs = orch_callsigns.clone();
        let uc = orch_user_callsign.clone();
        let cn = orch_console_name.clone();
        let dt = orch_default_tool.clone();
        let ms = orch_merge_strategy.clone();
        let cc = orch_cmd.clone();
        thread::spawn(move || {
            let repo_refs: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
            let tool_defs = tools::tool_definitions();
            let system_prompt = orchestrator::build_system_prompt(&repo_refs, &tool_defs, &cs, &uc, &cn, &dt, &ms);
            let _ = tx.send(orchestrator::spawn(&system_prompt, &cwd, &dt, &cc));
        });
    }
    app.push_ticker("ORCHESTRATOR: starting...".to_string());

    // dispatch-sa1: show multi-repo indicator if applicable.
    if app.is_multi_repo() {
        let repo_count = app.repo_list().len();
        app.push_ticker(format!("MULTI-REPO: detected {} repos", repo_count));
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut quit_requested = false;

    'main: loop {
        // Close any slots whose child exited naturally (dispatch-bgz.9, dispatch-xje).
        let slot_count = app.slots.len();
        for i in 0..slot_count {
            if let Some(s) = &app.slots[i] {
                if s.child_exited.load(Ordering::Relaxed) {
                    // Strike team: mark task failed on unexpected exit.
                    app.strike_team_on_agent_exit(i);
                    let callsign = s.display_name().to_string();
                    let task_id = s.task_id.clone();
                    app.slots[i] = None;
                    // Sync ws_state so the handler knows this slot is empty (dispatch-boa).
                    {
                        let mut st = app.ws_state.lock().unwrap();
                        st.slots[i] = None;
                    }
                    app.broadcast_agents();
                    if let Some(id) = task_id {
                        app.push_orch(OrchestratorEventKind::TaskComplete { id: id.clone(), agent: callsign.clone() });
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

        // Idle detection: detect when a working agent stops producing output.
        // An agent with a task that has no PTY output for IDLE_TIMEOUT is
        // considered idle (finished work, sitting at prompt). The orchestrator
        // is notified on the working->idle transition so it can take action.
        const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
        for i in 0..slot_count {
            if let Some(s) = &mut app.slots[i] {
                if s.task_id.is_some() {
                    let elapsed = s.last_output_at.lock().unwrap().elapsed();
                    let is_idle_now = elapsed >= IDLE_TIMEOUT;
                    if is_idle_now && !s.idle {
                        // Transition: working -> idle.
                        s.idle = true;
                        // Clear task_id so the idle detection loop stops
                        // monitoring this agent. Without this, any minor PTY
                        // output (cursor blink, terminal redraws) flips the
                        // agent back to "working" for 10s, causing the radio
                        // to show agents as perpetually working.
                        s.task_id = None;
                        let callsign = s.display_name().to_string();
                        if let Some(orch) = &mut app.orchestrator {
                            orch.send_message(&format!(
                                "[EVENT] AGENT_IDLE agent={} slot={}",
                                callsign, i + 1
                            ));
                        }
                        // Strike team: mark task done and free the slot.
                        app.strike_team_on_agent_idle(&callsign);
                        // Sync ws_state.
                        {
                            let mut st = app.ws_state.lock().unwrap();
                            if let Some(agent) = &mut st.slots[i] {
                                agent.status = ws_server::AgentStatus::Idle;
                            }
                        }
                        app.broadcast_agents();
                    } else if !is_idle_now && s.idle {
                        // Transition: idle -> working (new output detected).
                        s.idle = false;
                        {
                            let mut st = app.ws_state.lock().unwrap();
                            if let Some(agent) = &mut st.slots[i] {
                                agent.status = ws_server::AgentStatus::Busy;
                            }
                        }
                        app.broadcast_agents();
                    }
                }
            }
        }

        // Advance ticker animation each frame (dispatch-ami).
        app.tick_ticker();
        // Advance status blink animation (REC-light pulse).
        app.tick_status_blink();
        // Advance strike team state machine (dispatch next wave, check completion).
        app.tick_strike_team();

        // dispatch-guj: pick up background-spawned orchestrator when ready.
        if app.orchestrator.is_none() && app.orch_error.is_none() {
            if let Ok(result) = orch_ready_rx.try_recv() {
                match result {
                    Ok(orch) => {
                        app.orchestrator = Some(orch);
                        app.push_ticker("ORCHESTRATOR: online".to_string());
                        app.push_chat("System", "Orchestrator online.");
                        // Flush any voice messages that arrived before orchestrator was ready.
                        let pending: Vec<String> = app.pending_voice.drain(..).collect();
                        if let Some(orch) = &mut app.orchestrator {
                            for msg in pending {
                                orch.send_message(&format!("[MIC] {}", msg));
                            }
                        }
                    }
                    Err(err) => {
                        app.orch_error = Some(err.clone());
                        app.push_ticker(format!("ORCHESTRATOR: {}", err));
                        app.push_chat("System", &format!("Orchestrator failed: {}", err));
                    }
                }
            }
        }

        // Process events from the WebSocket thread (dispatch-1lc.1, dispatch-h62).
        while let Ok(event) = ws_event_rx.try_recv() {
            match event {
                ws_server::WsEvent::RadioConnected { addr } => {
                    app.radio_state = RadioState::Connected;
                    app.push_ticker(format!("RADIO: connected from {}", addr));
                }
                ws_server::WsEvent::RadioDisconnected { addr } => {
                    app.radio_state = RadioState::Disconnected;
                    app.push_ticker(format!("RADIO: disconnected ({})", addr));
                }
                ws_server::WsEvent::TlsFailure { addr } => {
                    app.push_ticker(format!("TLS FAILED: connection from {}", addr));
                }
                ws_server::WsEvent::VoiceTranscript { text } => {
                    app.push_orch(OrchestratorEventKind::VoiceTranscript { text: text.clone() });
                    app.push_chat(&app.user_callsign.clone(), &text);
                    if let Some(orch) = &mut app.orchestrator {
                        orch.send_message(&format!("[MIC] {}", text));
                    } else {
                        app.pending_voice.push(text);
                    }
                }
                ws_server::WsEvent::InvalidPsk { addr } => {
                    app.push_ticker(format!("REJECTED: invalid PSK from {}", addr));
                }
                ws_server::WsEvent::ImageReceived { callsign, data, filename } => {
                    // Save image to .dispatch/images/ in the repo root.
                    let images_dir = format!("{}/.dispatch/images", app.default_repo_root());
                    let _ = std::fs::create_dir_all(&images_dir);
                    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
                    let image_path = format!("{}/{}_{}", images_dir, timestamp, filename);

                    let decoded = base64::engine::general_purpose::STANDARD.decode(&data);
                    if let Ok(bytes) = decoded {
                        if std::fs::write(&image_path, &bytes).is_ok() {
                            // Find agent by callsign and write the image path to its PTY.
                            let slot_idx = app.slots.iter().enumerate().find_map(|(i, s)| {
                                s.as_ref().and_then(|slot| {
                                    if slot.display_name().eq_ignore_ascii_case(&callsign) {
                                        Some(i)
                                    } else {
                                        None
                                    }
                                })
                            });

                            if let Some(idx) = slot_idx {
                                if let Some(slot) = app.slots[idx].as_mut() {
                                    let msg = format!(
                                        "Dispatch sent you an image ({}). View it at: {}\r",
                                        filename, image_path
                                    );
                                    let _ = slot.writer.write_all(msg.as_bytes());
                                    let _ = slot.writer.flush();
                                    *slot.last_output_at.lock().unwrap() = Instant::now();
                                    slot.idle = false;
                                }
                                app.push_chat(
                                    &app.user_callsign.clone(),
                                    &format!("[image -> {}] {}", callsign, filename),
                                );
                                app.push_ticker(format!("IMAGE: sent to {}", callsign));

                                // Notify orchestrator so it has visibility.
                                if let Some(orch) = &mut app.orchestrator {
                                    orch.send_message(&format!(
                                        "[EVENT] IMAGE_SENT to={} file={}",
                                        callsign, filename
                                    ));
                                }
                            }
                        } else {
                            app.push_ticker(format!("IMAGE ERROR: failed to save {}", filename));
                        }
                    } else {
                        app.push_ticker("IMAGE ERROR: invalid base64 data".to_string());
                    }
                }
                ws_server::WsEvent::Interrupt => {
                    if let Some(orch) = &mut app.orchestrator {
                        orch.interrupt();
                    }
                    app.orchestrator = None;
                    app.orch_error = None;
                    app.push_ticker("ORCHESTRATOR: interrupted — restarting...".to_string());
                    app.push_chat("System", "Orchestrator interrupted. Restarting...");
                    // Respawn in background.
                    let tx = orch_ready_tx.clone();
                    let repos = orch_repos.clone();
                    let cwd = orch_cwd.clone();
                    let cs = orch_callsigns.clone();
                    let uc = orch_user_callsign.clone();
                    let cn = orch_console_name.clone();
                    let dt = orch_default_tool.clone();
                    let ms = orch_merge_strategy.clone();
                    let cc = orch_cmd.clone();
                    thread::spawn(move || {
                        let repo_refs: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
                        let tool_defs = tools::tool_definitions();
                        let system_prompt = orchestrator::build_system_prompt(&repo_refs, &tool_defs, &cs, &uc, &cn, &dt, &ms);
                        let _ = tx.send(orchestrator::spawn(&system_prompt, &cwd, &dt, &cc));
                    });
                }
            }
        }

        // dispatch-agentchat: poll agent message files for new content.
        // Agents write messages to `.dispatch/messages/{callsign}` files
        // instead of echoing to the terminal, avoiding PTY noise issues.
        for (slot_idx, text) in pty::poll_agent_messages(&mut app.slots) {
            let callsign = app.slots.get(slot_idx)
                .and_then(|s| s.as_ref())
                .map(|s| s.display_name().to_string())
                .unwrap_or_else(|| format!("Agent-{}", slot_idx + 1));
            app.push_chat(&callsign, &text);
            app.push_orch(OrchestratorEventKind::AgentMessage { agent: callsign.clone(), text: text.clone() });
            // Forward agent status messages to the orchestrator so it has
            // visibility into agent progress (e.g. "Task received", "Merging").
            if let Some(orch) = &mut app.orchestrator {
                orch.send_message(&format!("[AGENT_MSG] {}: {}", callsign, text));
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

                    // dispatch-chat: forward orchestrator reasoning to radio
                    // (strip action blocks, system context tags, and internal
                    // [EVENT] lines so raw LLM framework artifacts don't leak).
                    let chat_text = util::strip_action_blocks(&text);
                    let chat_text = util::strip_system_tags(&chat_text);
                    let chat_text = util::strip_event_lines(&chat_text);
                    let chat_text = chat_text.trim();
                    if !chat_text.is_empty() {
                        app.push_chat(&app.console_name.clone(), chat_text);
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
                            tools::ToolCall::StrikeTeam { .. } => "strike_team",
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
                    app.push_chat("System", "Orchestrator exited.");
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
                                        for slot in app.slots.iter_mut() {
                                            pty::terminate_slot(slot);
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
                                            app.push_chat("System", &format!("Terminated agent {} (slot {}).", callsign, target_g + 1));
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
                                                let tool_key = app.default_tool.clone();
                                                let cmd = app.tool_cmd(&tool_key).to_string();
                                                let cs = app.next_callsign().unwrap_or_else(|| format!("Agent-{}", g + 1));
                                                if let Some(slot) = pty::dispatch_slot(
                                                    g, &tool_key, &cmd, app.pane_rows, app.pane_cols,
                                                    Some(&selected_repo), app.scrollback_lines,
                                                    util::repo_name_from_path(&selected_repo), &selected_repo,
                                                    None,
                                                    &cs,
                                                    &app.merge_strategy,
                                                ) {
                                                    let page = g / SLOTS_PER_PAGE;
                                                    let local = g % SLOTS_PER_PAGE;
                                                    let name = slot.display_name().to_string();
                                                    app.push_orch(OrchestratorEventKind::Dispatched { agent: name.clone(), slot: g + 1, tool: tool_key });
                                                    app.push_ticker(format!("DISPATCH: {} launched in slot {} — repo {}", name, g + 1, util::repo_name_from_path(&selected_repo)));
                                                    app.push_chat("System", &format!("Dispatched agent {} to slot {}.", name, g + 1));
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

                                // Spawn new agent in empty target slot
                                KeyCode::Char('n') => {
                                    let target_g = app.target_global();
                                    if app.slots[target_g].is_none() {
                                        let cs = app.next_callsign().unwrap_or_else(|| format!("Agent-{}", target_g + 1));
                                        let repo = app.default_repo_root().to_string();
                                        let tool_key = app.default_tool.clone();
                                        let cmd = app.tool_cmd(&tool_key).to_string();
                                        if let Some(slot) = pty::dispatch_slot(
                                            target_g, &tool_key, &cmd, app.pane_rows, app.pane_cols,
                                            None, app.scrollback_lines,
                                            util::repo_name_from_path(&repo), &repo,
                                            None,
                                            &cs,
                                            &app.merge_strategy,
                                        ) {
                                            let name = slot.display_name().to_string();
                                            app.push_orch(OrchestratorEventKind::Dispatched { agent: name.clone(), slot: target_g + 1, tool: tool_key.clone() });
                                            app.push_ticker(format!("DISPATCH: {} launched in slot {}", name, target_g + 1));
                                            app.push_chat("System", &format!("Dispatched agent {} to slot {}.", name, target_g + 1));
                                            app.slots[target_g] = Some(slot);
                                            // Sync ws_state.
                                            {
                                                let mut st = app.ws_state.lock().unwrap();
                                                st.slots[target_g] = Some(ws_server::AgentSlot {
                                                    callsign: name.clone(),
                                                    tool: tool_key,
                                                    status: ws_server::AgentStatus::Idle,
                                                    task: None,
                                                    repo: Some(util::repo_name_from_path(&repo).to_string()),
                                                });
                                            }
                                            app.broadcast_agents();
                                        }
                                    }
                                }

                                // Terminate target agent
                                KeyCode::Char('k') => {
                                    let target_g = app.target_global();
                                    if app.slots[target_g].is_some() {
                                        app.overlay = Overlay::ConfirmTerminate;
                                    }
                                }

                                // Interrupt orchestrator (cancel current response)
                                KeyCode::Char('c') => {
                                    if let Some(orch) = &mut app.orchestrator {
                                        if orch.state == orchestrator::OrchestratorState::Responding {
                                            orch.interrupt();
                                            app.orchestrator = None;
                                            app.orch_error = None;
                                            app.push_ticker("ORCHESTRATOR: interrupted — restarting...".to_string());
                                            app.push_chat("System", "Orchestrator interrupted. Restarting...");
                                            // Respawn in background.
                                            let tx = orch_ready_tx.clone();
                                            let repos = orch_repos.clone();
                                            let cwd = orch_cwd.clone();
                                            let cs = orch_callsigns.clone();
                                            let uc = orch_user_callsign.clone();
                                            let cn = orch_console_name.clone();
                                            let dt = orch_default_tool.clone();
                                            let ms = orch_merge_strategy.clone();
                                            let cc = orch_cmd.clone();
                                            thread::spawn(move || {
                                                let repo_refs: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
                                                let tool_defs = tools::tool_definitions();
                                                let system_prompt = orchestrator::build_system_prompt(&repo_refs, &tool_defs, &cs, &uc, &cn, &dt, &ms);
                                                let _ = tx.send(orchestrator::spawn(&system_prompt, &cwd, &dt, &cc));
                                            });
                                        }
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
