// Persistent LLM orchestrator (dispatch-h62).
//
// Spawns a headless AI agent process as the orchestrator. Supports two protocols:
// - Claude stream-json: the original protocol using `--output-format stream-json`
// - ACP (Agent Client Protocol): JSON-RPC 2.0 over stdin/stdout, used by Copilot
//   and other ACP-compatible agents.
//
// Voice transcripts and system events are piped in as user messages. The
// orchestrator responds with reasoning and structured action JSON blocks,
// which the console parses and executes.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}};
use std::thread;

use crate::tools;

// ── Types ────────────────────────────────────────────────────────────────────

/// Output from the orchestrator process, sent over mpsc channel from reader.
pub enum OrchestratorOutput {
    /// Full text from an assistant response.
    Text(String),
    /// Turn complete signal.
    TurnComplete,
    /// Process exited or stdout closed.
    Exited,
}

/// Lifecycle state of the orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorState {
    /// Waiting for a user message.
    Idle,
    /// Sent a user message, waiting for response.
    Responding,
    /// Process died.
    Dead,
}

/// Which wire protocol the orchestrator is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    /// Claude stream-json: NDJSON lines with `type` field.
    StreamJson,
    /// Agent Client Protocol: JSON-RPC 2.0 over stdin/stdout.
    Acp,
}

/// A persistent orchestrator subprocess.
pub struct Orchestrator {
    child: Child,
    /// Shared writer for stdin. ACP reader thread also writes (to respond to
    /// agent requests like `requestPermission`), so this is behind Arc<Mutex>.
    writer: Arc<Mutex<BufWriter<std::process::ChildStdin>>>,
    rx: mpsc::Receiver<OrchestratorOutput>,
    pub state: OrchestratorState,
    /// Queued messages to send once the current turn completes.
    pending: std::collections::VecDeque<String>,
    /// Parallel to `pending`: whether each queued message is user-originated.
    pending_user: std::collections::VecDeque<bool>,
    /// Whether the current turn was triggered by authentic user voice/text input.
    /// Used to gate destructive actions (terminate) so the LLM cannot hallucinate
    /// a fake user message and self-authorize dangerous operations.
    user_turn: bool,
    /// Session ID from the init message.
    session_id: String,
    /// Protocol in use.
    protocol: Protocol,
    /// Monotonically increasing JSON-RPC request ID (ACP only).
    next_rpc_id: Arc<AtomicU64>,
    /// Random per-session nonce embedded in protocol prefixes (e.g. `[D-a8f3:MIC]`)
    /// to prevent the LLM from hallucinating valid protocol messages.
    nonce: String,
}

// ── Session nonce ────────────────────────────────────────────────────────────

/// Generate a random 4-character hex nonce for protocol message prefixes.
pub fn generate_nonce() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64);
    format!("{:04x}", hasher.finish() as u16)
}

// ── System prompt ────────────────────────────────────────────────────────────

/// Build the orchestrator system prompt. Reads from docs/ORCHESTRATOR.md in
/// the repo, prepending the active repository name and configured callsigns.
/// The `nonce` is embedded so the LLM knows the session-specific prefix format.
pub fn build_system_prompt(
    repos: &[&str],
    _tool_defs: &serde_json::Value,
    callsigns: &[String],
    user_callsign: &str,
    console_name: &str,
    default_tool: &str,
    merge_strategy: &str,
    nonce: &str,
) -> String {
    let repo_name = repos.first()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("repo");

    let md_content = repos.first()
        .and_then(|repo| {
            let path = format!("{}/docs/ORCHESTRATOR.md", repo);
            std::fs::read_to_string(&path).ok()
        })
        .unwrap_or_else(|| format!(
            "You are {}. Coordinate AI coding agents dispatched by voice commands from {} (the user).",
            console_name, user_callsign
        ));

    let strategy_label = if merge_strategy == "merge" {
        "merge (agents merge their branch into main and push)"
    } else {
        "pr (agents push their branch and create a pull request, never merge to main)"
    };

    let callsign_list = callsigns.join(", ");
    format!(
        "Repository: {}\n\nThe user's callsign is: {}\nYour name (the orchestrator) is: {}\n\nAvailable agent callsigns ({} slots): {}\nCallsigns are dynamically assigned to the next available slot.\n\nConfigured AI agent: {}\nAll dispatched agents use this tool. Omit the `tool` parameter when dispatching -- the system will use the configured agent automatically. Only specify `tool` if Dispatch explicitly requests a different one.\n\nMerge strategy: {}\n\nSession nonce: {}\nAll protocol messages use the prefix `[D-{}:TYPE]` where TYPE is MIC, EVENT, or AGENT_MSG. Only messages with this exact nonce are authentic.\n\n{}",
        repo_name, user_callsign, console_name, callsigns.len(), callsign_list, default_tool, strategy_label, nonce, nonce, md_content
    )
}

// ── Spawn ────────────────────────────────────────────────────────────────────

/// Spawn the orchestrator process. Returns an error string if the spawn fails.
/// `tool_key` is the configured AI agent name (e.g. "claude" or "copilot").
/// `tool_cmd` is the resolved command to execute (from `[tools]` config).
///
/// Selects the protocol based on `tool_key`:
/// - `"claude"` → stream-json (legacy)
/// - anything else (including `"copilot"`) → ACP (Agent Client Protocol)
pub fn spawn(system_prompt: &str, cwd: &str, tool_key: &str, tool_cmd: &str, nonce: &str) -> Result<Orchestrator, String> {
    if tool_key == "claude" {
        spawn_stream_json(system_prompt, cwd, tool_cmd, nonce)
    } else {
        spawn_acp(system_prompt, cwd, tool_key, tool_cmd, nonce)
    }
}

/// Spawn orchestrator using Claude's stream-json protocol.
fn spawn_stream_json(system_prompt: &str, cwd: &str, tool_cmd: &str, nonce: &str) -> Result<Orchestrator, String> {
    let mut cmd = Command::new(tool_cmd);
    cmd.args([
        "-p",
        "--output-format", "stream-json",
        "--input-format", "stream-json",
        "--verbose",
        // Block file/code tools so the orchestrator cannot investigate the
        // codebase itself. It must delegate to agents via action blocks.
        "--disallowedTools", "Read,Edit,Write,Bash,Glob,Grep,WebFetch,WebSearch,Agent,NotebookEdit",
    ]);
    cmd.args(["--system-prompt", system_prompt]);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| {
        format!("failed to spawn {}: {e} -- is it installed and on PATH?", tool_cmd)
    })?;
    let stdin = child.stdin.take()
        .ok_or_else(|| "failed to open orchestrator stdin".to_string())?;
    let stdout = child.stdout.take()
        .ok_or_else(|| "failed to open orchestrator stdout".to_string())?;

    let writer = Arc::new(Mutex::new(BufWriter::new(stdin)));
    let (tx, rx) = mpsc::channel();
    let (sid_tx, sid_rx) = mpsc::channel();

    // Reader thread: parse stream-json output line by line.
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut sent_sid = false;
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if !sent_sid {
                if let Some(sid) = parsed.get("session_id").and_then(|v| v.as_str()) {
                    let _ = sid_tx.send(sid.to_string());
                    sent_sid = true;
                }
            }

            let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match msg_type {
                "assistant" => {
                    if let Some(content) = parsed
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_array())
                    {
                        for block in content {
                            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                    if !text.is_empty() {
                                        let _ = tx.send(OrchestratorOutput::Text(text.to_string()));
                                    }
                                }
                            }
                        }
                    }
                }
                "result" => {
                    let _ = tx.send(OrchestratorOutput::TurnComplete);
                }
                _ => {}
            }
        }
        let _ = tx.send(OrchestratorOutput::Exited);
    });

    let session_id = sid_rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| "default".to_string());

    Ok(Orchestrator {
        child,
        writer,
        rx,
        state: OrchestratorState::Idle,
        pending: std::collections::VecDeque::new(),
        pending_user: std::collections::VecDeque::new(),
        user_turn: false,
        session_id,
        protocol: Protocol::StreamJson,
        next_rpc_id: Arc::new(AtomicU64::new(1)),
        nonce: nonce.to_string(),
    })
}

// ── ACP (Agent Client Protocol) ─────────────────────────────────────────────

/// Write a JSON-RPC line to the shared writer. Returns Err on I/O failure.
fn rpc_write(writer: &Arc<Mutex<BufWriter<std::process::ChildStdin>>>, msg: &serde_json::Value) -> Result<(), String> {
    let mut w = writer.lock().map_err(|e| format!("stdin lock: {e}"))?;
    writeln!(w, "{}", msg).map_err(|e| format!("stdin write: {e}"))?;
    w.flush().map_err(|e| format!("stdin flush: {e}"))
}

/// Read lines from stdout until we get a JSON-RPC response matching `expected_id`.
/// Any agent requests received in the meantime are auto-handled (permissions
/// approved, everything else rejected). Notifications are ignored during init.
/// Max lines to read before giving up on a response (prevents indefinite blocking).
const RPC_READ_MAX_LINES: usize = 10_000;

fn rpc_read_response(
    reader: &mut BufReader<std::process::ChildStdout>,
    writer: &Arc<Mutex<BufWriter<std::process::ChildStdin>>>,
    expected_id: u64,
) -> Result<serde_json::Value, String> {
    let mut line_buf = String::new();
    for i in 0..RPC_READ_MAX_LINES {
        line_buf.clear();
        let n = reader.read_line(&mut line_buf).map_err(|e| format!("stdout read: {e}"))?;
        if n == 0 {
            return Err("agent process closed stdout during init".to_string());
        }
        let trimmed = line_buf.trim();
        if trimmed.is_empty() { continue; }

        let parsed: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| format!("json parse: {e}"))?;

        let has_id = parsed.get("id").is_some();
        let has_method = parsed.get("method").is_some();

        // Response to our request.
        if has_id && !has_method {
            if let Some(id) = parsed.get("id").and_then(|v| v.as_u64()) {
                if id == expected_id {
                    if let Some(err) = parsed.get("error") {
                        return Err(format!("RPC error: {}", err));
                    }
                    return Ok(parsed.get("result").cloned().unwrap_or(serde_json::Value::Null));
                }
            }
        }

        if i == 1000 {
            eprintln!("warning: rpc_read_response still waiting for id {} after 1000 lines", expected_id);
        }

        // Agent request — auto-handle during init.
        if has_id && has_method {
            handle_agent_request(writer, &parsed);
        }
        // Notifications during init — ignore.
    }
    Err(format!("no response for RPC id {} after {} lines", expected_id, RPC_READ_MAX_LINES))
}

/// Respond to an agent-initiated JSON-RPC request.
/// Permissions are auto-approved; everything else is rejected.
fn handle_agent_request(
    writer: &Arc<Mutex<BufWriter<std::process::ChildStdin>>>,
    parsed: &serde_json::Value,
) {
    let id = match parsed.get("id") {
        Some(id) => id.clone(),
        None => return,
    };
    let method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let response = match method {
        "requestPermission" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "outcome": "approved" }
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not supported by dispatch" }
        }),
    };
    let _ = rpc_write(writer, &response);
}

/// Spawn orchestrator using the Agent Client Protocol (ACP).
/// Works with Copilot and any other ACP-compatible agent.
fn spawn_acp(system_prompt: &str, cwd: &str, tool_key: &str, tool_cmd: &str, nonce: &str) -> Result<Orchestrator, String> {
    let mut cmd = Command::new(tool_cmd);
    cmd.arg("--acp");
    if tool_key == "copilot" {
        cmd.arg("--yolo");
    }
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    // Force Copilot into app mode, bypassing its loader which tries to
    // re-exec with --no-warnings and crashes (GitHub CLI bug #1399).
    cmd.env("COPILOT_RUN_APP", "1");
    cmd.env_remove("COPILOT_LOADER_PID");

    let mut child = cmd.spawn().map_err(|e| {
        format!("failed to spawn {}: {e} -- is it installed and on PATH?", tool_cmd)
    })?;
    let stdin = child.stdin.take()
        .ok_or_else(|| "failed to open orchestrator stdin".to_string())?;
    let mut stdout = BufReader::new(
        child.stdout.take()
            .ok_or_else(|| "failed to open orchestrator stdout".to_string())?
    );

    let writer = Arc::new(Mutex::new(BufWriter::new(stdin)));
    let next_rpc_id = Arc::new(AtomicU64::new(1));

    // ── 1. Initialize ────────────────────────────────────────────────────
    let init_id = next_rpc_id.fetch_add(1, Ordering::SeqCst);
    rpc_write(&writer, &serde_json::json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientInfo": { "name": "dispatch", "version": "1.0" },
            "clientCapabilities": {}
        }
    }))?;
    let _init_result = rpc_read_response(&mut stdout, &writer, init_id)?;

    // ── 2. Create session ────────────────────────────────────────────────
    let session_id_rpc = next_rpc_id.fetch_add(1, Ordering::SeqCst);
    rpc_write(&writer, &serde_json::json!({
        "jsonrpc": "2.0",
        "id": session_id_rpc,
        "method": "session/new",
        "params": {
            "cwd": cwd,
            "mcpServers": []
        }
    }))?;
    let session_result = rpc_read_response(&mut stdout, &writer, session_id_rpc)?;
    let session_id = session_result.get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("ACP session/new response missing sessionId")?
        .to_string();

    // ── 3. Send system prompt as first turn ──────────────────────────────
    let sys_prompt_id = next_rpc_id.fetch_add(1, Ordering::SeqCst);
    rpc_write(&writer, &serde_json::json!({
        "jsonrpc": "2.0",
        "id": sys_prompt_id,
        "method": "session/prompt",
        "params": {
            "sessionId": &session_id,
            "prompt": [{ "type": "text", "text": system_prompt }]
        }
    }))?;
    // Drain all notifications and the response for the system-prompt turn.
    // We discard the agent's reply — it's just an acknowledgement.
    let _sys_result = rpc_read_response(&mut stdout, &writer, sys_prompt_id)?;

    // ── 4. Start reader thread ───────────────────────────────────────────
    let (tx, rx) = mpsc::channel();
    let reader_writer = writer.clone();

    thread::spawn(move || {
        // Accumulate text chunks within a turn so we can emit the full text.
        let mut turn_text = String::new();

        for line in stdout.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }

            let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let has_id = parsed.get("id").is_some();
            let has_method = parsed.get("method").is_some();
            let has_result = parsed.get("result").is_some();
            let has_error = parsed.get("error").is_some();

            if has_id && (has_result || has_error) && !has_method {
                // ── Response to a session/prompt request (turn complete) ──
                if !turn_text.is_empty() {
                    let _ = tx.send(OrchestratorOutput::Text(std::mem::take(&mut turn_text)));
                }
                let _ = tx.send(OrchestratorOutput::TurnComplete);
            } else if has_method && !has_id {
                // ── Notification from agent ──
                let method = parsed.get("method").and_then(|v| v.as_str()).unwrap_or("");
                if method == "session/update" {
                    if let Some(update) = parsed.pointer("/params/update") {
                        let update_type = update.get("sessionUpdate")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if update_type == "agent_message_chunk" {
                            if let Some(text) = update.pointer("/content/text").and_then(|v| v.as_str()) {
                                turn_text.push_str(text);
                            }
                        }
                        // agent_thought_chunk, tool calls, etc. are ignored for now.
                    }
                }
            } else if has_id && has_method {
                // ── Agent request (needs response) ──
                handle_agent_request(&reader_writer, &parsed);
            }
        }
        let _ = tx.send(OrchestratorOutput::Exited);
    });

    Ok(Orchestrator {
        child,
        writer,
        rx,
        state: OrchestratorState::Idle,
        pending: std::collections::VecDeque::new(),
        pending_user: std::collections::VecDeque::new(),
        user_turn: false,
        session_id,
        protocol: Protocol::Acp,
        next_rpc_id,
        nonce: nonce.to_string(),
    })
}

// ── Methods ──────────────────────────────────────────────────────────────────

impl Orchestrator {
    /// Send a system-originated message to the orchestrator (events, agent msgs,
    /// tool results). Marks the turn as NOT user-initiated, so destructive actions
    /// (terminate) will be blocked.
    pub fn send_message(&mut self, content: &str) {
        if self.state == OrchestratorState::Dead {
            return;
        }
        if self.state == OrchestratorState::Responding {
            self.pending.push_back(content.to_string());
            self.pending_user.push_back(false);
            return;
        }
        self.user_turn = false;
        self.send_raw(content);
    }

    /// Send a user-originated message (voice/text from Dispatch) to the
    /// orchestrator. Marks the turn as user-initiated, allowing destructive
    /// actions like terminate.
    pub fn send_user_message(&mut self, content: &str) {
        if self.state == OrchestratorState::Dead {
            return;
        }
        if self.state == OrchestratorState::Responding {
            self.pending.push_back(content.to_string());
            self.pending_user.push_back(true);
            return;
        }
        self.user_turn = true;
        self.send_raw(content);
    }

    /// Whether the current turn was triggered by user voice/text input.
    pub fn is_user_turn(&self) -> bool {
        self.user_turn
    }

    /// Session nonce for protocol message prefixes.
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// Send directly (bypasses queue check). Branches on protocol.
    fn send_raw(&mut self, content: &str) {
        let msg = match self.protocol {
            Protocol::StreamJson => {
                serde_json::json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": content
                    },
                    "session_id": self.session_id,
                    "parent_tool_use_id": null
                })
            }
            Protocol::Acp => {
                let id = self.next_rpc_id.fetch_add(1, Ordering::SeqCst);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": self.session_id,
                        "prompt": [{ "type": "text", "text": content }]
                    }
                })
            }
        };
        if rpc_write(&self.writer, &msg).is_err() {
            self.state = OrchestratorState::Dead;
            return;
        }
        self.state = OrchestratorState::Responding;
    }

    /// Try to receive output. Returns None if nothing available yet.
    pub fn try_recv(&mut self) -> Option<OrchestratorOutput> {
        match self.rx.try_recv() {
            Ok(output) => {
                match &output {
                    OrchestratorOutput::TurnComplete => {
                        self.state = OrchestratorState::Idle;
                        // Flush pending messages.
                        if let Some(msg) = self.pending.pop_front() {
                            self.user_turn = self.pending_user.pop_front().unwrap_or(false);
                            self.send_raw(&msg);
                        }
                    }
                    OrchestratorOutput::Exited => {
                        self.state = OrchestratorState::Dead;
                    }
                    _ => {}
                }
                Some(output)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.state = OrchestratorState::Dead;
                Some(OrchestratorOutput::Exited)
            }
        }
    }

    /// Kill the orchestrator process.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        self.state = OrchestratorState::Dead;
    }

    /// Interrupt the current response: kill the process and clear pending queue.
    pub fn interrupt(&mut self) {
        self.pending.clear();
        self.pending_user.clear();
        self.kill();
    }

    /// Check if the orchestrator is alive.
    pub fn is_alive(&self) -> bool {
        self.state != OrchestratorState::Dead
    }
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

// ── Action block parsing ─────────────────────────────────────────────────────

/// Parse action blocks from orchestrator response text.
/// Looks for ```action ... ``` fenced blocks containing JSON.
pub fn parse_all_tool_calls(text: &str) -> Vec<tools::ToolCall> {
    let mut calls = Vec::new();
    let mut search_from = 0;

    while search_from < text.len() {
        let remaining = &text[search_from..];

        // Look for ```action blocks
        let start_marker = "```action";
        let end_marker = "```";

        if let Some(start) = remaining.find(start_marker) {
            let json_start = start + start_marker.len();
            let after_marker = &remaining[json_start..];
            if let Some(end) = after_marker.find(end_marker) {
                let json_str = after_marker[..end].trim();
                if let Ok(call) = parse_action_json(json_str) {
                    calls.push(call);
                }
                search_from += json_start + end + end_marker.len();
                continue;
            }
        }

        // Also try <tool_call> format as fallback
        if let Some(start) = remaining.find("<tool_call>") {
            if let Some(end) = remaining[start..].find("</tool_call>") {
                let json_start = start + "<tool_call>".len();
                let json_end = start + end;
                let json_str = remaining[json_start..json_end].trim();
                if let Ok(call) = serde_json::from_str::<tools::ToolCall>(json_str) {
                    calls.push(call);
                }
                search_from += start + end + "</tool_call>".len();
                continue;
            }
        }

        break;
    }

    calls
}

/// Parse a JSON action block into a ToolCall.
fn parse_action_json(json_str: &str) -> Result<tools::ToolCall, serde_json::Error> {
    let v: serde_json::Value = serde_json::from_str(json_str)?;
    let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");

    match action {
        "dispatch" => {
            let repo = v.get("repo").and_then(|r| r.as_str()).unwrap_or("").to_string();
            let prompt = v.get("prompt").and_then(|p| p.as_str()).unwrap_or("").to_string();
            let callsign = v.get("callsign").and_then(|c| c.as_str()).map(|s| s.to_string());
            let tool = v.get("tool").and_then(|t| t.as_str()).map(|s| s.to_string());
            Ok(tools::ToolCall::Dispatch { repo, prompt, callsign, tool })
        }
        "terminate" => {
            let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("").to_string();
            Ok(tools::ToolCall::Terminate { agent })
        }
        "merge" => {
            let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("").to_string();
            Ok(tools::ToolCall::Merge { agent })
        }
        "list_agents" => Ok(tools::ToolCall::ListAgents),
        "list_repos" => Ok(tools::ToolCall::ListRepos),
        "message_agent" => {
            let agent = v.get("agent").and_then(|a| a.as_str()).unwrap_or("").to_string();
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
            Ok(tools::ToolCall::MessageAgent { agent, text })
        }
        "strike_team" => {
            let source_file = v.get("source_file").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let name = v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string());
            let repo = v.get("repo").and_then(|r| r.as_str()).unwrap_or("").to_string();
            Ok(tools::ToolCall::StrikeTeam { source_file, name, repo })
        }
        _ => {
            use serde::de::Error;
            Err(serde_json::Error::custom(format!("unknown action: {}", action)))
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_action_block() {
        let text = "Dispatching Alpha.\n```action\n{\"action\": \"dispatch\", \"repo\": \"myrepo\", \"prompt\": \"fix bug\"}\n```";
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], tools::ToolCall::Dispatch { .. }));
    }

    #[test]
    fn parse_multiple_action_blocks() {
        let text = "Doing two things.\n```action\n{\"action\": \"list_agents\"}\n```\nThen dispatch.\n```action\n{\"action\": \"dispatch\", \"repo\": \"myrepo\", \"prompt\": \"fix it\"}\n```";
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert!(matches!(calls[0], tools::ToolCall::ListAgents));
        assert!(matches!(calls[1], tools::ToolCall::Dispatch { .. }));
    }

    #[test]
    fn parse_tool_call_fallback() {
        let text = r#"<tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix bug"}}</tool_call>"#;
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], tools::ToolCall::Dispatch { .. }));
    }

    #[test]
    fn parse_strike_team_action() {
        let text = "Launching strike team.\n```action\n{\"action\": \"strike_team\", \"source_file\": \"docs/PERFORMANCE_REVIEW.md\", \"repo\": \"dispatch\"}\n```";
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            tools::ToolCall::StrikeTeam { source_file, name, repo } => {
                assert_eq!(source_file, "docs/PERFORMANCE_REVIEW.md");
                assert!(name.is_none());
                assert_eq!(repo, "dispatch");
            }
            _ => panic!("expected StrikeTeam"),
        }
    }

    #[test]
    fn parse_strike_team_action_with_name() {
        let text = "```action\n{\"action\": \"strike_team\", \"source_file\": \"docs/spec.md\", \"name\": \"perf\", \"repo\": \"myrepo\"}\n```";
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 1);
        match &calls[0] {
            tools::ToolCall::StrikeTeam { source_file, name, repo } => {
                assert_eq!(source_file, "docs/spec.md");
                assert_eq!(name.as_deref(), Some("perf"));
                assert_eq!(repo, "myrepo");
            }
            _ => panic!("expected StrikeTeam"),
        }
    }

    #[test]
    fn parse_no_actions() {
        let text = "Just some reasoning text with no action blocks.";
        let calls = parse_all_tool_calls(text);
        assert!(calls.is_empty());
    }

    #[test]
    fn system_prompt_includes_repo() {
        let repos = vec!["/home/user/myrepo"];
        let tools = tools::tool_definitions();
        let callsigns = vec!["Alpha".to_string(), "Bravo".to_string()];
        let prompt = build_system_prompt(&repos, &tools, &callsigns, "Dispatch", "Console", "claude", "pr", "ab12");
        // Should always contain repo name as context prefix.
        assert!(prompt.contains("Repository: myrepo"));
        // Should list configured callsigns.
        assert!(prompt.contains("Alpha, Bravo"));
        // Should include identity.
        assert!(prompt.contains("Dispatch"));
        assert!(prompt.contains("Console"));
        // Should include configured AI agent.
        assert!(prompt.contains("Configured AI agent: claude"));
        // Should include merge strategy.
        assert!(prompt.contains("Merge strategy: pr"));
        // Should include session nonce.
        assert!(prompt.contains("Session nonce: ab12"));
        assert!(prompt.contains("[D-ab12:TYPE]"));
    }

    #[test]
    fn system_prompt_includes_copilot_config() {
        let repos = vec!["/home/user/myrepo"];
        let tools = tools::tool_definitions();
        let callsigns = vec!["Alpha".to_string()];
        let prompt = build_system_prompt(&repos, &tools, &callsigns, "Dispatch", "Console", "copilot", "merge", "cd34");
        assert!(prompt.contains("Configured AI agent: copilot"));
        assert!(prompt.contains("Merge strategy: merge"));
    }
}
