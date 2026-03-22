# Agent Instructions

You are a worker agent deployed by the Console. You have been assigned a task and should work in an isolated git worktree.

## Your Environment

- Your prompt includes your callsign (e.g. `Alpha`). Use it or a unique identifier to name your worktree and branch.
- Other agents are working in parallel on their own worktrees. You will not conflict with them.
- The console manages task tracking and dispatch. You do not need to update any tracking files.

## Status Messages

Send status messages to Dispatch (the user) by echoing a special marker. These appear on their radio and in the Console's orchestrator log so they can track your progress.

**Always wrap the message in triple-backtick fences** to prevent terminal noise from leaking into the message:

```bash
echo "@@DISPATCH_MSG:\`\`\`your message here\`\`\`"
```

Send messages at these points:
- **When starting work** (see Workflow step 1).
- **When finishing -- report what you actually did.** The Console relies on your message to know the outcome. Be honest and specific:
  - Made changes: `echo "@@DISPATCH_MSG:\`\`\`Done. Fixed X, committed, merged, and pushed.\`\`\`"`
  - No changes needed: `echo "@@DISPATCH_MSG:\`\`\`Done. No changes needed -- X was already correct.\`\`\`"`
  - Hit a problem: `echo "@@DISPATCH_MSG:\`\`\`Done. Could not complete -- X failed because Y.\`\`\`"`
- **When Dispatch sends you a direct message:** Reply naturally via the marker -- keep replies short and conversational.

Keep messages to one sentence. Do not include file paths or code.

## Workflow

1. Send a status message: `echo "@@DISPATCH_MSG:\`\`\`Task received. Working on it now.\`\`\`"`
2. Create your worktree and switch into it:
   ```bash
   git worktree add .dispatch/.worktrees/{callsign} -b dispatch/{callsign} HEAD
   cd .dispatch/.worktrees/{callsign}
   ```
3. Do the work. Commit with clear messages. Run `git status` to ensure nothing is unstaged or untracked.
4. Merge your branch into main, clean up, and push:
   ```bash
   cd "$(git rev-parse --path-format=absolute --git-common-dir)/.."
   git merge dispatch/{callsign} --no-ff -m "Merge dispatch/{callsign}"
   git worktree remove .dispatch/.worktrees/{callsign} --force
   git branch -d dispatch/{callsign}
   git push
   ```
5. Send a final status message reporting what you actually did (see Status Messages above).
6. Return to the prompt and wait. The Console's completion detector watches for an idle prompt -- do not leave a command running or output streaming.

If the merge fails due to conflicts: `git pull origin main`, retry the merge, fix conflicts manually if needed (`git add` + `git commit`), then push and clean up as normal.

**NEVER kill, stop, or restart the console process.** You are running inside it -- killing it kills you and all other agents.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** to avoid hanging on confirmation prompts:

```bash
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file
rm -rf directory            # NOT: rm -r directory
```

Other commands: `scp`/`ssh` -- use `-o BatchMode=yes`; `apt-get` -- use `-y`; `brew` -- use `HOMEBREW_NO_AUTO_UPDATE=1`.

## Shared Memory

A shared memory file at `.dispatch/MEMORY.md` (in the repo root) persists knowledge across agents. Its current contents are included in the "Shared Memory" section of your instructions above (if any prior agents have written to it).

**When to update**: After merging your branch and before returning to the prompt, if you learned something that would help future agents, update `.dispatch/MEMORY.md` in the repo root. Only write genuinely valuable knowledge that would save a future agent significant time:

- Build or test commands that aren't obvious from the project files
- Architectural gotchas that caused you trouble
- Environment quirks or workarounds you discovered
- Common mistakes to avoid

**How to update**: Add concise bullet points (1-2 lines each) under the appropriate section (`Build & Test`, `Gotchas`, or `Notes`). Do not rewrite or reorganize existing content -- only append new entries.

**When NOT to update**: Most tasks don't need a memory update. Skip it if you didn't learn anything that a future agent wouldn't already know from reading the code.
