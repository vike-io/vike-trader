//! Resolve this build's IDENTITY at compile time and write it into `$OUT_DIR/buildinfo.rs`, which
//! `src/lib.rs` includes.
//!
//! # The incident this exists for
//!
//! A release binary was built on the the CI box test clone from that box's BARE repo, which was four
//! commits behind `main`, and it was nearly installed on the live recorder. The root `CLAUDE.md`
//! still carries the two runbook rules written in response — push `origin/main` to the bare repo
//! first, and `git checkout -f -q -B <branch> origin/<branch>` rather than a plain checkout — and
//! both are the same kind of defence: a human remembering a step. A binary that states the commit
//! it was built from turns that into a line you READ.
//!
//! # Why the values are written to a file instead of `cargo:rustc-env`
//!
//! `cargo:rustc-env` would need `env!("VIKE_BUILD_GIT_SHA")` in `src/lib.rs`, and
//! `crates/vike-ops/src/scan.rs`'s `find_map_lookups` harvests EVERY env-shaped string literal
//! under a `src/` directory whose name carries a known prefix — `VIKE_` is one — so the settings
//! registry would demand a `vike_ops::settings::SETTINGS` row for a compile-time constant no
//! operator can set. A generated file has no literal to harvest and no row to justify.
//!
//! # ⚠ Staleness — the one thing a build-identity crate must not get wrong
//!
//! A cached SHA is worse than no SHA, because it lies with authority. Cargo reruns a build script
//! only when a path named by a `cargo:rerun-if-changed=` line changes, and emitting even one such
//! line switches OFF the default "rerun when any file in the package changed". So the set below is
//! the whole contract:
//!
//! | watched | changes on |
//! |---|---|
//! | `<git dir>/HEAD` | `git checkout`, `git switch`, a detached-HEAD move |
//! | `<git dir>/logs/HEAD` | every commit / checkout / reset / rebase (the per-worktree reflog) |
//! | `<common dir>/refs/heads/<branch>` | a commit on the checked-out branch, when the ref is LOOSE |
//! | `<common dir>/packed-refs` | a commit on the checked-out branch, when the ref is PACKED |
//! | `rust-toolchain.toml` | the pinned toolchain, i.e. `RUSTC_VERSION` |
//! | `$VIKE_BUILDINFO_FREEZE` (an ENV watch) | the CI freeze switch being set, unset or changed |
//!
//! ⚠ **A FROZEN build watches only the last two rows** (plus this package's own sources) — no git
//! path at all, which is the whole point of freezing. The section below is the argument for why
//! that cannot reach a shipped binary.
//!
//! ⚠ **One combination defeats that table, and [`emit_rerun_paths`] handles it explicitly.** With
//! the branch ref PACKED there is no loose ref to watch, and `git commit` then CREATES one while
//! leaving `packed-refs` and `HEAD` untouched — so the reflog is the only watched file that moves.
//! A repository with `core.logAllRefUpdates=false` has no reflog, and the script does not rerun.
//! That was MEASURED on a scratch repository in that exact state rather than reasoned about — a
//! commit landed, the next build was a cache hit, and the generated file went on naming the
//! PREVIOUS commit. In that configuration, and only there, the absent loose ref is watched ANYWAY:
//! cargo cannot stat it, so the script reruns every build until the ref appears. Paying a rerun
//! beats shipping a binary that names the wrong commit with authority.
//!
//! **`GIT_SHA` therefore cannot go stale**: every operation that moves HEAD touches at least one
//! watched path, in every ref/reflog configuration. **`GIT_DIRTY` can**, and there is no cheap fix
//! — an unstaged edit to some other
//! crate touches nothing this script can watch, and the only path that would nearly catch it
//! (watching the git index) still misses unstaged edits and would relink every binary wired to this crate
//! each time somebody ran `git status`. So read the flag asymmetrically: `dirty` is authoritative,
//! `clean` means "no committed change since this crate was last built". A release build from a
//! fresh checkout has neither problem, which is the case the incident above is about.
//!
//! ⚠ **Only paths that EXIST are emitted.** Cargo cannot stat a missing path, treats the result as
//! changed, and reruns the script on EVERY build — which, since every binary wired to this crate is
//! at the top of the workspace graph, means relinking all of them every time. ⚠ Wired ⇒
//! top-of-graph, never the converse: vike-run was top-of-graph and its three bins linked none of
//! this, and its one surviving bin (`incident`, built from `crates/vike-mount` since
//! docs/decisions/0098 merged the two) links none of it either, so the cost is the ADOPTED set
//! (`crates/vike-buildinfo/tests/identity_adoption.rs` derives it) rather than every binary in the
//! workspace.
//!
//! ⚠ **`git status` is run with `--no-optional-locks`.** Without it, git refreshes the index's stat
//! cache as a side effect of the probe, which is a write on every build.
//!
//! # The CI freeze switch — and why a release build ignores it
//!
//! The table above has a price, and CI pays it on every run. Every commit moves the reflog, so
//! every commit reruns this script, and cargo has no early cutoff: once the script has rerun, every
//! unit that depends on this crate is dirty, whatever bytes the generated file holds. MEASURED on a
//! warm the latency box lane with CI's build environment (2026-10-03, two separate runs on a shared box): one
//! EMPTY commit, then the roster's `cargo nextest run --no-run`, rebuilt 13 packages / 210 rustc
//! units in 104-114 s (880 s of summed rustc time), and clippy another 19-43 s — for a commit that
//! changed no source at all.
//!
//! So `VIKE_BUILDINFO_FREEZE=1` — set by `.github/workflows/ci.yml` for its whole run, and by
//! nothing that builds a binary anybody installs — FREEZES the identity of a DEBUG build: no git
//! probe, no git path watched, and the generated file says `FROZEN` (`frozen`) for the commit,
//! `None` for the tree state and `0` for the timestamp. `RUSTC_VERSION`, `TARGET` and `PROFILE`
//! stay real — they are not what moves, and keeping the timestamp constant is what lets a frozen
//! rlib come out byte-identical on every runner, so the shared compile cache can serve it. The same
//! measurement with the switch set rebuilt 0 units: 1.8 s for the build and 0.9 s for clippy. The
//! price is one cascade the first time a target directory builds frozen (110 s, measured).
//!
//! Why this does not reopen the incident above, one property at a time:
//!
//! * **A release build IGNORES the switch.** `src/freeze.rs`'s `freeze_applies` freezes only when
//!   cargo reports `PROFILE=debug`, and every binary that ships is a release build —
//!   `.github/workflows/release.yml` builds `--release`, every unit under `deploy/` installs one,
//!   and the container image builds one. A switch leaked into an operator's shell or copied into
//!   another workflow therefore cannot freeze a shipped binary; this script prints a
//!   `cargo:warning` saying it was ignored, and probes git as always. (That warning is ROUTINE in
//!   CI's own release-profile jobs and in a lane's release build, which inherit the switch — it
//!   records the guard working, not a leak.) The test FAILS CLOSED: a
//!   profile cargo reports as anything but the exact word `debug` takes the real probe, and a
//!   custom profile that `inherits = "release"` is reported as `release` (measured, not assumed).
//! * **`frozen` cannot be read as a commit.** It is not hex, so it can never be a WRONG sha that
//!   looks right, and it is not `UNKNOWN`, so nobody can mistake it for a probe that failed on a
//!   box with no git. Where a consumer COMPARES the value — `vike-strategy-builder`'s source-version
//!   check — `frozen` matches no stamp a release writes, so it refuses rather than trusting it.
//! * **The identity of a debug build is not an artifact.** What CI and a verification lane build
//!   under the switch are test binaries and lint passes, run and thrown away on the box that built
//!   them. Nothing installs them, so nothing ever has to answer "which commit is this?" about them.
//! * **Turning the switch OFF over the same target directory reruns the script** — the env watch
//!   above is emitted on every arm — so the next build names the real HEAD again. MEASURED on the
//!   same lane: `vike-cli --version` printed `git frozen dirty?` with the switch set, the real sha
//!   the moment it was unset, and the next commit's sha after the next commit.
//! * **The real probe keeps a CI witness.** `.github/workflows/ci.yml`'s `plan` job, which runs on
//!   every PR and every push, runs `cargo test --release -p vike-buildinfo` WITH the switch still
//!   set. That build takes the real probe, so `crates/vike-buildinfo/src/lib.rs`'s
//!   `the_git_probe_resolved_in_a_checkout` still asserts a real sha in CI, and
//!   `a_release_build_is_never_frozen` asserts the guard held under exactly the condition a leak
//!   would create.
//!
//! ⚠ **`PROFILE` is deliberately NOT watched.** `cargo:rerun-if-env-changed` compares the
//! environment cargo was INVOKED with, never the variables cargo sets for the script (cargo's own
//! build-script reference says so), so a `PROFILE` watch would watch an unrelated shell variable.
//! Nor does it need one: the profile is part of the build-script UNIT's identity, so a debug and a
//! release build never share a run of this script — the frozen debug output and a release build's
//! real one sit side by side in one target directory, which the measurement above did.
//!
//! The alternatives, both rejected on measurement: freezing EVERY debug build (no switch) would take
//! the sha off the dev box's own `cargo run`; and taking the identity out of the dependents' input
//! saves nothing, because 209 of those 210 units were dirty on the script's output TIME, not on its
//! bytes.
//!
//! # Degrading
//!
//! Every probe is allowed to fail: no `git` on the box, a source tarball with no repository, a
//! shallow clone with no commits. Each unresolved fact becomes `UNKNOWN` and the build SUCCEEDS — a
//! crate whose whole job is to describe the build must never be the reason there is no build.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

// The RFC-3339 formatter, shared with the library rather than copied into it: `src/lib.rs` compiles
// the same file as a module and its tests are the only ones that ever run (a build script's
// `#[cfg(test)]` code is never built as a test). `iso8601_utc(BUILD_EPOCH_SECS) == BUILD_TIMESTAMP`
// is asserted there, which is what proves the two consts below are the same instant.
include!("src/timefmt.rs");

// The release guard on the freeze switch — shared with the library for the same reason, so the
// decision is unit-tested over inputs no CI run produces. The module doc's freeze section is the
// argument; that file's own header says why it carries no variable name.
include!("src/freeze.rs");

/// What an unresolved fact reports. Written INTO the generated file rather than declared in
/// `src/lib.rs`, so the spelling the build script produced and the spelling a consumer compares
/// against are one constant and cannot drift.
const UNKNOWN: &str = "unknown";

/// What a FROZEN build reports for its commit — the module doc's freeze section. Written into the
/// generated file beside [`UNKNOWN`] for the same one-spelling reason. Not hex, so it can never be
/// read as a commit; not `unknown`, so it can never be read as a failed probe.
const FROZEN: &str = "frozen";

/// The CI-only freeze switch. Honoured only as the exact value `1`, and only in a debug build:
/// [`freeze_applies`] is the whole decision.
const FREEZE_ENV: &str = "VIKE_BUILDINFO_FREEZE";

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.join("..").join("..");

    // ON EVERY ARM, frozen or not: turning the switch on OR off must rerun this script, or a target
    // directory that last built frozen would go on saying `frozen` once the switch is gone.
    println!("cargo:rerun-if-env-changed={FREEZE_ENV}");
    let switch = std::env::var(FREEZE_ENV).ok();
    let profile = std::env::var("PROFILE").ok();
    let frozen = freeze_applies(switch.as_deref(), profile.as_deref());
    if let Some(value) = switch.as_deref().filter(|_| !frozen) {
        // Said in the build log so the stamped identity's provenance is never a guess. EXPECTED in
        // CI's release-profile jobs (`plan`'s real-probe step, `latency`) and in a lane's release
        // build, which all inherit the switch and all ignore it; this is the guard working, not a
        // leak. Either way the identity stamped below is the real one.
        println!(
            "cargo:warning={FREEZE_ENV}={value:?} is ignored for this `{}` build: its identity is \
             probed from git as always (only the exact value \"1\" in a `debug` build freezes it; \
             expected in CI's release-profile jobs)",
            profile.as_deref().unwrap_or(UNKNOWN)
        );
    }

    let (sha, dirty, epoch_secs) =
        if frozen { (FROZEN.to_string(), None, 0) } else { probe_git(&manifest_dir) };

    // `$RUSTC` is what cargo is actually invoking, which is not necessarily the `rustc` on PATH.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_string());

    let target = std::env::var("TARGET").unwrap_or_else(|_| UNKNOWN.to_string());
    let profile = profile.unwrap_or_else(|| UNKNOWN.to_string());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let generated = render(&sha, dirty, epoch_secs, &rustc_version, &target, &profile);
    std::fs::write(out_dir.join("buildinfo.rs"), generated).expect("write buildinfo.rs");

    emit_rerun_paths(&manifest_dir, &repo_root, frozen);
}

/// The three facts git answers — `(sha, dirty, epoch_secs)` — for a build that is NOT frozen.
///
/// The build time rides along because it is only meaningful beside a real commit: a frozen build
/// writes `0` instead, so its rlib is the same bytes on every runner (the module doc's freeze
/// section).
fn probe_git(manifest_dir: &Path) -> (String, Option<bool>, u64) {
    let sha = git(manifest_dir, &["rev-parse", "--short", "HEAD"])
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| UNKNOWN.to_string());

    // `--no-optional-locks`: see the module doc. Empty output ⇒ clean; a failed probe ⇒ we do not
    // know, which is a THIRD answer and is reported as one rather than rounded to "clean".
    //
    // ⚠ UNTRACKED files count as dirty (plain `--porcelain`, not `--untracked-files=no`). An
    // untracked-but-unignored `.rs` file IS part of the build, and over-reporting dirty is the safe
    // direction here for the same reason over-redaction is elsewhere: a false `dirty` costs one
    // `git status`, a false `clean` costs an unidentifiable binary on a trading box.
    let dirty = git(manifest_dir, &["--no-optional-locks", "status", "--porcelain"])
        .map(|out| !out.trim().is_empty());

    // Seconds since the epoch. A clock before 1970 is not a case worth carrying code for.
    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    (sha, dirty, epoch_secs)
}

/// The generated `$OUT_DIR/buildinfo.rs`, as text.
///
/// Every string is written through `{:?}` — `Debug for str`, i.e. a valid Rust string literal with
/// quotes and backslashes escaped. Not optional: this is generated code, and a branch name or a
/// vendor's `rustc --version` banner may contain anything.
fn render(
    sha: &str,
    dirty: Option<bool>,
    epoch_secs: u64,
    rustc_version: &str,
    target: &str,
    profile: &str,
) -> String {
    let timestamp = iso8601_utc(epoch_secs);
    let mut s = String::new();
    writeln!(s, "// @generated by crates/vike-buildinfo/build.rs — do not edit.").unwrap();
    writeln!(
        s,
        "/// What an unresolved fact reports: no `git` on the box, no repository (a source"
    )
    .unwrap();
    writeln!(s, "/// tarball), or a `rustc --version` that would not run.").unwrap();
    writeln!(s, "pub const UNKNOWN: &str = {UNKNOWN:?};").unwrap();
    writeln!(s, "/// What a FROZEN debug build reports instead of a commit: CI's").unwrap();
    writeln!(s, "/// `VIKE_BUILDINFO_FREEZE=1`, which a release build ignores. Never a sha, never")
        .unwrap();
    writeln!(s, "/// [`UNKNOWN`] — `crates/vike-buildinfo/build.rs`'s module doc is the argument.")
        .unwrap();
    writeln!(s, "pub const FROZEN: &str = {FROZEN:?};").unwrap();
    writeln!(s, "/// The short git commit this binary was built from, [`UNKNOWN`], or [`FROZEN`].")
        .unwrap();
    writeln!(s, "pub const GIT_SHA: &str = {sha:?};").unwrap();
    writeln!(s, "/// Whether the working tree carried uncommitted changes; `None` when that could")
        .unwrap();
    writeln!(
        s,
        "/// not be established — a [`FROZEN`] build never asks. See [`git_state`] for the"
    )
    .unwrap();
    writeln!(s, "/// rendering.").unwrap();
    writeln!(s, "pub const GIT_DIRTY: Option<bool> = {dirty:?};").unwrap();
    writeln!(
        s,
        "/// When this binary was built, as seconds since the Unix epoch; `0` in a [`FROZEN`]"
    )
    .unwrap();
    writeln!(s, "/// build, so its bytes are the same on every runner.").unwrap();
    writeln!(s, "pub const BUILD_EPOCH_SECS: u64 = {epoch_secs};").unwrap();
    writeln!(s, "/// When this binary was built: RFC-3339 UTC, second resolution.").unwrap();
    writeln!(s, "pub const BUILD_TIMESTAMP: &str = {timestamp:?};").unwrap();
    writeln!(s, "/// `rustc --version` of the compiler that built this, or [`UNKNOWN`].").unwrap();
    writeln!(s, "pub const RUSTC_VERSION: &str = {rustc_version:?};").unwrap();
    writeln!(s, "/// The target triple this was compiled for, or [`UNKNOWN`].").unwrap();
    writeln!(s, "pub const TARGET: &str = {target:?};").unwrap();
    writeln!(s, "/// The cargo profile this was compiled under: `debug` or `release`.").unwrap();
    writeln!(s, "pub const PROFILE: &str = {profile:?};").unwrap();
    s
}

/// Run `git` from the package directory and return its trimmed stdout, or `None` for any failure —
/// git absent, not a repository, a non-zero status, non-UTF-8 output.
///
/// The working directory is named EXPLICITLY rather than inherited. A build script's cwd is the
/// package root today, and a probe whose answer depends on an unstated working directory is the
/// exact shape of the incident in the module doc.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// Emit one `cargo:rerun-if-changed=` line per watched path — the module doc's table says what each
/// one buys, and [`watch`] says why an absent path is skipped rather than emitted.
///
/// A `frozen` build stops after the package's own inputs and the toolchain pin: watching no git path
/// is what makes a commit leave it alone (the module doc's freeze section).
fn emit_rerun_paths(manifest_dir: &Path, repo_root: &Path, frozen: bool) {
    // The script itself and the two source files it `include!`s. Emitting ANY rerun line disables
    // cargo's default package-wide watch, so these have to be named or an edit to any of them is
    // invisible to the generated file — `src/freeze.rs` above all, since it decides which arm runs.
    watch(&manifest_dir.join("build.rs"));
    watch(&manifest_dir.join("src").join("timefmt.rs"));
    watch(&manifest_dir.join("src").join("freeze.rs"));
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
