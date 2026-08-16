#!/usr/bin/env bash
# SessionStart hook: report files that exist, are untracked, and are not
# gitignored, in the two repositories the session governs.
#
# Why this exists. `Target::is_untracked_on_allowed_branch` allows writes to
# such files without asking, because almost all of them are files the session
# itself created and asking about them is noise. The exception is a file you
# wrote by hand and have not committed: it is in the same population, and the
# guard cannot tell the two apart. Listing them once, at the start, is what
# makes that a choice you made rather than one assumed on your behalf.
#
# Reads the hook payload on stdin and ignores it. Prints plain text on stdout,
# which is what Claude Code adds to the session context; it prints nothing when
# there is nothing to report. Always exits 0: a hook that fails must not stop a
# session from starting.

set -uo pipefail

list_uncommitted() { # <repo dir> <label>
  local dir=$1 label=$2 root files
  [ -n "$dir" ] || return 0
  [ -d "$dir" ] || return 0
  root=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || return 0

  files=$(git -C "$root" ls-files --others --exclude-standard 2>/dev/null | head -40)
  [ -n "$files" ] || return 0

  printf '%s (%s), on branch %s:\n' "$root" "$label" \
    "$(git -C "$root" branch --show-current 2>/dev/null || echo 'detached HEAD')"
  printf '%s\n' "$files" | sed 's/^/  /'
  printf '\n'
}

report=$(
  list_uncommitted "${CLAUDE_PROJECT_DIR:-}" "the launch project"
  list_uncommitted "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" "the Claude config directory"
)

[ -n "$report" ] || exit 0

cat <<EOF
The action guard allows writes to existing untracked files without asking, on
the assumption that they are files the session created. These files existed
before this session started, so that assumption does not hold for them. They are
untracked and not gitignored, so git holds no copy: overwriting or deleting one
loses its contents for good.

$report

Before doing work that could touch any of them, raise this with the user. Offer
to commit them, and otherwise get explicit confirmation to proceed with them
unprotected. Do not repeat this if the user has already answered.
EOF

exit 0
