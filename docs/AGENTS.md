# Agent Instructions

You are a worker agent deployed by the Console. You have been assigned a task and should work in an isolated git worktree.

## Your Environment

- Your prompt includes your callsign (e.g. `Alpha`). Use it or a unique identifier to name your worktree and branch.
- Other agents are working in parallel on their own worktrees. You will not conflict with them.
- The console manages task tracking and dispatch. You do not need to update any tracking files.

## Status Messages

Send status messages to Dispatch (the user) by echoing a special marker. These appear on their radio and in the Console's orchestrator log so they can track your progress.

```bash
echo "@@DISPATCH_MSG:your message here"
```

Send messages at these points:
- **When starting work:** `echo "@@DISPATCH_MSG:Task received. Working on it now."`
- **When finishing -- report what you actually did.** The Console relies on your message to know the outcome. Be honest and specific:
  - If you made changes: `echo "@@DISPATCH_MSG:Done. Fixed X, committed, merged, and pushed."`
  - If no changes were needed: `echo "@@DISPATCH_MSG:Done. No changes needed -- X was already correct."`
  - If you hit a problem: `echo "@@DISPATCH_MSG:Done. Could not complete -- X failed because Y."`
- **When Dispatch sends you a direct message:** Reply naturally via the marker, e.g. `echo "@@DISPATCH_MSG:Copy. Standing by if you need anything."` -- keep replies short and conversational.

Keep messages short (one sentence). Do not include file paths or code in messages.

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
6. Send a final status message reporting what you actually did (see Status Messages above).
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
3. **Return to the prompt.** The Console's completion detector watches for an idle prompt pattern. Once it sees you are idle, it reports completion to the Console. Do not leave a command running or output streaming -- just stop and wait at the prompt.

If the merge fails due to conflicts, resolve them:
1. Pull the latest main: `git pull origin main`
2. Attempt the merge again. If conflicts remain, fix them manually, then `git add` the resolved files and `git commit`.
3. Push to remote and clean up as normal.

## Rules

- Always push to remote after merging into main.
- Create your own worktree at the start and clean it up at the end.
- Commit all changes before merging.
- NEVER kill, stop, or restart the console process. You are running inside it — killing it kills you and all other agents.
