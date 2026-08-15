//! The scanner: a command string in, the two dimensions of
//! [`crate::action_rules::Command`] out.
//!
//! Purpose-built rather than borrowed. The survey behind this choice found
//! `conch-parser` unmaintained since 2019, `brush-parser` maintained but
//! carrying fourteen dependencies, and `yash-syntax` maintained, lean and
//! async-only. None of them removes the work that dominates here: whichever
//! parser produces the tree, every construct still has to be enumerated as one
//! this module classifies or one it declares [`Action::Opaque`], and a borrowed
//! parser makes that enumeration harder rather than easier, because it succeeds
//! on syntax the classifier has never heard of.
//!
//! What it does model:
//!
//! - `'…'` inert, `"…"` live for `$` and backticks, `\` escaping.
//! - Longest-match operators, including `&&`, `>>`, `<<`, `<>`, `2>`, `&>`.
//! - Heredoc bodies, discarded.
//! - `#` as an ordinary character mid-word.
//! - One literal `cd` rebasing relative paths, with `(` as a barrier.
//! - Globs, **expanded against the filesystem**. A glob is a bounded set of
//!   real paths, so resolving it is strictly better than prompting about it.
//!
//! What it declares opaque: `$VAR`, `$(…)`, backticks and unbalanced quotes,
//! wherever they fall in a position that decides a target.
//!
//! What it accepts as a limit: a utility it does not know writes nothing. A
//! script's contents are invisible, and so is `some-tool --output f`. This is
//! the documented non-goal — completeness — and not a defect.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    action_rules::{Action, Command, Effect, GitAction, Target, TargetedEffect},
    facts::Resolver,
};

/// The result of scanning, with anything the hook should be able to explain.
pub struct Parsed {
    /// The command, ready to be judged.
    pub command: Command,
    /// Why the scan reached the verdict it did, for the decision's reason.
    pub notes: Vec<String>,
}

// -----------------------------------------------------------------------------
// Lexer
// -----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Op,
}

#[derive(Clone, Debug)]
struct Tok {
    /// The token's text, with quoting removed for words.
    text: String,
    kind: Kind,
    /// The word contains an expansion whose value the scanner cannot know.
    subst: bool,
    /// The word contains unquoted glob metacharacters.
    glob: bool,
}

impl Tok {
    fn op(text: &str) -> Self {
        Tok {
            text: text.to_owned(),
            kind: Kind::Op,
            subst: false,
            glob: false,
        }
    }

    fn is_word(&self) -> bool {
        self.kind == Kind::Word
    }
}

/// Operators, longest first so that `&&` never lexes as two `&`.
const OPERATORS: &[&str] = &[
    "<<<", "<<-", "&>>", "<<", ">>", "&&", "||", "|&", ";;", "<>", ">|", ">&", "<&", "&>", ";",
    "|", "&", "(", ")", ">", "<", "\n",
];

/// Splits a command string into words and operators.
///
/// Fails on unbalanced quoting, which the caller turns into [`Action::Opaque`]:
/// a command that does not lex is one whose targets cannot be known.
// The final `flush!` clears state nothing reads afterwards; that is the point
// of a macro that leaves the lexer in a known state at every exit.
#[allow(unused_assignments)]
fn lex(input: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out: Vec<Tok> = Vec::new();

    // Delimiters of heredocs opened on the current line, whose bodies begin
    // after the next newline.
    let mut pending_heredocs: Vec<(String, bool)> = Vec::new();

    let mut word = String::new();
    let mut in_word = false;
    let mut subst = false;
    let mut glob = false;

    macro_rules! flush {
        () => {
            if in_word {
                out.push(Tok {
                    text: std::mem::take(&mut word),
                    kind: Kind::Word,
                    subst,
                    glob,
                });
                in_word = false;
                subst = false;
                glob = false;
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];

        // A comment runs to the end of the line, but only when `#` opens a
        // word. Mid-word it is an ordinary character, which is what keeps
        // `--format=%h#x` intact.
        if c == '#' && !in_word {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if c == '\n' && !pending_heredocs.is_empty() {
            flush!();
            out.push(Tok::op("\n"));
            i += 1;
            i = skip_heredoc_bodies(&chars, i, &mut pending_heredocs);
            continue;
        }

        if c == ' ' || c == '\t' || c == '\r' {
            flush!();
            i += 1;
            continue;
        }

        if c == '\\' {
            if i + 1 < chars.len() {
                // A backslash-newline is a line continuation and disappears.
                if chars[i + 1] == '\n' {
                    i += 2;
                    continue;
                }
                in_word = true;
                word.push(chars[i + 1]);
                i += 2;
            } else {
                in_word = true;
                i += 1;
            }
            continue;
        }

        if c == '\'' {
            in_word = true;
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '\'' {
                i += 1;
            }
            if i >= chars.len() {
                return Err("unbalanced single quote".to_owned());
            }
            word.extend(&chars[start..i]);
            i += 1;
            continue;
        }

        if c == '"' {
            in_word = true;
            i += 1;
            loop {
                if i >= chars.len() {
                    return Err("unbalanced double quote".to_owned());
                }
                match chars[i] {
                    '"' => {
                        i += 1;
                        break;
                    }
                    '\\' if i + 1 < chars.len() => {
                        word.push(chars[i + 1]);
                        i += 2;
                    }
                    '$' | '`' => {
                        // Live inside double quotes, and unresolvable.
                        subst = true;
                        word.push(chars[i]);
                        i += 1;
                    }
                    other => {
                        word.push(other);
                        i += 1;
                    }
                }
            }
            continue;
        }

        if c == '$' || c == '`' {
            in_word = true;
            subst = true;
            word.push(c);
            i += 1;
            continue;
        }

        if c == '*' || c == '?' || c == '[' {
            in_word = true;
            glob = true;
            word.push(c);
            i += 1;
            continue;
        }

        // A leading run of digits belongs to the redirection that follows it:
        // `2>err.log` is one operator and one word, not three tokens.
        if let Some(op) = match_operator(&chars, i) {
            let fd_prefix = in_word && !word.is_empty() && word.chars().all(|d| d.is_ascii_digit());
            let starts_redirect = op.starts_with('>') || op.starts_with('<');
            if fd_prefix && starts_redirect {
                let text = format!("{}{}", word, op);
                word.clear();
                in_word = false;
                subst = false;
                glob = false;
                out.push(Tok::op(&text));
            } else {
                flush!();
                out.push(Tok::op(op));
            }
            if op == "<<" || op == "<<-" {
                // The delimiter is the next word; its body starts after the
                // newline. `<<-` strips leading tabs from the terminator.
                let (delim, next) = read_heredoc_delimiter(&chars, i + op.len());
                pending_heredocs.push((delim, op == "<<-"));
                i = next;
                continue;
            }
            i += op.len();
            continue;
        }

        in_word = true;
        word.push(c);
        i += 1;
    }

    flush!();
    Ok(out)
}

/// The operator starting at `i`, longest match first.
fn match_operator(chars: &[char], i: usize) -> Option<&'static str> {
    OPERATORS.iter().copied().find(|op| {
        let op_chars: Vec<char> = op.chars().collect();
        chars[i..].starts_with(&op_chars[..])
    })
}

/// Reads the word after `<<`, which names the heredoc's terminator.
fn read_heredoc_delimiter(chars: &[char], mut i: usize) -> (String, usize) {
    while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
        i += 1;
    }
    let mut delim = String::new();
    while i < chars.len() && !chars[i].is_whitespace() {
        match chars[i] {
            '\'' | '"' | '\\' => {}
            c => delim.push(c),
        }
        i += 1;
    }
    (delim, i)
}

/// Skips the bodies of every heredoc opened on the line just ended.
fn skip_heredoc_bodies(chars: &[char], mut i: usize, pending: &mut Vec<(String, bool)>) -> usize {
    for (delim, strip_tabs) in pending.drain(..) {
        loop {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            let line: String = chars[start..i].iter().collect();
            let candidate = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if i < chars.len() {
                i += 1;
            }
            if candidate == delim || start >= chars.len() {
                break;
            }
        }
    }
    i
}

// -----------------------------------------------------------------------------
// Segments
// -----------------------------------------------------------------------------

/// Operators that end one simple command and begin the next.
const SEPARATORS: &[&str] = &["\n", ";", ";;", "&&", "||", "|", "|&", "&"];

/// One simple command, with the working directory in force when it runs.
struct Segment {
    toks: Vec<Tok>,
    cwd: PathBuf,
}

/// Splits the token stream into simple commands, tracking `cd`.
///
/// A `cd` with a literal operand rebases the relative paths that follow it. A
/// subshell is a barrier: `(cd /tmp && rm x)` leaves the outer directory alone,
/// so the effect of the `cd` is popped with the `)`.
fn segments(toks: &[Tok], cwd: &Path) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut current: Vec<Tok> = Vec::new();
    let mut dir = cwd.to_path_buf();
    let mut stack: Vec<PathBuf> = Vec::new();

    let mut finish = |current: &mut Vec<Tok>, dir: &mut PathBuf| {
        if current.is_empty() {
            return;
        }
        let toks = std::mem::take(current);
        if let Some(next) = cd_destination(&toks, dir) {
            *dir = next;
        }
        out.push(Segment {
            toks,
            cwd: dir.clone(),
        });
    };

    for tok in toks {
        if tok.kind == Kind::Op {
            if tok.text == "(" {
                finish(&mut current, &mut dir);
                stack.push(dir.clone());
                continue;
            }
            if tok.text == ")" {
                finish(&mut current, &mut dir);
                if let Some(outer) = stack.pop() {
                    dir = outer;
                }
                continue;
            }
            if SEPARATORS.contains(&tok.text.as_str()) {
                finish(&mut current, &mut dir);
                continue;
            }
        }
        current.push(tok.clone());
    }
    finish(&mut current, &mut dir);
    out
}

/// Where a segment leaves the working directory, if it is a literal `cd`.
fn cd_destination(toks: &[Tok], dir: &Path) -> Option<PathBuf> {
    let words: Vec<&Tok> = toks.iter().filter(|t| t.is_word()).collect();
    let first = words.first()?;
    if first.text != "cd" {
        return None;
    }
    let operand = words.get(1)?;
    // A destination the scanner cannot resolve leaves the directory as it was,
    // which is the pessimistic choice: relative targets stay attached to the
    // repository they started in.
    if operand.subst || operand.glob {
        return None;
    }
    let target = expand_tilde(&operand.text);
    Some(if target.is_absolute() {
        target
    } else {
        dir.join(target)
    })
}

fn expand_tilde(word: &str) -> PathBuf {
    if (word == "~" || word.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let mut path = PathBuf::from(home);
        if let Some(rest) = word.strip_prefix("~/") {
            path.push(rest);
        }
        return path;
    }
    PathBuf::from(word)
}

// -----------------------------------------------------------------------------
// Effects
// -----------------------------------------------------------------------------

/// Utilities that are refused outright, whatever they name.
///
/// Irreversible *and* unreconstructable: the prior owner, group or content is
/// nowhere on disk once the command has run.
const FORBIDDEN_UTILITIES: &[&str] = &["chown", "chgrp", "shred"];

/// How a utility's operands relate to what it writes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Every operand is written: `rm a b`, `touch a b`, `mv a b` (the sources
    /// are removed, the destination is written).
    Every,
    /// Only the last operand is written; the rest are read: `cp src dst`.
    ///
    /// Getting this wrong in the other direction is what made the previous
    /// guard refuse `cp README.md /tmp/backup.md`.
    Last,
    /// Every operand but the first, which is the mode: `chmod +x f`.
    ///
    /// `+x` is not an option — it does not start with a dash — so without this
    /// the mode would be read as a path of its own.
    AfterMode,
    /// Written only with an in-place flag: `sed -i`, `perl -pi -e`.
    InPlace,
    /// The operand of `of=`.
    DdOutput,
}

/// The utilities whose writes the scanner can see.
fn role_of(utility: &str) -> Option<Role> {
    Some(match utility {
        "rm" | "rmdir" | "unlink" | "mkdir" | "touch" | "mkfifo" | "mknod" | "truncate" | "mv"
        | "tee" => Role::Every,
        "cp" | "install" | "rsync" | "ln" => Role::Last,
        "chmod" => Role::AfterMode,
        "sed" | "perl" => Role::InPlace,
        "dd" => Role::DdOutput,
        _ => return None,
    })
}

/// What one segment does.
#[derive(Default)]
struct SegmentEffect {
    forbidden: Option<String>,
    opaque: Option<String>,
    writes: Vec<PathBuf>,
    git: Vec<GitAction>,
    /// Directories whose git invocations still need a repository lookup.
    git_dirs: Vec<(PathBuf, GitKind)>,
}

/// A git invocation whose verdict depends on the repository it runs in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitKind {
    StateChange,
    Destructive,
}

fn classify_segment(seg: &Segment) -> SegmentEffect {
    let mut eff = SegmentEffect::default();

    // Redirections first: they are independent of the utility, and they are
    // what a content-blind scanner misses.
    let mut i = 0;
    while i < seg.toks.len() {
        let tok = &seg.toks[i];
        if tok.kind == Kind::Op && is_write_redirect(&tok.text) {
            match seg.toks.get(i + 1) {
                Some(next) if next.is_word() => {
                    // `2>&1` duplicates a descriptor and touches no file.
                    let duplicating =
                        tok.text.ends_with(">&") && next.text.chars().all(|c| c.is_ascii_digit());
                    if !duplicating {
                        if next.subst {
                            eff.opaque =
                                Some(format!("redirection target is unresolvable: {}", next.text));
                        } else {
                            eff.writes.push(resolve(&seg.cwd, &next.text));
                        }
                    }
                    i += 2;
                    continue;
                }
                _ => {
                    eff.opaque = Some("redirection with no target".to_owned());
                    i += 1;
                    continue;
                }
            }
        }
        i += 1;
    }

    let words = operand_words(seg);
    let Some((utility, args)) = words.split_first() else {
        return eff;
    };

    if utility.subst {
        eff.opaque = Some("the utility itself is an unresolvable expansion".to_owned());
        return eff;
    }
    let name = basename(&utility.text);

    if FORBIDDEN_UTILITIES.contains(&name) {
        eff.forbidden = Some(format!(
            "`{name}` cannot be undone from what remains on disk"
        ));
        return eff;
    }

    if name == "git" {
        classify_git(seg, args, &mut eff);
        return eff;
    }

    let Some(role) = role_of(name) else {
        return eff;
    };

    let operands = positional(args, name);
    let targets: Vec<&Tok> = match role {
        Role::Every => operands,
        Role::AfterMode => operands.into_iter().skip(1).collect(),
        Role::Last => operands.into_iter().next_back().into_iter().collect(),
        Role::InPlace => {
            if has_in_place_flag(args) {
                // `sed -i s/a/b/ f` and `perl -pi -e expr f`: the first
                // positional is the script, the rest are files.
                let mut rest = operands;
                if !rest.is_empty() && !expression_is_flag_operand(args, name) {
                    rest.remove(0);
                }
                rest
            } else {
                Vec::new()
            }
        }
        Role::DdOutput => args
            .iter()
            .copied()
            .filter(|a| a.text.starts_with("of="))
            .collect::<Vec<_>>(),
    };

    for target in targets {
        let text = match role {
            Role::DdOutput => target.text.trim_start_matches("of=").to_owned(),
            _ => target.text.clone(),
        };
        if target.subst {
            eff.opaque = Some(format!(
                "`{name}` names an unresolvable target: {}",
                target.text
            ));
            continue;
        }
        if target.glob {
            eff.writes.extend(expand_glob(&seg.cwd, &text));
        } else {
            eff.writes.push(resolve(&seg.cwd, &text));
        }
    }
    eff
}

/// Redirection operators that write a file.
fn is_write_redirect(op: &str) -> bool {
    let stripped = op.trim_start_matches(|c: char| c.is_ascii_digit());
    matches!(stripped, ">" | ">>" | ">|" | "&>" | "&>>" | "<>" | ">&")
}

/// The words of a segment, with redirection operands removed.
fn operand_words(seg: &Segment) -> Vec<&Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seg.toks.len() {
        let tok = &seg.toks[i];
        if tok.kind == Kind::Op {
            // Every redirection consumes its operand, write or read alike.
            let stripped = tok.text.trim_start_matches(|c: char| c.is_ascii_digit());
            if stripped.starts_with('>') || stripped.starts_with('<') {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        // `VAR=value cmd` prefixes are assignments, not the utility.
        if out.is_empty() && is_assignment(&tok.text) {
            i += 1;
            continue;
        }
        out.push(tok);
        i += 1;
    }
    out
}

fn is_assignment(text: &str) -> bool {
    match text.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Operands that are not option flags.
fn positional<'a>(args: &[&'a Tok], utility: &str) -> Vec<&'a Tok> {
    let mut out = Vec::new();
    let mut end_of_options = false;
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if !end_of_options {
            if arg.text == "--" {
                end_of_options = true;
                continue;
            }
            if arg.text.starts_with('-') && arg.text.len() > 1 {
                // Options that take a separate operand, which must not be read
                // as a target.
                if takes_operand(utility, &arg.text) {
                    skip_next = true;
                }
                continue;
            }
        }
        out.push(*arg);
    }
    out
}

fn takes_operand(utility: &str, flag: &str) -> bool {
    match utility {
        "cp" | "mv" | "install" | "rsync" | "ln" => {
            matches!(
                flag,
                "-t" | "--target-directory" | "-S" | "-m" | "--mode" | "--suffix"
            )
        }
        "sed" => matches!(flag, "-e" | "-f" | "--expression" | "--file"),
        "perl" => matches!(flag, "-e" | "-E" | "-I" | "-M"),
        "truncate" => matches!(flag, "-s" | "--size" | "-r" | "--reference"),
        "chmod" => matches!(flag, "--reference"),
        _ => false,
    }
}

/// `sed -i` / `perl -i`, including the clustered `-pi` form.
fn has_in_place_flag(args: &[&Tok]) -> bool {
    args.iter().any(|a| {
        a.text.starts_with("--in-place")
            || (a.text.starts_with('-')
                && !a.text.starts_with("--")
                && a.text.contains('i')
                && !a.text.contains("-e"))
    })
}

/// Whether the script came from a flag, so the first positional is a file.
fn expression_is_flag_operand(args: &[&Tok], utility: &str) -> bool {
    args.iter().any(|a| match utility {
        "sed" => a.text.starts_with("-e") || a.text.starts_with("--expression"),
        "perl" => a.text.starts_with("-e") || a.text.starts_with("-E"),
        _ => false,
    })
}

fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

fn resolve(cwd: &Path, text: &str) -> PathBuf {
    let path = expand_tilde(text);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

// -----------------------------------------------------------------------------
// Globs
// -----------------------------------------------------------------------------

/// Expands a glob against the filesystem.
///
/// A glob names a bounded set of paths that exist right now, so resolving it
/// gives the rules real targets instead of an unknown. An unmatched glob is
/// passed through literally, which is what the shell itself does.
fn expand_glob(cwd: &Path, pattern: &str) -> Vec<PathBuf> {
    let expanded = expand_tilde(pattern);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };

    let mut matches: BTreeSet<PathBuf> = BTreeSet::new();
    matches.insert(PathBuf::from("/"));
    let mut any_glob = false;

    for component in absolute.components() {
        let part = component.as_os_str().to_string_lossy().into_owned();
        if part == "/" {
            continue;
        }
        if !has_glob_meta(&part) {
            matches = matches.into_iter().map(|p| p.join(&part)).collect();
            continue;
        }
        any_glob = true;
        let mut next = BTreeSet::new();
        for dir in &matches {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // A leading dot is matched only by an explicit one.
                if name.starts_with('.') && !part.starts_with('.') {
                    continue;
                }
                if glob_match(&part, &name) {
                    next.insert(dir.join(name));
                }
            }
        }
        matches = next;
        if matches.is_empty() {
            break;
        }
    }

    if !any_glob || matches.is_empty() {
        return vec![absolute];
    }
    matches.into_iter().collect()
}

fn has_glob_meta(part: &str) -> bool {
    part.contains('*') || part.contains('?') || part.contains('[')
}

/// Matches one path component against one glob pattern.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_match_at(&p, 0, &n, 0)
}

fn glob_match_at(p: &[char], pi: usize, n: &[char], ni: usize) -> bool {
    if pi == p.len() {
        return ni == n.len();
    }
    match p[pi] {
        '*' => (ni..=n.len()).any(|skip| glob_match_at(p, pi + 1, n, skip)),
        '?' => ni < n.len() && glob_match_at(p, pi + 1, n, ni + 1),
        '[' => {
            let Some(close) = p[pi + 1..].iter().position(|c| *c == ']') else {
                return ni < n.len() && n[ni] == '[' && glob_match_at(p, pi + 1, n, ni + 1);
            };
            let close = pi + 1 + close;
            if ni >= n.len() {
                return false;
            }
            let (negated, start) = if p[pi + 1] == '!' || p[pi + 1] == '^' {
                (true, pi + 2)
            } else {
                (false, pi + 1)
            };
            let mut hit = false;
            let mut k = start;
            while k < close {
                if k + 2 < close && p[k + 1] == '-' {
                    if n[ni] >= p[k] && n[ni] <= p[k + 2] {
                        hit = true;
                    }
                    k += 3;
                } else {
                    if n[ni] == p[k] {
                        hit = true;
                    }
                    k += 1;
                }
            }
            hit != negated && glob_match_at(p, close + 1, n, ni + 1)
        }
        c => ni < n.len() && n[ni] == c && glob_match_at(p, pi + 1, n, ni + 1),
    }
}

// -----------------------------------------------------------------------------
// The git dimension
// -----------------------------------------------------------------------------

/// Subcommands that only read.
///
/// An **allowlist**, so that a subcommand nobody thought to add is treated as a
/// write. The inverse arrangement is what let `git add`, `git fetch` and
/// `git submodule update` pass unexamined in the previous guard.
const GIT_READ_SUBCOMMANDS: &[&str] = &[
    "annotate",
    "blame",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "cherry",
    "count-objects",
    "describe",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "for-each-ref",
    "for-each-repo",
    "grep",
    "help",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "merge-base",
    "name-rev",
    "range-diff",
    "rev-list",
    "rev-parse",
    "shortlog",
    "show",
    "show-branch",
    "show-ref",
    "status",
    "var",
    "verify-commit",
    "verify-tag",
    "version",
    "whatchanged",
];

/// Global options that consume the operand after them.
const GIT_GLOBAL_WITH_OPERAND: &[&str] = &[
    "-C",
    "-c",
    "--exec-path",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--config-env",
];

/// Classifies one `git …` segment into the git dimension.
fn classify_git(seg: &Segment, args: &[&Tok], eff: &mut SegmentEffect) {
    let mut dir = seg.cwd.clone();
    let mut redirected = false;
    let mut i = 0;
    let mut subcommand: Option<&Tok> = None;

    while i < args.len() {
        let arg = args[i];
        if !arg.text.starts_with('-') {
            subcommand = Some(arg);
            i += 1;
            break;
        }
        let (name, inline) = match arg.text.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (arg.text.as_str(), None),
        };
        // `--git-dir` and `--work-tree` point the invocation at a repository
        // other than the one its working directory names. The scanner does not
        // follow them, so it cannot say which repository the operation lands
        // in -- which is the definition of an opaque target, and asks.
        if matches!(name, "--git-dir" | "--work-tree") {
            redirected = true;
        }
        if name == "-C" {
            let operand = inline
                .map(str::to_owned)
                .or_else(|| args.get(i + 1).filter(|t| !t.subst).map(|t| t.text.clone()));
            match operand {
                Some(value) => {
                    let path = expand_tilde(&value);
                    dir = if path.is_absolute() {
                        path
                    } else {
                        dir.join(path)
                    };
                }
                None => redirected = true,
            }
        }
        if inline.is_none() && GIT_GLOBAL_WITH_OPERAND.contains(&name) {
            i += 2;
        } else {
            i += 1;
        }
    }

    let Some(subcommand) = subcommand else {
        // Bare `git`, or one whose subcommand is an expansion.
        eff.git.push(GitActionKind::Read.into());
        return;
    };
    if subcommand.subst {
        eff.opaque = Some("the git subcommand is an unresolvable expansion".to_owned());
        return;
    }

    let rest: Vec<&Tok> = args[i..].to_vec();
    let kind = git_kind(&subcommand.text, &rest);

    match kind {
        // A config write is refused whatever repository it is aimed at, so it
        // needs no repository to reach its verdict.
        GitActionKind::ConfigWrite => eff.git.push(GitActionKind::ConfigWrite.into()),
        _ if redirected => {
            eff.opaque = Some(
                "git is pointed at a repository the scanner cannot see (--git-dir/--work-tree)"
                    .to_owned(),
            )
        }
        GitActionKind::Read => eff.git.push(GitActionKind::Read.into()),
        GitActionKind::StateChange => eff.git_dirs.push((dir, GitKind::StateChange)),
        GitActionKind::Destructive => eff.git_dirs.push((dir, GitKind::Destructive)),
    }
}

/// The classification of a git subcommand before its repository is known.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GitActionKind {
    Read,
    StateChange,
    Destructive,
    ConfigWrite,
}

impl From<GitActionKind> for GitAction {
    fn from(kind: GitActionKind) -> Self {
        match kind {
            GitActionKind::Read => GitAction::Read,
            GitActionKind::ConfigWrite => GitAction::ConfigWrite,
            GitActionKind::StateChange => GitAction::StateChange(None),
            GitActionKind::Destructive => GitAction::Destructive(None),
        }
    }
}

fn git_kind(subcommand: &str, args: &[&Tok]) -> GitActionKind {
    let flags: Vec<&str> = args.iter().map(|t| t.text.as_str()).collect();
    let has = |f: &str| flags.contains(&f);
    let positional: Vec<&str> = flags
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let first = positional.first().copied().unwrap_or("");

    match subcommand {
        "config" => {
            if git_config_is_read(args) {
                GitActionKind::Read
            } else {
                GitActionKind::ConfigWrite
            }
        }
        "remote" => match first {
            "" | "show" | "get-url" => GitActionKind::Read,
            "add" | "remove" | "rm" | "rename" | "set-url" | "set-branches" | "set-head" => {
                GitActionKind::ConfigWrite
            }
            _ => GitActionKind::StateChange,
        },
        "branch" => {
            if has("-D") || (has("-d") && has("-f")) || has("--delete") && has("--force") {
                GitActionKind::Destructive
            } else if flags.iter().any(|f| {
                matches!(*f, "-d" | "-m" | "-M" | "-c" | "-C" | "--move" | "--copy")
                    || f.starts_with("--set-upstream")
                    || f.starts_with("--unset-upstream")
                    || f.starts_with("--edit-description")
            }) || !positional.is_empty()
            {
                GitActionKind::StateChange
            } else {
                GitActionKind::Read
            }
        }
        "tag" => {
            if has("-d") || has("--delete") {
                GitActionKind::Destructive
            } else if flags.iter().any(|f| {
                matches!(
                    *f,
                    "-l" | "--list" | "-n" | "--contains" | "--points-at" | "--sort"
                ) || f.starts_with("--format")
                    || f.starts_with("-n")
            }) || positional.is_empty()
            {
                GitActionKind::Read
            } else {
                GitActionKind::StateChange
            }
        }
        "stash" => match first {
            "" | "list" | "show" => {
                if positional.is_empty() {
                    GitActionKind::StateChange
                } else {
                    GitActionKind::Read
                }
            }
            "drop" | "clear" | "pop" => GitActionKind::Destructive,
            _ => GitActionKind::StateChange,
        },
        "worktree" => match first {
            "list" => GitActionKind::Read,
            "remove" | "prune" => GitActionKind::Destructive,
            _ => GitActionKind::StateChange,
        },
        "reflog" => match first {
            "" | "show" => GitActionKind::Read,
            "delete" | "expire" => GitActionKind::Destructive,
            _ => GitActionKind::StateChange,
        },
        "notes" => match first {
            "" | "list" | "show" => GitActionKind::Read,
            "remove" | "prune" => GitActionKind::Destructive,
            _ => GitActionKind::StateChange,
        },
        "reset" => {
            if has("--hard") {
                GitActionKind::Destructive
            } else {
                GitActionKind::StateChange
            }
        }
        "clean" => GitActionKind::Destructive,
        "checkout" | "restore" => {
            // `checkout -- <path>` and `restore <path>` throw away uncommitted
            // work in the tree. Switching branches does not.
            if has("--") || has("-f") || has("--force") || subcommand == "restore" {
                GitActionKind::Destructive
            } else {
                GitActionKind::StateChange
            }
        }
        "push" => {
            if flags
                .iter()
                .any(|f| matches!(*f, "-f" | "--force" | "--delete" | "-d" | "--mirror"))
                || flags.iter().any(|f| f.starts_with("--force-with-lease"))
                || flags.iter().any(|f| f.starts_with("--force-if-includes"))
            {
                GitActionKind::Destructive
            } else {
                GitActionKind::StateChange
            }
        }
        "filter-branch" | "prune" => GitActionKind::Destructive,
        "gc" => {
            if flags.iter().any(|f| f.starts_with("--prune")) {
                GitActionKind::Destructive
            } else {
                GitActionKind::StateChange
            }
        }
        "update-ref" => {
            if has("-d") {
                GitActionKind::Destructive
            } else {
                GitActionKind::StateChange
            }
        }
        "submodule" => match first {
            "status" | "summary" | "foreach" => GitActionKind::Read,
            "deinit" => GitActionKind::Destructive,
            _ => GitActionKind::StateChange,
        },
        other if GIT_READ_SUBCOMMANDS.contains(&other) => GitActionKind::Read,
        _ => GitActionKind::StateChange,
    }
}

/// Whether a `git config` invocation only reads.
fn git_config_is_read(args: &[&Tok]) -> bool {
    const READ_FLAGS: &[&str] = &[
        "--get",
        "--get-all",
        "--get-regexp",
        "--get-urlmatch",
        "--get-color",
        "--get-colorbool",
        "--list",
        "-l",
    ];
    const WRITE_FLAGS: &[&str] = &[
        "--unset",
        "--unset-all",
        "--replace-all",
        "--add",
        "--edit",
        "-e",
        "--rename-section",
        "--remove-section",
    ];
    const WITH_OPERAND: &[&str] = &["--file", "-f", "--blob", "--default", "--type", "-t"];

    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let text = args[i].text.as_str();
        let name = text.split_once('=').map(|(n, _)| n).unwrap_or(text);
        if READ_FLAGS.contains(&name) {
            return true;
        }
        if WRITE_FLAGS.contains(&name) {
            return false;
        }
        if text.starts_with('-') {
            if WITH_OPERAND.contains(&name) && !text.contains('=') {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        positional.push(text);
        i += 1;
    }

    match positional.first().copied() {
        Some("get") | Some("list") | Some("get-all") | Some("get-regexp")
        | Some("get-urlmatch") => true,
        Some("set")
        | Some("unset")
        | Some("edit")
        | Some("add")
        | Some("rename-section")
        | Some("remove-section") => false,
        // `git config <key>` reads it; `git config <key> <value>` writes it.
        _ => positional.len() <= 1,
    }
}

// -----------------------------------------------------------------------------
// Entry points
// -----------------------------------------------------------------------------

/// Scans a command string and resolves everything it names.
pub fn parse(command: &str, cwd: &Path) -> Parsed {
    let mut resolver = None;
    parse_with(command, cwd, &mut resolver)
}

/// Scans a command string against a resolver supplied by the caller.
///
/// The resolver is built on first need and not before: a command that writes
/// nothing and touches no repository runs **zero** subprocesses, which is what
/// lets the guard sit on every tool call.
pub fn parse_with(command: &str, cwd: &Path, resolver: &mut Option<Resolver>) -> Parsed {
    let mut notes = Vec::new();

    let toks = match lex(command) {
        Ok(toks) => toks,
        Err(reason) => {
            notes.push(format!("cannot read the command: {reason}"));
            return Parsed {
                command: Command::new(Action::Opaque, Vec::new()),
                notes,
            };
        }
    };

    let mut forbidden: Option<String> = None;
    let mut opaque: Option<String> = None;
    let mut writes: Vec<PathBuf> = Vec::new();
    let mut git: Vec<GitAction> = Vec::new();
    let mut pending: Vec<(PathBuf, GitKind)> = Vec::new();

    for segment in segments(&toks, cwd) {
        let effect = classify_segment(&segment);
        forbidden = forbidden.or(effect.forbidden);
        opaque = opaque.or(effect.opaque);
        writes.extend(effect.writes);
        git.extend(effect.git);
        pending.extend(effect.git_dirs);
    }

    // Nothing to resolve means nothing to ask git about.
    if writes.is_empty() && pending.is_empty() {
        let action = match (forbidden, opaque) {
            (Some(reason), _) => {
                notes.push(reason);
                Action::Forbidden
            }
            (None, Some(reason)) => {
                notes.push(reason);
                Action::Opaque
            }
            (None, None) => Action::ReadOnly,
        };
        return Parsed {
            command: Command::new(action, git),
            notes,
        };
    }

    if resolver.is_none() {
        match Resolver::from_env(cwd) {
            Ok(built) => *resolver = Some(built),
            Err(reason) => {
                notes.push(format!("cannot establish the lanes: {reason}"));
                return Parsed {
                    command: Command::new(Action::Opaque, git),
                    notes,
                };
            }
        }
    }
    let resolver = resolver.as_mut().expect("just built");

    for (dir, kind) in pending {
        match resolver.repo_of(&dir) {
            Ok(repo) => {
                let repo = repo.map(crate::action_rules::Repo::new);
                git.push(match kind {
                    GitKind::StateChange => GitAction::StateChange(repo),
                    GitKind::Destructive => GitAction::Destructive(repo),
                });
            }
            Err(reason) => {
                notes.push(format!("cannot resolve the repository for git: {reason}"));
                opaque = opaque.or(Some("git repository unresolvable".to_owned()));
            }
        }
    }

    // Ordering is the specification's: the most severe reading wins.
    if let Some(reason) = forbidden {
        notes.push(reason);
        return Parsed {
            command: Command::new(Action::Forbidden, git),
            notes,
        };
    }
    if let Some(reason) = opaque {
        notes.push(reason);
        return Parsed {
            command: Command::new(Action::Opaque, git),
            notes,
        };
    }

    let action = match resolve_writes(resolver, &writes, &mut notes) {
        Some(effects) if effects.is_empty() => Action::ReadOnly,
        Some(effects) => Action::Write(effects),
        None => Action::Opaque,
    };
    Parsed {
        command: Command::new(action, git),
        notes,
    }
}

/// Builds the [`Action::Write`] payload for a set of paths, resolved together.
fn resolve_writes(
    resolver: &mut Resolver,
    writes: &[PathBuf],
    notes: &mut Vec<String>,
) -> Option<Vec<TargetedEffect>> {
    if writes.is_empty() {
        return Some(Vec::new());
    }
    match resolver.resolve_all(writes) {
        Ok(facts) => Some(
            facts
                .into_iter()
                .map(|f| {
                    // Nothing in the rules turns on the effect today; it is
                    // recorded because a rule that distinguishes creating from
                    // clobbering has nowhere else to read it from.
                    let effect = if f.exists {
                        Effect::Change
                    } else {
                        Effect::Create
                    };
                    TargetedEffect::new(Target::new(f), effect)
                })
                .collect(),
        ),
        Err(reason) => {
            notes.push(format!("cannot resolve the targets: {reason}"));
            None
        }
    }
}

/// Builds the equivalent of a command for a file tool's `file_path`.
///
/// The file tools name a path rather than a command line, and go through the
/// same [`Action::Write`] the shell path produces so that one rule table judges
/// both.
pub fn parse_file_write(path: &Path, cwd: &Path, resolver: &mut Option<Resolver>) -> Parsed {
    let mut notes = Vec::new();
    if resolver.is_none() {
        match Resolver::from_env(cwd) {
            Ok(built) => *resolver = Some(built),
            Err(reason) => {
                notes.push(format!("cannot establish the lanes: {reason}"));
                return Parsed {
                    command: Command::new(Action::Opaque, Vec::new()),
                    notes,
                };
            }
        }
    }
    let resolver = resolver.as_mut().expect("just built");
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    let action = match resolve_writes(resolver, &[absolute], &mut notes) {
        Some(effects) => Action::Write(effects),
        None => Action::Opaque,
    };
    Parsed {
        command: Command::new(action, Vec::new()),
        notes,
    }
}
