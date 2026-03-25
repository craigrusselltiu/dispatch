// PTY management: spawn, kill, terminate, resize (dispatch-bgz.2, dispatch-bgz.6).

use std::{
    io::{Read, Write},
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
/// Only used for copilot agents that need shared writer access from
/// background threads. Claude agents use the writer directly.
struct SharedWriter(Arc<Mutex<Box<dyn Write + Send>>>);

impl Write for SharedWriter {
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

// Workflow step 4 for "merge" strategy (merge to main and push).
const WORKFLOW_MERGE: &str = "\
4. Merge your branch into main, clean up, and push:
   ```bash
   cd \"$(git rev-parse --path-format=absolute --git-common-dir)/..\"
   git merge dispatch/{callsign} --no-ff -m \"Merge dispatch/{callsign}\"
   git worktree remove .dispatch/.worktrees/{callsign} --force
   git branch -d dispatch/{callsign}
   git push
   ```";

// Workflow step 4 for "pr" strategy (push branch + create PR, never merge).
const WORKFLOW_PR: &str = "\
4. Push your branch and create a pull request. **Do NOT merge into main.**
   ```bash
   cd \"$(git rev-parse --path-format=absolute --git-common-dir)/..\"
   git push -u origin dispatch/{callsign}
   gh pr create --base main --head dispatch/{callsign} --title \"dispatch/{callsign}: <short summary>\" --fill
   git worktree remove .dispatch/.worktrees/{callsign} --force
   ```";

/// Build agent instructions by reading `docs/AGENTS.md` and swapping the
/// workflow finalization step based on `merge_strategy`.  Also appends
/// shared memory if any prior agents have written to it.
///
/// Returns the full instruction text ready for injection as a system prompt
/// (Claude) or written to `.dispatch/instructions/AGENTS.md` (Copilot).
fn build_agent_instructions(repo_root: &str, merge_strategy: &str) -> Option<String> {
    let agents_md_path = format!("{}/docs/AGENTS.md", repo_root);
    let mut instructions = std::fs::read_to_string(&agents_md_path).ok()?;

    // Replace the merge/PR workflow block between markers.
    // Uses HTML comment markers in AGENTS.md for robust matching.
    let marker_start = "<!-- WORKFLOW_STEP_4 -->";
    let marker_end = "<!-- WORKFLOW_STEP_4_END -->";
    if let (Some(s4), Some(s5_marker)) = (instructions.find(marker_start), instructions.find(marker_end)) {
        let after_marker = s5_marker + marker_end.len();
        let workflow = if merge_strategy == "merge" { WORKFLOW_MERGE } else { WORKFLOW_PR };
        let mut patched = String::with_capacity(instructions.len());
        patched.push_str(&instructions[..s4]);
        patched.push_str(workflow);
        patched.push_str(&instructions[after_marker..]);
        instructions = patched;
    } else {
        eprintln!("warning: AGENTS.md missing workflow markers, merge_strategy not applied");
    }

    // Append shared memory from prior agents.
    let memory = read_memory_file(repo_root);
    let memory = memory.trim();
    if !memory.is_empty() {
        instructions.push_str("\n\n---\n\n## Shared Memory (from prior agents)\n\n");
        instructions.push_str(memory);
        instructions.push('\n');
    }

    Some(instructions)
}

/// Ensure `.dispatch/messages/` directory exists in the repo root.
fn ensure_messages_dir(repo_root: &str) {
    let dir = format!("{}/.dispatch/messages", repo_root);
    let _ = std::fs::create_dir_all(&dir);
}

/// Inner implementation: type text char-by-char into a writer, wait for
/// output to settle, then press Enter. Caller must provide exclusive
/// access to the writer (either by holding a lock or owning &mut).
fn type_to_copilot_inner(w: &mut dyn Write, text: &str, output_ts: &Arc<Mutex<Instant>>) {
    for ch in text.chars() {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf);
        let _ = w.write_all(bytes.as_bytes());
        let _ = w.flush();
        thread::sleep(std::time::Duration::from_millis(5));
    }
    wait_for_output_settle(output_ts);
    let _ = w.write_all(b"\r");
    let _ = w.flush();
}

/// Type text into copilot's interactive PTY one character at a time, then
/// press Enter (`\r`). Writing in bulk causes copilot's TUI to enter paste
/// mode where `\r` is treated as a literal newline instead of submitting.
///
/// After all characters are written, polls `output_ts` (the PTY reader's
/// last-output timestamp) until it has been stable for `SETTLE_MS`. This
/// guarantees copilot has finished rendering the typed text before Enter
/// is sent, avoiding partial-prompt submission.
pub fn type_to_copilot(
    w: &Arc<Mutex<Box<dyn Write + Send>>>,
    text: &str,
    output_ts: &Arc<Mutex<Instant>>,
) {
    let mut guard = w.lock().unwrap();
    type_to_copilot_inner(&mut **guard, text, output_ts);
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

/// Spin until the PTY reader timestamp has not changed for `settle_ms`,
/// meaning copilot has finished processing and rendering input.
/// Gives up after `timeout_ms` to avoid hanging forever.
fn wait_for_output_settle_with(output_ts: &Arc<Mutex<Instant>>, settle_ms: u128, timeout_ms: u128) {
    let wall_start = Instant::now();
    let mut prev = *output_ts.lock().unwrap();
    loop {
        thread::sleep(std::time::Duration::from_millis(25));
        let now_ts = *output_ts.lock().unwrap();
        if now_ts != prev {
            prev = now_ts;
        } else if now_ts.elapsed().as_millis() >= settle_ms {
            break;
        }
        if wall_start.elapsed().as_millis() >= timeout_ms {
            break;
        }
    }
}

/// Convenience wrapper with default settle/timeout for post-typing waits.
fn wait_for_output_settle(output_ts: &Arc<Mutex<Instant>>) {
    wait_for_output_settle_with(output_ts, 150, 5_000);
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
    merge_strategy: &str,
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

    // Build agent instructions with the configured merge strategy.
    let instructions = build_agent_instructions(repo_root, merge_strategy);

    // Tool-specific flags for autonomous agent operation.
    if tool_key == "claude" {
        // Claude: system prompt injection and permission bypass.
        if let Some(ref instr) = instructions {
            cmd.arg("--system-prompt");
            cmd.arg(instr);
        }
        cmd.arg("--dangerously-skip-permissions");
    } else if tool_key == "copilot" {
        // Force Copilot into app mode, bypassing its loader which tries to
        // re-exec with --no-warnings and crashes (GitHub CLI bug #1399).
        cmd.env("COPILOT_RUN_APP", "1");
        cmd.env_remove("COPILOT_LOADER_PID");

        // GitHub Copilot CLI: YOLO mode auto-accepts all tool/path/URL
        // permissions so the agent works autonomously without prompts.
        // --no-ask-user prevents the agent from pausing to ask questions.
        cmd.arg("--yolo");
        cmd.arg("--no-ask-user");

        // Write modified instructions to .dispatch/instructions/ so copilot
        // picks them up via --add-dir. This applies the merge_strategy config.
        if let Some(ref instr) = instructions {
            let instr_dir = format!("{}/.dispatch/instructions", repo_root);
            let _ = std::fs::create_dir_all(&instr_dir);
            let _ = std::fs::write(format!("{}/AGENTS.md", instr_dir), instr);
            cmd.arg("--add-dir");
            cmd.arg(&instr_dir);
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

    // For copilot agents, wrap the writer in Arc<Mutex> so background threads
    // can type char-by-char. For claude agents, use the writer directly to
    // avoid unnecessary mutex overhead on every write.
    let (writer, shared_writer): (Box<dyn Write + Send>, Option<Arc<Mutex<Box<dyn Write + Send>>>>) =
        if tool_key == "copilot" {
            let shared: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));

            // Write the initial prompt into the interactive session after spawn.
            // Copilot with -p exits after processing; we need it to stay alive.
            // Unlike Claude (which gets AGENTS.md as a --system-prompt), Copilot
            // only gets it via --add-dir as supplementary context. We prepend an
            // explicit instruction to read the file so Copilot follows the full
            // dispatch workflow (status messages, worktrees, merge strategy).
            if let Some(prompt) = initial_prompt {
                let prompt = format!(
                    "IMPORTANT: First read .dispatch/instructions/AGENTS.md for your operating \
                     instructions -- it defines how to send status messages, create worktrees, \
                     and finalize your work. Follow those instructions exactly. {}",
                    prompt
                );
                let last_output_for_delay = Arc::clone(&last_output_at);
                let w = Arc::clone(&shared);
                thread::spawn(move || {
                    // Wait for copilot to produce its first output (startup begins),
                    // or time out after 10 seconds.
                    let start = Instant::now();
                    loop {
                        thread::sleep(std::time::Duration::from_millis(500));
                        let last = *last_output_for_delay.lock().unwrap();
                        if last > start || start.elapsed() > std::time::Duration::from_secs(10) {
                            break;
                        }
                    }
                    // Wait for copilot's startup output to fully settle before
                    // typing the prompt. The first output is just startup noise
                    // (banner, loading); we need to wait until the interactive
                    // prompt is actually ready (500ms of silence, up to 30s).
                    wait_for_output_settle_with(&last_output_for_delay, 500, 30_000);
                    // Type each character individually to the PTY to simulate real
                    // keyboard input. Bulk writes cause copilot's TUI to treat input
                    // as a paste event where \r becomes a literal newline instead of
                    // the submit action.
                    type_to_copilot(&w, &prompt, &last_output_for_delay);
                });
            }

            let boxed: Box<dyn Write + Send> = Box::new(SharedWriter(Arc::clone(&shared)));
            (boxed, Some(shared))
        } else {
            (writer, None)
        };

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
        shared_writer,
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
/// Preserves scrollback capacity (previously lost by passing 0).
pub fn resize_all_slots(slots: &mut [Option<SlotState>], new_size: PtySize, scrollback: u32) {
    let scrollback = (scrollback as usize).min(10_000);
    for slot in slots.iter_mut().flatten() {
        let _ = slot.master.resize(new_size);
        let mut parser = slot.screen.lock().unwrap();
        *parser = vt100::Parser::new(new_size.rows, new_size.cols, scrollback);
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
