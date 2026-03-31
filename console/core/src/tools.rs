// Orchestrator tool interface.
//
// Defines the tools available to the orchestrator agent and provides execution
// logic. The console intercepts tool calls from the orchestrator, executes them,
// and returns structured results.
//
// Tools:
//   dispatch(repo, prompt, callsign?) — dispatch an agent with a prompt
//   terminate(agent)                  — kill agent by callsign or slot number
//   merge(agent)                      — acknowledge a completed merge
//   list_agents()                     — get all agent slot states
//   list_repos()                      — list known repositories
//   message_agent(agent, text)        — send text to an agent's PTY
//   strike_team(source_file, name?, repo) — launch a strike team from a document

use serde::{Deserialize, Serialize};

// ── Tool call types ─────────────────────────────────────────────────────────

/// A tool call from the orchestrator agent.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "name", content = "input")]
#[serde(rename_all = "snake_case")]
pub enum ToolCall {
    /// Dispatch an agent with a prompt.
    Dispatch {
        /// Repository name or path to work in.
        repo: String,
        /// Task description / prompt for the agent.
        prompt: String,
        /// Optional NATO callsign (e.g. "Delta") to dispatch a specific agent.
        #[serde(default)]
        callsign: Option<String>,
        /// Optional tool key (e.g. "claude" or "copilot"). Falls back to
        /// the configured default tool if not specified.
        #[serde(default)]
        tool: Option<String>,
    },
    /// Terminate a running agent by callsign or slot number.
    Terminate {
        /// Agent callsign (e.g. "Alpha") or slot number as string (e.g. "1").
        agent: String,
    },
    /// Acknowledge a completed merge.
    Merge {
        /// Agent callsign (e.g. "Alpha").
        agent: String,
    },
    /// List all agent slots and their current state.
    ListAgents,
    /// List available repositories.
    ListRepos,
    /// Send text to a running agent's terminal.
    MessageAgent {
        /// Agent callsign (e.g. "Alpha") or slot number as string (e.g. "1").
        agent: String,
        /// Text to send to the agent's PTY.
        text: String,
    },
    /// Launch a Strike Team from a document.
    StrikeTeam {
        /// Optional document hint or path. May be omitted for common repo docs
        /// like "the spec", which the console resolves automatically.
        #[serde(default)]
        source_file: String,
        /// Short name for this operation. Defaults to source filename without extension.
        #[serde(default)]
        name: Option<String>,
        /// Repository name or path.
        repo: String,
    },
}

// ── Tool result types ───────────────────────────────────────────────────────

/// Information about an agent slot, returned by list_agents.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub slot: u32,
    pub callsign: String,
    pub tool: String,
    /// "busy", "idle", or "empty".
    pub status: String,
    pub task: Option<String>,
    pub repo: Option<String>,
}

/// Information about a repository, returned by list_repos.
#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    pub name: String,
    pub path: String,
}

/// Result of executing a tool call.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResult {
    /// Agent dispatched successfully.
    Dispatched {
        slot: u32,
        callsign: String,
        task_id: String,
    },
    /// Agent terminated.
    Terminated { slot: u32, callsign: String },
    /// Merge result.
    Merged {
        agent: String,
        success: bool,
        message: String,
    },
    /// Agent listing.
    Agents { agents: Vec<AgentInfo> },
    /// Repository listing.
    Repos { repos: Vec<RepoInfo> },
    /// Message sent to agent.
    MessageSent { agent: String, slot: u32 },
    /// Strike team launched.
    StrikeTeamAcknowledged {
        name: String,
        source_file: String,
        repo: String,
    },
    /// Tool call failed.
    Error { message: String },
}

// ── Tool definitions for LLM ────────────────────────────────────────────────

/// Return tool definitions as a JSON array suitable for LLM tool-calling APIs.
/// Each definition follows the Claude/OpenAI function-calling schema.
pub fn tool_definitions() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "dispatch",
            "description": "Dispatch an AI agent with a prompt. The agent creates its own git worktree, works, commits, merges, and pushes.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "repo": {
                        "type": "string",
                        "description": "Repository name or path to work in."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Task description / prompt for the agent."
                    },
                    "callsign": {
                        "type": "string",
                        "description": "Optional NATO callsign to assign (e.g. \"Delta\"). When provided, the agent is dispatched with this callsign to the next available slot."
                    },
                    "tool": {
                        "type": "string",
                        "description": "Override the configured AI agent for this dispatch only. Values: \"claude\" or \"copilot\". Omit this parameter to use the configured agent (normal behavior). Only specify when Dispatch explicitly requests a different tool."
                    }
                },
                "required": ["repo", "prompt"]
            }
        },
        {
            "name": "terminate",
            "description": "Terminate a running agent. The agent's process is killed and its slot is freed.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Agent callsign (e.g. \"Alpha\") or slot number (e.g. \"1\")."
                    }
                },
                "required": ["agent"]
            }
        },
        {
            "name": "merge",
            "description": "Acknowledge that an agent has completed its merge. Agents merge their own branches into main.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Agent callsign (e.g. \"Alpha\")."
                    }
                },
                "required": ["agent"]
            }
        },
        {
            "name": "list_agents",
            "description": "List all agent slots and their current state, including callsign, tool, busy/idle status, current task, and repository.",
            "input_schema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "list_repos",
            "description": "List available repositories that agents can be dispatched into.",
            "input_schema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "message_agent",
            "description": "Send text to a running agent's terminal (PTY). Use this to provide additional instructions, answer agent questions, or inject commands.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Agent callsign (e.g. \"Alpha\") or slot number (e.g. \"1\")."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to send to the agent's terminal."
                    }
                },
                "required": ["agent", "text"]
            }
        },
        {
            "name": "strike_team",
            "description": "Launch a Strike Team: read any document (spec, review, design doc, etc.), break it into tasks with dependencies, then dispatch agents in parallel waves until all tasks are complete.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "source_file": {
                        "type": "string",
                        "description": "Optional repo-relative path or shorthand for the document (for example \"docs/SPEC.md\", \"spec\", or \"architecture\"). Omit this when Dispatch says \"the spec\" and the console should resolve the repo's main spec automatically."
                    },
                    "name": {
                        "type": "string",
                        "description": "Short name for this operation. Defaults to source filename without extension."
                    },
                    "repo": {
                        "type": "string",
                        "description": "Repository name or path."
                    }
                },
                "required": ["repo"]
            }
        }
    ])
}

// ── Agent resolution ────────────────────────────────────────────────────────

/// Resolve an agent identifier (callsign or slot number) to a 0-indexed slot.
/// Returns None if the agent is not found.
pub fn resolve_agent(agent: &str, slots: &[bool], callsigns: &[Option<String>]) -> Option<usize> {
    // Try as slot number first (1-indexed).
    if let Ok(n) = agent.parse::<usize>() {
        if n >= 1 && n <= slots.len() && slots[n - 1] {
            return Some(n - 1);
        }
    }
    // Try as callsign (case-insensitive).
    let upper = agent.to_uppercase();
    callsigns.iter().enumerate().find_map(|(i, cs)| {
        cs.as_ref().and_then(|name| {
            if name.to_uppercase() == upper && slots[i] {
                Some(i)
            } else {
                None
            }
        })
    })
}

// ── Parsing tool calls from text ────────────────────────────────────────────

/// Attempt to parse a tool call from a text block. Looks for JSON between
/// `<tool_call>` and `</tool_call>` markers, or bare JSON with a "name" field.
pub fn parse_tool_call(text: &str) -> Option<ToolCall> {
    // Try tagged format: <tool_call>{...}</tool_call>
    if let Some(start) = text.find("<tool_call>") {
        if let Some(end) = text.find("</tool_call>") {
            let json_str = &text[start + "<tool_call>".len()..end].trim();
            if let Ok(call) = serde_json::from_str::<ToolCall>(json_str) {
                return Some(call);
            }
        }
    }
    // Try bare JSON object with "name" key.
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            let json_str = &text[start..=end];
            if json_str.contains("\"name\"") {
                if let Ok(call) = serde_json::from_str::<ToolCall>(json_str) {
                    return Some(call);
                }
            }
        }
    }
    None
}

/// Format a tool result as text that can be sent back to the orchestrator.
pub fn format_tool_result(id: Option<&str>, result: &ToolResult) -> String {
    let json = serde_json::to_string_pretty(result).unwrap_or_default();
    match id {
        Some(id) => format!("<tool_result id=\"{id}\">\n{json}\n</tool_result>"),
        None => format!("<tool_result>\n{json}\n</tool_result>"),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dispatch_call() {
        let text = r#"<tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix the bug"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::Dispatch {
                repo,
                prompt,
                callsign,
                tool,
            } => {
                assert_eq!(repo, "myrepo");
                assert_eq!(prompt, "fix the bug");
                assert!(callsign.is_none());
                assert!(tool.is_none());
            }
            _ => panic!("expected Dispatch"),
        }
    }

    #[test]
    fn parse_dispatch_call_with_callsign() {
        let text = r#"<tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix the bug", "callsign": "Delta"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::Dispatch {
                repo,
                prompt,
                callsign,
                tool,
            } => {
                assert_eq!(repo, "myrepo");
                assert_eq!(prompt, "fix the bug");
                assert_eq!(callsign.as_deref(), Some("Delta"));
                assert!(tool.is_none());
            }
            _ => panic!("expected Dispatch"),
        }
    }

    #[test]
    fn parse_dispatch_call_with_tool() {
        let text = r#"<tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix the bug", "tool": "copilot"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::Dispatch {
                repo,
                prompt,
                callsign,
                tool,
            } => {
                assert_eq!(repo, "myrepo");
                assert_eq!(prompt, "fix the bug");
                assert!(callsign.is_none());
                assert_eq!(tool.as_deref(), Some("copilot"));
            }
            _ => panic!("expected Dispatch"),
        }
    }

    #[test]
    fn parse_list_agents_call() {
        let text = r#"<tool_call>{"name": "list_agents"}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        assert!(matches!(call, ToolCall::ListAgents));
    }

    #[test]
    fn parse_terminate_call() {
        let text = r#"<tool_call>{"name": "terminate", "input": {"agent": "Alpha"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::Terminate { agent } => assert_eq!(agent, "Alpha"),
            _ => panic!("expected Terminate"),
        }
    }

    #[test]
    fn parse_merge_call() {
        let text = r#"<tool_call>{"name": "merge", "input": {"agent": "Alpha"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::Merge { agent } => assert_eq!(agent, "Alpha"),
            _ => panic!("expected Merge"),
        }
    }

    #[test]
    fn parse_message_agent_call() {
        let text = r#"<tool_call>{"name": "message_agent", "input": {"agent": "1", "text": "hello"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::MessageAgent { agent, text } => {
                assert_eq!(agent, "1");
                assert_eq!(text, "hello");
            }
            _ => panic!("expected MessageAgent"),
        }
    }

    #[test]
    fn parse_strike_team_call() {
        let text = r#"<tool_call>{"name": "strike_team", "input": {"source_file": "docs/auth-spec.md", "repo": "myrepo"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::StrikeTeam {
                source_file,
                name,
                repo,
            } => {
                assert_eq!(source_file, "docs/auth-spec.md");
                assert!(name.is_none());
                assert_eq!(repo, "myrepo");
            }
            _ => panic!("expected StrikeTeam"),
        }
    }

    #[test]
    fn parse_strike_team_call_with_name() {
        let text = r#"<tool_call>{"name": "strike_team", "input": {"source_file": "docs/auth-spec.md", "name": "auth", "repo": "myrepo"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::StrikeTeam {
                source_file,
                name,
                repo,
            } => {
                assert_eq!(source_file, "docs/auth-spec.md");
                assert_eq!(name.as_deref(), Some("auth"));
                assert_eq!(repo, "myrepo");
            }
            _ => panic!("expected StrikeTeam"),
        }
    }

    #[test]
    fn parse_strike_team_call_without_source_file() {
        let text = r#"<tool_call>{"name": "strike_team", "input": {"repo": "myrepo"}}</tool_call>"#;
        let call = parse_tool_call(text).unwrap();
        match call {
            ToolCall::StrikeTeam {
                source_file,
                name,
                repo,
            } => {
                assert!(source_file.is_empty());
                assert!(name.is_none());
                assert_eq!(repo, "myrepo");
            }
            _ => panic!("expected StrikeTeam"),
        }
    }

    #[test]
    fn parse_bare_json() {
        let text = r#"I'll dispatch an agent now. {"name": "dispatch", "input": {"repo": "myrepo", "prompt": "test"}}"#;
        let call = parse_tool_call(text).unwrap();
        assert!(matches!(call, ToolCall::Dispatch { .. }));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert!(parse_tool_call("just some text").is_none());
        assert!(parse_tool_call(r#"{"not_a_tool": true}"#).is_none());
    }

    #[test]
    fn resolve_agent_by_slot() {
        let slots = [true, true, false];
        let callsigns = [Some("ALPHA".to_string()), Some("BRAVO".to_string()), None];
        assert_eq!(resolve_agent("1", &slots, &callsigns), Some(0));
        assert_eq!(resolve_agent("2", &slots, &callsigns), Some(1));
        assert_eq!(resolve_agent("3", &slots, &callsigns), None); // empty slot
    }

    #[test]
    fn resolve_agent_by_callsign() {
        let slots = [true, true, false];
        let callsigns = [Some("ALPHA".to_string()), Some("BRAVO".to_string()), None];
        assert_eq!(resolve_agent("Alpha", &slots, &callsigns), Some(0));
        assert_eq!(resolve_agent("bravo", &slots, &callsigns), Some(1));
        assert_eq!(resolve_agent("Charlie", &slots, &callsigns), None);
    }

    #[test]
    fn tool_definitions_has_all_tools() {
        let defs = tool_definitions();
        let arr = defs.as_array().unwrap();
        let names: Vec<&str> = arr.iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"dispatch"));
        assert!(names.contains(&"terminate"));
        assert!(names.contains(&"merge"));
        assert!(names.contains(&"list_agents"));
        assert!(names.contains(&"list_repos"));
        assert!(names.contains(&"message_agent"));
        assert!(names.contains(&"strike_team"));
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn tool_result_serializes() {
        let result = ToolResult::Dispatched {
            slot: 1,
            callsign: "Alpha".to_string(),
            task_id: "t1".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"type\":\"dispatched\""));
        assert!(json.contains("\"slot\":1"));
    }

    #[test]
    fn format_tool_result_with_id() {
        let result = ToolResult::Agents { agents: vec![] };
        let formatted = format_tool_result(Some("call_1"), &result);
        assert!(formatted.contains("<tool_result id=\"call_1\">"));
        assert!(formatted.contains("</tool_result>"));
    }
}
