// Persistent LLM orchestrator (dispatch-h62).
//
// Spawns a Claude Code process as the orchestrator (always Claude -- the
// protocol depends on stream-json I/O and session_id handshake).
// Voice transcripts and system events are piped in as user messages. The
// orchestrator responds with reasoning and structured action JSON blocks,
// which the console parses and executes.

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
    pending: std::collections::VecDeque<String>,
    /// Session ID from the init message (Claude-specific).
    session_id: String,
    /// Tool key (e.g. "claude-code" or "copilot") -- determines wire protocol.
    tool_key: String,
}

// ── System prompt ────────────────────────────────────────────────────────────

/// Build the orchestrator system prompt. Reads from docs/ORCHESTRATOR.md in
/// the repo, prepending the active repository name and configured callsigns.
pub fn build_system_prompt(
    repos: &[&str],
    _tool_defs: &serde_json::Value,
    callsigns: &[String],
    user_callsign: &str,
    console_name: &str,
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

    let callsign_list = callsigns.join(", ");
    format!(
        "Repository: {}\n\nThe user's callsign is: {}\nYour name (the orchestrator) is: {}\n\nAvailable agent callsigns ({} slots): {}\nCallsigns are dynamically assigned to the next available slot.\n\n{}",
        repo_name, user_callsign, console_name, callsigns.len(), callsign_list, md_content
    )
}

// ── Spawn ────────────────────────────────────────────────────────────────────

/// Spawn the orchestrator process. Returns an error message if the spawn fails.
///
/// `tool_key` is the config key (e.g. "claude-code" or "copilot") and `tool_cmd`
/// is the resolved command string (e.g. "claude" or "gh copilot --").
pub fn spawn(system_prompt: &str, cwd: &str, tool_key: &str, tool_cmd: &str) -> Result<Orchestrator, String> {
    let parts: Vec<&str> = tool_cmd.split_whitespace().collect();
    if parts.is_empty() {
        return Err("tool command is empty".to_string());
    }
    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }
    match tool_key {
        "claude-code" => {
            cmd.args([
                "-p",
                "--output-format", "stream-json",
                "--input-format", "stream-json",
                "--verbose",
                "--system-prompt", system_prompt,
            ]);
        }
        "copilot" => {
            cmd.args([
                "--output-format", "json",
            ]);
        }
        _ => {}
    }
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn '{tool_cmd}': {e}"))?;
    let stdin = child.stdin.take().ok_or("failed to open orchestrator stdin")?;
    let stdout = child.stdout.take().ok_or("failed to open orchestrator stdout")?;

    let (tx, rx) = mpsc::channel();

    if tool_key == "claude-code" {
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

        Ok(Orchestrator {
            child,
            stdin,
            rx,
            state: OrchestratorState::Idle,
            pending: std::collections::VecDeque::new(),
            session_id,
            tool_key: tool_key.to_string(),
        })
    } else {
        // Non-Claude tool: simple line-based reader, no session_id handshake.
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let _ = tx.send(OrchestratorOutput::Text(trimmed.to_string()));
                }
            }
            let _ = tx.send(OrchestratorOutput::Exited);
        });

        Ok(Orchestrator {
            child,
            stdin,
            rx,
            state: OrchestratorState::Idle,
            pending: std::collections::VecDeque::new(),
            session_id: "default".to_string(),
            tool_key: tool_key.to_string(),
        })
    }
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
            self.pending.push_back(content.to_string());
            return;
        }
        self.send_raw(content);
    }

    /// Send directly (bypasses queue check).
    fn send_raw(&mut self, content: &str) {
        let line = if self.tool_key == "claude-code" {
            let msg = serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": content
                },
                "session_id": self.session_id,
                "parent_tool_use_id": null
            });
            format!("{}\n", msg)
        } else {
            format!("{}\n", content)
        };
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
                        if let Some(msg) = self.pending.pop_front() {
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
            Ok(tools::ToolCall::Dispatch { repo, prompt, callsign })
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
        let prompt = build_system_prompt(&repos, &tools, &callsigns, "Dispatch", "Console");
        // Should always contain repo name as context prefix.
        assert!(prompt.contains("Repository: myrepo"));
        // Should list configured callsigns.
        assert!(prompt.contains("Alpha, Bravo"));
        // Should include identity.
        assert!(prompt.contains("Dispatch"));
        assert!(prompt.contains("Console"));
    }
}
