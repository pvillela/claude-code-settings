<!-- markdownlint-disable MD013 -->

# Triage of `guard_test.sh`

All 118 assertions of the previous suite, each read against the new
specification **before** any new test was written. By the time the new suite
runs, every legitimate difference is predicted here, so an unpredicted one is
unambiguously a defect rather than an expectation to edit.

- **unchanged** — the new spec agrees with the old expectation. 94 rows.
- **changed-by-design** — the new spec disagrees, and a named decision explains
  it. 17 rows.
- **obsolete** — the rule the row tested no longer exists. 7 rows.

**Pass 2, after the suite ran.** 214 assertions, one unpredicted failure, and it
was in the prediction rather than in the rules:
`git --git-dir=<other>/.git commit` was scored `deny` here and came back `ask`.
`Ask` is right — the scanner does not follow `--git-dir`, so it cannot say which
repository the operation lands in, which is `Action::Opaque` by definition. The
implementation had reached the same verdict by the wrong route, classifying the
call as *destructive*, and was corrected to say what it means. A config write
still denies through a `--git-dir`, because a config write needs no repository
to reach its verdict. Everything else matched the table.

`ALLOW` in the old suite means the guard said nothing. The new guard also says
nothing when it allows, so the two are the same outcome.

## Two things the fixtures must change

**The fixture root may not be under `/tmp`.** `guard_test.sh` builds its repos
in `mktemp -d`, so every path in it now matches
`Target::EXCEPTION_PATH_REGEX_STRS` and is allowed outright, which would make
the suite pass vacuously. The new suite builds under
`${XDG_STATE_HOME:-$HOME/.local/state}/action-guard-tests` instead. Row 76 is
scored against the moved fixture, which is why it changes.

**`CLAUDE_PROJECT_DIR` must be set per case.** Lane membership no longer comes
from the hook process's working directory. Every row below is scored with the
launch project set to the repository the case runs in — `$A` for rows in `$A`,
`$D` for rows in `$D`, and `$A` for the rows in `$CH` and `$TMP`. Without that,
every row would collapse to "foreign repository, denied", which would be an
artefact of the harness rather than a fact about the rules.

## The changed and obsolete rows

| # | Where | Assertion | Old | New | Why |
| --- | --- | --- | --- | --- | --- |
| 66 | `$A` | `Write untracked.txt` | deny | **ask** | changed-by-design. An existing untracked file is either build spoil or something irreplaceable, and nothing can tell which, so `ask` is the only honest verdict. It sits in the gap between the two policy surfaces by construction. Reverses `0a759c4`. |
| 71 | `$A` | `Write ignored-dir/untracked-but-ignored.md` | deny | **allow** | changed-by-design. `ignored-dir/` is wholly ignored, so `Target::all_contents_ignored` holds and its contents are reproducible on any branch. |
| 72 | `$D` | `Write ignored-dir/untracked-but-ignored.md` | deny | **allow** | changed-by-design, as row 71. The branch does not enter it. |
| 75 | `$CH` | `Write settings.local.json` | ALLOW | **deny** | obsolete. The `~/.claude` carve-out is gone. It is *not* subsumed by the gitignore predicate: the file is ignored by a pattern of its own, not by a wholly-ignored directory, so on `$CH`'s disallowed branch `is_on_disallowed_branch` refuses it. **See the note below — this one bites in real use.** |
| 76 | `$TMP` | `Write loose.txt` | ALLOW | **deny** | changed-by-design. Under the moved fixture root this is `Target::is_loose`: a file with no version control behind it is the least recoverable target there is. The plan's own example, `echo hi > ~/x`, is the same row. |
| 77 | `$A` | `echo hi > untracked.txt` | deny | **ask** | changed-by-design, as row 66. |
| 78 | `$A` | `echo hi > ignored-dir/untracked-but-ignored.md` | deny | **allow** | changed-by-design, as row 71. |
| 80 | `$A` | `cp /etc/hostname untracked.txt` | deny | **ask** | changed-by-design, as row 66. |
| 81 | `$A` | `rm untracked.txt` | deny | **ask** | changed-by-design, as row 66. |
| 82 | `$A` | `echo hi > tracked.txt` | ask | **allow** | obsolete. The old rule 3 asked about shell rewrites of tracked files because they bypass the `PostToolUse` formatter. The new spec has no such rule: a tracked file on an allowed branch is recoverable from history, and formatting is not the guard's remit. |
| 83 | `$A` | `sed -i s/a/b/ tracked.txt` | ask | **allow** | obsolete, as row 82. |
| 84 | `$A` | `cp /etc/hostname tracked.txt` | ask | **allow** | obsolete, as row 82. |
| 86 | `$A` | `sed -i s/a/b/ tracked.txt untracked.txt` | deny | **ask** | changed-by-design. The tracked target allows, the untracked one asks, and `ask` is the worse of the two. Under the old rules the untracked target denied. |
| 87 | `$A` | `Write untracked.txt` | deny | **ask** | changed-by-design, as row 66. |
| 95 | `$A` | `rm -rf /tmp/scratch/*` | ask | **allow** | changed-by-design. Globs are expanded against the filesystem rather than treated as opaque; the expansion — or, unmatched, the literal path — lies under `/tmp`. |
| 96 | `$D` | `rm -rf "$SOMEVAR/dir"` | deny | **ask** | changed-by-design. `Action::Opaque` resolves to `Ask`, never `Deny`. The old suite denied unresolvable targets on a protected branch; the new spec puts *all* uncertainty in `Ask`, on the grounds that denying on a parse bug obstructs ordinary work. |
| 97 | `$D` | `cat x.txt > out-$(date +%s).txt` | deny | **ask** | changed-by-design, as row 96. |
| 100 | `$D` | `rm -rf /tmp/scratch/*` | ask | **allow** | changed-by-design, as row 95. |
| 101 | `$D` | `sed -i s/a/b/ /etc/hosts.d/*.conf` | ask | **deny** | changed-by-design. The glob expands to nothing, leaving a literal path outside every repository — `Target::is_loose`, which is now protected. |
| 102 | `$D` | `cd /tmp && rm -rf build/*` | ask | **allow** | changed-by-design. The literal `cd` rebases the relative glob into `/tmp`, an exception path, and the expansion is no longer opaque. |
| 103 | `$D` | `cd /tmp && rm -rf $X/build` | deny | **ask** | changed-by-design, as row 96. |
| 114 | `$A` | `echo hi >> untracked.txt` | deny | **ask** | changed-by-design, as row 66. |
| 117 | `$D` | `chmod +x tracked.txt` | ask | **deny** | obsolete. The old rule 3 prompted for metadata changes as a class. `Action::Forbidden` now holds only `chown`, `chgrp` and `shred`; `chmod` is an ordinary write judged by its target, and on a disallowed branch an ordinary write is refused. |
| 118 | `$A` | `chmod +x tracked.txt` | ask | **allow** | obsolete, as row 117. A tracked file on an allowed branch. |

## The unchanged rows

| Rows | Where | What | Verdict |
| --- | --- | --- | --- |
| 1–7 | `$A` | `git config` write forms: `--global`, `--system`, bare `k v`, `--local`, `--unset`, `set`, `--file` | deny — `GitAction::ConfigWrite`, unconditional |
| 8 | `$TMP` | `git config --global` outside every repository | deny — same, and it needs no repository to reach it |
| 9–13 | `$A`/`$D` | `git config` read forms: `--get`, `--list`, bare key, `get`, `--get-regexp` | allow |
| 14–19 | `$A` | `git remote add/set-url/remove/rm/rename/set-branches` | deny — config writes by another name |
| 20–24 | `$D` | `git remote`, `-v`, `--verbose`, `show`, `get-url` | allow |
| 25–28 | `$D`/`$A` | `git remote prune`/`update` | deny on `$D`, allow on `$A` — an ordinary state change, following the branch |
| 29 | `$A` | `git -c user.name=x -c user.email=y commit -m z` | allow — `-c` persists nothing |
| 30 | `$A` | `echo hi && git config --global user.name x` | deny — a config write mid-command is still caught |
| 31 | `$A` | `git status && git config --get user.name` | allow |
| 32–43 | `$D` | twelve read-only git invocations, and `echo 'git commit here'` | allow — the read allowlist, and a quoted string that only looks like one |
| 44–57 | `$D` | fourteen state-changing git invocations | deny — `StateChange`/`Destructive` in a repository the branch does not govern |
| 58–61 | `$A` | `commit`, `branch new`, `push`, `merge` | allow |
| 62–63 | `$D` | `Write tracked.txt`, `Write new.txt` | deny — disallowed branch, existing and new alike |
| 64 | `$A` | `Write tracked.txt` | allow |
| 67 | `$D` | `Write untracked.txt` | deny — the branch decides before the tracking does |
| 68–69 | `$A` | `Write brand-new.rs`, `Write nested/deep/brand-new.rs` | allow — creation on an allowed branch, at any depth |
| 70 | `$A` | `Write tracked.txt` | allow |
| 73–74 | `$CH` | `Write projects/someproj/memory/MEMORY.md`, `…/brand-new.md` | allow — the deny-all-then-allowlist `.gitignore` makes `projects/` wholly ignored, which is exactly the case `all_contents_ignored` is written for |
| 79 | `$A` | `echo hi > brand-new.txt` | allow |
| 85 | `$CH` | `sed -i s/a/b/ CLAUDE.md` | deny — tracked, on a disallowed branch |
| 88–89 | `$A` | `Write brand-new.txt`, `Write tracked.txt` | allow — the file tools agree with the shell |
| 90–94 | `$D` | five writes through the shell | deny |
| 98–99 | `$D` | `rm -rf *.log`, `rm -rf build/*` | deny — the globs match nothing, and the literal paths are in the repository |
| 104–112 | `$A`/`$D` | nine reads, including `2>/dev/null`, `> /dev/null`, `< tracked.txt`, and arrows inside quotes | allow — and `cargo build --release` allows because `cargo` is not a utility the scanner knows, the documented limit |
| 113 | `$D` | `echo hi > tracked.txt` | deny |
| 115–116 | `$A`/`$D` | `git commit -m x` style rows already covered | deny/allow per branch |

## Row 75, and what it means for the real `~/.claude`

The fixture puts `$CH` on `main` deliberately, so the row's `deny` is correct as
scored. The real `~/.claude` is on `aicode`, which *is* allowed, so the same
write there does not deny. What it does instead is worth knowing before
installation:

- A file that does not exist yet — a new `settings.local.json`, a new memory
  file — is `is_new_on_allowed_branch`, and is allowed.
- A file that already exists, is untracked, and is ignored by `~/.claude`'s
  deny-all `.gitignore` **asks**. It is `is_file_pattern_ignored`: ignored, so
  it has no history, and irreplaceable for all the guard can tell.
- A file under a wholly-ignored directory — `projects/`, `plans/`, `tasks/` —
  is allowed, because those directories satisfy `all_contents_ignored`. Files
  sitting directly in `~/.claude` do not.

So the population that asks is small and specific: existing loose files in the
top level of `~/.claude`. If that turns out to be a nuisance, the `POLICY:`
comment on `Target::is_protected` names the one-line change, and
`~/.claude/.claude/allowed-branches` does not enter into it — the branch is
already allowed.
