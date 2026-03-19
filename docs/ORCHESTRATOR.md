# Orchestrator

The orchestrator is a persistent LLM process (Claude) that acts as the central coordinator inside the dispatch console. It replaces the keyword-based command parser -- voice transcripts from the radio are piped directly to the orchestrator, which decides what to do via tool calls. The console executes those tool calls and returns structured results.

## Architecture

```
Android radio
  -> SpeechRecognizer (on-device STT)
  -> Raw transcript (no command parsing)
  -> WebSocket message: {"type":"send","text":"...","auto":true}
  -> Console WebSocket server
  -> WsEvent::VoiceTranscript
  -> Orchestrator LLM (stdin, stream-json)
  -> <tool_call> in response text
  -> Console executes tool, returns <tool_result>
  -> Orchestrator continues reasoning
```

## Spawn

The orchestrator is spawned on startup as a headless `claude` process with stream-json I/O:

```
claude -p --output-format stream-json --input-format stream-json \
  --verbose --no-config --max-turns 0 --system-prompt "..."
```

- **stdin**: JSON lines with user messages
- **stdout**: JSON lines with assistant responses (init, assistant, result)
- **stderr**: suppressed

The process stays alive as long as stdin remains open.

## System Prompt

The system prompt is built dynamically at startup from:
- Role description (central coordinator for voice-controlled agent system)
- Available repositories (from workspace scan)
- Tool definitions (JSON schema for all 7 tools)
- Tool call format (`<tool_call>...</tool_call>` tags)
- Decision guidelines (when to dispatch, plan, message, terminate, merge)

## Wire Protocol

**Input** (user messages sent to stdin):
```json
{"type":"user","content":"[MIC] refactor the auth module"}
```

Prefixes:
- `[MIC]` -- voice transcript from the radio
- `[EVENT] TASK_COMPLETE agent=Alpha task=t1` -- agent finished a task
- `[EVENT] MERGE_CONFLICT task=t1` -- merge failed with conflicts
- `[EVENT] AGENT_EXITED agent=Alpha slot=1` -- agent process died
- `<tool_result>...</tool_result>` -- result from a tool execution

**Output** (assistant responses from stdout):
```json
{"type":"assistant","message":{"content":[{"type":"text","text":"I'll dispatch an agent.\n<tool_call>{...}</tool_call>"}]}}
{"type":"result","subtype":"success","result":"..."}
```

## Available Tools

The orchestrator calls tools by embedding `<tool_call>` tags in its text response:

```json
<tool_call>{"name": "dispatch", "input": {"repo": "myrepo", "prompt": "fix the auth bug"}}</tool_call>
```

The console parses all `<tool_call>...</tool_call>` blocks from the response, executes them, and sends results back.

| Tool | Parameters | Description |
|------|-----------|-------------|
| `dispatch` | `repo`, `prompt` | Create a task, set up a git worktree, and dispatch an agent. |
| `terminate` | `agent` | Kill an agent by callsign or slot number. |
| `merge` | `task_id` | Merge a task's worktree branch into main. |
| `list_agents` | _(none)_ | List all active agent slots with status. |
| `list_repos` | _(none)_ | List available repositories. |
| `plan` | `repo`, `prompt` | Spawn a headless planner to decompose a complex prompt. |
| `message_agent` | `agent`, `text` | Send text to an agent's terminal (PTY). |

## State Machine

```
Idle -> Responding (user message sent via stdin)
Responding -> Idle (result message received on stdout)
Any -> Dead (process exited or stdout closed)
```

When the orchestrator is in `Responding` state, incoming messages (new voice transcripts, events) are queued and sent after the current turn completes.

## Orchestrator View

Pressing `o` in command mode shows the orchestrator conversation log with timestamped entries:

| Icon | Color | Source |
|------|-------|--------|
| MIC | Green | Voice transcript from radio |
| LLM | Magenta | Orchestrator reasoning text |
| TOOL | Yellow | Tool call issued by orchestrator |
| RESULT | Green/Red | Tool result sent back |
| PLAN | Yellow/Green | Planner start/complete |
| TASK | Cyan | Task created |
| ASSIGN | Yellow | Task assigned to agent |
| DONE | Green | Task completed |
| MERGE | Green | Worktree merged |
| CONFLICT | Red | Merge conflict |
| DISPATCH | Cyan | Agent launched |
| TERM | Red | Agent terminated |

## Headless Planner

The `plan` tool spawns a separate headless `claude -p` process to decompose complex prompts into subtasks. This is independent of the orchestrator -- it's a one-shot process that writes a task plan to `.dispatch/tasks.md` and exits. The orchestrator can call `plan` when it decides a prompt is too complex for a single agent.

## Header Bar

The header bar shows the orchestrator status:
- `ORCH: IDLE` -- waiting for input
- `ORCH: THINKING` -- processing a response
- `ORCH: DEAD` -- process exited
- `ORCH: OFF` -- orchestrator not spawned

## Error Recovery

If the orchestrator process dies, the console continues to function as a manual-mode TUI. Agents can still be dispatched and terminated via keyboard. The orchestrator status changes to `DEAD` / `OFF` in the header bar. A future enhancement could auto-respawn.

## Configuration

The orchestrator uses the configured `claude-code` tool command from `config.toml`:

```toml
[tools]
claude-code = "claude"
```

The `claude` command must support `--input-format stream-json --output-format stream-json` flags.
