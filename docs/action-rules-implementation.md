<!-- markdownlint-disable MD013 -->

# Implementing the action rules

What was built against `Plan_action_rules_impl.md`, 2026-08-15. All six steps
are done and the guard is installed.

## What the probe settled (no restart needed)

Run as a nested `claude -p` session against a throwaway `--settings` file —
`settings.json` was never touched, and the fixtures are deleted. Findings in
[hook-probe-findings.md](hook-probe-findings.md):

- **`CLAUDE_PROJECT_DIR` is stable across `cd`.** Across three tool calls it
  stayed on the launch repository while the hook's `$PWD` *and* the payload's
  `cwd` both followed the shell into another repository. That is the old guard's
  defect in one table, and it confirms the division of labour:
  `CLAUDE_PROJECT_DIR` fixes the lane, payload `cwd` resolves relative paths
  only. No `session_id`-keyed state file needed.
- **An `Ask` with nobody to prompt fails closed** — a blocking error carrying
  the reason, not a silent allow. The reason string is sometimes the only thing
  anyone sees, so it names the doc.
- **The vocabulary is `allow | deny | ask | defer`** (established from the
  Claude Code binary, not from the probe). There is no `escalate`; the 118
  assertions written against `ask` were right.

## Three places the spec had to change

These are the ones worth review — the code and the generated Markdown both
reflect them.

1. **`GitAction::StateChange` and `Destructive` now carry their repository.**
   The spec routed git through the repository root as a file target, but
   `is_repo_root` is "always protected" — so nothing reached through it could
   ever be allowed, and making the root allowable in order to permit
   `git commit` would have made `rm -rf <root>` allowable with it. The git
   dimension carries the branch rule directly instead.
2. **Branch permission is scoped to *lane* repositories** (new `Repo::is_lane`).
   Without it a foreign checkout on `aicode` was both allowed and protected —
   not an edge case, the central one.
3. **`location_grants_write`** groups write sinks and scratch roots, and every
   protected predicate excludes it. Found by an exhaustive invariant test over
   fabricated facts, which is now a permanent test.

Also: `all_contents_ignored` requires the directory to exist and be non-empty,
or to be matched by `check-ignore`. Without that, `<repo>/newdir/newfile`
inherits the verdict of its own absent parent and is writable on any branch.

## The parser survey

`conch-parser` has been dead since 2019, `brush-parser` is maintained but pulls
fourteen crates, `yash-syntax` is maintained and lean but async-only. None of
them removes the enumeration work — whichever parser produces the tree, every
construct still has to be classified or declared opaque — and a borrowed parser
makes that harder, because it succeeds on syntax the classifier has never heard
of. Hand-rolled at the archived plan's level.

## Triage and tests

[guard-triage.md](guard-triage.md) holds all 118 rows: 94 unchanged, 17
changed-by-design, 7 obsolete, plus the two fixture facts that forced changes.
The old suite built its repositories under `/tmp`, which is now an allowed
exception, so it would have passed vacuously.

Pass 2: **214 assertions, one unpredicted failure**, and it was the prediction
that was wrong. `git --git-dir=<other>/.git commit` came back `ask`, not `deny`
— and `ask` is right, since the scanner does not follow `--git-dir` and so
cannot say which repository the operation lands in. The implementation had
reached that verdict by mislabelling the call *destructive*; the route was
fixed, not the verdict. Everything else matched the table.

Green: `cargo check`, `cargo clippy --all-targets`, `cargo fmt --check`, 23 unit
tests, 214 shell assertions, `build-docs.sh`. Reader-path latency is 0.1 ms per
call including process spawn.

## Installed

`PreToolUse` on `*` runs `action-guard.sh`; `SessionStart` runs `build.sh`.
Hooks are snapshotted at startup, so the installing session is unaffected and
the guard goes live in the next one.

Hand-exercised against the real project, every verdict as specified:

| Action | Verdict |
| --- | --- |
| tracked write on `aicode` | allow |
| new file on `aicode` | allow |
| `rm -rf` the project root | deny |
| write into `~/.claude` on `aicode` | allow |
| write a loose file (`~/.bashrc`) | deny |
| `git config --global` | deny |
| `git commit` on `aicode` | allow |
| `git reset --hard` on `aicode` | ask |
| `cargo build --release` | allow |
| `rm -rf target/` | allow |
| an unknown MCP tool | ask |
| `Read` | allow |
| `rm -rf "$SOMEVAR/x"` | ask |

`global-git-guard.sh` and `guard_test.sh` are deleted; they are committed at
`f2f1711`, so they are recoverable. Nothing was committed — the change sits in
`~/.claude`'s working tree.

## Two things that may want adjusting

- The reader-tool allowlist in `hook.rs` is a maintenance point. An unlisted
  tool asks; `Artifact`, `CronCreate` and `EnterWorktree` are deliberately not
  on it.
- Existing untracked files sitting directly in `~/.claude`'s top level will ask,
  since they are ignored by a pattern rather than by a wholly-ignored directory.

To back the guard out entirely, set `"PreToolUse": []` in `settings.json`.
