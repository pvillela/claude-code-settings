# Action Rules: pivot the guard spec to the Rust framework

## Context

Hooks for potentially destructive commands are currently implemented by
`~/.claude/global-git-guard.sh` (609 lines of bash, 118 assertions in
`guard_test.sh`). That script has structural defects verified in the archived
`docs/archived/GUARD-REWRITE-PLAN-20260810.md`: it matches text rather than
parsing, and it anchors on `$PWD` so it fails **open** across repositories. Its
`PreToolUse` entry has been removed from `settings.json`, so there is no guard
active right now.

The pivot: `action-rules/src/action_rules.rs` becomes the single source of truth
for both the **specification** and (later) the implementation of the rules. This
change delivers the specification only — the rule vocabulary, the predicates, and
the composition — with function bodies left as `todo!()`. A build script renders
the spec to Rustdoc and to a Markdown page, and `CLAUDE.md` shrinks to a pointer
at that page.

Implementing `Action::parse` (the shell scanner) is explicitly **out of scope**
and is the next piece of work after this.

## Decisions carried in from the design session

| Area | Decision |
|---|---|
| Scope | Spec only; `todo!()` bodies stay; no hook installed |
| Uncertainty | Two **non-complementary booleans** on `Target`; the gap between them is Ask |
| `Outcome` | Appears nowhere below `check_action` |
| Branch | Moves off `Target` onto a new `Repo` term |
| Git | A small second dimension, composed worst-wins with the file dimension |
| Interim guard | None. Default Claude Code has no hooks; acceptable for now |

## Work

### 1. Rename the package

`action-rules/Cargo.toml`: `name = "action-target-rules"` → `"action-rules"`.
The crate then imports as `action_rules`, matching the directory and the module.

### 2. Rewrite `action-rules/src/action_rules.rs`

**New terms.**

- `Repo { root: PathBuf, branch: Option<String> }` with
  `is_allowed_branch()`. Allowed set is `{"aicode"} ∪ <root>/.claude/allowed-branches`
  — a **union**, fixing the replace-vs-union bug in the bash guard that no test
  covers. `branch: None` (detached HEAD) is **not** allowed.
- `Effect { Create, Change }`, carried **per target**.
- `Action::Write(Vec<(Target, Effect)>)` replaces the separate `Create` /
  `Change` variants, so `mv tracked.txt /tmp/x` reads correctly as a change to
  one target plus a creation of another.

**`Target` predicates** — all `bool`, each individually documented, since each
doc comment becomes a row in the generated Markdown.

- `is_tracked()` — true for a file tracked by its repo, **and for any directory
  containing a tracked file**. This is what gives `rm -rf src/` the branch rule
  and `git reset --hard` (target = repo root) its verdict without a git-specific
  rule.
- `is_repo_root()` — protected. Closes the case where the branch rule would
  authorise `rm -rf <repo root>`, which destroys the `.git` directory that makes
  the branch rule safe in the first place.
- `all_contents_ignored()` — renamed from `is_under_full_folder_gitignore()`.
  Semantics unchanged from the draft: everything under the folder is recursively
  ignored. **Implementation note:** cannot be implemented by testing the
  directory path. `git check-ignore -v projects` reports the *negation*
  `!**/` (`~/.claude/.gitignore` line 6, present so git can descend), while a
  file under it matches `*`. Test contents, or probe a synthetic path underneath.
- `is_file_pattern_ignored()` — ignored by a file-level pattern rather than a
  recursively-ignored folder (`devcontainer.env`, `data/*.xlsx`).
- Path-existence is first-class, so `Create` is reachable. In the draft,
  `is_in_allowed_branch()` required the target to be git-controlled, which made
  every new file in the launch project deny even on `aicode`.
- Exceptions list (`ALLOWED_PATH_REGEX_STRS`) is corrected: `^~/.claude` never
  matches `/home/vscode/.claude`; use `$CLAUDE_CONFIG_DIR` resolved at runtime.
  Add the write sinks `/dev/null`, `/dev/stdout`, `/dev/stderr`, `/dev/tty`.

**Lane placement**, replacing the `!is_under_git_root()` disjunct on line 102 —
note this **reverses** it, since being outside a repo becomes a deny:

- Under no repo and matching no exception → **protected**, creation included.
- Under a repo that is neither the launch project nor `~/.claude` → **protected**,
  creation included, with a `// POLICY:` marker to flip to Ask.

`~/.claude` needs no special rule: its `.gitignore` recursively ignores
`projects/`, `plans/`, and the rest, so `all_contents_ignored()` already permits
memory and session writes.

**The two policy surfaces.** Each is a flat disjunction of named predicates, with
`// POLICY:` on the flippable ones, so a change is one line moved:

```rust
fn is_allowed(&self, effect: Effect) -> bool { /* named disjuncts */ }

/// **Not** the complement of `is_allowed`. The invariant is
/// `!(is_allowed() && is_protected())`; the gap between them requires
/// confirmation.
fn is_protected(&self, effect: Effect) -> bool { /* named disjuncts */ }
```

Currently flippable: `is_file_pattern_ignored` (Ask now, uncomment in
`is_protected` for Deny) and foreign-repo (Deny now, remove for Ask).

**`Action` classification.**

- `Forbidden` shrinks to `chown`, `chgrp`, `shred`. `chmod`, `ln`, `touch` are
  demoted to ordinary target-classified writes so the exception lanes apply.
- `Opaque` → Ask, as drafted. Reserved for `$VAR`, `$(…)`, backticks, and
  unparseable syntax. **Globs are expanded at decision time** rather than treated
  as opaque, so `rm -rf target/*` resolves to real targets instead of prompting.

**Git dimension** — deliberately small, two jobs only:

1. Destructive subcommands that **Ask even on an allowed branch**:
   `reset --hard`, `clean -f*`, `checkout -- .`, `branch -D`, `stash drop`,
   `push --force`. Without this, `git clean -fdx` on `aicode` allows silently
   while `rm untracked.txt` — a strictly narrower action — asks.
2. `git config` writes at **any** scope denied unconditionally. `--local`
   targets `<repo>/.git/config`, which is neither tracked nor ignored and would
   otherwise land in the Ask gap.

Everything else about git flows through the file dimension via the repo-root
target. `check_command` takes the **worst** of the file verdict and the git
verdict.

**`check_action` keeps the shape already drafted** — short-circuits, Allow rules,
Deny rules, fall through to Ask — with the deny rule reading
`any(t.is_protected())` instead of `any(!t.is_allowed())`. That single change is
what makes the fall-through live.

### 3. Markdown generation — `action-rules/src/bin/gen-md.rs`

Doc comments are the source of truth. A `syn`-based extractor (stable toolchain;
rustdoc JSON is nightly-only with an unstable schema) walks the module, honours
`#[doc(hidden)]` and visibility exactly as the Rustdoc invocation does, and emits
`~/.claude/docs/action-rules.md`.

Per the prompt: non-public items relevant to rule semantics stay documented;
irrelevant non-public items get `#[doc(hidden)]`.

### 4. Build script — `action-rules/build-docs.sh`

```sh
cargo doc --document-private-items   # → action-rules/target/doc  (gitignored)
cargo run --bin gen-md               # → ~/.claude/docs/action-rules.md
```

`~/.claude/.gitignore` already allowlists `!action-rules/**` and `!docs/**`, and
`action-rules/.gitignore` already ignores `**/target/`. No gitignore changes
needed.

### 5. `~/.claude/CLAUDE.md`

Replace the `## Critical Git rules` / `TBD.` stub with two or three sentences and
a relative link to `docs/action-rules.md`.

### 6. Left alone

`global-git-guard.sh` and `guard_test.sh` are untouched and stay uninstalled.
The 118 assertions become the differential corpus when the implementation lands;
nine are already known to need updating.

## Prerequisite — resolved

`~/.claude` was on `main`, which would have made regenerating
`docs/action-rules.md` a write to a tracked file on a disallowed branch. It has
since been moved to `aicode` (verified, working tree clean). No
`~/.claude/.claude/allowed-branches` file exists, so the allowed set is the
`{"aicode"}` fallback and now matches. No further action needed.

## Verification

1. `cd ~/.claude/action-rules && cargo check` — the spec compiles with `todo!()`
   bodies.
2. `cargo clippy` — clean.
3. `bash build-docs.sh` — both outputs produced.
4. Open `target/doc/action_rules/index.html`; confirm private items relevant to
   rule semantics appear and `#[doc(hidden)]` items do not.
5. Read `~/.claude/docs/action-rules.md`; confirm every named predicate in
   `is_allowed` and `is_protected` has a row, and that the `// POLICY:` knobs are
   findable.
6. `npx markdownlint ~/.claude/docs/action-rules.md`.
7. Confirm the relative link in `CLAUDE.md` resolves.

No behavioural tests — nothing executes until `Action::parse` is implemented.
