# Restore `gitignored_allowed_dir` semantics in the action guard

## Context

The original Rust spec (`~/.claude` commit `98547a7`) had a predicate
`Target::is_under_full_folder_gitignore` — "a folder that is entirely gitignored
or is under such a folder". The rewrite renamed it to `all_contents_ignored` and
changed its meaning: instead of asking whether the **directory** is gitignored,
it now infers the answer from the directory's **contents** (nothing tracked and
nothing untracked-and-unignored under it).

That inference is wrong. A `data/` directory holding a `.gitignore` with only
`*` in it is judged a build directory, so writes to `data/*.xlsx` are allowed
outright — exactly the irreplaceable-data case the guard exists to protect.

Four changes, per the decisions taken:

1. A directory grants writes only when git itself reports the **directory** as
   ignored — that is, when a `.gitignore` **above** it ignores it. Git never
   applies `D/.gitignore` to `D`, so this is precisely `git check-ignore D`.
   `~/.claude/action-rules/target/` (ignored by `action-rules/.gitignore`)
   qualifies; `data/` with an inner `*` does not.
2. Every other gitignored file moves from the Ask gap to **protected**. Scoped
   to files that **exist** — clobbering is the irreversible act; creating a new
   ignored file stays allowed on an allowed branch.
3. A **directory inherits that protection** from an irreplaceable file at any
   depth, so `rm -rf data` is judged by the same standard as `rm
   data/notes.csv`. Tracked content takes precedence, which is the one place an
   irreplaceable file goes unprotected — `rm -rf src` still works on an allowed
   branch with a `src/.env` inside it. Protecting that case too would deny
   `rm -rf` for nearly every directory, since stray `*.log` files are common.
4. An **existing untracked, non-gitignored file becomes allowed** on an allowed
   branch. Almost every such file was created by the session itself — the first
   write to a new path is allowed, and the second finds it existing — so asking
   about them was noise, not protection. The risk this accepts is a file the
   *user* wrote and has not committed; a `SessionStart` hook lists those once,
   at the start, so the choice is made knowingly.

## Prerequisites

- `~/.claude` is on branch `main`, which is not an allowed branch, so the guard
  currently denies writes to its tracked files. Switch to `aicode` (or add
  `main` to `~/.claude/.claude/allowed-branches`) before implementing.
- Step 1 below must land **before** the rebuilt binary, or the session loses the
  ability to write plan and memory files under `~/.claude`.

## Step 1 — `~/.claude/.gitignore`

Every machine-written directory there is currently matched by the negation
`!**/` (line 6), so none is a `gitignored_allowed_dir` and its files would
become protected. Append explicit folder entries after the re-include block —
last match wins, so `check-ignore` then reports them ignored:

```
# 4. Machine-written directories. Declared as ignored folders so the guard's
# `gitignored_allowed_dir` rule permits session writes to them.
backups/
cache/
downloads/
file-history/
ide/
paste-cache/
plans/
plugins/
projects/
session-env/
sessions/
shell-snapshots/
tasks/
```

Verify with `git check-ignore -v plans` — it must report the new line, not
`!**/`. Note the deliberate omissions: `output-styles/` and the root-level
ignored files (`settings.local.json`, `settings-bak.json`, `.credentials.json`)
stay protected, since they are authored, not reproducible.

## Step 2 — `action-rules/src/facts.rs` (the evidence layer)

**`Ignored` enum (`facts.rs:24-40`).** Rename `ContentsRecursivelyIgnored` to
`UnderGitignoredDir` and rewrite its doc: the path is a directory git reports as
ignored, or lies under one.

**Classification (`facts.rs:634-646`).** Replace the two-witness test with the
directory test alone, and let a directory inherit `FilePattern` from what it
holds:

```rust
let ignored = if tracked {
    Ignored::No
} else if ancestors_within(&rel)
    .iter()
    .any(|anc| listing.ignored.contains(anc))
    || (is_dir && !is_repo_root && listing.ignored.contains(&rel))
{
    Ignored::UnderGitignoredDir
} else if listing.ignored.contains(&rel)
    || (is_dir && self.holds_irreplaceable_file(listing, &rel))
{
    Ignored::FilePattern
} else {
    Ignored::No
};
```

The `if tracked` arm coming first is what implements the exception: a directory
holding tracked content never reaches the `FilePattern` arm.

`ancestors_within` (`facts.rs:748`) and the check-ignore batching
(`facts.rs:351-361`) already query every ancestor plus the target, so no new git
calls are needed. `ancestors_within` still excludes the repo root, for the
reason its doc gives.

**Delete `dir_contents_all_ignored`** and everything only it used: the
`--others --exclude-standard` invocation in `load_listing`, the `untracked`
field of `Listing`, and `is_empty_dir`. Keep the tracked `ls-files` call, which
`TargetFacts::tracked` still needs.

**Add `load_ignored_files` and `holds_irreplaceable_file`,** so that a directory
inherits `FilePattern` from what it holds:

- `git ls-files --others --ignored --exclude-standard -- <dir>` lists the
  ignored files. `--directory` is **not** passed: it collapses a directory into
  one entry whenever everything inside happens to be ignored, which is the
  contents-based reading this change exists to remove.
- A listed file is reproducible when some directory above it is itself ignored,
  so `target/debug/app` under an ignored `target/` does not condemn its parent.
  Deciding that needs `check-ignore` on those ancestors, so the three queries
  run in order: tracked listing, ignored-file listing, then `check-ignore` over
  the targets' ancestors **and** the ancestors the second listing produced.
- The ignored-file listing is issued only for targets that are directories. It
  is the most expensive query here, and for anything else the question does not
  arise.

## Step 3 — `action-rules/src/action_rules.rs` (the policy layer)

**Rename `all_contents_ignored` → `is_under_gitignored_allowed_dir`**
(`action_rules.rs:435-454`), mirroring the original spec's name. Replace its doc
body: the test is now over the directory path, and the reason a directory's own
`.gitignore` cannot grant it is that git never applies `D/.gitignore` to `D` —
so `data/` with an inner `*` is not a `gitignored_allowed_dir` and the files
under it are protected. Keep the `!self.is_repo_root()` scoping.

Update the two carve-outs that name it: `is_on_disallowed_branch`
(`action_rules.rs:512-516`) and `is_in_foreign_repo` (`action_rules.rs:530-534`).

**Widen and scope `is_file_pattern_ignored` (`action_rules.rs:464-466`).** It
now covers a directory holding an irreplaceable file, and the
`location_grants_write` exclusion is required: without it the invariant test
fails, because an ignored file in a repository under `/tmp` comes out both
allowed and protected:

```rust
fn is_file_pattern_ignored(&self) -> bool {
    !self.location_grants_write()
        && self.exists()
        && self.facts.ignored == Ignored::FilePattern
}
```

This follows the module's stated convention: every protected predicate excludes
`location_grants_write` (`action_rules.rs:412`).

**`is_protected` (`action_rules.rs:575-584`)** — add the disjunct the existing
`POLICY:` comment already names, and flip that comment's direction:

```rust
|| self.is_file_pattern_ignored()
// POLICY: remove this disjunct to make ignored-but-irreplaceable files ask
// rather than deny.
```

**Prose to update, since it is the normative spec:**

- The `#![allow(dead_code)]` rationale (`action_rules.rs:73-77`) cites
  `is_file_pattern_ignored` as the standing example of a predicate in neither
  surface. That is no longer true; rewrite it, and drop the attribute if
  `cargo build` reports no dead code.
- Module doc §"Three verdicts" (`action_rules.rs:30-43`) — describe what is left
  in the Ask gap: existing untracked-and-not-ignored files, and `Action::Opaque`.
- `is_new_on_allowed_branch` (`action_rules.rs:499`) — note that a not-yet-
  existing ignored path stays creatable, and that `is_file_pattern_ignored`'s
  `exists()` guard is what keeps the two disjoint.

## Step 4 — the untracked rule and its session-start mitigation

**`action_rules.rs`.** Add `is_untracked_on_allowed_branch` to `is_allowed`:
exists, not tracked, `Ignored::No`, in a lane repository on an allowed branch,
and not the repository root — the root of a repository with nothing committed
is untracked, and would otherwise be writable whole. Carry a `POLICY:` comment,
since this is the disjunct most likely to be reconsidered.

**This empties the Ask gap.** Every target the resolver can produce is now
either allowed or protected, so `Ask` reaches the user only through
`Action::Opaque` and `GitAction::Destructive`. The module doc says so, and
`the_file_dimension_leaves_no_target_undecided` asserts it, so removing any
`is_allowed` disjunct later fails a test that explains itself.

**`~/.claude/uncommitted-at-start.sh`,** wired as a second `SessionStart` hook
in `settings.json`. It runs `git ls-files --others --exclude-standard` in the
launch project and in `$CLAUDE_CONFIG_DIR`, and injects the list as session
context with an instruction to raise it with the user — offering to commit the
files, or to get explicit agreement to proceed with them unprotected. Silent
when there is nothing to report, and always exits 0: a hook that fails must not
stop a session starting.

## Step 5 — tests

**`src/action_rules/tests.rs`**

- `:106-115` — rename to `a_gitignored_directory_is_allowed_on_any_branch`,
  keeping the `UnderGitignoredDir` variant.
- `:201-210` `a_file_pattern_ignored_file_neither_allows_nor_protects` — invert:
  assert `is_protected()` and `check_action(..) == Verdict::Deny`.
- New: an ignored file under `/tmp/repo/` is allowed, not protected (the
  `location_grants_write` scoping).
- New: a non-existent ignored path on an allowed branch is allowed.
- New: a directory carrying `FilePattern` is protected.
- New: a tracked directory is judged by branch, not by what it hides.
- `an_untracked_existing_file_on_an_allowed_branch_asks` → `..._is_allowed`.
- `one_undecided_target_takes_the_command_to_ask` → replaced by
  `the_file_dimension_leaves_no_target_undecided`, since no target is undecided
  any more. Factor the sweep into `for_each_well_formed` so both it and the
  invariant test share one product.
- `:249+` `no_target_is_both_allowed_and_protected` — confirm the path axis
  includes a repository under an exception root; add one if not, so the
  invariant test would have caught the `/tmp` overlap.

**`tests/guard-suite.sh`**

- `:227-233` — rename the section to `Target::is_under_gitignored_allowed_dir`.
  `ignored-dir/` and `target/` are declared in the fixture's root `.gitignore`,
  so these expectations stand.
- `:235-239` — `Write $A/app.log` and `rm app.log` flip from `ask` to `deny`.
- `:98-101` — the `$CH` fixture reproduces `~/.claude`'s deny-all-allowlist
  `.gitignore`. Mirror the Step 1 amendment in it (declare a machine-written
  folder), and add a case proving an undeclared directory's files deny.
- **New fixtures, the cases that motivated this:**
  - `data/`, whose own `.gitignore` holds only `*`. Writing, removing, or
    deleting the directory whole → `deny`.
  - `reproducible/`, undeclared but holding only `target/` → `ask`, since
    nothing under it is irreplaceable.
  - `mixed/`, holding one tracked file and one ignored file. `rm -rf mixed` →
    `allow` on an allowed branch (the exception); `rm -rf mixed/*` → `deny`,
    since naming the files reaches the ignored one.
  - Keep `src/` free of ignored files: an existing assertion globs `src/*`.

## Verification

Run from `~/.claude/action-rules`:

1. `cargo test` — unit tests, including the invariant sweep.
2. `bash tests/guard-suite.sh` — end-to-end against the real binary and real git
   fixtures.
3. `./build.sh` — rebuild the release binary the hook actually runs.
4. `./build-docs.sh` — regenerate `~/.claude/docs/action-rules.md` from the doc
   comments, and read the diff: that page is the spec the rules claim to be.

Then probe the live guard with a `PreToolUse` payload for each of:

| Target | Expected |
| --- | --- |
| `<project>/data/x.csv`, with `data/.gitignore` = `*` | deny |
| `~/.claude/action-rules/target/anything` | allow |
| `~/.claude/plans/x.md` | allow (after Step 1) |
| `<project>/app.log`, existing | deny |
| `<project>/new.log`, not existing, allowed branch | allow |
| `rm -rf <project>/data` | deny |
| `rm -rf <project>/src`, tracked | allow on an allowed branch |
| `<project>/untracked.txt`, existing | allow on an allowed branch, deny otherwise |

Run `uncommitted-at-start.sh` directly with `CLAUDE_PROJECT_DIR` and
`CLAUDE_CONFIG_DIR` set, and confirm it lists the uncommitted files and prints
nothing when there are none.

Finally confirm from the session itself: writing this plan file and a memory
file under `~/.claude/projects/` must still succeed.
