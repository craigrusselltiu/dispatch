# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches AI coding agents. Voice a prompt and the orchestrator dispatches agents that work in isolated git worktrees, merge to main, and push when done.

## Overview

Dispatch has two components:

- **Dispatch Radio** (Android) -- a minimal push-to-talk app controlled via hardware volume buttons. Hold Volume Down to speak; the app transcribes speech and sends raw transcripts to the console over a local WebSocket connection.
- **Dispatch Console** (PC) -- a TUI command center with up to 26 embedded terminal panes, each running a live AI agent session. A persistent LLM orchestrator receives voice transcripts and decides what to do -- dispatch agents, coordinate work, etc. Supports direct keyboard input into any agent pane via a vim-style modal interface.

```
┌──────────────┐    WebSocket TLS (LAN, PSK)   ┌──────────────────┐
│  Dispatch    │  <-------------------------> │  Dispatch        │
│  Radio       │                              │  Console         │
│  (Android)   │                              │  (PC TUI)        │
│              │                              │                  │
│  Volume keys │                              │  4x embedded     │
│  Speech-to-  │                              │  terminals (PTY) │
│  text        │                              │  Git worktrees   │
│              │                              │  .dispatch/      │
└──────────────┘                              └──────────────────┘
```

## Repository Structure

```
dispatch/
  radio/               # Android phone app (Kotlin, Gradle)
    app/               # Phone radio module
  console/             # PC TUI (Rust, Cargo)
  docs/
    SPEC.md            # Full system specification
    ARCHITECTURE.md    # High-level architecture overview
    ORCHESTRATOR.md    # Orchestrator behavior and tool reference
    AGENTS.md          # Template injected into agent prompts
  README.md
```

When agents are first dispatched in a target repo, Dispatch creates a `.dispatch/` directory:

```
sample-repo/
  .dispatch/
    .worktrees/        # Git worktrees for active agents
  (repo's own files)
```

## Setup

### Prerequisites

- **Dispatch Console**: Rust toolchain (`cargo`), Git
- **Dispatch Radio**: Android Studio, Android device running API 28+ (Android 9+)
- Both devices on the same local network

### Console (PC)

```sh
cd console
cargo install --path .
dispatch
```

On first run, a config file is auto-generated with a random pre-shared key (PSK):

- **Linux**: `~/.config/dispatch/config.toml`
- **macOS**: `~/Library/Application Support/dispatch/config.toml`
- **Windows**: `%APPDATA%\dispatch\config.toml`

The PSK is displayed in the console header bar. You'll need it to connect the radio.

### Radio (Android)

1. Open the `radio/` directory in Android Studio (File > Open, select the `radio/` folder).
2. Android Studio will start downloading dependencies and syncing the project automatically. You'll see a progress bar at the bottom of the window -- wait for it to finish. This can take a few minutes the first time. If it doesn't start automatically, click the elephant icon with a blue arrow in the toolbar (Sync Project with Gradle Files), or go to File > Sync Project with Gradle Files. When sync is done, the toolbar dropdown that said "Add Configuration" will now say **app**.
3. On your phone, enable Developer Options: go to Settings > About phone and tap "Build number" 7 times until it says "You are now a developer."
4. Connect your phone to Android Studio using either method:
   - **Wi-Fi (Android 11+)**: on your phone, go to Settings > Developer options > Wireless debugging and toggle it on. In Android Studio, go to the menu bar: View > Tool Windows > Running Devices. In the Running Devices panel, click the **+** button and select **Pair Devices Using Wi-Fi**. Choose either QR code or pairing code and follow the prompts to pair.
   - **USB**: plug your phone into your PC via USB cable. On your phone, go to Settings > Developer options and enable USB debugging. Tap "Allow" on the prompt that appears on your phone.
5. In the toolbar, you'll see two dropdowns side by side: one says **app** (the run configuration) and one shows available devices. Select your phone from the device dropdown, then click the green play button to the right of it. Android Studio will build and install the app on your phone.
6. Once the app is running on your phone, pair it to the Dispatch console:
   - **Connection info**: press `x` in the Dispatch console to show the connection info overlay (address, port, PSK). In the radio app on your phone, tap the gear icon in the top-right corner to open settings, then enter the displayed values.
   - **mDNS auto-discovery**: in the radio app settings, tap **DISCOVER CONSOLE** to auto-detect the console on your network.
   - **Manual**: in the radio app settings, enter the console's IP address, port, and PSK. The PSK is shown in the console header bar.

## Usage

`cd` into any git repo and run `dispatch`. The console starts listening for voice commands. A `.dispatch/` directory is created in the repo when the first agent is dispatched. You can also launch `dispatch` from a parent directory that contains multiple git repos -- it will detect the repos and let you choose which to target (multi-repo mode).

### Console

The console displays four agent panes at a time in a 2x2 grid with a scrolling ticker line for task events. Each agent runs in a fully interactive embedded terminal (PTY) inside an isolated git worktree.

**Command mode** (default) -- keystrokes control the console:

| Key               | Action                                              |
|-------------------|-----------------------------------------------------|
| `Enter`           | Enter input mode on targeted pane                   |
| `1`-`4`           | Select target slot on current page                  |
| `Tab`             | Cycle target forward across all agents              |
| `Right` / `Left`  | Next / previous page                                |
| `PgUp` / `PgDn`   | Scroll pane output up/down                          |
| `k`               | Kill agent in targeted slot                         |
| `o`               | Toggle orchestrator view (event log)                |
| `p`               | Show/hide full PSK                                  |
| `x`               | Show connection info                                |
| `q`               | Quit                                                |
| `?`               | Toggle help overlay                                 |

**Input mode** -- keystrokes go directly to the agent's terminal. Press `Escape` to immediately return to command mode. Double-tap `Escape` to send a literal Escape to the PTY.

### Radio

| Control                | Action                                              |
|------------------------|-----------------------------------------------------|
| Volume Down (hold)     | Push-to-talk: speak a command or prompt             |
| Volume Down (release)  | Send transcript to console                          |
| Volume Down (tap)      | Toggle continuous listening (when enabled in settings) |
| Volume Up              | Cycle to next active agent                          |
| Volume Up (hold >1s)   | Quick dispatch: pick and launch a new agent type    |

**Continuous listening mode** -- enable in settings to stay in listen mode without holding Volume Down. Uses voice-activity detection to automatically detect speech start and end. Volume Down becomes a toggle instead of push-to-talk.

**Background volume keys** -- enable the Dispatch Radio accessibility service in Android Settings > Accessibility to use volume buttons for PTT and target cycling even when the screen is off or the app is in the background. A shortcut button in the radio's settings screen opens the Android accessibility settings.

### Voice Commands

Speak naturally. The radio sends raw transcripts to the console's LLM orchestrator, which decides what to do:

| Utterance                            | Orchestrator action                      |
|--------------------------------------|------------------------------------------|
| "Alpha, refactor the auth module"    | Message Alpha with the prompt            |
| "dispatch an agent to fix the bug"   | Dispatch a new agent                     |
| "terminate bravo"                    | Terminate the Bravo agent                |
| "what agents are running"            | List active agents                       |
| "refactor the auth system"           | Dispatch agents to handle it             |
| "what did alpha do"                  | Summarize Alpha's recent work            |

No fixed command patterns -- the orchestrator understands natural language and uses conversational context.

## How It Works

1. **Voice a prompt** -- say something like "refactor the auth system" or "fix the login bug".
2. **Dispatch** -- the orchestrator sends the prompt to one or more agents via the `dispatch` tool. Each agent gets its own git worktree and branch.
3. **Work** -- agents do their work, commit, merge to main, clean up their worktree, and push.
4. **Ticker** -- a scrolling LED-style marquee shows events in real-time: dispatches, completions, and errors.

## Key Features

- **LLM orchestrator** -- a persistent headless Claude process acts as the central coordinator. Voice transcripts go directly to the orchestrator, which decides what to do via tool calls. No command parsing -- just natural language.
- **Embedded terminals** -- each pane is a real PTY, not text capture. Full color, interactive TUI apps, tab completion, Ctrl+C -- everything works.
- **Git worktree isolation** -- each agent runs on its own branch in its own worktree. Agents work in parallel without conflicts and handle their own merging.
- **Multi-agent dispatch** -- voice a complex task and the orchestrator can dispatch multiple agents to tackle different parts in parallel.
- **LED ticker** -- scrolling marquee shows dispatches, completions, merge results, and errors without consuming pane space.
- **Auto-dispatch** -- send a prompt without specifying an agent and the orchestrator decides the best action: message an existing agent or launch a new one.
- **NATO callsigns** -- agents are assigned Alpha, Bravo, Charlie, ... in dispatch order. Addressable by voice from any page.
- **Paged layout** -- up to 26 agents across 7 pages. Off-screen agents keep running and are still addressable.
- **Clean target repo** -- all dispatch artifacts live in `.dispatch/` (gitignored). Your repo stays untouched.
- **mDNS discovery** -- the console advertises itself on the LAN via Zeroconf. The radio can find it automatically without manual IP entry.
- **TLS encryption** -- WebSocket connections are wrapped in TLS (wss://) with a self-signed certificate.
- **PSK authentication** -- all WebSocket connections require a pre-shared key. Auto-generated on first run.
- **Radio chat log** -- the radio displays a scrollable chat history showing orchestrator decisions, agent events, and voice transcripts in real-time. No need to look at the console to know what's happening.
- **Cross-platform** -- console runs on Windows (ConPTY), macOS, and Linux.

## CLI Reference

```
dispatch                    # Start the console in the current repo
dispatch regenerate-psk     # Generate a new PSK
dispatch show-psk           # Print the current PSK
dispatch config             # Print config file path
```
