# Agent Instructions

You are a dispatch worker agent. You have been assigned a task and should work in an isolated git worktree.

## Your Environment

- Your prompt includes your callsign (e.g. `Alpha`). Use it or a unique identifier to name your worktree and branch.
- Other agents are working in parallel on their own worktrees. You will not conflict with them.
- The console manages task tracking and dispatch. You do not need to update any tracking files.

## Status Messages

Send status messages to the dispatch radio by echoing a special marker. These appear on the user's phone so they can track your progress remotely.

```bash
echo "@@DISPATCH_MSG:your message here"
```

Use these exact messages at the required points:
- **When starting work:** `echo "@@DISPATCH_MSG:Task received. Working on it now."`
- **Before merging:** `echo "@@DISPATCH_MSG:Task complete. Merging to main now."`
- **When the user sends you a direct message:** Reply naturally via the marker, e.g. `echo "@@DISPATCH_MSG:Copy. Standing by if you need anything."` -- keep replies short and conversational.

IMPORTANT: Only use these three cases. Do not send any other status messages. Do not include task details, file names, or technical information in the message -- keep it short and clean.

## Workflow

1. Read the task prompt delivered to your terminal.
2. Send a status message: `echo "@@DISPATCH_MSG:Task received. Working on it now."`
3. Create your worktree and switch into it:
   ```bash
   git worktree add .dispatch/.worktrees/{callsign} -b dispatch/{callsign} HEAD
   cd .dispatch/.worktrees/{callsign}
   ```
4. Do the work on your worktree branch.
5. Commit your changes with clear commit messages.
6. Send a status message: `echo "@@DISPATCH_MSG:Task complete. Merging to main now."`
7. Merge your branch into main, clean up, and push:
   ```bash
   cd "$(git rev-parse --path-format=absolute --git-common-dir)/.."
   git merge dispatch/{callsign} --no-ff -m "Merge dispatch/{callsign}"
   git worktree remove .dispatch/.worktrees/{callsign} --force
   git branch -d dispatch/{callsign}
   git push
   ```
8. Return to the prompt when done.

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

Your task is not done until you have merged your branch into main, cleaned up your worktree, and pushed to remote.

Before finishing:

1. **Commit all changes.** Run `git status` and ensure there are no unstaged or untracked files. Everything you want merged must be committed.
2. **Merge into main and push.** Navigate back to the repo root, merge your branch, remove the worktree, delete the branch, and push to remote (see workflow above).
3. **Return to the prompt.** The console's completion detector watches for an idle prompt pattern. Once it sees you are idle, it reports completion to the orchestrator. Do not leave a command running or output streaming -- just stop and wait at the prompt.

If the merge fails due to conflicts, stop and return to the prompt. The console flags the conflict for manual review.

## Rules

- Always push to remote after merging into main.
- Create your own worktree at the start and clean it up at the end.
- Commit all changes before merging.
