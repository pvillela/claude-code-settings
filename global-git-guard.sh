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
#        - a shell command that rewrites file content under the repo root;
#        - a shell command whose write targets cannot be resolved statically and
#          cannot be proven to lie outside the repo. See rule 3.
#
#   3. These prompt the user (ask) rather than being refused:
#        - a content-rewriting shell command whose targets cannot be resolved
#          statically, because a 'cd', a variable, a glob or a substitution
#          hides where it would write. This asks only on an ALLOWED branch. On
#          one that is not allowed it is DENIED, unless the targets can be
#          PROVEN to lie outside the repo -- an unresolved target must not turn
#          into a prompt that puts a write on a protected branch, and "I could
#          not parse this" is not the same as "I can say nothing about it".
#          What is and is not provable is set out at provably_outside_repo.
#        - chmod, chown, chgrp, ln and touch, ON ANY BRANCH and any file,
#          allowed branches and non-repo paths included, because git records the
#          executable bit but not ownership, so the damage can be invisible in
#          'git status'. Like rule 5a this runs ahead of the branch logic.
#        - on an ALLOWED branch, a shell command that rewrites an existing
#          TRACKED file in place -- 'sed -i', a redirection onto it, tee, cp
#          over it. The write is recoverable, so rule 2 has nothing to say about
#          it, but it bypasses the PostToolUse formatter, which keys on the
#          file_path of an Edit/Write call and cannot see a file rewritten by a
#          shell command. Asked and not refused, because a bulk mechanical
#          rewrite across many files is a legitimate use of sed. On a branch
#          that is not allowed the rule 2 deny still fires and takes precedence.
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
CLAUDE_HOME=$(realpath -m -- "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" 2>/dev/null)
if [ -n "$FILE_PATH" ]; then
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
# Computed here rather than after the branch check below, because the
# unresolvable-target deny names it too and runs earlier.
ALLOWED_LIST=$(printf '%s' "$ALLOWED" | paste -sd, -)

# Shell commands that rewrite file content: the writing utilities, an in-place
# sed, and any output redirection. The leading class excludes a digit, so
# '2>/dev/null' does not match; a '>', so '>>' is handled by the quantifier
# rather than matched twice; and a '-', so an arrow inside a quoted string --
# 'echo "a -> b"' -- is not mistaken for a redirection.
#
# That last exclusion is a targeted fix, not a general one. A '>' inside quotes
# is still a redirection to this regex in every other form ('=>', '<>'), because
# telling the two apart needs shell-aware parsing rather than a pattern. It
# earns its place because the arrow is the case that actually turns up, and
# because a false positive here is now a deny rather than a prompt on a branch
# that is not allowed.
CONTENT_RE='(^|[;&|[:space:]])(rm|mv|cp|dd|truncate|tee|install|shred)[[:space:]]|sed[[:space:]]+[^|]*-i|[^0-9>-]>>?[[:space:]]*[^&[:space:]]'

# Collects the arguments that resolve to paths inside the repo into REPO_TARGETS.
# Command names and flag values are skipped, and a token counts only if it looks
# like a path or actually exists, so 'install -m 755 ...' is not fooled by the
# mode. Returns 0 when at least one target was found, 1 when none were, and 2
# when the command hides its targets from static inspection.
REPO_TARGETS=()
INPLACE_TRACKED=""
collect_repo_targets() {
  local seg tok first resolved is_sed found=0
  # 'cd' can move the base of every relative path -- treat as unresolvable.
  echo "$COMMAND" | grep -qE '(^|[;&|[:space:]])cd[[:space:]]' && return 2
  # Substitutions and globs hide their targets from static inspection.
  echo "$COMMAND" | grep -qE '\$\(|`|\$[A-Za-z_{]|\*|\?' && return 2
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
      case "$resolved/" in "$TOPLEVEL"/*) REPO_TARGETS+=("$resolved"); found=1 ;; esac
    done
  done <<EOF
$(echo "$COMMAND" | tr ';|&' '\n')
EOF
  [ "$found" = 1 ] && return 0
  return 1
}

# True when every write target of an otherwise unresolvable command is PROVABLY
# outside $TOPLEVEL. Used only on a branch that is not allowed, where "I could
# not parse this" must not become "approve it and write to a protected branch".
# Giving up on resolving a target is not the same as being unable to say
# anything about it, and the difference is what decides deny versus ask.
#
# What can be proved, and what cannot:
#
#   $VAR, $(...), `...`   Nothing can be proved. A variable may hold '../..', so
#                         even '/tmp/$X' can name a file inside the repo. Any
#                         token carrying one is unprovable, and so is the whole
#                         command -- there is no safe partial credit here.
#   * and ?               Bounded. A glob never matches '/', so the literal text
#                         before the first glob character contains every
#                         expansion: '/tmp/*.log' cannot escape /tmp.
#   cd <literal>          Rebases the relative targets. One literal cd is worth
#                         following; a second, or one hiding a glob, is not.
#
# 'set -f' matters: without it the token loop would expand the very globs it is
# trying to reason about, against whatever PWD happens to be.
#
# Known limit, accepted deliberately: a glob may match a symlink pointing back
# into the repo, and an in-place rewrite would follow it. Refusing every command
# containing an asterisk costs more than that case is worth.
provably_outside_repo() {
  local - ; set -f
  local seg tok base first is_sed lit resolved
  echo "$COMMAND" | grep -qE '\$\(|`|\$[A-Za-z_{]' && return 1

  base="$PWD"
  if echo "$COMMAND" | grep -qE '(^|[;&|[:space:]])cd[[:space:]]'; then
    [ "$(echo "$COMMAND" | grep -cE '(^|[;&|[:space:]])cd[[:space:]]')" -eq 1 ] || return 1
    base=$(echo "${COMMAND//[;&|]/ }" | awk '{for(i=1;i<=NF;i++) if($i=="cd"){print $(i+1); exit}}')
    base=${base#[\"\']}; base=${base%[\"\']}
    [ -n "$base" ] || return 1
    case "$base" in *[\*\?]*) return 1 ;; esac
    case "$base" in /*) ;; *) base="$PWD/$base" ;; esac
  fi

  while IFS= read -r seg; do
    first=1
    is_sed=0
    echo "$seg" | grep -qE '(^|[[:space:]])sed([[:space:]]|$)' && is_sed=1
    for tok in $(echo "$seg" | tr '<>' '  '); do
      tok=${tok#[\"\']}; tok=${tok%[\"\']}
      if [ $is_sed = 1 ]; then
        case "$tok" in [0-9,\$]*s[/,\|:#]*|s[/,\|:#]*) continue ;; esac
      fi
      if [ $first = 1 ]; then
        case "$tok" in sudo|env|command|xargs|time|nohup|cd) continue ;; esac
        first=0; continue
      fi
      case "$tok" in -*) continue ;; esac
      case "$tok" in */*|*.*|*[\*\?]*) ;; *) [ -e "$tok" ] || continue ;; esac
      # Bound the token by the literal directory before its first glob character.
      lit=${tok%%[\*\?]*}
      case "$lit" in */*) lit=${lit%/*} ;; *) lit="." ;; esac
      case "$lit" in /*) ;; *) lit="$base/$lit" ;; esac
      resolved=$(realpath -m -- "$lit" 2>/dev/null) || return 1
      case "$resolved/" in "$TOPLEVEL"/*) return 1 ;; esac
    done
  done <<EOF
$(echo "$COMMAND" | tr ';|&' '\n')
EOF
  return 0
}

# Rule 2a for shell commands. The file tools are checked by path further up;
# this applies the same rule to 'echo > f', 'cp', 'sed -i' and friends, and like
# rule 2a it runs on EVERY branch. The classification is deliberately identical
# to the file-tool one, so that the same write is not permitted through one tool
# and refused through the other:
#
#   target does not exist   creating something, so nothing is destroyed. Left to
#                           the branch rule below, exactly as a Write to a new
#                           path is.
#   exists and untracked    refused on any branch: git holds no copy.
#   exists and tracked      left to the branch rule below: recoverable.
#
# A target that cannot be resolved is asked about rather than refused, because
# it may well lie outside the repo. Measured against a session's worth of real
# commands this fires on roughly one in twenty, and on none that only read.
if [ "$TOOL_NAME" = "Bash" ] && echo "$COMMAND" | grep -qE "$CONTENT_RE"; then
  collect_repo_targets; content_hit=$?
  if [ "$content_hit" = 2 ]; then
    # On a branch that is not allowed, no write under the repo root is
    # permitted, and an unresolved target is not a licence to prompt for one --
    # approving the prompt would put the write on the protected branch. So the
    # answer here is deny unless the targets can be PROVEN to lie outside the
    # repo, which is the one case where the branch has no bearing at all.
    if ! printf '%s\n' "$ALLOWED" | grep -qxF "$CURRENT_BRANCH" && ! provably_outside_repo; then
      emit deny "Blocked: this command modifies files, its targets could not be resolved statically -- a variable, substitution, glob or 'cd' hides where it would write -- and branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST). It cannot be shown to write only outside $TOPLEVEL, and on this branch that is what it would have to show. Rewrite it with literal paths, or ask the user to switch branches."
    fi
    emit ask "This command modifies files and its targets could not be resolved statically -- a variable, glob, substitution or 'cd' hides where it would write. Approve only if it touches nothing under $TOPLEVEL that git does not track."
  fi
  for t in "${REPO_TARGETS[@]}"; do
    tracked=0
    git -C "$CONTEXT_DIR" ls-files --error-unmatch -- "$t" >/dev/null 2>&1 && tracked=1
    # The ~/.claude carve-out applies here too, so a shell write to the memory
    # store behaves the same as a Write to it. It is keyed on UNTRACKED, exactly
    # as the file-tool carve-out further up is: a tracked file there (CLAUDE.md,
    # settings.json, this script) is not exempt and falls through to the checks
    # below like any other tracked file. Before the in-place rule was added this
    # distinction made no difference, since a tracked file never trips the
    # untracked deny -- a blanket 'continue' was equivalent, and is not now.
    case "$t/" in "$CLAUDE_HOME"/*) [ "$tracked" = 0 ] && continue ;; esac
    if [ -e "$t" ] && [ "$tracked" = 0 ]; then
      emit deny "Blocked: this command writes to '$t', which exists under $TOPLEVEL but is not tracked by git, so the change cannot be reviewed or undone -- there is no committed version to fall back on. This applies on every branch. Ask the user whether to track it first, or leave it alone."
    fi
    # Rule 3, in-place rewrite of a tracked file. Recorded rather than emitted
    # here, for two reasons: an untracked target later in the list must still
    # produce the stronger deny above, and on a disallowed branch the rule 2 deny
    # further down must win. The ask is emitted at the allowed-branch exit.
    [ "$tracked" = 1 ] && INPLACE_TRACKED="$t"
  done
fi

if printf '%s\n' "$ALLOWED" | grep -qxF "$CURRENT_BRANCH"; then
  # Rule 3, in-place rewrite of a tracked file. This sits INSIDE the allowed
  # branch arm on purpose. On a disallowed branch the same command is refused
  # outright by rule 2 below, and asking first would downgrade that deny to a
  # prompt the user could wave through.
  if [ -n "$INPLACE_TRACKED" ]; then
    emit ask "This rewrites the tracked file '$INPLACE_TRACKED' through the shell rather than through Edit/Write. The change is recoverable, so the branch is not the problem -- but the PostToolUse formatter keys on the file_path of an Edit/Write call and cannot see a file written by a shell command, so the file may be left unformatted. Approve if this is a bulk mechanical rewrite; otherwise have Claude use Edit."
  fi
  allow
fi

case "$TOOL_NAME" in
  Write|Edit|MultiEdit|NotebookEdit)
    emit deny "Blocked: branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST). Do not edit files here, and do not switch branches yourself — stop and ask the user."
    ;;
  Bash)
    if git_command_mutates "$COMMAND"; then
      emit deny "Blocked: state-changing git command on branch '$CURRENT_BRANCH', which is not an allowed branch ($ALLOWED_LIST)."
    fi

    # Only reached on a disallowed branch, where ANY write under the repo is
    # refused -- creating a new file included. CONTENT_RE, collect_repo_targets
    # and REPO_TARGETS were all evaluated above; the untracked and unresolvable
    # cases have already emitted, so what is left here is a write to a tracked
    # file or to a path that does not exist yet. Both are fine on an allowed
    # branch and neither is fine here.
    if [ "$content_hit" = 0 ]; then
      emit deny "Blocked: this command changes file content under $TOPLEVEL, and branch '$CURRENT_BRANCH' is not an allowed branch ($ALLOWED_LIST)."
    fi

    # The metadata check that used to sit here now runs before the branch logic
    # above, so that it applies on allowed branches too. Nothing replaces it.
    ;;
esac

allow
