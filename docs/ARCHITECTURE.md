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
│  Speech-to-  │                               │  │  - Tool calls    │    │
│  text        │                               │  │  - Voice interp  │    │
│              │                               │  │  - Dispatch      │    │
│              │                               │  └────────┬────────┘    │
│              │                               │           │             │
│              │                               │  ┌────────▼────────┐    │
│              │                               │  │  Agent Slots    │    │
│              │                               │  │  (up to 26)     │    │
│              │                               │  │                 │    │
│              │                               │  │  Each slot:     │    │
│              │                               │  │  - PTY process  │    │
│              │                               │  │  - vt100 parser │    │
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
- Dispatch agents with prompts via the `dispatch` tool.
- Terminate stuck or unneeded agents.
- Send follow-up messages to running agents.

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
│       └─ Working dir: .dispatch/.worktrees/{agent_id}/  │
│                                         │
│  vt100::Parser                          │
│    └─ Reads PTY output stream           │
│    └─ Maintains virtual terminal grid   │
│    └─ Completion detection              │
│                                         │
│  ratatui pane widget                    │
│    └─ Renders vt100::Screen to TUI      │
│    └─ Info strip (callsign, time)       │
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
│  Header bar (status, PSK, agents, page)  │  <- Region 1
│  ◄◄ Ticker (scrolling agent events)      │  <- Region 2
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

## Agent Dispatch Flow

```
Voice prompt (or direct keyboard input)
  │
  ▼
Orchestrator receives prompt
  │
  ├─▶ Orchestrator calls `dispatch` tool with agent callsign and prompt
  │
  ▼
Console assigns agent to slot
  ├─▶ Launch agent PTY
  ├─▶ Write prompt to agent's PTY
  │
  ▼
Agent works autonomously:
  ├─▶ Creates git worktree
  ├─▶ Does work, commits
  ├─▶ Merges to main
  ├─▶ Cleans up worktree
  ├─▶ Pushes
  │
  ▼
Done.
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
Raw transcript sent as { "type": "send", "text": "...", "auto": true }
  │
  ▼
WebSocket send to console orchestrator
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
- **Agent events** -- dispatches, completions, terminations.

The WebSocket server uses a `tokio::sync::broadcast` channel to push chat messages to all connected clients. Each connection handler uses `tokio::select!` to simultaneously process inbound requests and forward broadcast messages.

## Key Design Decisions

1. **Worktree-per-agent.** Each agent creates and manages its own git worktree. Agents are responsible for the full cycle: create worktree, work, commit, merge to main, clean up, push.

2. **LLM orchestrator, thin console.** The orchestrator is a persistent LLM that makes all decisions. The console is a thin runtime that executes tool calls, manages PTYs, and renders the TUI. The console holds no decision logic -- it reports events to the orchestrator.

3. **Ticker over status panel.** A single scrolling line costs minimal screen real estate while providing real-time visibility into agent events and errors.

4. **Completion detection.** Idle prompt pattern matching (primary) and inactivity timeout (safety net). Simpler than watching for file edits, and works with any tool.
