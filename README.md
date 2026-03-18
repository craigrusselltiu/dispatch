# Dispatch

> Voice-powered command center for AI coding agents.

Turn your Android phone into a push-to-talk radio that dispatches tasks to AI coding agents. Voice a big task and the console plans it, breaks it into subtasks, dispatches agents into isolated git worktrees, and merges results back -- all tracked in a simple markdown file (`.dispatch/tasks.md`).

## Overview

Dispatch has two components:

- **Dispatch Radio** (Android) -- a minimal push-to-talk app controlled via hardware volume buttons. Hold Volume Down to speak; the app transcribes speech, parses voice commands, and sends structured messages to the console over a local WebSocket connection.
- **Dispatch Console** (PC) -- a TUI command center with up to 26 embedded terminal panes, each running a live AI agent session. Receives voice commands from the radio, plans and decomposes tasks, dispatches agents into git worktrees, tracks progress in `.dispatch/tasks.md`, and merges completed work. Supports direct keyboard input into any agent pane via a vim-style modal interface.

```
┌──────────────┐     WebSocket (LAN, PSK)     ┌──────────────────┐
│  Dispatch    │  <-------------------------> │  Dispatch        │
│  Radio       │                              │  Console         │
│  (Android)   │                              │  (PC TUI)        │
│              │                              │                  │
│  Volume keys │                              │  4x embedded     │
│  Speech-to-  │                              │  terminals (PTY) │
│  text, voice │                              │  Git worktrees   │
│  commands    │                              │  .dispatch/      │
└──────────────┘                              └──────────────────┘
```

## Repository Structure

```
dispatch/
  radio/               # Android app (Kotlin, Gradle)
  console/             # PC TUI (Rust, Cargo)
  docs/
    SPEC.md            # Full system specification
    ARCHITECTURE.md    # High-level architecture overview
    CONSOLE.md         # Console task management reference
    AGENTS.md          # Template injected into agent prompts
  README.md
```

When you run `dispatch` in a target repo, it creates a `.dispatch/` directory:

```
sample-repo/
  .dispatch/
    tasks.md           # Live task plan (read/written by the console)
    .worktrees/        # Git worktrees for active tasks
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
3. Connect the radio: press `Q` in the console to show a QR code, then scan it from the radio's settings screen. Or enter the console's IP address and PSK manually.

## Usage

`cd` into any git repo and run `dispatch`. The console creates `.dispatch/` in the repo and starts listening for voice commands.

### Console

The console displays four agent panes at a time in a 2x2 grid with a scrolling ticker line for task events. Each agent runs in a fully interactive embedded terminal (PTY) inside an isolated git worktree.

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
| `t`               | Show task list overlay                              |
| `p`               | Show/hide full PSK                                  |
| `Q`               | Show QR code for radio pairing                      |
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
| "refactor the auth system"           | Plan, decompose, and dispatch subtasks   |
| "fix the login bug"                  | Send prompt to current target            |

Unaddressed prompts go to the current target. If no agents are running, the console auto-dispatches one.

## How Task Management Works

1. **Voice a task** -- say something like "refactor the auth system".
2. **Planning** -- the console spawns a headless planner agent that breaks it down into subtasks with dependencies, written to `.dispatch/tasks.md`.
3. **Dispatch** -- the console finds unblocked tasks and dispatches agents into isolated git worktrees (one branch per task).
4. **Completion** -- when an agent finishes, the console merges the branch to main, marks the task done, and dispatches the next unblocked task.
5. **Ticker** -- a scrolling LED-style marquee shows task events in real-time: planning status, dispatches, merges, and errors.

Simple one-off prompts skip the planning step and dispatch directly.

## Key Features

- **Embedded terminals** -- each pane is a real PTY, not text capture. Full color, interactive TUI apps, tab completion, Ctrl+C -- everything works.
- **Git worktree isolation** -- each task runs on its own branch in its own worktree. Agents work in parallel without conflicts. Completed work is auto-merged.
- **Task planning** -- voice a complex task and the console decomposes it into subtasks with dependencies, then orchestrates execution automatically.
- **LED ticker** -- scrolling marquee shows planner status, task completions, merge results, and errors without consuming pane space.
- **Auto-dispatch** -- send a prompt without specifying an agent and the console finds an idle agent, launches a new one, or queues the task if all slots are busy.
- **NATO callsigns** -- agents are assigned Alpha, Bravo, Charlie, ... in dispatch order. Addressable by voice from any page.
- **Paged layout** -- up to 26 agents across 7 pages. Off-screen agents keep running and are still addressable.
- **Clean target repo** -- all dispatch artifacts live in `.dispatch/` (gitignored). Your repo stays untouched.
- **PSK authentication** -- all WebSocket connections require a pre-shared key. Auto-generated on first run.
- **Cross-platform** -- console runs on Windows (ConPTY), macOS, and Linux.

## CLI Reference

```
dispatch                    # Start the console in the current repo
dispatch regenerate-psk     # Generate a new PSK
dispatch show-psk           # Print the current PSK
dispatch config             # Print config file path
```
