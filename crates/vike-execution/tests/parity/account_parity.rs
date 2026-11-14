//! R0 golden parity: `Account` (fold/marks/funding/account-state/liquidation/equity) replayed
//! against the FROZEN `fixtures/r0/account_scenarios.json` bytes. Bit-for-bit on every f64.
//!
//! ⚠ The committed bytes ARE the oracle; their Python exporter is gone
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`, `fixtures/README.md`).
//! The `manifest.source_sha` asserted PRESENT is provenance, compared to nothing. The exactness is
//! REQUIRED by 0021: a frozen fixture is a claim about THIS code not changing its arithmetic
//! unnoticed, so a widened tolerance here is a lost invariant.

use indexmap::IndexMap;
use serde::Deserialize;
use std::path::PathBuf;
use vike_execution::MarkSource;
use vike_execution::{Account, BalanceMode, FillFold};
use vike_model::events::{AccountState, FillEvent, FundingEvent, PositionLiquidated, PositionSide};
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
    // ⚠ Regeneration is NOT a supported operation (`fixtures/README.md`): the message names the
    // deleted exporter as provenance, not as an instruction.
    let text = std::fs::read_to_string(fixture_path()).expect(
        "fixtures/r0/account_scenarios.json missing — it is committed and cannot be regenerated \
         (its exporter, scripts/export_r0_fixtures.py, is gone); restore it from git",
    );
    let fx: Fixture = serde_json::from_str(&text).unwrap();
    assert!(fx.manifest.get("source_sha").is_some());

    for sc in &fx.scenarios {
        let mults: IndexMap<String, vike_model::ContractValue> = sc
            .multipliers
            .iter()
            .map(|(k, v)| {
                (k.clone(), vike_model::ContractValue::linear(f64_from_hex_bits(v).unwrap()))
            })
            .collect();
        let mut account = Account::new(
            f64_from_hex_bits(&sc.multiplier).unwrap(),
            &sc.venue,
            if mults.is_empty() { None } else { Some(mults) },
            BalanceMode::Delta,
        );

        // The two sums the `fees_paid` fence below is stated over (decision 0021, third instance):
        // the fixture's fill commissions and its forced-close fees, each in event order.
        let (mut fill_fees, mut liquidation_fees) = (0.0_f64, 0.0_f64);

        for event in &sc.events {
            match event["op"].as_str().unwrap() {
                // ⚠ `apply_fill` is IDEMPOTENT PER `trade_id` (the dedup ledger on `Account`), so
                // this ORACLE REPLAY is faithful only while every fixture fill carries a DISTINCT
                // id (120/120 unique in `randomized_multi_symbol`, the one scenario folding several
                // symbols through ONE account). Asserted, not assumed: a repeated id would silently
                // drop a fill and "prove" parity against a shorter stream.
                "fill" => {
                    fill_fees += hexf(event, "commission");
                    assert_eq!(
                        account.apply_fill(&FillEvent {
                            trade_id: vike_model::events::TradeId::new(s(event, "trade_id"))
                                .expect("every r0 fixture fill carries a non-empty trade_id"),
                            order_client_id: "c1".into(),
                            venue: s(event, "venue").into(),
                            symbol: s(event, "symbol").into(),
                            side: event["side"].as_i64().unwrap() as i32,
                            last_qty: hexf(event, "qty"),
                            last_price: hexf(event, "px"),
                            commission: hexf(event, "commission"),
                            commission_asset: String::new().into(),
                            liquidity_side: String::new().into(),
                            timestamp: 0,
                            mark_price: None,
                            position_side: PositionSide::from_word_or_both(&s(
                                event,
                                "position_side"
                            )),
                        }),
                        FillFold::Applied,
                        "oracle replay requires a DISTINCT trade_id per fixture fill: {}",
                        s(event, "trade_id")
                    )
                }
                "mark" => {
                    let venue = s(event, "venue");
                    let symbol = s(event, "symbol");
                    account.set_mark_from(
                        &venue,
                        &symbol,
                        hexf(event, "px"),
                        MarkSource::VenueMark,
                        0,
                    );
                }
                "funding" => account.apply_funding(&FundingEvent {
                    venue: s(event, "venue").into(),
                    symbol: s(event, "symbol").into(),
                    position_side: PositionSide::from_word_or_both(&s(event, "position_side")),
                    funding_rate: 0.0,
                    amount: hexf(event, "amount"),
                    mark_price: None,
                    timestamp: 0,
                    route_key: None,
                    currency: None,
                }),
                "account_state" => {
                    let balances: Vec<(String, f64)> = event["balances"]
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
                    let quote = s(event, "quote_asset");
                    account.apply_account_state(
                        &AccountState {
                            venue: s(event, "venue").into(),
                            balances,
                            timestamp: 0,
                            route_key: None,
                        },
                        &quote,
                    );
                }
                "liquidation" => {
                    // A liquidation of a FLAT position is a true no-op that charges nothing (the
                    // scenario's last event is one): the fee is owed only while the position is open.
                    let key: (ustr::Ustr, ustr::Ustr, PositionSide) = (
                        s(event, "venue").as_str().into(),
                        s(event, "symbol").as_str().into(),
                        PositionSide::from_word_or_both(&s(event, "position_side")),
                    );
                    if account.positions.get(&key).is_some_and(|p| p.size != 0.0) {
                        liquidation_fees += hexf(event, "fee");
                    }
                    account.apply_liquidation(&PositionLiquidated {
                        venue: s(event, "venue").into(),
                        symbol: s(event, "symbol").into(),
                        position_side: PositionSide::from_word_or_both(&s(event, "position_side")),
                        qty: hexf(event, "qty"),
                        liquidation_price: hexf(event, "liq_price"),
                        fee: hexf(event, "fee"),
                        timestamp: 0,
                        trade_id: String::new().into(),
                        route_key: None,
                        fee_currency: None,
                    })
                }
                other => panic!("unknown op {other}"),
            }
        }

        let n = &sc.name;
        let bits = |x: f64| f64_to_hex_bits(x);
        assert_eq!(bits(account.balance), sc.expect.balance, "{n}: balance");
        assert_eq!(bits(account.realized_pnl), sc.expect.realized_pnl, "{n}: realized_pnl");
        // ⚠ THE FENCE on `fees_paid` (decision 0021, the third instance). The frozen bytes were
        // exported while a forced-close fee reached `balance` and NOT `fees_paid`; this port now
        // books it into both (FEES-AUDIT 2026-10-26 §4.8 a), so for the one scenario that
        // liquidates the oracle's figure is no longer the whole of ours. The comparison stays
        // EXACT, against a stated function of the oracle's own values: its `fees_paid` plus the
        // scenario's forced-close fees, summed in event order. Four guards keep that a fence and
        // not a widened tolerance: (1) bits against bits; (2) the oracle's raw `fees_paid` is
        // still gated, equal to the sum of the scenario's fill commissions, so the bytes still
        // claim what they always did; (3) a control fails if the relaxation is a no-op, i.e. the
        // liquidating scenario really does differ from the raw bytes, and every other scenario
        // compares the raw bytes unchanged; (4) the committed fixture bytes are untouched.
        assert_eq!(
            bits(fill_fees),
            sc.expect.fees_paid,
            "{n}: the oracle's fees_paid is its fills'"
        );
        let oracle_fees = f64_from_hex_bits(&sc.expect.fees_paid).unwrap();
        assert_eq!(
            bits(account.fees_paid),
            bits(oracle_fees + liquidation_fees),
            "{n}: fees_paid = the oracle's + this scenario's forced-close fees"
        );
        if sc.name == "liquidation_partial_full_replay" {
            assert!(
                liquidation_fees > 0.0,
                "{n}: the control scenario must really liquidate with a fee"
            );
            assert_ne!(
                bits(account.fees_paid),
                sc.expect.fees_paid,
                "{n}: the fence is a no-op here, so it proves nothing"
            );
        } else {
            assert_eq!(
                liquidation_fees, 0.0,
                "{n}: only the liquidation scenario relaxes fees_paid"
            );
        }
        assert_eq!(bits(account.funding_paid), sc.expect.funding_paid, "{n}: funding_paid");
        let mode = match account.balance_mode {
            BalanceMode::Delta => "delta",
            BalanceMode::Authoritative => "authoritative",
        };
        assert_eq!(mode, sc.expect.balance_mode, "{n}: balance_mode");
        assert_eq!(account.closed_pnls.len(), sc.expect.trades.len(), "{n}: trade count");
        for (i, (got, want)) in account.closed_pnls.iter().zip(&sc.expect.trades).enumerate() {
            assert_eq!(&bits(*got), want, "{n}: trades[{i}]");
        }
        // positions in INSERTION order (IndexMap ↔ Python dict) — order itself is asserted
        assert_eq!(account.positions.len(), sc.expect.positions.len(), "{n}: position count");
        for (i, ((key, pos), want)) in
            account.positions.iter().zip(&sc.expect.positions).enumerate()
        {
            assert_eq!(key.0, want.venue, "{n}: positions[{i}].venue");
            assert_eq!(key.1, want.symbol, "{n}: positions[{i}].symbol");
            assert_eq!(key.2.to_string(), want.position_side, "{n}: positions[{i}].side");
            assert_eq!(bits(pos.size), want.size, "{n}: positions[{i}].size");
            assert_eq!(bits(pos.average_price), want.avg_px, "{n}: positions[{i}].avg_px");
        }
        // unrealized: sum in key order, mirroring the exporter's generator expression
        let mut unreal = 0.0;
        for key in account.positions.keys() {
            unreal += account.unrealized_of_key(key);
        }
        assert_eq!(bits(unreal), sc.expect.unrealized_total, "{n}: unrealized_total");
        let seed = f64_from_hex_bits(&sc.expect.equity_all.seed).unwrap();
        assert_eq!(bits(account.equity_all(seed)), sc.expect.equity_all.value, "{n}: equity_all");
    }
}

/// A mis-routed fill (wrong venue) is REFUSED, never a panic: `apply_fill` runs on the core's fold
/// thread, where no invariant violation may take the process down (its own doc says so). This was
/// `apply_fill_asserts_venue`, `#[should_panic]`, the twin of a Python `assert` whose oracle is
/// retired (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). What it pinned was
/// "a wrong-venue fill does not fold", and that is kept EXACTLY: nothing on the account moves, bit
/// for bit. The control is the same trade id arriving on the RIGHT venue, which must still fold: a
/// refusal must not burn the id.
#[test]
fn apply_fill_refuses_a_fill_of_another_venue_and_moves_nothing() {
    let fill = |venue: &str| FillEvent {
        trade_id: "t1".into(),
        order_client_id: "c1".into(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.5,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let mut account = Account::new(1.0, "binance", None, BalanceMode::Delta);
    let (balance, fees) = (account.balance.to_bits(), account.fees_paid.to_bits());

    assert_eq!(
        account.apply_fill(&fill("bybit")),
        FillFold::ForeignVenue,
        "refused, not folded and not a panic"
    );
    assert!(account.positions.is_empty(), "no position was opened");
    assert_eq!(account.balance.to_bits(), balance, "the commission was not charged");
    assert_eq!(account.fees_paid.to_bits(), fees);
    assert_eq!(account.duplicate_fills_refused, 0, "a wrong venue is not a duplicate");

    assert_eq!(
        account.apply_fill(&fill("binance")),
        FillFold::Applied,
        "control: the id was not burned"
    );
    assert_eq!(account.positions.len(), 1);
}
