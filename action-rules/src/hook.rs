//! The `PreToolUse` hook: payload in, decision out.
//!
//! Three things happen here and nowhere else: the tool name is dispatched, the
//! payload's fields are pulled out, and every way the guard can fail is turned
//! into a decision rather than into silence.
//!
//! ## Failure resolves to `Ask`
//!
//! A malformed payload, a panic, a git timeout, an unreadable
//! `allowed-branches` — each of them means the guard could not rule out a
//! write, and each produces `Ask` carrying the reason. Two failure modes are
//! specifically **not** available:
//!
//! - **Deny.** A parse bug that denies bricks the session.
//! - **Silence.** Claude Code reads malformed JSON, or a non-zero exit with no
//!   output, as a non-blocking error and proceeds — a fail-open back door. The
//!   decision is therefore built completely in memory and written with a single
//!   call, so a panic can never truncate it into something that reads as
//!   no-opinion.
//!
//! ## Allow is silence
//!
//! An allowed action emits nothing at all rather than `permissionDecision:
//! "allow"`. Emitting `allow` would put the guard's opinion *above* the user's
//! own `settings.json` permission rules; silence leaves the normal flow intact
//! and only the guard's refusals visible.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{
    action_rules::{Verdict, check},
    facts::Resolver,
    parse::{Parsed, parse_file_write, parse_with},
};

/// Tools that can write the filesystem, and are therefore judged by the rules.
///
/// The list lives here rather than in the `matcher` regex in `settings.json`,
/// so that the triage table and the test suite can reach it.
const WRITER_TOOLS: &[&str] = &["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"];

/// Tools that cannot write the filesystem or change git state.
///
/// Allowed by name, before any I/O at all. Since the matcher widens to `*`,
/// this path runs on every tool call in the session and has to stay far below
/// the cost of the call it precedes.
///
/// A tool missing from this list asks rather than misfires, and adding one is a
/// single line — which is the right direction for a list that a new release can
/// invalidate.
const READER_TOOLS: &[&str] = &[
    "Agent",
    "AskUserQuestion",
    "BashOutput",
    "CronList",
    "EndConversation",
    "EnterPlanMode",
    "ExitPlanMode",
    "Glob",
    "Grep",
    "KillBash",
    "KillShell",
    "ListAgents",
    "ListMcpResources",
    "Monitor",
    "NotebookRead",
    "Read",
    "ReadMcpResource",
    "ReportFindings",
    "ScheduleWakeup",
    "SendMessage",
    "Skill",
    "SlashCommand",
    "Task",
    "TaskOutput",
    "TaskStop",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Keys under `tool_input` that name a path, in the order they are tried.
const PATH_KEYS: &[&str] = &["file_path", "notebook_path", "path"];

/// What the guard says back to Claude Code.
pub enum Outcome {
    /// Say nothing, leaving the ordinary permission flow untouched.
    Silent,
    /// Emit a decision with its reason.
    Decide {
        decision: &'static str,
        reason: String,
    },
}

impl Outcome {
    fn ask(reason: impl Into<String>) -> Self {
        Outcome::Decide {
            decision: "ask",
            reason: reason.into(),
        }
    }

    fn deny(reason: impl Into<String>) -> Self {
        Outcome::Decide {
            decision: "deny",
            reason: reason.into(),
        }
    }

    fn of(verdict: Verdict, reason: String) -> Self {
        match verdict {
            Verdict::Allow => Outcome::Silent,
            Verdict::Ask => Outcome::ask(reason),
            Verdict::Deny => Outcome::deny(reason),
        }
    }

    /// The exact bytes to write to stdout, empty when there is nothing to say.
    ///
    /// Built in full before anything is written; see the module documentation.
    pub fn render(&self) -> String {
        match self {
            Outcome::Silent => String::new(),
            Outcome::Decide { decision, reason } => json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": decision,
                    "permissionDecisionReason": reason,
                }
            })
            .to_string(),
        }
    }
}

/// Judges one `PreToolUse` payload.
pub fn decide(payload: &str) -> Outcome {
    // A panic anywhere below is a defect in the guard, not evidence about the
    // command. It resolves to Ask like every other failure.
    match std::panic::catch_unwind(|| decide_inner(payload)) {
        Ok(outcome) => outcome,
        Err(_) => Outcome::ask("action-guard panicked; deciding by hand"),
    }
}

fn decide_inner(payload: &str) -> Outcome {
    let Ok(value) = serde_json::from_str::<Value>(payload) else {
        return Outcome::ask("action-guard could not read the hook payload as JSON");
    };

    let tool = value.get("tool_name").and_then(Value::as_str).unwrap_or("");
    if tool.is_empty() {
        return Outcome::ask("action-guard found no tool_name in the hook payload");
    }

    // The cheap path first: it runs on every tool call in the session.
    if READER_TOOLS.contains(&tool) {
        return Outcome::Silent;
    }
    if !WRITER_TOOLS.contains(&tool) {
        return Outcome::ask(format!(
            "`{tool}` is not a tool the action rules know. Add it to the reader \
             or writer list in action-rules/src/hook.rs if it should not ask."
        ));
    }

    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));

    let input = value.get("tool_input");
    let mut resolver: Option<Resolver> = None;

    let parsed = if tool == "Bash" {
        let Some(command) = input.and_then(|i| i.get("command")).and_then(Value::as_str) else {
            return Outcome::ask("action-guard found no command in the Bash payload");
        };
        parse_with(command, &cwd, &mut resolver)
    } else {
        let path = input.and_then(|i| {
            PATH_KEYS
                .iter()
                .find_map(|key| i.get(*key).and_then(Value::as_str))
        });
        let Some(path) = path else {
            return Outcome::ask(format!(
                "action-guard found no path in the {tool} payload, so it cannot \
                 tell what would be written"
            ));
        };
        parse_file_write(Path::new(path), &cwd, &mut resolver)
    };

    let Parsed { command, notes } = parsed;
    let verdict = check(&command);
    Outcome::of(verdict, reason_for(verdict, tool, &notes))
}

/// The sentence the user reads, which has to say what to do about it.
fn reason_for(verdict: Verdict, tool: &str, notes: &[String]) -> String {
    let detail = if notes.is_empty() {
        String::new()
    } else {
        format!(" ({})", notes.join("; "))
    };
    match verdict {
        Verdict::Allow => String::new(),
        Verdict::Ask => format!(
            "The action rules neither allow nor refuse this {tool} call{detail}. \
             See ~/.claude/docs/action-rules.md."
        ),
        Verdict::Deny => format!(
            "The action rules refuse this {tool} call{detail}. It writes outside \
             what the session governs, or on a branch it may not change. See \
             ~/.claude/docs/action-rules.md."
        ),
    }
}
