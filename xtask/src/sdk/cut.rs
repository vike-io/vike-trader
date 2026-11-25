//! `just sdk-cut`: tag HEAD `sdk-(N+1)` after its checks (decision 0296; `docs/ops/sdk-release.md`).
//!
//! **The checks, in order; any failure tags nothing.**
//! 1. The working tree is CLEAN (`git status --porcelain` says nothing; a gitignored path such as
//!    `target/` or `tmp/` is not listed): the hashes and the compile below read the working tree, so a
//!    clean tree is what makes them HEAD's. Refusing a dirty tree, rather than hashing and compiling a
//!    `git archive HEAD` copy, keeps the compile on the checkout's warm `target/`; a cut runs on a lane or a
//!    CI checkout, which is clean (a lane's claim file is gitignored). Untracked files count: the walks read
//!    the working tree, so one under a hashed folder would be hashed. Asked again after the compile, which must not have changed the tree (a
//!    stale `Cargo.lock` it rewrote).
//! 2. The tags are current: `git fetch origin main` and every `sdk-*` tag, and every local `sdk-*` tag is on
//!    `origin` (one a `--no-push` or an interrupted cut left would make the next number skip one on
//!    `origin`: push it or delete it first). Skipped with `--no-push`, a local cut, so a box with no remote
//!    can rehearse; its local tags count there, and nothing it makes reaches `origin`.
//! 3. HEAD is a commit of `main` (`origin/main` holds it): an SDK is cut from what `main` shipped.
//! 4. HEAD carries no `sdk-*` tag yet: one commit, one SDK.
//! 5. The boundary and the source hashes of HEAD's tree compute (the walks `build.rs` bakes, sixteen hex
//!    digits each), and the boundary is compared with the newest SDK's ([`super::boundary`]).
//! 6. The shipped strategies compile at HEAD: the strategy lab's test build (`cargo test -p
//!    vike-strategy-lab --no-run`, its `gallery` feature on), every shipped file a module of it.
//!
//! Then, unless `--dry-run`, an annotated tag `sdk-(N+1)` on HEAD whose message records the hashes, and
//! (unless `--no-push`) its push to `origin`. A push that fails deletes the local tag again, so the next
//! run numbers from what `origin` has. A lane cannot push a tag: there the cut is a `--dry-run`, and
//! `scripts/sdk_cut_tag.sh` (`just sdk-cut-tag`) makes the tag it printed, its message read back from that
//! output, on the dev box.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Boundary, TAG_PREFIX, git, newest_tag};

/// What a cut is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutOptions {
    /// Run every check, tag nothing.
    pub dry_run: bool,
    /// Fetch `origin`'s tags first and push the new one there; off (`--no-push`), the tag stays in this
    /// repository.
    pub push: bool,
}

impl CutOptions {
    /// The options `args` spell (`--dry-run`, `--no-push`); `Err` for any other word.
    pub fn parse(args: &[String]) -> Result<CutOptions, String> {
        let mut options = CutOptions { dry_run: false, push: true };
        for arg in args {
            match arg.as_str() {
                "--dry-run" => options.dry_run = true,
                "--no-push" => options.push = false,
                other => {
                    return Err(format!("sdk-cut takes --dry-run and --no-push, not {other:?}"));
                }
            }
        }
        Ok(options)
    }
}

/// The hashes and number a cut records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    pub number: u32,
    pub commit: String,
    pub boundary: Boundary,
    /// The source hash of HEAD's tree over both templates (`build.rs`'s `SOURCE_HASH`).
    pub source_hash: String,
}

impl Cut {
    /// The tag's name.
    pub fn tag(&self) -> String {
        format!("{TAG_PREFIX}{}", self.number)
    }

    /// The annotated tag's message.
    pub fn message(&self) -> String {
        format!(
            "{tag}: strategy SDK {n} (decision 0296)\n\ncommit = \"{}\"\nboundary_hash = \"{}\"\nsource_hash = \"{}\"\n{}\n",
            self.commit,
            self.boundary.head,
            self.source_hash,
            self.boundary.sentence(),
            tag = self.tag(),
            n = self.number,
        )
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Check 1: `Err` naming what is not committed, unless the working tree at `cwd` is clean.
fn clean(cwd: &Path, when: &str) -> Result<(), String> {
    let status =
        git(&["status", "--porcelain"], cwd).ok_or("cannot read the working tree's status")?;
    if status.trim().is_empty() {
        return Ok(());
    }
    let first: Vec<&str> = status.lines().take(5).collect();
    Err(format!(
        "the working tree is not clean {when} ({}): an SDK is hashed and compiled from HEAD's tree, so \
         commit, stash or remove the changes first",
        first.join("; ")
    ))
}

/// Check 2's second half: `Err` naming every local `sdk-*` tag `origin` does not have.
fn unpushed_tags(cwd: &Path) -> Result<(), String> {
    let pattern = format!("refs/tags/{TAG_PREFIX}*");
    let remote = git(&["ls-remote", "--tags", "origin", &pattern], cwd)
        .ok_or("cannot list origin's sdk-* tags: the next number would be a guess")?;
    let on_origin: BTreeSet<&str> = remote
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .map(|name| name.trim_start_matches("refs/tags/").trim_end_matches("^{}"))
        .collect();
    let local = git(&["tag", "--list", &format!("{TAG_PREFIX}*")], cwd).unwrap_or_default();
    let unpushed: Vec<&str> = local
        .lines()
        .map(str::trim)
        .filter(|tag| super::tag_number(tag).is_some() && !on_origin.contains(tag))
        .collect();
    if unpushed.is_empty() {
        return Ok(());
    }
    Err(format!(
        "local SDK tag(s) {unpushed:?} are not on origin (a --no-push or an interrupted cut left them): the \
         next number would skip one on origin. Push them (`git push origin refs/tags/<tag>`) or delete them \
         (`git tag --delete <tag>`) first"
    ))
}

/// Cut the next SDK at `cwd`'s HEAD, `compile` being check 6 (the module doc's list); the answer is what
/// was done, in words.
pub fn cut(
    cwd: &Path,
    options: CutOptions,
    compile: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<String, String> {
    clean(cwd, "before the cut")?;
    if options.push {
        git(
            &[
                "fetch",
                "--quiet",
                "origin",
                "main",
                &format!("+refs/tags/{TAG_PREFIX}*:refs/tags/{TAG_PREFIX}*"),
            ],
            cwd,
        )
        .ok_or("cannot fetch origin's main and sdk-* tags: the next number would be a guess")?;
        unpushed_tags(cwd)?;
    }
    let commit =
        git(&["rev-parse", "HEAD"], cwd).map(|s| s.trim().to_string()).ok_or("no HEAD commit")?;
    if git(&["merge-base", "--is-ancestor", &commit, "origin/main"], cwd).is_none() {
        return Err(format!(
            "HEAD {commit} is not on origin/main: an SDK is cut from a commit main holds"
        ));
    }
    let tagged = git(&["tag", "--points-at", &commit, "--list", &format!("{TAG_PREFIX}*")], cwd)
        .unwrap_or_default();
    if let Some(tag) = tagged.lines().find(|t| super::tag_number(t.trim()).is_some()) {
        return Err(format!("HEAD {commit} is already {tag}: one commit is one SDK"));
    }
    let number = newest_tag(cwd).map_or(1, |(n, _)| n + 1);
    let boundary = super::boundary(cwd)?;
    let template = |relative: &str| {
        std::fs::read_to_string(cwd.join(super::BOUNDARY_CRATE_DIR).join(relative))
            .unwrap_or_default()
    };
    let templates = crate::ci::source_hash::TEMPLATE_MANIFESTS.map(template);
    let templates: Vec<&str> = templates.iter().map(String::as_str).collect();
    let source_hash = crate::ci::source_hash::source_hash(
        cwd,
        &templates,
        crate::ci::source_hash::HashForm::Tokens,
    )
    .hash;
    if !is_hash(&boundary.head) || !is_hash(&source_hash) {
        return Err(format!(
            "the hashes did not compute (boundary {:?}, source {source_hash:?})",
            boundary.head
        ));
    }
    compile(cwd)?;
    clean(cwd, "after the compile")?;
    let cut = Cut { number, commit, boundary, source_hash };
    let tag = cut.tag();
    if options.dry_run {
        return Ok(format!(
            "dry run: every check passed; would tag {tag} at {}:\n{}",
            cut.commit,
            cut.message()
        ));
    }
    git(&["tag", "--annotate", &tag, "--message", &cut.message(), &cut.commit], cwd)
        .ok_or_else(|| format!("git tag {tag} failed"))?;
    if options.push
        && git(&["push", "--quiet", "origin", &format!("refs/tags/{tag}")], cwd).is_none()
    {
        let _ = git(&["tag", "--delete", &tag], cwd);
        return Err(format!("pushing {tag} to origin failed; the local tag is deleted again"));
    }
    let pushed = if options.push { "tagged and pushed" } else { "tagged here (not pushed)" };
    Ok(format!("{pushed} {tag} at {}:\n{}", cut.commit, cut.message()))
}
