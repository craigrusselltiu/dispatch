# Architecture

Dispatch is a two-component system: a Rust TUI (Console) that manages AI coding agents, and an Android app (Radio) that provides voice input over the local network.

## System Overview

```
┌──────────────────┐    TLS WebSocket (LAN, PSK)    ┌────────────────────────────┐
│  Dispatch Radio  │  <---------------------------> │  Dispatch Console          │
│  (Android)       │                                │                            │
│                  │                                │  Orchestrator (claude)     │
│  Volume keys     │   voice transcripts ───────>   │    ├─ receives transcripts │
│  SpeechRecognizer│   chat messages    <───────    │    ├─ issues tool calls    │
│  WebSocket client│   agent state      <───────    │    └─ decides all actions  │
│                  │                                │                            │
└──────────────────┘                                │  Agent PTYs (up to 26)     │
                                                    │    ├─ embedded terminals   │
                                                    │    ├─ git worktree each    │
                                                    │    └─ rendered in 2x2 grid │
                                                    └────────────────────────────┘
```

## Console (Rust)

### Crate Structure

The console is a Cargo workspace with two crates:

```
console/
├── Cargo.toml              # Binary crate: dispatch-console
├── config.default.toml     # Default config template
├── src/
│   ├── main.rs             # Event loop, startup, keyboard handling
│   ├── app.rs              # App state, tool execution
│   ├── pty.rs              # PTY spawn, read, kill, resize
│   ├── ui.rs               # TUI rendering (ratatui)
│   ├── ws_server.rs        # WebSocket server (tokio)
│   ├── config.rs           # Config loading, TLS cert generation
│   ├── types.rs            # Core types (Mode, SlotState, App)
│   ├── util.rs             # String cleanup, repo scanning
│   └── mdns.rs             # mDNS service advertisement
└── core/
    ├── Cargo.toml           # Library crate: dispatch-core
    └── src/
        ├── lib.rs
        ├── protocol.rs      # WebSocket message types (RawInbound, OutboundMsg)
        ├── handler.rs       # Message routing, shared ConsoleState
        ├── orchestrator.rs  # LLM subprocess lifecycle
        └── tools.rs         # Tool definitions, resolution, formatting
```

**dispatch-core** is a pure-logic library with no async dependencies (only `serde` + `serde_json`). It defines the WebSocket protocol, message handling, orchestrator interface, and tool schemas. This separation keeps networking and TUI concerns out of the core logic.

**dispatch-console** is the binary. It depends on `dispatch-core` plus `tokio`, `ratatui`, `crossterm`, `portable-pty`, `vt100`, `tokio-tungstenite`, `tokio-rustls`, and `mdns-sd`.

### Threading Model

```
Main Thread (sync, ~60 FPS)
├── TUI rendering (ratatui + crossterm)
├── Keyboard input polling
├── Orchestrator output polling (try_recv)
├── PTY cleanup and idle detection
├── Tool execution
└── Agent status message processing

WebSocket Thread (async, tokio)
├── TLS termination
├── PSK authentication
├── Message dispatch via handle_message()
└── Chat broadcast to all connected radios

Orchestrator Reader Thread (blocking)
└── Reads claude stdout line-by-line, sends to main via mpsc

PTY Reader Threads (one per agent, blocking)
├── Reads PTY output in 4KB chunks
├── Feeds vt100 parser
└── Updates idle-detection timestamp
```

The main thread runs a synchronous 16ms tick loop. Everything it needs from background threads arrives via channels (`mpsc` or `broadcast`). Lock contention is minimal -- the only shared mutex is `ConsoleState` (held briefly for WebSocket message dispatch).

### Orchestrator

The orchestrator is a persistent `claude` process running in stream-json mode. It receives voice transcripts and system events, then responds with text and embedded tool calls.

**Lifecycle:** Spawned at startup. Reads from stdout via a background thread. Messages queued if the orchestrator is mid-response (flushed on turn completion). If interrupted or crashed, respawned.

**Communication protocol:**
- Input: JSON lines on stdin (`{"type":"user","content":"[MIC] do something"}`)
- Output: JSON lines on stdout, parsed for text and tool call blocks
- Tool calls: embedded as ` ```action {"action":"dispatch",...} ``` ` blocks in response text
- Tool results: sent back as user messages wrapped in `<tool_result>` tags

**System prompt:** Built at startup from `docs/ORCHESTRATOR.md`, the configured callsign pool, identity names, and tool definitions (JSON schema).

**Available tools:**

| Tool | Purpose |
|------|---------|
| `dispatch` | Launch a new agent in an empty slot |
| `terminate` | Kill an agent by callsign or slot |
| `merge` | Acknowledge an agent's merge completion |
| `list_agents` | Return all slot states |
| `list_repos` | Return available repositories |
| `message_agent` | Write text to an agent's PTY |

### Agent Slots

Each slot holds one running agent process in a PTY. Slots are indexed 0-based internally, reported 1-indexed externally.

**Per-slot state (SlotState):**
- PTY: `vt100::Parser` (screen buffer + scrollback), writer handle, child PID
- Identity: callsign, tool name, task ID, repo name/root
- Tracking: `last_output_at` timestamp, `idle` flag, `scroll_offset`

**Dispatch flow:**
1. `pty::dispatch_slot()` creates the PTY via `portable-pty`
2. Reads `docs/AGENTS.md` for agent instructions, appends shared memory from `.dispatch/MEMORY.md`
3. Sets `DISPATCH_MSG_FILE` env var pointing to `.dispatch/messages/{callsign}`
4. Tool-specific spawn:
   - **claude-code**: `claude --system-prompt <instructions> --dangerously-skip-permissions [task]`
   - **Other tools**: Writes instructions to `.dispatch/instructions/{callsign}.md`, sets `DISPATCH_INSTRUCTIONS_FILE` env var, and prepends an instruction-file reference to the task prompt
5. Starts a reader thread that feeds output to the vt100 parser and updates the idle-detection timestamp
6. Returns `SlotState` to the main thread

**Idle detection:** If an agent with a task ID produces no output for 10 seconds, it's marked idle and an `[EVENT] AGENT_IDLE` is sent to the orchestrator. New output transitions it back to working.

**Agent status messages:** Agents write messages to `.dispatch/messages/{callsign}` files (one line per message). The main loop polls these files for new content and forwards messages to the orchestrator and radio. This file-based approach eliminates the fragile terminal-output-parsing system that was prone to ANSI noise and ConPTY artifacts.

### Event Loop (main.rs)

The main loop runs every 16ms and processes in this order:

1. **PTY cleanup** -- detect exited children, clear slots, notify orchestrator
2. **Idle detection** -- check `last_output_at` timestamps, transition idle/busy
3. **Ticker animation** -- advance scrolling marquee, pulse status indicator
4. **WebSocket events** -- voice transcripts forwarded to orchestrator; images saved and paths written to agent PTY
5. **Agent status messages** -- poll `.dispatch/messages/` files, forward to orchestrator and radio chat
6. **Orchestrator output** -- parse tool calls from response text, execute tools, send results back
7. **TUI render** -- header, ticker, 2x2 agent grid (or orchestrator log), footer, overlays
8. **Keyboard input** -- command mode (navigation, dispatch, kill) or input mode (type to PTY)

### TUI Layout

```
┌──────────────────────────────────────────┐
│  Header (status dot, radio state, PSK,   │
│          agents, repos, orch, page, time)│
│  ◄◄ Ticker (scrolling event messages)    │
├───────────────────┬──────────────────────┤
│  Pane 1           │  Pane 2              │
│  [info strip]     │  [info strip]        │
│  [vt100 screen]   │  [vt100 screen]      │
├───────────────────┼──────────────────────┤
│  Pane 3           │  Pane 4              │
│  [info strip]     │  [info strip]        │
│  [vt100 screen]   │  [vt100 screen]      │
├───────────────────┴──────────────────────┤
│  Footer (mode, target, key hints)        │
└──────────────────────────────────────────┘
```

Each pane's info strip shows: slot number, callsign, status (WORK/IDLE), uptime, tool, and task ID. Border color indicates targeting (cyan = command mode target, green = input mode target).

Pressing `o` toggles to the orchestrator log view -- a timestamped, color-coded event history (voice transcripts, dispatches, tool calls, agent messages).

### WebSocket Server

Async tokio server with TLS (self-signed cert, auto-generated) and PSK authentication (query string parameter).

Each connection runs a `tokio::select!` loop: inbound messages are parsed and routed through `handle_message()` (from dispatch-core); outbound chat messages arrive via a `broadcast` channel shared across all connections.

**Shared state (`ConsoleState`):**
- `slots: Vec<Option<AgentSlot>>` -- slot states visible to the radio
- `target: Option<u32>` -- currently targeted slot
- `event_tx` -- channel to send events (voice, images, interrupts) to the main thread
- Protected by `Arc<Mutex<>>`

### Configuration

Config file at `~/.config/dispatch/config.toml` (platform-appropriate). Created with defaults on first run.

```toml
[server]
port = 9800

[auth]
psk = "<24-char hex, auto-generated>"

[terminal]
scrollback_lines = 1000

[agents]
callsigns = ["Alpha", "Bravo", ..., "Hotel"]

[identity]
user_callsign = "Dispatch"
console_name = "Console"

[tools]
ai-agent = "claude-code"
claude-code = "claude"
copilot = "gh copilot --"
```

The `ai-agent` key selects which tool is used when dispatching agents. Tool commands are resolved from the same table. The orchestrator always uses Claude Code regardless of this setting.

Callsigns are dynamically assigned from the pool -- each new agent gets the next unused name. The pool size determines max agents and page count (4 per page).

TLS certificates are self-signed, stored alongside config, and auto-regenerated if missing. The SHA-256 fingerprint is shared via mDNS TXT records for optional certificate pinning.

### Workspace Modes

- **Single-repo:** Console launched inside a git repo. All agents work in that repo.
- **Multi-repo:** Console launched in a parent directory containing multiple git repos. Agents can be dispatched to specific repos via an overlay selector.

## Radio (Android / Kotlin)

### App Structure

```
radio/app/src/main/
├── kotlin/com/dispatch/radio/
│   ├── MainActivity.kt              # Main UI, key dispatch, chat log
│   ├── SettingsActivity.kt          # Connection settings UI
│   ├── QrScanActivity.kt            # CameraX + ML Kit QR scanner
│   ├── PushToTalkManager.kt         # PTT speech recognition
│   ├── ContinuousListenManager.kt   # Hands-free VAD listening
│   ├── VolumeKeyBridge.kt           # Singleton key event bridge
│   ├── VolumeKeyAccessibilityService.kt  # Background volume key capture
│   ├── VolumeUpHandler.kt           # Agent status overlay trigger
│   ├── AgentStatusOverlay.kt        # Agent list dialog
│   ├── QuickDispatchOverlay.kt      # Tool picker (prepared)
│   ├── ConsoleDiscovery.kt          # mDNS NSD service browser
│   ├── RadioSettings.kt             # SharedPreferences wrapper
│   ├── HapticFeedback.kt            # Vibration patterns
│   ├── TargetCycler.kt              # Target slot cycling
│   ├── model/Agent.kt               # Agent data class + callsign colors
│   └── ui/AudioLevelView.kt         # RMS audio level bar (custom View)
└── java/com/dispatch/radio/
    └── RadioWebSocketClient.kt       # OkHttp WebSocket + TLS client
```

Single-activity app. Portrait-locked. Min SDK 28.

### Push-to-Talk Flow

```
Volume Down (hold)
  │
  ├─ PushToTalkManager.startListening()
  │    └─ SpeechRecognizer begins, silence threshold set to 60s+
  │
  ├─ Partial results displayed on screen
  │    └─ If recognizer auto-completes, text accumulated and recognizer restarts
  │
  Volume Down (release)
  │
  ├─ PushToTalkManager.stopListening()
  │    └─ Final transcript assembled from accumulated + last result
  │
  └─ Sent as: {"type":"send", "text":"...", "auto":true}
```

**Continuous listening mode** (toggled by tapping Volume Down when enabled in settings): Uses Android's built-in voice activity detection with 3.5s silence threshold. Auto-restarts after each utterance. Emits normalized RMS audio levels for the visual level indicator.

### WebSocket Client

OkHttp-based. Connects to `wss://host:port/?psk=<key>`. TLS trust manager accepts self-signed certs; optional SHA-256 certificate pinning from QR scan. Auto-reconnects with exponential backoff (1s to 30s). Sends `list_agents` on connect to sync state. All callbacks posted to main looper.

### Volume Key Architecture

Volume keys are intercepted even when the app is backgrounded or the screen is off, via an accessibility service:

```
VolumeKeyAccessibilityService
  │ (intercepts key events when app not in foreground)
  └─ VolumeKeyBridge (singleton)
       └─ MainActivity.onKeyEvent lambda
            ├─ Volume Down → PTT or continuous listen toggle
            └─ Volume Up  → show/dismiss AgentStatusOverlay
```

When the activity is in the foreground, `dispatchKeyEvent()` handles keys directly. The bridge is only used for background capture.

### Connection Setup

Three paths to configure the console connection:

1. **mDNS discovery** -- browse for `_dispatch._tcp` services on the local network
2. **QR code** -- scan a code containing `wss://host:port/?psk=<key>&fp=<sha256>`
3. **Manual entry** -- type host, port, and PSK in settings

### Chat Log

Scrollable message history showing voice transcripts, orchestrator decisions, and agent events. Messages are color-coded by sender (user = red, console = green, agents = unique per-callsign colors). Long messages collapse to one line with tap-to-expand. Capped at 100 entries.

## WebSocket Protocol

All messages are JSON with a `type` field discriminator.

**Radio to Console:**

| Type | Key Fields | Purpose |
|------|-----------|---------|
| `send` | `text`, `auto` | Voice transcript (auto=true) or typed text |
| `list_agents` | -- | Request current slot states |
| `set_target` | `slot` | Change targeted agent |
| `dispatch` | `tool` | Request new agent (via quick dispatch) |
| `terminate` | `callsign` | Kill agent |
| `interrupt` | -- | Interrupt the orchestrator |
| `send_image` | `callsign`, `data`, `filename` | Base64 image to agent |
| `radio_status` | `state` | Listening/idle state |

**Console to Radio:**

| Type | Key Fields | Purpose |
|------|-----------|---------|
| `agents` | `slots[]`, `target`, `queued_tasks` | Full state sync |
| `chat` | `sender`, `text` | Push message to chat log |
| `ack` | `seq` | Send acknowledgment |
| `dispatched` | `seq` | Dispatch confirmation |
| `terminated` | `seq` | Termination confirmation |
| `target_changed` | `slot` | Target update |
| `error` | `message` | Protocol error |

## Agent Lifecycle

```
Orchestrator calls dispatch(repo, prompt, callsign)
  │
  ├─ Console assigns slot, spawns PTY
  │    └─ claude launched with system prompt from docs/AGENTS.md + MEMORY.md
  │
  ├─ Agent writes "Task received" to .dispatch/messages/{callsign}
  │
  ├─ Agent creates git worktree (.dispatch/.worktrees/{callsign})
  ├─ Agent works, commits on dispatch/{callsign} branch
  ├─ Agent merges to main, removes worktree, pushes
  │
  ├─ Agent writes "Done. Fixed X, merged and pushed." to message file
  │
  ├─ Agent goes idle at prompt
  │    └─ Console detects 10s inactivity → AGENT_IDLE event
  │    └─ Orchestrator can send follow-up via message_agent
  │
  └─ On explicit termination: PTY killed, slot cleared, callsign returned to pool
```

Agents share knowledge through `.dispatch/MEMORY.md` in the target repo -- a gitignored file where agents record build commands, gotchas, and architectural notes for future agents.

## Key Design Decisions

1. **LLM orchestrator, thin console.** The orchestrator makes all decisions. The console is a runtime that executes tool calls, manages PTYs, and renders the TUI. No decision logic in the console.

2. **Worktree-per-agent.** Each agent works on its own branch in its own worktree. Parallel work without merge conflicts. Agents handle the full git lifecycle (create, commit, merge, push, clean up).

3. **File-based agent communication.** Agents write messages to `.dispatch/messages/{callsign}` files. The console's main loop polls these files for new content and routes messages to the orchestrator and radio. This avoids the fragility of parsing messages from terminal output (ANSI noise, ConPTY artifacts, TUI redraws).

4. **Sync main loop, async networking.** The TUI runs a synchronous 16ms tick loop for predictable rendering and input handling. Networking (WebSocket, mDNS) runs on a separate tokio runtime. Communication is channel-based.

5. **Core crate isolation.** Protocol definitions, message handling, tool schemas, and orchestrator logic are in `dispatch-core` with no async dependencies. The binary crate handles I/O.

6. **Dynamic callsign assignment.** Callsigns are drawn from a configured pool, not fixed to slot numbers. This allows flexible naming and slot reuse.

7. **Background volume keys.** An Android accessibility service captures volume keys even when the app is backgrounded or the screen is off, enabling truly hands-free operation.
