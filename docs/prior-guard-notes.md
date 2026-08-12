<!-- markdownlint-disable MD013 -->

# Notes on the previous guard

Passages lifted out of the doc comments in `action-rules/src/action_rules.rs`.
Each explained a rule by contrast with `global-git-guard.sh`, the bash
`PreToolUse` hook that the action-rules framework replaces. They are accurate,
but they only land for a reader who knows the old script, so they were removed
from the specification rather than deleted.

The script itself is still at `~/.claude/global-git-guard.sh` (uninstalled), with
its test suite at `~/.claude/guard_test.sh`, and the analysis that led to its
replacement is in `docs/archived/GUARD-REWRITE-PLAN-20260810.md`.

## On `GitAction::Read` — why an allowlist rather than a denylist

The rule: read-only git invocations are determined by an **allowlist** of
subcommands, and anything not on the list is treated as mutating.

The removed rationale: the inverse arrangement — a denylist of *writing*
subcommands — is what lets `git add`, `git fetch` and `git submodule update`
pass unexamined in the bash guard. That script classifies a git subcommand as
mutating only if it appears in an explicit list, so any subcommand nobody thought
to add is silently treated as a read. An allowlist inverts the failure: a
subcommand nobody thought to add is treated as a write, and the fix when that
bites is one line.

## On `Target::is_in_foreign_repo` — why foreign repositories are protected

The rule: a target under a repository that is neither the launch project nor
`$CLAUDE_CONFIG_DIR` is protected, creation included.

The removed rationale: this is the case the previous `$PWD`-anchored guard failed
**open** on. That script derived its repository context from the hook process's
own working directory, so a command naming a path in a *different* checkout was
judged against the wrong repository — or against none. From a project on an
allowed branch, `git -C <other repo on main> commit`, `echo hi > <other>/f`,
`sed -i` and `rm` against that other repository were all permitted. Fixing the
lane boundary at session start, and treating every other repository as protected,
is what closes it.
