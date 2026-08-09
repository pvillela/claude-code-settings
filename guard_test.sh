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
  # Present in the working tree but never added: rule 2a's subject.
  echo loose > "$dir/untracked.txt"
  echo loose > "$dir/ignored-dir/untracked-but-ignored.md"
done

# A stand-in for ~/.claude, so the location carve-out can be exercised without
# touching the real one. The guard reads CLAUDE_CONFIG_DIR when it is set.
export CLAUDE_CONFIG_DIR="$TMP/claudehome"
CH="$CLAUDE_CONFIG_DIR"
git init -q -b main "$CH"          # deliberately a DISALLOWED branch
printf '*\n!*/\n!**/\n!.gitignore\n!CLAUDE.md\n' > "$CH/.gitignore"
echo doc > "$CH/CLAUDE.md"
git -C "$CH" add -A
git -C "$CH" -c user.name=t -c user.email=t@t commit -qm init
mkdir -p "$CH/projects/someproj/memory"
echo mem > "$CH/projects/someproj/memory/MEMORY.md"

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
echo "== rule 2a: existing untracked files, refused on EVERY branch =="
runw "$A" "$A/untracked.txt" deny
runw "$D" "$D/untracked.txt" deny
echo "== rule 2a: creating a new file is not covered =="
runw "$A" "$A/brand-new.rs" ALLOW
runw "$A" "$A/nested/deep/brand-new.rs" ALLOW
echo "== rule 2a: tracked files stay editable on an allowed branch =="
runw "$A" "$A/tracked.txt" ALLOW
echo "== rule 2a: being ignored is NOT a licence to overwrite =="
runw "$A" "$A/ignored-dir/untracked-but-ignored.md" deny
runw "$D" "$D/ignored-dir/untracked-but-ignored.md" deny

echo "== rule 4 carve-out: untracked paths under ~/.claude, on a disallowed branch =="
runw "$CH" "$CH/projects/someproj/memory/MEMORY.md" ALLOW
runw "$CH" "$CH/projects/someproj/memory/brand-new.md" ALLOW
runw "$CH" "$CH/settings.local.json" ALLOW
echo "== but TRACKED files under ~/.claude follow the ordinary branch rule =="
runw "$CH" "$CH/CLAUDE.md" deny
echo "== rule 6: outside any repo =="
runw "$TMP" "$TMP/loose.txt" ALLOW

echo "== rule 2a via SHELL: same answers as the file tools, on an allowed branch =="
run "$A" 'echo hi > untracked.txt' deny
run "$A" 'echo hi > ignored-dir/untracked-but-ignored.md' deny
run "$A" 'echo hi > brand-new.txt' ALLOW
run "$A" 'cp /etc/hostname untracked.txt' deny
run "$A" 'rm untracked.txt' deny

# Rule 3, in-place rewrite of a TRACKED file through the shell. Recoverable, so
# the branch rule stays out of it, but it bypasses the PostToolUse formatter --
# asked, not refused. These three asserted ALLOW before the rule existed.
echo "== rule 3: shell rewrites of a TRACKED file ask, on an allowed branch =="
run "$A" 'echo hi > tracked.txt' ask
run "$A" 'sed -i s/a/b/ tracked.txt' ask
run "$A" 'cp /etc/hostname tracked.txt' ask
# A tracked file under ~/.claude is not exempt from the shell path either: the
# carve-out is keyed on untracked, so this falls through to the branch rule and
# is denied, agreeing with the 'Write claudehome/CLAUDE.md' case above. $CH sits
# on main deliberately, so the deny -- not the ask -- is what proves the
# carve-out no longer swallows tracked files.
run "$CH" 'sed -i s/a/b/ CLAUDE.md' deny
# The deny for an untracked target outranks the ask when a command hits both.
run "$A" 'sed -i s/a/b/ tracked.txt untracked.txt' deny

echo "== the file tools agree with the shell on the same paths =="
runw "$A" "$A/untracked.txt" deny
runw "$A" "$A/brand-new.txt" ALLOW
runw "$A" "$A/tracked.txt" ALLOW

echo "== on a disallowed branch every write is still refused =="
run "$D" 'echo hi > brand-new.txt' deny
run "$D" 'echo hi > tracked.txt' deny
run "$D" 'echo hi > untracked.txt' deny
# The in-place rule must never soften this into a prompt: on a disallowed branch
# these are the same three commands that ask on an allowed one, and they are
# refused outright here.
run "$D" 'sed -i s/a/b/ tracked.txt' deny
run "$D" 'cp /etc/hostname tracked.txt' deny

echo "== unresolvable targets ask on an allowed branch =="
run "$A" 'rm -rf "$SOMEVAR/dir"' ask
run "$A" 'cat x.txt > out-$(date +%s).txt' ask
run "$A" 'rm -rf /tmp/scratch/*' ask

# On a disallowed branch an unresolved target is refused, not prompted for:
# approving the prompt would put the write on the protected branch. The escape
# is proof that the targets lie outside the repo, not a plausible-looking path.
echo "== unresolvable targets on a disallowed branch: deny unless provably outside =="
run "$D" 'rm -rf "$SOMEVAR/dir"' deny          # a variable may hold ../..
run "$D" 'cat x.txt > out-$(date +%s).txt' deny # so may a substitution
run "$D" 'rm -rf *.log' deny                    # relative glob: bounded by PWD, in the repo
run "$D" 'rm -rf build/*' deny                  # ditto, one level down
run "$D" 'rm -rf /tmp/scratch/*' ask            # glob cannot escape /tmp
run "$D" 'sed -i s/a/b/ /etc/hosts.d/*.conf' ask
run "$D" 'cd /tmp && rm -rf build/*' ask        # literal cd rebases it outside
run "$D" 'cd /tmp && rm -rf $X/build' deny      # variable defeats the proof anyway

echo "== reads never trip the content rule =="
run "$A" 'ls -la' ALLOW
run "$A" 'grep -rn pattern src/' ALLOW
run "$A" 'cargo build --release' ALLOW
run "$A" 'objdump -T bin 2>/dev/null | head -5' ALLOW
run "$A" 'echo hi > /dev/null' ALLOW
run "$A" 'wc -l < tracked.txt' ALLOW
# An arrow in a quoted string is not a redirection. On a disallowed branch this
# was a deny before the '-' exclusion: the arrow tripped the content rule and the
# substitutions made the targets unresolvable, so a read-only command was
# refused. Both branches, because the regex runs ahead of the branch logic.
run "$A" 'echo "CLAUDE.md: $(wc -c < a) -> $(wc -c < b) chars"' ALLOW
run "$D" 'echo "CLAUDE.md: $(wc -c < a) -> $(wc -c < b) chars"' ALLOW
run "$D" 'echo "size 3 -> 4"' ALLOW
# A real redirection is still caught: only the character before '>' changed.
run "$D" 'echo hi > tracked.txt' deny
run "$A" 'echo hi >> untracked.txt' deny

echo "== rule 3: metadata prompts rather than refusing =="
run "$D" 'chmod +x tracked.txt' ask
run "$A" 'chmod +x tracked.txt' ask

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
