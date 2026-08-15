//! Unit tests for the rules, over fabricated facts.
//!
//! No temporary repositories, no subprocesses, no mocking: every test states
//! the facts it means to test and reads the verdict back. That is the whole
//! point of resolving facts elsewhere — a rule becomes a pure function of a
//! value, and a value is cheap to write down.
//!
//! End-to-end coverage, against real git and real payloads, is the shell suite.

use std::{path::PathBuf, sync::Arc};

use super::*;
use crate::facts::{Ignored, RepoFacts, TargetFacts};

// -----------------------------------------------------------------------------
// Fabrication
// -----------------------------------------------------------------------------

/// Which lane a fabricated repository sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Launch,
    ClaudeHome,
    Foreign,
}

fn repo_facts(lane: Lane, branch: Option<&str>) -> Arc<RepoFacts> {
    Arc::new(RepoFacts {
        root: PathBuf::from("/repo"),
        branch: branch.map(str::to_owned),
        allowed_branches_file: Vec::new(),
        is_launch_project: lane == Lane::Launch,
        is_claude_home: lane == Lane::ClaudeHome,
    })
}

/// A target with everything false, which each test then contradicts.
fn target(path: &str) -> TargetFacts {
    TargetFacts::bare(path)
}

fn judge(facts: TargetFacts) -> (bool, bool) {
    let t = Target::new(facts);
    (t.is_allowed(), t.is_protected())
}

fn write(facts: TargetFacts) -> Action {
    Action::Write(vec![TargetedEffect::new(
        Target::new(facts),
        Effect::Change,
    )])
}

// -----------------------------------------------------------------------------
// Repo predicates
// -----------------------------------------------------------------------------

#[test]
fn allowed_branches_is_a_union_not_a_replacement() {
    let mut facts = (*repo_facts(Lane::Launch, Some("main"))).clone();
    facts.allowed_branches_file = vec!["release".to_owned(), "spike".to_owned()];
    let repo = Repo::new(Arc::new(facts));

    let branches = repo.allowed_branches();
    assert!(branches.iter().any(|b| b == Repo::FALLBACK_ALLOWED_BRANCH));
    assert!(branches.iter().any(|b| b == "release"));
    assert!(branches.iter().any(|b| b == "spike"));
}

#[test]
fn the_fallback_branch_is_allowed_with_no_file() {
    assert!(Repo::new(repo_facts(Lane::Launch, Some("aicode"))).is_allowed_branch());
    assert!(!Repo::new(repo_facts(Lane::Launch, Some("main"))).is_allowed_branch());
}

#[test]
fn a_detached_head_is_never_allowed() {
    assert!(!Repo::new(repo_facts(Lane::Launch, None)).is_allowed_branch());
}

#[test]
fn only_the_launch_project_and_claude_home_are_lanes() {
    assert!(Repo::new(repo_facts(Lane::Launch, Some("aicode"))).is_lane());
    assert!(Repo::new(repo_facts(Lane::ClaudeHome, Some("aicode"))).is_lane());
    assert!(!Repo::new(repo_facts(Lane::Foreign, Some("aicode"))).is_lane());
}

// -----------------------------------------------------------------------------
// Every disjunct of `is_allowed`
// -----------------------------------------------------------------------------

#[test]
fn a_write_sink_is_allowed() {
    let (allowed, protected) = judge(target("/dev/null"));
    assert!(allowed && !protected);
}

#[test]
fn a_scratch_path_is_allowed() {
    for path in ["/tmp/x", "/var/tmp/x", "/workspaces/scratch/x"] {
        let (allowed, protected) = judge(target(path));
        assert!(allowed && !protected, "{path}");
    }
}

#[test]
fn a_wholly_ignored_directory_is_allowed_on_any_branch() {
    let mut facts = target("/repo/target/debug/x");
    facts.repo = Some(repo_facts(Lane::Launch, Some("main")));
    facts.exists = true;
    facts.ignored = Ignored::ContentsRecursivelyIgnored;

    let (allowed, protected) = judge(facts);
    assert!(allowed && !protected);
}

#[test]
fn a_tracked_file_on_an_allowed_branch_is_allowed() {
    let mut facts = target("/repo/src/main.rs");
    facts.repo = Some(repo_facts(Lane::Launch, Some("aicode")));
    facts.exists = true;
    facts.tracked = true;

    let (allowed, protected) = judge(facts);
    assert!(allowed && !protected);
}

#[test]
fn a_new_file_on_an_allowed_branch_is_allowed() {
    let mut facts = target("/repo/src/new.rs");
    facts.repo = Some(repo_facts(Lane::Launch, Some("aicode")));

    let (allowed, protected) = judge(facts);
    assert!(allowed && !protected);
}

// -----------------------------------------------------------------------------
// Every disjunct of `is_protected`
// -----------------------------------------------------------------------------

#[test]
fn a_repository_root_is_protected_even_on_an_allowed_branch() {
    let mut facts = target("/repo");
    facts.repo = Some(repo_facts(Lane::Launch, Some("aicode")));
    facts.exists = true;
    facts.is_dir = true;
    facts.is_repo_root = true;
    facts.tracked = true;

    let (allowed, protected) = judge(facts);
    assert!(!allowed && protected);
}

#[test]
fn a_loose_path_is_protected() {
    for path in ["/etc/hosts", "/home/someone/.bashrc"] {
        let (allowed, protected) = judge(target(path));
        assert!(!allowed && protected, "{path}");
    }
}

#[test]
fn a_foreign_repository_is_protected_on_every_branch() {
    for branch in ["aicode", "main"] {
        let mut facts = target("/repo/src/main.rs");
        facts.repo = Some(repo_facts(Lane::Foreign, Some(branch)));
        facts.exists = true;
        facts.tracked = true;

        let (allowed, protected) = judge(facts);
        assert!(!allowed && protected, "{branch}");
    }
}

#[test]
fn creating_a_file_in_a_foreign_repository_is_protected_too() {
    let mut facts = target("/repo/new.rs");
    facts.repo = Some(repo_facts(Lane::Foreign, Some("aicode")));

    let (allowed, protected) = judge(facts);
    assert!(!allowed && protected);
}

#[test]
fn a_disallowed_branch_protects_tracked_and_new_alike() {
    let mut tracked = target("/repo/src/main.rs");
    tracked.repo = Some(repo_facts(Lane::Launch, Some("main")));
    tracked.exists = true;
    tracked.tracked = true;
    assert_eq!(judge(tracked), (false, true));

    let mut fresh = target("/repo/src/new.rs");
    fresh.repo = Some(repo_facts(Lane::Launch, Some("main")));
    assert_eq!(judge(fresh), (false, true));
}

// -----------------------------------------------------------------------------
// The gap between the two surfaces
// -----------------------------------------------------------------------------

#[test]
fn a_file_pattern_ignored_file_neither_allows_nor_protects() {
    let mut facts = target("/repo/data/secret.xlsx");
    facts.repo = Some(repo_facts(Lane::Launch, Some("aicode")));
    facts.exists = true;
    facts.ignored = Ignored::FilePattern;

    let (allowed, protected) = judge(facts.clone());
    assert!(!allowed && !protected);
    assert_eq!(check_action(&write(facts)), Verdict::Ask);
}

#[test]
fn an_untracked_existing_file_on_an_allowed_branch_asks() {
    let mut facts = target("/repo/src/scratch.rs");
    facts.repo = Some(repo_facts(Lane::Launch, Some("aicode")));
    facts.exists = true;

    let (allowed, protected) = judge(facts.clone());
    assert!(!allowed && !protected);
    assert_eq!(check_action(&write(facts)), Verdict::Ask);
}

// -----------------------------------------------------------------------------
// The invariant
// -----------------------------------------------------------------------------

/// Fact combinations the resolver can actually produce.
///
/// The resolver's contract, written down: a root exists and is a directory
/// inside a repository, nothing outside a repository is tracked or ignored,
/// and a tracked path is never reported as ignored.
fn well_formed(facts: &TargetFacts) -> bool {
    if facts.is_dir && !facts.exists {
        return false;
    }
    if facts.is_repo_root && (facts.repo.is_none() || !facts.exists || !facts.is_dir) {
        return false;
    }
    if facts.repo.is_none() && (facts.tracked || facts.ignored != Ignored::No) {
        return false;
    }
    if facts.tracked && facts.ignored != Ignored::No {
        return false;
    }
    true
}

#[test]
fn no_target_is_both_allowed_and_protected() {
    let lanes = [
        None,
        Some((Lane::Launch, Some("aicode"))),
        Some((Lane::Launch, Some("main"))),
        Some((Lane::Launch, None)),
        Some((Lane::ClaudeHome, Some("aicode"))),
        Some((Lane::ClaudeHome, Some("main"))),
        Some((Lane::Foreign, Some("aicode"))),
        Some((Lane::Foreign, Some("main"))),
    ];
    let paths = [
        "/repo",
        "/repo/src/f.rs",
        "/tmp/f",
        "/dev/null",
        "/etc/hosts",
    ];
    let ignoreds = [
        Ignored::No,
        Ignored::ContentsRecursivelyIgnored,
        Ignored::FilePattern,
    ];

    let mut checked = 0;
    for path in paths {
        for lane in lanes {
            for exists in [false, true] {
                for is_dir in [false, true] {
                    for is_repo_root in [false, true] {
                        for tracked in [false, true] {
                            for ignored in ignoreds {
                                let mut facts = target(path);
                                facts.repo = lane.map(|(l, b)| repo_facts(l, b));
                                facts.exists = exists;
                                facts.is_dir = is_dir;
                                facts.is_repo_root = is_repo_root;
                                facts.tracked = tracked;
                                facts.ignored = ignored;
                                if !well_formed(&facts) {
                                    continue;
                                }
                                checked += 1;
                                let (allowed, protected) = judge(facts.clone());
                                assert!(
                                    !(allowed && protected),
                                    "both allowed and protected: {facts:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        checked > 500,
        "only {checked} combinations were well-formed"
    );
}

// -----------------------------------------------------------------------------
// Composition
// -----------------------------------------------------------------------------

#[test]
fn the_file_dimension_takes_the_severe_reading() {
    assert_eq!(check_action(&Action::Forbidden), Verdict::Deny);
    assert_eq!(check_action(&Action::Opaque), Verdict::Ask);
    assert_eq!(check_action(&Action::ReadOnly), Verdict::Allow);
    assert_eq!(check_action(&Action::Write(Vec::new())), Verdict::Allow);
}

#[test]
fn one_protected_target_condemns_the_whole_command() {
    let mut allowed = target("/tmp/x");
    allowed.exists = true;
    let protected = target("/etc/hosts");

    let action = Action::Write(vec![
        TargetedEffect::new(Target::new(allowed), Effect::Change),
        TargetedEffect::new(Target::new(protected), Effect::Change),
    ]);
    assert_eq!(check_action(&action), Verdict::Deny);
}

#[test]
fn one_undecided_target_takes_the_command_to_ask() {
    let allowed = target("/tmp/x");
    let mut undecided = target("/repo/src/scratch.rs");
    undecided.repo = Some(repo_facts(Lane::Launch, Some("aicode")));
    undecided.exists = true;

    let action = Action::Write(vec![
        TargetedEffect::new(Target::new(allowed), Effect::Change),
        TargetedEffect::new(Target::new(undecided), Effect::Change),
    ]);
    assert_eq!(check_action(&action), Verdict::Ask);
}

#[test]
fn the_git_dimension_follows_the_branch() {
    let governed = Some(Repo::new(repo_facts(Lane::Launch, Some("aicode"))));
    let ungoverned = Some(Repo::new(repo_facts(Lane::Launch, Some("main"))));
    let foreign = Some(Repo::new(repo_facts(Lane::Foreign, Some("aicode"))));

    assert_eq!(check_git(&[GitAction::Read]), Verdict::Allow);
    assert_eq!(
        check_git(&[GitAction::StateChange(governed.clone())]),
        Verdict::Allow
    );
    assert_eq!(
        check_git(&[GitAction::StateChange(ungoverned.clone())]),
        Verdict::Deny
    );
    assert_eq!(check_git(&[GitAction::StateChange(foreign)]), Verdict::Deny);
    assert_eq!(check_git(&[GitAction::Destructive(governed)]), Verdict::Ask);
    assert_eq!(
        check_git(&[GitAction::Destructive(ungoverned)]),
        Verdict::Deny
    );
    assert_eq!(check_git(&[GitAction::ConfigWrite]), Verdict::Deny);
    assert_eq!(check_git(&[]), Verdict::Allow);
}

#[test]
fn git_outside_every_repository_has_no_branch_to_protect() {
    assert_eq!(check_git(&[GitAction::StateChange(None)]), Verdict::Allow);
    assert_eq!(check_git(&[GitAction::Destructive(None)]), Verdict::Ask);
}

#[test]
fn neither_dimension_can_mask_the_other() {
    let command = Command::new(Action::ReadOnly, vec![GitAction::ConfigWrite]);
    assert_eq!(check(&command), Verdict::Deny);

    let mut allowed = target("/tmp/x");
    allowed.exists = true;
    let command = Command::new(write(allowed), vec![GitAction::ConfigWrite]);
    assert_eq!(check(&command), Verdict::Deny);
}
