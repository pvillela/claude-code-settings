#!/usr/bin/env bash
# Renders the rule specification in src/action_rules.rs to its two outputs:
#
#   1. Rustdoc HTML  -> action-rules/target/doc          (gitignored, for browsing)
#   2. Markdown      -> $CLAUDE_CONFIG_DIR/docs/         (committed, linked from CLAUDE.md)
#
# The doc comments in src/action_rules.rs are the single source of truth for both.
set -euo pipefail

cd "$(dirname "$0")"

cargo doc --lib --no-deps --document-private-items
cargo run --quiet --bin gen-md -- "$@"

echo "rustdoc: $PWD/target/doc/action_rules/action_rules/index.html"
