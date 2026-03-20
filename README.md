# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches AI coding agents. Speak a prompt, and the orchestrator launches agents into isolated git worktrees where they work, commit, merge to main, and push -- all hands-free.

```
┌──────────────┐    WebSocket TLS (LAN, PSK)   ┌──────────────────┐
│  Dispatch    │  <------------------------->  │  Dispatch        │
│  Radio       │                               │  Console         │
│  (Android)   │                               │  (PC TUI)        │
│              │                               │                  │
│  Volume keys │                               │  26 agent slots  │
│  Speech-to-  │                               │  Embedded PTYs   │
│  text        │                               │  Git worktrees   │
└──────────────┘                               └──────────────────┘
```

## Components

**Dispatch Console** (PC) -- a terminal UI with up to 26 embedded agent panes displayed in a 2x2 grid across pages. A persistent LLM orchestrator receives voice transcripts and coordinates everything: dispatching agents, routing messages, managing work. Supports direct keyboard input into any agent pane via vim-style modal controls.

**Dispatch Radio** (Android) -- a push-to-talk app controlled by hardware volume buttons. Hold Volume Down to speak; the app transcribes speech and sends it to the console over a TLS-encrypted WebSocket on your local network.

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

### Radio (Android)

1. Open the `radio/` directory in Android Studio (File > Open).
2. Wait for Gradle sync to finish (progress bar at the bottom). If it does not start automatically, click the elephant icon in the toolbar or go to File > Sync Project with Gradle Files.
3. Enable Developer Options on your phone: Settings > About phone, tap "Build number" 7 times.
4. Connect your phone to Android Studio:
   - **Wi-Fi (Android 11+)**: Settings > Developer options > Wireless debugging. In Android Studio: View > Tool Windows > Running Devices, click **+**, select **Pair Devices Using Wi-Fi**.
   - **USB**: Plug in via USB, enable USB debugging in Developer options, tap "Allow" on the phone prompt.
5. Select your phone from the device dropdown in the toolbar and click the green play button to build and install.
6. Pair the radio to the console using one of these methods:
   - **Auto-discovery**: In the radio's settings, tap **DISCOVER CONSOLE** to find the console on your network via mDNS.
   - **Connection overlay**: Press `x` in the console to show the address, port, and PSK. Enter these in the radio's settings (gear icon).
   - **Manual**: Enter the console's IP, port, and PSK directly in the radio's settings.

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
| Volume Up (tap)        | Cycle to next active agent                 |
| Volume Up (hold >1s)   | Quick dispatch: pick and launch an agent   |

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
- **Embedded terminals** -- each pane is a real PTY with full color, interactive TUI support, tab completion, and signal handling.
- **Git worktree isolation** -- each agent works on its own branch in its own worktree. Agents run in parallel without conflicts and handle their own merging.
- **NATO callsigns** -- agents are named Alpha through Zulu in dispatch order (up to 26 concurrent agents across 7 pages). All agents are addressable by voice from any page.
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
dispatch config             # Print config file path
```

## Repository Structure

```
dispatch/
  console/             # PC TUI (Rust)
  radio/               # Android app (Kotlin)
  docs/
    SPEC.md            # Full system specification
    ARCHITECTURE.md    # Architecture overview
    ORCHESTRATOR.md    # Orchestrator behavior and tool reference
    AGENTS.md          # Template injected into agent prompts
```

## Inspiration

This project was inspired by [a video from Brian Harms](https://www.instagram.com/brian_harms/).
