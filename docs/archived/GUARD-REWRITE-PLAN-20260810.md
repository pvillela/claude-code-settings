# Guard rewrite — plan

Destination: project root. Written to scratchpad because the project is on `main`.

## 1. Purpose

The hook is a **safety net, not a policy enforcer**. Its remit is **irreversibility**: it exists to
stop changes that cannot be reviewed or undone. Everything else in `CLAUDE.md` is honour-system and
marked as such. Completeness is not a goal — shell writes are statically undecidable, so a gap in
the long tail is a known limit, not a defect to be patched.

`CLAUDE.md` is normative for **intent**. The hook is a **gate**. If the gate clashes with
`CLAUDE.md`, the gate is corrected or removed.

Two defects motivate the rewrite, both structural:

- **Text-based matching.** A regex decides "does this look like a write?" and cannot see quoting, so
  `echo "a -> b"` read as a redirection. Patched once by excluding `-`; `=>` and `<>` remain.
- **PWD-anchored context, which fails OPEN.** Verified: with cwd in a repo on `aicode`,
  `git -C <repo on main> commit`, `echo hi > <repo on main>/f`, `sed -i … <repo on main>/f` and
  `rm <repo on main>/f` are all permitted today.

Other verified holes on `main`: `git -c user.name="A B" commit` (silent allow — a quoted value with
a space defeats bash word-splitting), `git add -A`, `git fetch`, `git submodule update --init`,
`perl -pi -e`, `echo x 2>err.log`. And false denials: `cp README.md /tmp/backup.md`,
`echo hi > ~/x`, `echo "a; git commit -m x"`.

## 2. The decision model

Two lists, **allow** and **deny**, each in disjunctive normal form: an OR of triples, each triple an
AND of three predicates. A predicate may be any boolean combination of decidable sub-predicates.
**Deny wins** on overlap. Matching neither list → **ask**.

Axes: **operation × branch × target-class**. Target-class is enriched to absorb location, so the
`~/.claude` carve-out is a value rather than an exception.

**Lanes.** The safe region is:

```
project repo   — session root, FIXED AT STARTUP (not payload cwd, so a `cd` cannot move it),
                 judged on its own branch
~/.claude      — judged on ITS own branch; untracked paths there exempt on any branch
scratch        — /tmp, $TMPDIR, the session scratchpad, /dev write sinks
```

Anything outside all three is out of lane.

**Verdict table** (the substance of the two lists):

| operation | branch | target class | verdict |
|---|---|---|---|
| no effect (determined) | * | * | allow |
| git config write | * | * | **deny** |
| git, on the read allowlist | * | * | allow |
| git, not on the allowlist | protected | * | **deny** |
| git, not on the allowlist | allowed | * | allow |
| chown / chgrp | * | * | **deny** |
| other metadata | allowed | * | allow |
| other metadata | protected | * | **deny** |
| write / delete | protected | * | **deny** |
| write / delete | allowed | tracked | allow |
| write / delete | allowed | nonexistent | allow |
| write / delete | allowed | not a regular file | allow |
| write / delete | * | untracked under `~/.claude` | allow |
| write / delete | * | in scratch | allow |
| write / delete | allowed | existing regular file git cannot restore | **ask** |

The last row is the one that carries the remit, and it applies identically to an untracked file in a
repo, a gitignored file, and a file outside every repo. The ignored set is two populations — build
artifacts meant to be rewritten, and irreplaceable data (`data/*.xlsx`,
`.devcontainer/devcontainer.env`) — and nothing can tell them apart, so `ask` is the only honest
verdict. **This reverses `0a759c4`**, where untracked-existing denied on every branch.

`exists` is refined to `exists ∧ is_regular_file`, which handles `/dev/null`, FIFOs, sockets and
directories in one test rather than a location list that has to grow.

## 3. Unknowns

Atoms take values in {T, F, U}; connectives are Kleene (`F ∧ U = F`, `T ∨ U = T`). Evaluate both
predicates, then take the **worst possible verdict** (deny > ask > allow):

| deny | allow | verdict |
|---|---|---|
| T or U | — | deny |
| F | T | allow |
| F | U or F | ask |

The value of three-valued evaluation is that **an unknown usually never reaches the verdict**.
`grep -rn pattern "$X"` has every target atom U, but `is_write` is known false and the deny predicate
has it as a conjunct, so the deny is F and the command runs silently. `git config --global …` denies
with certainty from an unresolvable cwd, because the config deny has no target atoms in it.

- **`$VAR`, `$(…)`, backticks** — unbounded, since a variable may hold `../..`. All target atoms U.
- **globs** — bounded (a glob never matches `/`), and the guard may **expand them deliberately** at
  decision time, collapsing U to T or F.
- **`$HOME`, `$PWD`, `$TMPDIR`, `$CLAUDE_CONFIG_DIR`** — resolved from the hook's own environment,
  never U.

The verdict for a genuinely unknown target is **deny**, not ask, on this principle:

> **Ask when the target is known but its value isn't. Deny when the target itself is unknown.**

A gitignored file is the first kind — no care on my part resolves whether it's precious. An
unresolvable target is the second, and the cure is always mine: write the literal path. This becomes
a `CLAUDE.md` rule: **write literal paths; don't introduce a variable the guard can't resolve.**

## 4. Implementation

Python 3.10, stdlib only, under `~/.claude/guard/`:

| module | contents |
|---|---|
| `scan.py` | Purpose-built scanner. `Tok(text, op, has_subst, has_glob, adjacent)`; `'…'` inert, `"…"` live for `$`/`` ` ``; longest-match operators incl. `&&`, `>>`, `<<`, `<>`, `2>`, `&>`; fd-prefix merge; `#` ordinary mid-word; heredoc bodies discarded. |
| `effects.py` | Segment → effects. Utility table with **argument roles** (`cp src dst` — src is a read, which is why `cp README.md /tmp/x` wrongly denies today). `cd` rebasing with `(` as a barrier. |
| `gitcmd.py` | Read allowlist, config/remote analysis, `GitInvocation` with `-C` composition and `--git-dir`/`--work-tree`. |
| `repo.py` | `repo_of`, `tracked_set`, `allowed_branches` — memoised, batched, subprocess-hardened. |
| `judge.py` | The two lists and the Kleene evaluator. Under 80 lines. |
| `guard.py` | Payload parse, dispatch, single-write emitter, exception wrapper. |

**Not `shlex`.** Verified: it truncates `--format=%h#x` at the `#`, shreds unquoted `$( )` into
phantom operator tokens, and returns identical output for `'$(date)'` and `"$(date)"` — losing the
quote provenance the rewrite exists to preserve. Kept as a differential **test oracle**.

`global-git-guard.sh` becomes a POSIX-sh shim that `exec python3 …`, so `settings.json` is unchanged
and a missing interpreter degrades to `ask` rather than exit 127, which Claude Code treats as a
non-blocking error that lets the tool proceed.

`allowed = {"aicode"} ∪ file` — the current code uses the file *instead of* `aicode` when present,
contradicting `CLAUDE.md`. No test catches it.

**`~/.claude/.gitignore` is deny-all-then-allowlist**, so every new file is silently untracked until
listed. Add `!guard/**`.

## 5. Tests

`guard_test.sh` keeps its shape and `bash "$G"` contract. New blocks: cross-repo/lane cases, the git
allowlist default, scanner quoting (`=>`, `<>`, `%h#x`, `2>f`, heredocs, unbalanced quotes, `~`),
and two invariants — the `shlex` differential, and **zero subprocesses for a no-effect command**.

Expectations that change: untracked-existing on an allowed branch (deny → **ask**); `chmod` on a
protected branch (ask → **deny**); `chown` anywhere (ask → **deny**); unlisted git subcommand on a
protected branch (silent → **deny**); `cp src dst` (deny → **allow**); `echo hi > ~/x` (deny →
**allow**); `echo "a; git commit"` (deny → **allow**); `sed -n 1,5p f && grep -i x` (deny →
**allow**); `git -c user.name="A B" commit` (allow → **deny**).

## 6. Risks

- Malformed payload → **ask**, never deny: the matcher fires only on tools that can write, so an
  unreadable payload means "cannot rule out a write", and denying on a parse bug bricks the session.
- Any unhandled exception → **ask**, traceback to stderr. Build the JSON fully, then one
  `sys.stdout.write` — a partial write plus a traceback is malformed JSON, which reads as "no
  opinion", i.e. a silent allow.
- Git subprocess: `timeout=2`, `stdin=DEVNULL`, `GIT_TERMINAL_PROMPT=0`, `GIT_OPTIONAL_LOCKS=0`,
  and strip `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` from the child env.
- Emitting `permissionDecision: "allow"` is *not* the danger I claimed earlier — per the docs it
  does not bypass `settings.json` deny rules. Silence is still preferred, because it preserves the
  normal permission flow rather than auto-approving.
- Residual: the Bash tool's persistent shell cwd can drift from the payload's `cwd`. The fixed
  session root avoids this for lane membership; relative-path resolution still inherits it.

---

# Open questions

**Q9 — Scratch roots.** Which paths count as disposable?
➡️ `/tmp`, `$TMPDIR`, the session scratchpad, and `/dev` write sinks (`null`, `stdout`, `stderr`,
`fd/*`). The `is_regular_file` test already covers most of the `/dev` case; this list is the rest.

**Q11 — Creating a NEW file outside every lane** (`~/newnotes.txt`, `/data/out.csv`).
➡️ **Allow.** Nothing is destroyed, consistent with the in-lane rule where creation is explicitly not
covered. The alternative — deny, on containment grounds — is coherent if you want lane discipline to
be the stronger principle; scratch already covers every legitimate need.

**Q12 — Destructive git on an ALLOWED branch:** `reset --hard`, `clean -fdx`, `checkout -- .`,
`stash drop`, `branch -D`.
➡️ **Ask on a short list.** These destroy uncommitted work, which has no reflog and no copy. Today
we deny `rm untracked.txt` as unrecoverable while `git clean -fdx` deletes every untracked file
silently — same effect, opposite verdict, purely because one is spelled `git`.

**Q13 — Scripts.** `bash ./script.sh` shows the guard an invocation, never the writes inside.
➡️ **Accept as a documented limit.** Scanning would deny nearly every script under pessimism,
including `guard_test.sh`. The Golden Rule holds this line, not the gate.

**Q14 — The git read allowlist contents.** Everything unlisted denies on a protected branch, and
this repo is usually on `main`.
➡️ Start with: `status log diff show blame grep describe shortlog rev-parse rev-list ls-files
ls-tree ls-remote cat-file merge-base name-rev for-each-ref diff-tree diff-index check-ignore
check-attr cherry count-objects var help version verify-commit verify-tag range-diff`, plus the
reading forms of `branch`/`tag`/`stash`/`worktree`/`remote`. Deny messages name the fix, so an
omission costs one line of config.

**Q15 — Does the `!` prefix fire the hook?** Undocumented, and I can't test it — only you can type
one. It decides whether you retain an escape hatch by design.
➡️ Run `! chmod +x /tmp/guard-probe-nonexistent`. A prompt means hooks fire; silence means `!`
bypasses them.

**Q16 — `CLAUDE.md` interaction rules need amending**, independently of the guard. The current text
says not to restate open questions when you pick chat; you've since asked for proposals to be
replayed. And the batching failure today was mine against an explicit request.
➡️ Replace with: *one question per round*; on chat, *replay that question's options compactly and
stop*.

**Q17 — Migration order.** The rewrite changes many verdicts at once.
➡️ Land `CLAUDE.md` first (spec), then the guard behind the shim with the full suite green, then
delete the old script. Three commits, reviewable separately.
