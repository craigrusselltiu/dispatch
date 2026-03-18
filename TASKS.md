# Tasks

Tasks are tracked in this file. The console reads and writes it directly.

## Format

```
- [ ] ID: Title
- [~] ID: Title  | agent: Callsign
- [x] ID: Title
```

Status: `[ ]` = open, `[~]` = in progress, `[x]` = done.

Append `| blocked-by: ID1, ID2` to mark a task as blocked. Blocked tasks are skipped during auto-dispatch.

## Breaking down large tasks

Indent subtasks under a parent to create a hierarchy:

```
- [ ] p1: Build auth system
  - [ ] p1.1: Add login form
  - [ ] p1.2: Add session handling  | blocked-by: p1.1
  - [ ] p1.3: Write auth tests      | blocked-by: p1.2
```

The parent is considered blocked until all subtasks are done.

## Agent workflow

The console manages this file automatically:

- **New prompt**: appends `- [ ] ID: prompt text` and assigns to an available agent
- **Agent claims task**: changes `[ ]` to `[~]` and appends `| agent: Callsign`
- **Agent finishes**: changes `[~]` to `[x]` and removes the agent annotation
- **Auto-dispatch**: scans for `[ ]` tasks with no unresolved `blocked-by` entries

Agents may also edit this file directly to add subtasks or link dependencies.
