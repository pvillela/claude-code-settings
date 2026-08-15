<!-- markdownlint-disable MD013 -->

# What the probe established

Step 1 of the implementation plan, run as a nested `claude -p` session against a
throwaway `--settings` file rather than by installing anything: `settings.json`
was never touched and no restart was needed. The probe hook, its log and its
fixture repositories have been deleted.

Three things were undocumented and load-bearing. All three are now settled.

## `CLAUDE_PROJECT_DIR` is stable across `cd`

A session launched in `repo-a` ran `pwd`, then `cd …/repo-b && pwd`, then `pwd`
again. Across the three tool calls the hook saw:

| call | `CLAUDE_PROJECT_DIR` | hook process `$PWD` | payload `cwd` |
| --- | --- | --- | --- |
| 1 | `…/repo-a` | `…/repo-a` | `…/repo-a` |
| 2 | `…/repo-a` | `…/repo-a` | `…/repo-a` |
| 3 | `…/repo-a` | **`…/repo-b`** | **`…/repo-b`** |

`CLAUDE_PROJECT_DIR` did not move. Both of the other two did, which is the
previous guard's defect reproduced in one table: a guard anchored on either of
them judges a command against whichever repository the shell has wandered into.

So decision 2 stands as written, and the `session_id`-keyed state file of Q2c is
not needed. It also confirms the division of labour in the implementation:
`CLAUDE_PROJECT_DIR` fixes the lane, and the payload's `cwd` is used for one
thing only — resolving relative paths, which is exactly what it is right about.

## An `Ask` with nobody to prompt fails closed

In a non-interactive session an `ask` decision does not degrade to an allow and
does not hang. The tool call is refused with a blocking error whose entire
content is `permissionDecisionReason`, and the model is told only that.

Two consequences, both of which the implementation already honours:

- The failure direction is safe. `Ask` is a sound default for every failure
  mode, including where there is no user in the loop.
- The reason string is sometimes the *only* thing anybody sees, so it has to
  stand alone and name the fix.

The subagent half of the question was not observed directly: the probe's
sentinel appeared in the `Agent` tool's own payload, so the launch itself was
intercepted and no subagent ever ran. What the run does show is that the
no-prompt path refuses rather than proceeds, which is the property that
mattered.

## The decision vocabulary is `allow | deny | ask | defer`

Established from the Claude Code binary rather than from the probe. There is no
`escalate`; the research that reported one was wrong, and the 118 assertions
written against `ask` were right. `defer` is print-mode-only and solo-only —
the binary logs and ignores it in an interactive session, and ignores it when
other tool calls are in the same batch — so the guard has no use for it.
