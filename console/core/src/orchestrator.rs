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

    format!(
        r#"You are the Dispatch orchestrator — the central coordinator for a voice-controlled AI agent system.

You receive voice transcripts from a push-to-talk radio (prefixed with [MIC]) and system events (prefixed with [EVENT]). Based on these, you decide what actions to take by calling tools.

## Available Repositories
{repos}

## Available Tools

You have these tools. Call them by wrapping a JSON object in <tool_call> tags:

<tool_call>{{"name": "tool_name", "input": {{"param": "value"}}}}</tool_call>

Tool definitions:
{tools}

## How to Respond

1. When a [MIC] message addresses an agent by NATO callsign (e.g. "Alpha, do X" or "dispatch Alpha"), check if that agent exists. If it does, use `message_agent` to forward the message. If it does NOT exist, use `dispatch` to create it with the entire message as the prompt.

2. When a [MIC] message is an unaddressed prompt (no agent name), use your judgement:
   - Simple, single task (e.g. "fix the login bug") → `dispatch` an agent with the prompt
   - Complex task needing multiple agents (e.g. "perform a performance audit") → `plan` to decompose it, or dispatch multiple agents yourself
   - Quick follow-up to ongoing work → `message_agent` to an existing idle agent

3. When the user asks about status ("what agents are running?"), use `list_agents`.

4. When you receive [EVENT] TASK_COMPLETE, use `merge` to merge the completed work.

5. You may call multiple tools in one response — just include multiple <tool_call> blocks.

6. Keep your reasoning brief. The user sees your text in the orchestrator log view. Lead with the action, not the explanation."#,
        repos = repo_list.join("\n"),
        tools = tools_json,
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
        "--tools", "",
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
