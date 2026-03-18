# Architecture

High-level architecture of the Dispatch system.

## System Overview

```
┌──────────────┐     WebSocket (LAN, PSK)     ┌──────────────────────────┐
│  Dispatch    │  <------------------------->  │  Dispatch Console        │
│  Radio       │                               │                          │
│  (Android)   │                               │  ┌─────────────────┐    │
│              │                               │  │  Orchestrator    │    │
│  Voice input │                               │  │  - Task planner  │    │
│  Command     │                               │  │  - .dispatch/tasks.md r/w  │    │
│  parser      │                               │  │  - Worktree mgmt │    │
│              │                               │  │  - Merge logic   │    │
│              │                               │  └────────┬────────┘    │
│              │                               │           │             │
│              │                               │  ┌────────▼────────┐    │
│              │                               │  │  Agent Slots    │    │
│              │                               │  │  (up to 26)     │    │
│              │                               │  │                 │    │
│              │                               │  │  Each slot:     │    │
│              │                               │  │  - PTY process  │    │
│              │                               │  │  - vt100 parser │    │
│              │                               │  │  - Git worktree │    │
│              │                               │  │  - ratatui pane │    │
│              │                               │  └─────────────────┘    │
│              │                               │                          │
└──────────────┘                               └──────────────────────────┘
```

## Console Components

### Orchestrator

The central coordinator. It never appears as a visible pane -- it runs inside the console process itself.

**Responsibilities:**
- Receive voice commands and keyboard input.
- Decide whether a prompt needs planning (complex task) or direct dispatch (simple prompt).
- Spawn headless planner agents for task decomposition.
- Read/write `.dispatch/tasks.md` to track the task plan and progress.
- Create git worktrees for new tasks.
- Assign tasks to idle agent slots.
- Detect task completion (idle prompt patterns, inactivity timeout).
- Merge completed worktree branches back to main.
- Dispatch the next unblocked task when a dependency clears.
- Push messages to the ticker.

### Agent Slots

Each slot holds one running agent. Up to 26 slots, displayed 4 at a time in a 2x2 grid.

**Per-slot architecture:**

```
┌─────────────────────────────────────────┐
│  Agent Slot                             │
│                                         │
│  PTY (portable-pty)                     │
│    └─ Child process (e.g. `claude`)     │
│       └─ Working dir: .dispatch/.worktrees/{id}/  │
│                                         │
│  vt100::Parser                          │
│    └─ Reads PTY output stream           │
│    └─ Maintains virtual terminal grid   │
│    └─ Idle prompt detection             │
│                                         │
│  ratatui pane widget                    │
│    └─ Renders vt100::Screen to TUI      │
│    └─ Info strip (callsign, task, time) │
│                                         │
│  Input (keyboard in input mode)         │
│    └─ Writes directly to PTY fd         │
└─────────────────────────────────────────┘
```

### Ticker

Single-line scrolling marquee between the header and the quad panes. Receives messages from the orchestrator and renders them as right-to-left scrolling text. Messages queue and display sequentially.

### TUI Layout

```
┌──────────────────────────────────────────┐
│  Header bar (status, PSK, tasks, page)   │  <- Region 1
│  ◄◄ Ticker (scrolling task events)       │  <- Region 2
├───────────────────┬──────────────────────┤
│  Pane 1           │  Pane 2              │  <- Region 3
│                   │                      │     (quad panes)
├───────────────────┼──────────────────────┤
│  Pane 3           │  Pane 4              │
│                   │                      │
├───────────────────┴──────────────────────┤
│  Footer bar (mode, target, shortcuts)    │  <- Region 4
└──────────────────────────────────────────┘
```

## Task Flow

### Complex Task (with planning)

```
Voice prompt
  │
  ▼
Orchestrator receives "refactor the auth system"
  │
  ├─▶ Spawn headless planner agent (no pane, no slot)
  │     └─ Planner writes .dispatch/tasks.md with subtask breakdown
  │
  ├─▶ Ticker: "Planning: refactor the auth system..."
  │
  ▼
Orchestrator reads .dispatch/tasks.md
  │
  ├─▶ Find unblocked tasks (no -> dependencies)
  │
  ▼
For each unblocked task:
  │
  ├─▶ git worktree add .dispatch/.worktrees/{id} -b task/{id}
  ├─▶ Assign to idle agent slot
  ├─▶ Launch agent PTY in worktree directory
  ├─▶ Write task prompt to PTY
  ├─▶ Ticker: "t1.1 dispatched to Alpha"
  │
  ▼
Agent works in worktree...
  │
  ▼
Completion detected (idle prompt or timeout)
  │
  ├─▶ git merge task/{id} into main
  │     ├─ Success: clean up worktree, mark [x]
  │     └─ Conflict: flag on ticker, preserve worktree
  │
  ├─▶ Check newly unblocked tasks
  ├─▶ Dispatch next ready tasks
  │
  ▼
Repeat until plan complete
```

### Simple Prompt (direct dispatch)

```
Voice prompt
  │
  ▼
Orchestrator receives "Alpha, fix the login bug"
  │
  ├─▶ Create single task in .dispatch/tasks.md
  ├─▶ git worktree add .dispatch/.worktrees/{id} -b task/{id}
  ├─▶ Assign to Alpha
  ├─▶ Write prompt to Alpha's PTY
  │
  ▼
Alpha works, completes, merge, done.
```

## Radio Architecture

The Android radio is a single-activity app. It handles voice input, command parsing, and WebSocket communication.

```
Volume Down (hold)
  │
  ▼
SpeechRecognizer
  │
  ├─▶ Partial results displayed on screen
  │
  ▼
Volume Down (release)
  │
  ▼
Post-processing correction table
  │
  ▼
Command parser (keyword matcher, not LLM)
  │
  ├─▶ Dispatch command   -> { "type": "dispatch", ... }
  ├─▶ Terminate command  -> { "type": "terminate", ... }
  ├─▶ Set target command -> { "type": "set_target", ... }
  ├─▶ Agent-addressed    -> { "type": "send", "slot": N, ... }
  └─▶ Unaddressed prompt -> { "type": "send", ... }
  │
  ▼
WebSocket send to console
```

## Key Design Decisions

1. **Worktree-per-task, not worktree-per-agent.** Agents are ephemeral; tasks are the unit of work. If an agent is terminated mid-task, the worktree survives and can be reassigned.

2. **Console is the single coordinator.** Agents do not read `.dispatch/tasks.md` or manage their own lifecycle. The console holds all state, serializes all `.dispatch/tasks.md` writes, and orchestrates merges. No race conditions.

3. **Headless planner.** The planner agent is invisible -- no pane, no slot consumed. It runs, writes the plan, exits. This keeps the quad panes reserved for actual work.

4. **Ticker over status panel.** A single scrolling line costs minimal screen real estate while providing real-time visibility into task events, merges, and errors.

5. **Two-layer completion detection.** Idle prompt pattern matching (primary) and inactivity timeout (safety net). Simpler than watching for file edits, and works with any tool.
