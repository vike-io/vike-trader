//! R0 golden: cross-venue equity SUMMATION replayed against the FROZEN
//! `fixtures/r0/portfolio_scenario.json` bytes, re-anchored directly onto `Account::equity_all`.
//!
//! ⚠ This header used to say "parity vs the Python oracle". The comparison is unchanged and stays
//! EXACT, but it is not a comparison against Python: no exporter survives in this tree and the
//! committed bytes ARE the oracle
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).
//!
//! This preserves the one invariant the retired `vike_exec::Portfolio` aggregator gated — that the
//! cross-venue total equity is `py_sum` of each venue's mode-aware `equity_all(seed)` in venue
//! registration order — without the dead read-model. The scenario (two venues, one fill + mark
//! each, bybit flipped Authoritative by an AccountState frame) mirrors the exporter exactly, same
//! as the old `tests/portfolio_parity.rs`; the oracle's fixed hex bits pin it, so a wrong Account
//! setup fails loudly rather than passing silently.

use indexmap::IndexMap;
use serde::Deserialize;
use std::path::PathBuf;
use vike_exec::MarkSource;
use vike_exec::{Account, BalanceMode};
use vike_model::events::{AccountState, FillEvent};
use vike_model::{f64_to_hex_bits, py_sum};

#[derive(Deserialize)]
struct Fixture {
    manifest: serde_json::Value,
    expect: Expect,
}

#[derive(Deserialize)]
struct Expect {
    equity: String,
    venue_equities: IndexMap<String, String>,
}

fn fill(venue: &str, side: i32, qty: f64, px: f64, commission: f64) -> FillEvent {
    FillEvent {
        trade_id: "t".into(),
        client_order_id: "c".into(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_px: px,
        commission,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn mult(sym: &str, m: f64) -> IndexMap<String, f64> {
    let mut map = IndexMap::new();
    map.insert(sym.to_string(), m);
    map
}

#[test]
fn cross_venue_equity_summation_parity_vs_frozen_r0_bytes() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r0/portfolio_scenario.json");
    // ⚠ The message used to end "run scripts/export_r0_fixtures.py". That script was deleted with
    // the Python purge and regeneration is NOT a supported operation (`fixtures/README.md`); the
    // path is kept as the evidence for where these bytes came from, not as an instruction.
    let text = std::fs::read_to_string(path).expect(
        "fixtures/r0/portfolio_scenario.json missing — it is committed and cannot be regenerated \
         (its exporter, scripts/export_r0_fixtures.py, is gone); restore it from git",
    );
    let fx: Fixture = serde_json::from_str(&text).unwrap();
    assert!(fx.manifest.get("source_sha").is_some());

    // binance: seed 10_000, one long fill @ 60_000 (commission 5), marked to 61_000. Delta mode.
    let mut binance = Account::new(1.0, "binance", Some(mult("BTCUSDT", 1.0)), BalanceMode::Delta);
    binance.apply_fill(&fill("binance", 1, 0.5, 60_000.0, 5.0));
    binance.set_mark_from("binance", "BTCUSDT", 61_000.0, MarkSource::VenueMark, 0);

    // bybit: seed 0, one short fill @ 60_500 (commission 4), marked to 61_000, then an AccountState
    // frame flips it Authoritative with an absolute 25_000 USDT balance.
    let mut bybit = Account::new(1.0, "bybit", Some(mult("BTCUSDT", 1.0)), BalanceMode::Delta);
    bybit.apply_fill(&fill("bybit", -1, 0.5, 60_500.0, 4.0));
    bybit.set_mark_from("bybit", "BTCUSDT", 61_000.0, MarkSource::VenueMark, 0);
    bybit.apply_account_state(
        &AccountState {
            venue: "bybit".into(),
            balances: vec![("USDT".into(), 25_000.0)],
            ts: 0,
            route_key: None,
        },
        "USDT",
    );

    // The aggregation law: py_sum of each venue's equity_all(seed) in registration order
    // (binance then bybit) — the exact fold the retired Portfolio::equity() ran.
    let seeds = [("binance", 10_000.0), ("bybit", 0.0)];
    let per_venue = [binance.equity_all(seeds[0].1), bybit.equity_all(seeds[1].1)];
    let total = py_sum(per_venue.iter().copied());

    let b = f64_to_hex_bits;
    assert_eq!(b(total), fx.expect.equity, "cross-venue total equity");
    assert_eq!(fx.expect.venue_equities.len(), 2, "fixture must carry both venue equities");
    assert_eq!(b(per_venue[0]), fx.expect.venue_equities["binance"], "binance venue equity_all");
    assert_eq!(b(per_venue[1]), fx.expect.venue_equities["bybit"], "bybit venue equity_all");
}
