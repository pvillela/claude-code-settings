//! These are the rules governing what actions targeting files are allowed or disallowed.
//! The terms and rules are expressed in Rust skeleton code.
//!
//! Not all functions below may need to be implemented.

// -----------------------------------------------------------------------------
// Terms
// -----------------------------------------------------------------------------

use std::path::PathBuf;

/// Anything that is a file in the general Unix sense (includes directories, devices, etc.).
struct Target {
    #[allow(unused)]
    path: PathBuf,
}

/// A set of elements of type [`Target`]
pub struct TargetSet {
    targets: Vec<Target>,
}

/// Command (Bash expression, program, or script) that acts upon [`TargetSet`]s.
pub enum Action {
    /// The action is read-only.
    ReadOnly,
    /// The action creates files on all of its targets.
    Create,
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
    #[allow(unused)]
    /// The set of allowed exceptions for write actions.
    /// Includes the `~/.claude`` directory, scratchpads, and other allowed exceptions.
    fn allowed_exceptions() -> TargetSet {
        todo!()
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
        todo!()
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
    /// The parser must classify the resulting action as:
    /// - [`Action::Change`] if any portion of the command is changes a target and no portion of
    ///   the command is forbidden or opaque.
    /// - [`Action::Forbidden`] if any portion of the command is forbidden.
    /// - [`Action::Opaque`] if it cannot parse any portion of the command.
    ///
    /// This function can be more or less complex, depending on how far we want to
    /// go with the rules.
    fn parse(_command: impl AsRef<str>) -> Action {
        todo!()
    }

    /// The action is read-only.
    fn is_read_only(&self) -> bool {
        matches!(self, Action::ReadOnly)
    }

    /// `self`'s action kind is [`ActionKind::Forbidden`].
    fn is_forbidden(&self) -> bool {
        matches!(self, Action::Forbidden)
    }

    /// `self`'s action kind is [`ActionKind::Opaque`].
    fn is_opaque(&self) -> bool {
        matches!(self, Action::Opaque)
    }

    fn eval_predicate_all(&self, f: impl Fn(&Target) -> bool) -> bool {
        match &self {
            Action::Change(target_set) => target_set.targets.iter().all(f),
            _ => true,
        }
    }

    fn eval_predicate_any(&self, f: impl Fn(&Target) -> bool) -> bool {
        match &self {
            Action::Change(target_set) => target_set.targets.iter().any(f),
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

fn change_on_allowed_targets(action: &Action) -> bool {
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
    if read_only_on_any_target(action) || change_on_allowed_targets(action) {
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
