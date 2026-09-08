//! `vike-buildinfo` — **what a shipped binary says about its own provenance.**
//!
//! ```text
//! vike-tradehub 0.1.0 (git 62ccdd8e clean, built 2026-08-09T09:41:07Z, rustc 1.96.0, x86_64-unknown-linux-gnu)
//! ```
//!
//! One [`version_line`] for `--version`, one [`summary`] for the first line of a daemon's log, and
//! the consts underneath both. Every value is resolved at COMPILE time by
//! `crates/vike-buildinfo/build.rs`, which shells out to `git` and reads cargo's own environment.
//!
//! # The incident
//!
//! A release binary was built on the the CI box test clone from that box's BARE repo — four commits
//! behind `main` — and was nearly installed on the live recorder. The only defence at the time was
//! runbook discipline: push `origin/main` to the bare repo first, and use
//! `git checkout -f -q -B <branch> origin/<branch>` because a plain checkout of an existing local
//! branch silently does NOT fast-forward. The root `CLAUDE.md` still carries both rules. They are
//! good rules and they are a human remembering a step; this crate makes the answer readable
//! instead: `vike-recorder --version` on the box states the commit it was built from, and the
//! daemon logs the same line at startup, so "is this the code I think it is?" stops being a
//! question about anyone's memory.
//!
//! # Zero dependencies, and why that is not thrift
//!
//! `vergen` and `built` do this job and drag `anyhow` / `cargo_metadata` / `time` / `git2` into the
//! BUILD graph of every binary that signs real orders — a graph this workspace audits
//! (`cargo deny --all-features`, `deny.toml`). Shelling out to `git` and formatting an integer
//! needs none of that. `crates/vike-secrets/Cargo.toml` makes the same argument one layer down.
//!
//! # Staleness — read `dirty` and `clean` differently
//!
//! `crates/vike-buildinfo/build.rs`'s module doc is the authority and carries the watched-path
//! table. The short version: [`GIT_SHA`] cannot go stale, because every operation that moves HEAD
//! touches a watched file; [`GIT_DIRTY`] can, because an unstaged edit touches nothing cargo can be
//! told to watch. So `dirty` is authoritative and `clean` means "no committed change since this
//! crate was last built".
//!
//! # Degrading
//!
//! No git, no repository (a source tarball), or a `rustc` that would not run: each unresolved fact
//! becomes [`UNKNOWN`] and the build SUCCEEDS. A crate whose whole job is to describe the build must
//! never be the reason there is no build. `git_state` reports the third answer — "could not tell" —
//! rather than rounding it to `clean`.

/// Seconds-since-the-epoch → RFC-3339 UTC. Also `include!`d by
/// `crates/vike-buildinfo/build.rs`, which is what makes the conversion testable — that file's own
/// header comment carries the argument, and explains why it opens with `//` rather than `//!`.
mod timefmt;

pub use timefmt::iso8601_utc;

// The compile-time facts: `UNKNOWN`, `GIT_SHA`, `GIT_DIRTY`, `BUILD_EPOCH_SECS`, `BUILD_TIMESTAMP`,
// `RUSTC_VERSION`, `TARGET`, `PROFILE`. Each carries its own doc comment, written by the generator.
include!(concat!(env!("OUT_DIR"), "/buildinfo.rs"));

/// The working tree's state as one word: `clean`, `dirty`, or `dirty?` when it could not be
/// established.
///
/// ⚠ The third word is not decoration. Rounding "could not tell" to `clean` would put the
/// reassuring answer on the box where the probe failed — a source tarball, a container with no
/// `git` — which is exactly the box whose provenance nobody can check by other means.
pub fn git_state() -> &'static str {
    state_word(GIT_DIRTY)
}

/// [`git_state`]'s mapping as a PURE function of the flag.
///
/// Split out only so all three arms can be exercised: [`GIT_DIRTY`] is a compile-time constant, so
/// a test of `git_state` alone can only ever see whichever arm this build happened to produce — and
/// the arm that matters is the one no checkout ever reaches.
fn state_word(dirty: Option<bool>) -> &'static str {
    match dirty {
        Some(false) => "clean",
        Some(true) => "dirty",
        None => "dirty?",
    }
}

/// The canonical identity line, WITHOUT a package name: what a daemon logs at startup.
///
/// ```text
/// git 62ccdd8e clean, built 2026-08-09T09:41:07Z, rustc 1.96.0 (a1b2c3d4e 2026-07-01), x86_64-unknown-linux-gnu
/// ```
///
/// Comma-separated and ASCII-only on purpose: this lands in a JSON log file, in a `systemctl
/// status` excerpt and in pasted terminal output, and every one of those is grepped.
pub fn summary() -> String {
    format!("git {GIT_SHA} {}, built {BUILD_TIMESTAMP}, {RUSTC_VERSION}, {TARGET}", git_state())
}

/// The `--version` line: the package name and version this workspace's binaries have always
/// printed, followed by [`summary`] in parentheses.
///
/// ```text
/// vike-cli 0.1.0 (git 62ccdd8e clean, built 2026-08-09T09:41:07Z, rustc 1.96.0, x86_64-unknown-linux-gnu)
/// ```
///
/// ⚠ **The first two tokens are unchanged, deliberately.** `<name> <version>` is the shape every
/// `--version` on the box prints (`git version 2.x`, `cargo 1.x`), a packaging script reads it
/// positionally, and the `version_prints_the_crate_version_on_stdout` test each binary crate ships
/// asserts the version string appears. Appending is compatible; re-ordering would not be.
///
/// `name`/`version` are parameters rather than this crate's own `CARGO_PKG_*`, which would report
/// `vike-buildinfo` in every binary that linked it.
pub fn version_line(name: &str, version: &str) -> String {
    format!("{name} {version} ({})", summary())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`BUILD_TIMESTAMP`] and [`BUILD_EPOCH_SECS`] are two spellings of ONE instant, and this is
    /// the only place that can prove it: the string is produced inside the build script, which no
    /// test ever runs. Re-deriving it here from the shipped integer, with the shipped formatter,
    /// closes the loop across that boundary — and is why `timefmt.rs` is compiled twice.
    #[test]
    fn the_two_timestamp_constants_are_one_instant() {
        assert_eq!(iso8601_utc(BUILD_EPOCH_SECS), BUILD_TIMESTAMP);
    }

    /// The build script ran and resolved a real repository. ⚠ It is an ASSERTION about CI and the
    /// dev box, not about a customer's tarball: both build from a git checkout, so an `unknown` SHA
    /// here means the probe broke — silently, and in the direction that produces an unidentifiable
    /// binary — rather than that git was legitimately absent.
    #[test]
    fn the_git_probe_resolved_in_a_checkout() {
        assert_ne!(GIT_SHA, UNKNOWN, "the build script could not read this checkout's HEAD");
        assert!(GIT_SHA.len() >= 7, "a short sha is at least 7 hex chars: {GIT_SHA:?}");
        assert!(GIT_SHA.chars().all(|c| c.is_ascii_hexdigit()), "{GIT_SHA:?}");
        assert!(GIT_DIRTY.is_some(), "`git status` did not answer in a checkout");
    }

    /// A binary is identifiable from ONE line: the commit, the tree state, when, and with what.
    /// Asserted on the substance rather than on the exact punctuation, so the line may be reworded
    /// without a test edit but cannot silently lose a field.
    #[test]
    fn the_summary_carries_every_fact_that_identifies_the_build() {
        let s = summary();
        for fact in [GIT_SHA, git_state(), BUILD_TIMESTAMP, RUSTC_VERSION, TARGET] {
            assert!(s.contains(fact), "summary() dropped {fact:?}: {s}");
        }
    }

    /// The compatibility half of [`version_line`]: name and version stay the first two tokens, in
    /// that order, because a packaging probe reads them positionally.
    #[test]
    fn the_version_line_still_starts_with_name_then_version() {
        let line = version_line("vike-cli", "0.1.0");
        assert!(line.starts_with("vike-cli 0.1.0 ("), "{line}");
        assert!(line.ends_with(')'), "{line}");
        assert!(line.contains(GIT_SHA), "{line}");
    }

    /// THREE outcomes, and the unknown one never reads as the reassuring one. The arm that matters
    /// (`None`) is unreachable from any checkout, which is why [`state_word`] exists to be called
    /// directly.
    #[test]
    fn an_unestablished_tree_state_never_reads_as_clean() {
        assert_eq!(state_word(Some(false)), "clean");
        assert_eq!(state_word(Some(true)), "dirty");
        assert_eq!(state_word(None), "dirty?");
        assert_ne!(state_word(None), state_word(Some(false)));
        assert_eq!(git_state(), state_word(GIT_DIRTY), "git_state is exactly this mapping");
    }
}
