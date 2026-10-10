//! The `cargo:rerun-if-changed=` set; "the module doc" below is `build.rs`'s, whose table it emits.

use std::path::{Path, PathBuf};

use super::probe::git;

/// Emit one `cargo:rerun-if-changed=` line per watched path — the module doc's table says what each
/// one buys, and [`watch`] says why an absent path is skipped rather than emitted.
///
/// A `frozen` build stops after the package's own inputs and the toolchain pin: watching no git path
/// is what makes a commit leave it alone (the module doc's freeze section).
pub(super) fn emit_rerun_paths(manifest_dir: &Path, repo_root: &Path, frozen: bool) {
    // The script itself, the two source files it `include!`s and the three `build/` modules it
    // declares with `#[path]`. Emitting ANY rerun line disables cargo's default package-wide watch,
    // so these have to be named or an edit to any of them is invisible to the generated file —
    // `src/freeze.rs` above all, since it decides which arm runs, and `build/render.rs`, which IS
    // the generated file's text.
    watch(&manifest_dir.join("build.rs"));
    watch(&manifest_dir.join("src").join("timefmt.rs"));
    watch(&manifest_dir.join("src").join("freeze.rs"));
    watch(&manifest_dir.join("build").join("probe.rs"));
    watch(&manifest_dir.join("build").join("render.rs"));
    watch(&manifest_dir.join("build").join("watch.rs"));
    // The pinned toolchain — the input behind `RUSTC_VERSION`, and the only watched path that is
    // not git.
    watch(&repo_root.join("rust-toolchain.toml"));
    if frozen {
        return;
    }

    // ⚠ TWO git directories, not one, and this crate is routinely built inside a linked WORKTREE
    // where they differ: `.git` is then a FILE holding `gitdir: …/.git/worktrees/<name>`, HEAD and
    // the reflog live in that per-worktree directory, and the branch REFS live in the common one.
    // Resolving `.git/HEAD` against the repo root — the obvious implementation — reads a one-line
    // text file that is not HEAD at all in that layout.
    let Some(git_dir) = git(manifest_dir, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
    else {
        return; // not a repository: nothing to watch, and every fact already degraded to UNKNOWN
    };
    // `--git-common-dir` answers relative to the CWD in a main worktree (`.git`) and absolutely in
    // a linked one, so it is resolved against the directory the probe ran in.
    let common_dir = git(manifest_dir, &["rev-parse", "--git-common-dir"])
        .map(|d| {
            let p = PathBuf::from(&d);
            if p.is_absolute() { p } else { manifest_dir.join(p) }
        })
        .unwrap_or_else(|| git_dir.clone());

    watch(&git_dir.join("HEAD"));
    // The per-worktree reflog, and whether it was THERE is remembered rather than discarded: it is
    // the one file that moves in the packed-ref case below, so its absence changes what is enough.
    let reflog_watched = watch(&git_dir.join("logs").join("HEAD"));
    watch(&common_dir.join("packed-refs"));

    // The LOOSE ref for the checked-out branch, if there is one. A detached HEAD has no symbolic
    // ref and needs none — HEAD then holds the sha itself, and HEAD is already watched.
    if let Some(reference) = git(manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
        // `refs/heads/feat/boot-identity` — a relative path with `/` separators, which `Path::join`
        // handles on Windows too. Both directories are tried: a worktree can carry its own refs.
        let loose = common_dir.join(&reference);
        let in_common = watch(&loose);
        let in_worktree = watch(&git_dir.join(&reference));
        // ⚠ The measured staleness hole — see the module doc. No reflog AND no loose ref means the
        // ref is packed and the commit that unpacks it would move nothing this script is watching,
        // so the absent path is named anyway and cargo reruns until it exists. Deliberately NOT
        // unconditional: in every ordinary repository the reflog is there (git's own default for a
        // non-bare repo), and naming a missing path there would relink every binary wired to this crate on
        // every build for a rerun that buys nothing.
        if !reflog_watched && !in_common && !in_worktree {
            println!("cargo:rerun-if-changed={}", loose.display());
        }
    }
}

/// Emit `cargo:rerun-if-changed=` for a path THAT EXISTS, and report whether it did.
///
/// A missing path makes cargo rerun the script on every single build — it cannot stat the path, so
/// it assumes changed — and every binary WIRED to this crate is at the top of the workspace graph,
/// so "every build" means relinking all of them. (Not the converse: vike-run's three bins were
/// top-of-graph and linked none of this, and the one that survives, `incident`, is
/// `crates/vike-mount`'s since docs/decisions/0098 and links none of it either. The cost is the
/// adopted set, which
/// `crates/vike-buildinfo/tests/identity_adoption.rs` derives — it is still every daemon that gets
/// deployed.) The `bool` is what lets the one caller that MUST pay that price know it has to (see
/// [`emit_rerun_paths`]).
fn watch(path: &Path) -> bool {
    let present = path.exists();
    if present {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    present
}
