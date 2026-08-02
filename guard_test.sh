#!/bin/bash
# Test suite for global-git-guard.sh.
#
# Builds two throwaway repos in a temp dir -- one on a disallowed branch, one on
# an allowed one -- feeds PreToolUse JSON to the guard, and checks the decision.
# Self-contained: it depends on nothing outside $TMPDIR and the guard itself, so
# it can run on any machine and cleans up after itself.
#
# Run it after any change to the guard:   bash ~/.claude/guard_test.sh
#
# Note the assertions pass git strings as DATA. The guard cannot tell those from
# real invocations when they follow a ';', '&&' or '|', which is why they live
# in this file rather than being typed at a shell: the guard sees only the
# "bash guard_test.sh" that starts it.

G="${GUARD:-$HOME/.claude/global-git-guard.sh}"
[ -r "$G" ] || { echo "guard not found: $G" >&2; exit 2; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# D = disallowed branch, A = allowed branch. 'aicode' is the guard's fallback
# when a repo has no .claude/allowed-branches, so B needs no such file.
D="$TMP/disallowed"
A="$TMP/allowed"
for pair in "$D:main" "$A:aicode"; do
  dir=${pair%:*}; br=${pair#*:}
  git init -q -b "$br" "$dir"
  printf 'ignored-dir/\n*.log\n' > "$dir/.gitignore"
  mkdir -p "$dir/ignored-dir"
  echo hi > "$dir/tracked.txt"
  git -C "$dir" add -A
  git -C "$dir" -c user.name=t -c user.email=t@t commit -qm init
done

pass=0; fail=0

decide() { # decide <json> -> prints allow|deny|ask|ALLOW
  local out d
  out=$(printf '%s' "$1" | bash "$G" 2>&1)
  d=$(printf '%s' "$out" | jq -r '.hookSpecificOutput.permissionDecision // "ALLOW"' 2>/dev/null)
  [ -z "$d" ] && d=ALLOW
  printf '%s' "$d"
}

run() { # run <dir> <command> <expected>
  cd "$1" || return
  local d; d=$(decide "$(jq -cn --arg c "$2" '{tool_name:"Bash",tool_input:{command:$c}}')")
  if [ "$d" = "$3" ]; then pass=$((pass+1)); printf '  ok    %-46s %s\n' "$2" "$d"
  else fail=$((fail+1)); printf '  FAIL  %-46s got %s want %s\n' "$2" "$d" "$3"; fi
}

runw() { # runw <dir> <file_path> <expected>
  cd "$1" || return
  local d; d=$(decide "$(jq -cn --arg f "$2" '{tool_name:"Write",tool_input:{file_path:$f}}')")
  if [ "$d" = "$3" ]; then pass=$((pass+1)); printf '  ok    Write %-40s %s\n' "${2#$TMP/}" "$d"
  else fail=$((fail+1)); printf '  FAIL  Write %-40s got %s want %s\n' "${2#$TMP/}" "$d" "$3"; fi
}

echo "== rule 5a: config writes denied even on an ALLOWED branch =="
run "$A" 'git config --global user.name someone' deny
run "$A" 'git config --system core.x y' deny
run "$A" 'git config user.email a@b' deny
run "$A" 'git config --local a.b c' deny
run "$A" 'git config --unset core.foo' deny
run "$A" 'git config set user.name x' deny
run "$A" 'git config --file /tmp/x.cfg a.b c' deny

echo "== rule 5a: and outside a repo entirely =="
run "$TMP" 'git config --global user.name x' deny

echo "== config reads are never blocked =="
run "$A" 'git config --get user.name' ALLOW
run "$A" 'git config --list' ALLOW
run "$A" 'git config user.name' ALLOW
run "$A" 'git config get user.name' ALLOW
run "$D" "git config --get-regexp 'remote.*' 'url'" ALLOW

echo "== remote: config-writing forms denied on an allowed branch =="
run "$A" 'git remote add upstream https://example.com/x.git' deny
run "$A" 'git remote set-url origin https://example.com/y.git' deny
run "$A" 'git remote remove origin' deny
run "$A" 'git remote rm origin' deny
run "$A" 'git remote rename origin old' deny
run "$A" 'git remote set-branches origin main' deny

echo "== remote: reads allowed even on a disallowed branch =="
run "$D" 'git remote' ALLOW
run "$D" 'git remote -v' ALLOW
run "$D" 'git remote --verbose' ALLOW
run "$D" 'git remote show origin' ALLOW
run "$D" 'git remote get-url origin' ALLOW

echo "== remote: ref-moving forms follow the ordinary branch rule =="
run "$D" 'git remote prune origin' deny
run "$D" 'git remote update' deny
run "$A" 'git remote prune origin' ALLOW
run "$A" 'git remote update' ALLOW

echo "== the escape hatch: -c persists nothing, so it stays allowed =="
run "$A" 'git -c user.name=x -c user.email=y commit -m z' ALLOW

echo "== a config write hidden mid-command is still caught =="
run "$A" 'echo hi && git config --global user.name x' deny
run "$A" 'git status && git config --get user.name' ALLOW

echo "== rule 5: read-only git on a disallowed branch =="
for c in 'git branch --show-current' 'git branch -a' 'git branch --list' \
         'git tag -l' "git tag -l 'v*'" 'git stash list' 'git stash show' \
         'git worktree list' 'git status' 'git log --oneline' 'git diff' \
         "echo 'git commit here'"; do run "$D" "$c" ALLOW; done

echo "== rule 2: state-changing git on a disallowed branch =="
for c in 'git commit -m x' 'git branch new' 'git branch -D old' 'git tag v1' \
         'git tag -d v1' 'git stash' 'git push' 'git pull' 'git merge other' \
         'git rebase main' 'git worktree add ../w' 'git reset --hard' \
         'git checkout main' 'git -c user.name=x commit -m y'; do run "$D" "$c" deny; done

echo "== rule 1: the same state changes pass on an allowed branch =="
for c in 'git commit -m x' 'git branch new' 'git push' 'git merge other'; do run "$A" "$c" ALLOW; done

echo "== rules 2 and 4: file writes =="
runw "$D" "$D/tracked.txt" deny
runw "$D" "$D/new.txt" deny
runw "$A" "$A/tracked.txt" ALLOW
echo "== rule 4 carve-out: paths the repo ignores =="
runw "$D" "$D/ignored-dir/memory.md" ALLOW
runw "$D" "$D/debug.log" ALLOW
echo "== rule 6: outside any repo =="
runw "$TMP" "$TMP/loose.txt" ALLOW

echo "== rule 3: metadata prompts rather than refusing =="
run "$D" 'chmod +x tracked.txt' ask
run "$A" 'chmod +x tracked.txt' ask

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
