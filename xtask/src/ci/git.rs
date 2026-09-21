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

use std::collections::BTreeMap;
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
/// `crates/vike-ops/tests/ci_plan_gate.rs` drive this ladder over a planted git topology with an
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
pub fn diff_names(base: &str, cwd: &Path) -> Option<Vec<String>> {
    let out = git_raw(&["diff", "--name-only", base, "HEAD"], cwd)?;
    Some(out.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// The base commit's `Cargo.lock` as text, or `None` when git cannot produce it.
pub fn show_cargo_lock(base: &str, cwd: &Path) -> Option<String> {
    git(&["show", &format!("{base}:Cargo.lock")], cwd)
}
