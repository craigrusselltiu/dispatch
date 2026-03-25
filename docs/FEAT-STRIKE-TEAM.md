# Strike Team

A coordinated multi-agent execution mode that takes a spec or feature design document, breaks it into tasks with dependencies, then dispatches agents in parallel waves — maximizing throughput while respecting task ordering.

Named after the ICS (Incident Command System) term for a group of same-type units assembled for a specific mission.

## Overview

1. User provides a spec file (e.g., `docs/auth-spec.md`)
2. A **planner agent** reads the spec and creates a task file with dependencies
3. The console dispatches agents for all **ready** tasks (no unmet dependencies)
4. When an agent finishes, it merges to main and is terminated to free the slot
5. The console checks for newly unblocked tasks and dispatches the next wave
6. Repeats until all tasks are complete

## Task File Format

Location: `.dispatch/tasks-<name>.md`

```markdown
# Strike Team: auth-system
spec: docs/auth-spec.md

## T1: Implement user model
status: pending
dependencies: none
prompt: Create a User struct in src/models/user.rs.
  Fields: id (Uuid), email (String), name (String), created_at (DateTime).
  Derive serde Serialize/Deserialize and Debug.
  Add a User::new(email, name) constructor that generates the id and timestamp.

## T2: Add user API endpoints
status: pending
dependencies: T1
prompt: Create REST endpoints for CRUD operations on users in src/routes/users.rs.
  Add routes for GET /users, POST /users, GET /users/:id, DELETE /users/:id.
  Return JSON responses with proper HTTP status codes.

## T3: Add authentication middleware
status: pending
dependencies: T1
prompt: Implement JWT authentication middleware in src/middleware/auth.rs.

## T4: Wire auth into endpoints
status: pending
dependencies: T2, T3
prompt: Apply auth middleware to user endpoints.
  Protect POST, DELETE routes with JWT validation.
  Add integration tests covering authenticated and unauthenticated requests.
```

**Fields per task:**

| Field | Values | Description |
|-------|--------|-------------|
| `status` | `pending`, `active`, `done`, `failed` | Current state |
| `dependencies` | `none` or comma-separated IDs (`T1, T3`) | Dependency list |
| `prompt` | First line after `prompt:`, with 2-space indented continuation lines | Self-contained agent instruction (multi-line) |
| `agent` | Callsign (e.g., `Alpha`) | Written by console when assigned |

**Readiness rule:** a task is ready when its status is `pending` and all dependencies have status `done`.

**Parsing:** line-by-line string matching. `## T<N>:` starts a task, `key: value` lines set fields. Prompt continuation lines are indented with 2+ spaces. No markdown parsing library needed.

## Lifecycle

```
Idle --> Planning --> Executing --> Complete
                 \-> Aborted (planner error)
```

### Planning Phase

1. Orchestrator issues `strike_team(spec_file, name, repo)` tool call
2. Console dispatches a planner agent to the repo root (no worktree)
3. Planner reads the spec, creates `.dispatch/tasks-<name>.md`, reports task count via status message, then stops
4. Console detects planner idle/exit, parses the task file, transitions to Executing

### Execution Loop

Runs inside the existing 16ms main loop tick — no new threads or async.

1. `git pull --ff-only` in repo root (pick up prior merges from completed agents). On failure, log to the ticker and continue -- agents may work against stale code but execution is not halted.
2. Scan tasks: find all where status=`pending` and all deps are `done`
3. For each ready task with an available slot: dispatch a fresh agent with the task's prompt and a reference to the original spec file for context
4. Update task file: status=`active`, agent=`<callsign>`
5. When an agent goes idle (existing 10s idle detection):
   - Mark task `done` in the task file
   - Terminate the agent (free the slot for next wave)
   - Re-run from step 1
6. When an agent process exits unexpectedly: mark task `failed`, continue
7. When all tasks are `done` or `failed`: transition to Complete and notify the orchestrator via `[EVENT] STRIKE_TEAM_COMPLETE`

### Agent Lifecycle

Each task agent follows the normal dispatch workflow:
- Creates worktree from latest main (which includes all prior completed tasks)
- Works on its assigned task
- Merges to main, pushes, cleans up worktree
- Goes idle at the prompt

On idle detection, the console **terminates** the agent to free the slot. This ensures each subsequent agent starts fresh from the latest main with all prior merges.

## Orchestrator Tool

```json
{
  "name": "strike_team",
  "description": "Launch a Strike Team: break a spec into tasks with dependencies, then dispatch agents in parallel waves until all tasks are complete.",
  "input_schema": {
    "type": "object",
    "properties": {
      "spec_file": {
        "type": "string",
        "description": "Path to the spec/feature markdown file, relative to repo root."
      },
      "name": {
        "type": "string",
        "description": "Short name for this operation. Defaults to spec filename without extension."
      },
      "repo": {
        "type": "string",
        "description": "Repository name or path."
      }
    },
    "required": ["spec_file", "repo"]
  }
}
```

## Planner Agent Prompt

The planner is dispatched with a special prompt (overrides normal AGENTS.md worktree instructions):

```
You are a task planner for the Dispatch Strike Team system. Your ONLY job is to
read a spec file and create a task breakdown.

1. Read the spec file at: {spec_file}
2. Create a task file at: .dispatch/tasks-{name}.md

Use this EXACT format:

# Strike Team: {name}
spec: {spec_file}

## T1: <short title>
status: pending
dependencies: none
prompt: <first line of prompt>
  <continuation lines indented with 2 spaces>

## T2: <short title>
status: pending
dependencies: T1
prompt: <first line of prompt>
  <more detail on indented continuation lines>

EXAMPLE of a good prompt:

prompt: Create a User struct in src/models/user.rs.
  Fields: id (Uuid), email (String), name (String), created_at (DateTime).
  Derive serde Serialize/Deserialize and Debug.
  Add a User::new(email, name) constructor that generates the id and timestamp.
  Add unit tests for User::new in the same file.

RULES:
- Each task must be completable by a single agent in one session.
- Maximize parallelism: only add dependencies when truly required.
- Write detailed, multi-line prompts with specific file paths, function signatures, and acceptance criteria.
- Use 2-space indented continuation lines for multi-line prompts.
- Sequential IDs: T1, T2, T3, etc.
- Aim for 3-15 tasks.
- Do NOT create a git worktree. Work directly in the repo root.
- After creating the file, report the task count via your status message file, then stop.
```

## UI

Minimal additions:

- **Header bar**: `STRIKE TEAM 3/7` (done/total) appended when active
- **Pane info strip**: task ID next to callsign, e.g., `Alpha [T3]`
- **Ticker messages** at lifecycle events:
  - `STRIKE TEAM: planning <name>...`
  - `STRIKE TEAM: plan ready, 7 tasks`
  - `STRIKE TEAM: T3 -> Alpha`
  - `STRIKE TEAM: T3 done (Alpha)`
  - `STRIKE TEAM: complete (7/7)`

## Architecture

### New module: `console/core/src/strike_team.rs`

Pure logic — no PTY, TUI, or async dependencies. Contains:

- `TaskStatus` enum (`Pending`, `Active`, `Done`, `Failed`)
- `Task` struct (id, title, status, dependencies, prompt, agent)
- `StrikeTeamPhase` enum (`Planning`, `Executing`, `Complete`, `Aborted`)
- `StrikeTeamState` struct (name, spec_file, repo, phase, tasks, task_file_path)
- Parser: `parse_task_file(contents: &str) -> Vec<Task>`
- Writer: `write_task_file(tasks: &[Task]) -> String`
- Readiness: `ready_tasks(tasks: &[Task]) -> Vec<&Task>`
- Queries: `task_for_agent(tasks: &[Task], callsign: &str) -> Option<&Task>`, `is_complete(tasks: &[Task]) -> bool`, `summary(tasks: &[Task]) -> String`
- Mutations: `assign_task(tasks: &mut [Task], ...)`, `complete_task(tasks: &mut [Task], ...)`, `fail_task(tasks: &mut [Task], ...)`

### Changes to existing files

**`console/core/src/lib.rs`** — `pub mod strike_team;`

**`console/core/src/tools.rs`** — Add `StrikeTeam` variant to `ToolCall` enum and `StrikeTeamAcknowledged { name, spec_file, repo }` variant to `ToolResult` enum. Add tool definition JSON and parser arm.

**`console/src/types.rs`** — Add `strike_team: Option<StrikeTeamState>` to `App`.

**`console/src/app.rs`** — New methods:
- `execute_tool` arm for `StrikeTeam` (dispatch planner, init state)
- `tick_strike_team()` (advance state machine each frame)
- `strike_team_dispatch_ready()` (git pull, find ready tasks, dispatch agents)
- `strike_team_on_agent_idle(callsign)` (mark done, terminate, dispatch next wave)
- `strike_team_on_agent_exit(slot_idx)` (mark failed)

**`console/src/main.rs`** — Three hook points in the main loop:
1. After idle detection (~line 361): call `app.tick_strike_team()`
2. In AGENT_IDLE block (~line 324): call `app.strike_team_on_agent_idle(&callsign)`
3. In child_exited block (~line 279): call `app.strike_team_on_agent_exit(i)`

**`console/src/ui.rs`** — Header progress indicator, pane task ID label.

**`docs/ORCHESTRATOR.md`** — Add `strike_team` to action table.

**`docs/SPEC.md`** — Add Strike Team section.

## Edge Cases

- **Max slots full**: ready tasks wait. As agents finish and slots free up, next wave dispatches.
- **Agent failure**: task marked `failed`. Its dependents stay `pending` forever (blocked). Siblings continue normally.
- **Merge conflicts**: agents already handle conflicts per AGENTS.md. If unresolvable, agent reports failure.
- **Cancellation**: press `s` in command mode to abort the active strike team. This transitions the strike team to the Aborted phase, which stops all future task dispatching. Active agents are not killed — they finish their current work normally but no new tasks are dispatched.
- **One at a time**: only one strike team active at once. Second `strike_team` call returns an error.

## Implementation Sequence

1. Core types + parser in `strike_team.rs` with unit tests
2. Tool definition in `tools.rs`
3. App integration in `app.rs` and `types.rs`
4. Main loop hooks in `main.rs`
5. UI in `ui.rs`
6. Docs updates

## Implementation Review

Post-implementation comparison of this design doc against the code as committed. Organized by severity.

### ~~Critical: Planner idle detection race~~ (resolved)

~~The Planning phase detects planner completion by scanning slots for a `task_id` matching `"strike-team-planner:<name>"`. However, the main loop clears `task_id` (idle case) or removes the slot entirely (exit case) before `tick_strike_team()` runs, so the planner can never be found.~~

**Fixed:** Added `planner_callsign: Option<String>` to `StrikeTeamState`. The callsign is stored at planner dispatch time and used directly by `tick_strike_team()` to detect idle/exit, bypassing the task_id scan entirely. The field is cleared when the Planning phase transitions out.

### Moderate: Design drift — RESOLVED

**Function signatures — methods vs free functions.** The Architecture section describes `write_task_file(&self)`, `ready_tasks(&self)`, `task_for_agent()`, `summary()` as if they are methods on `StrikeTeamState`. The implementation uses free functions taking `&[Task]` slices (e.g., `write_task_file(tasks: &[Task])`, `summary(tasks: &[Task])`). This is better design — pure functions on slices are more composable and testable — but the doc should be updated to match. **Fixed:** Architecture section updated to show actual free function signatures with `&[Task]` parameters.

**ToolResult variant name.** The original app integration code referenced a `ToolResult::StrikeTeamStarted` variant with fields `{ name, planner_slot, planner_callsign }`. The actual enum defines `StrikeTeamAcknowledged` with fields `{ name, spec_file, repo }`. The variant name and fields were mismatched at the call site. **Fixed:** Architecture section updated to explicitly name the `StrikeTeamAcknowledged { name, spec_file, repo }` variant.

**No cancellation mechanism.** The Edge Cases section says "console stops dispatching if strike team state is cleared" but no code path clears the state while a strike team is active. The only terminal transitions are Complete (all tasks done/failed) and Aborted (planner error). A user who manually terminates agents will find the strike team keeps dispatching new ones for ready tasks. **Fixed:** Added `abort_strike_team()` method to `App` and wired it to the `s` keybinding in command mode. Pressing `s` transitions the strike team to Aborted phase, stopping all future dispatching. Active agents finish normally. Edge Cases sections in FEAT-STRIKE-TEAM.md and SPEC.md updated, keybinding added to SPEC.md, help overlay, and main loop.

### Minor

**~~Lifecycle diagram vs enum naming.~~** Resolved -- updated lifecycle diagrams in both FEAT-STRIKE-TEAM.md and SPEC.md to say `Aborted` instead of `Failed`, matching the `StrikeTeamPhase` enum.

**~~Planner prompt punctuation.~~** Resolved -- updated the design doc planner prompt template to use double hyphens (`--`) matching the code.

**~~Git pull errors silently ignored.~~** Resolved -- `strike_team_dispatch_ready()` now checks the git pull result and logs failures to the ticker. Execution continues (agents may work against stale code) but the error is visible. Documented in the execution loop description.

**~~Orchestrator completion event undocumented.~~** Resolved -- documented `[EVENT] STRIKE_TEAM_COMPLETE` in SPEC.md (execution loop step 7), ORCHESTRATOR.md (message format section), and the design doc execution loop.
