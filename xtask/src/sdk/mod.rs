//! The versioned strategy SDK's release discipline (decision 0296; `docs/ops/sdk-release.md` is the
//! runbook): which SDK is the newest ([`newest_tag`]), whether the strategy boundary moved since it
//! ([`boundary`], the boundary-forced cut), what a strategy file can see change since it ([`report`],
//! with its [`rename_table`]), and the cut itself ([`cut`]).
//!
//! **An SDK is a TAG.** `sdk-N` is an annotated tag on a commit of `main`, and nothing in the tree
//! records it: a deploy packages the newest tag's tree and writes its marker there (`sdk-N/SDK`,
//! `crates/vike-rust-strategy/src/sdk.rs`'s `SdkMarker`). So every answer here reads the tag's TREE
//! out of git ([`materialize`]) and runs the same walk over both sides: the boundary hash is
//! `crates/vike-rust-strategy/build/source_hash.rs`'s `boundary_hash` (compiled into this crate by
//! `#[path]`, `ci::source_hash`), the walk the marker and the host's `BOUNDARY_HASH` are made by. The
//! boundary check alone compares HEAD's with the hash the tag RECORDS ([`recorded_boundary`]): that is the
//! one its marker holds, which a box compares the host's against.
//!
//! Std only, like the planner: git and tar are run as processes.

pub mod cut;
pub mod items;
/// The rename table's format, writer and reader: the file `vike-cli strategy migrate` reads through
/// `vike_rust_strategy::sdk::rename_table`, compiled in here (std only) since xtask links no `vike-*` crate.
#[path = "../../../crates/vike-rust-strategy/src/sdk/rename_table.rs"]
pub mod rename_table;
pub mod report;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The crate whose C-ABI glue, templates and goldens are the strategy BOUNDARY (decision 0296, verdict
/// 7): the host's, never frozen in an SDK.
pub const BOUNDARY_CRATE_DIR: &str = crate::ci::tables::roster::STRATEGY_TEMPLATE_CRATE_DIR;

/// The prefix of an SDK's tag, `sdk-N`.
pub const TAG_PREFIX: &str = "sdk-";

/// Where a tag's tree is written for the walks, under the repository root: the root's gitignored
/// scratch folder, never the system temp directory.
pub const SCRATCH_DIR: &str = "tmp/sdk-trees";

/// The number an `sdk-N` tag names; `None` for any other name (`sdk-03` too: one SDK, one tag).
pub fn tag_number(tag: &str) -> Option<u32> {
    let rest = tag.strip_prefix(TAG_PREFIX)?;
    let number: u32 = rest.parse().ok()?;
    (number.to_string() == rest).then_some(number)
}

/// The newest SDK tag the repository at `cwd` holds: its number and name. `None` when it holds none
/// (or git cannot list them).
pub fn newest_tag(cwd: &Path) -> Option<(u32, String)> {
    let out = git(&["tag", "--list", &format!("{TAG_PREFIX}*")], cwd)?;
    out.lines().filter_map(|t| tag_number(t.trim()).map(|n| (n, t.trim().to_string()))).max()
}

/// `git <args>` in `cwd`: its stdout, or `None` when it failed.
pub(crate) fn git(args: &[&str], cwd: &Path) -> Option<String> {
    let out =
        Command::new("git").args(args).current_dir(cwd).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A tree written out of git: removed again when dropped.
pub struct Materialized {
    pub root: PathBuf,
}

impl Drop for Materialized {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The files of `rev` under the `pathspecs`, written under `<cwd>/`[`SCRATCH_DIR`]`/<rev>` through `git
/// archive | tar -x`, bytes as committed. Removed when the answer is dropped.
pub fn materialize(rev: &str, pathspecs: &[&str], cwd: &Path) -> Result<Materialized, String> {
    let root = cwd.join(SCRATCH_DIR).join(rev.replace(['/', ':'], "_"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let tree = Materialized { root };
    extract(rev, pathspecs, cwd, &tree.root)?;
    Ok(tree)
}

/// Add the files of `rev` under `pathspecs` to the tree at `dest`.
pub fn extract(rev: &str, pathspecs: &[&str], cwd: &Path, dest: &Path) -> Result<(), String> {
    let mut archive = Command::new("git")
        .args(["archive", "--format=tar", rev, "--"])
        .args(pathspecs)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run git archive: {e}"))?;
    let stdout = archive.stdout.take().ok_or("git archive gave no output")?;
    let tar = Command::new("tar")
        .arg("-x")
        .arg("-f")
        .arg("-")
        .arg("-C")
        .arg(dest)
        .stdin(stdout)
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    let archived = archive.wait().map_err(|e| format!("git archive: {e}"))?;
    // A pathspec that matches nothing at `rev` makes `git archive` fail: that is no file, not an error.
    if !tar.success() && archived.success() {
        return Err(format!("tar could not unpack {rev}'s {pathspecs:?}"));
    }
    Ok(())
}

/// The boundary hash of the tree at `root` (`build/source_hash.rs`'s `boundary_hash` over its
/// [`BOUNDARY_CRATE_DIR`], read as its tokens: a comment moves it not).
pub fn boundary_hash(root: &Path) -> String {
    use crate::ci::source_hash::{HashForm, boundary_hash};
    boundary_hash(&root.join(BOUNDARY_CRATE_DIR), HashForm::Tokens).hash
}

/// The boundary hash the annotated tag `tag` records (`boundary_hash = "…"` in its message, written by
/// [`cut`] with the walk of ITS commit, the hash its marker holds); `None` for a tag that records none.
/// A box compares the host's hash with the marker's, never with one recomputed by a later walk: an SDK cut
/// before the walk read tokens (SDK 1) records the hash of its bytes, which no later host's equals.
pub fn recorded_boundary(tag: &str, cwd: &Path) -> Option<String> {
    let message = git(&["tag", "--list", "--format=%(contents)", tag], cwd)?;
    message.lines().find_map(|line| {
        let value = line.trim().strip_prefix("boundary_hash")?.trim_start().strip_prefix('=')?;
        let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        (value.len() == 16
            && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then(|| value.to_string())
    })
}

/// The boundary-forced cut's question: has the strategy boundary moved since the newest SDK?
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// The newest SDK's number, `None` when no `sdk-*` tag exists.
    pub sdk: Option<u32>,
    /// HEAD's boundary hash.
    pub head: String,
    /// The newest SDK's boundary hash, the one its tag records ([`recorded_boundary`]; the tag's tree hashed
    /// by this walk for a tag that records none), `None` with no tag.
    pub at_sdk: Option<String>,
}

impl Boundary {
    /// Whether the next deploy must cut an SDK: no SDK exists yet, or HEAD's boundary is not the newest
    /// SDK's. An SDK whose boundary is not the host's builds nothing on it (`crates/vike-rust-strategy/src/sdk.rs`'s
    /// `usable`), so a deploy of a moved boundary without a cut would leave every pinned file NOT RUNNING.
    /// An SDK adapter forces none: a box takes its adapters from the vike it deploys (decision 0296's
    /// amendment of 2026-11-20), so an adapter's fix reaches it with any deploy.
    pub fn cut_due(&self) -> bool {
        self.at_sdk.as_deref() != Some(self.head.as_str())
    }

    /// The `key=value` lines a workflow step reads (`sdk`, `boundary`, `cut`).
    pub fn render(&self) -> String {
        let sdk = self.sdk.map(|n| n.to_string()).unwrap_or_default();
        let boundary = match &self.at_sdk {
            None => "none",
            Some(at) if at != &self.head => "moved",
            Some(_) => "same",
        };
        format!(
            "sdk={sdk}\nboundary={boundary}\nboundary_hash={}\ncut={}\n",
            self.head,
            self.cut_due()
        )
    }

    /// One sentence for a person.
    pub fn sentence(&self) -> String {
        match (self.sdk, &self.at_sdk) {
            (Some(n), Some(at)) if at == &self.head => {
                format!("the strategy boundary is sdk-{n}'s ({at}): no cut is forced")
            }
            (Some(n), Some(at)) => format!(
                "the strategy boundary MOVED since sdk-{n} ({at} -> {}): the next deploy cuts sdk-{}",
                self.head,
                n + 1
            ),
            _ => format!(
                "no sdk-* tag exists yet (boundary {}): the next deploy cuts sdk-1",
                self.head
            ),
        }
    }
}

/// [`Boundary`] of the repository at `cwd`: HEAD's working tree against the newest `sdk-*` tag's tree.
pub fn boundary(cwd: &Path) -> Result<Boundary, String> {
    let head = boundary_hash(cwd);
    let Some((n, tag)) = newest_tag(cwd) else {
        return Ok(Boundary { sdk: None, head, at_sdk: None });
    };
    let at_sdk = match recorded_boundary(&tag, cwd) {
        Some(recorded) => recorded,
        None => boundary_hash(&materialize(&tag, &[BOUNDARY_CRATE_DIR], cwd)?.root),
    };
    Ok(Boundary { sdk: Some(n), head, at_sdk: Some(at_sdk) })
}

/// The SDK report of the repository at `cwd`: the newest `sdk-*` tag's tree against HEAD's working
/// tree. `None` when no tag exists.
pub fn sdk_report(cwd: &Path) -> Result<Option<report::SdkReport>, String> {
    let Some((_, tag)) = newest_tag(cwd) else { return Ok(None) };
    report_from(&tag, cwd).map(Some)
}

/// The SDK report of the repository at `cwd`: the tree of the revision `tag` against HEAD's working tree.
/// [`sdk_report`] asks it of the newest tag; the packer asks it of the SDK before the one it packs, at that
/// SDK's tag, for the step's rename table (`xtask sdk-renames`, `scripts/package_strategy_source.sh`).
pub fn report_from(tag: &str, cwd: &Path) -> Result<report::SdkReport, String> {
    // `git archive` of a revision that does not exist writes nothing a pipe would refuse: an empty tree here,
    // and a report of everything added. Named first, so a missing tag is an error and never an empty table.
    if git(&["rev-parse", "--verify", "-q", &format!("{tag}^{{commit}}")], cwd).is_none() {
        return Err(format!("{tag} names no commit in this repository"));
    }
    let tree = materialize(tag, &["crates/*/Cargo.toml", BOUNDARY_CRATE_DIR], cwd)?;
    let source: Vec<String> = report::sdk_crates(&tree.root)
        .iter()
        .chain([&report::RHAI_CRATE_DIR.to_string()])
        .map(|dir| format!("{dir}/src"))
        .collect();
    let source: Vec<&str> = source.iter().map(String::as_str).collect();
    extract(tag, &source, cwd, &tree.root)?;
    let old = report::read_tree(&tree.root, tag);
    let head = git(&["rev-parse", "HEAD"], cwd)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "HEAD".into());
    let new = report::read_tree(cwd, &head);
    Ok(report::compare(&old, &new))
}
