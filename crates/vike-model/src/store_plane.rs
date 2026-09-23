//! Whose data a store kind holds — the MARKET's, or this ACCOUNT's.
//!
//! One question, asked from two crates that must not answer differently:
//!
//! * `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` is the LAYOUT authority — columns,
//!   partitioning, commit keys — and knows the kinds but declares no plane.
//! * `crates/vike-cli/src/cmd/data.rs` must REFUSE an account kind at `data rm`
//!   (`docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` verdict 3) and
//!   exclude it from `data hist` with a disclosure
//!   (`docs/superpowers/specs/2026-09-20-cli-data-surface-design.md` §9.3.2).
//!
//! ⚠ **The CLI cannot name `STORE_KINDS`, and that is structural rather than an oversight.**
//! `vike-cli` takes `vike-data` as a DEV-dependency only — `crates/vike-cli/src/cmd/data/
//! tape_health.rs`'s module doc calls it "the type wall" and it is why the read verbs flatten wire
//! types on arrival. Making it a normal dependency would pull the DataFusion tree into a CLI whose
//! manifest argues against exactly that edge by edge, and CI's `light-consumers` lane exists to
//! catch it.
//!
//! So the first answer was a COPY in `vike-cli` held equal by a text gate that parsed both files.
//! That gate worked and is deleted, because the root `CLAUDE.md` prescribes the real cure by name:
//!
//! > *when two sides must not disagree, the cure is a shared crate BELOW both, not a shared crate
//! > containing both.*
//!
//! `vike-model` is that crate. It is already a NORMAL dependency of both, it carries no DataFusion,
//! and it already hosts [`crate::venues::VENUES`] plus seven per-venue tables for the same reason —
//! that module's own doc calls a private per-table copy "the copy-drift disease one level up", and
//! this is the same disease one roster down.
//!
//! ⚠ **The direction of the check matters.** This crate sits BELOW `vike-data`, so it cannot see
//! `STORE_KINDS` to derive the list. [`ACCOUNT_KINDS`] is therefore DECLARED here and
//! `crates/vike-data/tests/store_kind_gate.rs` asserts the store agrees — every `STORE_KINDS` row
//! this list names must exist, and every row that looks like account data must be named here. A
//! kind that lands in the store and not here is one `data rm` would happily delete.
//!
//! ⚠ **What this deliberately does NOT do: decide whether a kind is COLLECTED, or by whom.** It
//! answers one question — whose activity the rows describe — and the two planes differ in what a
//! mistake costs. Deleting a market series costs a re-fetch; deleting an account series costs
//! history no venue will serve twice.

/// Whose activity a store kind's rows describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePlane {
    /// An instrument's own tape or a metric about the market: bars, quotes, trades, books, depth,
    /// instrument properties, funding RATES, cohort and perp metrics, option chains.
    Market,
    /// THIS account's own activity: what it executed, what it was charged, what it is worth.
    Account,
}

/// The store kinds that hold ACCOUNT activity.
///
/// ⚠ Three of the four share the `exec_` prefix and that is load-bearing rather than cosmetic —
/// `docs/decisions/0080` renamed `funding` to `exec_funding` precisely so a reader can SEE which
/// side of the boundary a kind is on. `equity` predates the prefix and keeps its name because
/// nothing else is called that.
///
/// ⚠ **`funding` is NOT here, and the omission is the interesting one.** The bare word means the
/// MARKET's published funding RATE, which lives in the `bar` kind under `interval=funding` and is
/// ordinary market data. Before 0079 the bare word sat on the account side and
/// `store_kind.rs` carried a pinned NAME COLLISION note saying neither name told a reader which
/// was which.
pub const ACCOUNT_KINDS: [&str; 4] = ["equity", "exec_fill", "exec_funding", "exec_order"];

/// Which plane a store kind belongs to.
///
/// An UNKNOWN kind answers [`StorePlane::Market`], deliberately and in the safe direction: this is
/// consulted to decide whether to REFUSE a destructive or scoping operation, and the account plane
/// is the closed set. A kind nobody has classified is therefore treated as ordinary market data
/// rather than silently gaining a refusal nobody wrote — and the completeness assertion in
/// `crates/vike-data/tests/store_kind_gate.rs` is what stops a genuinely-new account kind from
/// riding that fallback unnoticed.
pub fn plane_of(kind: &str) -> StorePlane {
    if ACCOUNT_KINDS.contains(&kind) { StorePlane::Account } else { StorePlane::Market }
}

/// `true` when a kind holds this account's own activity rather than the market's.
pub fn is_account_kind(kind: &str) -> bool {
    plane_of(kind) == StorePlane::Account
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_account_kinds_classify_as_account() {
        for kind in ACCOUNT_KINDS {
            assert_eq!(plane_of(kind), StorePlane::Account, "{kind}");
            assert!(is_account_kind(kind), "{kind}");
        }
    }

    #[test]
    fn market_kinds_and_unknown_kinds_classify_as_market() {
        for kind in [
            "bar",
            "quote",
            "trade",
            "book",
            "depth",
            "properties",
            "chain",
            "cohort",
            "perp_metrics",
            "a_kind_nobody_has_declared",
        ] {
            assert_eq!(plane_of(kind), StorePlane::Market, "{kind}");
            assert!(!is_account_kind(kind), "{kind}");
        }
    }

    /// The collision `docs/decisions/0080` resolved, asserted so the resolution cannot quietly
    /// reverse: the MARKET funding rate is not an account kind, and the ACCOUNT's realized payments
    /// are — and they no longer share a word.
    #[test]
    fn the_market_funding_rate_is_not_an_account_kind() {
        assert!(
            !is_account_kind("funding"),
            "`funding` is the MARKET rate: kind=bar, \
                                              interval=funding. It must never be refused as \
                                              account data"
        );
        assert!(is_account_kind("exec_funding"));
    }

    /// The list is SORTED, so a diff against the store's own rows reads as a set comparison rather
    /// than an ordering one.
    #[test]
    fn the_roster_is_sorted_and_has_no_duplicates() {
        let mut sorted = ACCOUNT_KINDS.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.as_slice(), &ACCOUNT_KINDS[..]);
    }
}
