#!/bin/bash
# End-to-end suite for the action rules: real binary, real git, real payloads.
#
# Written from the specification in src/action_rules.rs, with
# docs/guard-triage.md as the coverage checklist. Deliberately not written by
# editing the previous suite's assertions -- that is how a superseded
# expectation survives by inertia.
#
#   bash ~/.claude/action-rules/tests/guard-suite.sh
#
# Two things about the fixtures matter, and both are consequences of the rules:
#
#   1. The fixture root is NOT under /tmp. Every path under /tmp is an allowed
#      exception, so building the repositories there would make the suite pass
#      without testing anything.
#   2. CLAUDE_PROJECT_DIR is set per case rather than inherited. Lane
#      membership no longer comes from the hook process's working directory,
#      which is the whole point, so each case has to say which repository the
#      session was launched in.
#
# The assertions pass git strings as DATA. The guard cannot tell those from
# real invocations, which is why they live in a file rather than being typed at
# a shell: the guard sees only the `bash guard-suite.sh` that starts it.

GUARD="${GUARD:-$HOME/.claude/action-guard.sh}"
[ -r "$GUARD" ] || { echo "guard not found: $GUARD" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

# Not $TMPDIR: see note 1 above.
BASE="${XDG_STATE_HOME:-$HOME/.local/state}/action-guard-tests"
rm -rf "$BASE"
mkdir -p "$BASE"
ROOT=$(cd "$BASE" && pwd -P)
trap 'rm -rf "$ROOT"' EXIT

git_init() { # git_init <dir> <branch>
  git init -q -b "$2" "$1"
  git -C "$1" config user.name t
  git -C "$1" config user.email t@t
}

commit_all() { git -C "$1" add -A && git -C "$1" commit -qm "$2"; }

# A: the launch project, on the fallback allowed branch.
# D: the same shape, on a branch the session may not change.
A="$ROOT/allowed"
D="$ROOT/disallowed"
for pair in "$A:aicode" "$D:main"; do
  dir=${pair%:*}; br=${pair#*:}
  git_init "$dir" "$br"
  printf 'ignored-dir/\n*.log\ntarget/\n' > "$dir/.gitignore"
  echo hi > "$dir/tracked.txt"
  mkdir -p "$dir/src" && echo mod > "$dir/src/lib.rs"
  commit_all "$dir" init
  # Present in the working tree but never added.
  echo loose > "$dir/untracked.txt"
  mkdir -p "$dir/ignored-dir" && echo x > "$dir/ignored-dir/untracked-but-ignored.md"
  # Wholly ignored, and non-empty: the build-output case.
  mkdir -p "$dir/target/debug" && echo bin > "$dir/target/debug/app"
  # Ignored by a pattern of its own, and existing: the irreplaceable population.
  echo log > "$dir/app.log"
  # A directory that ignores its own contents from within. NOT a
  # gitignored_allowed_dir: git never applies data/.gitignore to data, so
  # nothing above it ever declared it reproducible.
  mkdir -p "$dir/data" && printf '*\n' > "$dir/data/.gitignore"
  echo secret > "$dir/data/notes.csv"
  # Undeclared itself, but everything under it is reproducible build output.
  mkdir -p "$dir/reproducible/target/debug" && echo bin > "$dir/reproducible/target/debug/app"
  # A tracked directory hiding an irreplaceable file: the exception.
  mkdir -p "$dir/mixed" && echo keep > "$dir/mixed/keep.txt"
  git -C "$dir" add mixed/keep.txt && git -C "$dir" commit -qm mixed
  echo log > "$dir/mixed/debug.log"
done

# F: a second checkout, on an allowed branch, that is never the launch project.
F="$ROOT/foreign"
git_init "$F" aicode
echo hi > "$F/tracked.txt"
printf 'target/\n' > "$F/.gitignore"
commit_all "$F" init
echo loose > "$F/untracked.txt"
mkdir -p "$F/target" && echo bin > "$F/target/app"

# B: a repository whose allowed-branches file names a branch of its own.
B="$ROOT/branchfile"
git_init "$B" release
mkdir -p "$B/.claude"
printf '# comment\n\nrelease\n' > "$B/.claude/allowed-branches"
echo hi > "$B/tracked.txt"
commit_all "$B" init

# E: a repository whose allowed-branches file does NOT name the fallback, to
# prove the fallback is not withdrawn by the file's presence.
E="$ROOT/fallback"
git_init "$E" aicode
mkdir -p "$E/.claude"
printf 'release\n' > "$E/.claude/allowed-branches"
echo hi > "$E/tracked.txt"
commit_all "$E" init

# H: a detached HEAD.
H="$ROOT/detached"
git_init "$H" aicode
echo hi > "$H/tracked.txt"
commit_all "$H" init
git -C "$H" checkout -q --detach HEAD

# CH: the $CLAUDE_CONFIG_DIR stand-in, on a disallowed branch deliberately, with
# the deny-all-then-allowlist .gitignore the real one uses. `!*/` re-includes
# every directory so git can descend, which is exactly why a machine-written
# directory has to be declared explicitly to become a gitignored_allowed_dir.
CH="$ROOT/claudehome"
git_init "$CH" main
printf '*\n!*/\n!**/\n!.gitignore\n!CLAUDE.md\nprojects/\n' > "$CH/.gitignore"
echo doc > "$CH/CLAUDE.md"
commit_all "$CH" init
mkdir -p "$CH/projects/someproj/memory"
echo mem > "$CH/projects/someproj/memory/MEMORY.md"
# Ignored by the catch-all `*`, in a directory nothing declared: irreplaceable.
mkdir -p "$CH/undeclared"
echo note > "$CH/undeclared/note.md"
echo '{}' > "$CH/settings.local.json"

# LOOSE: under no repository, and not under an exception path.
LOOSE="$ROOT/loose"
mkdir -p "$LOOSE"
echo hi > "$LOOSE/file.txt"

export CLAUDE_CONFIG_DIR="$CH"

pass=0; fail=0; failed_lines=()

decide() { # decide <project_dir> <json> -> allow|ask|deny
  local out d
  out=$(printf '%s' "$2" | CLAUDE_PROJECT_DIR="$1" bash "$GUARD" 2>&1)
  d=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.permissionDecision // "allow"' 2>/dev/null)
  [ -z "$d" ] && d=allow
  printf '%s' "$d"
}

check() { # check <label> <got> <want>
  if [ "$2" = "$3" ]; then
    pass=$((pass+1)); printf '  ok    %-62s %s\n' "$1" "$2"
  else
    fail=$((fail+1)); printf '  FAIL  %-62s got %s want %s\n' "$1" "$2" "$3"
    failed_lines+=("$1 (got $2, want $3)")
  fi
}

run() { # run <cwd> <project> <command> <expected>
  local json d
  json=$(jq -cn --arg c "$3" --arg w "$1" \
    '{hook_event_name:"PreToolUse",cwd:$w,tool_name:"Bash",tool_input:{command:$c}}')
  d=$(decide "$2" "$json")
  check "$(printf '%.60s' "$3")" "$d" "$4"
}

runw() { # runw <cwd> <project> <file_path> <expected>
  local json d
  json=$(jq -cn --arg f "$3" --arg w "$1" \
    '{hook_event_name:"PreToolUse",cwd:$w,tool_name:"Write",tool_input:{file_path:$f}}')
  d=$(decide "$2" "$json")
  check "Write ${3#$ROOT/}" "$d" "$4"
}

runtool() { # runtool <tool_name> <json_input> <expected>
  local json d
  json=$(jq -cn --arg t "$1" --argjson i "$2" --arg w "$A" \
    '{hook_event_name:"PreToolUse",cwd:$w,tool_name:$t,tool_input:$i}')
  d=$(decide "$A" "$json")
  check "tool $1" "$d" "$3"
}

# =============================================================================
echo "== Tool dispatch: readers return before any I/O =="
for t in Read Grep Glob TodoWrite WebFetch WebSearch Task; do
  runtool "$t" '{}' allow
done
echo "== Tool dispatch: an unknown tool asks, MCP included =="
runtool "mcp__whatever__do_thing" '{}' ask
runtool "SomeToolFromTheFuture" '{}' ask
echo "== Tool dispatch: a writer with no path to judge asks rather than passing =="
runtool "Write" '{}' ask
runtool "Bash" '{}' ask

# =============================================================================
echo
echo "== Target::is_write_sink =="
run "$A" "$A" 'echo hi > /dev/null' allow
run "$D" "$D" 'echo hi > /dev/null' allow
run "$D" "$D" 'objdump -T x 2>/dev/null' allow

echo "== Target::is_allowed_exception: scratch roots, on any branch =="
run "$D" "$D" 'echo hi > /tmp/scratch-file' allow
run "$D" "$D" 'rm -rf /var/tmp/whatever' allow
runw "$D" "$D" "/tmp/some/new/file.txt" allow

echo "== Target::is_loose: no repository behind it, so nothing to recover from =="
runw "$A" "$A" "$LOOSE/file.txt" deny
runw "$A" "$A" "$LOOSE/brand-new.txt" deny
run "$A" "$A" "rm $LOOSE/file.txt" deny
run "$A" "$A" 'echo hi > /etc/some-config' deny

echo "== Target::is_repo_root: protected on an allowed branch too =="
runw "$A" "$A" "$A" deny
run "$A" "$A" "rm -rf $A" deny
run "$A" "$A" "rm -rf $D" deny

echo "== Target::is_in_foreign_repo: a second checkout is not this project =="
runw "$A" "$A" "$F/tracked.txt" deny
runw "$A" "$A" "$F/brand-new.txt" deny
run "$A" "$A" "echo hi > $F/tracked.txt" deny
run "$A" "$A" "sed -i s/a/b/ $F/tracked.txt" deny
run "$A" "$A" "rm $F/untracked.txt" deny
echo "== ... but its build output is still recoverable =="
runw "$A" "$A" "$F/target/app" allow

echo "== Target::is_tracked_on_allowed_branch =="
runw "$A" "$A" "$A/tracked.txt" allow
runw "$A" "$A" "$A/src/lib.rs" allow
run "$A" "$A" 'echo hi > tracked.txt' allow
run "$A" "$A" 'sed -i s/a/b/ tracked.txt' allow
run "$A" "$A" 'cp /etc/hostname tracked.txt' allow
run "$A" "$A" 'rm -rf src' allow

echo "== Target::is_new_on_allowed_branch =="
runw "$A" "$A" "$A/brand-new.rs" allow
runw "$A" "$A" "$A/nested/deep/brand-new.rs" allow
run "$A" "$A" 'echo hi > brand-new.txt' allow
run "$A" "$A" 'mkdir -p nested/deep' allow

echo "== Target::is_on_disallowed_branch: existing and new alike =="
runw "$D" "$D" "$D/tracked.txt" deny
runw "$D" "$D" "$D/new.txt" deny
runw "$D" "$D" "$D/untracked.txt" deny
run "$D" "$D" 'echo hi > tracked.txt' deny
run "$D" "$D" 'echo hi > brand-new.txt' deny
run "$D" "$D" 'echo hi > untracked.txt' deny
run "$D" "$D" 'sed -i s/a/b/ tracked.txt' deny
run "$D" "$D" 'cp /etc/hostname tracked.txt' deny
run "$D" "$D" 'rm -rf src' deny

echo "== Target::is_under_gitignored_allowed_dir: build output, on any branch =="
runw "$A" "$A" "$A/target/debug/app" allow
runw "$D" "$D" "$D/target/debug/app" allow
runw "$D" "$D" "$D/target/debug/brand-new" allow
run "$D" "$D" 'rm -rf target' allow
run "$A" "$A" 'echo hi > ignored-dir/untracked-but-ignored.md' allow
run "$D" "$D" 'echo hi > ignored-dir/untracked-but-ignored.md' allow

echo "== Target::is_file_pattern_ignored: ignored, existing, irreplaceable =="
runw "$A" "$A" "$A/app.log" deny
run "$A" "$A" 'rm app.log' deny
runw "$D" "$D" "$D/app.log" deny

echo "== A directory that ignores itself from within grants nothing =="
runw "$A" "$A" "$A/data/notes.csv" deny
run "$A" "$A" 'echo x > data/notes.csv' deny
run "$A" "$A" 'rm data/notes.csv' deny

echo "== A directory is judged by what it holds =="
# Removing the directory is no cheaper than removing the files inside it.
run "$A" "$A" 'rm -rf data' deny
# Reproducible contents do not condemn their directory: holds only target/.
run "$A" "$A" 'rm -rf reproducible' allow
# The exception: tracked content takes precedence over what is hidden inside.
run "$A" "$A" 'rm -rf mixed' allow
run "$D" "$D" 'rm -rf mixed' deny
# Naming the files individually reaches the ignored one, and is denied.
run "$A" "$A" 'rm -rf mixed/*' deny

echo "== An existing untracked file is allowed on an allowed branch =="
# Almost always a file the session itself created: the first write to a path
# that did not exist is allowed, and the second finds it existing.
runw "$A" "$A" "$A/untracked.txt" allow
run "$A" "$A" 'echo hi > untracked.txt' allow
run "$A" "$A" 'echo hi >> untracked.txt' allow
run "$A" "$A" 'cp /etc/hostname untracked.txt' allow
run "$A" "$A" 'rm untracked.txt' allow
echo "== ... but the branch still decides first =="
runw "$D" "$D" "$D/untracked.txt" deny
run "$D" "$D" 'rm untracked.txt' deny

echo "== One protected target condemns the whole command =="
run "$A" "$A" 'sed -i s/a/b/ tracked.txt untracked.txt' allow
run "$A" "$A" "sed -i s/a/b/ tracked.txt $LOOSE/file.txt" deny
run "$A" "$A" 'rm tracked.txt target/debug/app' allow

# =============================================================================
echo
echo "== Repo::allowed_branches is a union, not a replacement =="
runw "$B" "$B" "$B/tracked.txt" allow
runw "$E" "$E" "$E/tracked.txt" allow
echo "== Repo::is_allowed_branch: a detached HEAD is never allowed =="
runw "$H" "$H" "$H/tracked.txt" deny
run "$H" "$H" 'git commit -m x' deny

echo "== CLAUDE_CONFIG_DIR is a lane of its own, judged on its own branch =="
runw "$CH" "$A" "$CH/projects/someproj/memory/MEMORY.md" allow
runw "$CH" "$A" "$CH/projects/someproj/memory/brand-new.md" allow
runw "$CH" "$A" "$CH/CLAUDE.md" deny
runw "$CH" "$A" "$CH/settings.local.json" deny
# Ignored, but under no declared directory: protected, not reproducible.
runw "$CH" "$A" "$CH/undeclared/note.md" deny
run "$CH" "$A" "sed -i s/a/b/ $CH/CLAUDE.md" deny

# =============================================================================
echo
echo "== Action::Forbidden: irreversible and unreconstructable =="
run "$A" "$A" 'chown root tracked.txt' deny
run "$A" "$A" 'chgrp staff tracked.txt' deny
run "$A" "$A" 'shred tracked.txt' deny
run "$A" "$A" 'echo hi > /tmp/x && chown root /tmp/x' deny
echo "== ... and chmod, ln and touch are deliberately NOT forbidden =="
run "$A" "$A" 'chmod +x tracked.txt' allow
run "$D" "$D" 'chmod +x tracked.txt' deny
run "$A" "$A" 'touch brand-new.txt' allow
run "$A" "$A" 'ln -s /etc/hostname brand-new-link' allow

echo "== Action::Opaque: genuine unknowns ask, and never deny =="
run "$A" "$A" 'rm -rf "$SOMEVAR/dir"' ask
run "$D" "$D" 'rm -rf "$SOMEVAR/dir"' ask
run "$D" "$D" 'cat x.txt > out-$(date +%s).txt' ask
run "$D" "$D" 'cd /tmp && rm -rf $X/build' ask
run "$D" "$D" 'rm -rf `cat list.txt`' ask
run "$A" "$A" "rm -rf 'unbalanced" ask

echo "== Globs are expanded, not treated as unknowns =="
run "$A" "$A" 'rm -rf target/*' allow
run "$D" "$D" 'rm -rf target/*' allow
run "$D" "$D" 'rm -rf /tmp/scratch/*' allow
run "$D" "$D" 'rm -rf *.log' deny
run "$D" "$D" 'rm -rf build/*' deny
run "$D" "$D" 'sed -i s/a/b/ /etc/hosts.d/*.conf' deny
run "$A" "$A" 'rm -rf src/*' allow

echo "== One literal cd rebases relative paths; a subshell is a barrier =="
run "$D" "$D" 'cd /tmp && rm -rf build/*' allow
run "$D" "$D" 'cd /tmp && echo hi > f.txt' allow
run "$D" "$D" '(cd /tmp && echo hi > f.txt) && echo hi > tracked.txt' deny

echo "== Argument roles: a source is a read =="
run "$A" "$A" 'cp tracked.txt /tmp/backup.md' allow
run "$A" "$A" "cp $LOOSE/file.txt /tmp/backup.md" allow
run "$A" "$A" "cp tracked.txt $LOOSE/backup.md" deny
run "$A" "$A" "mv tracked.txt $LOOSE/backup.md" deny

echo "== Quoting: an arrow in a string is not a redirection =="
run "$A" "$A" 'echo "CLAUDE.md: $(wc -c < a) -> $(wc -c < b) chars"' allow
run "$D" "$D" 'echo "size 3 -> 4"' allow
run "$D" "$D" "echo 'git commit here'" allow
run "$D" "$D" 'echo "a; git commit -m x"' allow
run "$D" "$D" 'git log --format=%h#x' allow
run "$D" "$D" 'wc -l < tracked.txt' allow
run "$D" "$D" 'grep -rn pattern src/' allow
run "$D" "$D" 'ls -la' allow
run "$D" "$D" 'cargo build --release' allow
echo "== ... and a real redirection still is one =="
run "$D" "$D" 'echo hi > tracked.txt' deny
run "$D" "$D" 'echo hi 2> tracked.txt' deny
run "$D" "$D" 'echo hi &> tracked.txt' deny
run "$D" "$D" 'echo hi >| tracked.txt' deny
run "$A" "$A" 'echo hi 2>&1' allow

echo "== Heredoc bodies are discarded =="
run "$D" "$D" 'cat <<EOF
echo hi > tracked.txt
EOF' allow
run "$D" "$D" 'cat > tracked.txt <<EOF
hi
EOF' deny

echo "== An interpreter's inline program is opaque, not read-only =="
# The hole this closes: the path lives in the heredoc body, which the scan
# discards, and python3 is not in the role table -- so the write was invisible,
# the segment scanned as read-only, and the branch rule never saw a target to
# refuse. Opaque asks on the allowed branch too: the point is that nobody can
# tell what it writes, which the branch does not change.
run "$D" "$D" 'python3 - <<EOF
open("tracked.txt","w").write("x")
EOF' ask
run "$D" "$D" 'python3 -c "open(1,2)"' ask
run "$A" "$A" 'python3 -c "open(1,2)"' ask
run "$D" "$D" 'node -e "require(1)"' ask
run "$D" "$D" 'bash -c "rm -rf x"' ask
run "$D" "$D" 'perl -e "open F"' ask
run "$D" "$D" 'cat x | python3' ask

echo "== ... but a script FILE stays the documented non-goal =="
run "$D" "$D" 'python3 script.py' allow
run "$D" "$D" 'bash deploy.sh' allow
run "$D" "$D" 'bash -e deploy.sh' allow
run "$D" "$D" 'ruby -c script.rb' allow

echo "== ... and a probe that runs no program is not a program =="
run "$D" "$D" 'python3 --version' allow
run "$D" "$D" 'python3 -V' allow
run "$D" "$D" 'python3 -m pip list' allow

echo "== awk is deliberately out: its program is always inline =="
run "$D" "$D" 'ps aux | awk "{print \$1}"' allow

echo "== perl -i keeps its precise verdict rather than degrading to ask =="
run "$D" "$D" 'perl -pi -e s/a/b/ tracked.txt' deny
run "$A" "$A" 'perl -pi -e s/a/b/ tracked.txt' allow

# =============================================================================
echo
echo "== GitAction::ConfigWrite: denied at every scope, on every branch =="
for c in 'git config --global user.name someone' 'git config --system core.x y' \
         'git config user.email a@b' 'git config --local a.b c' \
         'git config --unset core.foo' 'git config set user.name x' \
         'git config --file /tmp/x.cfg a.b c'; do
  run "$A" "$A" "$c" deny
done
run "$LOOSE" "$A" 'git config --global user.name x' deny
run "$A" "$A" 'echo hi && git config --global user.name x' deny

echo "== ... and config reads are never blocked =="
for c in 'git config --get user.name' 'git config --list' 'git config user.name' \
         'git config get user.name'; do
  run "$A" "$A" "$c" allow
done
run "$D" "$D" "git config --get-regexp 'remote.*' 'url'" allow
run "$A" "$A" 'git status && git config --get user.name' allow

echo "== Remote: the config-writing forms are config writes =="
for c in 'git remote add upstream https://example.com/x.git' \
         'git remote set-url origin https://example.com/y.git' \
         'git remote remove origin' 'git remote rm origin' \
         'git remote rename origin old' 'git remote set-branches origin main'; do
  run "$A" "$A" "$c" deny
done
echo "== ... the reads are reads =="
for c in 'git remote' 'git remote -v' 'git remote --verbose' \
         'git remote show origin' 'git remote get-url origin'; do
  run "$D" "$D" "$c" allow
done
echo "== ... and the ref-moving forms follow the branch =="
run "$D" "$D" 'git remote prune origin' deny
run "$D" "$D" 'git remote update' deny
run "$A" "$A" 'git remote prune origin' allow
run "$A" "$A" 'git remote update' allow

echo "== GitAction::Read: the allowlist, on a branch the session may not change =="
for c in 'git branch --show-current' 'git branch -a' 'git branch --list' \
         'git tag -l' "git tag -l 'v*'" 'git stash list' 'git stash show' \
         'git worktree list' 'git status' 'git log --oneline' 'git diff' \
         'git show HEAD' 'git rev-parse HEAD' 'git ls-files' 'git blame tracked.txt'; do
  run "$D" "$D" "$c" allow
done

echo "== GitAction::StateChange follows the branch =="
for c in 'git commit -m x' 'git branch new' 'git tag v1' 'git stash' 'git push' \
         'git pull' 'git merge other' 'git rebase main' 'git worktree add ../w' \
         'git checkout main' 'git switch main' 'git add -A' 'git fetch' \
         'git submodule update --init' 'git -c user.name=x commit -m y'; do
  run "$D" "$D" "$c" deny
done
for c in 'git commit -m x' 'git branch new' 'git push' 'git merge other' \
         'git add -A' 'git fetch'; do
  run "$A" "$A" "$c" allow
done
run "$A" "$A" 'git -c user.name=x -c user.email=y commit -m z' allow

echo "== ... and it follows the LANE, which closes the cross-repository hole =="
run "$A" "$A" "git -C $F commit -m x" deny
run "$A" "$A" "git -C $D commit -m x" deny
# Not deny: the scanner does not follow --git-dir, so it cannot say which
# repository this lands in. That is an opaque target, and opaque asks.
run "$A" "$A" "git --git-dir=$F/.git commit -m x" ask
run "$A" "$A" "git --work-tree=$F status" ask
run "$A" "$A" "git --git-dir=$F/.git config --global a.b c" deny

echo "== GitAction::Destructive asks on an allowed branch, denies on any other =="
for c in 'git reset --hard' 'git clean -fdx' 'git checkout -- .' 'git branch -D old' \
         'git stash drop' 'git stash clear' 'git push --force' 'git tag -d v1' \
         'git restore .' 'git reflog expire'; do
  run "$A" "$A" "$c" ask
  run "$D" "$D" "$c" deny
done

# =============================================================================
echo
echo "== Both dimensions compose; neither masks the other =="
run "$A" "$A" 'echo hi > /tmp/x && git config --global a.b c' deny
run "$A" "$A" 'git status && echo hi > tracked.txt' allow
run "$A" "$A" 'git status && rm untracked.txt' allow

echo
echo "passed $pass, failed $fail"
if [ "$fail" -ne 0 ]; then
  echo
  echo "failures:"
  for line in "${failed_lines[@]}"; do echo "  $line"; done
fi
[ "$fail" -eq 0 ]
