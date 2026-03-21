# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## v0.3.6

### Changed

- Prompt system messages in the radio app are now collapsible. Dispatch and send-to-agent messages show a single line by default and expand on tap, reducing screen clutter on the phone.

## v0.3.5

### Changed

- Footer hotkey hints for occupied slots now show page navigation (arrow keys), connection info (x), and PSK toggle (p) in addition to existing hints.

## v0.3.4

### Added

- Manual agent spawn keybinding: press `n` in command mode on an empty slot to launch a new agent. The agent is automatically assigned the next available callsign from the configured list, skipping any callsigns already in use. The standby pane now hints about this keybinding, and the footer shows context-sensitive hints (`n:new` for empty slots, `k:kill` for occupied ones).

## v0.3.3

### Added

- Image sending from radio to agents: the phone app can now send images (from gallery or camera) targeted at a specific agent by callsign. The console saves images to `.dispatch/images/` and writes the file path to the agent's PTY so the agent can view it. New `send_image` WebSocket message type, image attach button in the radio UI, and agent picker dialog for targeting.

## v0.3.2

### Added

- Shared agent memory system: a persistent knowledge base at `.dispatch/MEMORY.md` in each target repo. The console creates the file on first dispatch and injects its contents into each agent's system prompt. Agents can append valuable learnings (build commands, gotchas, environment quirks) after completing work, benefiting all future agents in the same repo.

## v0.3.1

### Added

- Agent idle detection: the console monitors PTY output activity and detects when an agent stops producing output (10-second threshold). On each working-to-idle transition, the orchestrator receives an `[EVENT] AGENT_IDLE` event so it can take action (e.g. acknowledge completion, dispatch follow-up work).
- Activity status indicator in TUI pane info strip: shows "WORK" (green) when agent is actively producing output, "IDLE" (gray) when quiet.
- `list_agents` now reports "working" or "idle" status based on actual PTY output activity, replacing the previous task-presence-only heuristic.

## v0.3.0

### Added

- Configurable identity via `[identity]` section in `config.toml`: `user_callsign` (default "Dispatch") and `console_name` (default "Console"). These names appear in the radio chat log, orchestrator prompts, and agent prompts.
- Identity names are propagated to the radio app via the `agents` response, enabling correct display and color-coding.

### Changed

- Naming convention: "Dispatch" now refers to the human user (who speaks over the radio), "Console" refers to the orchestrator system that coordinates agents.
- Chat sender label changed from "Dispatcher" to the configured `console_name` (default "Console").
- Voice transcript sender label changed from "You" to the configured `user_callsign` (default "Dispatch").
- Orchestrator and agent prompt templates updated to use "Console" for the system and "Dispatch" for the user.

## v0.2.1

### Added

- `dispatch edit-config` command to open config.toml in VS Code.

## v0.2.0

### Added

- Configurable agent names via `[agents].callsigns` in `config.toml`. The list drives the slot count, page layout, and agent callsigns -- replacing the hardcoded NATO alphabet and `max_agents` setting.

### Changed

- Slot count is now determined by the length of the `callsigns` list (default 8) instead of a fixed 26.
- Page count adapts automatically to fit the configured number of agents.
- The orchestrator system prompt now includes the configured callsign list so the LLM knows which agent names are valid.
- `terminal.max_agents` is deprecated; existing configs without `[agents]` fall back to NATO names using `max_agents` for backward compatibility.

### Removed

- Hardcoded 26-slot NATO alphabet as the sole agent naming scheme.
