//! Pure divergence detection: reports × local view → `Vec<Divergence>`. Deterministic ordering
//! (fills, then orders, then positions; input order within each) so golden fixtures are stable.
//! Both the order leg and the position leg are TWO-SIDED: a venue-anchored pass (what the venue
//! reports that local disagrees with) followed by a LOCAL-side sweep (what local holds that the
//! venue's report never mentions) — `OrphanLocalOrder` for orders, `OrphanLocalPosition` for
//! positions. The position sweep is the newer of the two: before it, a locally-open position whose
//! venue row was entirely ABSENT raised nothing at all, because the position leg iterated venue
//! rows only (#830 corrected a venue doc that claimed the diff engine already did this; this is
//! the engine actually doing it).
//!
//! An optional third leg — a `JournalView` (edge 2, Task 14) — upgrades the fill AND order checks
//! from a two-way (local vs venue) comparison to a three-way one: `journal: None` reproduces the
//! original two-way behavior byte-for-byte; `Some` distinguishes a plain `MissingFill` (venue
//! trade_id in neither local nor journal) from a `JournalDivergence` (venue trade_id the journal
//! recorded but live local state has since lost — a restore/persistence bug, not an ordinary
//! catch-up fill). The SAME split applies to orders: a venue order absent from local is a plain
//! `UnknownOrder` unless the journal records it as still LIVE (non-terminal), in which case local
//! lost a live order both the venue and the journal know — a `JournalDivergence`. Anchoring the
//! order check on the venue report (only orders the venue currently lists) keeps it free of
//! materializer-lag / terminal-reap false positives.

use std::collections::HashSet;

use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

use super::journal_view::JournalView;
use super::types::{BalanceTol, Divergence, LocalView};
use crate::account::BalanceMode;
use crate::order::OrderStatus;

pub fn diff(
    orders: &[OrderStatusReport],
    fills: &[FillReport],
    positions: &[PositionStatusReport],
    local: &LocalView,
    journal: Option<&JournalView>,
) -> Vec<Divergence> {
    let mut out = Vec::new();

    // 1. Missing fills — venue trade_id we have never folded. When a journal view is supplied,
    // split this into two cases: the journal also never saw it (plain catch-up MissingFill) vs
    // the journal DID record it (the local in-memory state lost a fill the journal has —
    // JournalDivergence, the three-way persistence-bug signal).
    for f in fills {
        if local.seen_trade_ids.contains(f.trade_id.as_str()) {
            continue;
        }
        let journal_has_it =
            journal.is_some_and(|j| j.seen_trade_ids.contains(f.trade_id.as_str()));
        if journal_has_it {
            out.push(Divergence::JournalDivergence {
                detail: format!(
                    "trade_id {} ({} {}) is recorded in the journal but missing from live local \
                     state — possible restore/persistence bug",
                    f.trade_id, f.venue, f.symbol
                ),
                recover_order: None, // fill-loss: nothing to re-register (the fill is at the venue)
            });
        } else {
            out.push(Divergence::MissingFill(f.clone()));
        }
    }

    // 2. Orders: missing-terminal, unknown-order. When the venue reports an order local does not
    // have, split it like the fill check above: if the journal records that order as still LIVE
    // (non-terminal), local lost a live order both the venue and the journal know — a
    // JournalDivergence (restore/persistence bug); otherwise it is an ordinary UnknownOrder (an
    // order local never tracked, or one the journal only has as already-terminal → reaped locally,
    // not a bug).
    for o in orders {
        match o.client_order_id.as_ref().and_then(|c| local.orders.get(c)) {
            Some(mo) => {
                let venue_terminal =
                    OrderStatus::parse(&o.status).map(|s| s.is_terminal()).unwrap_or(false);
                if venue_terminal && !mo.status.is_terminal() {
                    out.push(Divergence::MissingTerminal { order: o.clone() });
                }
            }
            None => {
                let journal_has_it_live = o
                    .client_order_id
                    .as_deref()
                    .zip(journal)
                    .and_then(|(c, j)| j.orders.get(c))
                    .is_some_and(|s| !s.is_terminal());
                if journal_has_it_live {
                    out.push(Divergence::JournalDivergence {
                        detail: format!(
                            "order {} ({} {}) is recorded live in the journal but missing from live \
                             local state — possible restore/persistence bug",
                            o.client_order_id.as_deref().unwrap_or(""),
                            o.venue,
                            o.symbol
                        ),
                        // order-loss: carry the venue report so an operator confirm can re-register it.
                        recover_order: Some(Box::new(o.clone())),
                    });
                } else {
                    out.push(Divergence::UnknownOrder(o.clone()));
                }
            }
        }
    }

    // 3. Orphan local orders — a live local order the venue no longer reports.
    let venue_coids: HashSet<&str> =
        orders.iter().filter_map(|o| o.client_order_id.as_deref()).collect();
    for (coid, mo) in local.orders {
        if !mo.status.is_terminal() && !venue_coids.contains(coid.as_str()) {
            out.push(Divergence::OrphanLocalOrder { client_order_id: coid.clone() });
        }
    }

    // 4. Positions: drift and external-only.
    for p in positions {
        let key = (p.symbol.clone(), position_side_str(p.position_side).to_string());
        match local.positions.get(&key) {
            Some(&local_qty) => {
                if (local_qty - p.qty).abs() > local.qty_tol {
                    out.push(Divergence::PositionDrift { report: p.clone(), local_qty });
                }
            }
            None if p.qty.abs() > local.qty_tol => {
                out.push(Divergence::PositionOnlyExternal(p.clone()));
            }
            None => {}
        }
    }

    // 5. Orphan local positions — a live LOCAL position this pass's venue report does not mention
    // AT ALL. The mirror of step 3's order sweep, and the leg that was missing: step 4 iterates
    // VENUE rows, so an absent row produced no divergence and the pass reported "reconciled" while
    // local still believed it held risk the venue never confirmed. A venue row that IS present —
    // including an explicitly FLAT (`qty == 0`) one, which is why several `ReconClient`s
    // deliberately keep zero rows instead of filtering them — stays entirely on step 4's
    // `PositionDrift` path, so the same disagreement is never reported twice.
    //
    // GATED ON A NON-EMPTY POSITION REPORT. An empty `positions` slice is indistinguishable from
    // "this venue has no position concept / the fetch is not implemented" — binance SPOT's
    // `fetch_position_status_reports` returns `Ok(Vec::new())` unconditionally, and every venue
    // whose `ReconClient` is orders-only does the same — so an empty report sweeps NOTHING. Without
    // that gate every spot inventory row would raise a permanent, un-healable divergence on every
    // pass. Residual (documented, not hidden): a venue whose position fetch is SYMBOL-SCOPED (most
    // of them: binance perp / bybit / deribit / alpaca / ig all filter to the mounted symbol) can
    // still omit a row for a local position in a DIFFERENT symbol, which surfaces here as a
    // false-positive orphan. That is precisely why the kind is quarantine-under-hybrid and folds
    // nothing (`resolve`'s module doc is the authority).
    if !positions.is_empty() {
        let venue_pos_keys: HashSet<(&str, &str)> = positions
            .iter()
            .map(|p| (p.symbol.as_str(), position_side_str(p.position_side)))
            .collect();
        // Same shape as step 3's sweep: genuinely-open local risk (beyond `qty_tol` — the local
        // book keeps a key after it closes) whose (symbol, side) the venue did not report at all.
        for ((symbol, side), &local_qty) in local.positions {
            if local_qty.abs() > local.qty_tol
                && !venue_pos_keys.contains(&(symbol.as_str(), side.as_str()))
            {
                out.push(Divergence::OrphanLocalPosition {
                    venue: local.venue.to_string(),
                    symbol: symbol.clone(),
                    position_side: side.clone(),
                    local_qty,
                });
            }
        }
    }

    out
}

/// Feature 2 — first-class cash/balance reconcile diff. DELIBERATELY SEPARATE from [`diff`], whose
/// signature and golden fixtures stay untouched: the caller pushes this `Option` into the same
/// `divergences` vec `diff` produced, before `resolve`.
///
/// THE MONEY CONFOUND (why this is NOT `|local.balance − venue| > ε`): local `Account::balance` is
/// not a clean mirror of venue cash. Realized PnL never enters `balance` in Authoritative mode
/// (`account.rs::equity_all` — the venue is relied on to re-seed it), while commissions/funding DO
/// adjust it. So between venue balance syncs, local `balance` legitimately diverges from venue cash
/// by Σ realized-PnL-since-sync (which can be large). A naive raw diff false-positives on EVERY
/// realized trade. We therefore compare venue cash against the REALIZED-CORRECTED expected value
///
/// ```text
/// expected = balance + (realized_pnl − realized_at_sync)
/// ```
///
/// which cancels that legitimate drift; the residual is the genuinely-unexplained cash move
/// (withdrawal / deposit / liquidation haircut / funding mis-track) — exactly what we want to catch.
///
/// FIRST-OBSERVATION (adopt, never flag): while local is still `Delta` (`balance` == the arbitrary
/// `seed_cash`, not venue truth) OR was never synced (`realized_at_sync == None`), there is no
/// venue-anchored baseline to diff against — return `None`. The caller ADOPTS (seeds) instead. This
/// mirrors LEAN's `PerformCashSync` waiting for a settled state before comparing.
///
/// Returns `Some(BalanceDrift)` iff `venue_balance` is `Some(v)` AND the realized-corrected drift
/// exceeds `max(tol.abs_floor, tol.rel_frac·|v|)`.
pub fn diff_balance(
    local: &LocalView,
    venue_balance: Option<f64>,
    quote_asset: &str,
    tol: BalanceTol,
    ts: i64,
) -> Option<Divergence> {
    let venue_bal = venue_balance?;
    let cash = &local.cash;
    // First-observation: no venue-anchored baseline yet → adopt, never flag.
    if cash.mode == BalanceMode::Delta {
        return None;
    }
    let baseline = cash.realized_at_sync?; // None => never synced => adopt, never flag
    let expected = cash.balance + (cash.realized_pnl - baseline);
    let drift = venue_bal - expected;
    let threshold = tol.abs_floor.max(tol.rel_frac * venue_bal.abs());
    if drift.abs() > threshold {
        Some(Divergence::BalanceDrift {
            venue: local.venue.to_string(),
            asset: quote_asset.to_string(),
            local: expected,
            venue_bal,
            ts,
        })
    } else {
        None
    }
}

fn position_side_str(s: vike_model::events::PositionSide) -> &'static str {
    use vike_model::events::PositionSide::*;
    match s {
        Both => "BOTH",
        Long => "LONG",
        Short => "SHORT",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::order::ManagedOrder;
    use indexmap::IndexMap;
    use vike_model::OrderRequest;
    use vike_model::events::{LiquiditySide, PositionSide};

    fn empty_local<'a>(
        orders: &'a IndexMap<String, crate::order::ManagedOrder>,
        seen: &'a HashSet<String>,
        pos: &'a IndexMap<(String, String), f64>,
    ) -> LocalView<'a> {
        LocalView {
            venue: "binance",
            orders,
            seen_trade_ids: seen,
            positions: pos,
            qty_tol: 1e-9,
            cash: super::super::types::LocalCash::default(),
        }
    }

    fn fill_report(trade_id: &'static str) -> FillReport {
        FillReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade_id: trade_id.into(),
            venue_order_id: "v1".into(),
            client_order_id: Some("c1".into()),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 1,
        }
    }

    #[test]
    fn unseen_fill_is_missing_fill() {
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[], &[fill_report("t-new")], &[], &local, None);
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::MissingFill(_)));
    }

    #[test]
    fn seen_fill_is_no_divergence() {
        let orders = IndexMap::new();
        let mut seen = HashSet::new();
        seen.insert("t-old".to_string());
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[], &[fill_report("t-old")], &[], &local, None);
        assert!(d.is_empty());
    }

    #[test]
    fn venue_only_position_is_external() {
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let p = PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            avg_px: 100.0,
            ts: 1,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        };
        let d = diff(&[], &[], &[p], &local, None);
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::PositionOnlyExternal(_)));
    }

    #[test]
    fn position_drift_beyond_tol() {
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let mut pos = IndexMap::new();
        pos.insert(("BTCUSDT".to_string(), "BOTH".to_string()), 1.0);
        let local = empty_local(&orders, &seen, &pos);
        let p = PositionStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            avg_px: 100.0,
            ts: 1,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        };
        let d = diff(&[], &[], &[p], &local, None);
        assert!(matches!(d[0], Divergence::PositionDrift { local_qty, .. } if local_qty == 1.0));
    }

    // --- the local-side position sweep (step 5): OrphanLocalPosition ---

    fn position_report(symbol: &str, side: PositionSide, qty: f64) -> PositionStatusReport {
        PositionStatusReport {
            venue: "binance".into(),
            symbol: symbol.into(),
            position_side: side,
            qty,
            avg_px: 100.0,
            ts: 1,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        }
    }

    fn local_positions(rows: &[(&str, &str, f64)]) -> IndexMap<(String, String), f64> {
        rows.iter().map(|(sym, side, q)| ((sym.to_string(), side.to_string()), *q)).collect()
    }

    fn orphan_positions(d: &[Divergence]) -> Vec<&Divergence> {
        d.iter().filter(|x| matches!(x, Divergence::OrphanLocalPosition { .. })).collect()
    }

    #[test]
    fn local_position_the_venue_never_reported_is_an_orphan() {
        // THE closed blind spot: local holds +1 BTCUSDT, the venue's report mentions only a
        // (flat) ETHUSDT row. Before the sweep, the position leg iterated venue rows only, so this
        // produced NO divergence at all and the pass reported "reconciled".
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = local_positions(&[("BTCUSDT", "BOTH", 1.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("ETHUSDT", PositionSide::Both, 0.0)], &local, None);
        assert_eq!(d.len(), 1, "{d:?}");
        match &d[0] {
            Divergence::OrphanLocalPosition { venue, symbol, position_side, local_qty } => {
                assert_eq!(venue, "binance", "echoes the LocalView's venue");
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(position_side, "BOTH");
                assert_eq!(*local_qty, 1.0);
            }
            other => panic!("expected OrphanLocalPosition, got {other:?}"),
        }
    }

    #[test]
    fn venue_reporting_the_position_flat_stays_position_drift_not_an_orphan() {
        // No double-reporting: a venue row that IS present — including an explicitly FLAT one, the
        // shape several ReconClients deliberately emit — keeps the disagreement on the existing
        // PositionDrift path. The sweep must not raise a second divergence for the same key.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = local_positions(&[("BTCUSDT", "BOTH", 1.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 0.0)], &local, None);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(matches!(d[0], Divergence::PositionDrift { local_qty, .. } if local_qty == 1.0));
        assert!(orphan_positions(&d).is_empty(), "no orphan for a key the venue DID report");
    }

    #[test]
    fn empty_venue_position_report_never_sweeps_local_positions() {
        // The gate. An EMPTY position report is indistinguishable from "this venue has no position
        // concept / the fetch is not implemented" — binance SPOT's fetch_position_status_reports
        // returns Ok(Vec::new()) unconditionally — so it must sweep NOTHING, or every spot
        // inventory row would raise a permanent, un-healable divergence on every pass.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = local_positions(&[("BTCUSDT", "BOTH", 1.0), ("ETHUSDT", "BOTH", -3.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[], &[], &[], &local, None);
        assert!(d.is_empty(), "empty position report sweeps nothing: {d:?}");
    }

    #[test]
    fn flat_local_position_rows_are_not_orphans() {
        // `Account::positions` keeps a key after it closes, so the local book routinely holds
        // zero/near-zero rows. Only genuinely-open risk (beyond qty_tol) may raise.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = local_positions(&[("BTCUSDT", "BOTH", 0.0), ("SOLUSDT", "BOTH", 1e-12)]);
        let local = empty_local(&orders, &seen, &pos); // qty_tol = 1e-9
        let d =
            diff(&[], &[], &[position_report("ETHUSDT", PositionSide::Both, 0.0)], &local, None);
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn orphan_local_position_is_keyed_by_side_not_symbol_alone() {
        // Hedge mode: the venue reports the LONG leg only. The SHORT leg is a separate key with no
        // venue row of its own, so it is the orphan — matching a symbol alone would miss it.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = local_positions(&[("BTCUSDT", "LONG", 1.0), ("BTCUSDT", "SHORT", -2.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Long, 1.0)], &local, None);
        let orphans = orphan_positions(&d);
        assert_eq!(
            d.len(),
            1,
            "the LONG leg agrees, so the SHORT orphan is the ONLY finding: {d:?}"
        );
        assert_eq!(orphans.len(), 1, "{d:?}");
        assert!(matches!(
            orphans[0],
            Divergence::OrphanLocalPosition { position_side, local_qty, .. }
                if position_side == "SHORT" && *local_qty == -2.0
        ));
    }

    /// THE DRIFT COMPARISON ITSELF — `(local_qty - p.qty).abs() > qty_tol` — was pinned by nothing.
    /// A mutation sweep replaced its `-` with `+` and with `/` and the whole of `vike-exec` stayed
    /// green (473/473), as did vike-core's recon lane (24/24): every existing position test either
    /// agrees on both sides or has no local key at all, and both of those shapes are blind to the
    /// operator between the two numbers.
    ///
    /// This matters because `PositionDrift` is the ONE no-local-origin kind that `hybrid`
    /// auto-applies: it rewrites local position size onto the venue's number and books realized PnL
    /// at the venue's average price with no operator in front of it. A comparison that computes the
    /// wrong delta therefore either fires when the books agree, or stays silent while they do not.
    ///
    /// Three shapes, chosen so that each mutation is caught by at least one:
    #[test]
    fn the_drift_test_is_a_difference_not_a_sum_or_a_ratio() {
        let orders = IndexMap::new();
        let seen = HashSet::new();

        // 1. SIGN-OPPOSED: local is short 1, the venue says long 1. The true delta is 2 (a
        //    divergence); a SUM is 0 (silence). This is also the only production feeder of
        //    `synth_position_legs`' zero-crossing PnL branch, so silence here is expensive.
        let pos = local_positions(&[("BTCUSDT", "BOTH", -1.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 1.0)], &local, None);
        assert_eq!(d.len(), 1, "sign-opposed books are a drift: {d:?}");
        assert!(
            matches!(&d[0], Divergence::PositionDrift { local_qty, .. } if *local_qty == -1.0),
            "{d:?}"
        );

        // 2. FLAT LOCAL vs a live venue position. The true delta is 5; a RATIO is 0/5 = 0
        //    (silence). A 0.0 local key is the ordinary shape, not a contrived one —
        //    `Account::apply_fill` REBINDS a closed key to the new size rather than removing it,
        //    so a flat-but-known symbol is exactly what `local_view` copies.
        let pos = local_positions(&[("BTCUSDT", "BOTH", 0.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 5.0)], &local, None);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            matches!(&d[0], Divergence::PositionDrift { local_qty, .. } if *local_qty == 0.0),
            "a KNOWN-but-flat key drifts; it is not PositionOnlyExternal, which is for keys \
             local has never heard of: {d:?}"
        );

        // 3. THE AGREEING CONTROL. Without it a mutation that fires ALWAYS would still pass the
        //    two above — they only prove the comparison is not silent.
        let pos = local_positions(&[("BTCUSDT", "BOTH", 3.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("BTCUSDT", PositionSide::Both, 3.0)], &local, None);
        assert!(d.is_empty(), "books that agree are not a divergence: {d:?}");
    }

    #[test]
    fn venue_only_positions_with_an_empty_local_book_are_unchanged() {
        // The pre-existing venue-anchored behavior is untouched when local holds nothing.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let reports = [
            position_report("BTCUSDT", PositionSide::Both, 2.0),
            position_report("ETHUSDT", PositionSide::Both, -1.0),
        ];
        let d = diff(&[], &[], &reports, &local, None);
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(d.iter().all(|x| matches!(x, Divergence::PositionOnlyExternal(_))));
    }

    #[test]
    fn orphan_local_positions_are_emitted_after_the_venue_position_leg() {
        // Deterministic ordering (the module doc's contract): the venue-anchored leg first, then
        // the local-side sweep — the same shape the order legs already have.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = local_positions(&[("BTCUSDT", "BOTH", 1.0)]);
        let local = empty_local(&orders, &seen, &pos);
        let d =
            diff(&[], &[], &[position_report("ETHUSDT", PositionSide::Both, 2.0)], &local, None);
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(matches!(d[0], Divergence::PositionOnlyExternal(_)));
        assert!(matches!(d[1], Divergence::OrphanLocalPosition { .. }));
    }

    // --- three-way (journal) cross-check, Task 14 ---

    #[test]
    fn journal_none_matches_two_way_missing_fill() {
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[], &[fill_report("t-new")], &[], &local, None);
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::MissingFill(_)));
    }

    #[test]
    fn trade_in_neither_local_nor_journal_stays_missing_fill() {
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let journal = JournalView { seen_trade_ids: HashSet::new(), orders: IndexMap::new() };
        let d = diff(&[], &[fill_report("t-new")], &[], &local, Some(&journal));
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::MissingFill(_)));
    }

    #[test]
    fn trade_in_journal_but_not_local_is_journal_divergence() {
        let orders = IndexMap::new();
        let seen = HashSet::new(); // local lost it
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let mut journal_seen = HashSet::new();
        journal_seen.insert("t-restored".to_string());
        let journal = JournalView { seen_trade_ids: journal_seen, orders: IndexMap::new() };
        let d = diff(&[], &[fill_report("t-restored")], &[], &local, Some(&journal));
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::JournalDivergence { .. }));
    }

    #[test]
    fn trade_in_both_local_and_journal_is_no_divergence() {
        let orders = IndexMap::new();
        let mut seen = HashSet::new();
        seen.insert("t-old".to_string());
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let mut journal_seen = HashSet::new();
        journal_seen.insert("t-old".to_string());
        let journal = JournalView { seen_trade_ids: journal_seen, orders: IndexMap::new() };
        let d = diff(&[], &[fill_report("t-old")], &[], &local, Some(&journal));
        assert!(d.is_empty());
    }

    fn order_report(coid: &str, status: &str) -> OrderStatusReport {
        OrderStatusReport {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v1".into(),
            client_order_id: Some(coid.into()),
            side: 1,
            order_type: "LIMIT".into(),
            qty: 1.0,
            filled_qty: 0.0,
            avg_px: 0.0,
            status: status.into(),
            ts: 1,
        }
    }

    fn journal_with_order(coid: &str, status: OrderStatus) -> JournalView {
        let mut orders = IndexMap::new();
        orders.insert(coid.to_string(), status);
        JournalView { seen_trade_ids: HashSet::new(), orders }
    }

    #[test]
    fn venue_order_not_in_local_no_journal_is_unknown_order() {
        // byte-identical to the pre-journal two-way behavior (journal: None → UnknownOrder).
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[order_report("c-ext", "ACCEPTED")], &[], &[], &local, None);
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::UnknownOrder(_)));
    }

    #[test]
    fn venue_order_live_in_journal_but_not_local_is_journal_divergence() {
        // the journal recorded this order as still LIVE (ACCEPTED) and the venue reports it, but
        // local in-memory state lost it → a restore/persistence bug, not a plain unknown order.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let journal = journal_with_order("c-lost", OrderStatus::Accepted);
        let d = diff(&[order_report("c-lost", "ACCEPTED")], &[], &[], &local, Some(&journal));
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::JournalDivergence { .. }));
    }

    #[test]
    fn venue_order_terminal_in_journal_but_not_local_stays_unknown_order() {
        // the journal only has the order as already-terminal (FILLED) — local correctly reaped it,
        // so its absence is NOT a persistence bug: it stays a plain UnknownOrder, not a divergence.
        let orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = empty_local(&orders, &seen, &pos);
        let journal = journal_with_order("c-done", OrderStatus::Filled);
        let d = diff(&[order_report("c-done", "FILLED")], &[], &[], &local, Some(&journal));
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Divergence::UnknownOrder(_)));
    }

    // --- diff_balance (Feature 2: first-class cash reconcile) ---

    use super::super::types::{BalanceTol, LocalCash};
    use crate::account::BalanceMode;

    /// A `LocalView` carrying a specific cash slice (orders/positions empty — irrelevant here).
    fn cash_local<'a>(
        orders: &'a IndexMap<String, crate::order::ManagedOrder>,
        seen: &'a HashSet<String>,
        pos: &'a IndexMap<(String, String), f64>,
        cash: LocalCash,
    ) -> LocalView<'a> {
        LocalView {
            venue: "binance",
            orders,
            seen_trade_ids: seen,
            positions: pos,
            qty_tol: 1e-9,
            cash,
        }
    }

    fn authoritative(balance: f64, realized_pnl: f64, realized_at_sync: f64) -> LocalCash {
        LocalCash {
            balance,
            realized_pnl,
            realized_at_sync: Some(realized_at_sync),
            mode: BalanceMode::Authoritative,
        }
    }

    #[test]
    fn diff_balance_none_when_venue_balance_absent() {
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let local = cash_local(&o, &s, &p, authoritative(10_000.0, 0.0, 0.0));
        assert!(diff_balance(&local, None, "USDT", BalanceTol::default(), 1).is_none());
    }

    #[test]
    fn diff_balance_none_in_delta_first_observation() {
        // Delta mode: `balance` is the arbitrary seed_cash, not venue truth — never flag even when
        // the raw numbers differ wildly (the caller adopts instead).
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let delta = LocalCash { balance: 10_000.0, ..LocalCash::default() };
        let local = cash_local(&o, &s, &p, delta);
        assert!(diff_balance(&local, Some(50_000.0), "USDT", BalanceTol::default(), 1).is_none());
    }

    #[test]
    fn diff_balance_none_without_baseline() {
        // Authoritative but never synced (realized_at_sync None): still first-observation → adopt.
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let no_baseline = LocalCash {
            balance: 10_000.0,
            realized_pnl: 500.0,
            realized_at_sync: None,
            mode: BalanceMode::Authoritative,
        };
        let local = cash_local(&o, &s, &p, no_baseline);
        assert!(diff_balance(&local, Some(12_000.0), "USDT", BalanceTol::default(), 1).is_none());
    }

    #[test]
    fn diff_balance_none_within_tolerance() {
        // venue == expected within the 1-unit floor: no divergence.
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let local = cash_local(&o, &s, &p, authoritative(10_000.0, 0.0, 0.0));
        // expected = 10_000; venue 10_000.5 is inside max(1.0, 1e-4·10_000.5 = 1.00005) = 1.00005.
        assert!(diff_balance(&local, Some(10_000.5), "USDT", BalanceTol::default(), 1).is_none());
    }

    #[test]
    fn diff_balance_realized_pnl_since_seed_does_not_trip() {
        // THE CONFOUND GUARD. Last sync captured realized_at_sync = 200. Since then the account
        // realized +800 more (realized_pnl now 1000) but `balance` did NOT move (Authoritative mode
        // does not fold realized into balance). The venue cash, however, DID grow by that +800 to
        // 10_800. expected = balance 10_000 + (1000 − 200) = 10_800 == venue → NO drift, despite an
        // 800-unit raw gap. A naive |balance − venue| diff would false-flag 800 here.
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let local = cash_local(&o, &s, &p, authoritative(10_000.0, 1000.0, 200.0));
        assert!(diff_balance(&local, Some(10_800.0), "USDT", BalanceTol::default(), 1).is_none());
    }

    #[test]
    fn diff_balance_flags_genuine_surprise_beyond_tolerance() {
        // Same realized state as above (expected 10_800), but the venue reports 10_950 — an extra
        // +150 that realized PnL does NOT explain (a deposit / mis-track / haircut). Flag it.
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let local = cash_local(&o, &s, &p, authoritative(10_000.0, 1000.0, 200.0));
        let d = diff_balance(&local, Some(10_950.0), "USDT", BalanceTol::default(), 7)
            .expect("a 150-unit unexplained move must flag");
        match d {
            Divergence::BalanceDrift { venue, asset, local, venue_bal, ts } => {
                assert_eq!(venue, "binance");
                assert_eq!(asset, "USDT");
                assert_eq!(local, 10_800.0, "carries the realized-corrected expected, not raw");
                assert_eq!(venue_bal, 10_950.0);
                assert_eq!(ts, 7);
            }
            other => panic!("expected BalanceDrift, got {other:?}"),
        }
    }

    #[test]
    fn diff_balance_relative_band_scales_with_wallet_size() {
        // A 5-unit drift on a $10M wallet is inside the 1-bp relative band (1e-4·10M = 1000) → no
        // flag; the same 5-unit drift on a $1k wallet clears max(1.0, 0.1) = 1.0 → flags.
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let big = cash_local(&o, &s, &p, authoritative(10_000_000.0, 0.0, 0.0));
        assert!(diff_balance(&big, Some(10_000_005.0), "USDT", BalanceTol::default(), 1).is_none());
        let small = cash_local(&o, &s, &p, authoritative(1_000.0, 0.0, 0.0));
        assert!(diff_balance(&small, Some(1_005.0), "USDT", BalanceTol::default(), 1).is_some());
    }

    #[test]
    fn diff_balance_honors_a_custom_tolerance() {
        // A NON-default BalanceTol (the env-tunable knob path) actually moves the flag boundary.
        let (o, s, p) = (IndexMap::new(), HashSet::new(), IndexMap::new());
        let local = cash_local(&o, &s, &p, authoritative(10_000.0, 0.0, 0.0)); // expected = 10_000

        // A 0.5 drift is WITHIN the default band (no flag) but TRIPS a tighter abs floor.
        assert!(diff_balance(&local, Some(10_000.5), "USDT", BalanceTol::default(), 1).is_none());
        let tight = BalanceTol { abs_floor: 0.1, rel_frac: 0.0 };
        assert!(diff_balance(&local, Some(10_000.5), "USDT", tight, 1).is_some());

        // A 50.0 drift TRIPS the default band (flag) but is SUPPRESSED by a looser abs floor.
        assert!(diff_balance(&local, Some(10_050.0), "USDT", BalanceTol::default(), 1).is_some());
        let loose = BalanceTol { abs_floor: 100.0, rel_frac: 0.0 };
        assert!(diff_balance(&local, Some(10_050.0), "USDT", loose, 1).is_none());
    }

    /// A local order whose coid the venue ALSO reports on, which sounds like the ordinary case and
    /// is in fact the one shape nothing in this workspace built. Every existing order test either
    /// has an empty `local.orders` (so `diff` takes the `None` arm) or a venue slice that never
    /// names the local coid — so `venue_terminal && !mo.status.is_terminal()`, the whole
    /// `MissingTerminal` decision, was reachable by no test at all. A sweep both flipped its `&&`
    /// to `||` and deleted its `!`; `vike-exec` stayed green through each.
    ///
    /// The three rows below are the truth table of that condition. Without all three, one mutation
    /// or the other slips through: `||` needs a case where the venue is live, and dropping `!`
    /// needs a case where BOTH sides are terminal.
    #[test]
    fn missing_terminal_needs_the_venue_terminal_and_the_local_order_still_live() {
        // `ManagedOrder`'s fields are `pub` and this is the same crate, so the baseline status is
        // ASSIGNED rather than driven through `apply` — the FSM would refuse most of these
        // transitions and the test would then prove nothing about `diff`.
        fn local_order(coid: &str, status: OrderStatus) -> IndexMap<String, ManagedOrder> {
            let mut mo = ManagedOrder::new(OrderRequest {
                client_order_id: coid.to_string(),
                ..Default::default()
            });
            mo.status = status;
            IndexMap::from([(coid.to_string(), mo)])
        }
        let (seen, pos) = (HashSet::new(), IndexMap::new());

        // 1. Both sides say the order is LIVE — nothing to reconcile.
        //    ⚠ the venue string must be "ACCEPTED", not "NEW": `OrderStatus::parse` has no `NEW`
        //    arm, so `"NEW"` would be `unwrap_or(false)` and pass for the wrong reason. The
        //    binance/okx `normalize_order_status` helpers emit "ACCEPTED".
        let orders = local_order("c-1", OrderStatus::Accepted);
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[order_report("c-1", "ACCEPTED")], &[], &[], &local, None);
        assert!(d.is_empty(), "both sides live is not a divergence: {d:?}");

        // 2. The venue finished it, local still thinks it rests — THE case this branch exists for.
        let orders = local_order("c-1", OrderStatus::Accepted);
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[order_report("c-1", "FILLED")], &[], &[], &local, None);
        assert_eq!(d.len(), 1, "{d:?}");
        match &d[0] {
            Divergence::MissingTerminal { order } => {
                // `as_deref`: `client_order_id` is `Option<String>` and there is no `PartialEq`
                // against `Option<&str>`.
                assert_eq!(order.client_order_id.as_deref(), Some("c-1"));
                assert_eq!(order.status, "FILLED");
            }
            other => panic!("expected MissingTerminal, got {other:?}"),
        }

        // 3. Both terminal — local already knows. Deleting the `!` makes THIS the divergence.
        let orders = local_order("c-1", OrderStatus::Filled);
        let local = empty_local(&orders, &seen, &pos);
        let d = diff(&[order_report("c-1", "FILLED")], &[], &[], &local, None);
        assert!(
            d.is_empty(),
            "an order local already knows is terminal is not a divergence: {d:?}"
        );
    }
}
