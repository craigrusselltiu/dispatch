# Architecture

High-level architecture of the Dispatch system.

## System Overview

```
┌──────────────┐     WebSocket (LAN, PSK)     ┌──────────────────────────┐
│  Dispatch    │  <------------------------->  │  Dispatch Console        │
│  Radio       │                               │                          │
│  (Android)   │                               │  ┌─────────────────┐    │
│              │                               │  │  Orchestrator    │    │
│  Voice input │                               │  │  (persistent LLM)│    │
│  Command     │                               │  │  - Tool calls    │    │
│  parser      │                               │  │  - Voice interp  │    │
│              │                               │  │  - Dispatch/merge│    │
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

A persistent LLM agent that acts as the central decision-maker. It runs headlessly inside the console process (no visible pane) for the entire session. The console forwards voice transcripts and completion events to the orchestrator, and the orchestrator issues tool calls that the console executes.

**Responsibilities:**
- Interpret voice transcripts and decide what action to take.
- Dispatch agents into repositories to work on tasks.
- Decompose complex tasks into subtasks via the headless planner.
- Merge completed worktree branches back to main.
- Terminate stuck or unneeded agents.
- Send follow-up messages to running agents.
- React to completion and conflict events by dispatching next tasks.

See ORCHESTRATOR.md for the full specification including system prompt, tools, and decision-making logic.

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

The Android radio is a single-activity app. It handles voice input and WebSocket communication. Raw voice transcripts are sent to the console's orchestrator for interpretation -- no local command parsing.

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
WebSocket send to console (auto:true)
  │
  ▼
Console pushes chat messages back
  │
  ▼
Radio displays in scrollable chat log
```

### Chat Log

The radio displays a scrollable chat log showing orchestrator decisions, agent events, and voice transcripts. The console pushes `chat` messages over the WebSocket whenever significant events occur:

- **Voice transcripts** -- the user's spoken commands, echoed back.
- **Orchestrator reasoning** -- the dispatcher's decisions (e.g. "Dispatching Alpha.").
- **Agent events** -- task completions, dispatches, terminations.
- **Merge results** -- successful merges and conflicts.

The WebSocket server uses a `tokio::sync::broadcast` channel to push chat messages to all connected clients. Each connection handler uses `tokio::select!` to simultaneously process inbound requests and forward broadcast messages.

## Key Design Decisions

1. **Worktree-per-task, not worktree-per-agent.** Agents are ephemeral; tasks are the unit of work. If an agent is terminated mid-task, the worktree survives and can be reassigned.

2. **LLM orchestrator, thin console.** The orchestrator is a persistent LLM that makes all decisions. The console is a thin runtime that executes tool calls, manages PTYs, and renders the TUI. The console holds no decision logic -- it serializes `.dispatch/tasks.md` writes and reports events to the orchestrator.

3. **Headless planner.** The planner agent is invisible -- no pane, no slot consumed. It runs, writes the plan, exits. This keeps the quad panes reserved for actual work.

4. **Ticker over status panel.** A single scrolling line costs minimal screen real estate while providing real-time visibility into task events, merges, and errors.

5. **Two-layer completion detection.** Idle prompt pattern matching (primary) and inactivity timeout (safety net). Simpler than watching for file edits, and works with any tool.
