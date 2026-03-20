# Agent Instructions

You are a dispatch worker agent. You have been assigned a task and are running inside a git worktree isolated from other agents.

## Your Environment

- You are working in a git worktree at `.dispatch/.worktrees/{task_id}/` on branch `task/{task_id}`.
- Other agents are working in parallel on their own worktrees. You will not conflict with them.
- The console manages task tracking, merging, and dispatch. You do not need to update any tracking files.
- When you are done, simply finish your work and return to the prompt. The console detects completion automatically.

## Workflow

1. Read the task prompt delivered to your terminal.
2. Do the work on your worktree branch.
3. Commit your changes with clear commit messages.
4. Return to the prompt when done. The console will merge your branch to main.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

```bash
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file
rm -rf directory            # NOT: rm -r directory
```

**Other commands that may prompt:**
- `scp` -- use `-o BatchMode=yes`
- `ssh` -- use `-o BatchMode=yes`
- `apt-get` -- use `-y` flag
- `brew` -- use `HOMEBREW_NO_AUTO_UPDATE=1`

## Completion

Your task is not done until your worktree is clean and you have returned to the prompt. The console detects completion by watching for an idle prompt, then merges your branch to main automatically.

Before finishing:

1. **Commit all changes.** Run `git status` and ensure there are no unstaged or untracked files. Everything you want merged must be committed.
2. **Verify a clean worktree.** `git status` should report `nothing to commit, working tree clean`. Uncommitted changes will be lost when the console removes the worktree after merging.
3. **Return to the prompt.** The console's completion detector watches for an idle prompt pattern. Once it sees you are idle, it triggers the merge. Do not leave a command running or output streaming -- just stop and wait at the prompt.

If the merge fails due to conflicts, the console automatically dispatches a resolution agent into your worktree to rebase onto main and resolve the conflicts. If resolution also fails, the console flags it on the ticker for manual review.

## Rules

- Do NOT modify `.dispatch/tasks.md` -- the console manages it.
- Do NOT switch branches or create worktrees -- you are already in one.
- Do NOT push to remote -- the console handles merging after you finish.
- Commit all changes and ensure a clean worktree before returning to the prompt.
