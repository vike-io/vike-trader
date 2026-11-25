//! `FillIdentityRow`: whether a venue names ONE fill by the SAME trade id on its private stream and in the fill
//! history its reconcile client reads, so a fill booked from the history and delivered later by the stream (or a
//! replay) is one trade id the fill ledger refuses the second time.
//!
//! **Its one reader.** The reconcile pass books our own order's fill that it confirmed missing on two passes in a
//! row while the node was UP only on a venue whose row says [`FillIdentity::Same`] (`vike_core`'s
//! `book_own_fills_filled_while_down`, decisions 0043 and 0120 as amended 2026-11-21). Where the two ids differ, the
//! stream's later delivery would be booked a second time; so would a row stamped when it was READ rather than by
//! the venue, which says nothing about when the fill happened.
//!
//! **Step 1 of the playbook** (`crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the playbook)"): every
//! roster venue is a NAMED row citing the stream parser and the history parser it was read from. `Same` today is
//! the five venues the connection-sync plan names first (bybit, binance, okx, deribit, hyperliquid); every other
//! row says why it waits. The connection harness's S10 scenario (the stream parser and the history parser give
//! one trade id, run on the venue's real decoders) is what moves a row later; until it exists, the cited parsers
//! and their own pins are the proof. An unknown venue string is `Unproven` (fail closed).

/// Whether the stream and the history name one fill alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillIdentity {
    /// One trade id, venue-stamped, on the stream and in the history.
    Same,
    /// Not shown, or shown to differ: the reason on the row says which.
    Unproven,
}

/// One venue's declaration: the answer and where it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillIdentityRow {
    pub identity: FillIdentity,
    /// Why, citing the adapter code (a `path`'s `symbol`, never a line number).
    pub reason: &'static str,
}

const fn same(reason: &'static str) -> FillIdentityRow {
    FillIdentityRow { identity: FillIdentity::Same, reason }
}

const fn unproven(reason: &'static str) -> FillIdentityRow {
    FillIdentityRow { identity: FillIdentity::Unproven, reason }
}

/// Not compared yet: the row waits for the harness's S10 scenario.
const NOT_COMPARED: &str = "not compared yet: waits for the connection harness's S10 row";

/// The row of `venue`.
#[must_use]
pub fn fill_identity_for(venue: &str) -> FillIdentityRow {
    match venue {
        "binance" => same(concat!(
            "the stream's `t` and userTrades' `id`, both through ",
            "crates/bridges/binance/src/family/mod.rs's fill_trade_id ",
            "(crates/vike-bridge-core/tests/perp_fill_label.rs's ",
            "a_reconcile_pass_after_a_streamed_fill_raises_nothing)"
        )),
        "bybit" => same(concat!(
            "`execId` on the stream (crates/bridges/bybit/src/execution/event_mapper.rs's map_execution) ",
            "and in execution/list (crates/bridges/bybit/src/reconcile/mod.rs's parse_fills)"
        )),
        "okx" => same(concat!(
            "`tradeId` on the orders channel and in the fills history, a foreign instrument's scoped ",
            "alike by crates/bridges/okx/src/execution/event_mapper.rs's scoped_trade_id"
        )),
        "deribit" => same(concat!(
            "`trade_id` on user.trades (crates/bridges/deribit/src/execution/event_mapper.rs's ",
            "map_deribit_trade) and in get_user_trades (crates/bridges/deribit/src/reconcile/mod.rs's ",
            "parse_user_trades)"
        )),
        "hyperliquid" => same(concat!(
            "`tid` on userFills (crates/bridges/hyperliquid/src/execution/event_mapper.rs's map_one_fill) ",
            "and in the userFills history (crates/bridges/hyperliquid/src/reconcile/mod.rs's parse_fills)"
        )),
        "aster" => unproven(concat!(
            "shares the binance family's parsers, but its passes are authenticated MAINNET reads ",
            "(crates/bridges/aster/CLAUDE.md), so no demo proves the booking"
        )),
        "ibkr" => unproven(concat!(
            "the socket client stamps a fill row with the time it READ it ",
            "(crates/bridges/vike-ibkr/src/reconcile/socket.rs's parse_socket_fill_reports), not the venue's"
        )),
        "oanda" | "ig" | "fxcm" | "dukascopy" | "polymarket" | "ctrader" | "alpaca" => {
            unproven(NOT_COMPARED)
        }
        // vike:new-venue:row "{venue}" => unproven(NOT_COMPARED), // TODO(new-venue: {venue}): a NAMED row; Same only with the S10 proof
        _ => unproven("not a roster venue"),
    }
}

/// Does `venue` name one fill by one trade id on its stream and in its history? `false` for an unknown string.
#[must_use]
pub fn stream_and_history_share_the_trade_id(venue: &str) -> bool {
    fill_identity_for(venue).identity == FillIdentity::Same
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::VENUES;

    /// The whole table, verbatim.
    #[test]
    fn the_fill_identity_matrix_is_pinned_verbatim() {
        use FillIdentity::{Same, Unproven};
        #[rustfmt::skip]
        const MATRIX: &[(&str, FillIdentity)] = &[
            ("binance",     Same),
            ("bybit",       Same),
            ("okx",         Same),
            ("deribit",     Same),
            ("oanda",       Unproven),
            ("ig",          Unproven),
            ("fxcm",        Unproven),
            ("dukascopy",   Unproven),
            ("polymarket",  Unproven),
            ("ibkr",        Unproven),
            ("ctrader",     Unproven),
            ("alpaca",      Unproven),
            ("aster",       Unproven),
            ("hyperliquid", Same),
            // vike:new-venue:row ("{venue}", Unproven), // TODO(new-venue: {venue}): move with the row above
        ];
        assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
        for &(v, identity) in MATRIX {
            assert!(VENUES.contains(&v), "{v} is not a roster venue");
            assert_eq!(fill_identity_for(v).identity, identity, "{v}");
            assert!(!fill_identity_for(v).reason.is_empty(), "{v}: a row without a reason");
            assert_eq!(stream_and_history_share_the_trade_id(v), identity == Same, "{v}");
        }
        assert!(!stream_and_history_share_the_trade_id("nope"), "an unknown venue fails CLOSED");
    }
}
