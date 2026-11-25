//! `reconstruct_trades` — fold a chronological `FillEvent` stream into closed [`Trade`]
//! round-trips through the shared cost-basis primitive [`vike_model::compute_fill`].
//!
//! This is `vike_execution::Account::fold` (`vike-execution/src/account.rs`) with FULL trade bookkeeping
//! kept instead of discarded: `Account::fold` runs each fill through `compute_fill` and pushes
//! only `realized_pnl` into `closed_pnls`; here we additionally build a `Trade` on every
//! reduce/close/flip, apportioning entry/exit fees EXACTLY as `vike_backtest`'s
//! `OrderBrokerSimulator::apply_fill` does (`crates/vike-simulator/src/engine/sim_broker/fills.rs`, the reduce/close/flip
//! branch). Routing ALL cost-basis math through `compute_fill` is what makes a live tearsheet and
//! a backtest tearsheet over the same fills bit-for-bit identical.
//!
//! Keying mirrors `Account`: positions are keyed by `(venue, symbol, position_side)`.
//!
//! PURE: no I/O. `mae`/`mfe` are left `0.0` — a bare fill stream carries no intrabar path, so
//! excursions are genuinely uncomputable (the vector engine leaves them `0.0` too; this is
//! correct, not a shortcut).
//!
//! Moved here out of `vike-report` on 2026-09-28 ([`crate::results_table`] carries the argument).
//! ⚠ The PARITY ANCHOR that folds the same fills through `vike_execution::Account` did NOT come with
//! it: this crate names nothing above `vike_model`, and a dev-dependency on the execution plane for
//! one assertion would be the first `vike_execution` edge this crate ever carried. So that test lives
//! where both halves are already linked — `crates/vike-journal/tests/account_parity.rs`'s
//! `realized_pnl_matches_account_fold`.

use std::collections::HashMap;

use vike_model::events::{FillEvent, PositionSide};
use vike_model::{AccountVenuePosition, Trade, TradeFold, TradeFoldClose};

/// What this fold values one unit of a contract at: linear, its price, PnL `(exit - entry) * qty`.
/// A journal holds other contracts (a coin-margined perpetual, a multiplier of 50) and `Account::fold`
/// looks each one up, so a door leaves those fills out before they reach this fold and labels them
/// ([`crate::results_table::without_non_default_contracts`], decision 0243 item 6). The parity anchor test
/// builds an `Account` at the same default 1.0, so the two folds agree on what is left.
///
/// `pub` because it is the ONE spelling of that assumption: the mark-to-market store builder
/// (`crates/vike-client/src/account_report/history.rs`'s `mtm_curve_from_store`) threads this same constant
/// into [`crate::reconstruct_mtm`], so the trades fold and the MTM curve cannot disagree about it.
pub const MULTIPLIER: f64 = 1.0;

/// Reconstruct closed [`Trade`] round-trips from a chronological `FillEvent` stream.
///
/// `fills` MUST be in journal (chronological) order — the same order `Account::fold` sees them.
/// One `Trade` is emitted per reduce/close/flip (a partial reduce closes part of a position and
/// emits one trade for the closed portion; a flip emits one trade for the fully-closed prior side
/// and opens the remainder on the opposite side). Open/Add emit no trade.
pub fn reconstruct_trades(fills: &[FillEvent]) -> Vec<Trade> {
    reconstruct_trades_from(fills, i64::MIN).1
}

/// **The tradehub's position snapshot a window opens from**: the latest at or before its start of the account's
/// boot statements (`vike_journal::JournalRecord::PositionSnapshot`, as the datahub stores it). A reader folds it,
/// as [`Self::opening_fills`], ahead of the account's fills journaled AFTER it and of no other, so a fill recorded
/// before the execution stream (which carries no account) is never needed.
#[derive(Debug, Clone, PartialEq)]
pub struct TradesOpeningSnapshot {
    /// When the tradehub took it.
    pub as_of_ms: i64,
    /// The account's open positions then (each `(venue, symbol, position_side)` once), or why the tradehub could not
    /// know them.
    pub positions: Result<Vec<AccountVenuePosition>, String>,
}

/// **An account's stored position snapshot as a window opens from it**: the one reading of the datahub's row (its
/// `as_of_ms`, its `venue`, its `positions` and, when the tradehub could not know them, its `unknown` words) that
/// `vike-cli report account` and the account window share. With `unknown` the positions are that refusal, else
/// each one on `venue`.
pub fn opening_snapshot(
    as_of_ms: i64,
    venue: &str,
    positions: &[vike_model::AccountPosition],
    unknown: Option<&str>,
) -> TradesOpeningSnapshot {
    let positions = match unknown {
        Some(why) => Err(why.to_string()),
        None => Ok(positions.iter().cloned().map(|p| p.on_venue(venue.to_string())).collect()),
    };
    TradesOpeningSnapshot { as_of_ms, positions }
}

impl TradesOpeningSnapshot {
    /// **The snapshot's positions as the fills that open them**, one per open position, at `at_ms`, at its average
    /// price and with no fee: folded first, they leave each position at exactly the snapshot's size and cost basis,
    /// in [`reconstruct_trades_from`] and in a mark-to-market reconstruction alike. `at_ms` is the snapshot's instant,
    /// or earlier when a fill folded after it carries an earlier venue time (one the venue made while the tradehub
    /// was down), so a fold in time order still opens with them. The fees paid before the snapshot are not in them:
    /// a trade one of them opens carries only the fees paid after it. Empty when the positions are unknown.
    pub fn opening_fills(&self, at_ms: i64) -> Vec<FillEvent> {
        let Ok(held) = &self.positions else { return Vec::new() };
        held.iter()
            .filter(|p| p.size != 0.0)
            .map(|p| FillEvent {
                trade_id: vike_model::events::TradeId::prefixed(
                    "position-snapshot-",
                    format!("{}-{}-{}", p.symbol, p.position_side, self.as_of_ms),
                ),
                order_client_id: String::new(),
                venue: p.venue.as_str().into(),
                symbol: p.symbol.as_str().into(),
                side: if p.size > 0.0 { 1 } else { -1 },
                last_qty: p.size.abs(),
                last_price: p.average_price,
                commission: 0.0,
                commission_asset: "".into(),
                liquidity_side: vike_model::events::LiquiditySide::Unknown,
                timestamp: at_ms,
                mark_price: None,
                position_side: p.position_side,
            })
            .collect()
    }
}

/// [`reconstruct_trades`] measured over a WINDOW that starts at `from_ms`: every fill is folded, so a position
/// opened before the window keeps its cost basis, but only the trades CLOSED at or after `from_ms` are returned,
/// beside the positions open when the window starts (each `(venue, symbol, position_side)` once, sorted). A window
/// folded from flat would read an exit of an older long as a new short and book its P&L against nothing.
/// `from_ms = i64::MIN` is [`reconstruct_trades`] with an empty opening.
pub fn reconstruct_trades_from(
    fills: &[FillEvent],
    from_ms: i64,
) -> (Vec<AccountVenuePosition>, Vec<Trade>) {
    let mut states: HashMap<(String, String, PositionSide), TradeFold> = HashMap::new();
    let mut trades = Vec::new();
    let mut opening: Option<Vec<AccountVenuePosition>> = None;

    for f in fills {
        if opening.is_none() && f.timestamp >= from_ms {
            opening = Some(open_positions(&states));
        }
        let key = (f.venue.to_string(), f.symbol.to_string(), f.position_side);
        let st = states.entry(key).or_default();

        // `commission` is SIGNED (>0 cost / <0 rebate) — carried through verbatim by the shared
        // fold so `Trade.fees` inherits that sign, EXACTLY as `OrderBrokerSimulator::apply_fill` does.
        let step =
            st.apply(f.side, f.last_qty, f.last_price, f.commission, f.timestamp, MULTIPLIER);
        if let Some(c) = step.closed.filter(|_| f.timestamp >= from_ms) {
            trades.push(trade_from_closed(c, f.symbol.to_string()));
        }
    }

    (opening.unwrap_or_else(|| open_positions(&states)), trades)
}

/// The open positions of `states`, sorted by key.
fn open_positions(
    states: &HashMap<(String, String, PositionSide), TradeFold>,
) -> Vec<AccountVenuePosition> {
    let mut open: Vec<AccountVenuePosition> = states
        .iter()
        .filter(|(_, st)| st.size != 0.0)
        .map(|((venue, symbol, side), st)| AccountVenuePosition {
            venue: venue.clone(),
            symbol: symbol.clone(),
            position_side: *side,
            size: st.size,
            average_price: st.average_price,
        })
        .collect();
    // The sides' declaration order (Both, Long, Short) is their words' order (BOTH, LONG, SHORT), so the
    // opening lists as it did when the side was its word.
    open.sort_by(|a, b| {
        (&a.venue, &a.symbol, a.position_side as u8).cmp(&(
            &b.venue,
            &b.symbol,
            b.position_side as u8,
        ))
    });
    open
}

/// **Why the position `book` opens a window with cannot be read**, or `None`: the one rule every reader of a
/// window folded by [`reconstruct_trades_from`] applies (`vike-cli report account:`, the Account window's
/// History Summary). No `start`: the whole recorded history is folded, which opens flat by definition.
///
/// - With the tradehub's position `snapshot` at or before `start` ([`TradesOpeningSnapshot`]), the window opens
///   from it and the account's fills after it, and nothing recorded before the stream is needed. A snapshot whose
///   positions the tradehub could not know refuses, in its words.
/// - Without one (a window that starts before the account's first snapshot), `unattributed_before` counts, per
///   symbol, the venue's fills BEFORE `start` that name no account (recorded before the execution stream): any of
///   them could be `book`'s own, so a symbol it trades (`traded`) has no knowable opening, and the window is refused
///   rather than folded from flat.
pub fn trades_opening_refusal<'a>(
    book: &str,
    start: Option<i64>,
    traded: impl IntoIterator<Item = &'a str>,
    unattributed_before: &[(String, usize)],
    snapshot: Option<&TradesOpeningSnapshot>,
) -> Option<String> {
    let start = start?;
    match snapshot {
        Some(TradesOpeningSnapshot { positions: Ok(_), .. }) => return None,
        Some(TradesOpeningSnapshot { as_of_ms, positions: Err(why) }) => {
            return Some(format!(
                "the opening position of {book} at {} cannot be read: the tradehub's position snapshot of {} \
                 could not know its positions ({why}). Start the window after the tradehub's next start, whose \
                 snapshot may know them, or read the account's whole recorded history.",
                utc(start),
                utc(*as_of_ms)
            ));
        }
        None => {}
    }
    let traded: std::collections::BTreeSet<&str> = traded.into_iter().collect();
    let blind: Vec<String> = unattributed_before
        .iter()
        .filter(|(symbol, n)| *n > 0 && traded.contains(symbol.as_str()))
        .map(|(symbol, n)| format!("{n} of {symbol}"))
        .collect();
    (!blind.is_empty()).then(|| {
        format!(
            "the opening position of {book} at {} cannot be read: fills before it ({}) were recorded \
             before the execution stream and carry no account, so any of them could be its own, and no \
             position snapshot of the tradehub's is at or before it. Start the window after one (the \
             tradehub takes one at every start), or read the account's whole recorded history.",
            utc(start),
            blind.join(", ")
        )
    })
}

/// **The position a window opens with, in words**: at its `start`, every position [`reconstruct_trades_from`]'s
/// earlier fills left (`opening`), or flat, and where it came from (the tradehub's position `snapshot` and the
/// fills after it, or the account's fills before it); with no start, that the fold starts flat at the first
/// recorded fill. `None` with no fill at all.
pub fn trades_opening_words(
    start: Option<i64>,
    opening: &[AccountVenuePosition],
    first_fill: Option<i64>,
    snapshot: Option<&TradesOpeningSnapshot>,
) -> Option<String> {
    let Some(start) = start else {
        return first_fill.map(|t| {
            format!("the fold starts flat at the account's first recorded fill, {}", utc(t))
        });
    };
    let from_snapshot = snapshot.map(|s| {
        format!(
            "the tradehub's position snapshot of {} and the account's fills after it",
            utc(s.as_of_ms)
        )
    });
    if opening.is_empty() {
        return Some(match from_snapshot {
            Some(source) => format!("opening position at {}: flat ({source})", utc(start)),
            None => format!("opening position at {}: flat", utc(start)),
        });
    }
    let held: Vec<String> = opening
        .iter()
        .map(|p| {
            let side = if p.size > 0.0 { "long" } else { "short" };
            let leg = if p.position_side == PositionSide::Both {
                String::new()
            } else {
                format!(" ({} leg)", p.position_side)
            };
            format!("{side} {} {}{leg} at {}", p.size.abs(), p.symbol, p.average_price)
        })
        .collect();
    let source =
        from_snapshot.unwrap_or_else(|| "folded from the account's fills before it".to_string());
    Some(format!("opening position at {}: {} ({source})", utc(start), held.join(", ")))
}

/// An instant as the opening's words print it: RFC 3339, to the second, UTC.
fn utc(ms: i64) -> String {
    vike_model::time::utc_rfc3339(ms.div_euclid(1000))
}

/// Build a full `Trade` from a shared-fold [`TradeFoldClose`]. A bare fill stream has no intrabar
/// path, so `mae`/`mfe` are genuinely uncomputable and left `0.0` (the vector engine does the
/// same); `symbol` is the site's own.
fn trade_from_closed(c: TradeFoldClose, symbol: String) -> Trade {
    Trade {
        entry_price: c.entry_price,
        exit_price: c.exit_price,
        size: c.size,
        pnl: c.pnl,
        fees: c.fees,
        entry_timestamp: c.entry_timestamp,
        exit_timestamp: c.exit_timestamp,
        symbol,
        mae: 0.0,
        mfe: 0.0,
        is_long: c.is_long,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::Event;

    use vike_model::test_support::reports::fill_event;

    #[test]
    fn long_open_then_full_close_is_one_trade() {
        // buy 1 @ 100 (fee 0.1), sell 1 @ 110 (fee 0.2) -> 1 Trade, pnl = (110-100)*1 = 10.
        let fills =
            vec![fill_event(1, 1.0, 100.0, 1_000, 0.1), fill_event(-1, 1.0, 110.0, 2_000, 0.2)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 100.0);
        assert_eq!(t.exit_price, 110.0);
        assert_eq!(t.size, 1.0);
        assert_eq!(t.pnl, 10.0);
        // whole entry fee (portion 1.0) + whole exit fee (closing_qty/qty = 1.0)
        assert!((t.fees - 0.3).abs() < 1e-12);
        assert_eq!(t.entry_timestamp, 1_000);
        assert_eq!(t.exit_timestamp, 2_000);
        assert_eq!(t.symbol, "BTCUSDT");
        assert!(t.is_long);
        assert_eq!(t.mae, 0.0);
        assert_eq!(t.mfe, 0.0);
    }

    #[test]
    fn short_open_then_close_is_one_trade() {
        // sell 1 @ 110, buy 1 @ 100 -> short profits when price falls: pnl = (100-110)*(-1) = 10.
        let fills =
            vec![fill_event(-1, 1.0, 110.0, 1_000, 0.0), fill_event(1, 1.0, 100.0, 2_000, 0.0)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 110.0);
        assert_eq!(t.exit_price, 100.0);
        assert_eq!(t.size, 1.0);
        assert_eq!(t.pnl, 10.0);
        assert!(!t.is_long);
    }

    #[test]
    fn partial_close_leaves_position_open() {
        // buy 2 @ 100, sell 1 @ 120 -> 1 Trade closing the 1-unit half; position still long 1.
        let fills =
            vec![fill_event(1, 2.0, 100.0, 1_000, 0.4), fill_event(-1, 1.0, 120.0, 2_000, 0.2)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 100.0);
        assert_eq!(t.exit_price, 120.0);
        assert_eq!(t.size, 1.0);
        assert_eq!(t.pnl, 20.0); // (120-100)*1
        // portion = closing/|prior| = 1/2 -> entry_fee_portion = 0.4*0.5 = 0.2;
        // exit_fee_portion = 0.2 * (closing 1 / fill_qty 1) = 0.2 -> 0.4 total
        assert!((t.fees - 0.4).abs() < 1e-12);
        assert!(t.is_long);
    }

    #[test]
    fn scale_in_then_close_uses_weighted_average_entry() {
        // buy 1 @ 100, buy 1 @ 200 (avg 150), sell 2 @ 180 -> 1 Trade, pnl = (180-150)*2 = 60.
        let fills = vec![
            fill_event(1, 1.0, 100.0, 1_000, 0.0),
            fill_event(1, 1.0, 200.0, 1_500, 0.0),
            fill_event(-1, 2.0, 180.0, 2_000, 0.0),
        ];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 150.0); // (100 + 200) / 2
        assert_eq!(t.exit_price, 180.0);
        assert_eq!(t.size, 2.0);
        assert_eq!(t.pnl, 60.0);
        // entry_timestamp is the FIRST open's ts (weighted-avg entry keeps the original open time)
        assert_eq!(t.entry_timestamp, 1_000);
        assert_eq!(t.exit_timestamp, 2_000);
    }

    #[test]
    fn flip_closes_prior_and_opens_opposite() {
        // long 2 @ 100, sell 3 @ 130 -> closes the long 2 (pnl = (130-100)*2 = 60) AND opens a
        // short 1 @ 130. Exactly one Trade (the closed long); the new short is still open.
        let fills =
            vec![fill_event(1, 2.0, 100.0, 1_000, 0.0), fill_event(-1, 3.0, 130.0, 2_000, 0.9)];
        let trades = reconstruct_trades(&fills);
        assert_eq!(trades.len(), 1);
        let t = &trades[0];
        assert_eq!(t.entry_price, 100.0);
        assert_eq!(t.exit_price, 130.0);
        assert_eq!(t.size, 2.0); // the closed portion
        assert_eq!(t.pnl, 60.0);
        assert!(t.is_long); // the CLOSED position was long
        // exit_fee_portion = fee * closing/qty = 0.9 * (2/3) = 0.6; no prior entry fee.
        assert!((t.fees - 0.6).abs() < 1e-12);

        // A subsequent buy 1 @ 130 closes the flipped short flat with zero pnl, proving the short
        // opened at the fill price 130 with the leftover 1 unit.
        let mut fills2 = fills.clone();
        fills2.push(fill_event(1, 1.0, 130.0, 3_000, 0.0));
        let trades2 = reconstruct_trades(&fills2);
        assert_eq!(trades2.len(), 2);
        let short = &trades2[1];
        assert_eq!(short.entry_price, 130.0);
        assert_eq!(short.exit_price, 130.0);
        assert_eq!(short.size, 1.0);
        assert_eq!(short.pnl, 0.0);
        assert!(!short.is_long);
    }

    // ⚠ `realized_pnl_matches_account_fold` — the PARITY ANCHOR against `vike_execution::Account` — sat
    // here until 2026-09-28, behind vike-report's old `journal` feature. It moved to
    // `crates/vike-journal/tests/account_parity.rs` rather than into this crate's dev-dependencies;
    // the module doc above says why.

    /// Guard: only bare `Event::Fill`s (the account-affecting fills) feed the reconstructor — the
    /// journal read layer filters lifecycle events out. This test documents that a `Trade` derives
    /// only from `FillEvent` fields, independent of any surrounding `Event` wrapper.
    #[test]
    fn trade_is_built_from_fill_event_fields_only() {
        let f = fill_event(1, 1.0, 100.0, 1, 0.0);
        // wrapping in Event::Fill and unwrapping yields the same struct the reconstructor uses.
        let event = Event::Fill(f.clone());
        let inner = match event {
            Event::Fill(x) => x,
            _ => unreachable!(),
        };
        assert_eq!(inner, f);
    }

    /// [`MULTIPLIER`] is the one spelling of the journaled venues' linear multiplier, shared with the
    /// MTM store builder in `crates/vike-backend` (which held its own private `1.0` copy). Pinned to that
    /// exact value: a change here moves BOTH the trades fold and the mark-to-market curve.
    #[test]
    fn the_journaled_multiplier_is_exactly_linear_one() {
        assert_eq!(MULTIPLIER.to_bits(), 1.0_f64.to_bits());
    }

    /// **A window opens with the position its earlier fills left**: a long bought at 100 before the window and
    /// sold at 110 inside it is ONE trade of +10 against its basis, never a phantom short opened by the exit; a
    /// round trip closed before the window is not the window's. Folded from flat, the window's fills alone read
    /// the exit as a short and book nothing.
    #[test]
    fn a_window_measures_its_exits_against_the_position_it_opens_with() {
        let fills = vec![
            fill_event(1, 1.0, 90.0, 100, 0.0),
            fill_event(-1, 1.0, 95.0, 200, 0.0),
            fill_event(1, 1.0, 100.0, 1_000, 0.0),
            fill_event(-1, 1.0, 110.0, 3_000, 0.0),
        ];
        let (opening, trades) = reconstruct_trades_from(&fills, 2_000);
        assert_eq!(opening.len(), 1, "{opening:?}");
        assert_eq!((opening[0].size, opening[0].average_price), (1.0, 100.0));
        assert_eq!(trades.len(), 1, "{trades:?}");
        assert_eq!(
            (trades[0].pnl, trades[0].entry_timestamp, trades[0].is_long),
            (10.0, 1_000, true)
        );

        let words = trades_opening_words(Some(2_000), &opening, Some(100), None).unwrap();
        assert_eq!(
            words,
            "opening position at 1970-01-01T00:00:02Z: long 1 BTCUSDT at 100 (folded from the account's \
             fills before it)"
        );
        // A fill before the window that names no account, on a traded symbol: no knowable opening.
        let blind = [("BTCUSDT".to_string(), 1)];
        let refused =
            trades_opening_refusal("binance/MAIN", Some(2_000), ["BTCUSDT"], &blind, None);
        assert!(refused.is_some_and(|r| r.contains("binance/MAIN") && r.contains("1 of BTCUSDT")));
        assert_eq!(
            trades_opening_refusal("binance/MAIN", Some(2_000), ["ETHUSDT"], &blind, None),
            None
        );
        assert_eq!(trades_opening_refusal("binance/MAIN", None, ["BTCUSDT"], &blind, None), None);

        let flat = reconstruct_trades(&fills[3..]);
        assert!(flat.is_empty(), "from flat the exit opens a short and closes nothing: {flat:?}");
        assert_eq!(
            reconstruct_trades_from(&fills, i64::MIN),
            (Vec::new(), reconstruct_trades(&fills))
        );
    }

    /// **A position opened before the deploy and closed after it is ONE trade against its basis**: the tradehub's
    /// snapshot at its start holds long 1 at 100 (the fills that opened it were recorded before the stream and
    /// name no account), and a sell of 1 at 110 inside the window closes it for +10. A buy the venue made while
    /// the tradehub was down (journaled after the snapshot, its venue time before it) still folds after the
    /// opening. The unattributed fills before it are never needed; a snapshot that could not know its positions
    /// refuses in its words.
    #[test]
    fn a_window_opens_from_the_tradehubs_position_snapshot() {
        let snapshot = TradesOpeningSnapshot {
            as_of_ms: 1_000,
            positions: Ok(vec![AccountVenuePosition {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                size: 1.0,
                average_price: 100.0,
            }]),
        };
        let after = [fill_event(-1, 1.0, 110.0, 3_000, 0.1)];
        let mut fills = snapshot.opening_fills(1_000);
        fills.extend(after.iter().cloned());
        let (opening, trades) = reconstruct_trades_from(&fills, 2_000);
        assert_eq!((opening.len(), opening[0].size, opening[0].average_price), (1, 1.0, 100.0));
        assert_eq!(trades.len(), 1, "{trades:?}");
        let t = &trades[0];
        assert_eq!((t.pnl, t.entry_price, t.exit_price, t.is_long), (10.0, 100.0, 110.0, true));
        assert_eq!((t.entry_timestamp, t.exit_timestamp), (1_000, 3_000));
        assert!((t.fees - 0.1).abs() < 1e-12, "only the fee paid after the snapshot: {}", t.fees);

        // A down-time buy of 1 at 120, venue time 500: the opening fills take its time, and fold first.
        let mut late = snapshot.opening_fills(500);
        late.push(fill_event(1, 1.0, 120.0, 500, 0.0));
        late.sort_by_key(|f| f.timestamp);
        let (opening, _) = reconstruct_trades_from(&late, 2_000);
        assert_eq!((opening[0].size, opening[0].average_price), (2.0, 110.0));

        let blind = [("BTCUSDT".to_string(), 3)];
        assert_eq!(
            trades_opening_refusal(
                "binance/MAIN",
                Some(2_000),
                ["BTCUSDT"],
                &blind,
                Some(&snapshot)
            ),
            None,
            "the snapshot stands for the fills before it"
        );
        let words =
            trades_opening_words(Some(2_000), &opening_of(&snapshot), None, Some(&snapshot));
        assert_eq!(
            words.as_deref(),
            Some(
                "opening position at 1970-01-01T00:00:02Z: long 1 BTCUSDT at 100 (the tradehub's position \
                 snapshot of 1970-01-01T00:00:01Z and the account's fills after it)"
            )
        );
        let unknown = TradesOpeningSnapshot {
            as_of_ms: 1_000,
            positions: Err("the journal holds no earlier book".into()),
        };
        assert!(unknown.opening_fills(1_000).is_empty());
        let refused = trades_opening_refusal(
            "binance/MAIN",
            Some(2_000),
            Vec::<&str>::new(),
            &[],
            Some(&unknown),
        );
        assert!(
            refused.as_deref().is_some_and(|r| r.contains("binance/MAIN")
                && r.contains("could not know its positions (the journal holds no earlier book)")),
            "{refused:?}"
        );
    }

    /// The positions a snapshot holds, as an opening.
    fn opening_of(snapshot: &TradesOpeningSnapshot) -> Vec<AccountVenuePosition> {
        snapshot.positions.clone().unwrap_or_default()
    }

    /// A hedged opening lists its legs in the order their words did before the side was typed (BOTH, LONG, SHORT),
    /// and names a hedge leg by its word.
    #[test]
    fn a_hedged_opening_lists_its_legs_in_the_order_of_their_words() {
        let leg = |side: PositionSide, direction: i32, ts: i64| {
            let mut fill = fill_event(direction, 1.0, 100.0, ts, 0.0);
            fill.position_side = side;
            fill
        };
        let fills = [
            leg(PositionSide::Short, -1, 3),
            leg(PositionSide::Long, 1, 2),
            leg(PositionSide::Both, 1, 1),
        ];
        let (opening, _) = reconstruct_trades_from(&fills, 10);
        let sides: Vec<String> = opening.iter().map(|p| p.position_side.to_string()).collect();
        assert_eq!(sides, ["BOTH", "LONG", "SHORT"]);
        let words = trades_opening_words(Some(10), &opening, None, None)
            .expect("a start states an opening");
        assert!(words.contains("long 1 BTCUSDT (LONG leg) at 100"), "{words}");
        assert!(words.contains("short 1 BTCUSDT (SHORT leg) at 100"), "{words}");
    }

    /// The constructor puts each stored position on the row's venue in its side's word, and a row that could not
    /// know its positions becomes that refusal, whatever positions it carries.
    #[test]
    fn the_stored_row_becomes_the_snapshot_a_window_opens_from() {
        let held = [vike_model::AccountPosition {
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Short,
            size: -2.0,
            average_price: 50.0,
        }];
        assert_eq!(
            opening_snapshot(1_000, "bybit", &held, None),
            TradesOpeningSnapshot {
                as_of_ms: 1_000,
                positions: Ok(vec![AccountVenuePosition {
                    venue: "bybit".into(),
                    symbol: "BTCUSDT".into(),
                    position_side: PositionSide::Short,
                    size: -2.0,
                    average_price: 50.0,
                }]),
            }
        );
        assert_eq!(
            opening_snapshot(1_000, "bybit", &held, Some("no earlier book")).positions,
            Err("no earlier book".to_string())
        );
    }
}
