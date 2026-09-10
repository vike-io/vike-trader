//! **The stdio summary line's MOUNTED-SET scoping** — the pure half of `main.rs`'s
//! `summary_line`, kept here because it must be callable from an integration test over a REAL
//! multi-mount core (`tests/daemon/multi_mount_profile.rs`); a bin crate's private function is
//! reachable from nothing.
//!
//! # The observation this exists for (I10 rehearsal, `docs/ops/i10-rehearsal-2026-08-19.md`)
//!
//! A two-venue live rehearsal's summary line printed `equity_book: 100000.0` while the operator
//! had seeded exactly two mounts at 10k each. Nothing was wrong with the number: `build_node`
//! seeds EVERY default-build venue engine with the primary mount's `seed_cash`, so ten paper
//! engines exist and `Portfolio::equity_book_total` sums all ten `Delta` blocks — the daemon's
//! whole book, honestly reported. What was wrong was the READER's inference, and the rehearsal
//! note says so: "an operator eyeballing the summary should know the number is not 'the two
//! mounts' seeds'".
//!
//! # Why a NEW field rather than a narrowed `equity_book`
//!
//! `equity_book` is one half of a documented PARTITION: `equity_book + equity_wallet` covers
//! exactly the sum `Portfolio::equity_total` takes, which is what makes the "a wallet and a book
//! are not addable" split in `summary_line`'s own doc checkable rather than merely asserted.
//! Scoping that field to the mounted subset would silently break the partition, silently change a
//! number an existing `jq`/alerting consumer already reads, and leave the un-mounted seed capital
//! reported by NOTHING.
//!
//! So this follows the precedent that fixed the ORIGINAL conflation — report both, conflate
//! neither, name the provenance in the key. `equity_wallet` gained `wallet_venues`; the daemon's
//! book half now gains `equity_book_mounted` + `mounted_venues`. Every existing key keeps its
//! meaning and its value, and the operator gets the figure they were reaching for, labelled.
//!
//! ⚠ The wire snapshot (`crate::publish`'s `WireSnapshot`) is deliberately untouched: it carries
//! the per-venue blocks AND the mount rows already, so any wire consumer can compute this scoping
//! itself. This is the stdio line's problem, and it is fixed on the stdio line.

use vike_core::snapshot::{CoreSnapshot, MountRowKind};

/// The mounted-set book equity plus the venues it was scoped to.
#[derive(Debug, Clone, PartialEq)]
pub struct MountedBookEquity {
    /// `Σ` the `BalanceMode::Delta` venue blocks whose venue is MOUNTED, py_sum in venue
    /// REGISTRATION order — the same fold law and the same order `Portfolio::equity_book_total`
    /// uses, so the scoped figure and the unscoped one can be compared without an ULP argument.
    pub total: f64,
    /// The distinct venues of the snapshot's `MountRowKind::Mount` rows, in MOUNT order — "what
    /// this daemon actually runs", which is the question the operator was asking. The trailing
    /// `MountRowKind::Residual` row (whose `venue` is the empty string by construction) is
    /// excluded: it is a ledger row, not a mount.
    pub venues: Vec<String>,
}

/// Scope the book-kept equity to the MOUNTED venues (module doc for why this is a new figure and
/// not a narrowed one).
///
/// Two edge shapes, both deliberate and both tested:
///
/// - **No mount rows** (a core that has mounted nothing, or a hand-built `CoreSnapshot::empty`):
///   the mounted set is EMPTY, so the total is `0.0` and `venues` is empty. A reader can tell the
///   difference between "scoped to nothing" and "nothing to scope" from the empty name list —
///   which is exactly why the names ship beside the number.
/// - **A mounted venue that has flipped `Authoritative`** (reconcile adopted its wallet):
///   it is NAMED in `venues` but contributes NOTHING to `total`, because it is no longer part of
///   the book half at all — its equity is in `equity_wallet`, under `wallet_venues`. That is the
///   same partition rule `equity_book` obeys, merely scoped; a mounted-venue figure that quietly
///   pulled an adopted wallet back into a "book" number would re-commit the exact conflation the
///   split exists to prevent.
pub fn mounted_book_equity(snap: &CoreSnapshot) -> MountedBookEquity {
    let mut venues: Vec<String> = Vec::new();
    for m in snap.mounts.iter().filter(|m| m.kind == MountRowKind::Mount) {
        if !venues.iter().any(|v| v == &m.venue) {
            venues.push(m.venue.clone());
        }
    }
    let total = vike_model::py_sum(
        snap.portfolio
            .venues
            .iter()
            .filter(|v| v.balance_mode == vike_exec::BalanceMode::Delta)
            .filter(|v| venues.iter().any(|m| m == &v.venue))
            .map(|v| v.equity),
    );
    MountedBookEquity { total, venues }
}
