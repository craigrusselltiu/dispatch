# Orchestrator

The orchestrator is the central coordinator inside the dispatch console process. It never appears as a visible pane -- it runs on the main TUI thread (`main.rs`), receiving events from multiple sources and making all dispatch, planning, completion, and merge decisions.

## System Prompt

The orchestrator itself does not use an LLM. It is deterministic Rust code. However, it spawns a **headless planner agent** when complex prompts arrive. The planner's system prompt:

```
You are the Dispatch task planner. Decompose the following task into a structured plan.

Output ONLY a markdown task list in this exact format (no other text, no code fences):

# Short plan title

- [ ] t1: First task description
- [ ] t2: Second task that depends on t1 -> t1
  - [ ] t2.1: Subtask of t2
  - [ ] t2.2: Another subtask that depends on t2.1 -> t2.1
- [ ] t3: Third task that depends on t1 and t2 -> t1, t2

Rules:
- Use t1, t2, t3 for top-level tasks. Use t1.1, t1.2 for subtasks.
- Add -> id1, id2 when a task depends on other tasks being done first.
- No arrow means the task can start immediately (no blockers).
- Keep each task small: one agent should complete it in one session.
- If the request is simple enough for one agent, output just one task entry.
- Output ONLY the markdown. No explanation, no commentary.

Task to plan:
```

The planner runs as a background process (`tool_cmd -p "<prompt>"`), produces a markdown task list on stdout, and exits. The orchestrator writes the output to `.dispatch/tasks.md` and begins dispatching.

## Available Tools

The orchestrator operates through these internal functions:

| Function | Purpose |
|---|---|
| `dispatch_slot()` | Creates a PTY process and launches an agent tool in a slot |
| `dispatch_plan_tasks()` | Finds unblocked tasks and dispatches them to idle/empty slots |
| `spawn_planner()` | Spawns the headless planner in a background thread |
| `create_worktree()` | Creates a git worktree at `.dispatch/.worktrees/{task_id}` on branch `task/{task_id}` |
| `merge_worktree()` | Merges `task/{task_id}` into main with `--no-ff`; on conflict, aborts and preserves the worktree |
| `create_task_in_file()` | Appends a new task entry to `.dispatch/tasks.md` |
| `update_task_in_file()` | Updates a task's status marker and agent annotation in `.dispatch/tasks.md` |
| `fetch_ready_tasks()` | Parses `.dispatch/tasks.md` and returns tasks with status `[ ]` and no unresolved dependencies |
| `is_idle_prompt()` | Checks the vt100 virtual screen for tool-specific idle patterns (e.g. `>` or `> ` for claude-code) |
| `compute_screen_hash()` | Hashes all screen content to detect output changes without storing the full buffer |

## Receiving Voice Transcripts

Voice transcripts flow through this path:

```
Android radio
  -> SpeechRecognizer (on-device STT)
  -> Command parser (keyword matcher, not LLM)
  -> WebSocket message (JSON, PSK-authenticated)
  -> Console WebSocket server (ws_server.rs, tokio async thread)
  -> handle_message() routes by msg_type
  -> mpsc channel sends WsEvent to main TUI thread
  -> Orchestrator processes event in main loop
```

### Inbound message types

| `type` | Purpose | Key fields |
|---|---|---|
| `send` | Prompt to an agent (addressed or unaddressed) | `slot`, `text`, `auto` |
| `dispatch` | Launch a new agent | `slot`, `tool` |
| `terminate` | Kill an agent | `slot` |
| `set_target` | Change default recipient for unaddressed prompts | `slot` |
| `rename` | Change an agent's callsign | `slot`, `callsign` |
| `list_agents` | Query all slot states | -- |
| `radio_status` | Heartbeat with radio state | `state` |

### WsEvent channel

The WebSocket handler translates `send` messages with `auto: true` into one of three events for the main thread:

| Event | Trigger | Effect |
|---|---|---|
| `AutoDispatch { slot, prompt }` | Short prompt (<=15 words), idle or empty slot available | Spawn PTY if needed, create task, write prompt to agent |
| `QueueTask { prompt }` | All 26 slots busy | Create open task in `.dispatch/tasks.md`; dispatched when a slot frees |
| `PlanRequest { prompt }` | Long prompt (>15 words) | Spawn headless planner to decompose into subtasks |

## Decision-Making

### Dispatch

The orchestrator decides how to route each prompt:

```
Unaddressed prompt arrives (auto: true)
  |
  +-- >15 words? --> PlanRequest (spawn headless planner)
  |
  +-- <=15 words:
       +-- Idle agent available? --> Send to idle agent
       +-- Empty slot available? --> Launch new agent, send prompt
       +-- All slots busy? --> QueueTask (create open task, dispatch later)
```

For addressed prompts (`slot` field set), the text goes directly to that agent's PTY.

### Plan

When a `PlanRequest` arrives:

1. If no planner is running, `spawn_planner()` launches the configured tool (e.g. `claude -p`) in a background thread with the planner system prompt + user prompt.
2. If a planner is already running, the prompt is queued as a direct task instead.
3. The main loop polls `planner.receiver.try_recv()` each frame.
4. On planner completion:
   - Success: write plan to `.dispatch/tasks.md`, parse it, call `dispatch_plan_tasks()`.
   - Failure: fall back to direct single-task dispatch.

### Terminate

Triggered by a `terminate` WebSocket message or the `x` key in command mode:

1. Kill the PTY child process.
2. Clear the slot (but preserve the worktree and branch for reassignment).
3. Revert the task status back to `[ ]` in `.dispatch/tasks.md`.

### Merge

Triggered automatically when task completion is detected:

1. Run `git merge task/{task_id} --no-ff -m "merge task/{task_id}"` in the repo root.
2. On success: remove worktree (`git worktree remove --force`), delete branch (`git branch -d`), mark task `[x]`.
3. On conflict: abort merge (`git merge --abort`), add task ID to `conflict_tasks`, push ticker message. The worktree is preserved for manual resolution.
4. After any completion, call `dispatch_plan_tasks()` to dispatch newly unblocked tasks.

## State Awareness

### Agent slots

The orchestrator tracks up to 26 agent slots in `App.slots: [Option<SlotState>; 26]`. Each `SlotState` contains:

- `callsign` -- NATO phonetic name (or custom rename)
- `tool` -- agent tool name (e.g. "claude-code")
- `task_id` -- current task assignment
- `worktree_path` -- absolute path to the task's git worktree
- `writer` -- PTY file descriptor for sending input
- `screen` -- `Arc<Mutex<vt100::Parser>>` for reading terminal output
- `last_screen_hash` -- hash of last observed screen content
- `last_output_at` -- timestamp of last screen change
- `idle_since` -- timestamp when idle prompt was first detected (for debounce)
- `child_exited` -- atomic flag set by the PTY reader thread when the process exits

### Task tracking

Tasks are tracked in `.dispatch/tasks.md` with markdown checklist syntax:

```
- [ ] t1: Task description                    # open
- [~] t2: Another task | agent: Alpha         # in progress
- [x] t3: Done task                            # complete
- [ ] t4: Blocked task -> t1, t2               # blocked by dependencies
```

The orchestrator is the sole writer. A background thread polls the file every 5 seconds via `fetch_ready_tasks()` and sends results to the main loop over an mpsc channel.

### Other state

- `queued_tasks: Vec<QueuedTask>` -- tasks waiting for an available agent slot
- `planner: Option<PlannerState>` -- current headless planner execution (prompt + receiver)
- `conflict_tasks: Vec<String>` -- task IDs with unresolved merge conflicts
- `repo_root: String` -- absolute path to the target repository
- `tools: HashMap<String, String>` -- tool name to command mapping from config

## Communication with the Console Runtime

The orchestrator **is** the console runtime. It runs as the main TUI thread in a single-threaded event loop. It coordinates with other threads through channels:

### Inbound channels

| Source | Channel | Events |
|---|---|---|
| WebSocket server (tokio async thread) | `ws_event_rx` | `AutoDispatch`, `QueueTask`, `PlanRequest` |
| Task polling thread (5s interval) | `tasks_rx` | `Vec<QueuedTask>` of ready tasks from `.dispatch/tasks.md` |
| Planner thread (one-shot) | `planner.receiver` | `Option<String>` plan text on planner exit |
| PTY reader threads (per-slot) | `Arc<Mutex<vt100::Parser>>` | Screen content updated in shared mutex |
| PTY reader threads (per-slot) | `child_exited: AtomicBool` | Set when the agent process exits |
| Terminal | crossterm event poll | Keyboard input |

### Outbound actions

| Target | Mechanism | Actions |
|---|---|---|
| Agent PTYs | `slot.writer` (PTY fd) | Write task prompts, forward keyboard input |
| `.dispatch/tasks.md` | Direct filesystem I/O | Create/update/complete tasks |
| Git | `Command::new("git")` | Create worktrees, merge branches, clean up |
| Ticker | `app.push_ticker()` | Status messages rendered as scrolling marquee |
| WebSocket clients | `OutboundMsg` responses | Slot status, ack, error messages |

### Event loop

Each frame of the main loop:

1. Check for exited child processes -- merge worktree if task was assigned.
2. Scan slots for task completion (idle prompt with 500ms debounce, or inactivity timeout).
3. For completed tasks: mark `[x]`, dispatch newly unblocked plan tasks, assign next queued task to freed slot.
4. Poll task file changes from background thread.
5. Check headless planner completion.
6. Advance ticker animation.
7. Process `WsEvent`s from WebSocket thread.
8. Render TUI frame.
9. Poll keyboard input and handle mode-specific key bindings.

## Completion Detection

Two-layer strategy ensures agents are detected as done regardless of tool behavior:

**Layer 1: Idle prompt detection (primary).** Each frame, the orchestrator checks the last non-blank row of the vt100 virtual screen. For claude-code, the idle pattern is `>` or `> `. When first detected, `idle_since` is set. After 500ms of continuous idle, the task is marked complete.

**Layer 2: Inactivity timeout (safety net).** If `completion_timeout_secs` is non-zero (default 60) and no screen content has changed for that duration, the task is marked complete. This catches tools that don't have a recognizable idle prompt.

## Configuration

Relevant keys in `config.toml`:

```toml
[beads]
auto_track = true                    # Create tasks for voice prompts
auto_dispatch = true                 # Auto-dispatch unaddressed prompts
default_tool = "claude-code"         # Tool for auto-dispatch and planner
completion_timeout_secs = 60         # Inactivity timeout (0 to disable)

[tools]
claude-code = "claude"               # Shell command to launch each tool
copilot = "gh copilot suggest"
```
