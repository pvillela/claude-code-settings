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
//! [`Verdict`] has three values, but the predicates on [`Target`] are all
//! `bool`. Uncertainty is deliberately *not* spread through the predicate
//! layer; it is produced in exactly two places:
//!
//! - [`Action::Opaque`], when the parser cannot determine what a command does.
//! - [`GitAction::Destructive`], which asks even where the branch rule permits.
//! - The gap between [`Target::is_allowed`] and [`Target::is_protected`], which
//!   are **not** complements. A target that satisfies neither is one the rules
//!   recognise but decline to decide, and it is handed to the user.
//!
//! **The gap is currently empty.** Every target the resolver can produce is now
//! either allowed or protected, so a command whose targets are all understood
//! never asks; `Ask` reaches the user only by the two routes above. The gap is
//! kept in the design regardless, because it is what any future policy change
//! falls into: a two-valued design would force every recognised-but-undecided
//! case to resolve as *allow* (defeating the guard) or *deny* (obstructing
//! ordinary work). Removing any disjunct of [`Target::is_allowed`] — which the
//! `POLICY:` comments invite — reopens it, and a test asserts as much.
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
//! are may potentially move carry a `POLICY:` comment.

// The rules are documented on non-public items by design, and both renderings
// of this module are produced with `--document-private-items`, so intra-doc
// links to private items always resolve.
#![allow(rustdoc::private_intra_doc_links)]
// A predicate may be named and documented without appearing in either policy
// surface — that is what the `POLICY:` comments describe moving in and out of.
// Dead-code analysis has nothing useful to say about a specification of that
// shape, and a warning here would be pressure to delete documented rules.
#![allow(dead_code)]

use std::{
    path::Path,
    sync::{Arc, LazyLock},
};

use regex::Regex;

use crate::facts::{Ignored, RepoFacts, TargetFacts};

#[cfg(test)]
#[doc(hidden)]
mod tests;

// -----------------------------------------------------------------------------
// Terms
// -----------------------------------------------------------------------------

/// The verdict for an action.
///
/// The ordering is significant: `Allow < Ask < Deny`, so composing several
/// verdicts is `max`, and the worst verdict always wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
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
    /// Everything resolved about the repository: its top level, its checked-out
    /// branch (`None` when `HEAD` is detached), the lines of its
    /// `allowed-branches` file, and which lane it belongs to.
    ///
    /// Raw facts only, resolved once per command and shared. Every judgement
    /// built on them is made by the predicates below, which is what keeps this
    /// module free of the filesystem.
    facts: Arc<RepoFacts>,
}

/// Anything that is a file in the general Unix sense: regular files,
/// directories, devices, FIFOs, sockets.
#[derive(Clone, Debug)]
pub struct Target {
    /// Everything resolved about the path: its absolute symlink-resolved form,
    /// whether it exists, whether it is a directory, the repository governing
    /// it (`None` if it is under none), whether it is that repository's top
    /// level, whether it is tracked, and how git ignores it.
    ///
    /// Raw facts only; see [`Repo::facts`].
    facts: TargetFacts,
}

/// A [`Target`] together with what the action does to it.
#[derive(Clone, Debug)]
pub struct TargetedEffect {
    /// The path acted upon.
    target: Target,
    /// What the action does to it.
    effect: Effect,
}

impl TargetedEffect {
    #[doc(hidden)]
    pub fn new(target: Target, effect: Effect) -> Self {
        TargetedEffect { target, effect }
    }
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
#[derive(Clone, Debug)]
pub enum GitAction {
    /// A read-only git invocation.
    ///
    /// Determined by an **allowlist** of subcommands: anything not on the list
    /// is treated as mutating.
    Read,
    /// An ordinary state mutation: `commit`, `merge`, `rebase`, `switch`, in
    /// the repository named, or `None` when it runs under no repository.
    ///
    /// It carries the branch rule directly, rather than reaching it through the
    /// repository root as a file target. Routing it through the root cannot
    /// work: the root is protected outright by [`Target::is_repo_root`], so
    /// nothing reached through it could ever be allowed — and making the root
    /// allowable in order to permit `git commit` would make `rm -rf <root>`
    /// allowable with it.
    StateChange(Option<Repo>),
    /// Destructive even on an allowed branch: `reset --hard`, `clean -f`,
    /// `checkout -- .`, `branch -D`, `stash drop`, `push --force`.
    ///
    /// Without this, `git clean -fdx` on an allowed branch would be permitted
    /// outright while `rm untracked.txt` — a strictly narrower action, deleting
    /// a subset of the same files — would ask. Carries its repository for the
    /// same reason [`Self::StateChange`] does: on a branch the session does not
    /// govern, destroying uncommitted work is worse than an ordinary mutation,
    /// not better.
    Destructive(Option<Repo>),
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
    #[doc(hidden)]
    pub fn new(facts: Arc<RepoFacts>) -> Self {
        Repo { facts }
    }

    /// The branch that is allowed even when no `allowed-branches` file exists.
    pub const FALLBACK_ALLOWED_BRANCH: &'static str = "aicode";

    /// The set of branches on which tracked files may be written.
    ///
    /// The **union** of [`Self::FALLBACK_ALLOWED_BRANCH`] and the non-comment,
    /// non-blank lines of `<root>/.claude/allowed-branches`. A union, not a
    /// replacement: adding an entry to that file must never silently withdraw
    /// permission from the fallback branch.
    // gen-md: show-body
    fn allowed_branches(&self) -> Vec<String> {
        let mut branches = vec![Self::FALLBACK_ALLOWED_BRANCH.to_owned()];
        branches.extend(
            self.facts
                .allowed_branches_file
                .iter()
                .filter(|b| b.as_str() != Self::FALLBACK_ALLOWED_BRANCH)
                .cloned(),
        );
        branches
    }

    /// The repository's current branch is in [`Self::allowed_branches`].
    ///
    /// A detached `HEAD` is **not** allowed. It is a state one arrives at by
    /// accident, and commits made there are unreferenced by default.
    // gen-md: show-body
    fn is_allowed_branch(&self) -> bool {
        match &self.facts.branch {
            Some(branch) => self.allowed_branches().iter().any(|b| b == branch),
            None => false,
        }
    }

    /// This repository is the project Claude Code was launched in.
    ///
    /// Resolved once at startup and cached, so that a `cd` during the session
    /// cannot move the lane boundary.
    fn is_launch_project(&self) -> bool {
        self.facts.is_launch_project
    }

    /// This repository is `$CLAUDE_CONFIG_DIR` (default `~/.claude`).
    fn is_claude_home(&self) -> bool {
        self.facts.is_claude_home
    }

    /// This repository is one of the two the session governs by branch.
    ///
    /// The Lanes table names exactly two repositories as governed by their own
    /// branch and gitignore state. In every other repository a branch carries
    /// no permission at all, which is what stops an allowed branch name in a
    /// foreign checkout from authorising a write there.
    // gen-md: show-body
    fn is_lane(&self) -> bool {
        self.is_launch_project() || self.is_claude_home()
    }
}

// -----------------------------------------------------------------------------
// Target predicates
// -----------------------------------------------------------------------------

impl Target {
    #[doc(hidden)]
    pub fn new(facts: TargetFacts) -> Self {
        Target { facts }
    }

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
        self.facts
            .path
            .to_str()
            .expect("path should have been validated at construction")
    }

    #[doc(hidden)]
    fn repo(&self) -> Option<Repo> {
        self.facts.repo.as_ref().map(|facts| Repo {
            facts: facts.clone(),
        })
    }

    /// The path exists on disk.
    fn exists(&self) -> bool {
        self.facts.exists
    }

    /// The target is a write sink — see [`Self::WRITE_SINK_REGEX_STRS`].
    ///
    /// Scoped so as never to overlap [`Self::is_protected`], on the same
    /// grounds as [`Self::is_allowed_exception`]: whatever a repository root
    /// happens to be called, it is not a sink.
    // gen-md: show-body
    fn is_write_sink(&self) -> bool {
        !self.is_repo_root()
            && Self::sink_regexes()
                .iter()
                .any(|re| re.is_match(self.path_str()))
    }

    /// The target is under one of [`Self::EXCEPTION_PATH_REGEX_STRS`].
    ///
    /// Scoped so as never to overlap [`Self::is_protected`]: a path that is
    /// itself a repository root is excluded, so that an exception root cannot
    /// authorise the destruction of the repository it contains.
    // gen-md: show-body
    fn is_allowed_exception(&self) -> bool {
        !self.is_repo_root()
            && Self::exception_regexes()
                .iter()
                .any(|re| re.is_match(self.path_str()))
    }

    /// The target's location grants the write on its own, whatever repository
    /// it may sit in: a write sink, or a scratch root.
    ///
    /// Named because every protected predicate has to exclude it. That
    /// exclusion is what holds the invariant between the two policy surfaces:
    /// a location grant is unconditional, so anything that would protect the
    /// same path has to stand down.
    // gen-md: show-body
    fn location_grants_write(&self) -> bool {
        self.is_write_sink() || self.is_allowed_exception()
    }

    /// The target is tracked by its repository.
    ///
    /// True for a tracked file, and **also for any directory containing a
    /// tracked file at any depth**. That is what gives `rm -rf src/` the branch
    /// rule: a directory's recoverability is the recoverability of what is
    /// under it.
    fn is_tracked(&self) -> bool {
        self.facts.tracked
    }

    /// The target is the top level of a repository.
    ///
    /// Always protected. The branch rule's premise is that changes to tracked
    /// files are recoverable from history; removing the repository root removes
    /// the `.git` directory that premise depends on.
    fn is_repo_root(&self) -> bool {
        self.facts.is_repo_root
    }

    /// The target is a directory git reports as ignored — a
    /// *gitignored allowed dir* — or lies under one.
    ///
    /// This is the build-output case: `target/`, `node_modules/`, `dist/`.
    /// Contents are reproducible, so they may be written and removed freely on
    /// any branch.
    ///
    /// The test is over the **directory's own path**, and the grant has to come
    /// from above it. Git never applies `D/.gitignore` to `D` itself, so
    /// `git check-ignore D` answers exactly the right question: a directory is
    /// ignored only when some `.gitignore` in an ancestor directory says so.
    /// The distinction is the whole point of the predicate. A `data/` directory
    /// whose own `.gitignore` reads `*` has every file inside it ignored, but
    /// nothing above it ever declared it reproducible — those files are
    /// irreplaceable, and [`Self::is_file_pattern_ignored`] protects them.
    ///
    /// A directory a deny-all-then-allowlist `.gitignore` re-includes by `!**/`
    /// is likewise not one of these, so `$CLAUDE_CONFIG_DIR` declares its
    /// machine-written directories explicitly.
    ///
    /// Scoped so as never to overlap [`Self::is_protected`]: a repository root
    /// is excluded, since a repository with nothing tracked in it would
    /// otherwise be writable whole.
    // gen-md: show-body
    fn is_under_gitignored_allowed_dir(&self) -> bool {
        !self.is_repo_root() && self.facts.ignored == Ignored::UnderGitignoredDir
    }

    /// The target is ignored by a pattern of its own rather than by a
    /// gitignored directory — see [`Self::is_under_gitignored_allowed_dir`] —
    /// or is a directory holding such a file at any depth.
    ///
    /// This is the population that is ignored but *not* reproducible:
    /// `devcontainer.env`, `data/*.xlsx`, credentials. Being ignored, it has no
    /// history to recover from; being irreplaceable, it cannot be reconstructed
    /// either. It is therefore the least recoverable target inside a repository,
    /// and is protected.
    ///
    /// ## A directory is judged by what it holds
    ///
    /// A directory carries this too, whenever it holds a gitignored file that
    /// no tool can regenerate. Without that, `rm data/notes.csv` would be
    /// denied while `rm -rf data` — which destroys the same file, and every one
    /// of its neighbours — would merely ask. The reading is the one
    /// [`Self::is_tracked`] already takes: a directory's recoverability is the
    /// recoverability of what is under it.
    ///
    /// Gitignored is the operative word, and it is not the same as untracked. A
    /// new file that git merely does not track yet has no history either, but
    /// git can see it and will offer to commit it, so it is on its way into
    /// version control rather than kept out of it. Those ask, and so does the
    /// directory holding them.
    ///
    /// Build output has a way to be rebuilt, so it never puts the label on the
    /// directory above it. Every file under a gitignored directory has that
    /// directory as an ancestor, which is what makes it reproducible, so
    /// `target/debug/app` leaves `target/` alone and a `data/` holding nothing
    /// but `data/target/` stays in the gap.
    ///
    /// **The exception: tracked content takes precedence.** A directory holding
    /// anything git tracks is judged by the branch rule instead, and stays
    /// writable on an allowed branch. `rm -rf src` therefore still works, even
    /// with a `src/.env` inside it. This is deliberate, and it is the one place
    /// where an irreplaceable file goes unprotected. The alternative is worse:
    /// stray `*.log` files are common enough that protecting every directory
    /// holding one would deny `rm -rf` almost everywhere, and a guard that
    /// refuses ordinary work is one that gets switched off.
    ///
    /// ## Scope
    ///
    /// Existence is required because clobbering is the irreversible act.
    /// Creating a file at an ignored path destroys nothing, so a path that does
    /// not exist stays with [`Self::is_new_on_allowed_branch`] — which is also
    /// what keeps the two disjoint.
    ///
    /// Scoped so as never to overlap [`Self::is_allowed`]: a target whose
    /// location grants the write is excluded, so an ignored file in a
    /// repository under `/tmp` stays writable.
    // gen-md: show-body
    fn is_file_pattern_ignored(&self) -> bool {
        !self.location_grants_write() && self.exists() && self.facts.ignored == Ignored::FilePattern
    }

    /// The target is in a lane repository whose branch is allowed, and is
    /// tracked.
    ///
    /// A *lane* repository: see [`Repo::is_lane`]. A branch name in a foreign
    /// checkout grants nothing, so this cannot overlap
    /// [`Self::is_in_foreign_repo`].
    ///
    /// Scoped so as never to overlap [`Self::is_protected`]: a repository root
    /// is excluded, since every root with anything tracked in it is tracked by
    /// [`Self::is_tracked`]'s recursive reading.
    // gen-md: show-body
    fn is_tracked_on_allowed_branch(&self) -> bool {
        !self.is_repo_root()
            && self.is_tracked()
            && self
                .repo()
                .is_some_and(|r| r.is_lane() && r.is_allowed_branch())
    }

    /// The target is an existing file in a lane repository on an allowed
    /// branch, and git neither tracks nor ignores it.
    ///
    /// Almost every file in this population was created by the session itself:
    /// [`Self::is_new_on_allowed_branch`] permits the first write, and the
    /// second write to the same path finds a file that now exists. Keying on
    /// existence therefore asks about the session's own output, which is noise
    /// rather than protection.
    ///
    /// The cost is real and is accepted deliberately. A file **you** wrote by
    /// hand and have not committed is in the same population, and this allows it
    /// to be overwritten without a prompt. The mitigation is at the other end:
    /// a `SessionStart` hook lists the uncommitted files that already exist and
    /// raises them before any work begins, so the choice to leave them
    /// unprotected is made once, knowingly, rather than assumed on every write.
    ///
    /// Gitignored files are excluded, and stay with
    /// [`Self::is_file_pattern_ignored`]: an ignore entry is a standing decision
    /// to keep a file out of version control, so it will never gain the history
    /// this disjunct assumes is coming.
    ///
    /// Scoped so as never to overlap [`Self::is_protected`]: a repository root
    /// is excluded, since the root of a repository with nothing committed to it
    /// is untracked, and would otherwise be writable whole.
    // POLICY: remove this disjunct to make an existing untracked file ask
    // before it is overwritten.
    // gen-md: show-body
    fn is_untracked_on_allowed_branch(&self) -> bool {
        !self.is_repo_root()
            && self.exists()
            && !self.is_tracked()
            && self.facts.ignored == Ignored::No
            && self
                .repo()
                .is_some_and(|r| r.is_lane() && r.is_allowed_branch())
    }

    /// The target does not yet exist, and lies in a lane repository whose
    /// branch is allowed.
    ///
    /// Creating a new file must be possible on an allowed branch. A rule set
    /// that keys only on tracked-ness cannot express this, because a path that
    /// does not exist is not tracked.
    ///
    /// A *lane* repository, for the same reason as
    /// [`Self::is_tracked_on_allowed_branch`]. Nothing further is needed to
    /// keep it clear of [`Self::is_repo_root`]: a root that does not exist is
    /// not a root.
    ///
    /// This is also where an ignored path that does not exist yet lands, and
    /// deliberately so: creating `devcontainer.env` or a fresh `*.log` destroys
    /// nothing, while overwriting one is what
    /// [`Self::is_file_pattern_ignored`] protects against.
    // gen-md: show-body
    fn is_new_on_allowed_branch(&self) -> bool {
        !self.exists()
            && self
                .repo()
                .is_some_and(|r| r.is_lane() && r.is_allowed_branch())
    }

    /// The target is in a repository whose branch is **not** allowed.
    ///
    /// Scoped so as never to overlap [`Self::is_allowed`]: targets whose
    /// location grants the write, and targets under a gitignored directory, are
    /// excluded, since neither depends on history for recovery.
    // gen-md: show-body
    fn is_on_disallowed_branch(&self) -> bool {
        !self.location_grants_write()
            && !self.is_under_gitignored_allowed_dir()
            && self.repo().is_some_and(|r| !r.is_allowed_branch())
    }

    /// The target is in a repository that is neither the launch project nor
    /// `$CLAUDE_CONFIG_DIR`.
    ///
    /// Protected, creation included. A second checkout is not the project the
    /// session was opened against, and the guard has no basis for judging what
    /// is safe there.
    ///
    /// Scoped so as never to overlap [`Self::is_allowed`], and scoped exactly
    /// as [`Self::is_on_disallowed_branch`] is: a location grant and a
    /// gitignored directory are recoverable wherever they sit, so a checkout
    /// under `/tmp`, and a foreign checkout's build output, stay writable.
    // gen-md: show-body
    fn is_in_foreign_repo(&self) -> bool {
        !self.location_grants_write()
            && !self.is_under_gitignored_allowed_dir()
            && self.repo().is_some_and(|r| !r.is_lane())
    }

    /// The target is under no repository at all, and matches no exception.
    ///
    /// Protected, creation included. `~/.bashrc`, `/etc/hosts` and the like have
    /// no version control behind them, so a write there is the least recoverable
    /// action available.
    ///
    /// Both location grants are excluded: `/dev/null` and `/tmp/x` are under no
    /// repository either, and without the exclusion each would be both allowed
    /// and protected.
    // gen-md: show-body
    fn is_loose(&self) -> bool {
        self.repo().is_none() && !self.location_grants_write()
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
    // gen-md: show-body
    fn is_allowed(&self) -> bool {
        self.is_write_sink()
            || self.is_allowed_exception()
            || self.is_under_gitignored_allowed_dir()
            || self.is_tracked_on_allowed_branch()
            || self.is_new_on_allowed_branch()
            || self.is_untracked_on_allowed_branch()
    }

    /// **Policy surface.** The target must not be written.
    ///
    /// **Not** the complement of [`Self::is_allowed`]. The invariant is
    /// `!(is_allowed() && is_protected())`; the gap between the two is the space
    /// of targets that require confirmation.
    // gen-md: show-body
    fn is_protected(&self) -> bool {
        self.is_repo_root()
            || self.is_loose()
            // POLICY: remove this disjunct to make foreign repositories ask
            // rather than deny.
            || self.is_in_foreign_repo()
            || self.is_on_disallowed_branch()
            // POLICY: remove this disjunct to make ignored-but-irreplaceable
            // files ask rather than deny.
            || self.is_file_pattern_ignored()
    }
}

// -----------------------------------------------------------------------------
// Action classification
// -----------------------------------------------------------------------------

impl Command {
    #[doc(hidden)]
    pub fn new(action: Action, git: Vec<GitAction>) -> Command {
        Command { action, git }
    }

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
    ///
    /// `cwd` is the working directory relative paths are taken against, and
    /// nothing else: it never decides which lane a target is in. Passing it
    /// rather than reading it from the environment is what keeps the guard from
    /// inheriting the previous one's defect, where the hook process's own
    /// working directory silently chose the repository a command was judged
    /// against.
    ///
    /// The scanning itself lives in [`crate::parse`], and the facts its targets
    /// carry come from [`crate::facts`]. This is the one function here that
    /// reaches outside the module — turning a string into judgeable data is
    /// exactly the boundary, and everything below it is a pure function of what
    /// comes back.
    fn parse(command: impl AsRef<str>, cwd: impl AsRef<Path>) -> Command {
        crate::parse::parse(command.as_ref(), cwd.as_ref()).command
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
// gen-md: show-body
fn forbidden_action(action: &Action) -> bool {
    action.is_forbidden()
}

/// The command could not be parsed, or its targets could not be determined.
// gen-md: show-body
fn opaque_action(action: &Action) -> bool {
    action.is_opaque()
}

// ----- `Allow` rules -----

/// The command writes nothing.
// gen-md: show-body
fn read_only_action(action: &Action) -> bool {
    action.is_read_only()
}

/// The command writes, and **every** target it writes is allowed.
// gen-md: show-body
fn write_on_allowed_targets(action: &Action) -> bool {
    matches!(action, Action::Write(_)) && action.eval_predicate_all(Target::is_allowed)
}

// ----- `Deny` rules -----

/// The command writes, and **any** target it writes is protected.
// gen-md: show-body
fn write_on_protected_targets(action: &Action) -> bool {
    action.eval_predicate_any(Target::is_protected)
}

// -----------------------------------------------------------------------------
// Rule composition
// -----------------------------------------------------------------------------

/// Judges the file dimension.
///
/// Falling through every rule yields [`Verdict::Ask`]: the command was
/// understood, but its targets are neither clearly safe nor clearly protected.
// gen-md: show-body
fn check_action(action: &Action) -> Verdict {
    // Short-circuit checks.
    if forbidden_action(action) {
        return Verdict::Deny;
    }
    if opaque_action(action) {
        return Verdict::Ask;
    }

    // `Allow` rules.
    if read_only_action(action) || write_on_allowed_targets(action) {
        return Verdict::Allow;
    }

    // `Deny` rules.
    if write_on_protected_targets(action) {
        return Verdict::Deny;
    }

    Verdict::Ask
}

impl GitAction {
    /// The verdict this git operation contributes on its own.
    // gen-md: show-body
    fn verdict(&self) -> Verdict {
        match self {
            GitAction::Read => Verdict::Allow,
            GitAction::StateChange(repo) if governed(repo) => Verdict::Allow,
            GitAction::StateChange(_) => Verdict::Deny,
            GitAction::Destructive(repo) if governed(repo) => Verdict::Ask,
            GitAction::Destructive(_) => Verdict::Deny,
            GitAction::ConfigWrite => Verdict::Deny,
        }
    }
}

/// The repository a git operation runs in is one the session may change.
///
/// Running under no repository is governed: there is no branch to protect, and
/// whatever files the operation touches are judged by the file dimension in the
/// ordinary way.
// gen-md: show-body
fn governed(repo: &Option<Repo>) -> bool {
    repo.as_ref()
        .is_none_or(|r| r.is_lane() && r.is_allowed_branch())
}

/// Judges the git dimension: the worst verdict among the git operations found.
// gen-md: show-body
fn check_git(git: &[GitAction]) -> Verdict {
    git.iter()
        .map(|g| g.verdict())
        .max()
        .unwrap_or(Verdict::Allow)
}

// -----------------------------------------------------------------------------
// Command checking
// -----------------------------------------------------------------------------

/// The verdict for an already-classified command: the worse of its two
/// dimensions.
///
/// The entry point for a caller that builds its own [`Command`]. The file tools
/// deliver a `file_path` rather than a command line, and are turned into an
/// [`Action::Write`] over a single [`TargetedEffect`] so that both entry points
/// are judged by one rule table rather than two that can drift apart.
// gen-md: show-body
pub fn check(command: &Command) -> Verdict {
    check_action(&command.action).max(check_git(&command.git))
}

/// The verdict for a command string.
// gen-md: show-body
pub fn check_command(command: impl AsRef<str>, cwd: impl AsRef<Path>) -> Verdict {
    check(&Command::parse(command, cwd))
}
