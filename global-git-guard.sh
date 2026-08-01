#!/bin/bash
# PreToolUse guard: block edits to the working tree unless on an allowed branch.
#
# Emits the documented PreToolUse schema:
#   {"hookSpecificOutput":{"hookEventName":"PreToolUse",
#                          "permissionDecision":"allow|deny|ask",
#                          "permissionDecisionReason":"..."}}
#
# Allowed branches are read from .claude/allowed-branches at the repo root
# (one branch per line, # comments ignored). Falls back to 'aicode'.

INPUT=$(cat)

TOOL_NAME=$(echo "$INPUT" | jq -r '.tool_name // empty')
FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // .tool_input.notebook_path // empty')
COMMAND=$(echo "$INPUT" | jq -r '.tool_input.command // empty')

emit() { # emit <decision> <reason>
  jq -cn --arg d "$1" --arg r "$2" \
    '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:$d,permissionDecisionReason:$r}}'
  exit 0
}

allow() { exit 0; } # silent: defer to the normal permission system

# Resolve the repo relative to the file being touched, not just the CWD, so a
# Write to another checkout is judged against that checkout's branch.
CONTEXT_DIR="$PWD"
if [ -n "$FILE_PATH" ]; then
  d=$(dirname "$FILE_PATH")
  while [ ! -d "$d" ] && [ "$d" != "/" ]; do d=$(dirname "$d"); done
  CONTEXT_DIR="$d"
fi

TOPLEVEL=$(git -C "$CONTEXT_DIR" rev-parse --show-toplevel 2>/dev/null) || allow
CURRENT_BRANCH=$(git -C "$CONTEXT_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)

ALLOWED_FILE="$TOPLEVEL/.claude/allowed-branches"
if [ -r "$ALLOWED_FILE" ]; then
  ALLOWED=$(sed 's/#.*//' "$ALLOWED_FILE" | tr -d '[:blank:]' | grep -v '^$')
else
  ALLOWED="aicode"
fi

if printf '%s\n' "$ALLOWED" | grep -qxF "$CURRENT_BRANCH"; then
  allow
fi

ALLOWED_LIST=$(printf '%s' "$ALLOWED" | paste -sd, -)

case "$TOOL_NAME" in
  Write|Edit|MultiEdit|NotebookEdit)
    emit deny "Blocked: branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST). Do not edit files here, and do not switch branches yourself — stop and ask the user."
    ;;
  Bash)
    if echo "$COMMAND" | grep -qE '(^|[;&|[:space:]])git([[:space:]]+-[^[:space:]]+)*[[:space:]]+(commit|merge|rebase|push|pull|checkout|switch|restore|branch|reset|revert|cherry-pick|stash|apply|clean|rm|mv|tag|worktree)\b'; then
      emit deny "Blocked: state-changing git command on branch '$CURRENT_BRANCH', which is not an allowed branch ($ALLOWED_LIST)."
    fi

    CONTENT_RE='(^|[;&|[:space:]])(rm|mv|cp|dd|truncate|tee|install|shred)[[:space:]]|sed[[:space:]]+[^|]*-i|[^0-9>]>>?[[:space:]]*[^&[:space:]]'
    META_RE='(^|[;&|[:space:]])(chmod|chown|chgrp|ln|touch)[[:space:]]'

    # Does any argument resolve to a path inside the repo? Command names and
    # flag values are skipped; a token counts only if it looks like a path or
    # actually exists, so 'install -m 755 ...' is not fooled by the mode.
    targets_repo() {
      local seg tok first resolved
      # 'cd' can move the base of every relative path -- treat as unresolvable.
      echo "$COMMAND" | grep -qE '(^|[;&|[:space:]])cd[[:space:]]' && return 2
      # Substitutions and globs hide their targets from static inspection.
      echo "$COMMAND" | grep -qE '\$\(|`|\$[A-Za-z_{]|\*|\?' && return 2
      local is_sed
      while IFS= read -r seg; do
        first=1
        is_sed=0
        echo "$seg" | grep -qE '(^|[[:space:]])sed([[:space:]]|$)' && is_sed=1
        for tok in $(echo "$seg" | tr '<>' '  '); do
          tok=${tok#[\"\']}; tok=${tok%[\"\']}
          # a sed script ('s/a/b/', '1,$d') is not a path despite the slashes
          if [ $is_sed = 1 ]; then
            case "$tok" in [0-9,\$]*s[/,\|:#]*|s[/,\|:#]*) continue ;; esac
          fi
          # skip the command word and common wrappers
          if [ $first = 1 ]; then
            case "$tok" in sudo|env|command|xargs|time|nohup) continue ;; esac
            first=0; continue
          fi
          case "$tok" in -*) continue ;; esac
          # ignore bare words that are neither path-like nor existing files
          case "$tok" in
            */*|*.*) ;;
            *) [ -e "$tok" ] || continue ;;
          esac
          resolved=$(realpath -m -- "$tok" 2>/dev/null) || continue
          case "$resolved/" in "$TOPLEVEL"/*) return 0 ;; esac
        done
      done <<EOF
$(echo "$COMMAND" | tr ';|&' '\n')
EOF
      return 1
    }

    if echo "$COMMAND" | grep -qE "$CONTENT_RE"; then
      targets_repo; hit=$?
      case $hit in
        0) emit deny "Blocked: this command changes file content under $TOPLEVEL, and branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST)." ;;
        2) emit ask "Branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST). This command modifies files and its targets could not be resolved statically. Approve only if it touches nothing in $TOPLEVEL." ;;
      esac
    fi

    if echo "$COMMAND" | grep -qE "$META_RE"; then
      emit ask "Branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST). This command changes file metadata (permissions, ownership, links or timestamps), which git may not show. Approve only if you intend it."
    fi
    ;;
esac

allow
