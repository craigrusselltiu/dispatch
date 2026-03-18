# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches tasks to AI coding agents. The PC-side TUI gives you a live quad-pane view of embedded agent terminals, backed by [Beads](https://github.com/steveyegge/beads) for structured task tracking, dependency management, and persistent memory across sessions.

## Overview

Dispatch has two components:

- **Dispatch Radio** (Android) -- a minimal push-to-talk app controlled via hardware volume buttons. Hold Volume Down to speak; the app transcribes speech, parses voice commands, and sends structured messages to the console over a local WebSocket connection.
- **Dispatch Console** (PC) -- a TUI command center with up to 26 embedded terminal panes, each running a live AI agent session. Receives voice commands from the radio, manages agent lifecycles, and integrates with Beads for task orchestration. Supports direct keyboard input into any agent pane via a vim-style modal interface.

```
┌──────────────┐     WebSocket (LAN, PSK)     ┌──────────────────┐
│  Dispatch    │  ◄────────────────────────►  │  Dispatch        │
│  Radio       │                               │  Console         │
│  (Android)   │                               │  (PC TUI)        │
│              │                               │                  │
│  Volume keys │                               │  4x embedded     │
│  Speech-to-  │                               │  terminals (PTY) │
│  text, voice │                               │  Beads task      │
│  commands    │                               │  orchestration   │
└──────────────┘                               └──────────────────┘
```

## Repository Structure

```
dispatch/
  radio/               # Android app (Kotlin, Gradle)
  console/             # PC TUI (Rust, Cargo)
  docs/                # SPEC.md and other documentation
  .beads/              # Beads issue tracker
  AGENTS.md            # Agent workflow instructions
```

## Setup

### Prerequisites

- **Dispatch Console**: Rust toolchain (`cargo`), [`bd` CLI](https://github.com/steveyegge/beads) on PATH
- **Dispatch Radio**: Android Studio, Android device running API 28+ (Android 9+)
- Both devices on the same local network

### Console (PC)

```sh
cd console
cargo build --release
./target/release/dispatch
```

On first run, a config file is auto-generated with a random pre-shared key (PSK):

- **Linux**: `~/.config/dispatch/config.toml`
- **macOS**: `~/Library/Application Support/dispatch/config.toml`
- **Windows**: `%APPDATA%\dispatch\config.toml`

The PSK is displayed in the console header bar. You'll need it to connect the radio.

### Radio (Android)

1. Open the `radio/` directory in Android Studio.
2. Build and install the app on your Android device.
3. Enter the console's IP address and PSK in the radio's settings screen.

### Beads Setup

Dispatch uses Beads for task tracking. After cloning, initialize the database:

```sh
bd init
```

See `AGENTS.md` for the full agent workflow.

## Usage

### Console

The console displays four agent panes at a time in a 2x2 grid. Each agent runs in a fully interactive embedded terminal (PTY).

**Command mode** (default) -- keystrokes control the console:

| Key               | Action                                              |
|-------------------|-----------------------------------------------------|
| `Enter` / `i`     | Enter input mode on targeted pane                   |
| `1`-`4`           | Select target slot on current page                  |
| `Tab`             | Cycle target forward across all agents              |
| `]` / `[`         | Next / previous page                                |
| `n`               | Dispatch new agent into first empty slot            |
| `x`               | Terminate agent in targeted slot                    |
| `R`               | Rename agent in targeted slot                       |
| `t`               | Show Beads task list                                |
| `p`               | Show/hide full PSK                                  |
| `q`               | Quit                                                |
| `?`               | Toggle help overlay                                 |

**Input mode** -- keystrokes go directly to the agent's terminal. Press `Escape` to return to command mode.

### Radio

| Control                | Action                                              |
|------------------------|-----------------------------------------------------|
| Volume Down (hold)     | Push-to-talk: speak a command or prompt             |
| Volume Down (release)  | Send transcript to console                          |
| Volume Up              | Cycle to next active agent                          |
| Volume Up (hold >1s)   | Quick dispatch: pick and launch a new agent type    |

### Voice Commands

Speak naturally. The radio parses the transcript before sending:

| Utterance                            | Result                                   |
|--------------------------------------|------------------------------------------|
| "Alpha, refactor the auth module"    | Send prompt to Alpha                     |
| "dispatch claude code"               | Launch a new Claude Code agent           |
| "terminate bravo"                    | Terminate the Bravo agent                |
| "switch to charlie"                  | Change default target to Charlie         |
| "fix the login bug"                  | Send prompt to current target            |

Unaddressed prompts go to the current target. If no agents are running, the console auto-dispatches one.

## Key Features

- **Embedded terminals** -- each pane is a real PTY, not text capture. Full color, interactive TUI apps, tab completion, Ctrl+C -- everything works.
- **Beads integration** -- every voice prompt creates a tracked task with a persistent ID. Agents can close tasks themselves; the console also detects completion via idle prompt patterns or inactivity timeout.
- **Auto-dispatch** -- send a prompt without specifying an agent and the console finds an idle agent, launches a new one, or queues the task if all slots are busy.
- **NATO callsigns** -- agents are assigned Alpha, Bravo, Charlie, ... in dispatch order. Addressable by voice from any page.
- **Paged layout** -- up to 26 agents across 7 pages. Off-screen agents keep running and are still addressable.
- **PSK authentication** -- all WebSocket connections require a pre-shared key. Auto-generated on first run.
- **Cross-platform** -- console runs on Windows (ConPTY), macOS, and Linux.

## CLI Reference

```
dispatch                    # Start the console
dispatch regenerate-psk     # Generate a new PSK
dispatch show-psk           # Print the current PSK
dispatch config             # Print config file path
```
