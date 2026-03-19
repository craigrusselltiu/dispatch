// Persistent LLM orchestrator (dispatch-h62).
//
// Spawns a headless `claude` process using stream-json I/O as the orchestrator.
// Voice transcripts and system events are piped in as user messages. The
// orchestrator responds with reasoning and <tool_call> tags, which the console
// parses and executes. Tool results are sent back as the next user message.
//
// Wire protocol:
//   stdin:  JSON lines — {"type":"user","content":"..."}
//   stdout: JSON lines — init, assistant, result messages

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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

/// A persistent orchestrator subprocess.
pub struct Orchestrator {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<OrchestratorOutput>,
    pub state: OrchestratorState,
    /// Queued messages to send once the current turn completes.
    pending: Vec<String>,
    /// Session ID from the init message.
    session_id: String,
}

// ── System prompt ────────────────────────────────────────────────────────────

/// Build the orchestrator system prompt from dynamic state.
pub fn build_system_prompt(
    repos: &[&str],
    tool_defs: &serde_json::Value,
) -> String {
    let repo_list: Vec<String> = repos
        .iter()
        .map(|p| {
            let name = std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(p);
            format!("- {} ({})", name, p)
        })
        .collect();

    let tools_json = serde_json::to_string_pretty(tool_defs).unwrap_or_default();

    let repo_name = repos.first()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("repo");

    format!(
        r#"You are a dispatch coordinator. You receive voice commands prefixed with [MIC] and system events prefixed with [EVENT]. You MUST respond by calling tools using <tool_call> tags. Do NOT use any other tool format. Do NOT use Claude Code's built-in tools. ONLY use the tools defined below via <tool_call> tags.

CRITICAL: Every action MUST include a <tool_call> tag. Never just acknowledge a request — always act on it immediately with a tool call. If the user says "dispatch Alpha", you MUST include a <tool_call> in your response. No exceptions.

Repositories: {repos}

Tools — call by including this exact format in your response text:
<tool_call>{{"name":"tool_name","input":{{"key":"value"}}}}</tool_call>

Available tools:
- dispatch(repo, prompt) — dispatch a new agent with a task
- terminate(agent) — kill an agent by callsign or slot number
- merge(task_id) — merge a completed task branch
- list_agents() — list all agent slots
- list_repos() — list available repos
- plan(repo, prompt) — decompose a complex task into subtasks
- message_agent(agent, text) — send text to an existing agent's terminal

Rules:
1. "Alpha, do X" or "dispatch Alpha" → dispatch with: <tool_call>{{"name":"dispatch","input":{{"repo":"{repo_name}","prompt":"do X"}}}}</tool_call>
2. "terminate Alpha" → <tool_call>{{"name":"terminate","input":{{"agent":"Alpha"}}}}</tool_call>
3. Unaddressed task → dispatch an agent for it
4. [EVENT] TASK_COMPLETE → merge it
5. Be brief. Act first, explain after."#,
        repos = repo_list.join(", "),
        repo_name = repo_name,
    )
}

// ── Spawn ────────────────────────────────────────────────────────────────────

/// Spawn the orchestrator process. Returns None if the spawn fails.
pub fn spawn(system_prompt: &str, cwd: &str) -> Option<Orchestrator> {
    let mut cmd = Command::new("claude");
    cmd.args([
        "-p",
        "--output-format", "stream-json",
        "--input-format", "stream-json",
        "--verbose",
        "--system-prompt", system_prompt,
    ]);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;

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

            // Parse the JSON line.
            let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Capture session_id from the first message that has one.
            if !sent_sid {
                if let Some(sid) = parsed.get("session_id").and_then(|v| v.as_str()) {
                    let _ = sid_tx.send(sid.to_string());
                    sent_sid = true;
                }
            }

            let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match msg_type {
                "assistant" => {
                    // Extract text from message.content[].text
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
                    // Don't emit Text here -- the same content was already sent
                    // via the "assistant" message. Only signal turn completion.
                    let _ = tx.send(OrchestratorOutput::TurnComplete);
                }
                _ => {
                    // init, rate_limit_event, etc. — ignore.
                }
            }
        }
        let _ = tx.send(OrchestratorOutput::Exited);
    });

    // Wait briefly for the init message to get the session_id.
    let session_id = sid_rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| "default".to_string());

    Some(Orchestrator {
        child,
        stdin,
        rx,
        state: OrchestratorState::Idle,
        pending: Vec::new(),
        session_id,
    })
}

// ── Methods ──────────────────────────────────────────────────────────────────

impl Orchestrator {
    /// Send a user message to the orchestrator.
    /// If the orchestrator is mid-response, the message is queued.
    pub fn send_message(&mut self, content: &str) {
        if self.state == OrchestratorState::Dead {
            return;
        }
        if self.state == OrchestratorState::Responding {
            self.pending.push(content.to_string());
            return;
        }
        self.send_raw(content);
    }

    /// Send directly (bypasses queue check).
    fn send_raw(&mut self, content: &str) {
        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content
            },
            "session_id": self.session_id,
            "parent_tool_use_id": null
        });
        let line = format!("{}\n", msg);
        if self.stdin.write_all(line.as_bytes()).is_err() || self.stdin.flush().is_err() {
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
                        if let Some(msg) = self.pending.first().cloned() {
                            self.pending.remove(0);
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

// ── Multi-tool-call parsing ──────────────────────────────────────────────────

/// Parse all tool calls from a text block. Returns them in order of appearance.
pub fn parse_all_tool_calls(text: &str) -> Vec<tools::ToolCall> {
    let mut calls = Vec::new();
    let mut search_from = 0;

    while search_from < text.len() {
        let remaining = &text[search_from..];
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

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_tool_call() {
        let text = r#"I'll dispatch an agent. <tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix bug"}}</tool_call>"#;
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0], tools::ToolCall::Dispatch { .. }));
    }

    #[test]
    fn parse_multiple_tool_calls() {
        let text = r#"Let me check the state first.
<tool_call>{"name": "list_agents"}</tool_call>
Then dispatch.
<tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix it"}}</tool_call>"#;
        let calls = parse_all_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert!(matches!(calls[0], tools::ToolCall::ListAgents));
        assert!(matches!(calls[1], tools::ToolCall::Dispatch { .. }));
    }

    #[test]
    fn parse_no_tool_calls() {
        let text = "Just some reasoning text with no tool calls.";
        let calls = parse_all_tool_calls(text);
        assert!(calls.is_empty());
    }

    #[test]
    fn system_prompt_includes_repos() {
        let repos = vec!["/home/user/myrepo"];
        let tools = tools::tool_definitions();
        let prompt = build_system_prompt(&repos, &tools);
        assert!(prompt.contains("myrepo"));
        assert!(prompt.contains("dispatch"));
        assert!(prompt.contains("<tool_call>"));
    }
}
