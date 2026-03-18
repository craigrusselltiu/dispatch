# Console Task Management

Reference for how the dispatch console manages tasks. This document describes the task format, planning flow, worktree lifecycle, and completion detection. See SPEC.md for the full system specification.

## Task Format

The console tracks tasks in `.dispatch/tasks.md` in the target repo. The format:

```markdown
# Plan title

- [ ] t1: Task description
  - [ ] t1.1: Subtask description
  - [ ] t1.2: Another subtask -> t1.1
- [ ] t2: Task that depends on t1 -> t1
```

**Status markers:** `[ ]` open, `[~]` in progress, `[x]` done.

**Dependencies:** `-> t1, t2` at the end of a line means "blocked by t1 and t2". No arrow means no blockers.

**Agent annotation:** when assigned, the console appends `| agent: Callsign`:

```
- [~] t1.1: Create auth module skeleton | agent: Alpha
```

Indentation groups subtasks under a parent for readability. The parent is considered done when all subtasks are done.

## Planning

When a voice prompt describes a complex task (e.g. "refactor the auth system"):

1. The console spawns a headless planner agent (no pane, no slot consumed).
2. The planner analyzes the codebase and writes a task breakdown to `.dispatch/tasks.md` with IDs, descriptions, and dependency arrows.
3. The planner exits.
4. The console reads the plan and begins dispatching workers for unblocked tasks.

Simple one-off prompts (e.g. "Alpha, fix the login bug") skip planning and create a single task directly.

## Worktree Lifecycle

Each task runs in an isolated git worktree.

**On task assignment:**
```
git worktree add .dispatch/.worktrees/{task_id} -b task/{task_id}
```

The agent's PTY launches with its working directory set to the worktree path.

**On task completion:**
1. Merge the task branch to main.
2. If merge succeeds: clean up worktree, mark `[x]`.
3. If merge conflicts: flag on ticker, preserve worktree for manual review.

**On agent termination:**
Worktree and branch are preserved. Task reverts to `[ ]` for reassignment.

## Completion Detection

Two-layer strategy:

1. **Idle prompt detection (primary)** -- watch the vt100 virtual screen for tool-specific idle patterns (e.g. `^> $` for claude-code). Confirmed after 500ms of no new output.
2. **Inactivity timeout (safety net)** -- if no idle pattern fires within a configurable timeout (default 60s), mark the task complete.

On completion, the console merges the worktree, marks the task `[x]`, scans for newly unblocked tasks, and dispatches the next one.

## Dispatch Flow

```
Orchestrator reads .dispatch/tasks.md
  -> finds [ ] tasks with no unresolved -> dependencies
  -> picks one, marks [~] with agent annotation
  -> creates worktree, launches agent PTY inside it
  -> writes task prompt to PTY
  -> detects completion
  -> merges worktree, marks [x]
  -> checks what's now unblocked
  -> dispatches next
```

## Configuration

Relevant config keys in `config.toml`:

```toml
[tasks]
dir = ".dispatch"           # Base directory for dispatch artifacts in the target repo
auto_dispatch = true        # Auto-dispatch agents for unaddressed prompts
default_tool = "claude-code" # Default tool for auto-dispatched and planner agents
completion_timeout_secs = 60 # Inactivity timeout (0 to disable)
auto_merge = true           # Auto-merge completed branches (false = leave for review)
```
