//! `halt_verify_support`: whether `vike_model::HaltAdmit::Verify` can verify anything at a venue, the per-venue
//! table under the HALT sentinel's admit policy, and the reason an operator is told at mount when it cannot.
//!
//! It came down from `vike-model` (2026-11-22, B3): a per-venue table in the vocabulary crate was against decision
//! 0162, so the model keeps only the vocabulary. [`vike_model::HaltVerify`] is what a row answers, and the mode and
//! its mount report (`HaltAdmit`, `effective_halt_admit`, which takes this row's answer as a parameter, and
//! `halt_admit_arming`) are not per-venue facts.
//!
//! ⚠ **Only ONE venue answers [`HaltVerify::Real`] today, and that is the honest answer, not a stub.** Exactly two
//! bridge adapters hold a position book of their own, and only one of them holds it at a halt boundary:
//!
//! * **cTrader** does: `crates/bridges/ctrader/src/transport/connection/mod.rs`'s `PositionsByIdShared`, seeded at
//!   connect and maintained from every execution event, read by `crates/bridges/ctrader/src/execution/positions.rs`'s
//!   `opposing_available`.
//! * **Polymarket** does NOT, on the exec path: its only position source is a blocking geo-proxied HTTP fetch on the
//!   settlement side (`crates/bridges/polymarket/src/reconcile/mod.rs`'s `fetch_position_status_reports`).
//!
//! ⚠ **This answers a question about the ADAPTER, not about a MOUNT**: a venue whose live client is absent mounts the
//! paper client, which holds no book either, and only the arm knows that (`vike_model::halt_admit_arming`).

use vike_model::HaltVerify;

/// The reason shared by every venue whose exec client is the venue-neutral command actor: that boundary owns a
/// channel, an event lane and a sentinel path, and nothing that could say how big any position is
/// (`crates/vike-bridge-core/src/exec_actor.rs`'s `halt_engaged`).
const NO_BOOK_ON_THE_SHARED_ACTOR: &str = "its exec client is the shared ExecutionClientActor, whose submit boundary holds no position book";

/// Can `vike_model::HaltAdmit::Verify` verify anything at `venue`? One NAMED row per roster venue, even where the
/// answer equals the fallback. Each reason is the exact sentence an operator sees when they set `verify` and the
/// venue cannot honour it.
#[must_use]
pub fn halt_verify_support(venue: &str) -> HaltVerify {
    match venue {
        // ── the one venue that can ──
        "ctrader" => HaltVerify::Real,
        // ── the shared-ExecutionClientActor family ──
        "binance" | "bybit" | "okx" | "deribit" | "oanda" | "ig" | "alpaca" | "aster" | "ibkr"
        | "hyperliquid" => HaltVerify::Degrades(NO_BOOK_ON_THE_SHARED_ACTOR),
        "fxcm" => HaltVerify::Degrades(NO_BOOK_ON_THE_SHARED_ACTOR),
        "polymarket" => HaltVerify::Degrades(
            "its exec path holds no position book at all — the venue's only position source is a \
             blocking proxied HTTP fetch on the settlement side, which also misses redemptions",
        ),
        // ── a bespoke halt boundary, no book ──
        "dukascopy" => HaltVerify::Degrades(
            "its bespoke halt boundary (ExecutionClientDukascopy::submit) holds no position \
             book; its position state lives in the Java sidecar",
        ),
        // A venue string this table has never heard of (the last arm): no verification is claimed.
        // vike:new-venue:row "{venue}" => HaltVerify::Degrades(NO_BOOK_ON_THE_SHARED_ACTOR), // TODO(new-venue: {venue}): does its halt boundary hold a position book? A bespoke boundary writes its own reason
        _ => HaltVerify::Degrades("unknown venue — no position book is claimed for it"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::VENUES;

    /// The whole table, verbatim: the answer of every roster venue, the reason pinned by which sentence it is
    /// (`Shared`: [`NO_BOOK_ON_THE_SHARED_ACTOR`]), word for word the sentence an operator reads at mount.
    #[test]
    fn the_halt_verify_matrix_is_pinned_verbatim() {
        const SHARED: Option<&str> = Some(NO_BOOK_ON_THE_SHARED_ACTOR);
        const POLYMARKET: Option<&str> = Some(
            "its exec path holds no position book at all — the venue's only position source is a blocking proxied \
             HTTP fetch on the settlement side, which also misses redemptions",
        );
        const DUKASCOPY: Option<&str> = Some(
            "its bespoke halt boundary (ExecutionClientDukascopy::submit) holds no position book; its position \
             state lives in the Java sidecar",
        );
        #[rustfmt::skip]
        const MATRIX: &[(&str, Option<&str>)] = &[
            ("binance",     SHARED),
            ("bybit",       SHARED),
            ("okx",         SHARED),
            ("deribit",     SHARED),
            ("oanda",       SHARED),
            ("ig",          SHARED),
            ("fxcm",        SHARED),
            ("dukascopy",   DUKASCOPY),
            ("polymarket",  POLYMARKET),
            ("ibkr",        SHARED),
            ("ctrader",     None),
            ("alpaca",      SHARED),
            ("aster",       SHARED),
            ("hyperliquid", SHARED),
            // vike:new-venue:row ("{venue}", SHARED), // TODO(new-venue: {venue}): move with the row above
        ];
        assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
        let fallback = halt_verify_support("no-such-venue-anywhere");
        for &(venue, degrades) in MATRIX {
            assert!(VENUES.contains(&venue), "{venue} is not a roster venue");
            let row = halt_verify_support(venue);
            let want = degrades.map_or(HaltVerify::Real, HaltVerify::Degrades);
            assert_eq!(row, want, "{venue}");
            assert_ne!(row, fallback, "{venue} rides the unknown-venue fallback");
        }
        assert_eq!(
            NO_BOOK_ON_THE_SHARED_ACTOR,
            "its exec client is the shared ExecutionClientActor, whose submit boundary holds no position book"
        );
    }

    /// A degrade must SAY something an operator can act on.
    #[test]
    fn every_degrade_reason_is_an_argument() {
        for venue in VENUES {
            if let HaltVerify::Degrades(why) = halt_verify_support(venue) {
                assert!(
                    why.len() > 30 && why.contains(' '),
                    "{venue}'s degrade reason is not something an operator can act on: {why:?}"
                );
            }
        }
    }
}
