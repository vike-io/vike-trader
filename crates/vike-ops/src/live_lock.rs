//! `live_lock` — ONE live process per venue account (split-plane spec, B11; the Danger-2
//! tripwire).
//!
//! Two processes live-trading one account each keep their own books; each then sees the other's
//! activity as `PositionDrift` — the one no-local-origin divergence kind the `hybrid` reconcile
//! policy AUTO-APPLIES — and they rewrite each other's positions and book realized PnL with no
//! operator in front of it. The SEPARATION mechanism is the spec's Pattern A (strategies that
//! share an account share a PROCESS — multi-mount); this lock guards against the ACCIDENT: same
//! settings dir, a live process started twice (a stale unit, a second terminal, a forgotten
//! worktree, a fat GUI beside a live daemon).
//!
//! # Scope, and the one residual
//!
//! Keyed per VENUE within one `state_dir`. One settings dir resolves one credential store, so
//! "same dir + same venue" IS "same account" by construction — the whole accident class.
//!
//! ⚠ **The unit protected is a venue ACCOUNT, not a process and not a MOUNT** — the type name, the
//! refusal message and the `PositionDrift` argument above all say so, and the consequence is that
//! **a PAPER mount claims nothing.** A paper mount holds no credentials, sends no order to a venue
//! and keeps its books locally, so two of them cannot see each other as drift; there is no account
//! to be one process of. Locking one would be actively harmful in both directions — the claim
//! would refuse a genuinely LIVE process on that venue, and the refusal message it printed
//! ("another live process already trades the X account") would be false. That is why the claim set
//! is `vike_run::armed_live_venues` rather than a mount list: a profile naming a venue used to take
//! its lock regardless of credentials, and that is a behaviour this fix deliberately removes. The
//! neighbouring hazard — two daemons double-mounting one STRATEGY, which is a duplicate-order
//! problem even on paper — has its own mechanism in `crates/vike-core/src/journal_lock.rs` and is
//! not this file's job.
//!
//! RESIDUAL
//! (documented, deliberately out of scope): two processes with DIFFERENT settings dirs holding
//! COPIES of the same credentials are invisible to this lock; that configuration is an operator's
//! deliberate act, and the spec's answer to it is sub-accounts, not a global registry.
//!
//! # Mechanism
//!
//! The `crates/vike-core/src/journal_lock.rs` idiom — an OS advisory lock (`flock` /
//! `LockFileEx` via std `File::try_lock`) on a sentinel file whose VALUE is meaningless and whose
//! LIFETIME is the lock. A killed process's lock is released by the OS, so there is no stale-lock
//! sweep and no PID file to go wrong. Refusal is a hard [`io::ErrorKind::AddrInUse`] error whose
//! message is written for the operator who must choose which process trades — per the spec's
//! Phase-1 decision: REFUSE, never warn (a warning beside a live double-mount is an incident
//! report nobody has read yet).
//!
//! Why THIS crate: the composition roots that mount live engines (vike-tradehub, vike-app) both
//! already stand on vike-ops' LIGHT half for their stop path; the lock names no vike type and
//! adds no edge.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// Sentinel filename prefix: `<state_dir>/LIVE-<venue>.lock`.
pub const LIVE_LOCK_PREFIX: &str = "LIVE-";

/// An acquired exclusive live-mount claim on one venue account. Hold it for as long as the live
/// engine exists; dropping releases (see the module doc — the OS releases it on death too).
///
/// `Debug` is derived, mirroring `JournalLock`: `File`'s own `Debug` prints only the handle and
/// path, nothing sensitive, and the derive is what lets a caller `expect_err` on a refusal.
#[derive(Debug)]
pub struct LiveLock {
    /// The locked sentinel handle. Never read after construction — its VALUE is irrelevant, its
    /// LIFETIME is the lock. (The derived `Debug` reads it, so this is not a dead field.)
    _file: File,
}

impl LiveLock {
    /// Claim `<state_dir>/LIVE-<venue>.lock` exclusively, creating `state_dir` if absent.
    ///
    /// ⚠ **The parameter is an ACCOUNT identity, not a capability key** — the type doc above and
    /// this function's own refusal message both say "account", and `vike_exec`'s engine now spells
    /// that distinction as two fields: `ExecutionEngine::route_key` (unique per venue-ACCOUNT) and
    /// `ExecutionEngine::venue` (the canonical exchange id every capability table is keyed on).
    /// **A caller that ever has both must pass the ROUTE KEY here.** Keying on the canonical venue
    /// would make one process mounting two accounts of one exchange refuse its own second mount —
    /// two `open()`s of one path conflict within a process, so `LIVE-binance.lock` claimed for
    /// account A blocks account B — turning a per-account lock into a per-exchange one and
    /// forbidding by accident the exact configuration the split exists to allow. On the route key
    /// the two accounts take `LIVE-binance.lock` and `LIVE-binance-sub2.lock`, and each still
    /// excludes a second PROCESS on the SAME account, which is the property this guards.
    ///
    /// Nothing passes anything but a canonical venue today: both callers take their string from
    /// `vike_run::armed_live_venues` — the PRE-MOUNT probe over `vike_run::WIRED_MARKETS`, whose
    /// rows are canonical venue ids — never from an engine field, so the two answers coincide and
    /// this note is for whoever wires the second account. Note also that the argument becomes a
    /// FILENAME component below, so a route key reaching here must be path-safe.
    ///
    /// ⚠ **Callers claim BEFORE they construct.** The probe is pure (no socket, no signature, no
    /// client), which is what makes that possible, and it is not tidiness: three venue arms post
    /// `set_leverage` while constructing their exec client (`crates/bridges/bybit/src/exec.rs`,
    /// `crates/bridges/okx/src/exec.rs`, `crates/bridges/aster/src/exec.rs`), so a refusal raised
    /// after construction has already changed account state at the very accounts it is refusing to
    /// double-trade. `crates/vike-ops/tests/live_lock_claim_order_gate.rs` holds both roots to it.
    ///
    /// Errors:
    /// - [`io::ErrorKind::AddrInUse`] — another live process holds this venue's account. This is
    ///   the accidental-double-launch case; the message names the venue and both remedies.
    /// - anything else — the sentinel could not be created/opened, or the platform refused the
    ///   lock; surfaced verbatim rather than degraded into "no lock".
    pub fn acquire(state_dir: &Path, venue: &str) -> io::Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let path = state_dir.join(format!("{LIVE_LOCK_PREFIX}{venue}.lock"));
        // truncate(false): never rewrite a file another process currently holds. read+write (not
        // append) — Windows refuses to lock an append-opened handle. (The journal_lock idiom.)
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
        // Bind the attempt BEFORE the match so the `&file` autoref is unambiguously dead by the
        // time the success arm MOVES `file` into the guard.
        let attempt = file.try_lock();
        match attempt {
            Ok(()) => Ok(LiveLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "another live process already trades the {venue} account (lock file {}). One \
                     account gets ONE live process: two sets of books on one account see each \
                     other as PositionDrift and silently rewrite each other's positions. Stop \
                     the other process first, or give this one its own sub-account credentials \
                     (its own settings dir).",
                    path.display()
                ),
            )),
            Err(other) => Err(io::Error::other(format!(
                "could not acquire the live-account lock at {}: {other:?}",
                path.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_acquire_wins_and_the_second_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let held = LiveLock::acquire(dir.path(), "binance").expect("first acquire");
        let second = LiveLock::acquire(dir.path(), "binance");
        let err = second.expect_err("same venue, same dir, still held");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(err.to_string().contains("binance"), "the message names the venue: {err}");
        drop(held);
    }

    #[test]
    fn a_dropped_lock_is_reacquirable_and_venues_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let held = LiveLock::acquire(dir.path(), "binance").unwrap();
        // A DIFFERENT venue in the same dir is a different account — independent lock.
        let _other = LiveLock::acquire(dir.path(), "bybit").expect("different venue");
        drop(held);
        let _again = LiveLock::acquire(dir.path(), "binance").expect("released on drop");
    }

    #[test]
    fn acquire_creates_the_state_dir_if_absent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("state");
        let _l = LiveLock::acquire(&nested, "okx").expect("creates the dir");
        assert!(nested.join(format!("{LIVE_LOCK_PREFIX}okx.lock")).exists());
    }
}
