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

/// Thin wrapper so an `Arc<Mutex<Box<dyn Write + Send>>>` can itself
/// satisfy `Write + Send` and be stored in `SlotState::writer`.
struct SharedWriter(Arc<Mutex<Box<dyn std::io::Write + Send>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

/// Ensure `.dispatch/MEMORY.md` exists in the repo root with a starter template.
/// Called before spawning an agent so shared memory is always available.
fn ensure_memory_file(repo_root: &str) {
    let dispatch_dir = format!("{}/.dispatch", repo_root);
    let memory_path = format!("{}/MEMORY.md", dispatch_dir);
    if std::path::Path::new(&memory_path).exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&dispatch_dir);
    let template = "\
# Shared Agent Memory

Knowledge base from prior agents. Updated when agents learn something valuable.

## Build & Test

## Gotchas

## Notes
";
    let _ = std::fs::write(&memory_path, template);
}

/// Read `.dispatch/MEMORY.md` from the repo root. Returns empty string if missing.
fn read_memory_file(repo_root: &str) -> String {
    let path = format!("{}/.dispatch/MEMORY.md", repo_root);
    std::fs::read_to_string(&path).unwrap_or_default()
}

/// Ensure `.dispatch/messages/` directory exists in the repo root.
fn ensure_messages_dir(repo_root: &str) {
    let dir = format!("{}/.dispatch/messages", repo_root);
    let _ = std::fs::create_dir_all(&dir);
}

/// Type text into copilot's interactive PTY one character at a time, then
/// press Enter (`\r`). Writing in bulk causes copilot's TUI to enter paste
/// mode where `\r` is treated as a literal newline instead of submitting.
/// By sending each character individually with a small delay we simulate
/// real keyboard typing and keep copilot in single-char input mode.
pub fn type_to_copilot(w: &Arc<Mutex<Box<dyn std::io::Write + Send>>>, text: &str) {
    use std::io::Write;
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf);
        let mut guard = w.lock().unwrap();
        let _ = guard.write_all(bytes.as_bytes());
        let _ = guard.flush();
        drop(guard);
        thread::sleep(std::time::Duration::from_millis(2));
    }
    // Brief pause then send Enter to submit.
    thread::sleep(std::time::Duration::from_millis(50));
    let mut guard = w.lock().unwrap();
    let _ = guard.write_all(b"\r");
    let _ = guard.flush();
}

/// Prepare the agent's message file: delete any stale file and return the path.
/// Called before spawning so the new agent starts with a clean file.
pub fn prepare_msg_file(repo_root: &str, callsign: &str) -> String {
    ensure_messages_dir(repo_root);
    let path = format!("{}/.dispatch/messages/{}", repo_root, callsign);
    // Remove stale file from a previous agent with the same callsign.
    let _ = std::fs::remove_file(&path);
    path
}

/// Open a PTY and spawn a process. Returns a SlotState on success.
/// `cwd` sets the working directory for the PTY (dispatch-xje: worktree path).
/// `initial_prompt` is passed as a CLI argument so the agent starts working immediately.
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
    // Ensure shared memory file exists for this repo.
    ensure_memory_file(repo_root);

    // Prepare message file for this agent (file-based messaging).
    let msg_file = prepare_msg_file(repo_root, callsign);

    // Set DISPATCH_MSG_FILE env var so the agent can write messages to a file
    // instead of echoing to the terminal. This eliminates terminal noise issues.
    cmd.env("DISPATCH_MSG_FILE", &msg_file);

    // Force Copilot into app mode, bypassing its loader which tries to
    // re-exec with --no-warnings and crashes (GitHub CLI bug #1399).
    cmd.env("COPILOT_RUN_APP", "1");
    cmd.env_remove("COPILOT_LOADER_PID");

    // Tool-specific flags for autonomous agent operation.
    if tool_key == "claude" {
        // Claude: system prompt injection and permission bypass.
        let agents_md_path = format!("{}/docs/AGENTS.md", repo_root);
        if let Ok(mut instructions) = std::fs::read_to_string(&agents_md_path) {
            let memory = read_memory_file(repo_root);
            let memory = memory.trim();
            if !memory.is_empty() {
                instructions.push_str("\n\n---\n\n## Shared Memory (from prior agents)\n\n");
                instructions.push_str(memory);
                instructions.push('\n');
            }
            cmd.arg("--system-prompt");
            cmd.arg(&instructions);
        }
        cmd.arg("--dangerously-skip-permissions");
    } else if tool_key == "copilot" {
        // GitHub Copilot CLI: YOLO mode auto-accepts all tool/path/URL
        // permissions so the agent works autonomously without prompts.
        // --no-ask-user prevents the agent from pausing to ask questions.
        cmd.arg("--yolo");
        cmd.arg("--no-ask-user");

        // Copilot reads AGENTS.md from the repo root (not docs/).
        // Ensure it exists by symlinking from docs/AGENTS.md if needed.
        let root_agents = format!("{}/AGENTS.md", repo_root);
        let docs_agents = format!("{}/docs/AGENTS.md", repo_root);
        if !std::path::Path::new(&root_agents).exists() && std::path::Path::new(&docs_agents).exists() {
            #[cfg(unix)]
            { let _ = std::os::unix::fs::symlink(&docs_agents, &root_agents); }
            #[cfg(windows)]
            { let _ = std::fs::copy(&docs_agents, &root_agents); }
        }
    }
    if let Some(prompt) = initial_prompt {
        // Claude accepts the prompt as a bare positional arg and stays interactive.
        // Copilot with -p runs non-interactively and exits after processing, so
        // we skip -p here and write the prompt into the PTY stdin after spawn.
        if tool_key != "copilot" {
            cmd.arg(prompt);
        }
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

    // PTY reader thread: updates the VT100 screen buffer and idle timestamp.
    // Agent messages are read from the message file by the main loop, not
    // extracted from the PTY stream.
    let _slot_idx = global_idx;
    thread::spawn(move || {
        let mut child = child;
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Update last output timestamp for idle detection.
                    *last_output_w.lock().unwrap() = Instant::now();
                    screen_w.lock().unwrap().process(&buf[..n]);
                }
            }
        }
        let _ = child.wait();
        child_exited_w.store(true, Ordering::Relaxed);
    });

    let writer = pair.master.take_writer().ok()?;

    // For Copilot: write the prompt into the interactive session after spawn.
    // Copilot with -p exits after processing; we need it to stay alive.
    // Wrap writer in Arc<Mutex> so a delayed-prompt thread can share it.
    // Use \r (not \n) to submit since that's what the PTY expects.
    let shared: Arc<Mutex<Box<dyn std::io::Write + Send>>> = Arc::new(Mutex::new(writer));

    if tool_key == "copilot" {
        if let Some(prompt) = initial_prompt {
            let prompt = prompt.to_string();
            let last_output_for_delay = Arc::clone(&last_output_at);
            let w = Arc::clone(&shared);
            thread::spawn(move || {
                // Wait for copilot to produce its first output (prompt ready),
                // or time out after 10 seconds.
                let start = Instant::now();
                loop {
                    thread::sleep(std::time::Duration::from_millis(500));
                    let last = *last_output_for_delay.lock().unwrap();
                    if last > start || start.elapsed() > std::time::Duration::from_secs(10) {
                        thread::sleep(std::time::Duration::from_millis(1000));
                        break;
                    }
                }
                // Type each character individually to the PTY to simulate real
                // keyboard input. Bulk writes cause copilot's TUI to treat input
                // as a paste event where \r becomes a literal newline instead of
                // the submit action.
                type_to_copilot(&w, &prompt);
            });
        }
    }

    // Wrap shared writer as Box<dyn Write + Send> for SlotState.
    let writer: Box<dyn std::io::Write + Send> = Box::new(SharedWriter(shared));
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
        msg_file,
        msg_offset: 0,
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

/// Poll agent message files for new content. Returns (slot_index, text).
/// Each agent writes messages to `.dispatch/messages/{callsign}`, one per line.
pub fn poll_agent_messages(slots: &mut [Option<SlotState>]) -> Vec<(usize, String)> {
    let mut messages = Vec::new();
    for (i, slot) in slots.iter_mut().enumerate() {
        if let Some(s) = slot {
            if let Ok(meta) = std::fs::metadata(&s.msg_file) {
                let len = meta.len();
                if len > s.msg_offset {
                    if let Ok(mut file) = std::fs::File::open(&s.msg_file) {
                        use std::io::{Seek, SeekFrom};
                        let _ = file.seek(SeekFrom::Start(s.msg_offset));
                        let mut buf = String::new();
                        if file.read_to_string(&mut buf).is_ok() {
                            for line in buf.lines() {
                                let line = line.trim();
                                if !line.is_empty() {
                                    messages.push((i, line.to_string()));
                                }
                            }
                        }
                        s.msg_offset = len;
                    }
                }
            }
        }
    }
    messages
}

/// Same as `type_to_copilot` but operates on a bare `Write` trait object.
/// Used from `app.rs` where we have `&mut Box<dyn Write + Send>` directly.
pub fn type_to_copilot_writer(w: &mut Box<dyn std::io::Write + Send>, text: &str) {
    use std::io::Write;
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf);
        let _ = w.write_all(bytes.as_bytes());
        let _ = w.flush();
        thread::sleep(std::time::Duration::from_millis(2));
    }
    // Brief pause then send Enter to submit.
    thread::sleep(std::time::Duration::from_millis(50));
    let _ = w.write_all(b"\r");
    let _ = w.flush();
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
