//! Resolving WHOSE changes this run is testing — the diff-base ladder.
//!
//! The base must be a commit the checked-out TREE actually descends from, or the plan describes a
//! different change than the one being compiled. That is not hypothetical, and the measurement is
//! what forced every rung below:
//!
//!   `actions/checkout` on a `pull_request` event checks out the MERGE REF, not the PR head —
//!   verified verbatim in a `plan` job log: `HEAD is now at 27e4f8e7b Merge <head> into <base>`.
//!   That commit's FIRST parent is the base-branch tip GitHub built it on and its SECOND is the PR
//!   head (checked on four real refs: `refs/pull/{1134,1137,1140}/merge`'s `HEAD^2` each equal that
//!   PR's `head.sha`). `github.event.pull_request.base.sha` is a separate payload field that LAGS
//!   that parent — and a THREE-dot `base.sha...HEAD` cannot repair the gap, because base.sha is an
//!   ANCESTOR of the merge ref through its first parent, so the merge base IS base.sha and the
//!   3-dot silently degenerates into the 2-dot. Everything `main` merged in between is then
//!   attributed to the PR.
//!
//!   MEASURED on PR #1139, which changed 2 files: its run's merge ref was `Merge 911a048d into
//!   9fff0c94` while `base.sha` was still 046bc036 — TWO commits behind (#1132, #1130) — so it
//!   arrived with 40 paths, `.github/CODEOWNERS` (a `GLOBAL_PREFIXES` entry) among them, and the
//!   plan escalated to the full matrix on the strength of files belonging to #1130 and #1132.
//!   `HEAD^1..HEAD` on that same merge ref returns exactly its own 2 paths.
//!
//! That measurement produced the invariant which decides every `else` branch below:
//!
//! ⚠ **The failure mode of getting this wrong is UNDER-testing, so every ambiguity resolves toward
//! the LARGER set.** A parent pair that cannot be classified, a base branch that is not an ancestor,
//! an orphaned `before` SHA, unrelated histories, a failed `git diff` — each falls through to the
//! next, more conservative rung, and the last rung is [`Option::None`], which the caller reads as
//! the full matrix.
//!
//! Every step is a TWO-dot diff. A three-dot's merge-base pass could only ever move the base this
//! ladder already chose, and on a rollback force-push it moves it the wrong way: with the new tip an
//! ancestor of the old one, `merge-base(before, HEAD) == HEAD` and `before...HEAD` is EMPTY, testing
//! nothing at all for a commit that reverted files.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Stdio};

/// `git` stdout, trimmed — or `None` when git FAILED. Never panics.
///
/// Separate from [`git`] because empty output is a real answer for exactly one caller: a `git diff`
/// that lists no files means "this change touches nothing", not "the diff could not be computed",
/// and collapsing the two would silently escalate an empty PR to the full matrix.
fn git_raw(args: &[&str], cwd: &Path) -> Option<String> {
    let out =
        Command::new("git").args(args).current_dir(cwd).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// [`git_raw`], with empty output folded into `None` — the right shape for every rev query
/// (`rev-parse --quiet` exits 1 AND prints nothing for an unknown ref, so both spellings of "no
/// answer" arrive here).
fn git(args: &[&str], cwd: &Path) -> Option<String> {
    git_raw(args, cwd).filter(|s| !s.is_empty())
}

/// `rev` resolved to a commit sha, or `None` when git cannot resolve it to one.
pub fn rev(r: &str, cwd: &Path) -> Option<String> {
    if r.is_empty() {
        return None;
    }
    git(&["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")], cwd)
}

/// True iff commit `a` is an ancestor of (or equal to) commit `b`.
fn is_ancestor(a: &str, b: &str, cwd: &Path) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", a, b])
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `(sha, label)` of the branch this change is aimed at, or `(None, None)`.
///
/// `GITHUB_BASE_REF` is set automatically on `pull_request` and holds the base BRANCH NAME, so a
/// stacked PR resolves against ITS base rather than main. The remote-tracking ref is tried first:
/// the `plan` job checks out a DETACHED merge ref with `fetch-depth: 0`, so `origin/main` is the
/// freshly fetched tip while a local `main` branch may not exist at all.
fn base_branch_tip(env: &Env, cwd: &Path) -> (Option<String>, Option<String>) {
    let named = var(env, "GITHUB_BASE_REF");
    let mut cands: Vec<String> = Vec::new();
    if !named.is_empty() {
        cands.push(format!("origin/{named}"));
        cands.push(named.to_string());
    }
    cands.push("origin/main".to_string());
    cands.push("main".to_string());
    for cand in cands {
        if let Some(sha) = rev(&cand, cwd) {
            return (Some(sha), Some(cand));
        }
    }
    (None, None)
}

/// Which parent of a two-parent HEAD is the BASE side? `None` = REFUSE TO GUESS.
///
/// Two independent oracles, and a parent is returned only when every oracle that can decide agrees.
/// Guessing wrong here is the one way this whole path could UNDER-test: pick the PR side by mistake
/// and the diff becomes "what the other branch added", which can be nothing at all.
///
///   * `CI_PR_HEAD` — exact: the parent that is NOT the PR head is the base. Survives a change in
///     GitHub's merge-ref parent ORDER, which an ordering claim would not.
///   * ancestry — a merge ref's base parent is a commit ON the base branch and its head parent is
///     not. This also REJECTS a HEAD that is merely a merge commit at the tip of a PR branch:
///     neither of ITS parents is on `main`, no oracle decides, and the caller falls through to the
///     merge-base rung, which returns the PR's whole change set instead of one side of its own merge.
///
/// Silence (an oracle with no opinion) is not a vote; disagreement returns `None`, never a coin flip.
pub fn pick_base_parent(
    p1: &str,
    p2: &str,
    pr_head: Option<&str>,
    base_tip: Option<&str>,
    cwd: &Path,
) -> Option<String> {
    let mut votes: Vec<Option<&str>> = Vec::new();
    if let Some(head) = pr_head {
        votes.push(if p2 == head {
            Some(p1)
        } else if p1 == head {
            Some(p2)
        } else {
            None
        });
    }
    if let Some(tip) = base_tip {
        let (on1, on2) = (is_ancestor(p1, tip, cwd), is_ancestor(p2, tip, cwd));
        votes.push(if on1 && !on2 {
            Some(p1)
        } else if on2 && !on1 {
            Some(p2)
        } else {
            None
        });
    }
    let decided: std::collections::BTreeSet<&str> = votes.into_iter().flatten().collect();
    if decided.len() == 1 { decided.into_iter().next().map(str::to_string) } else { None }
}

/// The caller-supplied environment. A PARAMETER, never `std::env` — which is what lets the gate in
/// `xtask/tests/ci_plan_gate.rs` drive this ladder over a planted git topology with an
/// exact, ambient-free variable set.
pub type Env = BTreeMap<String, String>;

/// `env[name]`, trimmed — an ABSENT variable reads as empty. The distinction that survives is
/// set-but-empty vs set-to-a-value; "absent" and "empty" are deliberately the same answer at every
/// call site below, because every one of them is fed by a GitHub Actions expression that renders an
/// unavailable payload field as the empty string rather than omitting the variable.
pub fn var<'a>(env: &'a Env, name: &str) -> &'a str {
    env.get(name).map(|s| s.trim()).unwrap_or("")
}

/// `(base_sha, how)` for a TWO-dot `git diff <base> HEAD`, or `None` = the caller runs everything.
///
/// The ladder, most authoritative first:
///   1. push        -> `CI_BASE` = `github.event.before`, the pre-push tip of the branch.
///   2. PR merge ref -> the base-side PARENT of HEAD. Provably consistent with the tree being
///      compiled — it IS one of its parents — which no payload field can promise.
///   3. base branch -> `merge-base(origin/<base>, HEAD)`. Correct for a non-merge-ref checkout, and
///      a self-healing echo of rung 2, so the two can only disagree toward MORE files, never fewer.
///   4. `CI_BASE`   -> honoured as an explicit override for local/manual runs.
///   5. `None`      -> the caller escalates to the full matrix.
///
/// `how` is the operator-facing description, and it names the RUNG as well as the sha: "which base
/// did it pick" is never the useful question on its own — "and why that one" is what tells an
/// operator whether a surprising plan came from a bad payload field or a rewritten branch.
pub fn resolve_base(env: &Env, cwd: &Path) -> Option<(String, String)> {
    rev("HEAD", cwd)?;
    let event = var(env, "GITHUB_EVENT_NAME");
    let ci_base = var(env, "CI_BASE").to_string();

    // (1) A push diffs against the tip it replaced. An all-zeros `before` (branch creation) or a SHA
    // git cannot resolve (a force-push that orphaned and GC'd it) is not a base — return None and
    // let the caller run everything, rather than invent one from the base branch, which on a push to
    // `main` IS this commit and would diff nothing at all.
    if event == "push" {
        if ci_base.is_empty() || ci_base.chars().all(|c| c == '0') {
            return None;
        }
        let before = rev(&ci_base, cwd)?;
        let short = short_sha(&ci_base);
        return Some((before, format!("base = push: {short}..HEAD (github.event.before)")));
    }

    let (base_tip, base_label) = base_branch_tip(env, cwd);

    // (2) the PR merge ref: the base is one of HEAD's own parents. EXACTLY two parents — a GitHub
    // merge ref never has more, and on an octopus the "base side" is not a well-posed question, so
    // rung 3 answers it unambiguously instead of this rung answering it by luck.
    let (p1, p2) = (rev("HEAD^1", cwd), rev("HEAD^2", cwd));
    if let (Some(p1), Some(p2)) = (&p1, &p2)
        && rev("HEAD^3", cwd).is_none()
    {
        let pr_head = rev(var(env, "CI_PR_HEAD"), cwd);
        let chosen = pick_base_parent(p1, p2, pr_head.as_deref(), base_tip.as_deref(), cwd);
        if let Some(chosen) = chosen {
            let side = if &chosen == p1 { "HEAD^1" } else { "HEAD^2" };
            let how = format!("base = PR merge ref: {side} ({})..HEAD", short_sha(&chosen));
            return Some((chosen, how));
        }
    }

    // (3) not a classifiable merge ref — fork at the base branch instead.
    if let (Some(tip), Some(label)) = (&base_tip, &base_label)
        && let Some(mb) = git(&["merge-base", tip, "HEAD"], cwd)
    {
        let how = format!(
            "base = merge-base({label} {}, HEAD) = {}..HEAD",
            short_sha(tip),
            short_sha(&mb)
        );
        return Some((mb, how));
    }

    // (4) explicit override, last: it is the field that lags, so it never outranks the tree.
    if !ci_base.is_empty() {
        let sha = git(&["merge-base", &ci_base, "HEAD"], cwd).or_else(|| rev(&ci_base, cwd));
        if let Some(sha) = sha {
            let how = format!("base = CI_BASE override: {}..HEAD", short_sha(&sha));
            return Some((sha, how));
        }
    }

    // (5) unrelated histories, a bare repo, a first commit — nothing trustworthy to narrow with.
    None
}

/// The first 9 characters, by CHARACTER boundary rather than by byte, so it cannot panic on a
/// shorter or non-ASCII input. `CI_BASE` is operator-settable and can hold a branch name, so the
/// input is not always 40 hex characters.
fn short_sha(s: &str) -> &str {
    let end = s.char_indices().nth(9).map_or(s.len(), |(i, _)| i);
    &s[..end]
}

/// The paths `base..HEAD` changed, or `None` when git could not produce the diff at all.
///
/// An empty list is NOT `None`: a PR that changes no file is a real, narrow answer, while a failed
/// diff is the caller's cue to run everything.
///
/// ⚠ `--no-renames`, so a RENAME arrives as both of its paths — the old one deleted, the new one
/// added. git's porcelain `diff` detects renames by default (`diff.renames`), and `--name-only`
/// then prints the NEW path alone: a file renamed away from an exempted path vanished from the
/// plan's input entirely, so `super::selection::escalates`'s existence rule never saw it, and a file moved
/// OUT of a crate never selected the crate it left. Both answers are larger now, never narrower —
/// this module's rule for every ambiguity.
pub fn diff_names(base: &str, cwd: &Path) -> Option<Vec<String>> {
    let out = git_raw(&["diff", "--no-renames", "--name-only", base, "HEAD"], cwd)?;
    Some(out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// True iff `base..HEAD` changed `f` by adding or removing ONLY whole-line `#` comments and blank
/// lines. DOUBT IS `false`: an unreadable diff, a new or deleted file, a mode change, a binary diff
/// — each makes the caller treat the file as an ordinary change, which is the larger plan.
///
/// The decision reads the CONTENT of the diff, not the name of the file, and it is only meaningful
/// for a file whose comment syntax is `#` ([`super::tables::escalation::has_hash_comments`] says
/// which); the caller asks that first.
pub fn comment_only_change(base: &str, f: &str, cwd: &Path) -> bool {
    // `--literal-pathspecs`: `f` is a path, never a glob.
    git_raw(&["--literal-pathspecs", "diff", "--no-renames", "-U0", base, "HEAD", "--", f], cwd)
        .is_some_and(|patch| patch_is_comment_only(&patch))
}

/// The most diff text [`diff_patch`] will hold. A diff past it is not read at all (the caller treats
/// that as "cannot read it" and widens), so a vendored-blob PR cannot make the planner allocate
/// without bound.
pub const MAX_PATCH_BYTES: usize = 16 * 1024 * 1024;

/// The text of `git diff -U0 <base> HEAD` — every file, no context, no renames — or `None` when git
/// could not produce it or it is larger than [`MAX_PATCH_BYTES`]. DOUBT IS `None`, and the caller reads
/// `None` as "run everything": it is what a token trigger is judged against, and a token that cannot
/// be looked for is a token that cannot be ruled out.
///
/// Lossy UTF-8: a file that is not valid UTF-8 (Latin-1 prose, a binary blob git still diffs) must not
/// make the whole diff unreadable, and a token is ASCII-or-UTF-8 text that survives the lossy decode
/// wherever it was valid. `--no-ext-diff --no-textconv` keep a developer's own `diff.external` or
/// textconv filter from rewriting the text; `--no-color` keeps escape codes out of it.
pub fn diff_patch(base: &str, cwd: &Path) -> Option<String> {
    use std::io::Read;

    let mut child = Command::new("git")
        .args([
            "diff",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "-U0",
            base,
            "HEAD",
        ])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    let read = child.stdout.take()?.take(MAX_PATCH_BYTES as u64 + 1).read_to_end(&mut bytes);
    // Over the bound the child may still be writing: stop it, or `wait` would block on a full pipe.
    if bytes.len() > MAX_PATCH_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let status = child.wait().ok()?;
    if read.is_err() || !status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// [`comment_only_change`]'s verdict over the text of a `git diff -U0` of ONE file.
///
/// ⚠ **Only whole lines count.** A changed line is acceptable iff, after trimming, it is empty or
/// starts with `#`. `key = 1 # note` starts with `key`, and `color = "#fff"` starts with `color`:
/// both are code. A SHEBANG (`#!`) starts with `#` and is code too — it picks the interpreter — so
/// it is refused.
///
/// ⚠ **Declared residual:** `-U0` shows no context, so a `#`-leading line inside a multi-line string
/// (a TOML `"""` value, a shell heredoc, a YAML block scalar that is not a script) reads as a
/// comment though it is data. No tracked file in the global set does that, and the alternative —
/// parsing every file's grammar — is the cost this rule exists to avoid.
pub fn patch_is_comment_only(patch: &str) -> bool {
    let mut in_hunk = false;
    let mut changed = false;
    for line in patch.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            // The header of a plain edit is exactly these four; `new file mode`, `deleted file
            // mode`, `old mode`, `new mode`, `Binary files` and the rest are not an edit of text.
            let plain = ["diff --git ", "index ", "--- ", "+++ "];
            if plain.iter().any(|p| line.starts_with(p)) {
                continue;
            }
            return false;
        }
        match line.as_bytes().first() {
            Some(b'+' | b'-') => {
                let body = line[1..].trim();
                if !(body.is_empty() || (body.starts_with('#') && !body.starts_with("#!"))) {
                    return false;
                }
                changed = true;
            }
            // "\ No newline at end of file" belongs to the line above it, which was judged.
            Some(b'\\') => {}
            _ => return false,
        }
    }
    changed
}

/// The text of `git diff -U0 <base> HEAD -- <f>` for ONE file, or `None` when git could not produce it.
/// The same flags as [`diff_patch`] (no renames, no external diff or textconv, no colour), so the text a
/// content rule judges is git's own and not a developer's filter's. Read by
/// [`super::docs_content::edit_moves_docs`].
pub fn file_patch(base: &str, f: &str, cwd: &Path) -> Option<String> {
    git_raw(
        &[
            "--literal-pathspecs",
            "diff",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "-U0",
            base,
            "HEAD",
            "--",
            f,
        ],
        cwd,
    )
}

/// `path` as it stands at commit `rev`, BYTE FOR BYTE (no trimming: a line-numbered reader needs the
/// leading blank lines too), or `None` when git cannot produce it or it is not UTF-8. Never reads the
/// working tree.
pub fn blob_at(rev: &str, path: &str, cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["cat-file", "blob", &format!("{rev}:{path}")])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// The subset of `candidates` (repository paths of `.rs` files in `base..HEAD`'s file list) whose edit
/// changes comments alone, judged by [`super::rust_comments::rust_edit_is_comment_only`] over each file's
/// section of ONE `git diff -U0` ([`diff_patch`]) and its two texts read through ONE `git cat-file
/// --batch` ([`blobs_at`]): two processes for the whole change, whatever its size. DOUBT IS ABSENCE: a
/// diff or a batch git could not produce (or past its bound) leaves every file out, and so does a file
/// whose section or text is missing. [`rust_comment_only_change`] is the same verdict for one file.
pub fn rust_comment_only_files(base: &str, candidates: &[&str], cwd: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if candidates.is_empty() {
        return out;
    }
    let Some(patch) = diff_patch(base, cwd) else { return out };
    // Each file's section, keyed by its `diff --git` header line (a path git had to quote matches no
    // candidate's header, and stays out).
    let mut sections: BTreeMap<&str, String> = BTreeMap::new();
    let mut current: Option<&str> = None;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            current = Some(line);
        }
        if let Some(header) = current {
            let section = sections.entry(header).or_default();
            section.push_str(line);
            section.push('\n');
        }
    }
    let mut specs = Vec::new();
    for f in candidates {
        specs.push(format!("{base}:{f}"));
        specs.push(format!("HEAD:{f}"));
    }
    let Some(texts) = blobs_at(&specs, cwd) else { return out };
    for (i, f) in candidates.iter().enumerate() {
        let Some(section) = sections.get(format!("diff --git a/{f} b/{f}").as_str()) else {
            continue;
        };
        let (old, new) = (texts[2 * i].as_deref(), texts[2 * i + 1].as_deref());
        if super::rust_comments::rust_edit_is_comment_only(section, old, new) {
            out.insert((*f).to_string());
        }
    }
    out
}

/// The most blob text [`blobs_at`] will hold, all blobs together; past it the batch is not read.
pub const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;

/// The text of each `rev:path` spec, in order, through ONE `git cat-file --batch`: `None` for a spec git
/// reports missing, an object that is not a blob, or a blob that is not UTF-8. `None` for the whole
/// answer when git could not run, its output did not parse, or it passed [`MAX_BLOB_BYTES`].
pub fn blobs_at(specs: &[String], cwd: &Path) -> Option<Vec<Option<String>>> {
    use std::io::{Read, Write};

    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // A thread writes the specs, so a full stdout pipe can never block the write and the read at once.
    let mut stdin = child.stdin.take()?;
    let input: String = specs.iter().map(|s| format!("{s}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()).is_ok());
    let mut bytes = Vec::new();
    let read = child.stdout.take()?.take(MAX_BLOB_BYTES as u64 + 1).read_to_end(&mut bytes);
    if bytes.len() > MAX_BLOB_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let wrote = writer.join().unwrap_or(false);
    let status = child.wait().ok()?;
    if read.is_err() || !wrote || !status.success() {
        return None;
    }
    let mut out = Vec::with_capacity(specs.len());
    let mut at = 0usize;
    for _ in specs {
        let end = at + bytes[at..].iter().position(|&b| b == b'\n')?;
        let header = std::str::from_utf8(&bytes[at..end]).ok()?;
        at = end + 1;
        // `<sha> <type> <size>`, then the content and a newline; or `<spec> missing` (and the like).
        let fields: Vec<&str> = header.split(' ').collect();
        let size = match fields[..] {
            [_, kind, size] => match size.parse::<usize>() {
                Ok(size) => Some((kind, size)),
                Err(_) => None,
            },
            _ => None,
        };
        match size {
            Some((kind, size)) => {
                let body = bytes.get(at..at + size)?;
                out.push((kind == "blob").then(|| String::from_utf8(body.to_vec()).ok()).flatten());
                at += size + 1;
            }
            None => out.push(None),
        }
    }
    Some(out)
}

/// True iff `base..HEAD` changed the Rust source `f` in its comments alone: the plain `-U0` diff of
/// the file ([`file_patch`]) and its text at `base` and at HEAD ([`blob_at`]), judged by
/// [`super::rust_comments::rust_edit_is_comment_only`]. DOUBT IS `false`: a diff or a side git cannot
/// produce (a new, deleted or renamed-away file, a base it cannot read) is a code edit to the caller.
///
/// The single-file form of [`rust_comment_only_files`], which the planner reads (one batch for the whole
/// change); the `#`-comment rule for global files is [`comment_only_change`], and the two never judge the
/// same file.
pub fn rust_comment_only_change(base: &str, f: &str, cwd: &Path) -> bool {
    let Some(patch) = file_patch(base, f, cwd) else { return false };
    let old = blob_at(base, f, cwd);
    let new = blob_at("HEAD", f, cwd);
    super::rust_comments::rust_edit_is_comment_only(&patch, old.as_deref(), new.as_deref())
}

/// The base commit's `Cargo.lock` as text, or `None` when git cannot produce it.
pub fn show_cargo_lock(base: &str, cwd: &Path) -> Option<String> {
    show_at(base, "Cargo.lock", cwd)
}

/// `path` as it stands at commit `rev`, or `None` when git cannot produce it (no such path at that
/// commit included). Never reads the working tree.
pub fn show_at(rev: &str, path: &str, cwd: &Path) -> Option<String> {
    git(&["show", &format!("{rev}:{path}")], cwd)
}
