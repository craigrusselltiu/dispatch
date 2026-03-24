# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches AI coding agents. Speak a prompt, and the orchestrator launches agents into isolated git worktrees where they work, commit, merge to main, and push -- all hands-free.

```
┌──────────────┐    WebSocket TLS (LAN, PSK)   ┌──────────────────┐
│  Dispatch    │  <------------------------->  │  Dispatch        │
│  Radio       │                               │  Console         │
│  (Android)   │                               │  (PC TUI)        │
│              │                               │                  │
│  Volume keys │                               │  Agent slots     │
│  Speech-to-  │                               │  Embedded PTYs   │
│  text        │                               │  Git worktrees   │
└──────────────┘                               └──────────────────┘
```

## Components

**Dispatch Console** (PC) -- a terminal UI with configurable agent slots displayed in a 2x2 grid across pages. A persistent LLM orchestrator receives voice transcripts and coordinates everything: dispatching agents, routing messages, managing work. Supports direct keyboard input into any agent pane via vim-style modal controls.

<img width="2555" height="1343" alt="image" src="https://github.com/user-attachments/assets/563decbe-8171-438b-95cc-ee9e9755b8c0" />

**Dispatch Radio** (Android) -- a push-to-talk app controlled by hardware volume buttons. Hold Volume Down to speak; the app transcribes speech and sends it to the console over a TLS-encrypted WebSocket on your local network.

<img width="500" height="1541" alt="image" src="https://github.com/user-attachments/assets/2230a774-4f7a-4e6b-838d-838fa4c328e7" />


## How It Works

1. **Speak** -- hold Volume Down and say something like "refactor the auth system" or "fix the login bug."
2. **Dispatch** -- the orchestrator interprets your intent and launches one or more agents, each in its own git worktree and branch.
3. **Work** -- agents do their work autonomously: edit code, commit changes, merge to main, clean up, and push.
4. **Monitor** -- watch progress in real-time via the 2x2 agent grid, the scrolling LED ticker, the orchestrator event log, or the radio's chat history.

No fixed command patterns. The orchestrator understands natural language and uses full conversational context to decide the best action.

## Setup

### Prerequisites

- **Console**: Rust toolchain (`cargo`), Git
- **Radio**: Android Studio, Android device running API 28+ (Android 9+)
- Both devices on the same local network

### Console (PC)

```sh
cd console
cargo install --path .
```

Then `cd` into any git repo and run:

```sh
dispatch
```

On first run, a config file is generated with a random pre-shared key (PSK):

| Platform | Config path |
|----------|-------------|
| Linux    | `~/.config/dispatch/config.toml` |
| macOS    | `~/Library/Application Support/dispatch/config.toml` |
| Windows  | `%APPDATA%\dispatch\config.toml` |

The PSK is displayed in the console header bar. You will need it to connect the radio.

Agent callsigns are configured in the `[agents]` section. The user and console display names are configurable in the `[identity]` section (`user_callsign` and `console_name`). See [`console/config.default.toml`](console/config.default.toml) for the full default configuration.

### Radio (Android)

Open the `radio/` directory in Android Studio, sync Gradle, and deploy to your phone over USB or Wi-Fi debugging. Once installed, open the app's settings and connect to the console by tapping **DISCOVER CONSOLE** (mDNS) or entering the IP, port, and PSK manually (press `x` in the console to display connection details). Save connection details as named profiles to switch between different consoles without re-entering credentials.

## Usage

### Console Keybindings

**Command mode** (default) -- keystrokes control the console:

| Key               | Action                                     |
|-------------------|--------------------------------------------|
| `Enter`           | Enter input mode on targeted pane          |
| `1`-`4`           | Select target slot on current page         |
| `Tab`             | Cycle target forward across all agents     |
| `Right` / `Left`  | Next / previous page                       |
| `PgUp` / `PgDn`   | Scroll pane output up / down               |
| `n`               | Spawn new agent in empty targeted slot     |
| `c`               | Interrupt orchestrator                     |
| `k`               | Kill agent in targeted slot                |
| `o`               | Toggle orchestrator view (event log)       |
| `p`               | Show / hide full PSK                       |
| `x`               | Show connection info overlay               |
| `q`               | Quit                                       |
| `?`               | Toggle help overlay                        |

**Input mode** -- keystrokes go directly to the agent's terminal (PTY). Press `Escape` to return to command mode. Double-tap `Escape` to send a literal Escape to the agent.

### Radio Controls

| Control                | Action                                     |
|------------------------|--------------------------------------------|
| Volume Down (hold)     | Push-to-talk: speak a command              |
| Volume Down (release)  | Send transcript to console                 |
| Volume Down (tap)      | Toggle continuous listening mode            |
| Volume Up (hold)       | Show agent status overlay (hold to view)   |

**Continuous listening**: Enable in settings to stay in listen mode without holding Volume Down. Uses voice-activity detection to start and stop automatically.

**Background volume keys**: Enable the Dispatch Radio accessibility service (Android Settings > Accessibility) to use volume buttons even when the screen is off or the app is in the background.

### Voice Examples

| Say this                                   | What happens                          |
|--------------------------------------------|---------------------------------------|
| "Alpha, refactor the auth module"          | Message sent to Alpha                 |
| "dispatch an agent to fix the login bug"   | New agent launched with the prompt    |
| "terminate bravo"                          | Bravo is terminated                   |
| "what agents are running"                  | Orchestrator lists active agents      |
| "what did alpha do"                        | Orchestrator summarizes Alpha's work  |

## Key Features

- **LLM orchestrator** -- a persistent headless Claude process acts as the central coordinator. Voice transcripts go directly to it; it responds with tool calls. No command parsing.
- **Multi-agent support** -- supports Claude Code and GitHub Copilot CLI as AI agents. The orchestrator can dispatch either tool per task. Copilot runs in YOLO mode for fully autonomous operation.
- **Embedded terminals** -- each pane is a real PTY with full color, interactive TUI support, tab completion, and signal handling.
- **Git worktree isolation** -- each agent works on its own branch in its own worktree. Agents run in parallel without conflicts and handle their own merging.
- **Configurable agent names** -- agent callsigns are defined in `config.toml`. The number of entries determines slot count and page layout. Defaults to NATO phonetic alphabet (Alpha through Hotel). All agents are addressable by voice from any page.
- **Configurable identity** -- the user and console display names are configurable in `config.toml` (`[identity]` section). These names appear in the radio chat log and in orchestrator/agent prompts. Defaults to "Dispatch" (user) and "Console" (orchestrator).
- **LED ticker** -- a scrolling marquee shows dispatches, completions, merge results, and errors in real-time without consuming pane space.
- **Clean target repo** -- all dispatch artifacts live in `.dispatch/` (gitignored). Your repo stays untouched.
- **Networking** -- TLS-encrypted WebSocket with PSK authentication. mDNS auto-discovery eliminates manual IP configuration.
- **Radio chat log** -- the radio displays a scrollable chat history of orchestrator decisions, agent events, and voice transcripts. Monitor progress without looking at the console.
- **Multi-repo mode** -- launch `dispatch` from a parent directory containing multiple git repos to work across repositories.
- **Cross-platform** -- the console runs on Windows (ConPTY), macOS, and Linux.

## CLI Reference

```
dispatch                    # Start the console in the current repo
dispatch regenerate-psk     # Generate a new PSK
dispatch show-psk           # Print the current PSK
dispatch edit-config        # Open config.toml in VS Code
dispatch channel save <name>    # Save current config as a named channel
dispatch channel load <name>    # Load a saved channel (restart to apply)
dispatch channel list           # List all saved channels
dispatch channel delete <name>  # Delete a saved channel
dispatch channel show <name>    # Print contents of a saved channel
```

### Console Channels

Channels let you save and switch between different console configurations -- like tuning a radio dial to different frequencies. Each channel is a snapshot of your `config.toml` (server port, PSK, agent callsigns, identity, tools).

Save your current setup before changing anything:

```sh
dispatch channel save home
```

Create different configurations for different machines or contexts, then load the one you need:

```sh
dispatch channel load work
```

Channels are stored as individual TOML files in the config directory under `channels/`.

## Repository Structure

```
dispatch/
  console/                    # PC TUI (Rust)
    config.default.toml       # Default configuration template
  radio/                      # Android app (Kotlin)
  docs/
    SPEC.md                   # Full system specification
    ARCHITECTURE.md           # Architecture overview
    ORCHESTRATOR.md           # Orchestrator behavior and tool reference
    AGENTS.md                 # Template injected into agent prompts
```

## Inspiration

A fun little vibe-coded side-project inspired by Brian Harms's project, also named Dispatch. Not to be confused with Claude's new Dispatch feature.
