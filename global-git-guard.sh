#!/bin/bash
# PreToolUse guard: block edits to the working tree unless on an allowed branch.
#
# THE RULES THIS ENFORCES
#
#   1. A branch is allowed if it is listed in .claude/allowed-branches at the
#      root of the repo being touched (one branch per line, '#' comments and
#      surrounding blanks stripped). If that file is missing or unreadable, the
#      single allowed branch is 'aicode'. Note the file is looked up per repo,
#      so each checkout carries its own list.
#
#   2. On a branch that is not allowed, these are refused outright (deny):
#        - Write, Edit, MultiEdit, NotebookEdit to any path in the repo;
#        - a git command that changes repository state;
#        - a shell command that rewrites file content under the repo root.
#
#   3. These prompt the user (ask) rather than being refused:
#        - on a branch that is not allowed, a content-rewriting shell command
#          whose targets cannot be resolved statically, because a 'cd', a
#          variable, a glob or a substitution hides where it would write;
#        - chmod, chown, chgrp, ln and touch, ON ANY BRANCH and any file,
#          allowed branches and non-repo paths included, because git records the
#          executable bit but not ownership, so the damage can be invisible in
#          'git status'. Like rule 5a this runs ahead of the branch logic.
#
#   2a. Also branch-independent: an EXISTING file in the repo that git does not
#      track may not be overwritten, on allowed branches as much as disallowed
#      ones. Git holds no copy, so the overwrite cannot be reviewed or undone.
#      This includes files excluded by .gitignore -- being ignored is not a
#      licence to overwrite, since a project's ignored set routinely holds real
#      data and secrets. Creating a file is not covered: a path that does not
#      exist yet destroys nothing, and every new file is untracked until added.
#
#   4. The test is whether the WORKING TREE is affected, not whether git would
#      notice. A scratch or throwaway file is not exempt, and neither is an
#      ignored one. The single carve-out is keyed on LOCATION: an untracked path
#      under ~/.claude, Claude's own config and memory store, may be written on
#      any branch. Tracked files there are not exempt.
#
#   5. Read-only inspection is never blocked. A guard that refuses
#      'git branch --show-current' would break the very check the caller is
#      required to run before every write.
#
#   5a. One rule ignores the branch entirely: WRITING GIT CONFIG IS ALWAYS
#      DENIED, at every scope (--local, --worktree, --file, --global, --system),
#      on allowed branches as much as disallowed ones, and outside a repo too.
#      This covers 'git remote add/remove/rename/set-url/set-branches' as well,
#      since those edit the same config file by another name.
#      Config is not working-tree state -- it is invisible to status and diff,
#      it survives checkouts, and at global scope it reshapes every repo on the
#      machine -- so no branch makes it reviewable. Reading config is untouched,
#      and 'git -c name=value <command>' still works, since it persists nothing.
#
#   6. Anything outside a git repo is allowed: no working tree, nothing to
#      protect. Switching or creating branches is not this script's job to
#      permit -- that prohibition lives in CLAUDE.md.
#
# Emits the documented PreToolUse schema:
#   {"hookSpecificOutput":{"hookEventName":"PreToolUse",
#                          "permissionDecision":"allow|deny|ask",
#                          "permissionDecisionReason":"..."}}
# Printing nothing and exiting 0 means "no opinion": the normal permission
# system then decides, which is not the same as forcing an allow.

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

# True when one shell segment is a state-changing git invocation (rule 2), false
# when it only reads (rule 5). One segment only -- the caller splits first.
#
# Matching the verb alone is not good enough, for two reasons:
#
#   - Four verbs are overloaded. 'branch', 'tag', 'stash' and 'worktree' each
#     both read and write depending on their arguments, so the decision has to
#     look at the arguments:
#       branch    writes when a branch name is given, or with a flag that
#                 deletes (-d/-D), renames (-m/-M), copies (-c/-C), forces (-f),
#                 retargets upstream (-u/--set-upstream-to/--unset-upstream) or
#                 edits the description. Bare, or with only listing and
#                 filtering flags, it reads.
#       tag       writes when a tag name is given without a listing flag, or
#                 with a flag that deletes, annotates, signs, forces or supplies
#                 a message. 'git tag' bare and 'git tag -l <pattern>' read.
#       stash     reads only as 'stash list' and 'stash show'. Bare 'git stash'
#                 pushes, so it writes.
#       worktree  reads only as 'worktree list'.
#     Flags that consume a value (--contains, --merged, --points-at, --sort,
#     --format, --color) have that value skipped, so it is not mistaken for a
#     branch or tag name.
#
#   - A verb inside some other command is not an invocation. The old regex
#     matched 'git commit' inside an echo or a grep pattern. Requiring the
#     segment to actually begin with git -- after any sudo/env/command/time/
#     nohup wrapper, and after git's own -C/-c options and their values -- fixes
#     that. Note -c before the subcommand is git's config option, while -c after
#     'branch' is copy-branch: position decides, which is why the parse tracks
#     where the subcommand starts.
#
# Unrecognised subcommands return false: this guard is not the last line of
# defence, and rules 2 and 3 still catch content and metadata changes.
# On a match, GIT_MUTATION says which kind it was: 'config' for a config write,
# which is refused on every branch, or 'state' for everything else, which is
# refused only on a branch that is not allowed. Deliberately not local.
git_segment_mutates() {
  local - ; set -f  # a glob in the message must not expand into more arguments
  local seg="$1" tok sub="" seen_git=0 skip=0 listing=0 skipval=0 a
  local -a args=() pos=()
  GIT_MUTATION=state

  for tok in $seg; do
    tok=${tok#[\"\']}; tok=${tok%[\"\']}
    if [ "$skip" = 1 ]; then skip=0; continue; fi
    if [ "$seen_git" = 0 ]; then
      case "$tok" in
        git) seen_git=1 ;;
        sudo|env|command|time|nohup) ;;
        *) return 1 ;; # segment does not invoke git
      esac
      continue
    fi
    if [ -z "$sub" ]; then
      case "$tok" in
        -C|-c) skip=1 ;;  # git's own options that consume a value
        -*) ;;
        *) sub=$tok ;;
      esac
      continue
    fi
    args+=("$tok")
  done
  [ -n "$sub" ] || return 1

  case "$sub" in
    branch)
      # Listing unless a branch is named or a writing flag appears.
      for a in "${args[@]}"; do
        if [ "$skipval" = 1 ]; then skipval=0; continue; fi
        case "$a" in
          -d|-D|-m|-M|-c|-C|-f|-u|--delete|--move|--copy|--force|--set-upstream-to|--unset-upstream|--edit-description)
            return 0 ;;
          --contains|--no-contains|--merged|--no-merged|--points-at|--sort|--format|--color)
            skipval=1 ;;
          -*) ;;
          *) pos+=("$a") ;;
        esac
      done
      [ ${#pos[@]} -gt 0 ] && return 0
      return 1 ;;
    tag)
      for a in "${args[@]}"; do
        if [ "$skipval" = 1 ]; then skipval=0; continue; fi
        case "$a" in
          -d|-D|-a|-s|-f|-m|-F|-u|--delete|--annotate|--sign|--force|--message|--file|--local-user)
            return 0 ;;
          -l|--list|-n|-n[0-9]*) listing=1 ;;
          --contains|--no-contains|--merged|--no-merged|--points-at|--sort|--format|--color)
            skipval=1; listing=1 ;;
          -*) ;;
          *) pos+=("$a") ;;
        esac
      done
      # `git tag` alone lists; `git tag v1.0` creates one.
      [ "$listing" = 0 ] && [ ${#pos[@]} -gt 0 ] && return 0
      return 1 ;;
    config)
      # 'git config' is overloaded the same way, and writes repository state
      # without touching a tracked file, so nothing else here would catch it.
      #
      #   classic form   a name alone prints it and reads; a name plus a value
      #                  sets it and writes. So the count of positionals decides,
      #                  unless a reading flag says otherwise (--get-regexp takes
      #                  a pattern and a value-pattern, two positionals, and
      #                  still only reads).
      #   subcommand     get and list read; set, unset, edit, remove-section and
      #     form         rename-section write. Available since git 2.46.
      #   action flags   --unset, --unset-all, --add, --replace-all,
      #                  --rename-section, --remove-section and --edit always
      #                  write, whatever the positionals look like.
      #
      # Scope does NOT matter: every writing form is refused, --global and
      # --system included. Those two land in ~/.gitconfig and /etc/gitconfig
      # rather than in any checkout, so on a strict reading they are outside
      # this guard's remit -- but they reconfigure identity, hooks, aliases and
      # merge behaviour for every repo at once, which is a wider blast radius
      # than the repo-local write this guard already refuses. Deliberately
      # deciding them is worth one round-trip. Reads are never affected, and
      # nothing here applies on an allowed branch.
      #
      # --file and --blob take an arbitrary target that cannot be resolved
      # reliably here, and are treated as writing: over-blocking is the safe
      # direction.
      local writes=0 reads=0
      GIT_MUTATION=config
      for a in "${args[@]}"; do
        if [ "$skipval" = 1 ]; then skipval=0; continue; fi
        case "$a" in
          --unset|--unset-all|--add|--replace-all|--rename-section|--remove-section|--edit|-e)
            writes=1 ;;
          --get|--get-all|--get-regexp|--get-urlmatch|--list|-l|--get-color|--get-colorbool)
            reads=1 ;;
          --file|-f|--blob|--type|-t|--default) skipval=1 ;;
          -*) ;;
          *) pos+=("$a") ;;
        esac
      done
      case "${pos[0]:-}" in
        get|list) ;;
        set|unset|edit|remove-section|rename-section) writes=1 ;;
        *) [ "$reads" = 0 ] && [ ${#pos[@]} -ge 2 ] && writes=1 ;;
      esac
      [ "$writes" = 1 ] && return 0
      return 1 ;;
    remote)
      # 'git remote' is overloaded too, and its writing forms edit the
      # [remote "..."] section of .git/config exactly as 'git config' does, so
      # they inherit the same unconditional refusal rather than the branch rule.
      #
      #   reads    bare (lists names), -v/--verbose, 'show', 'get-url'
      #   config   add, remove/rm, rename, set-url, set-branches -- these
      #            rewrite config, are invisible to status and diff, and can
      #            silently repoint a push at a different server
      #   state    prune, update, set-head -- these move refs rather than
      #            config, so the ordinary branch rule covers them
      #
      # Leading flags are skipped so that 'git remote -v' finds no subcommand
      # and reads. An unrecognised subcommand is assumed to write.
      local sc=""
      for a in "${args[@]}"; do
        case "$a" in -*) ;; *) sc=$a; break ;; esac
      done
      case "$sc" in
        ""|show|get-url) return 1 ;;
        add|remove|rm|rename|set-url|set-branches) GIT_MUTATION=config; return 0 ;;
        *) return 0 ;;
      esac ;;
    stash)
      case "${args[0]:-}" in list|show) return 1 ;; *) return 0 ;; esac ;;
    worktree)
      case "${args[0]:-}" in list) return 1 ;; *) return 0 ;; esac ;;
    commit|merge|rebase|push|pull|checkout|switch|restore|reset|revert|cherry-pick|apply|clean|rm|mv)
      return 0 ;;
    *) return 1 ;;
  esac
}

git_command_mutates() {
  local seg
  while IFS= read -r seg; do
    git_segment_mutates "$seg" && return 0
  done <<EOF
$(echo "$1" | tr ';|&' '\n')
EOF
  return 1
}

# True when any one segment writes git config. Checked per segment rather than
# reusing git_command_mutates, so that a config write is still caught when it
# shares a command line with an ordinary state change that matched first.
git_command_writes_config() {
  local seg
  while IFS= read -r seg; do
    if git_segment_mutates "$seg" && [ "$GIT_MUTATION" = config ]; then return 0; fi
  done <<EOF
$(echo "$1" | tr ';|&' '\n')
EOF
  return 1
}

# Writing git config is refused unconditionally: on allowed branches as well as
# disallowed ones, and outside a repo entirely. This is the one rule that is not
# about the working tree, which is why it sits ahead of the branch logic below
# rather than inside it -- there is no branch that makes it acceptable.
#
# The reasoning: config decides identity, hooks, aliases, merge and push
# behaviour. A change there is silent, survives every checkout, is invisible in
# 'git status' and 'git diff', and at --global or --system scope reaches every
# repository on the machine. None of that is reviewable the way a working-tree
# edit is, so it is the user's call every time.
#
# Reading config is untouched. So is 'git -c name=value <cmd>', which applies a
# setting to a single invocation and persists nothing -- that is the intended
# way to supply an identity for one commit without reconfiguring anything.
if [ "$TOOL_NAME" = "Bash" ] && git_command_writes_config "$COMMAND"; then
  emit deny "Blocked: this writes git config, which is refused on every branch, including allowed ones, because it silently changes behaviour for future work and is invisible to git status. Reading config is fine, and 'git -c name=value <command>' applies a setting for one invocation without persisting it. If the setting really should persist, ask the user to make the change."
fi

# Rule 3, metadata. Like the config rule above, this sits ahead of the branch
# logic because it applies on any branch and to any file, allowed branches and
# non-repo paths included. It used to live after the allowed-branch check, which
# made it unreachable exactly where it was most needed: a chmod on an allowed
# branch went through silently.
#
# Git records the executable bit but not ownership, so a chown can make a file
# unreadable without ever showing up in 'git status'. The decision is 'ask' and
# not 'deny' because these commands are often legitimate -- the point is that
# they are seen, not that they are refused.
if [ "$TOOL_NAME" = "Bash" ] &&
   echo "$COMMAND" | grep -qE '(^|[;&|[:space:]])(chmod|chown|chgrp|ln|touch)[[:space:]]'; then
  emit ask "This command changes file metadata (permissions, ownership, links or timestamps), which git may not record -- a chown leaves no trace in git status. This is asked on every branch, allowed ones included. Approve only if you intend it."
fi

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

# The one carve-out, and it is keyed on LOCATION, not on ignore status: an
# untracked path under ~/.claude may be written on any branch.
#
# That directory is Claude's own config and memory store. It happens to be a git
# checkout, so without this every memory write is judged against whatever branch
# that checkout sits on -- which has nothing to do with the memory being written.
# Tracked files there (CLAUDE.md, settings.json, this script) are NOT exempt:
# they fall through to the ordinary branch rule, so editing them still requires
# an allowed branch.
#
# An earlier version keyed this on 'git check-ignore' instead, exempting any
# ignored path in any repo. That was far too wide: a project's ignored set
# routinely holds real data and secrets -- spreadsheets under data/, API keys in
# .devcontainer/devcontainer.env -- and exempting those meant they could be
# overwritten silently, with no diff and nothing to recover from. Ignored files
# outside ~/.claude are now covered by rule 2a like any other untracked file.
if [ -n "$FILE_PATH" ]; then
  CLAUDE_HOME=$(realpath -m -- "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" 2>/dev/null)
  FILE_ABS=$(realpath -m -- "$FILE_PATH" 2>/dev/null)
  case "$FILE_ABS/" in
    "$CLAUDE_HOME"/*)
      git -C "$CONTEXT_DIR" ls-files --error-unmatch -- "$FILE_PATH" >/dev/null 2>&1 || allow
      ;;
  esac
fi

# Rule 2a: an EXISTING file inside the repo that git does not track may not be
# overwritten, on any branch, allowed ones included. Git holds no copy of it, so
# an overwrite is unrecoverable -- no diff to review, nothing to check out, no
# earlier version anywhere. A tracked file is the opposite: every previous state
# is retrievable, which is what makes editing it safe.
#
# CREATING a file is not covered. A path that does not exist yet destroys
# nothing, and every new file is untracked until it is added, so refusing those
# would mean never being able to add a source file, test or document.
#
# Ignored files are untracked, so they are covered here too: being listed in
# .gitignore is not a licence to overwrite. That is the point of keying the
# carve-out above on location instead -- data/*.xlsx and devcontainer.env are
# ignored, and must still be protected.
#
# Order matters twice over. This runs after the ~/.claude carve-out above, so
# the memory store stays writable. And it runs before the allowed-branch check
# below, because being on an allowed branch does not give git a copy of the file.
if [ -n "$FILE_PATH" ] && [ -e "$FILE_PATH" ] &&
   ! git -C "$CONTEXT_DIR" ls-files --error-unmatch -- "$FILE_PATH" >/dev/null 2>&1; then
  emit deny "Blocked: '$FILE_PATH' exists under $TOPLEVEL but is not tracked by git, so overwriting it cannot be reviewed or undone -- there is no committed version to fall back on. This applies on every branch. Ask the user whether to track it first, or leave it alone."
fi

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
    if git_command_mutates "$COMMAND"; then
      emit deny "Blocked: state-changing git command on branch '$CURRENT_BRANCH', which is not an allowed branch ($ALLOWED_LIST)."
    fi

    CONTENT_RE='(^|[;&|[:space:]])(rm|mv|cp|dd|truncate|tee|install|shred)[[:space:]]|sed[[:space:]]+[^|]*-i|[^0-9>]>>?[[:space:]]*[^&[:space:]]'

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

    # The metadata check that used to sit here now runs before the branch logic
    # above, so that it applies on allowed branches too. Nothing replaces it.
    ;;
esac

allow
