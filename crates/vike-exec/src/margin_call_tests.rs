use super::*;
use crate::MarkSource;
use crate::account::BalanceMode;
use std::assert_matches;
use vike_model::events::FillEvent;

fn fill(symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // Unique per fill: never the EMPTY id the newtype forbids, and never leaning on dedup.
        trade_id: vike_model::events::TradeId::prefixed(
            "mc-",
            format_args!("{symbol}-{side}-{qty}-{px}"),
        ),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn account_with(symbol: &str, size: f64, entry: f64, mark: f64) -> Account {
    let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
    a.apply_fill(&fill(symbol, if size > 0.0 { 1 } else { -1 }, size.abs(), entry));
    a.set_mark_from("binance", symbol, mark, MarkSource::VenueMark, 0);
    a
}

#[test]
fn flat_account_is_healthy() {
    let a = Account::new(1.0, "binance", None, BalanceMode::Delta);
    assert_eq!(check_margin_call(&a, 1_000.0, &MarginCallConfig::default()), MarginCall::Healthy);
}

#[test]
fn comfortable_margin_is_healthy() {
    // 1 BTC @ 100, mm 5% -> used 5; equity 1000 -> remaining 995 >> 50
    let a = account_with("BTCUSDT", 1.0, 100.0, 100.0);
    assert_eq!(check_margin_call(&a, 1_000.0, &MarginCallConfig::default()), MarginCall::Healthy);
}

#[test]
fn warning_fires_at_five_percent() {
    // used = 10·100·0.05 = 50; equity 52 -> remaining 2 <= 52·0.05 = 2.6 -> warn
    // (not liquidatable: remaining > 0)
    let a = account_with("BTCUSDT", 10.0, 100.0, 100.0);
    match check_margin_call(&a, 52.0, &MarginCallConfig::default()) {
        MarginCall::Warning { margin_used, margin_remaining } => {
            assert_eq!(margin_used, 50.0);
            assert_eq!(margin_remaining, 2.0);
        }
        other => panic!("expected warning, got {other:?}"),
    }
}

#[test]
fn liquidation_needs_both_conditions() {
    // used 50, equity 48: remaining -2 <= 0 BUT used(50) <= 48·1.1=52.8 -> warning only
    let a = account_with("BTCUSDT", 10.0, 100.0, 100.0);
    assert_matches!(
        check_margin_call(&a, 48.0, &MarginCallConfig::default()),
        MarginCall::Warning { .. }
    );
    // equity 40: remaining -10, used 50 > 44 -> liquidate the excess (10/5 = 2 units)
    match check_margin_call(&a, 40.0, &MarginCallConfig::default()) {
        MarginCall::Liquidate(intents) => {
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].side, -1); // closing a long
            assert!((intents[0].qty - 2.0).abs() < 1e-12); // excess 10 / per-unit 5
        }
        other => panic!("expected liquidation, got {other:?}"),
    }
}

#[test]
fn losers_liquidated_first() {
    let mut a = account_with("AAA", 10.0, 100.0, 100.0); // flat PnL
    a.apply_fill(&fill("BBB", 1, 10.0, 120.0)); // entry 120, marked 100 -> loser
    a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
    // used = 2·(10·100·0.05) = 100; equity 20 -> excess 80 -> BBB (loser) first: full 10
    // units free 50, then AAA for the remaining 30 -> 6 units
    match check_margin_call(&a, 20.0, &MarginCallConfig::default()) {
        MarginCall::Liquidate(intents) => {
            assert_eq!(intents[0].symbol, "BBB");
            assert!((intents[0].qty - 10.0).abs() < 1e-12);
            assert_eq!(intents[1].symbol, "AAA");
            assert!((intents[1].qty - 6.0).abs() < 1e-12);
        }
        other => panic!("expected liquidation, got {other:?}"),
    }
}

#[test]
fn unmarked_position_never_liquidated() {
    let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
    a.apply_fill(&fill("NOMARK", 1, 10.0, 100.0)); // no set_mark
    assert_eq!(
        check_margin_call(&a, 1.0, &MarginCallConfig::default()),
        MarginCall::Healthy // contributes no margin, cannot be priced
    );
}

// --- the price-generalized law ---------------------------------------------------------

/// The price basis is the parameter: the SAME account judged through a caller-supplied
/// price (the live watchdog's resolver closure shape) liquidates where the marks-priced
/// legacy entry — with NO marks recorded — sees nothing at all; and with the marks
/// closure the two entries are the same function (delegation).
#[test]
fn check_margin_call_priced_takes_its_price_from_the_closure() {
    // position long 10 @ 100 but NO `set_mark` — the marks-priced law cannot price it.
    let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
    a.apply_fill(&fill("BTCUSDT", 1, 10.0, 100.0));
    let cfg = MarginCallConfig::default();
    assert_eq!(check_margin_call(&a, 40.0, &cfg), MarginCall::Healthy, "unpriceable → skip");
    // the SAME account under a closure pricing at 100 reproduces the marked verdict:
    // used 50, equity 40 → remaining −10 ≤ 0 and 50 > 44 → liquidate excess 10/5 = 2.
    match check_margin_call_priced(&a, 40.0, &cfg, |_k, _p| Some(100.0)) {
        MarginCall::Liquidate(intents) => {
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].side, -1);
            assert!((intents[0].qty - 2.0).abs() < 1e-12);
        }
        other => panic!("expected liquidation under the supplied price, got {other:?}"),
    }
    // a DIFFERENT price re-judges the same book: at 40, used = 10·40·0.05 = 20,
    // equity 40 → remaining 20 → healthy (the plan reprices with the basis).
    assert_eq!(check_margin_call_priced(&a, 40.0, &cfg, |_k, _p| Some(40.0)), MarginCall::Healthy);
}

// --- the cross-only byte-identity pin --------------------------------------------------
// The unpartitioned sweep, verbatim. Every position defaults `MarginMode::Cross`, so the
// partitioned law must return the IDENTICAL verdict — same variant, same f64 bits, same intent
// order — across the whole scenario matrix.
fn check_margin_call_legacy(account: &Account, equity: f64, cfg: &MarginCallConfig) -> MarginCall {
    // The OLD fold inlined VERBATIM (margin_call.rs @ 67314e4~1), not `Account::margin_in_use`:
    // that delegates to the shared body the new path uses, so the pin would compare the fold
    // against itself and stop discriminating a drift IN the fold.
    let mut margin_used = 0.0;
    for ((v, s, _side), p) in account.positions.iter() {
        if p.size == 0.0 {
            continue;
        }
        if let Some(mark) = account.mark_of(v, s) {
            margin_used += p.size.abs() * mark * account.multiplier_of(s) * cfg.mm_requirement;
        }
    }
    if margin_used <= 0.0 {
        return MarginCall::Healthy;
    }
    let remaining = equity - margin_used;
    let liquidatable = remaining <= 0.0 && margin_used > equity * (1.0 + cfg.buffer);
    if liquidatable {
        let mut rows: Vec<(String, String, String, f64, f64, f64)> = Vec::new();
        for ((v, s, side), p) in account.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let Some(mark) = account.mark_of(v, s) else {
                continue;
            };
            let upnl = account.unrealized_of_key(&(*v, *s, *side));
            rows.push((v.to_string(), s.to_string(), side.to_string(), p.size, mark, upnl));
        }
        rows.sort_by(|a, b| a.5.partial_cmp(&b.5).unwrap_or(std::cmp::Ordering::Equal));
        let mut excess = margin_used - equity;
        let mut intents = Vec::new();
        for (v, s, side, size, mark, _upnl) in rows {
            if excess <= 0.0 {
                break;
            }
            let per_unit = mark * account.multiplier_of(&s) * cfg.mm_requirement;
            if per_unit <= 0.0 {
                continue;
            }
            let qty = (excess / per_unit).min(size.abs());
            if qty <= 0.0 {
                continue;
            }
            intents.push(LiquidationIntent {
                venue: v,
                symbol: s,
                position_side: side,
                side: vike_model::closing_side(size),
                qty,
            });
            excess -= qty * per_unit;
        }
        if !intents.is_empty() {
            return MarginCall::Liquidate(intents);
        }
    }
    if remaining <= equity * cfg.warn_fraction {
        return MarginCall::Warning { margin_used, margin_remaining: remaining };
    }
    MarginCall::Healthy
}

/// f64-bit-exact comparison of two verdicts (PartialEq would pass -0.0 == 0.0).
fn assert_verdict_bits(got: &MarginCall, want: &MarginCall, label: &str) {
    match (got, want) {
        (MarginCall::Healthy, MarginCall::Healthy) => {}
        (
            MarginCall::Warning { margin_used: a, margin_remaining: b },
            MarginCall::Warning { margin_used: c, margin_remaining: d },
        ) => {
            assert_eq!(a.to_bits(), c.to_bits(), "{label}: margin_used bits");
            assert_eq!(b.to_bits(), d.to_bits(), "{label}: margin_remaining bits");
        }
        (MarginCall::Liquidate(a), MarginCall::Liquidate(b)) => {
            assert_eq!(a.len(), b.len(), "{label}: intent count");
            for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                assert_eq!(x.venue, y.venue, "{label}[{i}]: venue");
                assert_eq!(x.symbol, y.symbol, "{label}[{i}]: symbol");
                assert_eq!(x.position_side, y.position_side, "{label}[{i}]: position_side");
                assert_eq!(x.side, y.side, "{label}[{i}]: side");
                assert_eq!(x.qty.to_bits(), y.qty.to_bits(), "{label}[{i}]: qty bits");
            }
        }
        _ => panic!("{label}: verdict variant diverged: got {got:?}, want {want:?}"),
    }
}

#[test]
fn cross_only_verdicts_byte_identical_to_legacy() {
    let cfg = MarginCallConfig::default();
    // the scenario matrix: flat, healthy, warning, both-conditions boundary, deep breach,
    // multi-position losers-first, unmarked, short, and an off-grid equity sweep
    let mut accounts: Vec<Account> = vec![
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        account_with("BTCUSDT", 1.0, 100.0, 100.0),
        account_with("BTCUSDT", 10.0, 100.0, 100.0),
        account_with("ETHUSDT", -10.0, 100.0, 105.0), // losing short
    ];
    {
        let mut a = account_with("AAA", 10.0, 100.0, 100.0);
        a.apply_fill(&fill("BBB", 1, 10.0, 120.0));
        a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
        accounts.push(a);
    }
    {
        let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
        a.apply_fill(&fill("NOMARK", 1, 10.0, 100.0));
        accounts.push(a);
    }
    for (ai, a) in accounts.iter().enumerate() {
        for equity in [-10.0, 0.0, 1.0, 20.0, 40.0, 44.999, 48.0, 50.0, 52.0, 100.0, 1_000.0, 1e9] {
            let got = check_margin_call(a, equity, &cfg);
            let want = check_margin_call_legacy(a, equity, &cfg);
            assert_verdict_bits(&got, &want, &format!("account[{ai}] equity={equity}"));
        }
    }
}

// --- isolated / cash pools -------------------------------------------------------------

use vike_model::MarginMode;

fn make_isolated(a: &mut Account, symbol: &str, wallet: Option<f64>) {
    let key: crate::account::PositionKey = ("binance".into(), symbol.into(), "BOTH".into());
    let p = a.positions.get_mut(&key).expect("position exists");
    p.margin_mode = MarginMode::Isolated;
    p.isolated_margin = wallet;
}

fn make_cash(a: &mut Account, symbol: &str) {
    let key: crate::account::PositionKey = ("binance".into(), symbol.into(), "BOTH".into());
    a.positions.get_mut(&key).expect("position exists").margin_mode = MarginMode::Cash;
}

#[test]
fn isolated_breach_closes_only_that_position_full_size() {
    // cross AAA is comfortably healthy; isolated BBB is deep underwater:
    // wallet 10, entry 120 → mark 100 ⇒ upnl −200, pool equity −190 ≤ maint 50 → breach
    let mut a = account_with("AAA", 1.0, 100.0, 100.0);
    a.apply_fill(&fill("BBB", 1, 10.0, 120.0));
    a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
    make_isolated(&mut a, "BBB", Some(10.0));
    match check_margin_call(&a, 10_000.0, &MarginCallConfig::default()) {
        MarginCall::Liquidate(intents) => {
            assert_eq!(intents.len(), 1, "only the isolated pool closes");
            assert_eq!(intents[0].symbol, "BBB");
            assert_eq!(intents[0].side, -1);
            assert_eq!(intents[0].qty, 10.0); // FULL close, never partial
        }
        other => panic!("expected isolated liquidation, got {other:?}"),
    }
}

#[test]
fn breached_isolated_position_does_not_drag_cross_pool() {
    // The isolated loss is walled off: cross AAA (margin 5 at mm 5%) stays healthy on the
    // cross pool's own equity even though the ACCOUNT equity is wrecked by BBB's loss.
    let mut a = account_with("AAA", 1.0, 100.0, 100.0);
    a.apply_fill(&fill("BBB", 1, 10.0, 120.0));
    a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
    make_isolated(&mut a, "BBB", Some(10.0));
    // account equity 50: naive whole-account math would margin-call AAA too; the law
    // subtracts BBB's pool (10 + (−200) = −190) → cross equity 240 → AAA healthy.
    match check_margin_call(&a, 50.0, &MarginCallConfig::default()) {
        MarginCall::Liquidate(intents) => {
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].symbol, "BBB"); // BBB closes; AAA is never touched
        }
        other => panic!("expected only the isolated close, got {other:?}"),
    }
}

#[test]
fn healthy_isolated_pool_stays_open_and_out_of_cross_margin() {
    // isolated BBB: wallet 100, flat PnL → pool equity 100 > maint 50 → no breach; and its
    // margin never counts against the cross pool, so a tiny cross-equity stays healthy…
    let mut a = account_with("AAA", 1.0, 100.0, 100.0); // cross maint = 5
    a.apply_fill(&fill("BBB", 1, 10.0, 100.0));
    a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
    make_isolated(&mut a, "BBB", Some(100.0));
    // account equity 200 → cross equity 200 − (100 + 0) = 100 ≫ 5 → Healthy
    assert_eq!(check_margin_call(&a, 200.0, &MarginCallConfig::default()), MarginCall::Healthy);
}

#[test]
fn isolated_without_wallet_or_mark_is_skipped_not_liquidated() {
    // wallet unreported → unpriceable → LEAN skip (never a spurious close)
    let mut a = account_with("AAA", 1.0, 100.0, 100.0);
    a.apply_fill(&fill("BBB", 1, 10.0, 120.0));
    a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
    make_isolated(&mut a, "BBB", None);
    assert_eq!(check_margin_call(&a, 10_000.0, &MarginCallConfig::default()), MarginCall::Healthy);
    // unmarked isolated (wallet present) → also unpriceable → skip
    let mut b = account_with("AAA", 1.0, 100.0, 100.0);
    b.apply_fill(&fill("NOMARK", 1, 10.0, 120.0));
    make_isolated(&mut b, "NOMARK", Some(1.0));
    assert_eq!(check_margin_call(&b, 10_000.0, &MarginCallConfig::default()), MarginCall::Healthy);
}

#[test]
fn cash_positions_never_liquidate() {
    // a cash position contributes no maintenance and is never a candidate, even at
    // catastrophic account equity — the pool that structurally cannot breach
    let mut a = account_with("SPOT", 10.0, 100.0, 100.0);
    make_cash(&mut a, "SPOT");
    for equity in [-1_000.0, 0.0, 1.0, 1_000.0] {
        assert_eq!(
            check_margin_call(&a, equity, &MarginCallConfig::default()),
            MarginCall::Healthy,
            "equity {equity}"
        );
    }
}

#[test]
fn cross_breach_excludes_isolated_from_candidates() {
    // cross AAA underwater AND isolated BBB healthy: the cross plan may only touch AAA.
    let mut a = account_with("AAA", 10.0, 100.0, 100.0); // cross maint 50
    a.apply_fill(&fill("BBB", 1, 10.0, 100.0));
    a.set_mark_from("binance", "BBB", 100.0, MarkSource::VenueMark, 0);
    make_isolated(&mut a, "BBB", Some(100.0));
    // account equity 140 → cross equity 140 − 100 = 40 → breach (50 > 44); excess 10 → 2 units
    match check_margin_call(&a, 140.0, &MarginCallConfig::default()) {
        MarginCall::Liquidate(intents) => {
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].symbol, "AAA");
            assert!((intents[0].qty - 2.0).abs() < 1e-12);
        }
        other => panic!("expected cross-only liquidation, got {other:?}"),
    }
}
