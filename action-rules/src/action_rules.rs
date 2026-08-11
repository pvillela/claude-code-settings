//! These are the rules governing what actions targeting files are allowed or disallowed.
//! The terms and rules are expressed in Rust skeleton code.
//!
//! Not all functions below may need to be implemented.

// -----------------------------------------------------------------------------
// Terms
// -----------------------------------------------------------------------------

use std::{path::PathBuf, sync::LazyLock};

use regex::Regex;

/// Anything that is a file in the general Unix sense (includes directories, devices, etc.).
struct Target {
    #[allow(unused)]
    path: PathBuf,
}

/// A set of elements of type [`Target`]
struct TargetSet {
    targets: Vec<Target>,
}

/// Command (Bash expression, program, or script) that acts upon [`TargetSet`]s.
enum Action {
    /// The action is read-only.
    ReadOnly,
    /// The action creates files on all of its targets.
    Create(TargetSet),
    /// The action changes one or more of its targets.
    Change(TargetSet),
    /// The action is forbidden, e.g., `chown` or a composite command that includes `chown`.
    Forbidden,
    /// The action parser cannot parse the command or is unable to determine the command's
    /// target set.
    Opaque,
}

// -----------------------------------------------------------------------------
// Primary predicates and other functions
// -----------------------------------------------------------------------------

impl Target {
    /// The set of allowed exceptions for write actions.
    /// Includes the `~/.claude`` directory, scratchpads, and other allowed exceptions.
    // *** Below is illustrative only and may be incorrect ***
    pub const ALLOWED_PATH_REGEX_STRS: [&'static str; 3] =
        [r"^~/.claude", "^/workspaces/scratch", "^/tmp"];

    /// The set of allowed exceptions for write actions.
    /// Includes the `~/.claude`` directory, scratchpads, and other allowed exceptions.
    fn allowed_exceptions() -> &'static Vec<Regex> {
        static ALLOWED_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
            Target::ALLOWED_PATH_REGEX_STRS
                .iter()
                .map(|re| Regex::new(re).expect(&format!("invalid regex '{re}'")))
                .collect()
        });
        &ALLOWED_REGEXES
    }

    /// The target is under the directory from which Claude Code was launched.
    fn is_in_launch_project(&self) -> bool {
        todo!()
    }

    /// The target is under a directory that is the root of a Git repo.
    fn is_under_git_root(&self) -> bool {
        todo!()
    }

    /// The target [`Self::is_git_controlled()`] AND in an allowed branch.
    fn is_in_allowed_branch(&self) -> bool {
        todo!()
    }

    /// The target is a folder that is entirely gitignored or is under such a folder.
    ///
    /// If this is true `then` `!`[`Self::is_git_controlled()`] is `true`.
    fn is_under_full_folder_gitignore(&self) -> bool {
        todo!()
    }

    /// `self` is directly or indirectly in [`Self::allowed_exceptions()`]
    fn is_allowed_exception(&self) -> bool {
        Self::allowed_exceptions().iter().any(|re| {
            re.is_match(
                self.path
                    .as_path()
                    .to_str()
                    .expect("path should have been validated at construction"),
            )
        })
    }

    fn is_allowed(&self) -> bool {
        self.is_in_launch_project()
            && (self.is_in_allowed_branch() || self.is_under_full_folder_gitignore())
            || (self.is_allowed_exception()
                && (self.is_in_allowed_branch() || self.is_under_full_folder_gitignore())
                || !self.is_under_git_root())
    }
}

impl Action {
    /// Parses a `command` string to produce an [`Action`].
    ///
    /// The parser must classify the resulting action in the following order:
    /// - [`Action::Forbidden`] if any portion of the command is forbidden.
    /// - [`Action::Opaque`] if it cannot parse any portion of the command.
    /// - [`Action::Change`] if any portion of the command changes a target.
    /// - [`Action::Create`] if any portion of the command creates a target.
    /// - [`Action::ReadOnly`] if all portions of the command are read-only.
    ///
    /// This function can be more or less complex, depending on how far we want to
    /// go with the rules.
    fn parse(_command: impl AsRef<str>) -> Action {
        todo!()
    }

    /// `self`'s action kind is [`ActionKind::Forbidden`].
    fn is_forbidden(&self) -> bool {
        matches!(self, Action::Forbidden)
    }

    /// `self`'s action kind is [`ActionKind::Opaque`].
    fn is_opaque(&self) -> bool {
        matches!(self, Action::Opaque)
    }

    /// The action is read-only.
    fn is_change(&self) -> bool {
        matches!(self, Action::Change(_))
    }

    /// The action is read-only.
    fn is_create(&self) -> bool {
        matches!(self, Action::Create(_))
    }

    /// The action is read-only.
    fn is_read_only(&self) -> bool {
        matches!(self, Action::ReadOnly)
    }

    fn eval_predicate_all(&self, f: impl Fn(&Target) -> bool) -> bool {
        match &self {
            Action::Change(ts) | Action::Create(ts) => ts.targets.iter().all(f),
            _ => true,
        }
    }

    fn eval_predicate_any(&self, f: impl Fn(&Target) -> bool) -> bool {
        match &self {
            Action::Change(ts) | Action::Create(ts) => ts.targets.iter().any(f),
            _ => false,
        }
    }
}

// -----------------------------------------------------------------------------
// Rules
// -----------------------------------------------------------------------------

// ----- Short-circuit -----

fn forbidden_on_any_target(action: &Action) -> bool {
    action.is_forbidden()
}

fn opaque_on_any_target(action: &Action) -> bool {
    action.is_opaque()
}

// ----- `Allow` Rules -----

fn read_only_on_any_target(action: &Action) -> bool {
    action.is_read_only()
}

fn create_on_allowed_targets(action: &Action) -> bool {
    if !action.is_create() {
        return false;
    }
    action.eval_predicate_all(|t| t.is_allowed())
}

fn change_on_allowed_targets(action: &Action) -> bool {
    if !action.is_change() {
        return false;
    }
    action.eval_predicate_all(|t| t.is_allowed())
}

// ----- `Deny` Rules -----

fn change_on_disallowed_targets(action: &Action) -> bool {
    action.eval_predicate_any(|t| !t.is_allowed())
}

// -----------------------------------------------------------------------------
// Rule composition
// -----------------------------------------------------------------------------

pub enum Outcome {
    Allow,
    Deny,
    Ask,
}

fn check_action(action: &Action) -> Outcome {
    // Short-circuit checks.
    if forbidden_on_any_target(action) {
        return Outcome::Deny;
    }
    if opaque_on_any_target(action) {
        return Outcome::Ask;
    }

    // `Allow` rules
    if read_only_on_any_target(action)
        || create_on_allowed_targets(action)
        || change_on_allowed_targets(action)
    {
        return Outcome::Allow;
    }

    // `Deny` rules
    if change_on_disallowed_targets(action) {
        return Outcome::Deny;
    }

    Outcome::Ask
}

// -----------------------------------------------------------------------------
// Command checking
// -----------------------------------------------------------------------------

pub fn check_command(command: impl AsRef<str>) -> Outcome {
    let action = Action::parse(command);
    check_action(&action)
}
