# Action Rules: implementing the specification

## Context

The rule specification landed at `0f8666f`: `action-rules/src/action_rules.rs`
defines the vocabulary, the two policy surfaces, and the composition, rendered
to Rustdoc and to `~/.claude/docs/action-rules.md`. Every rule body is `todo!()`.

This change implements it. Fifteen stubs remain — fourteen predicates plus
`Command::parse` — but three things the spec does not mention are equally
missing, and they are the larger part of the work:

- **Nothing can construct a `Target` or `Repo`.** Both have private fields and no
  constructors, so there is no path from a string to a judgeable target.
- **There is no hook.** Nothing reads the `PreToolUse` payload from stdin or
  emits a decision. `check_command(&str)` is the only entry point, and the file
  tools deliver a `file_path` rather than a command.
- **There are no tests.** `guard_test.sh` is frozen, and most of its 118
  assertions now encode superseded rules.

The guard remains uninstalled throughout; `settings.json` has
`"PreToolUse": []`. Installation is the last step, deliberately.

## Decisions from the design session

| # | Decision |
|---|---|
| 1 | Split by **concern**: rule logic stays in `action_rules.rs`; facts move out |
| 2 | Launch project from `CLAUDE_PROJECT_DIR`, plus a stateless sanity check |
| 3 | A new `action-guard.sh` shim execs a new `action-guard` binary |
| 4 | Tests at two levels: Rust unit tests over fabricated facts, shell suite end-to-end |
| 5 | Every failure mode resolves to **Ask**, never Deny, never silence |
| 6 | **No trait** — a resolved `TargetFacts` value struct; rules are pure functions |
| 7 | Three-way tool split; matcher widens to `*` |
| 8 | Two-pass triage of the old suite |
| 9 | `SessionStart` runs `cargo build --release`; the shim only checks existence |
| 10 | `TargetFacts` carries **raw** facts; lanes are derived by the rules |
| 11 | Scanner at the archived plan's level, after surveying existing parsers |

## Architecture

```
action-rules/src/
  action_rules.rs   rule logic only. Pure functions over TargetFacts.
                    Cannot perform I/O — it has no means to.
  facts.rs          TargetFacts + the resolver. All git and filesystem access.
  parse.rs          the scanner: command string -> Action + Vec<GitAction>
  hook.rs           payload in, decision out. Tool dispatch.
  bin/action-guard.rs   the binary
  bin/gen-md.rs         unchanged
```

The property bought in step 1 is structural, not conventional: `action_rules`
never gains a subprocess or a filesystem call, because everything it needs
arrives as data.

## Work

### 1. Probe — throwaway, not committed

A temporary `PreToolUse` hook logging its environment and payload, run in one
session while deliberately `cd`-ing between repositories and `/tmp`, and while
dispatching a subagent. Establishes three things that are undocumented and
load-bearing:

- Whether `CLAUDE_PROJECT_DIR` is **stable across `cd`**, or tracks `cwd`. If it
  drifts, fall back to a `session_id`-keyed state file (design-session Q2c).
- What an **`Ask` does inside a subagent**, where there may be nobody to prompt.
- Whether the decision vocabulary is **`ask` or `escalate`**. Research reported
  `allow|deny|escalate`; `global-git-guard.sh` emits `ask` and all 118 assertions
  are written against `ask`. These cannot both be current, and the answer
  determines the binary's entire output contract.

Needs explicit go-ahead: it means installing a hook while `PreToolUse` is
deliberately empty. Removed immediately afterwards.

### 2. `facts.rs` and the fourteen predicates

`TargetFacts` carries raw facts only — `path`, `exists`, `is_dir`, `repo` (root
and branch), `is_repo_root`, `tracked`, and `ignored` as
`No | ContentsRecursivelyIgnored | FilePattern`. Lane membership and everything
above it is derived in `action_rules.rs`.

Two resolver mechanics settle the cost:

- A directory's contents are all ignored iff `git ls-files -- <dir>` is empty
  **and** `git ls-files --others --exclude-standard -- <dir>` is empty.
- A file is `FilePattern`-ignored iff `git check-ignore` matches it but no
  ancestor directory passes the test above.

Both compose from two commands per repository, batched across every target in
one invocation and memoised. Every git subprocess runs with a timeout, `stdin`
closed, `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`, and
`GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` stripped.

`Repo::allowed_branches` is the **union** of `"aicode"` and
`<root>/.claude/allowed-branches`. Detached `HEAD` is not allowed.

Unit tests construct `TargetFacts` literals directly — no temporary
repositories, no subprocesses, no mocking.

### 3. Triage of `guard_test.sh` — pass 1, for review

All 118 assertions in a table, each marked:

- **unchanged** — the new spec agrees with the old expectation.
- **changed-by-design** — the new spec disagrees, and a named decision from the
  design session explains it. E.g. `echo hi > ~/x` was allowed under the old
  "outside a repository passes" rule; loose paths are now protected.
- **obsolete** — the rule no longer exists, such as the `~/.claude` carve-out now
  subsumed by the gitignore predicate.

Reviewed before any test is written. By the time anything runs, every legitimate
failure is predicted, so an unpredicted one is unambiguously a defect.

### 4. `parse.rs` — the scanner

Survey maintained Rust shell parsers first. The archived plan's rejection of
`shlex` was about a *lexer* — it truncates `--format=%h#x` at the `#` and cannot
distinguish `'$(date)'` from `"$(date)"` — and does not extend to a real POSIX
parser. If nothing suitable is maintained, hand-roll at the archived plan's
level: longest-match operators including `&&`, `>>`, `<<`, `<>`, `2>`, `&>`;
`'…'` inert versus `"…"` live; heredoc bodies discarded; `#` ordinary mid-word;
one literal `cd` rebasing relative paths with `(` as a barrier.

Globs are **expanded against the filesystem** at decision time rather than
treated as opaque. `Opaque` is reserved for `$VAR`, `$(…)`, backticks, and syntax
the scanner does not model.

### 5. `hook.rs`, the binary, the shim, and `build.sh` — built, not installed

Tool dispatch is three-way:

- **Known writers** — `Bash`, `Write`, `Edit`, `MultiEdit`, `NotebookEdit` — get
  the rules. File paths go through `Action::Write` with a single
  `TargetedEffect`, so both entry points share one rule table.
- **Known readers** — `Read`, `Grep`, `Glob`, `WebSearch`, `WebFetch`,
  `TodoWrite`, and the rest — allowed by name, **returning before any I/O**.
  Since the matcher widens to `*`, this path runs on every tool call and must
  stay sub-millisecond.
- **Everything else, including all `mcp__*`** — Ask.

The allowlist lives in the binary, not in the `matcher` regex, so the triage
table can reach it.

Failure policy: every malformed payload, panic, git timeout or unreadable
`allowed-branches` resolves to **Ask** carrying the reason. The decision JSON is
built completely in memory and written with a single call — a partial write
followed by a panic message is malformed JSON, which Claude Code treats as a
non-blocking error and proceeds, i.e. fail-open through the back door.

`build.sh` runs `cargo build --release`. A `SessionStart` hook runs it too:
cargo's own fingerprinting is the staleness check, costing tens of milliseconds
when nothing changed. The shim keeps only an existence check — binary missing or
not executable emits Ask with the reason, rather than exiting 127, which would
read as no-opinion. Known gap, accepted: a source edit mid-session leaves the old
binary in use until the next session or a manual build.

### 6. The new suite, then installation

The suite is written **from the spec**, using the triage table as a coverage
checklist — not by editing the old assertions, which is how a superseded
expectation survives by inertia. Pass 2 of the triage runs here: any failure not
already predicted is *newly wrong*, and is a defect to investigate rather than an
expectation to edit.

Then `settings.json` gains the `PreToolUse` entry, and `global-git-guard.sh` and
`guard_test.sh` are deleted.

## Verification

1. `cargo check`, `cargo clippy --all-targets` clean, `cargo fmt --check`.
2. `cargo test` — unit tests over fabricated `TargetFacts`, covering every arm of
   both policy surfaces.
3. `./build-docs.sh` still succeeds and the generated Markdown is unchanged
   except where doc comments were deliberately edited.
4. The new shell suite green against the real binary, real git, real payloads.
5. Every triage row accounted for; no unexplained failures.
6. Only after 1–5: install into `settings.json`, then exercise deliberately —
   a write on an allowed branch, a write on `main`, a write to a foreign repo, a
   `git config --global`, an unknown MCP tool — confirming each verdict by hand.
