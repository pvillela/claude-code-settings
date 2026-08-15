//! The `PreToolUse` guard.
//!
//! Reads one payload on stdin, writes at most one decision on stdout, and
//! always exits 0. A non-zero exit with no output reads to Claude Code as a
//! non-blocking error, which is a silent allow — so every outcome, including
//! every failure, is expressed as a decision on stdout instead.

use std::io::{Read, Write};

use action_rules::hook::{Outcome, decide};

fn main() {
    let mut payload = String::new();
    let outcome = match std::io::stdin().read_to_string(&mut payload) {
        Ok(_) => decide(&payload),
        Err(e) => Outcome::Decide {
            decision: "ask",
            reason: format!("action-guard could not read the hook payload: {e}"),
        },
    };

    let text = outcome.render();
    if !text.is_empty() {
        // One write of a complete object. A partial write followed by a panic
        // message would be malformed JSON, which is treated as no opinion.
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }
}
