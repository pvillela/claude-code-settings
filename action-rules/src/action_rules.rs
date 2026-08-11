//! # Action rules
//!
//! The rules governing which actions are allowed, denied, or require
//! confirmation. This module is the single source of truth for both the
//! *specification* of the rules and (once [`Command::parse`] is implemented)
//! their *implementation*. The prose in these doc comments is normative; the
//! Markdown page in `~/.claude/docs/action-rules.md` is generated from it.
//!
//! ## What the rules are for
//!
//! The guard is a safety net for **irreversibility**, not a policy enforcer. It
//! exists to stop actions whose effects cannot be undone from the shell or from
//! git history. Completeness is explicitly a non-goal: writes performed by a
//! script the guard cannot see through are an accepted limit.
//!
//! ## Two dimensions
//!
//! A command is judged along two independent dimensions, and the **worse** of
//! the two verdicts wins:
//!
//! 1. The **file dimension** — what paths the command writes to. This carries
//!    almost all of the semantics, and is evaluated by [`check_action`].
//! 2. The **git dimension** — the handful of repository operations that either
//!    name no path at all or are destructive even where the file rules would
//!    permit them. Evaluated by [`check_git`]. It is deliberately small; see
//!    [`GitAction`].
//!
//! Composing rather than ranking the two means neither can mask the other.
//!
//! ## Three verdicts, and where uncertainty lives
//!
//! [`Outcome`] has three values, but the predicates on [`Target`] are all
//! `bool`. Uncertainty is deliberately *not* spread through the predicate
//! layer; it is produced in exactly two places:
//!
//! - [`Action::Opaque`], when the parser cannot determine what a command does.
//! - The gap between [`Target::is_allowed`] and [`Target::is_protected`], which
//!   are **not** complements. A target that satisfies neither is one the rules
//!   recognise but decline to decide, and it is handed to the user.
//!
//! Keeping the gap explicit is the point: a two-valued design would force every
//! recognised-but-undecided case to resolve as allow (defeating the guard) or
//! deny (obstructing ordinary work).
//!
//! ## Lanes
//!
//! Every target sits in exactly one region of the filesystem, and the region
//! determines which repository's branch governs it:
//!
//! | Region | Governed by |
//! | --- | --- |
//! | The launch project (fixed at startup, so a `cd` cannot move it) | its own branch and gitignore state |
//! | `$CLAUDE_CONFIG_DIR` (default `~/.claude`) | its own branch and gitignore state |
//! | Scratch roots and write sinks | allowed outright |
//! | Any other git repository | [`Target::is_in_foreign_repo`] — protected |
//! | Under no repository at all | [`Target::is_loose`] — protected |
//!
//! Note that being outside a repository is a *deny*, not an allow: a file with
//! no version control behind it is the least recoverable target there is.
//!
//! ## Changing policy
//!
//! The two policy surfaces are [`Target::is_allowed`] and
//! [`Target::is_protected`]. Each is a flat disjunction of individually named,
//! individually documented predicates. Moving a predicate from one body to the
//! other, or removing it from both, is how policy changes; the disjuncts that
//! are expected to move carry a `POLICY:` comment.

// This module is a specification. Its predicates are documented and composed
// but not yet called from a running hook, so the usual dead-code analysis has
// nothing to see.
//
// The rules are documented on non-public items by design, and both renderings
// of this module are produced with `--document-private-items`, so intra-doc
// links to private items always resolve.
#![allow(rustdoc::private_intra_doc_links)]
#![allow(dead_code)]

use std::{path::PathBuf, sync::LazyLock};

use regex::Regex;

// -----------------------------------------------------------------------------
// Terms
// -----------------------------------------------------------------------------

/// The verdict for an action.
///
/// The ordering is significant: `Allow < Ask < Deny`, so composing several
/// verdicts is `max`, and the worst verdict always wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    /// Proceed without involving the user.
    Allow,
    /// Hand the decision to the user.
    Ask,
    /// Refuse.
    Deny,
}

/// What an action does to one particular target.
///
/// Effects are recorded per target rather than per command, because a single
/// command is rarely uniform: `mv tracked.txt /tmp/x` *changes* one target and
/// *creates* another, and flattening that to a single classification would give
/// the wrong verdict for one half of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// The target does not exist and the action would bring it into being.
    Create,
    /// The target exists and the action would modify or remove it.
    Change,
}

/// A git repository, and the facts about it that the rules depend on.
///
/// Branch is a property of a repository, not of a file. Keeping it here rather
/// than on [`Target`] is what allows the four combinations of (tracked?,
/// allowed branch?) to be expressed independently.
#[derive(Clone, Debug)]
pub struct Repo {
    /// Absolute path of the repository's top level.
    root: PathBuf,
    /// The checked-out branch, or `None` when `HEAD` is detached.
    branch: Option<String>,
}

/// Anything that is a file in the general Unix sense: regular files,
/// directories, devices, FIFOs, sockets.
#[derive(Clone, Debug)]
pub struct Target {
    /// Absolute, symlink-resolved path.
    path: PathBuf,
    /// The repository governing this target, or `None` if it is under no
    /// repository.
    repo: Option<Repo>,
}

/// A [`Target`] together with what the action does to it.
#[derive(Clone, Debug)]
pub struct TargetedEffect {
    target: Target,
    effect: Effect,
}

/// The file dimension of a command: what it does to paths.
#[derive(Clone, Debug)]
pub enum Action {
    /// The command writes nothing.
    ReadOnly,
    /// The command writes to each listed target with the listed effect.
    Write(Vec<TargetedEffect>),
    /// The command is refused regardless of what it targets.
    ///
    /// Reserved for operations that are irreversible *and* whose prior state
    /// cannot be reconstructed: `chown`, `chgrp`, `shred`. Note that `chmod`,
    /// `ln` and `touch` are deliberately **not** here — they are ordinary
    /// writes, and classifying them by target lets the scratch lanes apply.
    Forbidden,
    /// The parser cannot determine what the command does, or which targets it
    /// acts upon.
    ///
    /// Reserved for genuine unknowns: `$VAR`, `$(…)`, backticks, and syntax the
    /// scanner does not model. Globs are **not** opaque — they are expanded
    /// against the filesystem at decision time, so `rm -rf target/*` resolves to
    /// real targets rather than prompting.
    Opaque,
}

/// The git dimension of a command.
///
/// This exists only for what the file dimension structurally cannot express.
/// Every other git operation is judged by the file rules through its repository
/// root, which [`Target::is_tracked`] reports as tracked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitAction {
    /// A read-only git invocation.
    ///
    /// Determined by an **allowlist** of subcommands: anything not on the list
    /// is treated as mutating. The inverse arrangement — a denylist of writing
    /// subcommands — is what lets `git add`, `git fetch` and
    /// `git submodule update` pass unexamined today.
    Read,
    /// An ordinary state mutation: `commit`, `merge`, `rebase`, `switch`.
    ///
    /// Contributes nothing on its own. These are governed by the file dimension
    /// through the repository root, which carries the branch rule.
    StateChange,
    /// Destructive even on an allowed branch: `reset --hard`, `clean -f`,
    /// `checkout -- .`, `branch -D`, `stash drop`, `push --force`.
    ///
    /// Without this, `git clean -fdx` on an allowed branch would be permitted
    /// outright while `rm untracked.txt` — a strictly narrower action, deleting
    /// a subset of the same files — would ask.
    Destructive,
    /// A write to git configuration at **any** scope, or a mutation of remotes.
    ///
    /// Denied unconditionally. `--global` and `--system` write outside every
    /// repository; `--local` writes `<repo>/.git/config`, which is neither
    /// tracked nor ignored and would otherwise fall into the undecided gap.
    ConfigWrite,
}

/// A parsed command: both dimensions, judged together.
#[derive(Clone, Debug)]
pub struct Command {
    /// What the command does to files.
    action: Action,
    /// Every git invocation the command contains.
    git: Vec<GitAction>,
}

// -----------------------------------------------------------------------------
// Repository facts
// -----------------------------------------------------------------------------

impl Repo {
    /// The branch that is allowed even when no `allowed-branches` file exists.
    pub const FALLBACK_ALLOWED_BRANCH: &'static str = "aicode";

    /// The set of branches on which tracked files may be written.
    ///
    /// The **union** of [`Self::FALLBACK_ALLOWED_BRANCH`] and the non-comment,
    /// non-blank lines of `<root>/.claude/allowed-branches`. A union, not a
    /// replacement: adding an entry to that file must never silently withdraw
    /// permission from the fallback branch.
    fn allowed_branches(&self) -> Vec<String> {
        todo!()
    }

    /// The repository's current branch is in [`Self::allowed_branches`].
    ///
    /// A detached `HEAD` is **not** allowed. It is a state one arrives at by
    /// accident, and commits made there are unreferenced by default.
    fn is_allowed_branch(&self) -> bool {
        todo!()
    }

    /// This repository is the project Claude Code was launched in.
    ///
    /// Resolved once at startup and cached, so that a `cd` during the session
    /// cannot move the lane boundary.
    fn is_launch_project(&self) -> bool {
        todo!()
    }

    /// This repository is `$CLAUDE_CONFIG_DIR` (default `~/.claude`).
    fn is_claude_home(&self) -> bool {
        todo!()
    }
}

// -----------------------------------------------------------------------------
// Target predicates
// -----------------------------------------------------------------------------

impl Target {
    /// Paths that are always writable because writing to them discards data by
    /// definition, or because they are session scratch space.
    ///
    /// `$CLAUDE_CONFIG_DIR` is deliberately **not** here. It is a lane of its
    /// own, governed by its own repository's branch and gitignore state, not a
    /// blanket exception.
    pub const EXCEPTION_PATH_REGEX_STRS: [&'static str; 3] = [
        r"^/tmp(/|$)",
        r"^/var/tmp(/|$)",
        r"^/workspaces/scratch(/|$)",
    ];

    /// Write sinks: writing to them cannot destroy anything.
    pub const WRITE_SINK_REGEX_STRS: [&'static str; 1] = [r"^/dev/(null|stdout|stderr|tty)$"];

    #[doc(hidden)]
    fn compiled(strs: &'static [&'static str]) -> Vec<Regex> {
        strs.iter()
            .map(|re| Regex::new(re).unwrap_or_else(|e| panic!("invalid regex '{re}': {e}")))
            .collect()
    }

    #[doc(hidden)]
    fn exception_regexes() -> &'static [Regex] {
        static RES: LazyLock<Vec<Regex>> =
            LazyLock::new(|| Target::compiled(&Target::EXCEPTION_PATH_REGEX_STRS));
        &RES
    }

    #[doc(hidden)]
    fn sink_regexes() -> &'static [Regex] {
        static RES: LazyLock<Vec<Regex>> =
            LazyLock::new(|| Target::compiled(&Target::WRITE_SINK_REGEX_STRS));
        &RES
    }

    #[doc(hidden)]
    fn path_str(&self) -> &str {
        self.path
            .to_str()
            .expect("path should have been validated at construction")
    }

    /// The path exists on disk.
    fn exists(&self) -> bool {
        todo!()
    }

    /// The target is a write sink — see [`Self::WRITE_SINK_REGEX_STRS`].
    fn is_write_sink(&self) -> bool {
        Self::sink_regexes()
            .iter()
            .any(|re| re.is_match(self.path_str()))
    }

    /// The target is under one of [`Self::EXCEPTION_PATH_REGEX_STRS`].
    ///
    /// Scoped so as never to overlap [`Self::is_protected`]: a path that is
    /// itself a repository root is excluded, so that an exception root cannot
    /// authorise the destruction of the repository it contains.
    fn is_allowed_exception(&self) -> bool {
        !self.is_repo_root()
            && Self::exception_regexes()
                .iter()
                .any(|re| re.is_match(self.path_str()))
    }

    /// The target is tracked by its repository.
    ///
    /// True for a tracked file, and **also for any directory containing a
    /// tracked file at any depth**. That is what gives `rm -rf src/` the branch
    /// rule, and what gives repository-wide git operations — whose target is
    /// the repository root — a verdict without any git-specific rule.
    fn is_tracked(&self) -> bool {
        todo!()
    }

    /// The target is the top level of a repository.
    ///
    /// Always protected. The branch rule's premise is that changes to tracked
    /// files are recoverable from history; removing the repository root removes
    /// the `.git` directory that premise depends on.
    fn is_repo_root(&self) -> bool {
        todo!()
    }

    /// The target is a directory every file under which is ignored, or is
    /// itself under such a directory.
    ///
    /// This is the build-output case: `target/`, `node_modules/`, `dist/`.
    /// Contents are reproducible, so they may be written and removed freely on
    /// any branch.
    ///
    /// The test is over **contents**, not over the directory path. Testing the
    /// path gives the wrong answer under a deny-all-then-allowlist `.gitignore`
    /// such as the one in `$CLAUDE_CONFIG_DIR`, where directories are
    /// explicitly re-included by `!**/` so that git can descend into them while
    /// every file inside remains ignored.
    fn all_contents_ignored(&self) -> bool {
        todo!()
    }

    /// The target is an existing file ignored by a file-level pattern rather
    /// than by a recursively-ignored directory.
    ///
    /// This is the population that is ignored but *not* reproducible:
    /// `devcontainer.env`, `data/*.xlsx`, credentials. Being ignored, it has no
    /// history to recover from; being irreplaceable, it must not be clobbered
    /// silently. It therefore sits in neither policy surface, and asks.
    fn is_file_pattern_ignored(&self) -> bool {
        todo!()
    }

    /// The target is in a repository whose branch is allowed, and is tracked.
    fn is_tracked_on_allowed_branch(&self) -> bool {
        todo!()
    }

    /// The target does not yet exist, and lies in a repository whose branch is
    /// allowed.
    ///
    /// Creating a new file must be possible on an allowed branch. A rule set
    /// that keys only on tracked-ness cannot express this, because a path that
    /// does not exist is not tracked.
    fn is_new_on_allowed_branch(&self) -> bool {
        todo!()
    }

    /// The target is in a repository whose branch is **not** allowed.
    ///
    /// Scoped so as never to overlap [`Self::is_allowed`]: targets under an
    /// exception path, and targets whose contents are entirely ignored, are
    /// excluded, since neither depends on history for recovery.
    fn is_on_disallowed_branch(&self) -> bool {
        todo!()
    }

    /// The target is in a repository that is neither the launch project nor
    /// `$CLAUDE_CONFIG_DIR`.
    ///
    /// Protected, creation included. A second checkout is not the project the
    /// session was opened against, and the guard has no basis for judging what
    /// is safe there. This is the case the previous `$PWD`-anchored guard failed
    /// **open** on.
    fn is_in_foreign_repo(&self) -> bool {
        todo!()
    }

    /// The target is under no repository at all, and matches no exception.
    ///
    /// Protected, creation included. `~/.bashrc`, `/etc/hosts` and the like have
    /// no version control behind them, so a write there is the least recoverable
    /// action available.
    fn is_loose(&self) -> bool {
        todo!()
    }

    // -------------------------------------------------------------------------
    // Policy surfaces
    // -------------------------------------------------------------------------

    /// **Policy surface.** The target may be written without consulting the
    /// user.
    ///
    /// See also [`Self::is_protected`]. The two are deliberately not
    /// complements; the invariant is that no target satisfies both, and a target
    /// satisfying neither is referred to the user.
    fn is_allowed(&self) -> bool {
        self.is_write_sink()
            || self.is_allowed_exception()
            || self.all_contents_ignored()
            || self.is_tracked_on_allowed_branch()
            || self.is_new_on_allowed_branch()
    }

    /// **Policy surface.** The target must not be written.
    ///
    /// **Not** the complement of [`Self::is_allowed`]. The invariant is
    /// `!(is_allowed() && is_protected())`; the gap between the two is the space
    /// of targets that require confirmation.
    fn is_protected(&self) -> bool {
        self.is_repo_root()
            || self.is_loose()
            // POLICY: remove this disjunct to make foreign repositories ask
            // rather than deny.
            || self.is_in_foreign_repo()
            || self.is_on_disallowed_branch()
        // POLICY: add `|| self.is_file_pattern_ignored()` to deny writes to
        // ignored-but-irreplaceable files rather than asking about them.
    }
}

// -----------------------------------------------------------------------------
// Action classification
// -----------------------------------------------------------------------------

impl Command {
    /// Parses a command string into both dimensions.
    ///
    /// The file dimension must be classified in this order, so that the most
    /// severe reading of a command wins:
    ///
    /// 1. [`Action::Forbidden`] if any portion of the command is forbidden.
    /// 2. [`Action::Opaque`] if any portion cannot be parsed.
    /// 3. [`Action::Write`] if any portion writes, collecting every target
    ///    with its own [`Effect`].
    /// 4. [`Action::ReadOnly`] if no portion writes.
    ///
    /// The git dimension collects one [`GitAction`] per git invocation found,
    /// independently of the above; it is composed with the file verdict rather
    /// than ranked against it, so neither dimension can mask the other.
    fn parse(_command: impl AsRef<str>) -> Command {
        todo!()
    }
}

impl Action {
    #[doc(hidden)]
    fn is_forbidden(&self) -> bool {
        matches!(self, Action::Forbidden)
    }

    #[doc(hidden)]
    fn is_opaque(&self) -> bool {
        matches!(self, Action::Opaque)
    }

    #[doc(hidden)]
    fn is_read_only(&self) -> bool {
        matches!(self, Action::ReadOnly)
    }

    #[doc(hidden)]
    fn eval_predicate_all(&self, f: impl Fn(&Target) -> bool) -> bool {
        match self {
            Action::Write(tes) => tes.iter().all(|te| f(&te.target)),
            _ => true,
        }
    }

    #[doc(hidden)]
    fn eval_predicate_any(&self, f: impl Fn(&Target) -> bool) -> bool {
        match self {
            Action::Write(tes) => tes.iter().any(|te| f(&te.target)),
            _ => false,
        }
    }
}

// -----------------------------------------------------------------------------
// Rules
// -----------------------------------------------------------------------------

// ----- Short-circuit -----

/// The command contains a forbidden operation.
fn forbidden_action(action: &Action) -> bool {
    action.is_forbidden()
}

/// The command could not be parsed, or its targets could not be determined.
fn opaque_action(action: &Action) -> bool {
    action.is_opaque()
}

// ----- `Allow` rules -----

/// The command writes nothing.
fn read_only_action(action: &Action) -> bool {
    action.is_read_only()
}

/// The command writes, and **every** target it writes is allowed.
fn write_on_allowed_targets(action: &Action) -> bool {
    matches!(action, Action::Write(_)) && action.eval_predicate_all(Target::is_allowed)
}

// ----- `Deny` rules -----

/// The command writes, and **any** target it writes is protected.
fn write_on_protected_targets(action: &Action) -> bool {
    action.eval_predicate_any(Target::is_protected)
}

// -----------------------------------------------------------------------------
// Rule composition
// -----------------------------------------------------------------------------

/// Judges the file dimension.
///
/// Falling through every rule yields [`Outcome::Ask`]: the command was
/// understood, but its targets are neither clearly safe nor clearly protected.
fn check_action(action: &Action) -> Outcome {
    // Short-circuit checks.
    if forbidden_action(action) {
        return Outcome::Deny;
    }
    if opaque_action(action) {
        return Outcome::Ask;
    }

    // `Allow` rules.
    if read_only_action(action) || write_on_allowed_targets(action) {
        return Outcome::Allow;
    }

    // `Deny` rules.
    if write_on_protected_targets(action) {
        return Outcome::Deny;
    }

    Outcome::Ask
}

impl GitAction {
    /// The verdict this git operation contributes on its own.
    fn outcome(self) -> Outcome {
        match self {
            GitAction::Read => Outcome::Allow,
            // Judged by the file dimension through the repository root.
            GitAction::StateChange => Outcome::Allow,
            GitAction::Destructive => Outcome::Ask,
            GitAction::ConfigWrite => Outcome::Deny,
        }
    }
}

/// Judges the git dimension: the worst verdict among the git operations found.
fn check_git(git: &[GitAction]) -> Outcome {
    git.iter()
        .map(|g| g.outcome())
        .max()
        .unwrap_or(Outcome::Allow)
}

// -----------------------------------------------------------------------------
// Command checking
// -----------------------------------------------------------------------------

/// The verdict for a command: the worse of its two dimensions.
pub fn check_command(command: impl AsRef<str>) -> Outcome {
    let Command { action, git } = Command::parse(command);
    check_action(&action).max(check_git(&git))
}
