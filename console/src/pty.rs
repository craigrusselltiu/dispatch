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

use crate::types::SlotState;
use crate::util;

/// Marker prefix that agents echo to send chat messages to the radio app.
/// Usage: `echo "@@DISPATCH_MSG:your message here"`
pub const DISPATCH_MSG_MARKER: &str = "@@DISPATCH_MSG:";

/// Check a line buffer for a dispatch marker and send the message if it's new.
/// Called on line endings (\n, \r\n, \r\r) and on bare \r (before redraw discard).
fn check_dispatch_marker(
    line_buf: &[u8],
    marker: &str,
    last_msg: &mut String,
    tx: &std::sync::mpsc::Sender<(usize, String)>,
    slot_idx: usize,
) {
    if let Ok(line) = std::str::from_utf8(line_buf) {
        // Use rfind to get the LAST occurrence of the marker. When the
        // terminal renders the shell command echo and its output on the
        // same line, the last occurrence is the actual output.
        if let Some(pos) = line.rfind(marker) {
            // Skip shell command lines (e.g. `echo "@@DISPATCH_MSG:..."`).
            // Only check a narrow window (16 bytes) before the marker so
            // that "echo" from a command rendering earlier in the line
            // doesn't suppress the actual output marker.
            //
            // Use byte-level search to avoid panicking when the window
            // start falls inside a multi-byte UTF-8 character.
            let window_start = pos.saturating_sub(16);
            if line.as_bytes()[window_start..pos]
                .windows(4)
                .any(|w| w == b"echo")
            {
                return;
            }
            let msg = util::clean_dispatch_msg(&line[pos + marker.len()..]);
            if !msg.is_empty() && msg != *last_msg {
                *last_msg = msg.clone();
                let _ = tx.send((slot_idx, msg));
            }
        }
    }
}

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
    callsign: &str,
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
    let now = Instant::now();
    let last_output_at = Arc::new(Mutex::new(now));
    let last_output_w = Arc::clone(&last_output_at);
    let mut pty_reader = pair.master.try_clone_reader().ok()?;

    thread::spawn(move || {
        let mut child = child;
        let mut buf = [0u8; 4096];
        let mut line_buf: Vec<u8> = Vec::with_capacity(512);
        let mut last_msg = String::new();
        // Track whether the last byte was \r so we can distinguish
        // \r\n (normal line ending) from bare \r (terminal redraw).
        let mut cr_pending = false;
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Update last output timestamp for idle detection.
                    *last_output_w.lock().unwrap() = Instant::now();

                    // Scan for @@DISPATCH_MSG: markers in the byte stream.
                    //
                    // Only process the line buffer on true line endings (\n
                    // or \r\n). Bare \r (cursor-home for terminal redraws)
                    // clears the buffer WITHOUT checking for markers, so
                    // progressive redraws don't emit duplicate partial
                    // messages.
                    //
                    // Special case: \r\r\n (common on Windows where the
                    // shell outputs \r\n and the PTY adds another \r) must
                    // still be detected. When \r follows a pending \r, we
                    // process the buffer before resetting.
                    for &byte in &buf[..n] {
                        if cr_pending {
                            cr_pending = false;
                            if byte == b'\n' {
                                // \r\n — normal line ending. Process the buffer.
                                check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &agent_msg_tx, global_idx);
                                line_buf.clear();
                                continue;
                            }
                            if byte == b'\r' {
                                // \r\r — the first \r was a real line ending;
                                // process the buffer, then track this new \r.
                                check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &agent_msg_tx, global_idx);
                                line_buf.clear();
                                cr_pending = true;
                                continue;
                            }
                            // Bare \r followed by content — terminal redraw.
                            // Still check for markers before discarding: TUI
                            // renderers (Ink/ConPTY) may write the marker then
                            // reposition the cursor with \r in the same pass.
                            // Dedup (last_msg) prevents duplicate emissions.
                            check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &agent_msg_tx, global_idx);
                            line_buf.clear();
                            // Fall through to handle the current byte.
                        }
                        if byte == b'\r' {
                            cr_pending = true;
                        } else if byte == b'\n' {
                            // Bare \n — process the line.
                            check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &agent_msg_tx, global_idx);
                            line_buf.clear();
                        } else {
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
    let callsign = callsign.to_string();
    let wall = Local::now().format("%H:%M").to_string();

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
        last_output_at,
        idle: false,
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

/// Terminate a slot: kill child, clear slot, return task_id.
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
pub fn resize_all_slots(slots: &mut [Option<SlotState>], new_size: PtySize) {
    for slot in slots.iter_mut().flatten() {
        let _ = slot.master.resize(new_size);
        let mut parser = slot.screen.lock().unwrap();
        *parser = vt100::Parser::new(new_size.rows, new_size.cols, 0);
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn run_marker_check(line: &str) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        let mut last_msg = String::new();
        check_dispatch_marker(
            line.as_bytes(),
            DISPATCH_MSG_MARKER,
            &mut last_msg,
            &tx,
            0,
        );
        rx.try_recv().ok().map(|(_, msg)| msg)
    }

    #[test]
    fn marker_detected_at_line_start() {
        let msg = run_marker_check("@@DISPATCH_MSG:Task received.");
        assert_eq!(msg.as_deref(), Some("Task received."));
    }

    #[test]
    fn marker_detected_with_ansi_prefix() {
        let msg = run_marker_check("\x1b[0m@@DISPATCH_MSG:Task received.");
        assert_eq!(msg.as_deref(), Some("Task received."));
    }

    #[test]
    fn marker_filtered_on_echo_command_line() {
        let msg = run_marker_check("echo \"@@DISPATCH_MSG:Task received.\"");
        assert_eq!(msg, None);
    }

    #[test]
    fn marker_filtered_with_prompt_and_echo() {
        let msg = run_marker_check("$ echo \"@@DISPATCH_MSG:Task received.\"");
        assert_eq!(msg, None);
    }

    #[test]
    fn marker_detected_when_command_and_output_same_line() {
        // When terminal renders command + output on one line, rfind picks
        // the last (output) marker whose narrow window has no "echo".
        let line = "echo \"@@DISPATCH_MSG:Task received.\" => @@DISPATCH_MSG:Task received.";
        let msg = run_marker_check(line);
        assert_eq!(msg.as_deref(), Some("Task received."));
    }

    #[test]
    fn marker_detected_with_indentation() {
        let msg = run_marker_check("  @@DISPATCH_MSG:Task complete.");
        assert_eq!(msg.as_deref(), Some("Task complete."));
    }

    #[test]
    fn dedup_prevents_same_message_twice() {
        let (tx, rx) = mpsc::channel();
        let mut last_msg = String::new();
        let line = b"@@DISPATCH_MSG:Task received.";
        check_dispatch_marker(line, DISPATCH_MSG_MARKER, &mut last_msg, &tx, 0);
        check_dispatch_marker(line, DISPATCH_MSG_MARKER, &mut last_msg, &tx, 0);
        // Only one message should be sent.
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn echo_window_safe_with_multibyte_utf8() {
        // When a multi-byte UTF-8 char (e.g. ⏺ = 3 bytes) sits within 16
        // bytes of the marker, the echo window check must not panic.
        // ⏺ is \xe2\x8f\xba (3 bytes).
        let line = "\x1b[1m⏺\x1b[0m Bash: echo \"@@DISPATCH_MSG:Task received.\"";
        // This is a command line — should be filtered (contains echo in window).
        let msg = run_marker_check(line);
        assert_eq!(msg, None);
    }

    #[test]
    fn echo_window_safe_with_wide_unicode_prefix() {
        // Multiple multi-byte chars before the marker.
        let line = "──── @@DISPATCH_MSG:Task complete.";
        let msg = run_marker_check(line);
        assert_eq!(msg.as_deref(), Some("Task complete."));
    }

    /// Simulate the full byte processing loop to test bare-\r marker recovery.
    fn simulate_pty_bytes(input: &[u8]) -> Vec<String> {
        let (tx, rx) = mpsc::channel();
        let mut line_buf: Vec<u8> = Vec::with_capacity(512);
        let mut last_msg = String::new();
        let mut cr_pending = false;
        for &byte in input {
            if cr_pending {
                cr_pending = false;
                if byte == b'\n' {
                    check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &tx, 0);
                    line_buf.clear();
                    continue;
                }
                if byte == b'\r' {
                    check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &tx, 0);
                    line_buf.clear();
                    cr_pending = true;
                    continue;
                }
                check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &tx, 0);
                line_buf.clear();
            }
            if byte == b'\r' {
                cr_pending = true;
            } else if byte == b'\n' {
                check_dispatch_marker(&line_buf, DISPATCH_MSG_MARKER, &mut last_msg, &tx, 0);
                line_buf.clear();
            } else {
                line_buf.push(byte);
                if line_buf.len() > 4096 {
                    line_buf.clear();
                }
            }
        }
        let mut msgs = Vec::new();
        while let Ok((_, msg)) = rx.try_recv() {
            msgs.push(msg);
        }
        msgs
    }

    #[test]
    fn bare_cr_does_not_lose_marker() {
        // ConPTY / TUI redraw: marker text followed by bare \r then new content.
        // Before the fix, the bare \r discarded the buffer and the marker was lost.
        let input = b"@@DISPATCH_MSG:Task received.\rMore content\r\n";
        let msgs = simulate_pty_bytes(input);
        assert_eq!(msgs, vec!["Task received."]);
    }

    #[test]
    fn marker_via_normal_crlf_still_works() {
        let input = b"@@DISPATCH_MSG:Task received.\r\n";
        let msgs = simulate_pty_bytes(input);
        assert_eq!(msgs, vec!["Task received."]);
    }

    #[test]
    fn marker_via_double_cr_lf_still_works() {
        // Windows \r\r\n line ending.
        let input = b"@@DISPATCH_MSG:Task received.\r\r\n";
        let msgs = simulate_pty_bytes(input);
        assert_eq!(msgs, vec!["Task received."]);
    }

    #[test]
    fn bare_cr_dedup_prevents_double_emit() {
        // Marker on a line that ends with \r then gets redrawn with the same
        // marker. The bare-\r check emits the first, dedup prevents the second.
        let input = b"@@DISPATCH_MSG:Task received.\r@@DISPATCH_MSG:Task received.\r\n";
        let msgs = simulate_pty_bytes(input);
        assert_eq!(msgs, vec!["Task received."]);
    }

    #[test]
    fn echo_command_filtered_in_byte_stream() {
        // The echo command line should be filtered, output line detected.
        let input = b"echo \"@@DISPATCH_MSG:Task received.\"\r\n@@DISPATCH_MSG:Task received.\r\n";
        let msgs = simulate_pty_bytes(input);
        assert_eq!(msgs, vec!["Task received."]);
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
