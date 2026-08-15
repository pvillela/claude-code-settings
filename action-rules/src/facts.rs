//! Facts about paths and repositories, and the resolver that produces them.
//!
//! [`crate::action_rules`] holds the policy; this module holds the evidence.
//! The split is structural rather than conventional: the rules read
//! [`TargetFacts`] values and have no means of reaching a subprocess or the
//! disk, because everything they need has already arrived as data.
//!
//! The facts carried here are **raw**. Lane membership, branch permission and
//! everything else built on top of them is derived in `action_rules`.

use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Write as _,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

/// How git ignores a path.
///
/// The distinction matters because the two populations have opposite
/// recoverability: a build directory's contents are reproducible, while a file
/// ignored by a pattern of its own — a credential, a spreadsheet — has neither
/// history nor a way to regenerate it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ignored {
    /// Not ignored.
    No,
    /// The path is a directory every file under which is ignored, or lies
    /// under such a directory.
    ContentsRecursivelyIgnored,
    /// The path is ignored by a pattern that matches it directly, without any
    /// ancestor directory being recursively ignored.
    FilePattern,
}

/// Raw facts about a git repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoFacts {
    /// Absolute, symlink-resolved path of the repository's top level.
    pub root: PathBuf,
    /// The checked-out branch, or `None` when `HEAD` is detached.
    pub branch: Option<String>,
    /// The non-comment, non-blank lines of `<root>/.claude/allowed-branches`,
    /// or empty when that file does not exist.
    ///
    /// Raw: the union with the fallback branch is policy, and is applied by
    /// [`crate::action_rules`].
    pub allowed_branches_file: Vec<String>,
    /// This repository is the one Claude Code was launched in.
    pub is_launch_project: bool,
    /// This repository is `$CLAUDE_CONFIG_DIR`.
    pub is_claude_home: bool,
}

/// Raw facts about one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetFacts {
    /// Absolute, symlink-resolved path.
    pub path: PathBuf,
    /// The path exists on disk.
    pub exists: bool,
    /// The path exists and is a directory.
    pub is_dir: bool,
    /// The repository governing the path, or `None` if it is under none.
    ///
    /// Shared rather than cloned so that every target in one command sees the
    /// same repository facts, resolved once.
    pub repo: Option<Arc<RepoFacts>>,
    /// The path is the top level of its repository.
    pub is_repo_root: bool,
    /// The path is tracked, or is a directory containing a tracked file at any
    /// depth.
    pub tracked: bool,
    /// How git ignores the path.
    pub ignored: Ignored,
}

impl TargetFacts {
    /// Facts for a path under no repository, with nothing on disk.
    ///
    /// The starting point for the unit tests, which fabricate facts rather
    /// than building repositories.
    pub fn bare(path: impl Into<PathBuf>) -> Self {
        TargetFacts {
            path: path.into(),
            exists: false,
            is_dir: false,
            repo: None,
            is_repo_root: false,
            tracked: false,
            ignored: Ignored::No,
        }
    }
}

// -----------------------------------------------------------------------------
// Git invocation
// -----------------------------------------------------------------------------

/// How long a single git invocation may take before the resolver gives up.
///
/// Exceeding it is a resolution failure, which the hook reports as `Ask`. A
/// guard that hangs is worse than one that asks.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

struct GitOutput {
    ok: bool,
    stdout: String,
    stderr: String,
}

/// Runs one git command in `dir`, hardened and bounded.
///
/// `stdin` is closed and the ambient `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`
/// are stripped, so the invocation cannot be redirected at a repository other
/// than the one named by `-C`, and cannot block on a credential prompt.
fn git(dir: &Path, args: &[OsString]) -> Result<GitOutput, String> {
    git_input(dir, args, None)
}

/// Runs one git command, optionally feeding it a NUL-separated path list.
///
/// `check-ignore` will not take `-z` with pathnames as arguments, and its
/// argument form quotes unusual characters, so the only encoding that survives
/// every filename is `--stdin -z`.
fn git_input(dir: &Path, args: &[OsString], input: Option<String>) -> Result<GitOutput, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run git in {}: {e}", dir.display()))?;

    // Written from a thread so that a large path list cannot deadlock against
    // the output this thread is waiting to read. Dropping the handle at the end
    // of the closure is what closes the pipe.
    if let Some(data) = input {
        let mut stdin = child.stdin.take().ok_or("git stdin was not piped")?;
        thread::spawn(move || {
            let _ = stdin.write_all(data.as_bytes());
        });
    }

    // `wait_with_output` has no timeout, so it runs on a worker thread and the
    // caller waits on the channel instead. A timed-out child is abandoned
    // rather than killed: the guard is about to exit anyway, and every command
    // issued here is a read.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(GIT_TIMEOUT) {
        Ok(Ok(out)) => Ok(GitOutput {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        }),
        Ok(Err(e)) => Err(format!("git failed in {}: {e}", dir.display())),
        Err(_) => Err(format!(
            "git timed out after {}s in {}",
            GIT_TIMEOUT.as_secs(),
            dir.display()
        )),
    }
}

/// Splits the `-z` output of a git command into paths.
fn nul_paths(out: &str) -> HashSet<PathBuf> {
    out.split('\0')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// A pathspec that git must read literally.
///
/// Without this a file called `*` — or, far more commonly, one containing `[`
/// — would be read as a glob and match the wrong set.
fn literal_pathspec(rel: &Path) -> OsString {
    let mut s = OsString::from(":(literal)");
    s.push(rel.as_os_str());
    s
}

// -----------------------------------------------------------------------------
// Repository listings
// -----------------------------------------------------------------------------

/// What one repository reports about the subtrees a command touches.
///
/// Both sets are relative to the repository root, and both are bounded by the
/// pathspecs in `scopes` — the top-level directories the command names — so a
/// large repository is never listed in full.
#[derive(Default)]
struct Listing {
    /// Tracked paths under the queried scopes.
    tracked: HashSet<PathBuf>,
    /// Untracked, non-ignored paths under the queried scopes.
    untracked: HashSet<PathBuf>,
    /// Scopes already queried.
    scopes: HashSet<PathBuf>,
    /// Paths `git check-ignore` matched, among those asked about.
    ignored: HashSet<PathBuf>,
    /// Paths already asked about with `check-ignore`.
    ignore_queried: HashSet<PathBuf>,
}

// -----------------------------------------------------------------------------
// The resolver
// -----------------------------------------------------------------------------

/// Turns paths into [`TargetFacts`].
///
/// One resolver serves one command: its memoisation exists so that the several
/// targets of `mv a b c dir/` cost two git invocations for the repository they
/// share rather than two apiece, not to survive between hook invocations.
pub struct Resolver {
    /// Top level of the project Claude Code was launched in.
    ///
    /// Fixed at construction from `CLAUDE_PROJECT_DIR`, so that a `cd` during
    /// the session cannot move the lane boundary.
    launch_project: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR`, symlink-resolved.
    claude_home: PathBuf,
    /// Working directory that relative paths are taken against.
    cwd: PathBuf,
    /// Directory -> the repository root containing it, if any.
    repo_of_dir: HashMap<PathBuf, Option<PathBuf>>,
    /// Repository root -> its facts.
    repos: HashMap<PathBuf, Arc<RepoFacts>>,
    /// Repository root -> what has been listed for it.
    listings: HashMap<PathBuf, Listing>,
}

impl Resolver {
    /// Builds a resolver from the hook's environment.
    ///
    /// `cwd` is the working directory reported in the payload, used only to
    /// anchor relative paths. The launch project comes from
    /// `CLAUDE_PROJECT_DIR` and never from `cwd`.
    ///
    /// Failing to establish the launch project is an error rather than a
    /// silently empty lane: without it every repository would look foreign,
    /// and the guard would start denying ordinary work for an environmental
    /// reason the user cannot see.
    pub fn from_env(cwd: impl Into<PathBuf>) -> Result<Self, String> {
        let claude_home = match env::var_os("CLAUDE_CONFIG_DIR") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let home =
                    env::var_os("HOME").ok_or("neither CLAUDE_CONFIG_DIR nor HOME is set")?;
                PathBuf::from(home).join(".claude")
            }
        };

        let raw = env::var_os("CLAUDE_PROJECT_DIR")
            .filter(|v| !v.is_empty())
            .ok_or("CLAUDE_PROJECT_DIR is not set, so the launch project cannot be established")?;
        let project_dir = PathBuf::from(raw);
        // The stateless sanity check: the variable must name a directory that
        // exists. Anything else is a broken environment, not a lane.
        if !project_dir.is_dir() {
            return Err(format!(
                "CLAUDE_PROJECT_DIR is not a directory: {}",
                project_dir.display()
            ));
        }
        let project_dir = canonicalize(&project_dir);

        let mut resolver = Resolver {
            launch_project: None,
            claude_home: canonicalize(&claude_home),
            cwd: canonicalize(&cwd.into()),
            repo_of_dir: HashMap::new(),
            repos: HashMap::new(),
            listings: HashMap::new(),
        };
        resolver.launch_project = resolver.repo_root_of_dir(&project_dir)?;
        Ok(resolver)
    }

    /// Builds a resolver with the lanes given explicitly, for tests.
    pub fn with_lanes(
        cwd: impl Into<PathBuf>,
        launch_project: Option<PathBuf>,
        claude_home: impl Into<PathBuf>,
    ) -> Self {
        Resolver {
            launch_project: launch_project.map(|p| canonicalize(&p)),
            claude_home: canonicalize(&claude_home.into()),
            cwd: canonicalize(&cwd.into()),
            repo_of_dir: HashMap::new(),
            repos: HashMap::new(),
            listings: HashMap::new(),
        }
    }

    /// The working directory relative paths are resolved against.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Resolves every path of one command together.
    ///
    /// Batched rather than resolved one at a time: the two listings a
    /// repository needs are issued once for all the targets that share it.
    pub fn resolve_all(&mut self, paths: &[PathBuf]) -> Result<Vec<TargetFacts>, String> {
        let normalized: Vec<PathBuf> = paths.iter().map(|p| self.normalize(p)).collect();

        let mut repos: Vec<Option<Arc<RepoFacts>>> = Vec::with_capacity(normalized.len());
        for path in &normalized {
            repos.push(self.repo_facts_for(path)?);
        }

        // Batch: every scope and every check-ignore query, grouped by
        // repository, issued before any target is judged.
        let mut wanted: HashMap<PathBuf, (HashSet<PathBuf>, HashSet<PathBuf>)> = HashMap::new();
        for (path, repo) in normalized.iter().zip(&repos) {
            let Some(repo) = repo else { continue };
            let Some(rel) = relative(&repo.root, path) else {
                continue;
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            let entry = wanted.entry(repo.root.clone()).or_default();
            if let Some(first) = rel.components().next() {
                entry.0.insert(PathBuf::from(first.as_os_str()));
            }
            // check-ignore is asked about the target and about every ancestor
            // directory that stands between it and the repository root.
            for anc in ancestors_within(&rel) {
                entry.1.insert(anc);
            }
            entry.1.insert(rel);
        }
        for (root, (scopes, queries)) in &wanted {
            self.load_listing(root, scopes)?;
            self.load_check_ignore(root, queries)?;
        }

        let mut facts = Vec::with_capacity(normalized.len());
        for (path, repo) in normalized.into_iter().zip(repos) {
            facts.push(self.facts_for(path, repo));
        }
        Ok(facts)
    }

    /// The repository containing `dir`, if any.
    ///
    /// The git dimension needs this without needing any target: `git commit`
    /// names no path, but its verdict still turns on which repository it runs
    /// in and on what branch.
    pub fn repo_of(&mut self, dir: &Path) -> Result<Option<Arc<RepoFacts>>, String> {
        let normalized = self.normalize(dir);
        self.repo_facts_for(&normalized)
    }

    /// Resolves a single path.
    pub fn resolve(&mut self, path: impl AsRef<Path>) -> Result<TargetFacts, String> {
        let mut all = self.resolve_all(&[path.as_ref().to_path_buf()])?;
        Ok(all.remove(0))
    }

    // -------------------------------------------------------------------------
    // Internals
    // -------------------------------------------------------------------------

    /// Makes a path absolute, lexically clean, and symlink-resolved as far as
    /// it exists.
    ///
    /// A path that does not exist yet still has to be judged — creating a file
    /// is the ordinary case — so the deepest existing ancestor is canonicalised
    /// and the remainder appended to it.
    fn normalize(&self, path: &Path) -> PathBuf {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let cleaned = lexical_clean(&absolute);

        let mut prefix = cleaned.as_path();
        let mut tail: Vec<&OsStr> = Vec::new();
        loop {
            if prefix.exists() {
                let mut resolved = canonicalize(prefix);
                for part in tail.iter().rev() {
                    resolved.push(part);
                }
                return resolved;
            }
            match (prefix.file_name(), prefix.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    prefix = parent;
                }
                _ => return cleaned,
            }
        }
    }

    /// The repository root containing `dir`, memoised per directory.
    fn repo_root_of_dir(&mut self, dir: &Path) -> Result<Option<PathBuf>, String> {
        if let Some(hit) = self.repo_of_dir.get(dir) {
            return Ok(hit.clone());
        }
        let out = git(
            dir,
            &[
                OsString::from("rev-parse"),
                OsString::from("--show-toplevel"),
            ],
        );
        let root = match out {
            // A directory outside every repository makes git exit non-zero.
            // That is an answer, not a failure.
            Ok(o) if !o.ok => None,
            Ok(o) => {
                let line = o.stdout.trim();
                if line.is_empty() {
                    None
                } else {
                    Some(canonicalize(Path::new(line)))
                }
            }
            Err(e) => return Err(e),
        };
        self.repo_of_dir.insert(dir.to_path_buf(), root.clone());
        Ok(root)
    }

    /// The repository facts for the repository containing `path`, if any.
    fn repo_facts_for(&mut self, path: &Path) -> Result<Option<Arc<RepoFacts>>, String> {
        let dir = nearest_existing_dir(path);
        let Some(dir) = dir else { return Ok(None) };
        let Some(root) = self.repo_root_of_dir(&dir)? else {
            return Ok(None);
        };
        if let Some(hit) = self.repos.get(&root) {
            return Ok(Some(hit.clone()));
        }
        let facts = Arc::new(self.load_repo(&root)?);
        self.repos.insert(root, facts.clone());
        Ok(Some(facts))
    }

    /// Reads the branch, the allowed-branches file, and the lane membership of
    /// one repository.
    fn load_repo(&self, root: &Path) -> Result<RepoFacts, String> {
        let out = git(
            root,
            &[
                OsString::from("symbolic-ref"),
                OsString::from("--quiet"),
                OsString::from("--short"),
                OsString::from("HEAD"),
            ],
        )?;
        // A detached HEAD makes `symbolic-ref` exit non-zero with no output,
        // which is exactly the `None` the rules expect.
        let branch = if out.ok {
            let name = out.stdout.trim();
            (!name.is_empty()).then(|| name.to_owned())
        } else {
            None
        };

        let allowed_file = root.join(".claude/allowed-branches");
        let allowed_branches_file = match fs::read_to_string(&allowed_file) {
            Ok(text) => text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_owned)
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            // Present but unreadable is not the same as absent: treating it as
            // absent would silently narrow the allowed set.
            Err(e) => return Err(format!("cannot read {}: {e}", allowed_file.display())),
        };

        Ok(RepoFacts {
            root: root.to_path_buf(),
            branch,
            allowed_branches_file,
            is_launch_project: self.launch_project.as_deref() == Some(root),
            is_claude_home: root == self.claude_home,
        })
    }

    /// Issues the two listings that decide whether a directory's contents are
    /// wholly ignored, for any scope not already covered.
    fn load_listing(&mut self, root: &Path, scopes: &HashSet<PathBuf>) -> Result<(), String> {
        let known = self
            .listings
            .get(root)
            .map(|l| l.scopes.clone())
            .unwrap_or_default();
        let fresh: Vec<PathBuf> = scopes.difference(&known).cloned().collect();
        if fresh.is_empty() {
            return Ok(());
        }

        let mut tracked_args = vec![OsString::from("ls-files"), OsString::from("-z")];
        let mut untracked_args = vec![
            OsString::from("ls-files"),
            OsString::from("-z"),
            OsString::from("--others"),
            OsString::from("--exclude-standard"),
        ];
        for args in [&mut tracked_args, &mut untracked_args] {
            args.push(OsString::from("--"));
            for scope in &fresh {
                args.push(literal_pathspec(scope));
            }
        }

        let tracked = git(root, &tracked_args)?;
        if !tracked.ok {
            return Err(format!(
                "git ls-files failed in {}: {}",
                root.display(),
                tracked.stderr
            ));
        }
        let untracked = git(root, &untracked_args)?;
        if !untracked.ok {
            return Err(format!(
                "git ls-files --others failed in {}: {}",
                root.display(),
                untracked.stderr
            ));
        }

        let listing = self.listings.entry(root.to_path_buf()).or_default();
        listing.tracked.extend(nul_paths(&tracked.stdout));
        listing.untracked.extend(nul_paths(&untracked.stdout));
        listing.scopes.extend(fresh);
        Ok(())
    }

    /// Asks `check-ignore` about every path not already asked about.
    fn load_check_ignore(&mut self, root: &Path, paths: &HashSet<PathBuf>) -> Result<(), String> {
        let known = self
            .listings
            .get(root)
            .map(|l| l.ignore_queried.clone())
            .unwrap_or_default();
        let fresh: Vec<PathBuf> = paths.difference(&known).cloned().collect();
        if fresh.is_empty() {
            return Ok(());
        }

        let args = vec![
            OsString::from("check-ignore"),
            OsString::from("-z"),
            OsString::from("--stdin"),
        ];
        let mut input = String::new();
        for path in &fresh {
            input.push_str(&path.to_string_lossy());
            input.push('\0');
        }
        let out = git_input(root, &args, Some(input))?;
        // Exit 1 means "nothing matched", which is an answer. Anything above
        // that is a real failure.
        if !out.ok && !out.stderr.is_empty() {
            return Err(format!(
                "git check-ignore failed in {}: {}",
                root.display(),
                out.stderr
            ));
        }

        let listing = self.listings.entry(root.to_path_buf()).or_default();
        listing.ignored.extend(nul_paths(&out.stdout));
        listing.ignore_queried.extend(fresh);
        Ok(())
    }

    /// Assembles the facts for one already-batched path.
    fn facts_for(&self, path: PathBuf, repo: Option<Arc<RepoFacts>>) -> TargetFacts {
        let exists = path.exists();
        let is_dir = path.is_dir();

        let Some(repo) = repo else {
            return TargetFacts {
                path,
                exists,
                is_dir,
                repo: None,
                is_repo_root: false,
                tracked: false,
                ignored: Ignored::No,
            };
        };

        let is_repo_root = path == repo.root;
        let rel = relative(&repo.root, &path).unwrap_or_default();
        let empty = Listing::default();
        let listing = self.listings.get(&repo.root).unwrap_or(&empty);

        // A directory counts as tracked when anything under it is tracked, so
        // that `rm -rf src/` carries the branch rule and a repository-wide git
        // operation gets a verdict through its root.
        let tracked = if is_repo_root {
            !listing.tracked.is_empty()
        } else {
            listing.tracked.iter().any(|t| t.starts_with(&rel))
        };

        let ignored = if tracked {
            Ignored::No
        } else if ancestors_within(&rel)
            .iter()
            .any(|anc| self.dir_contents_all_ignored(&repo.root, listing, anc))
            || (is_dir && !is_repo_root && self.dir_contents_all_ignored(&repo.root, listing, &rel))
        {
            Ignored::ContentsRecursivelyIgnored
        } else if listing.ignored.contains(&rel) {
            Ignored::FilePattern
        } else {
            Ignored::No
        };

        TargetFacts {
            path,
            exists,
            is_dir,
            repo: Some(repo),
            is_repo_root,
            tracked,
            ignored,
        }
    }

    /// Every file under this directory is ignored.
    ///
    /// Two independent witnesses, because neither alone is right:
    ///
    /// - Nothing under it is tracked and nothing under it is untracked-and-not-
    ///   ignored. This is the test that works under a deny-all-then-allowlist
    ///   `.gitignore`, where directories are re-included by `!**/` so that git
    ///   can descend into them while every file inside stays ignored — there,
    ///   testing the directory's own path gives the wrong answer.
    /// - `check-ignore` matches the directory itself. This is what still
    ///   answers for a `target/` that a `cargo clean` has just emptied.
    ///
    /// The first witness requires the directory to exist and to hold something,
    /// since "every file under it is ignored" is vacuous otherwise. Without
    /// that requirement a path that does not exist yet would inherit the
    /// verdict of its own absent parent, and `<repo>/newdir/newfile` would be
    /// writable on any branch.
    fn dir_contents_all_ignored(&self, root: &Path, listing: &Listing, rel: &Path) -> bool {
        if rel.as_os_str().is_empty() {
            return false;
        }
        if listing.ignored.contains(rel) {
            return true;
        }
        let absolute = root.join(rel);
        if !absolute.is_dir() || is_empty_dir(&absolute) {
            return false;
        }
        !listing.tracked.iter().any(|p| p.starts_with(rel))
            && !listing.untracked.iter().any(|p| p.starts_with(rel))
    }
}

// -----------------------------------------------------------------------------
// Path helpers
// -----------------------------------------------------------------------------

/// Resolves symlinks where possible, and leaves the path alone where not.
fn canonicalize(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| lexical_clean(path))
}

/// Removes `.` and collapses `..` without touching the filesystem.
fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// The deepest ancestor of `path` that exists and is a directory.
fn nearest_existing_dir(path: &Path) -> Option<PathBuf> {
    let mut candidate = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    while let Some(dir) = candidate {
        if dir.is_dir() {
            return Some(dir.to_path_buf());
        }
        candidate = dir.parent();
    }
    None
}

/// `path` expressed relative to `root`, or `None` if it is not under it.
fn relative(root: &Path, path: &Path) -> Option<PathBuf> {
    path.strip_prefix(root).ok().map(Path::to_path_buf)
}

/// The proper ancestors of a repository-relative path, excluding the root.
///
/// For `a/b/c` this is `a` and `a/b`. The repository root is deliberately not
/// among them: it is protected in its own right, and letting it be judged as a
/// wholly-ignored directory would hand out the whole repository.
fn ancestors_within(rel: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    let components: Vec<_> = rel.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        acc.push(component.as_os_str());
        out.push(acc.clone());
    }
    out
}

/// The directory holds no entries at all.
fn is_empty_dir(path: &Path) -> bool {
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}
