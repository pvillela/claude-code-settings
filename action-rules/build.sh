#!/usr/bin/env bash
# Builds the action-guard binary.
#
# Run from the SessionStart hook as well as by hand. Cargo's own fingerprinting
# is the staleness check: when nothing has changed this costs tens of
# milliseconds, so there is no cheaper check worth writing.
#
# Known gap, accepted: a source edit mid-session leaves the previously built
# binary in use until the next session, or until this script is run by hand.
set -euo pipefail

cd "$(dirname "$0")"
cargo build --release --bin action-guard "$@"
