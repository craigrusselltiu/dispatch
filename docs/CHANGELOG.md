# Changelog

All notable changes to this project will be documented in this file.

## v0.4.8

### Added

- **Search and rescue**: when a `strike_team` call is made and a task file already exists, the console skips planning and enters rescue mode — resetting `failed` and `working` tasks to `ready` for retry. If a stalled task's worktree still exists on disk, the agent is redispatched into the existing worktree with a resume prompt.
- Verifier now deletes the task file (`.dispatch/tasks-<name>.md`) after completing verification.

### Changed

- Strike team task statuses renamed: `pending`→`ready`, `active`→`working`, `done`→`success`. The parser accepts both old and new names. Planner prompt template and SPEC updated to match.

### Fixed

- Strike team planner task file detection now retries for up to 5 seconds after the planner goes idle and scans `.dispatch/` for alternative filenames, instead of aborting immediately when the file isn't at the exact expected path. Fixes #36.
- Strike team idle detection now checks agent PTY output for usage-limit indicators before marking a task as `success`. If a usage limit is detected, the task stays as `working`, which prevents the team from proceeding to verification on incomplete work. Fixes #35.
- Replaced unlinked `log::warn!` calls with `eprintln!` in strike team git-pull error handling.

## v0.4.7

### Fixed

- `.dispatch/` directory is now created eagerly at startup for each configured repo, instead of lazily on first agent dispatch. Includes `messages/`, `images/`, `MEMORY.md`, and `.gitignore`.
- Restructured orchestrator instructions with "Ground Rules" section at the top: do not use tools (blocks message reception), strike team requests should be issued immediately without reading the document first. Removes duplicate investigation rules.

## v0.4.6

### Fixed

- Protocol messages to the orchestrator now use per-session nonce prefixes (`[D-{nonce}:MIC]`, `[D-{nonce}:EVENT]`, `[D-{nonce}:AGENT_MSG]`) instead of plain `[MIC]`, `[EVENT]`, `[AGENT_MSG]`. A random 4-character hex nonce is generated each time the orchestrator spawns, making it difficult for the LLM to hallucinate valid protocol messages that could poison its own context.

## v0.4.5

### Fixed

- Ticker now shows "PR CREATED" instead of "MERGED" when merge_strategy is set to "pr". The orchestrator log event type also changes from "MERGE" to "PR" accordingly.

### Added

- The console header title now displays the initialization path (e.g. "DISPATCH -- myrepo"). Long paths are shortened to the last directory component.

## v0.4.4

### Changed

- Strike team planner now accepts any document type (specs, performance reviews, design docs, TODO lists, etc.), not just formal coding specs. The planner prompt was generalized to read any document and intelligently extract actionable tasks from its contents.

## v0.4.3

### Changed

- Strike team task prompts now support multi-line values. Lines indented with 2+ spaces after `prompt:` are parsed as continuation lines, allowing the planner to write detailed prompts with file paths, function signatures, and acceptance criteria.
- Strike team task agents now receive the spec file path in their prompt so they can read the original spec for full context about the overall project.
- Updated planner agent prompt to include a concrete example of a good multi-line prompt and explicit guidance on using indented continuation lines.

## v0.4.2

### Added

- Radio connection profiles: save and switch between named connection profiles (host, port, PSK) in the radio app settings. Each profile stores the connection details for a different console. Profiles persist across app restarts. The active profile name is shown in settings and clears automatically if connection fields are manually changed.

## v0.4.1

### Removed

- Console channels feature (`dispatch channel save/load/list/delete/show`).

## v0.4.0

### Added

- GitHub Copilot CLI support as an alternative AI agent alongside Claude Code. The orchestrator can dispatch agents with `"tool": "copilot"` to use Copilot instead of Claude. Copilot agents launch in YOLO mode (`--yolo` flag) for fully autonomous operation without permission prompts.
- The `dispatch` action now accepts an optional `tool` parameter (`"claude"` or `"copilot"`) to select which AI agent to use per dispatch. Defaults to the configured default tool.

### Changed

- Updated default Copilot tool command from `gh copilot suggest` (deprecated) to `copilot` (GitHub Copilot CLI).

## v0.3.9

### Changed

- Agent messaging overhauled from terminal-output-parsing to file-based system. Agents now write messages to `.dispatch/messages/{callsign}` files via `$DISPATCH_MSG_FILE` env var instead of echoing `@@DISPATCH_MSG:` markers to the PTY. Eliminates all ANSI noise, ConPTY artifacts, and TUI redraw issues.
- Removed system merge messages. Agents self-report merge status in their own messages instead of the console generating a separate "X has merged to remote" notification.
- Removed ~250 lines of terminal noise filtering code (ANSI stripping, backtick fence extraction, echo detection, deduplication, line-ending handling).

## v0.3.8

### Fixed

- Collapsible system messages in the radio app now work with multi-line dispatch prompts. The previous regex-based matching failed when prompts contained newlines; replaced with startsWith/contains checks.
- Volume Up agent status overlay no longer flickers when held. The dialog now uses FLAG_NOT_FOCUSABLE so it doesn't steal input focus from the activity, allowing key-up events to properly dismiss it.

### Changed

- User (Dispatch) messages in the radio chat that are multi-line or longer than 80 characters are now also collapsible, matching the system message behavior.
- Agent status overlay font changed to monospace bold, consistent with the rest of the radio app.
- Volume Up long press (>1s) no longer transitions to quick dispatch picker. The status overlay now stays visible for the entire hold duration.

## v0.3.7

### Added

- Orchestrator interrupt: press `c` in the console or tap the stop button on the radio to cancel the orchestrator's current response and restart it.

## v0.3.6

### Changed

- Prompt system messages in the radio app are now collapsible. Dispatch and send-to-agent messages show a single line by default and expand on tap, reducing screen clutter on the phone.

## v0.3.5

### Changed

- Footer hotkey hints for occupied slots now show page navigation (arrow keys), connection info (x), and PSK toggle (p) in addition to existing hints.

### Added

- Volume Up hold-to-view agent status overlay on radio app. Pressing Volume Up immediately shows a dialog listing all active agents sorted by dispatch time, with callsign on the left and status on the right (Busy in red, Idle in yellow). Releasing the button dismisses the overlay. Long press (>1s) still transitions to the quick dispatch picker.

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
