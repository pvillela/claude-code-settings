#!/bin/sh
# PreToolUse shim: hands the payload to the action-guard binary.
#
# The shim exists so that settings.json names a stable path and never a build
# artefact. It does one thing the binary cannot do for itself -- check that the
# binary is there -- and nothing else. In particular it does NOT build: a build
# on the hot path would put cargo between every tool call and its execution.
# `build.sh`, run from the SessionStart hook, keeps the binary current.
#
# A missing binary emits Ask rather than exiting 127. A non-zero exit with no
# output reads to Claude Code as a non-blocking error, which lets the tool call
# proceed -- so exiting on a missing guard would fail open.

# Located relative to this script, not to $CLAUDE_CONFIG_DIR. The two are
# normally the same directory, but CLAUDE_CONFIG_DIR legitimately points
# somewhere else while the suite is running, and the guard must not go missing
# just because the lane it is judging has moved.
BIN="$(cd "$(dirname "$0")" && pwd)/action-rules/target/release/action-guard"

if [ ! -x "$BIN" ]; then
  printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"The action-guard binary is missing or not executable. Run ~/.claude/action-rules/build.sh, then decide this one by hand."}}'
  exit 0
fi

exec "$BIN"
