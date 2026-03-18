# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches tasks to AI coding agents. The PC-side TUI gives you a live quad-pane view of embedded agent terminals. Voice a big task and the console plans it, breaks it into subtasks, dispatches agents into isolated git worktrees, and merges results back -- all tracked in a simple markdown file (`.dispatch/tasks.md`).

## Overview

The system has two components:

1. **Dispatch Radio** (Android) -- a minimal push-to-talk app controlled via hardware volume buttons. Transcribes speech, parses voice commands, and sends structured messages over a local WebSocket connection.
2. **Dispatch Console** (PC) -- a TUI command center with four embedded terminal panes, each running a live AI agent session. Receives voice commands from the radio, plans and decomposes tasks, dispatches agents into git worktrees, tracks progress in `.dispatch/tasks.md`, and merges completed work. Supports direct keyboard input into any agent pane via a vim-style modal interface.

Both components live in a single monorepo.

```
┌──────────────┐     WebSocket (LAN, PSK)     ┌──────────────────┐
│  Dispatch    │  ◄────────────────────────►  │  Dispatch        │
│  Radio       │                               │  Console         │
│  (Android)   │                               │  (PC TUI)        │
│              │                               │                  │
│  Volume keys │                               │  4x embedded     │
│  Speech-to-  │                               │  terminals (PTY) │
│  text, voice │                               │  Git worktrees   │
│  commands    │                               │  .dispatch/      │
└──────────────┘                               └──────────────────┘
```

---

## Repository Structure

```
dispatch/
  radio/               # Android app (Kotlin, Gradle)
  console/             # PC TUI (Rust, Cargo)
  docs/
    SPEC.md            # Full system specification
    ARCHITECTURE.md    # High-level architecture overview
    CONSOLE.md         # Console task management reference
    AGENTS.md          # Template injected into agent prompts
  README.md
```

**In the target repo** (created by dispatch at runtime):

```
sample-repo/
  .dispatch/
    tasks.md           # Live task plan (read/written by the console)
    .worktrees/        # Git worktrees for active tasks
  (repo's own files)
```

The `.dispatch/` directory is gitignored by the console on first run.

---

## Agent Naming

Every agent is assigned a callsign from the NATO phonetic alphabet by default, in dispatch order:

| Slot | Callsign | Slot | Callsign  | Slot | Callsign  |
|------|----------|------|-----------|------|-----------|
| 1    | Alpha    | 10   | Juliet    | 19   | Sierra    |
| 2    | Bravo    | 11   | Kilo      | 20   | Tango     |
| 3    | Charlie  | 12   | Lima      | 21   | Uniform   |
| 4    | Delta    | 13   | Mike      | 22   | Victor    |
| 5    | Echo     | 14   | November  | 23   | Whiskey   |
| 6    | Foxtrot  | 15   | Oscar     | 24   | X-ray     |
| 7    | Golf     | 16   | Papa      | 25   | Yankee    |
| 8    | Hotel    | 17   | Quebec    | 26   | Zulu      |
| 9    | India    | 18   | Romeo     |      |           |

Maximum 26 concurrent agents. Callsigns are bound to slots, not agent instances. If Alpha is terminated and a new agent is dispatched into slot 1, it becomes Alpha again.

Agents can be renamed from the console via the `R` key. Custom names replace the NATO default until the agent is terminated, at which point the slot reverts. Custom names are validated against reserved command vocabulary to avoid parser conflicts.

Callsigns are the primary identifier for voice commands. All agents are addressable by voice regardless of which page is currently displayed in the console.

---

## Voice Commands

The radio parses the transcript locally before sending it to the console. The parser checks for command patterns and agent addressing. If no command or agent is detected, the entire transcript is sent as a prompt to the currently targeted agent.

### Natural Agent Addressing

Agents can be addressed directly by callsign at the start of an utterance, as if speaking to a person:

| Utterance                                      | Parsed as                                    |
|------------------------------------------------|----------------------------------------------|
| "Alpha, can you refactor the auth module"      | `send` to Alpha, text="can you refactor..."  |
| "Charlie, investigate the memory leak"         | `send` to Charlie, text="investigate..."     |
| "Bravo, write tests for the payment module"    | `send` to Bravo, text="write tests..."       |
| "Alpha refactor the auth module"               | `send` to Alpha, text="refactor..."          |

The parser detects a callsign at the start of the utterance, optionally followed by a comma, strips it, and routes the remaining text to that agent. The prompt is sent directly -- the target does not change. This is the primary method for addressing a specific agent.

If no callsign is detected at the start, the prompt goes to the currently targeted agent.

### Command Patterns

**Dispatch a new agent:**

| Utterance                                       | Parsed as                              |
|-------------------------------------------------|----------------------------------------|
| "dispatch claude code"                          | `dispatch` tool=claude-code            |
| "new copilot"                                   | `dispatch` tool=copilot                |
| "spin up claude code"                           | `dispatch` tool=claude-code            |

The console assigns the agent to the first empty slot.

**Terminate an agent:**

| Utterance                                       | Parsed as                              |
|-------------------------------------------------|----------------------------------------|
| "terminate alpha"                               | `terminate` agent=Alpha                |
| "kill bravo"                                    | `terminate` agent=Bravo                |
| "shut down charlie"                             | `terminate` agent=Charlie              |

**Switch target (changes default recipient for unaddressed prompts):**

| Utterance                                       | Parsed as                              |
|-------------------------------------------------|----------------------------------------|
| "switch to bravo"                               | `set_target` agent=Bravo               |
| "target charlie"                                | `set_target` agent=Charlie             |

**Unaddressed prompt (auto-dispatch):**

If the utterance doesn't start with a callsign and doesn't match a command, it goes to the current target. If there is no current target (no agents running), the console auto-dispatches a new agent and sends the prompt to it. See the Task Management section for the full auto-dispatch flow.

### Parser Design

Priority-ordered keyword matcher. Not an LLM. Runs synchronously on the final transcript.

```
1. Normalize: lowercase, trim whitespace
2. Check for command prefixes:
   a. /^(dispatch|new|spin up)\s+(claude code|copilot)/     -> dispatch
   b. /^(terminate|kill|shut down)\s+{agent_name}/           -> terminate
   c. /^(switch to|target)\s+{agent_name}/                   -> set_target
3. Check for agent addressing:
   d. /^{agent_name}[,]?\s+(.+)/                             -> send to specific agent
4. Default:
   e. No match                                                -> send to current target (or auto-dispatch)
```

`{agent_name}` matches all active callsigns (NATO and custom), case-insensitive.

Fuzzy alias table for tool names:

| Canonical      | Aliases                                    |
|----------------|--------------------------------------------|
| `claude-code`  | claude code, cloud code, claud code        |
| `copilot`      | copilot, co-pilot, co pilot, github copilot|

---

## Task Management

Tasks are tracked in `.dispatch/tasks.md` at the repo root. The console orchestrates all task lifecycle: planning, decomposition, dispatch, and completion. Each agent works in an isolated git worktree. No external tooling required.

### Task Format

```markdown
# Refactor auth system

- [ ] t1: Extract auth middleware into separate module
  - [ ] t1.1: Create auth module skeleton
  - [ ] t1.2: Move JWT validation logic -> t1.1
  - [ ] t1.3: Update imports across codebase -> t1.1, t1.2
- [ ] t2: Add OAuth2 support -> t1
- [ ] t3: Write integration tests -> t2
```

**Status markers:** `[ ]` open, `[~]` in progress (with agent annotation), `[x]` done.

**Dependencies:** `-> t1.1, t1.2` means "blocked by t1.1 and t1.2". No arrow means no blockers. Indentation is for readability; the `->` arrow is what encodes dependencies.

**Agent annotation:** when a task is assigned, the console appends `| agent: Callsign`:

```
- [~] t1.1: Create auth module skeleton | agent: Alpha
```

### Planning

When a voice prompt describes a complex task (e.g. "refactor the auth system"), the console spawns a headless planner agent to decompose it:

1. **Planner dispatch**: the console spawns a temporary agent (no pane, no slot consumed) with the prompt and instructions to write a plan to `.dispatch/tasks.md`.
2. **Plan output**: the planner writes the task breakdown with IDs, descriptions, and dependency arrows.
3. **Planner exits**: once `.dispatch/tasks.md` is written, the planner process terminates.
4. **Dispatch begins**: the console reads the plan and starts dispatching worker agents for unblocked tasks.

The ticker line (see [Ticker](#ticker)) shows planner progress in real-time.

For simple one-off prompts (e.g. "Alpha, fix this typo"), no planning step occurs -- the console creates a single task and dispatches directly.

### Git Worktrees

Each task runs in an isolated git worktree to prevent agents from stepping on each other.

**On task assignment:**

```
git worktree add .dispatch/.worktrees/{task_id} -b task/{task_id}
```

The agent's PTY is launched with its working directory set to the worktree path. The agent sees a normal git repo and works as usual.

**On task completion:**

1. The console merges the task branch back to the main branch.
2. If the merge succeeds, the worktree is cleaned up: `git worktree remove .dispatch/.worktrees/{task_id}`.
3. If the merge has conflicts, the console flags it on the ticker and leaves the worktree intact for manual review.

**On agent termination:**

If an agent is terminated before completing its task, the worktree and branch are preserved. The task is marked `[ ]` (open) so it can be picked up later -- the next agent assigned to it reuses the existing worktree.

The `.dispatch/` directory is gitignored.

### Task Lifecycle

**Complex task (planning flow):**

```
Voice: "refactor the auth system"
  -> Console spawns headless planner agent
  -> Ticker: "Planning: refactor the auth system..."
  -> Planner writes .dispatch/tasks.md with breakdown
  -> Console reads plan, finds unblocked tasks
  -> Dispatches workers into worktrees (one per task)
  -> On completion: merge, mark [x], check what's unblocked
  -> Dispatches next ready tasks
  -> Repeat until plan is done
```

**Simple prompt (direct flow):**

```
Voice: "Alpha, fix the login bug"
  -> Console creates single task in .dispatch/tasks.md
  -> Creates worktree, assigns to Alpha
  -> Alpha works in worktree
  -> On completion: merge, mark [x], clean up
```

**Prompt delivery:** the prompt text is sent to the agent's terminal, prefixed with a context line:

```
[task t1.2] Move JWT validation logic to the new auth module
```

### Auto-Dispatch

When a prompt arrives without a specified agent:

1. The console creates a task (or triggers planning if the prompt is complex -- currently prompts with more than 15 words are considered complex and routed to the headless planner).
2. It checks agent states:
   - If an idle agent exists, assign the task to it.
   - If all agents are busy and an empty slot exists, dispatch a new agent (default tool: `claude-code`) and assign the task.
   - If all slots are full and all agents are busy, add the task as `[ ]` (open/queued) and notify the radio: "All agents busy, task queued."
3. Queued tasks are picked up automatically when an agent becomes idle. The console scans `.dispatch/tasks.md` for `[ ]` tasks with no unresolved `->` dependencies.

### Task Completion Detection

Determining when an agent has finished a task is non-trivial. The console uses a three-layer strategy, evaluated in priority order:

**Layer 1 -- Idle prompt detection (primary)**

The console watches the virtual terminal screen (via the `vt100` parser) for idle prompt patterns that indicate the agent has returned to a ready state:

| Tool        | Idle pattern                |
|-------------|-----------------------------|
| claude-code | `^> $` on last active row  |
| gh copilot  | `What would you like help with?` or the prompt cursor `?` |
| Shell       | Prompt ending in `$ ` or `# ` |

The pattern match applies to the last non-blank row of the virtual screen. A match is confirmed only after no new output has arrived for 500ms, to avoid false positives during streaming output that briefly hits the idle-looking state.

**Layer 2 -- Inactivity timeout (safety net)**

If layer 1 does not fire within a configurable timeout after the last PTY output, the console marks the task complete. Default timeout: 60 seconds. Configurable in `config.toml`:

```toml
[tasks]
completion_timeout_secs = 60  # 0 to disable
```

When this fires, the agent is marked idle and can receive new tasks. The pane briefly shows a "timed out" indicator.

**State machine**

Each agent slot tracks a `completion_state`:

```
Idle -> Busy (task assigned + prompt delivered)
Busy -> Idle (layer 1 or 2 triggered -> merge worktree -> mark [x])
```

Only one completion event fires per task: whichever layer triggers first cancels the other.

### Ticker

A single-line LED-style scrolling marquee between the header bar and the quad panes. Text scrolls right-to-left continuously. Messages queue up -- when one finishes scrolling off, the next starts. When idle, the line is blank.

**Message sources:**

- Planner status: `Planning: breaking down "refactor auth" into 5 subtasks...`
- Task events: `t1.1 complete, merging... t1.2 unblocked, dispatching to Bravo`
- Merge results: `t1.1 merged to main` or `t1.3 merge conflict, needs manual review`
- Errors: `All agents busy, task t4 queued`

**Rendering:** fixed-width viewport, text offset decremented each frame tick (e.g. every 50ms). Once a message scrolls fully off-screen, it is discarded and the next queued message begins. If multiple messages queue up during a burst (e.g. several tasks completing at once), they scroll sequentially with a small gap between them.

### Task Visibility

The console displays task state across multiple areas:

- **Header bar**: total task progress (e.g. `Tasks: 3/7`) and queued count.
- **Ticker**: real-time event stream (planning, dispatch, merges, errors).
- **Pane info strip**: each pane shows its current task ID or "idle".
- **Task list overlay** (`t` key): full view of all tasks with status, agent assignments, and dependencies.

---

## Protocol

Communication happens over a single WebSocket connection. Messages are JSON. Either side can initiate messages.

### mDNS / Zeroconf Discovery

The console advertises itself on the local network via mDNS (DNS-SD) as a `_dispatch._tcp.local.` service. The service name is the console's hostname. The radio discovers this service using Android's `NsdManager` API, eliminating the need for manual IP entry.

- **Console**: uses the `mdns-sd` crate to register the service on startup. The service is advertised on all network interfaces with automatic address detection.
- **Radio**: the Settings screen has a "DISCOVER CONSOLE" button that scans for `_dispatch._tcp.` services for up to 5 seconds. When found, the host and port fields are auto-filled.

Manual IP/port entry remains available as a fallback.

### Authentication

The WebSocket handshake includes a pre-shared key as a query parameter:

```
ws://192.168.1.x:9800/?psk=<key>
```

The console generates a random PSK on first run and stores it in `~/.config/dispatch/config.toml`. The key is displayed on the console's header bar (truncated, expandable with `p`). Any connection attempt with an invalid or missing PSK is rejected with a 401 before the WebSocket upgrade completes.

**QR code pairing:** Press `Q` in command mode to display a QR code overlay encoding the full WebSocket URL (`ws://host:port/?psk=key`). The host is auto-detected from the machine's local network interface. The radio scans this QR code via its camera (Settings > Scan QR) to configure the connection without manual entry. The scanned URL populates host, port, and PSK fields automatically.

### Message Types

**List agents**

```
-> { "type": "list_agents" }
<- {
     "type": "agents",
     "slots": [
       { "slot": 1, "callsign": "Alpha", "tool": "claude-code", "status": "busy", "task": "t-1" },
       { "slot": 2, "callsign": "Bravo", "tool": "claude-code", "status": "idle", "task": null },
       { "slot": 3, "callsign": "Charlie", "tool": "copilot", "status": "idle", "task": null },
       { "slot": 4, "callsign": null, "tool": null, "status": "empty", "task": null }
     ],
     "target": 1,
     "queued_tasks": 0
   }
```

Slots are numbered 1-26. Only active (dispatched) and empty slots on allocated pages are included. `task` is the current task ID if the agent is working on one.

**Set target**

```
-> { "type": "set_target", "slot": 2 }
<- { "type": "target_changed", "slot": 2, "callsign": "Bravo" }
```

**Send prompt**

```
-> { "type": "send", "text": "refactor the auth module to use JWT" }
<- { "type": "ack", "slot": 1, "callsign": "Alpha", "task": "t-1" }
```

Sent to the current target. The console creates a task, assigns it, and returns the task ID in the ack.

**Send prompt to specific agent**

```
-> { "type": "send", "text": "write unit tests", "slot": 3 }
<- { "type": "ack", "slot": 3, "callsign": "Charlie", "task": "t-2" }
```

**Send prompt with auto-dispatch**

```
-> { "type": "send", "text": "set up the CI pipeline", "auto": true }
<- { "type": "ack", "slot": 2, "callsign": "Bravo", "task": "t-3", "auto_dispatched": false }
```

`auto: true` tells the console to pick the best agent. Response includes whether a new agent was auto-dispatched.

**Dispatch new agent**

```
-> { "type": "dispatch", "tool": "claude-code", "slot": 3 }
<- { "type": "dispatched", "slot": 3, "callsign": "Charlie", "tool": "claude-code" }
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

**Error**

```
<- { "type": "error", "message": "all slots full and busy, task queued as t-7" }
```

### Design Notes

- All messages are JSON in WebSocket text frames.
- Unknown message types are silently ignored for forward compatibility.
- Messages include an optional `seq` field for request-response correlation.
- The radio re-requests `list_agents` on reconnect to sync state.

---

## Dispatch Console (PC TUI)

### Target

- Rust
- Dependencies: `ratatui`, `crossterm`, `tokio`, `tokio-tungstenite`, `serde`, `serde_json`, `toml`, `portable-pty`, `vt100`, `dirs`, `notify` (file watcher), `mdns-sd` (mDNS advertisement), `hostname`
- Single binary, cross-platform (Windows, macOS, Linux)

### Embedded Terminals

Each agent pane is a real terminal emulator, not a text capture. The console manages PTYs directly -- there is no tmux dependency.

**Platform support:**

- **Linux/macOS**: uses native Unix PTYs (via `openpty`).
- **Windows**: uses ConPTY (Console Pseudo Terminal), available on Windows 10 1809+. ConPTY is what Windows Terminal uses internally. The `portable-pty` crate (by the WezTerm author) abstracts over both backends behind a single API, so application code is platform-agnostic.

The rest of the stack is also cross-platform: `crossterm` for terminal input/output, `ratatui` for rendering, `vt100` for escape sequence parsing (it operates on byte streams, so it's OS-independent). Claude Code and `gh copilot` both support Windows natively.

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

Pages are cycled with `[` / `]` or `Shift+Left` / `Shift+Right`. The header shows the current page and total pages.

```
┌─ DISPATCH ──────────────────────────────────────────────────────────┐
│ RADIO: ● CONNECTED   PSK: a7f3...  Tasks: 3/7  PAGE 1/2    14:32 │
│ ◄◄ t1.1 complete, merging... t1.2 unblocked, dispatching to Bravo │
├────────────────────────────────┬────────────────────────────────────┤
│ ▸ [1] ALPHA                    │ [2] BRAVO                         │
│   CLAUDE-CODE | t1.1           │ CLAUDE-CODE | t1.2                │
│   dispatched 14:20 | 12m03s   │ dispatched 14:28 | 4m11s          │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄   │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄  │
│ ~/project$ claude              │ ~/project$ claude                  │
│                                │                                    │
│ > I'll start by updating the   │ > I'll create a comprehensive     │
│   auth middleware. First, let  │   test suite covering the core    │
│   me examine the current...    │   payment flows...                │
│                                │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┼────────────────────────────────────┤
│ [3] CHARLIE                    │ [4] DELTA                         │
│ COPILOT | idle                 │ CLAUDE-CODE | t1.3                │
│ dispatched 14:15 | 17m12s     │ dispatched 14:30 | 2m04s          │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄   │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄  │
│ ~/project$ gh copilot suggest  │ ~/project$ claude                  │
│                                │                                    │
│ ? What would you like help     │ > Setting up the CI pipeline...   │
│   with?                        │                                    │
│ █                              │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┴────────────────────────────────────┤
│ ▸ RADIO IDLE │ TARGET: ALPHA │ i input │ [] page │ n new │ ?      │
└─────────────────────────────────────────────────────────────────────┘
```

Page 2 of the same session:

```
┌─ DISPATCH ──────────────────────────────────────────────────────────┐
│ RADIO: ● CONNECTED   PSK: a7f3...  Tasks: 3/7  PAGE 2/2    14:32 │
│ ◄◄ t2 merged to main                                              │
├────────────────────────────────┬────────────────────────────────────┤
│ [5] ECHO                       │ [6] FOXTROT                       │
│ CLAUDE-CODE | t2.1             │ CLAUDE-CODE | t2.2                │
│ dispatched 14:31 | 1m22s      │ dispatched 14:32 | 0m15s          │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄   │ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄  │
│ ~/project$ claude              │ ~/project$ claude                  │
│                                │                                    │
│ > Analyzing the database       │ > Starting linter configuration   │
│   schema for migration...      │   ...                             │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┼────────────────────────────────────┤
│ [7] ── STANDBY ──              │ [8] ── STANDBY ──                 │
│                                │                                    │
│  Dispatch new agent:           │  Queued tasks: 2                  │
│                                │                                    │
│  [c] claude-code               │  t3  "Write integration tests"    │
│  [g] gh copilot                │  t4  "Fix CORS headers"           │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
│                                │                                    │
├────────────────────────────────┴────────────────────────────────────┤
│ ▸ RADIO IDLE │ TARGET: ALPHA │ i input │ [] page │ n new │ ?      │
└─────────────────────────────────────────────────────────────────────┘
```

**Auto-navigate:** when you address an agent by voice or select a slot number that's on a different page, the console automatically switches to that page. Targeting Alpha while viewing page 2 flips back to page 1.

**Input mode** changes the footer and the targeted pane's border:

```
┌─ DISPATCH ──────────────────────────────────────────────────────────┐
│ RADIO: ● CONNECTED   PSK: a7f3...  Tasks: 3/7  PAGE 1/2    14:32 │
│ ◄◄ t1.3 merged to main                                            │
├────────────────────────────────┬────────────────────────────────────┤
│ ┃ [1] ALPHA                    │ [2] BRAVO                         │
│ ┃ CLAUDE-CODE | t1.1           │ ...                                │
│ ┃ dispatched 14:20 | 12m03s   │                                    │
│ ┃┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄   │                                    │
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

1. **Header bar** -- radio connection state, PSK (truncated), task progress (done/total), current page indicator, clock.
2. **Ticker** -- single-line LED-style scrolling marquee. Shows planner status, task events, merge results, and errors. Text scrolls right-to-left. Blank when idle. See [Ticker](#ticker).
3. **Quad pane** -- four slots from the current page. Targeted pane has `▸` marker and cyan border (command mode) or green border (input mode). Each pane has:
   - **Info strip**: callsign, tool type, current task ID (or "idle"), dispatch time, and runtime.
   - **Terminal area**: live embedded terminal output rendered from the VTE parser.
   - Empty slots show "STANDBY" with dispatch shortcuts. The last STANDBY slot on the last page also shows queued task count and titles.
4. **Footer bar** -- command mode: radio state, target (regardless of page), page navigation, shortcuts. Input mode: `-- INPUT ({CALLSIGN}) --` with ESC hint.

### Input Model

Modal, vim-style. Two modes:

**Command mode** (default) -- keystrokes control the console.

**Input mode** -- keystrokes are written directly to the targeted agent's PTY. The terminal in the pane is fully interactive: you can type prompts, use arrow keys, tab completion, Ctrl+C to cancel, scroll through output -- everything. Because writes go straight to the PTY file descriptor, there is zero latency overhead.

| Transition       | Key         | Behavior                                           |
|------------------|-------------|----------------------------------------------------|
| Command -> Input | `Enter`     | Enter input mode on the currently targeted pane    |
| Command -> Input | `i`         | Same as `Enter`                                    |
| Input -> Command | `Escape`    | Return to command mode                             |

While in input mode, `Escape` is the only key intercepted by the console. Everything else goes to the PTY. If the underlying tool uses `Escape`, press it twice.

**Radio commands during input mode:** voice commands from the radio are always processed regardless of console mode. The two input channels (keyboard and radio) operate independently.

#### Command Mode Keys

| Key               | Action                                                       |
|-------------------|--------------------------------------------------------------|
| `Enter` / `i`     | Enter input mode on targeted pane                            |
| `1-4`             | Select target slot on current page (slot = page offset + key)|
| `Tab`             | Cycle target forward across all pages (skips empty slots, auto-navigates) |
| `Shift+Tab`       | Cycle target backward across all pages                       |
| `]` / `Shift+Right` | Next page                                                 |
| `[` / `Shift+Left`  | Previous page                                             |
| `n`               | Dispatch new agent (prompts for tool, fills first empty slot across all pages) |
| `N`               | Dispatch new agent into a specific slot (prompts for slot number) |
| `x`               | Terminate agent in currently targeted slot (confirms first)  |
| `R`               | Rename agent in currently targeted slot                      |
| `k` / `Up` / `PgUp` | Scroll up half a page in targeted pane's scrollback       |
| `j` / `Down` / `PgDn` | Scroll down half a page toward live output              |
| `G`               | Snap to live output (exit scrollback)                        |
| `t`               | Show task list overlay (plan, active, queued, completed)             |
| `P`               | Prune completed tasks from `.dispatch/tasks.md`              |
| `p`               | Show/hide full PSK                                           |
| `Q`               | Show QR code overlay for radio pairing                       |
| `q`               | Quit (confirms if agents are running)                        |
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

**Scrollback:** the `vt100::Parser` is initialized with a scrollback buffer (default 1000 lines, configurable via `terminal.scrollback_lines`). In command mode, `k`/`Up`/`PgUp` and `j`/`Down`/`PgDn` scroll the targeted pane by half-page increments; `G` snaps to live output. A `SCROLL` indicator appears when not at the bottom. Scrollback resets to the bottom on new output or when entering input mode.

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

**Prompt injection (from voice or auto-dispatch):**

When a prompt arrives from the radio (or from task auto-dispatch), it is written to the PTY as if typed, followed by `\r` (Enter). This happens regardless of the console's current input mode.

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

The child process is killed, the PTY is closed, and the slot is marked empty. Any active task for that agent is updated to `[ ]` (open/unassigned) in `.dispatch/tasks.md` so it can be picked up later. The worktree and branch are preserved for the next agent.

### Configuration

Stored in a platform-appropriate config directory, auto-generated on first run:

- **Linux**: `~/.config/dispatch/config.toml`
- **macOS**: `~/Library/Application Support/dispatch/config.toml`
- **Windows**: `%APPDATA%\dispatch\config.toml`

The `dirs` crate handles path resolution.

```toml
[server]
port = 9800
bind = "0.0.0.0"

[auth]
# Auto-generated. Run `dispatch regenerate-psk` to rotate.
psk = "a7f3e9b1c4d8..."

[terminal]
scrollback_lines = 1000
# Maximum concurrent agents. 4-26, in multiples of 4 (one page per 4 agents).
max_agents = 8

[tasks]
# Base directory for dispatch artifacts in the target repo (tasks.md, .worktrees/).
dir = ".dispatch"
# Auto-dispatch agents for unaddressed prompts.
auto_dispatch = true
# Default tool for auto-dispatched and planner agents.
default_tool = "claude-code"
# Inactivity timeout for task completion detection (seconds). 0 to disable.
completion_timeout_secs = 60
# Auto-merge completed task branches to main. If false, branches are left for manual review.
auto_merge = true

[tools]
claude-code = "claude"
copilot = "gh copilot suggest"
```

### CLI

```
dispatch                    # Start the console (default)
dispatch regenerate-psk     # Generate a new PSK
dispatch show-psk           # Print the current PSK to stdout
dispatch config             # Print config file path
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
| Key up    | Stop recognizer, parse transcript, send appropriate message, confirm vibration, send `radio_status: idle` |

The command parser runs between recognition and send. If a voice command or agent address is detected, the radio shows what it parsed (e.g. "-> ALPHA: refactor the auth module" or "DISPATCH: claude-code") before sending.

If the transcript is empty, double-pulse vibration, no message sent.

**Volume Up -- Cycle Target**

| Event     | Action                                                    |
|-----------|-----------------------------------------------------------|
| Key down  | Advance to next occupied slot across all agents (skip empty), send `set_target`, display new callsign, short vibration |

**Volume Up Long Press -- Quick Dispatch**

| Event     | Action                                                    |
|-----------|-----------------------------------------------------------|
| Hold >1s  | Show agent type picker on screen. Tap to dispatch.        |

### UI Layout

Minimal, high-contrast, dark theme. Uppercase labels, monospaced accents.

```
┌─────────────────────────────┐
│  DISPATCH RADIO             │
│  ● CONNECTED                │
│                             │
│  TARGET                     │
│  [1] ALPHA                  │  <- callsign, large
│  CLAUDE-CODE | t-1          │  <- tool + active task
│                             │
│  ┌───────────────────────┐  │
│  │   ◉ LISTENING          │  │
│  │   ░░░░░███████░░░░░░  │  │
│  └───────────────────────┘  │
│                             │
│  LAST DISPATCH              │
│  -> ALPHA                   │
│  "refactor the auth module  │
│   to use JWT"               │
│  task t-1                   │  <- task ID
│                             │
│  AGENTS                     │
│  ▸α  β  χ  δ  ε  φ        │  <- scrollable, initials for all active agents
│                             │
│  QUEUED: 2                  │
│                             │
└─────────────────────────────┘
```

### Settings

- **Console discovery**: mDNS scan to auto-fill address and port.
- **Console address**: IP and port (auto-filled by discovery or manual entry).
- **Pre-shared key**: manual entry or QR scan.
- **Haptic feedback**: toggle (default on).
- **Confirm before send**: toggle (default off).
- **Keep screen on**: toggle (default on).
- **Language**: speech recognition locale (default `en-AU`).
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
- Volume Up long press (Quick Dispatch overlay) is suppressed when backgrounded since a dialog cannot be shown without a foreground activity. Short press (target cycling) works in both states.

**Setup:** The user must enable the service in Android Settings > Accessibility. The settings screen provides a shortcut button and shows the current status (ENABLED / DISABLED).

### Code Vocabulary Accuracy

Programming terms ("JWT", "OAuth", "useState", etc.) often transcribe incorrectly with general speech models. Two mechanisms are used together:

**1. `EXTRA_BIASING_STRINGS`** -- passed in the `RecognizerIntent` to hint the recognizer toward known terms. Engine support varies; Google's recognizer honors it, third-party engines may not. Include the canonical forms of common terms (e.g. "JWT", "OAuth", "useState", "TypeScript").

**2. Post-processing correction table** -- applied to every transcript after recognition, before parsing. Engine-independent and fully testable. Maps phonetic variants to canonical forms:

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

The correction pass runs after normalization (lowercase, trimmed) and before command parsing. It uses whole-word replacement to avoid false positives.

Both mechanisms are additive: biasing reduces misrecognitions at the source; the correction table catches what biasing misses. Maintain both as new terms are encountered in use.

### Voice Command Parser

Kotlin sealed class:

```kotlin
sealed class Command {
    data class Dispatch(val tool: String) : Command()
    data class Terminate(val slot: Int) : Command()
    data class SetTarget(val slot: Int) : Command()
    data class SendTo(val slot: Int, val text: String) : Command()
    data class SendToTarget(val text: String) : Command()
}

fun parse(transcript: String, agents: List<Agent>): Command
```

The parser needs the current agent list (synced from console) for callsign matching and the fuzzy alias table for tool names.

### Networking

- OkHttp WebSocket client.
- PSK in connection URL query parameter.
- Auto-reconnect with exponential backoff (1s, 2s, 4s, 8s, max 30s).
- Ping/pong keepalive every 15s.
- On connect/reconnect: request `list_agents` to sync state.

---

## Phases

### Phase 0 -- Proof of Concept

**Goal:** Validate embedded PTY + VTE rendering and git worktree workflow.

- Minimal Rust program that spawns a single PTY running `claude`, pipes output through `vt100`, and renders it in a `ratatui` widget.
- Accept keyboard input and forward to the PTY.
- Create a git worktree, launch the agent inside it, and merge the branch back on completion.
- No WebSocket, no Android, no multi-pane. Just one terminal and one worktree.

**Done when:** you can interact with Claude Code through a ratatui pane, and changes made by the agent in a worktree are merged back to main on completion.

### Phase 1 -- Core

**Goal:** Fully functional voice-to-agent pipeline with embedded terminals, worktree isolation, task planning, and the Android radio.

Console:
- TUI with quad-pane layout via `ratatui`.
- Embedded terminals via `portable-pty` + `vt100`.
- Command mode and input mode with direct PTY writes.
- WebSocket server with PSK authentication.
- Full protocol support.
- Agent lifecycle: dispatch, terminate, rename.
- Task tracking: `.dispatch/tasks.md` with planning, dependencies, and worktree-per-task.
- Ticker line: LED-style scrolling marquee for task events and planner status.
- Headless planner agent for task decomposition.
- Git worktree creation, agent dispatch into worktree, merge on completion.
- Auto-dispatch for unaddressed prompts.
- Pane info strip: callsign, tool, task ID, dispatch time, runtime.
- Config file with auto-generation and CLI subcommands.
- Terminal scrollback in panes (PgUp/PgDn in command mode, configurable buffer size).

Radio:
- Single Activity with volume button overrides.
- Push-to-talk via `SpeechRecognizer` on volume down.
- Voice command parser with natural agent addressing ("Alpha, can you...").
- Command parsing (dispatch, terminate, switch target).
- Target cycling on volume up, quick dispatch on long press.
- WebSocket connection with PSK and auto-reconnect.
- Agent list sync, task ID display.
- Settings screen.
- Haptic feedback with distinct patterns per command type.

**Done when:** you can say "refactor the auth system", watch the ticker show planning progress, then see agents dispatched into worktrees for each subtask -- with completed work auto-merged back to main.

### Phase 2 -- Polish

- mDNS/Zeroconf console discovery.
- QR code pairing in console TUI.
- ~~Continuous listening mode with voice-activity detection.~~ (done)
- ~~Terminal scrollback in panes.~~ (done)
- ~~Agent busy/idle detection: refine idle prompt patterns and completion timeout per tool as edge cases surface in testing.~~ (done)
- TLS on the WebSocket.
- ~~AccessibilityService for screen-off volume button capture.~~ (done)
- Console prompt history and logging.
- ~~`.dispatch/tasks.md` pruning for long-running projects (archive completed tasks).~~ (done)
- Wear OS companion: minimal wrist app (`radio/wear/` module) with status glance (connection state, current target, active agents), crown rotation for target cycling, and tap-to-dispatch trigger. Standalone APK, same WebSocket protocol as the phone radio. Settings (host, port, PSK) via long-press.

---

## Open Questions

1. ~~**`vt100` crate limitations**~~ **Resolved**: start with `vt100` for its simpler API and smaller footprint. If gaps appear in practice (advanced alternate screen, mouse events, 256/truecolor), migrate to `alacritty_terminal`, which is more complete but significantly heavier.

2. ~~**Speech recognition and code vocabulary**~~ **Resolved**: use both `EXTRA_BIASING_STRINGS` (engine-level hint, supported by Google's recognizer) and a post-processing correction table (engine-independent fallback). See [Code Vocabulary Accuracy](#code-vocabulary-accuracy).

3. **Copilot CLI interactive TUI**: `gh copilot suggest` has a multi-step interactive interface. PTY embedding helps here (it's a real terminal), but the auto-prompt-injection flow may conflict with Copilot's input expectations.

4. ~~**Task completion detection**~~ **Resolved**: use a two-layer strategy: (1) idle prompt pattern match on the `vt100` virtual screen with 500ms debounce (primary), (2) configurable inactivity timeout (safety net). On completion, the console merges the worktree branch and marks the task `[x]`. See [Task Completion Detection](#task-completion-detection).

5. **Voice command ambiguity**: "alpha" at the start of an utterance is treated as agent addressing. If the user wants to say a prompt that happens to start with "alpha" (e.g. "alpha testing needs to be improved"), it would be misrouted. Mitigation: the comma after the callsign is a strong signal ("Alpha, ..." vs "alpha testing..."), and the confirm-before-send setting provides a safety net.

6. **Custom callsign conflicts**: validate custom names against reserved command vocabulary ("dispatch", "kill", "terminate", etc.) and against active tool names.

7. ~~**`bd` CLI availability**~~ **Resolved**: the console uses `.dispatch/tasks.md` and git worktrees directly. No external task tracking tool required.

8. ~~**PTY size synchronization**~~ **Resolved (dispatch-dvo)**: debounce resize events with a 100ms delay. See [Terminal resize](#pty-management) for the implementation pattern.

9. **Concurrent `.dispatch/tasks.md` writes**: if multiple agents finish tasks simultaneously, the console may issue concurrent file writes. Use a single-writer task on the console side to serialize all `.dispatch/tasks.md` mutations.

10. **Worktree merge conflicts**: when multiple tasks touch overlapping files, merges may conflict. The console flags conflicts on the ticker and preserves the worktree for manual resolution. Consider sequential merge ordering based on the dependency graph to minimize conflicts.

11. **Planner quality**: the headless planner agent must produce well-structured `.dispatch/tasks.md` output with valid IDs and dependency arrows. Provide a system prompt template with the expected format. If the planner output is malformed, the console falls back to treating the original prompt as a single task.

10. **Windows ConPTY quirks**: ~~Open -- see decision below.~~

**Decision (dispatch-env):** ConPTY behavior was researched against Claude Code's TUI on Windows. Decisions per area:

- **Cursor visibility** (`\x1b[?25l` / `\x1b[?25h`): ConPTY handles these correctly via `ENABLE_VIRTUAL_TERMINAL_PROCESSING`. No explicit handling needed -- `portable-pty` is sufficient.

- **Alternate screen buffer** (`\x1b[?1049h` / `\x1b[?1049l`): ConPTY re-encodes output from the win32 screen buffer and can synthesize full-screen repaints. This is the highest-risk area. Decision: test with Claude Code's TUI during Phase 0. If alternate screen transitions produce corrupt output, the fallback is to pipe through `vt100` with tolerance for extra synthetic sequences (it ignores unknown escapes). No pre-emptive workaround -- identify the failure mode first.

- **Backspace**: ConPTY sends `\x7f` (DEL) for backspace. `portable-pty` does not normalize this. The input forwarding table in the PTY Management section already maps `Backspace -> \x7f`, which is correct for ConPTY. No change needed.

- **Ctrl+C**: Windows uses console control events, not Unix signals. `portable-pty` abstracts the delivery mechanism but semantics differ (a new thread is created on the Windows side). The ANSI sequence `\x03` written to the PTY still triggers an interrupt for console applications. Decision: write `\x03` as specified. Verify during Phase 0 that Claude Code actually cancels its current operation on Windows when Ctrl+C is forwarded this way.

Summary: the backspace mapping is already correct. Alternate screen and Ctrl+C both require Phase 0 validation on Windows before any mitigations are coded.
