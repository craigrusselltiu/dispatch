# Console Runtime

Reference for the dispatch console's role and responsibilities. The console is a thin runtime -- it manages the TUI, PTY processes, and tool execution on behalf of the orchestrator. It does not make decisions about what to dispatch, when to plan, or how to interpret voice commands. See ORCHESTRATOR.md for decision-making logic and SPEC.md for the full system specification.

## Role

The console has three responsibilities:

1. **Tool executor** -- the orchestrator issues tool calls (dispatch, terminate, merge, etc.) and the console executes them. The console returns results to the orchestrator but never initiates actions on its own.
2. **WebSocket relay** -- voice transcripts arrive from the radio over WebSocket. The console forwards them to the orchestrator for interpretation. It does not parse or classify commands.
3. **TUI renderer** -- the console renders the 2x2 agent grid, header, ticker, and overlays. It manages keyboard input routing (command mode vs. input mode) and pane navigation.

## PTY Management

Each agent slot owns one PTY. The console manages the full lifecycle:

**Dispatch** (triggered by orchestrator `dispatch` tool call):

```
1. Create git worktree: git worktree add .dispatch/.worktrees/{task_id} -b task/{task_id}
2. Open PTY via portable-pty
3. Spawn tool process (e.g. `claude`) in the worktree directory
4. Start reader thread: PTY output -> vt100::Parser
5. Write task prompt to PTY
```

**Input forwarding:**

In input mode, keystrokes are written directly to the PTY file descriptor. Voice prompts relayed from the orchestrator are written the same way.

**Resize:**

On terminal resize, PTY dimensions and vt100 parser are updated (debounced 100ms).

**Termination** (triggered by orchestrator `terminate` tool call):

Kill the child process, close the PTY, mark the slot empty. The worktree and branch are preserved.

## Completion Detection

The console monitors agent slots and reports status back to the orchestrator:

1. **Idle prompt detection** -- watch the vt100 virtual screen for tool-specific idle patterns (e.g. `^> $` for claude-code). Confirmed after 500ms of no new output.
2. **Inactivity timeout** -- if no idle pattern fires within a configurable timeout (default 60s), treat the agent as idle.

When an agent is detected as idle, the console notifies the orchestrator. The orchestrator decides what happens next (merge, dispatch new task, etc.).

## Worktree Lifecycle

Worktree operations are executed by the console on behalf of the orchestrator.

**Creation:** the console creates a worktree when the orchestrator calls the `dispatch` tool.

**Merge:** the console merges a task branch when the orchestrator calls the `merge` tool. It reports success or conflict back to the orchestrator.

**Cleanup:** on successful merge, the worktree is removed. On conflict, the worktree is preserved and the orchestrator is notified.

## WebSocket Server

The console runs a WebSocket server (default port 9800, PSK-authenticated) that accepts connections from the radio.

**Inbound flow:**

```
Radio -> WebSocket -> Console -> Orchestrator
```

Voice transcripts arrive as JSON messages. The console does not interpret them -- it forwards them to the orchestrator as-is. The orchestrator decides whether to dispatch, plan, address a specific agent, or take some other action.

**Outbound flow:**

```
Orchestrator -> Console -> WebSocket -> Radio
```

The console relays status updates (agent states, acknowledgments, errors) back to the radio.

## TUI

The console renders the interface and handles all keyboard interaction.

**Layout:**

```
+--------------------------------------------------------------+
| Header (radio status, PSK, task count, page, clock)          |
| Ticker (scrolling task events from orchestrator)              |
+-----------------------------+--------------------------------+
| Pane 1                      | Pane 2                         |
|                             |                                |
+-----------------------------+--------------------------------+
| Pane 3                      | Pane 4                         |
|                             |                                |
+-----------------------------+--------------------------------+
| Footer (mode, target, keybindings)                           |
+--------------------------------------------------------------+
```

**Pane rendering:** each pane reads from its slot's `vt100::Screen` and renders via ratatui, mapping terminal colors to ratatui styles.

**Ticker:** receives messages from the orchestrator and displays them as a scrolling marquee. The console does not generate ticker messages itself -- it renders what the orchestrator sends.

## Configuration

Relevant config keys in `config.toml`:

```toml
[tasks]
dir = ".dispatch"              # Base directory for dispatch artifacts in the target repo
completion_timeout_secs = 60   # Inactivity timeout for idle detection (0 to disable)
```
