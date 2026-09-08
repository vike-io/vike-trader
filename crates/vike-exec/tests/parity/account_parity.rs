//! R0 golden parity: `Account` (fold/marks/funding/account-state/liquidation/equity) replayed
//! against the FROZEN `fixtures/r0/account_scenarios.json` bytes. Bit-for-bit on every f64.
//!
//! ⚠ This header used to say "vs the Python oracle". Nothing here compares against Python: the
//! exporter that produced these bytes is gone and the committed bytes ARE the oracle
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`, `fixtures/README.md`).
//! The `manifest.source_sha` this test asserts is PRESENT is recorded provenance — the Python SHA
//! the export ran at — and is compared to nothing. What the exactness buys is unchanged and is
//! REQUIRED by 0021: a frozen fixture is a claim about THIS code not changing its arithmetic
//! unnoticed, so a widened tolerance here is a lost invariant, not a relaxed Python comparison.

use indexmap::IndexMap;
use serde::Deserialize;
use std::path::PathBuf;
use vike_exec::MarkSource;
use vike_exec::{Account, BalanceMode, FillFold};
use vike_model::events::{AccountState, FillEvent, FundingEvent, PositionLiquidated};
use vike_model::{f64_from_hex_bits, f64_to_hex_bits};

#[derive(Deserialize)]
struct Fixture {
    manifest: serde_json::Value,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    venue: String,
    multiplier: String,
    multipliers: IndexMap<String, String>,
    events: Vec<serde_json::Value>,
    expect: Expect,
}

#[derive(Deserialize)]
struct Expect {
    balance: String,
    realized_pnl: String,
    fees_paid: String,
    funding_paid: String,
    balance_mode: String,
    trades: Vec<String>,
    positions: Vec<ExpectPos>,
    unrealized_total: String,
    equity_all: ExpectEquity,
}

#[derive(Deserialize)]
struct ExpectPos {
    venue: String,
    symbol: String,
    position_side: String,
    size: String,
    avg_px: String,
}

#[derive(Deserialize)]
struct ExpectEquity {
    seed: String,
    value: String,
}

fn hexf(v: &serde_json::Value, key: &str) -> f64 {
    f64_from_hex_bits(v[key].as_str().unwrap()).unwrap()
}

fn s(v: &serde_json::Value, key: &str) -> String {
    v[key].as_str().unwrap().to_string()
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r0/account_scenarios.json")
}

#[test]
fn account_bit_parity_vs_frozen_r0_bytes() {
    // ⚠ The message used to end "run scripts/export_r0_fixtures.py". That script was deleted with
    // the Python purge and regeneration is NOT a supported operation (`fixtures/README.md`); the
    // path is kept as the evidence for where these bytes came from, not as an instruction.
    let text = std::fs::read_to_string(fixture_path()).expect(
        "fixtures/r0/account_scenarios.json missing — it is committed and cannot be regenerated \
         (its exporter, scripts/export_r0_fixtures.py, is gone); restore it from git",
    );
    let fx: Fixture = serde_json::from_str(&text).unwrap();
    assert!(fx.manifest.get("source_sha").is_some());

    for sc in &fx.scenarios {
        let mults: IndexMap<String, f64> = sc
            .multipliers
            .iter()
            .map(|(k, v)| (k.clone(), f64_from_hex_bits(v).unwrap()))
            .collect();
        let mut acc = Account::new(
            f64_from_hex_bits(&sc.multiplier).unwrap(),
            &sc.venue,
            if mults.is_empty() { None } else { Some(mults) },
            BalanceMode::Delta,
        );

        for ev in &sc.events {
            match ev["op"].as_str().unwrap() {
                // ⚠ `apply_fill` is now IDEMPOTENT PER `trade_id` (the dedup ledger lives on
                // `Account` — see its module doc), so this ORACLE REPLAY is only faithful while
                // every fixture fill carries a DISTINCT id. Every r0 scenario does today (checked:
                // 120/120 unique in `randomized_multi_symbol`, which is also the one scenario that
                // folds several symbols through ONE account — the fixture's own model of the key).
                // Assert it rather than assume it: a regenerated fixture that ever repeated an id
                // would otherwise silently drop a fill and "prove" parity against a shorter stream.
                "fill" => assert_eq!(
                    acc.apply_fill(&FillEvent {
                        trade_id: vike_model::events::TradeId::new(s(ev, "trade_id"))
                            .expect("every r0 fixture fill carries a non-empty trade_id"),
                        client_order_id: "c1".into(),
                        venue: s(ev, "venue").into(),
                        symbol: s(ev, "symbol").into(),
                        side: ev["side"].as_i64().unwrap() as i32,
                        last_qty: hexf(ev, "qty"),
                        last_px: hexf(ev, "px"),
                        commission: hexf(ev, "commission"),
                        commission_asset: String::new().into(),
                        liquidity_side: String::new().into(),
                        ts: 0,
                        mark_price: None,
                        position_side: s(ev, "position_side").into(),
                    }),
                    FillFold::Applied,
                    "oracle replay requires a DISTINCT trade_id per fixture fill: {}",
                    s(ev, "trade_id")
                ),
                "mark" => {
                    let venue = s(ev, "venue");
                    let symbol = s(ev, "symbol");
                    acc.set_mark_from(&venue, &symbol, hexf(ev, "px"), MarkSource::VenueMark, 0);
                }
                "funding" => acc.apply_funding(&FundingEvent {
                    venue: s(ev, "venue").into(),
                    symbol: s(ev, "symbol").into(),
                    position_side: s(ev, "position_side").into(),
                    funding_rate: 0.0,
                    amount: hexf(ev, "amount"),
                    mark_price: None,
                    ts: 0,
                    route_key: None,
                }),
                "account_state" => {
                    let balances: Vec<(String, f64)> = ev["balances"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| {
                            (
                                p[0].as_str().unwrap().to_string(),
                                f64_from_hex_bits(p[1].as_str().unwrap()).unwrap(),
                            )
                        })
                        .collect();
                    let quote = s(ev, "quote_asset");
                    acc.apply_account_state(
                        &AccountState {
                            venue: s(ev, "venue").into(),
                            balances,
                            ts: 0,
                            route_key: None,
                        },
                        &quote,
                    );
                }
                "liquidation" => acc.apply_liquidation(&PositionLiquidated {
                    venue: s(ev, "venue").into(),
                    symbol: s(ev, "symbol").into(),
                    position_side: s(ev, "position_side").into(),
                    qty: hexf(ev, "qty"),
                    liq_price: hexf(ev, "liq_price"),
                    fee: hexf(ev, "fee"),
                    ts: 0,
                    trade_id: String::new().into(),
                    route_key: None,
                }),
                other => panic!("unknown op {other}"),
            }
        }

        let n = &sc.name;
        let bits = |x: f64| f64_to_hex_bits(x);
        assert_eq!(bits(acc.balance), sc.expect.balance, "{n}: balance");
        assert_eq!(bits(acc.realized_pnl), sc.expect.realized_pnl, "{n}: realized_pnl");
        assert_eq!(bits(acc.fees_paid), sc.expect.fees_paid, "{n}: fees_paid");
        assert_eq!(bits(acc.funding_paid), sc.expect.funding_paid, "{n}: funding_paid");
        let mode = match acc.balance_mode {
            BalanceMode::Delta => "delta",
            BalanceMode::Authoritative => "authoritative",
        };
        assert_eq!(mode, sc.expect.balance_mode, "{n}: balance_mode");
        assert_eq!(acc.closed_pnls.len(), sc.expect.trades.len(), "{n}: trade count");
        for (i, (got, want)) in acc.closed_pnls.iter().zip(&sc.expect.trades).enumerate() {
            assert_eq!(&bits(*got), want, "{n}: trades[{i}]");
        }
        // positions in INSERTION order (IndexMap ↔ Python dict) — order itself is asserted
        assert_eq!(acc.positions.len(), sc.expect.positions.len(), "{n}: position count");
        for (i, ((key, pos), want)) in acc.positions.iter().zip(&sc.expect.positions).enumerate() {
            assert_eq!(key.0, want.venue, "{n}: positions[{i}].venue");
            assert_eq!(key.1, want.symbol, "{n}: positions[{i}].symbol");
            assert_eq!(key.2.to_string(), want.position_side, "{n}: positions[{i}].side");
            assert_eq!(bits(pos.size), want.size, "{n}: positions[{i}].size");
            assert_eq!(bits(pos.avg_px), want.avg_px, "{n}: positions[{i}].avg_px");
        }
        // unrealized: sum in key order, mirroring the exporter's generator expression
        let mut unreal = 0.0;
        for key in acc.positions.keys() {
            unreal += acc.unrealized_of_key(key);
        }
        assert_eq!(bits(unreal), sc.expect.unrealized_total, "{n}: unrealized_total");
        let seed = f64_from_hex_bits(&sc.expect.equity_all.seed).unwrap();
        assert_eq!(bits(acc.equity_all(seed)), sc.expect.equity_all.value, "{n}: equity_all");
    }
}

/// A mis-routed fill (wrong venue) must panic — the Python `assert` twin.
#[test]
#[should_panic(expected = "routed to Account")]
fn apply_fill_asserts_venue() {
    let mut acc = Account::new(1.0, "binance", None, BalanceMode::Delta);
    acc.apply_fill(&FillEvent {
        trade_id: "t1".into(),
        client_order_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    });
}
