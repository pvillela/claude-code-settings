#!/usr/bin/env bash
#
# PostToolUse hook: keep edited Rust files conforming to rustfmt.
#
# Editing an .rs file by hand or by script does not keep rustfmt happy — line lengths, chain breaks
# and argument wrapping all drift, and the drift is invisible until someone opens the file in an
# editor with format-on-save and gets a diff they did not ask for.
#
# Scope and limits:
#   - One file per invocation, the one the tool just wrote. Never the whole crate: reformatting code
#     that was not otherwise touched is noise in the diff.
#   - Only fires for Edit/Write/MultiEdit. A file written by a shell command (sed -i, a heredoc,
#     `cargo fmt`'s own output) is invisible here, so an explicit `cargo fmt` before reporting work
#     done is still the rule; this is a backstop, not a substitute.
#   - Silent and non-blocking. A formatting failure — syntax error mid-edit, rustfmt absent — must
#     not fail the tool call that triggered it, so every failure path exits 0.
#
# Input: the PostToolUse JSON payload on stdin.

set -uo pipefail

payload=$(cat)
file=$(printf '%s' "$payload" | jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null)

[ -n "$file" ] || exit 0
case "$file" in
    *.rs) ;;
    *) exit 0 ;;
esac
[ -f "$file" ] || exit 0
command -v rustfmt >/dev/null 2>&1 || exit 0

# The edition comes from the nearest Cargo.toml. rustfmt defaults to 2015, which rejects syntax
# every current crate uses, so guessing wrong here means the hook silently does nothing.
dir=$(dirname "$file")
edition=""
while [ "$dir" != "/" ] && [ "$dir" != "." ]; do
    if [ -f "$dir/Cargo.toml" ]; then
        edition=$(grep -m1 -E '^[[:space:]]*edition[[:space:]]*=' "$dir/Cargo.toml" 2>/dev/null |
            sed -E 's/.*"([^"]+)".*/\1/')
        break
    fi
    dir=$(dirname "$dir")
done

# `skip_children` keeps the hook to the file that was edited. Without it rustfmt follows `mod`
# declarations, so one edit to a crate root reformats the whole module tree — verified, not
# assumed. It is a `--config` option, not a flag: `--skip-children` is not recognised.
rustfmt --edition "${edition:-2021}" --config skip_children=true "$file" >/dev/null 2>&1 || true
exit 0
