# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches AI coding agents. The PC-side TUI gives you a live quad-pane view of embedded agent terminals. Voice a prompt and the orchestrator dispatches agents into isolated git worktrees -- agents do their work, commit, merge to main, clean up, and push.

## Overview

The system has two components:

1. **Dispatch Radio** (Android) -- a minimal push-to-talk app controlled via hardware volume buttons. Transcribes speech and sends raw transcripts over a local WebSocket connection to the console's orchestrator.
2. **Dispatch Console** (PC) -- a TUI command center with up to 26 embedded terminal panes (displayed 4 at a time in a 2x2 grid across pages), each running a live AI agent session. A persistent LLM orchestrator receives voice transcripts and decides what to do -- dispatch agents, send messages, merge completed work, etc. Supports direct keyboard input into any agent pane via a vim-style modal interface.

Both components live in a single monorepo.

```
┌──────────────┐    WebSocket TLS (LAN, PSK)   ┌──────────────────┐
│  Dispatch    │  ◄────────────────────────►   │  Dispatch        │
│  Radio       │                               │  Console         │
│  (Android)   │                               │  (PC TUI)        │
│              │                               │                  │
│  Volume keys │                               │  4x embedded     │
│  Speech-to-  │                               │  terminals (PTY) │
│  text        │                               │  Git worktrees   │
│              │                               │  .dispatch/      │
└──────────────┘                               └──────────────────┘
```

---

## Repository Structure

```
dispatch/
  radio/                      # Android app (Kotlin, Gradle)
  console/                    # PC TUI (Rust, Cargo)
    config.default.toml       # Default configuration template
  docs/
    SPEC.md                   # Full system specification
    ARCHITECTURE.md           # High-level architecture overview
    ORCHESTRATOR.md           # Orchestrator behavior and action reference
    AGENTS.md                 # Template injected into agent prompts
  README.md
```

**In the target repo** (created by dispatch at runtime):

```
sample-repo/
  .dispatch/
    .worktrees/        # Git worktrees for active agents
    MEMORY.md          # Shared agent memory (persistent knowledge base)
  (repo's own files)
```

The `.dispatch/` directory is gitignored by the console on first run.

### Shared Agent Memory

The console maintains a shared memory file at `.dispatch/MEMORY.md` in each target repo. This file persists knowledge across agent sessions -- build commands, architectural gotchas, environment quirks, and lessons learned.

**Reading**: When an agent is dispatched, the console reads `.dispatch/MEMORY.md` and appends its contents to the agent's system prompt. Agents benefit from prior learnings without any extra steps.

**Writing**: Agents are instructed (via `docs/AGENTS.md`) to append valuable learnings to the memory file after completing their work. Only high-value knowledge is written -- not routine observations.

**Lifecycle**: The memory file is created with an empty template on first agent dispatch. It is never automatically pruned. The file lives in `.dispatch/` (gitignored) so it persists locally across sessions but is not committed to the repository.

---

## Agent Naming

Agent names are defined in the `[agents]` section of `config.toml`. The list determines both the callsigns and the number of available slots. By default, the full NATO phonetic alphabet is used:

```toml
[agents]
callsigns = ["Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar", "Papa", "Quebec", "Romeo", "Sierra", "Tango", "Uniform", "Victor", "Whiskey", "X-ray", "Yankee", "Zulu"]
```

Users can customize this list with any names they prefer. The number of entries determines the slot count (4-26), and pages are allocated automatically (4 slots per page).

Callsigns are dynamically assigned from the configured list. Each new agent gets the next available callsign regardless of which slot it occupies. When an agent is terminated, its callsign returns to the pool and can be reused by the next dispatch.

Agents can be renamed by the orchestrator. Custom names replace the assigned callsign until the agent is terminated.

Callsigns are the primary identifier for voice commands. All agents are addressable by voice regardless of which page is currently displayed in the console.

## Identity

The `[identity]` section of `config.toml` configures display names for the user and the console/orchestrator:

```toml
[identity]
user_callsign = "Dispatch"
console_name = "Console"
```

- **user_callsign**: The user's display name. Shown in the radio chat log for voice transcripts and used in orchestrator/agent prompts to address the user. Default: `"Dispatch"`.
- **console_name**: The console/orchestrator's display name. Shown in the radio chat log for orchestrator messages and used as the sender label for system decisions. Default: `"Console"`.

Both names are propagated to the radio app via the `agents` response (fields `user_callsign` and `console_name`), so the radio can display and color-code them correctly.

---

## Voice Commands

The radio sends raw voice transcripts to the console without any local parsing. The console's persistent LLM orchestrator (see `docs/ORCHESTRATOR.md`) receives these transcripts and decides what to do -- dispatch agents, send messages, terminate agents, etc.

### How It Works

1. User speaks into the radio (push-to-talk or continuous listening).
2. Radio transcribes speech via Android `SpeechRecognizer`.
3. Raw transcript is sent to the console as `{"type":"send","text":"...","auto":true}`.
4. Console forwards the transcript to the orchestrator as `[MIC] <transcript>`.
5. Orchestrator decides what action(s) to take and issues tool calls.
6. Console executes the tool calls and returns results to the orchestrator.

### Examples

| Utterance                                      | Orchestrator action                          |
|------------------------------------------------|----------------------------------------------|
| "Alpha, can you refactor the auth module"      | `message_agent` to Alpha                     |
| "dispatch an agent to fix the login bug"       | `dispatch` with prompt                       |
| "terminate bravo"                              | `terminate` agent=Bravo                      |
| "what agents are running"                      | `list_agents`                                |
| "refactor the auth system"                     | `dispatch` agent with prompt                 |
| "merge alpha's work"                           | `merge` to acknowledge agent merged          |

The orchestrator understands natural language -- there are no fixed command patterns. It uses the full context of the conversation (agent states, prior tool results, etc.) to decide the best action.

---

## Agent Dispatch

The orchestrator dispatches agents via the `dispatch` tool. Each agent works in an isolated git worktree, does its work, commits, merges to main, cleans up the worktree, and pushes. No task files, no task IDs, no dependency tracking.

### Git Worktrees

Each agent runs in an isolated git worktree to prevent agents from stepping on each other. Agents create and manage their own worktrees.

**On dispatch:**

The agent's PTY is launched in the repo root. The agent creates its own worktree and works there.

**Idle detection:**

The console monitors each agent's PTY output to detect activity. When an agent with a task produces no output for 10 seconds, it transitions to "idle" state -- this typically means the agent finished its work and is sitting at a prompt. The orchestrator is notified via an `[EVENT] AGENT_IDLE` message on each working-to-idle transition. Activity status is reported in `list_agents` as "working" or "idle" and shown in the TUI pane info strip.

**On completion:**

The agent merges its own branch back to main, removes the worktree, deletes the branch, and pushes. If the merge has conflicts, the agent stops and returns to the prompt.

**On agent termination:**

If an agent is terminated before completing its work, the worktree and branch are preserved.

The `.dispatch/` directory is gitignored.

### Multi-Repo Mode

Dispatch supports two workspace modes:

**Single-repo mode** (default): Launched inside a git repo. Behaves as documented above -- one repo, `.dispatch/` in the repo root.

**Multi-repo mode**: Launched from a directory that is not itself a git repo. Dispatch scans immediate children for directories containing `.git` and holds the list in memory. No `.dispatch/` or workspace-level artifacts are created at the parent directory level.

In multi-repo mode:

- Each agent slot tracks its own `repo_root`. Worktree operations use the slot's repo, not a global root.
- The header bar shows the repo count.

### Orchestrator Tool Interface

The console exposes a set of actions that the orchestrator LLM can invoke to manage the dispatch system. The orchestrator emits action blocks (JSON wrapped in ` ```action ` fenced code blocks), which the console parses and executes.

**Action block format:**

````
```action
{"action": "dispatch", "repo": "myrepo", "prompt": "fix the auth bug", "callsign": "Alpha"}
```
````

The console parses the `"action"` field to determine which tool to execute. Parameters vary by action type (see table below).

**Available actions:**

| Action | Parameters | Description |
|--------|-----------|-------------|
| `dispatch` | `repo`, `prompt`, `callsign` (optional), `tool` (optional) | Dispatch an agent with a prompt. The agent creates its own worktree. Returns slot and callsign. Omit `tool` to use the configured AI agent from `[tools] ai_agent`. Only specify `tool` (`"claude"` or `"copilot"`) to override for a specific dispatch. |
| `terminate` | `agent` | Kill an agent by callsign or slot number. Frees the slot. |
| `merge` | `task_id` | Acknowledge that an agent has merged its branch. |
| `list_agents` | _(none)_ | List all active agent slots with callsign, tool, working/idle status, and repo. |
| `list_repos` | _(none)_ | List available repositories that agents can work in. |
| `message_agent` | `agent`, `text` | Send text to an agent's terminal (PTY). Use for follow-up instructions or answering agent questions. |
| `strike_team` | `spec_file` (required), `repo` (required), `name` (optional) | Launch a Strike Team: break a spec into tasks with dependencies, then dispatch agents in parallel waves until all tasks are complete. See [Strike Team](#strike-team). |

The `agent` parameter accepts either a callsign (e.g. "Alpha") or a slot number (e.g. "1"), case-insensitive.

### Destructive Action Safeguard

The `terminate` action is gated by user input authentication. The orchestrator LLM can only execute `terminate` when the current turn was triggered by an authentic voice or text message from Dispatch (a `[MIC]` message). If the orchestrator attempts to terminate an agent in response to a system event (`[EVENT]`, `[AGENT_MSG]`, or `<tool_result>`), the action is blocked with an error.

This prevents the LLM from hallucinating fake user messages and self-authorizing destructive actions. Each message sent to the orchestrator is tagged as user-originated or system-originated, and the tag is tracked through the pending queue so it correctly reflects the trigger for each turn.

When a terminate is blocked, the ticker displays "BLOCKED: orchestrator tried to terminate without user input" and a chat message is sent to the radio.

### Ticker

A single-line LED-style scrolling marquee between the header bar and the quad panes. Text scrolls right-to-left continuously. Messages queue up -- when one finishes scrolling off, the next starts. When idle, the line is blank.

**Message sources:**

- Agent events: `Alpha dispatched to myrepo`, `Bravo merged to main`
- Merge results: `Alpha merged to main` or `Alpha merge conflict, needs manual review`
- Errors: `All agent slots full`

**Rendering:** fixed-width viewport, text offset decremented each frame tick (e.g. every 50ms). Once a message scrolls fully off-screen, it is discarded and the next queued message begins. If multiple messages queue up during a burst, they scroll sequentially with a small gap between them.

### Agent Visibility

The console displays agent state across multiple areas:

- **Header bar**: active agent count, current page indicator, clock.
- **Ticker**: real-time event stream (dispatch, merges, errors).
- **Pane info strip**: each pane shows its callsign, tool, and status.
- **Orchestrator view** (`o` key): toggles the main area between the 2x2 agent grid and a scrollable orchestrator event log showing voice transcripts, reasoning decisions, and tool calls in real time.

### Orchestrator View

Pressing `o` in command mode replaces the 2x2 agent grid with a full-height scrollable log of orchestrator events. Each entry is timestamped and categorized:

- **MIC**: incoming voice transcripts from the radio.
- **MERGE**: branch merged to main.
- **CONFLICT**: merge conflict detected.
- **DISPATCH**: agent launched into a slot.
- **TERM**: agent terminated.

Pressing `o` again returns to the agent grid. While in the orchestrator view, `Up`/`Down` and `PageUp`/`PageDown` scroll through history. The footer shows contextual hints for the active view mode.

### Prompt History and Logging

All voice prompts from the radio and keyboard input submitted in input mode are recorded with timestamps to `.dispatch/prompt_history.log`. The log is append-only, human-readable, and persists across sessions.

**Log format:**

```
[14:32:05] VOICE -> ALPHA: "refactor the auth module"
[14:35:12] KEYBOARD -> ALPHA: "fix the typo in line 42"
[14:38:00] VOICE -> orchestrator: "set up CI pipeline for all microservices"
```

**Keyboard input tracking:** in input mode, the console maintains a shadow buffer of typed characters. When Enter is pressed, the accumulated text is saved to the history log. The shadow buffer is cleared on mode exit (Escape).

---

## Strike Team

A coordinated multi-agent execution mode that takes a spec or feature design document, breaks it into tasks with dependencies, then dispatches agents in parallel waves -- maximizing throughput while respecting task ordering.

### Lifecycle

```
Idle --> Planning --> Executing --> Complete
                 \-> Aborted (planner error)
```

### How It Works

1. The orchestrator issues a `strike_team(spec_file, name, repo)` action.
2. The console dispatches a **planner agent** that reads the spec file and creates a task file (`.dispatch/tasks-<name>.md`) with dependency information.
3. Once the planner finishes, the console parses the task file and transitions to the Executing phase.
4. The console scans for **ready** tasks (status `pending` with all dependencies `done`) and dispatches agents in parallel for each ready task that has an available slot.
5. When an agent finishes, it merges to main and is terminated to free the slot.
6. The console pulls the latest main, checks for newly unblocked tasks, and dispatches the next wave.
7. Repeats until all tasks are `done` or `failed`.

### Task File Format

Location: `.dispatch/tasks-<name>.md`

```markdown
# Strike Team: auth-system
spec: docs/auth-spec.md

## T1: Implement user model
status: pending
dependencies: none
prompt: Create a User struct in src/models/user.rs.
  Fields: id (Uuid), email (String), name (String), created_at (DateTime).
  Derive serde Serialize/Deserialize and Debug.
  Add a User::new(email, name) constructor that generates the id and timestamp.

## T2: Add user API endpoints
status: pending
dependencies: T1
prompt: Create REST endpoints for CRUD operations on users in src/routes/users.rs.
  Add routes for GET /users, POST /users, GET /users/:id, DELETE /users/:id.
  Return JSON responses with proper HTTP status codes.

## T3: Add authentication middleware
status: pending
dependencies: T1
prompt: Implement JWT authentication middleware in src/middleware/auth.rs.

## T4: Wire auth into endpoints
status: pending
dependencies: T2, T3
prompt: Apply auth middleware to user endpoints.
  Protect POST, DELETE routes with JWT validation.
  Add integration tests covering authenticated and unauthenticated requests.
```

**Fields per task:**

| Field | Values | Description |
|-------|--------|-------------|
| `status` | `pending`, `active`, `done`, `failed` | Current state |
| `dependencies` | `none` or comma-separated IDs (`T1, T3`) | Dependency list |
| `prompt` | First line after `prompt:`, with 2-space indented continuation lines | Self-contained agent instruction (multi-line) |
| `agent` | Callsign (e.g., `Alpha`) | Written by console when assigned |

**Readiness rule:** a task is ready when its status is `pending` and all dependencies have status `done`.

**Parsing:** line-by-line string matching. `## T<N>:` starts a task, `key: value` lines set fields. Prompt continuation lines are indented with 2+ spaces. No markdown parsing library needed.

### Execution Loop

Runs inside the existing 16ms main loop tick -- no new threads or async.

1. `git pull --ff-only` in repo root (pick up prior merges from completed agents).
2. Scan tasks: find all where status=`pending` and all deps are `done`.
3. For each ready task with an available slot: dispatch a fresh agent with the task's prompt and a reference to the original spec file for context.
4. Update task file: status=`active`, agent=`<callsign>`.
5. When an agent goes idle (existing 10s idle detection):
   - Mark task `done` in the task file.
   - Terminate the agent (free the slot for next wave).
   - Re-run from step 1.
6. When an agent process exits unexpectedly: mark task `failed`, continue.
7. When all tasks are `done` or `failed`: transition to Complete and notify the orchestrator via `[EVENT] STRIKE_TEAM_COMPLETE name=<name> result=<done>/<total>`.

Each task agent follows the normal dispatch workflow: creates a worktree from latest main, works on its task, merges to main, pushes, cleans up, and goes idle. On idle detection, the console terminates the agent to free the slot for the next wave.

### UI Indicators

- **Header bar**: `STRIKE TEAM 3/7` (done/total) appended when active.
- **Pane info strip**: task ID next to callsign, e.g., `Alpha [T3]`.
- **Ticker messages** at lifecycle events:
  - `STRIKE TEAM: planning <name>...`
  - `STRIKE TEAM: plan ready, 7 tasks`
  - `STRIKE TEAM: T3 -> Alpha`
  - `STRIKE TEAM: T3 done (Alpha)`
  - `STRIKE TEAM: complete (7/7)`

### Edge Cases

- **Max slots full**: ready tasks wait. As agents finish and slots free up, the next wave dispatches.
- **Agent failure**: task marked `failed`. Its dependents stay `pending` forever (blocked by the failed dependency). Sibling tasks continue normally.
- **Merge conflicts**: agents handle conflicts per their normal workflow. If unresolvable, the agent reports failure.
- **Cancellation**: press `s` in command mode to abort the active strike team. This transitions to the Aborted phase, stopping all future task dispatching. Active agents finish their current work but no new tasks are dispatched.
- **One at a time**: only one strike team can be active at once. A second `strike_team` call returns an error.

---

## Protocol

Communication happens over a single WebSocket connection. Messages are JSON. Either side can initiate messages.

### mDNS / Zeroconf Discovery

The console advertises itself on the local network via mDNS (DNS-SD) as a `_dispatch._tcp.local.` service. The service name is the console's hostname. The radio discovers this service using Android's `NsdManager` API, eliminating the need for manual IP entry.

- **Console**: uses the `mdns-sd` crate to register the service on startup. The service is advertised on all network interfaces with automatic address detection.
- **Radio**: the Settings screen has a "DISCOVER CONSOLE" button that scans for `_dispatch._tcp.` services for up to 5 seconds. When found, the host and port fields are auto-filled.

Manual IP/port entry remains available as a fallback.

### TLS

The WebSocket server uses TLS (`wss://`) for encrypted transport. On first run, the console generates a self-signed certificate and private key, stored as DER files in the config directory (`~/.config/dispatch/cert.der` and `key.der`). The certificate covers the SANs `dispatch.local` and `localhost`.

The radio pins the certificate by its SHA-256 fingerprint rather than relying on a CA chain. When no fingerprint is available (manual connection), the radio trusts any certificate -- the PSK still authenticates the connection, and TLS provides encryption.

### Authentication

The WebSocket handshake includes a pre-shared key as a query parameter:

```
wss://192.168.1.x:9800/?psk=<key>
```

The console generates a random PSK on first run and stores it in `~/.config/dispatch/config.toml`. The key is displayed on the console's header bar (truncated, expandable with `p`). Any connection attempt with an invalid or missing PSK is rejected with a 401 before the WebSocket upgrade completes.

**Connection info overlay:** Press `x` in command mode to display a connection info overlay showing the console's local IP address, port, and full PSK. The host is auto-detected from the machine's local network interface. Use this information to manually configure the radio app's connection settings.

### Message Types

**List agents**

```
-> { "type": "list_agents" }
<- {
     "type": "agents",
     "slots": [
       { "slot": 1, "callsign": "Alpha", "tool": "claude", "status": "busy" },
       { "slot": 2, "callsign": "Bravo", "tool": "claude", "status": "idle" },
       { "slot": 3, "callsign": "Charlie", "tool": "copilot", "status": "idle" },
       { "slot": 4, "callsign": null, "tool": null, "status": "empty" }
     ],
     "target": 1
   }
```

Slots are numbered 1-26. Only active (dispatched) and empty slots on allocated pages are included.

**Set target**

```
-> { "type": "set_target", "slot": 2 }
<- { "type": "target_changed", "slot": 2, "callsign": "Bravo" }
```

**Send prompt**

```
-> { "type": "send", "text": "refactor the auth module to use JWT" }
<- { "type": "ack", "slot": 1, "callsign": "Alpha" }
```

Sent to the current target.

**Send prompt to specific agent**

```
-> { "type": "send", "text": "write unit tests", "slot": 3 }
<- { "type": "ack", "slot": 3, "callsign": "Charlie" }
```

**Send prompt via orchestrator**

```
-> { "type": "send", "text": "set up the CI pipeline", "auto": true }
<- { "type": "ack", "slot": 2, "callsign": "Bravo" }
```

`auto: true` tells the console to route through the orchestrator, which decides what to do.

**Dispatch new agent**

```
-> { "type": "dispatch", "tool": "claude", "slot": 3 }
<- { "type": "dispatched", "slot": 3, "callsign": "Charlie", "tool": "claude" }
```

**Terminate agent**

```
-> { "type": "terminate", "slot": 2 }
<- { "type": "terminated", "slot": 2, "callsign": "Bravo" }
```

**Rename agent**

```
-> { "type": "rename", "slot": 2, "callsign": "Jenkins" }
<- { "type": "renamed", "slot": 2, "callsign": "Jenkins" }
```

**Radio status**

```
-> { "type": "radio_status", "state": "listening" }
-> { "type": "radio_status", "state": "idle" }
```

**Send image to agent**

```
-> { "type": "send_image", "callsign": "Alpha", "data": "<base64>", "filename": "screenshot.png" }
<- { "type": "ack", "slot": 0, "callsign": "Alpha", "task": "image_received" }
```

Sends a base64-encoded image targeted at a specific agent by callsign. The console saves the image to `.dispatch/images/` in the repo root and writes the file path to the agent's PTY so it can view the image. The radio app provides gallery and camera capture as image sources.

**Chat (server push)**

```
<- { "type": "chat", "sender": "Console", "text": "Dispatched agent Alpha." }
<- { "type": "chat", "sender": "Alpha", "text": "Merged to main." }
<- { "type": "chat", "sender": "Dispatch", "text": "refactor the auth module" }
```

Pushed to all connected clients whenever the orchestrator produces text or other significant events occur. Not a response to any request -- the console pushes these proactively. The `sender` field identifies who said it: the configured `user_callsign` (default `"Dispatch"`) for voice transcripts, the configured `console_name` (default `"Console"`) for orchestrator decisions, or an agent callsign (e.g. `"Alpha"`) for agent status messages.

**Agent status messages:** Agents write messages to `.dispatch/messages/{callsign}` files by appending lines via `echo "message" >> "$DISPATCH_MSG_FILE"`. The console polls these files for new content and broadcasts each new line as a chat message with the agent's callsign as the sender. Agent messages are also forwarded to the orchestrator LLM as `[AGENT_MSG] Callsign: text` so it can track agent progress. Agents are instructed to emit these at key workflow points (started, completed, merged).

**Error**

```
<- { "type": "error", "message": "all agent slots full" }
```

### Design Notes

- All messages are JSON in WebSocket text frames.
- Unknown message types are silently ignored for forward compatibility.
- Messages include an optional `seq` field for request-response correlation.
- The radio re-requests `list_agents` on reconnect to sync state.
- The `chat` message type is a server push -- it is sent without a corresponding request. The WebSocket server uses a broadcast channel to push chat messages to all connected clients simultaneously.

---

## Dispatch Console (PC TUI)

### Target

- Rust
- Dependencies: `ratatui`, `crossterm`, `tokio`, `tokio-tungstenite`, `serde`, `serde_json`, `toml`, `portable-pty`, `vt100`, `dirs`, `mdns-sd` (mDNS advertisement), `hostname`
- Single binary, cross-platform (Windows, macOS, Linux)

### Embedded Terminals

Each agent pane is a real terminal emulator, not a text capture. The console manages PTYs directly -- there is no tmux dependency.

**Platform support:**

- **Linux/macOS**: uses native Unix PTYs (via `openpty`).
- **Windows**: uses ConPTY (Console Pseudo Terminal), available on Windows 10 1809+. ConPTY is what Windows Terminal uses internally. The `portable-pty` crate (by the WezTerm author) abstracts over both backends behind a single API, so application code is platform-agnostic.

The rest of the stack is also cross-platform: `crossterm` for terminal input/output, `ratatui` for rendering, `vt100` for escape sequence parsing (it operates on byte streams, so it's OS-independent). Claude Code and GitHub Copilot CLI both support Windows natively.

**Architecture per slot:**

1. **PTY**: created via `portable-pty`. A child process (e.g. `claude`) runs inside the PTY.
2. **VTE parser**: the `vt100` crate maintains a virtual terminal grid by processing the PTY's output stream. This correctly handles escape sequences, colors, cursor movement, scrollback, and alternate screen buffers.
3. **Renderer**: `ratatui` reads the `vt100::Screen` grid and renders it into the pane widget, mapping terminal colors to ratatui styles.
4. **Input**: in input mode, keystrokes are written directly to the PTY file descriptor. This is instantaneous -- no subprocess spawning, no tmux.

This makes each pane a fully interactive terminal. You see exactly what you'd see in a normal terminal emulator, including color output, progress bars, and TUI applications like Claude Code's interface.

### Visual Design

911 dispatch / command center aesthetic. High information density, dark background, status-driven color.

**Color language:**

| Color          | Meaning                        |
|----------------|--------------------------------|
| Green          | Connected, active, healthy     |
| Amber/Yellow   | Busy, processing, in-progress  |
| Red            | Disconnected, error, alert     |
| Cyan           | Targeted (receiving next radio prompt) |
| White on dark  | Default text                   |
| Dim grey       | Secondary info, IDs, timestamps|

### Layout

The console displays four agent panes at a time in a 2x2 grid. With more than four agents, panes are spread across multiple pages. All agents remain active regardless of which page is visible -- off-screen agents keep running and are still addressable by voice.

**Page structure:**

- Page 1: slots 1-4 (Alpha through Delta)
- Page 2: slots 5-8 (Echo through Hotel)
- Page 3: slots 9-12 (India through Lima)
- ...up to page 7 (26 slots max)

Pages are cycled with `Left` / `Right` arrow keys. The header shows the current page and total pages.

```
┌─ DISPATCH ──────────────────────────────────────────────────────────┐
│ RADIO: ● CONNECTED   PSK: a7f3...  PAGE 1/2                14:32    │
│ ◄◄ Alpha dispatched to myrepo... Bravo merged to main               │
├────────────────────────────────┬────────────────────────────────────┤
│ ▸ [1] ALPHA                    │ [2] BRAVO                          │
│   CLAUDE-CODE | busy           │ CLAUDE-CODE | idle                 │
│   dispatched 14:20 | 12m03s    │ dispatched 14:28 | 4m11s           │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │
│ ~/project$ claude              │ ~/project$ claude                  │
│                                │                                    │
│ > I'll start by updating the   │ > I'll create a comprehensive      │
│   auth middleware. First, let  │   test suite covering the core     │
│   me examine the current...    │   payment flows...                 │
│                                │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┼────────────────────────────────────┤
│ [3] CHARLIE                    │ [4] DELTA                          │
│ COPILOT | idle                 │ CLAUDE-CODE | busy                 │
│ dispatched 14:15 | 17m12s      │ dispatched 14:30 | 2m04s           │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │
│ ~/project$ copilot --yolo  │ ~/project$ claude                  │
│                                │                                    │
│ ? What would you like help     │ > Setting up the CI pipeline...    │
│   with?                        │                                    │
│ █                              │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┴────────────────────────────────────┤
│ ▸ RADIO IDLE │ TARGET: ALPHA │ ⏎ input │ ←→ page │ ?               │
└─────────────────────────────────────────────────────────────────────┘
```

Page 2 of the same session:

```
┌─ DISPATCH ──────────────────────────────────────────────────────────┐
│ RADIO: ● CONNECTED   PSK: a7f3...  PAGE 2/2                14:32    │
│ ◄◄ Echo merged to main                                              │
├────────────────────────────────┬────────────────────────────────────┤
│ [5] ECHO                       │ [6] FOXTROT                        │
│ CLAUDE-CODE | busy             │ CLAUDE-CODE | busy                 │
│ dispatched 14:31 | 1m22s       │ dispatched 14:32 | 0m15s           │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │
│ ~/project$ claude              │ ~/project$ claude                  │
│                                │                                    │
│ > Analyzing the database       │ > Starting linter configuration    │
│   schema for migration...      │   ...                              │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┼────────────────────────────────────┤
│ [7] ── STANDBY ──              │ [8] ── STANDBY ──                  │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┴────────────────────────────────────┤
│ ▸ RADIO IDLE │ TARGET: ALPHA │ ⏎ input │ ←→ page │ ?               │
└─────────────────────────────────────────────────────────────────────┘
```

**Auto-navigate:** when you address an agent by voice or select a slot number that's on a different page, the console automatically switches to that page. Targeting Alpha while viewing page 2 flips back to page 1.

**Input mode** changes the footer and the targeted pane's border:

```
┌─ DISPATCH ──────────────────────────────────────────────────────────┐
│ RADIO: ● CONNECTED   PSK: a7f3...  PAGE 1/2                14:32    │
│ ◄◄ Alpha merged to main                                             │
├────────────────────────────────┬────────────────────────────────────┤
│ ┃ [1] ALPHA                    │ [2] BRAVO                          │
│ ┃ CLAUDE-CODE | busy           │ ...                                │
│ ┃ dispatched 14:20 | 12m03s    │                                    │
│ ┃┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄      │                                    │
│ ┃~/project$ claude             │                                    │
│ ┃                              │                                    │
│ ┃> I'll start by updating...   │                                    │
│ ┃                              │                                    │
│ ┃█                             │                                    │
│ ┃                              │                                    │
├────────────────────────────────┼────────────────────────────────────┤
│ ...                            │ ...                                │
├────────────────────────────────┴────────────────────────────────────┤
│ -- INPUT (ALPHA) --                                  ESC to exit   │
└─────────────────────────────────────────────────────────────────────┘
```

Bright green border on the active pane. Footer shows mode indicator.

**Regions:**

1. **Header bar** -- radio connection state, PSK (truncated), current page indicator, clock.
2. **Ticker** -- single-line LED-style scrolling marquee. Shows agent events, merge results, and errors. Text scrolls right-to-left. Blank when idle. See [Ticker](#ticker).
3. **Quad pane** -- four slots from the current page. Targeted pane has `▸` marker and cyan border (command mode) or green border (input mode). Each pane has:
   - **Info strip**: callsign, tool type, status (busy/idle), dispatch time, and runtime.
   - **Terminal area**: live embedded terminal output rendered from the VTE parser.
   - Empty slots show "STANDBY".
4. **Footer bar** -- command mode: radio state, target (regardless of page), context-sensitive hotkey hints (occupied slots show Enter, arrow keys, x, p, k, o, ?, q; empty slots show n, o, ?, q). Input mode: `-- INPUT ({CALLSIGN}) --` with ESC hint.

### Input Model

Modal, vim-style. Two modes:

**Command mode** (default) -- keystrokes control the console.

**Input mode** -- keystrokes are written directly to the targeted agent's PTY. The terminal in the pane is fully interactive: you can type prompts, use arrow keys, tab completion, Ctrl+C to cancel, scroll through output -- everything. Because writes go straight to the PTY file descriptor, there is zero latency overhead.

| Transition       | Key         | Behavior                                           |
|------------------|-------------|----------------------------------------------------|
| Command -> Input | `Enter`     | Enter input mode on the currently targeted pane    |
| Input -> Command | `Escape`    | Return to command mode (immediate)                 |

While in input mode, `Escape` is the only key intercepted by the console -- it immediately returns to command mode. Everything else goes to the PTY. To send a literal Escape to the PTY, double-tap `Escape` quickly (within 300ms): the first press exits input mode, the second press in command mode sends `\x1b` to the targeted pane.

**Radio commands during input mode:** voice commands from the radio are always processed regardless of console mode. The two input channels (keyboard and radio) operate independently.

#### Command Mode Keys

| Key               | Action                                                       |
|-------------------|--------------------------------------------------------------|
| `Enter`           | Enter input mode on targeted pane                            |
| `1-4`             | Select target slot on current page (slot = page offset + key)|
| `Tab`             | Cycle target forward across all pages (skips empty slots, auto-navigates) |
| `Shift+Tab`       | Cycle target backward across all pages                       |
| `Right`             | Next page                                                  |
| `Left`              | Previous page                                              |
| `n`               | Spawn new agent in empty targeted slot (next available callsign) |
| `c`               | Interrupt orchestrator (cancel current response, restart)     |
| `k`               | Kill agent in currently targeted slot (confirms first)       |
| `s`               | Abort active strike team (stops dispatching new tasks)       |
| `o`               | Toggle orchestrator view (replaces agent grid with event log) |
| `p`               | Show/hide full PSK                                           |
| `x`               | Show connection info overlay (address, port, PSK)            |
| `q`               | Quit (confirms if agents are running)                        |
| `PgUp` / `PgDn`   | Scroll pane output up/down (half-page increments)            |
| `?`               | Toggle help overlay                                          |

`Tab` / `Shift+Tab` cycle through all active agents across all pages, not just the current page. The view auto-navigates to the page containing the newly targeted agent. `1-4` always refer to the four slots on the current page -- so pressing `2` on page 2 selects slot 6 (Foxtrot).

### PTY Management

The console manages process lifecycles directly. No tmux.

**Agent dispatch:**

```rust
// Pseudocode
let pty = portable_pty::native_pty_system().open_pty(PtySize { rows: 24, cols: 80 })?;
let child = pty.slave.spawn_command(CommandBuilder::new("claude"))?;
let reader = pty.master.try_clone_reader()?;
let writer = pty.master.take_writer()?;
let vte = vt100::Parser::new(24, 80, 1000); // rows, cols, scrollback
```

**Output processing:**

A tokio task per slot reads from the PTY reader and feeds bytes into the `vt100::Parser`. The parser maintains a `Screen` object representing the current terminal state. The ratatui render loop reads from this screen on each frame.

**Scrollback:** the `vt100::Parser` is initialized with a scrollback buffer (default 1000 lines, configurable via `terminal.scrollback_lines`). In command mode, `PgUp`/`PgDn` scroll the targeted pane by half-page increments. A `SCROLL` indicator appears when not at the bottom. Scrollback resets to the bottom on new output or when entering input mode.

**Input forwarding (input mode):**

Keystrokes from crossterm are translated to ANSI sequences and written to the PTY writer. Regular characters are written as-is. Special keys are mapped:

| Key              | ANSI sequence    |
|------------------|------------------|
| Enter            | `\r`             |
| Backspace        | `\x7f`          |
| Tab              | `\t`             |
| Up               | `\x1b[A`        |
| Down             | `\x1b[B`        |
| Right            | `\x1b[C`        |
| Left             | `\x1b[D`        |
| Home             | `\x1b[H`        |
| End              | `\x1b[F`        |
| Ctrl+C           | `\x03`          |
| Ctrl+D           | `\x04`          |
| Ctrl+L           | `\x0c`          |

These ANSI sequences are universal -- they're written to the PTY, not the host terminal, so they work identically on Windows (ConPTY), macOS, and Linux. The host terminal differences are handled by `crossterm` on the input side.

**Prompt injection (from voice or orchestrator):**

When a prompt arrives from the radio (or from the orchestrator), it is written to the PTY as if typed, followed by `\r` (Enter). This happens regardless of the console's current input mode.

```rust
writer.write_all(format!("{}\r", prompt_text).as_bytes())?;
```

**Terminal resize:**

When the console window is resized, each PTY is notified via `pty.master.resize()` and the VTE parser dimensions are updated. The terminal content reflows.

Resize events are debounced with a **100ms delay**. When a resize event arrives, any pending resize is cancelled and a fresh 100ms timer starts. Only after 100ms of no further resize events are the PTYs and `vt100::Parser` dimensions actually updated. This prevents rendering artifacts from resize storms during window drag on all platforms (including Windows ConPTY, which has higher per-resize overhead).

```rust
// Pseudocode: debounced resize in the event loop
let mut resize_deadline: Option<tokio::time::Instant> = None;

// On crossterm resize event:
resize_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(100));

// In the tick/select loop, after 100ms elapses:
if let Some(deadline) = resize_deadline {
    if tokio::time::Instant::now() >= deadline {
        resize_deadline = None;
        let (cols, rows) = crossterm::terminal::size()?;
        for slot in active_slots.iter_mut() {
            slot.pty_master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
            slot.vte_parser.set_size(rows, cols);
        }
    }
}
```

**Agent termination:**

The child process is killed, the PTY is closed, and the slot is marked empty. The worktree and branch are preserved.

### Configuration

Stored in a platform-appropriate config directory, auto-generated on first run:

- **Linux**: `~/.config/dispatch/config.toml`
- **macOS**: `~/Library/Application Support/dispatch/config.toml`
- **Windows**: `%APPDATA%\dispatch\config.toml`

The `dirs` crate handles path resolution. A default configuration template is provided at `console/config.default.toml`.

```toml
[server]
port = 9800
bind = "0.0.0.0"

[auth]
# Auto-generated on first run. Run `dispatch regenerate-psk` to rotate.
psk = "a7f3e9b1c4d8..."

[terminal]
scrollback_lines = 1000

[agents]
# Agent names, in slot order. The number of entries determines the slot count.
# Pages are allocated automatically (4 slots per page).
callsigns = ["Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliet", "Kilo", "Lima", "Mike", "November", "Oscar", "Papa", "Quebec", "Romeo", "Sierra", "Tango", "Uniform", "Victor", "Whiskey", "X-ray", "Yankee", "Zulu"]

[tools]
ai_agent = "claude"
claude = "claude"
copilot = "copilot"
```

The console automatically adds tool-specific flags when spawning agents: `--dangerously-skip-permissions` and `--system-prompt` for Claude, `--yolo` for Copilot (auto-accepts all permissions for autonomous operation).

### Channels

Channels save and restore complete console configurations. Each channel is a named copy of `config.toml` stored in the `channels/` subdirectory of the config directory. Use channels to maintain different setups for different machines, contexts, or projects.

```sh
dispatch channel save home       # Snapshot current config as "home"
dispatch channel load work       # Overwrite config.toml with "work" channel
dispatch channel list            # Show all saved channels with summary
dispatch channel show home       # Print contents of "home" channel
dispatch channel delete old      # Remove a saved channel
```

Channel names are sanitized to lowercase alphanumeric characters and hyphens. Loading a channel overwrites `config.toml`; restart the console to apply.

### CLI

```
dispatch                        # Start the console (default)
dispatch regenerate-psk         # Generate a new PSK
dispatch show-psk               # Print the current PSK to stdout
dispatch edit-config            # Open config.toml in VS Code
dispatch channel save <name>    # Save current config as a channel
dispatch channel load <name>    # Load a saved channel
dispatch channel list           # List saved channels
dispatch channel delete <name>  # Delete a saved channel
dispatch channel show <name>    # Print channel contents
```

---

## Dispatch Radio (Android)

### Target

- Kotlin
- Minimum SDK: API 28 (Android 9)
- Single-activity architecture
- Dependencies: OkHttp (WebSocket client)

### Interaction Model

Primary controls are hardware volume buttons with haptic feedback.

**Volume Down -- Push-to-Talk**

| Event     | Action                                                    |
|-----------|-----------------------------------------------------------|
| Key down  | Start `SpeechRecognizer`, show listening indicator, short vibration, send `radio_status: listening` |
| Held      | Partial transcription results displayed on screen         |
| Key up    | Stop recognizer, send raw transcript to console, confirm vibration, send `radio_status: idle` |

If the transcript is empty, double-pulse vibration, no message sent.

**Volume Up -- Agent Status Overlay**

| Event     | Action                                                    |
|-----------|-----------------------------------------------------------|
| Key down  | Show hold-to-view status overlay listing all active agents sorted by dispatch time. Each line: callsign (left, white, monospace bold), status (right, RED for Busy / YELLOW for Idle, monospace bold). Overlay stays visible for entire hold duration. |
| Key up    | Dismiss the status overlay, short vibration.              |

### UI Layout

Minimal, high-contrast, dark theme. Uppercase labels, monospaced accents.

```
┌─────────────────────────────┐
│  DISPATCH RADIO             │
│  ● CONNECTED                │
│                             │
│  TARGET                     │
│  [1] ALPHA                  │  <- callsign, large
│  CLAUDE-CODE | busy         │  <- tool + status
│                             │
│  ┌───────────────────────┐  │
│  │   ◉ LISTENING        │  │
│  │   ░░░░░███████░░░░░░  │  │
│  └───────────────────────┘  │
│                             │
│  LOG                        │
│  Dispatch: refactor the      │  <- scrollable chat log
│  Console: Dispatching       │
│    Alpha.                   │
│  Console: Dispatched        │
│    agent Alpha.             │
│  Alpha: Merged to main.     │
│                             │
│  AGENTS                     │
│  ▸α  β  χ  δ  ε  φ          │  <- scrollable, initials for all active agents
│                             │
└─────────────────────────────┘
```

### Settings

- **Connection profiles**: save and load named connection profiles (host, port, PSK). Switch between different consoles without re-entering credentials. Profiles are stored in SharedPreferences and persist across app restarts. Profile names are sanitized to lowercase alphanumeric and hyphens. The active profile name is displayed in the settings screen; it clears automatically if connection fields are manually changed.
- **Console discovery**: mDNS scan to auto-fill address and port.
- **Console address**: IP and port (auto-filled by discovery or manual entry).
- **Pre-shared key**: manual entry.
- **Haptic feedback**: toggle (default on).
- **Confirm before send**: toggle (default off).
- **Keep screen on**: toggle (default on).
- **Language**: speech recognition locale (default `en-US`).
- **Continuous listening**: toggle (default off). When enabled, Volume Down toggles continuous listening on/off instead of push-to-talk. Uses SpeechRecognizer's built-in silence detection as VAD.
- **Background volume keys**: opens Android Accessibility Settings to enable `VolumeKeyAccessibilityService`. Shows ENABLED/DISABLED status.

### Speech Recognition

- Android `SpeechRecognizer` API.
- `EXTRA_PARTIAL_RESULTS` enabled.
- `EXTRA_LANGUAGE` set to configured locale.
- Offline recognition preferred, cloud fallback.
- No timeout while volume down is held (PTT mode).

### Continuous Listening Mode

When the "Continuous Listening (VAD)" setting is enabled:

- **Volume Down** acts as a toggle: tap to start continuous listening, tap again to stop.
- SpeechRecognizer's silence detection acts as voice-activity detection (VAD):
  - `EXTRA_SPEECH_INPUT_COMPLETE_SILENCE_LENGTH_MILLIS`: 1500 ms
  - `EXTRA_SPEECH_INPUT_POSSIBLY_COMPLETE_SILENCE_LENGTH_MILLIS`: 1000 ms
  - `EXTRA_SPEECH_INPUT_MINIMUM_LENGTH_MILLIS`: 500 ms
- After each utterance is processed, recognition auto-restarts after a 300 ms delay.
- The listening panel shows "CONTINUOUS" instead of "LISTENING" to indicate the mode.
- `onRmsChanged` drives the `AudioLevelView` bar with real audio levels.
- No-speech timeouts and errors trigger automatic restart rather than stopping.

### Background Volume Key Capture (AccessibilityService)

When the activity is in the foreground, volume key events are handled by `MainActivity.onKeyDown` / `onKeyUp` as normal. When the activity is backgrounded or the screen is off, an Android `AccessibilityService` intercepts volume key events and forwards them through a static `VolumeKeyBridge` singleton so PTT and target cycling continue to work hands-free.

**Architecture:**

- `VolumeKeyAccessibilityService` extends `AccessibilityService` with `flagRequestFilterKeyEvents`.
- `VolumeKeyBridge` singleton holds a foreground flag and a key event callback registered by `MainActivity`.
- `MainActivity` sets `isActivityInForeground = true` in `onResume`, `false` in `onPause`.
- When the service receives a volume key event and the activity is NOT in the foreground, it invokes the bridge callback, which calls the activity's existing `onKeyDown` / `onKeyUp`.
- When the activity IS in the foreground, the service returns `false` to let normal dispatch handle it.
- Volume Up overlays (status overlay) are suppressed when backgrounded since a dialog cannot be shown without a foreground activity.

**Setup:** The user must enable the service in Android Settings > Accessibility. The settings screen provides a shortcut button and shows the current status (ENABLED / DISABLED).

### Code Vocabulary Accuracy

Programming terms ("JWT", "OAuth", "useState", etc.) often transcribe incorrectly with general speech models. Two mechanisms are used together:

**1. `EXTRA_BIASING_STRINGS`** -- passed in the `RecognizerIntent` to hint the recognizer toward known terms. Engine support varies; Google's recognizer honors it, third-party engines may not. Include the canonical forms of common terms (e.g. "JWT", "OAuth", "useState", "TypeScript").

**2. Post-processing correction table** -- applied to every transcript after recognition, before sending to the console. Engine-independent and fully testable. Maps phonetic variants to canonical forms:

| Raw transcript                        | Corrected      |
|---------------------------------------|----------------|
| jay double you tea / jwt              | JWT            |
| o auth / oh auth / oauth              | OAuth          |
| use state / usestate                  | useState       |
| use effect / useeffect                | useEffect      |
| type script / typescript              | TypeScript     |
| java script / javascript              | JavaScript     |
| git hub / github                      | GitHub         |
| react / react.js                      | React          |
| node.js / nodejs / node js            | Node.js        |
| postgres / postgress / post gres      | PostgreSQL     |

The correction pass runs after normalization (lowercase, trimmed) and before sending to the console. It uses whole-word replacement to avoid false positives.

Both mechanisms are additive: biasing reduces misrecognitions at the source; the correction table catches what biasing misses. Maintain both as new terms are encountered in use.

### Networking

- OkHttp WebSocket client.
- PSK in connection URL query parameter.
- Auto-reconnect with exponential backoff (1s, 2s, 4s, 8s, max 30s).
- Ping/pong keepalive every 15s.
- On connect/reconnect: request `list_agents` to sync state.
