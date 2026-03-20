// PTY management: spawn, kill, terminate, resize (dispatch-bgz.2, dispatch-bgz.6).

use std::{
    io::Read,
    process::Command,
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    thread,
    time::Instant,
};

use chrono::Local;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use crate::types::{SlotState, NATO, MAX_SLOTS};

/// Marker prefix that agents echo to send chat messages to the radio app.
/// Usage: `echo "@@DISPATCH_MSG:your message here"`
pub const DISPATCH_MSG_MARKER: &str = "@@DISPATCH_MSG:";

/// Open a PTY and spawn a process. Returns a SlotState on success.
/// `cwd` sets the working directory for the PTY (dispatch-xje: worktree path).
/// `initial_prompt` is passed as a CLI argument so the agent starts working immediately.
/// `agent_msg_tx` receives (slot_index, message_text) when the agent emits a @@DISPATCH_MSG marker.
pub fn dispatch_slot(
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
    agent_msg_tx: std::sync::mpsc::Sender<(usize, String)>,
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
    cmd.cwd(cwd.unwrap_or(repo_root));

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
        let mut line_buf: Vec<u8> = Vec::with_capacity(512);
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Scan for @@DISPATCH_MSG: markers in the byte stream.
                    for &byte in &buf[..n] {
                        if byte == b'\n' {
                            if let Ok(line) = std::str::from_utf8(&line_buf) {
                                if let Some(pos) = line.find(DISPATCH_MSG_MARKER) {
                                    let msg = line[pos + DISPATCH_MSG_MARKER.len()..].trim();
                                    if !msg.is_empty() {
                                        let _ = agent_msg_tx.send((global_idx, msg.to_string()));
                                    }
                                }
                            }
                            line_buf.clear();
                        } else if byte != b'\r' {
                            line_buf.push(byte);
                            if line_buf.len() > 4096 {
                                line_buf.clear();
                            }
                        }
                    }
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
        scroll_offset: 0,
    })
}

/// Kill a child process by PID (dispatch-bgz.6).
pub fn kill_child_pid(pid: u32) {
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
pub fn terminate_slot(slot: &mut Option<SlotState>) -> Option<String> {
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
pub fn resize_all_slots(slots: &mut [Option<SlotState>; MAX_SLOTS], new_size: PtySize) {
    for slot in slots.iter_mut().flatten() {
        let _ = slot.master.resize(new_size);
        let mut parser = slot.screen.lock().unwrap();
        *parser = vt100::Parser::new(new_size.rows, new_size.cols, 0);
    }
}

pub fn key_to_pty_bytes(key: &KeyEvent) -> Vec<u8> {
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
