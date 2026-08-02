## Interaction
- When I choose the clarify/chat option on an `AskUserQuestion` prompt, output nothing. End the turn silently and wait for me. Do not ask what I want to clarify, do not restate the open questions, do not summarise where we are. I chose that option because I have something to say, not because I want to be prompted.

## Subagents
- Use subagents (Explore, Plan, general-purpose) whenever you judge them useful. Do not ask first. In plan mode, follow the workflow's own guidance on Explore and Plan agents rather than any default that reserves them for explicit requests.
- Keep exercising judgement: read directly when the search space is small or already in context, since a cold agent re-derives what is already known.

## Critical Git rules
- The allowed branches are `aicode` and the branches listed in `.claude/allowed-branches` at the repo root (one per
  line). If that file is absent, the only allowed branch is `aicode`.
- Before ANY action that touches a repo — creating, editing, or deleting a file in the working tree, or running a state-changing Git command (commit, merge, rebase, push, checkout, switch, reset, stash, ...) — you MUST run `git branch --show-current` and confirm the result is in the allowed list. This applies to file writes, not just Git commands: creating, modifying, or deleting a file on a disallowed branch is itself a violation.
- If the current branch is not allowed, STOP immediately and ask me to switch. Do not perform the write "just this once", and do not treat a scratch or throwaway file as exempt.
- File METADATA changes (`chmod`, `chown`, `chgrp`, `ln`, `touch`) are a separate case: ask me before running one, on any branch and on any file. Do not reason about whether Git would show the change — Git records the executable bit but not ownership, so a `chown` can make a file unreadable to me without ever appearing in `git status`. The test is whether the working tree is affected, not whether Git notices.
- NEVER attempt to switch branches, create new branches, or checkout other branches yourself.
- When on an allowed branch, you are free to commit as often as you deem necessary.
