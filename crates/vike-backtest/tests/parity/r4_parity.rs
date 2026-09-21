//! R4 golden gate against the FROZEN `fixtures/r4/mse_runs.json` bytes: the StrategyEngine
//! running the reference strategies — full trade lists (incl. MAE/MFE + symbols), equity
//! curves, per-symbol PnL, dropped-order diagnostics, and the ledger-mirror Account fold.
//! Bit-for-bit on every f64.
//!
//! ⚠ This line read "R4 golden parity vs the Python oracle" until 2026-08-28. The fixture's
//! PROVENANCE is unchanged and is why `load` still names the exporter — but that exporter was
//! deleted by `751de662`, so the committed bytes ARE the oracle and nothing here compares against
//! a running Python. What the assertions below prove is that this engine has not changed its
//! arithmetic unnoticed; see `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`
//! and `crates/vike-backtest/tests/parity.rs`'s `r4_parity` bullet. The comments further down that
//! reason about what the EXPORTER did (its liquidation model, its inline event ordering) are
//! statements about how these bytes were produced, and stay true as such.

use serde_json::Value;
use std::path::PathBuf;
use vike_backtest::ref_strategies::{
    BracketPerSymbol, CapsSizersMask, GatedWeights, RotationTopK, TickPairMse,
};
use vike_backtest::sizing::{DrawdownThrottleSizer, PctEquitySizer};
use vike_backtest::{BacktestResult, EngineParams, FillModelKind, StrategyEngine, Tick};
use vike_exec::MarkSource;
use vike_exec::{Account, BalanceMode};
use vike_model::events::{FillEvent, FundingEvent, TradeId};
use vike_model::{Bar, QuoteTick, f64_from_hex_bits, f64_to_hex_bits};

fn load() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r4/mse_runs.json");
    let text = std::fs::read_to_string(&path).expect(
        "fixtures/r4/mse_runs.json missing — exported by scripts/export_r4_fixtures.py, removed \
         by 751de662's Python purge",
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["manifest"]["source_sha"].is_string());
    v
}

fn f(v: &Value) -> f64 {
    f64_from_hex_bits(v.as_str().unwrap()).unwrap()
}

fn parse_bar(v: &Value) -> Bar {
    Bar {
        ts: v["ts"].as_i64().unwrap(),
        open: f(&v["open"]),
        high: f(&v["high"]),
        low: f(&v["low"]),
        close: f(&v["close"]),
        volume: f(&v["volume"]),
        funding: if v["funding"].is_null() { None } else { Some(f(&v["funding"])) },
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn parse_bars_by_symbol(run: &Value) -> Vec<(String, Vec<Bar>)> {
    run["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let sym = s.as_str().unwrap().to_string();
            let bars = run["bars"][&sym].as_array().unwrap().iter().map(parse_bar).collect();
            (sym, bars)
        })
        .collect()
}

fn assert_result(name: &str, got: &BacktestResult, want: &Value) {
    assert_eq!(
        f64_to_hex_bits(got.final_equity),
        want["final_equity"].as_str().unwrap(),
        "{name}: final_equity (got {})",
        got.final_equity
    );
    assert_eq!(
        got.intrabar_both_hit as i64,
        want["intrabar_both_hit"].as_i64().unwrap(),
        "{name}: intrabar_both_hit"
    );
    let want_curve = want["equity_curve"].as_array().unwrap();
    assert_eq!(got.equity_curve.len(), want_curve.len(), "{name}: curve len");
    for (i, (g, w)) in got.equity_curve.iter().zip(want_curve).enumerate() {
        assert_eq!(&f64_to_hex_bits(*g), w.as_str().unwrap(), "{name}: curve[{i}] (got {g})");
    }
    let want_ts = want["equity_ts"].as_array().unwrap();
    for (i, (g, w)) in got.equity_ts.iter().zip(want_ts).enumerate() {
        assert_eq!(*g, w.as_i64().unwrap(), "{name}: equity_ts[{i}]");
    }
    for (sym, pnl) in &got.per_symbol_pnl {
        assert_eq!(
            &f64_to_hex_bits(*pnl),
            want["per_symbol_pnl"][sym].as_str().unwrap(),
            "{name}: per_symbol_pnl[{sym}]"
        );
    }
    let want_trades = want["trades"].as_array().unwrap();
    assert_eq!(got.trades.len(), want_trades.len(), "{name}: trade count");
    for (i, (t, w)) in got.trades.iter().zip(want_trades).enumerate() {
        for (field, val) in [
            ("entry_price", t.entry_price),
            ("exit_price", t.exit_price),
            ("size", t.size),
            ("pnl", t.pnl),
            ("fees", t.fees),
            ("mae", t.mae),
            ("mfe", t.mfe),
        ] {
            assert_eq!(
                f64_to_hex_bits(val),
                w[field].as_str().unwrap(),
                "{name}: trades[{i}].{field} (got {val})"
            );
        }
        assert_eq!(t.entry_ts, w["entry_ts"].as_i64().unwrap(), "{name}: trades[{i}].entry_ts");
        assert_eq!(t.exit_ts, w["exit_ts"].as_i64().unwrap(), "{name}: trades[{i}].exit_ts");
        assert_eq!(t.symbol, w["symbol"].as_str().unwrap(), "{name}: trades[{i}].symbol");
    }
}

fn assert_dropped(name: &str, got: &[(String, String, f64, f64)], want: &Value) {
    let want = want.as_array().unwrap();
    assert_eq!(got.len(), want.len(), "{name}: dropped count");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        let w = w.as_array().unwrap();
        assert_eq!(g.0, w[0].as_str().unwrap(), "{name}: dropped[{i}].symbol");
        assert_eq!(g.1, w[1].as_str().unwrap(), "{name}: dropped[{i}].kind");
        assert_eq!(f64_to_hex_bits(g.2), w[2].as_str().unwrap(), "{name}: dropped[{i}].size");
        assert_eq!(f64_to_hex_bits(g.3), w[3].as_str().unwrap(), "{name}: dropped[{i}].weight");
    }
}

#[test]
fn mse_runs_bit_parity() {
    let fx = load();
    for run in fx["runs"].as_array().unwrap() {
        let name = run["name"].as_str().unwrap();
        let p = &run["params"];
        match name {
            "rotation" => {
                let params = EngineParams {
                    fee_rate: f(&p["fee_rate"]),
                    slippage: f(&p["slippage"]),
                    cash: f(&p["cash"]),
                    ..Default::default()
                };
                let mut eng =
                    StrategyEngine::new(parse_bars_by_symbol(run), RotationTopK::default(), params);
                let got = eng.run();
                assert_result(name, &got, &run["result"]);
            }
            "brackets_granular" => {
                let granular = vec![(
                    "B".to_string(),
                    run["granular"]["B"].as_array().unwrap().iter().map(parse_bar).collect(),
                )];
                let params = EngineParams {
                    maker_fee: Some(f(&p["maker_fee"])),
                    taker_fee: Some(f(&p["taker_fee"])),
                    slippage: f(&p["slippage"]),
                    cash: f(&p["cash"]),
                    granular_by_symbol: granular,
                    ..Default::default()
                };
                let mut eng =
                    StrategyEngine::new(parse_bars_by_symbol(run), BracketPerSymbol, params);
                let got = eng.run();
                assert_result(name, &got, &run["result"]);
            }
            "gated" => {
                let params = EngineParams {
                    fee_rate: f(&p["fee_rate"]),
                    slippage: f(&p["slippage"]),
                    cash: f(&p["cash"]),
                    cash_gate: true,
                    ..Default::default()
                };
                let mut eng = StrategyEngine::new(parse_bars_by_symbol(run), GatedWeights, params);
                let got = eng.run();
                assert_result(name, &got, &run["result"]);
                assert_dropped(name, &eng.core.dropped, &run["dropped"]);
            }
            "caps_sizers_mask" => {
                let n = run["bars"]["M0"].as_array().unwrap().len();
                let (off0, off1) = (
                    p["mask_off"][0].as_u64().unwrap() as usize,
                    p["mask_off"][1].as_u64().unwrap() as usize,
                );
                let mask_sym = p["mask_symbol"].as_str().unwrap().to_string();
                let symbols: Vec<String> = run["symbols"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect();
                let active_mask: Vec<(String, Vec<bool>)> = symbols
                    .iter()
                    .map(|s| {
                        let m: Vec<bool> =
                            (0..n).map(|i| !(*s == mask_sym && i >= off0 && i < off1)).collect();
                        (s.clone(), m)
                    })
                    .collect();
                let sizer = DrawdownThrottleSizer {
                    base: Box::new(PctEquitySizer { pct: f(&p["sizer"]["pct"]) }),
                    sensitivity: f(&p["sizer"]["sensitivity"]),
                    floor: f(&p["sizer"]["floor"]),
                };
                let params = EngineParams {
                    fee_rate: f(&p["fee_rate"]),
                    slippage: f(&p["slippage"]),
                    cash: f(&p["cash"]),
                    maint_margin: f(&p["maint_margin"]),
                    // The Python exporter that produced this fixture computed the WIPE model
                    // (whole-account adverse-intrabar liquidation), so a replay must opt into it
                    // to reproduce the committed bytes. Today's caps_sizers_mask scenario never
                    // breaches, so both paths agree and the flag is currently inert.
                    // ⚠ This used to justify itself as keeping "a future re-export that DOES
                    // breach" on the oracle's model. There will be no re-export: every fixture
                    // exporter went with `751de662` and fixtures/r4/ is frozen. The flag stays
                    // because it is the model these bytes were computed under — flip it and this
                    // gate reddens for a reason that says nothing about the engine.
                    venue_style_liquidation: true,
                    max_open_positions: p["max_open_positions"].as_u64().unwrap() as usize,
                    max_open_long: p["max_open_long"].as_u64().unwrap() as usize,
                    volume_limit: Some(f(&p["volume_limit"])),
                    multipliers: vec![
                        ("M0".to_string(), f(&p["multipliers"]["M0"])),
                        ("M1".to_string(), f(&p["multipliers"]["M1"])),
                    ],
                    active_mask: Some(active_mask),
                    sizer: Some(Box::new(sizer)),
                    ..Default::default()
                };
                let mut eng =
                    StrategyEngine::new(parse_bars_by_symbol(run), CapsSizersMask, params);
                let got = eng.run();
                assert_result(name, &got, &run["result"]);
                assert_dropped(name, &eng.core.dropped, &run["dropped"]);
            }
            "tick_pair" => {
                let symbols: Vec<String> = run["symbols"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect();
                let ticks: Vec<(String, Vec<Tick>)> = symbols
                    .iter()
                    .map(|s| {
                        let t: Vec<Tick> = run["quotes"][s]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|q| {
                                Tick::Quote(QuoteTick {
                                    ts: q["ts"].as_i64().unwrap(),
                                    local_ts: 0,
                                    bid: f(&q["bid"]),
                                    ask: f(&q["ask"]),
                                    bid_size: 1.0,
                                    ask_size: 1.0,
                                    symbol: q["symbol"].as_str().unwrap().to_string(),
                                })
                            })
                            .collect();
                        (s.clone(), t)
                    })
                    .collect();
                let params = EngineParams {
                    fee_rate: f(&p["fee_rate"]),
                    cash: f(&p["cash"]),
                    fill_model: FillModelKind::Tick,
                    ..Default::default()
                };
                let bars_by_symbol: Vec<(String, Vec<Bar>)> =
                    symbols.iter().map(|s| (s.clone(), Vec::new())).collect();
                let mut eng = StrategyEngine::new(bars_by_symbol, TickPairMse::default(), params);
                let got = eng.run_ticks(&ticks);
                assert_result(name, &got, &run["result"]);
            }
            "ledger_mirror" => {
                let params = EngineParams {
                    fee_rate: f(&p["fee_rate"]),
                    slippage: f(&p["slippage"]),
                    cash: f(&p["cash"]),
                    mirror: true,
                    ..Default::default()
                };
                let mut eng =
                    StrategyEngine::new(parse_bars_by_symbol(run), RotationTopK::default(), params);
                let got = eng.run();
                assert_result(name, &got, &run["result"]);

                // drain the mirror into an Account with the exporter's exact mapping
                let mut acc = Account::new(1.0, "sim", None, BalanceMode::Delta);
                let fills = eng.core.mirror_fills.take().unwrap();
                let funding = eng.core.mirror_funding.take().unwrap();
                // interleave in the ORDER events occurred: Python fires them inline; here the
                // two sinks preserve per-kind order and funding always happens after the fill
                // phase of its bar, so replay fills-then-funding per ts is NOT enough — replay
                // by merged sequence: reconstruct via ts + kind ordering.
                // Simpler and exact: Python's Account is commutative-enough ONLY in exact
                // event order, so we merge on (ts, arrival index) with fills-before-funding
                // within a bar (the engine fires fills in the fill phase, funding in the mark
                // phase of the SAME bar).
                let mut fi = 0usize;
                let mut gi = 0usize;
                let mut counter = 0u64;
                while fi < fills.len() || gi < funding.len() {
                    let take_fill = if fi >= fills.len() {
                        false
                    } else if gi >= funding.len() {
                        true
                    } else {
                        fills[fi].ts <= funding[gi].ts
                    };
                    if take_fill {
                        let mf = &fills[fi];
                        counter += 1;
                        acc.apply_fill(&FillEvent {
                            // minted by this harness — same `m<counter>` bytes as before, and the
                            // per-fill uniqueness the account's dedup needs is unchanged
                            trade_id: TradeId::prefixed("m", counter),
                            client_order_id: String::new(),
                            venue: "sim".into(),
                            symbol: mf.symbol.clone().into(),
                            side: mf.side,
                            last_qty: mf.size,
                            last_px: mf.price,
                            commission: mf.fee,
                            commission_asset: String::new().into(),
                            liquidity_side: if mf.is_maker {
                                "maker".into()
                            } else {
                                "taker".into()
                            },
                            ts: mf.ts,
                            mark_price: None,
                            position_side: "BOTH".into(),
                        });
                        fi += 1;
                    } else {
                        let g = &funding[gi];
                        acc.apply_funding(&FundingEvent {
                            venue: "sim".into(),
                            symbol: g.symbol.clone().into(),
                            position_side: "BOTH".into(),
                            funding_rate: 0.0,
                            amount: g.amount,
                            mark_price: None,
                            ts: g.ts,
                            route_key: None,
                        });
                        gi += 1;
                    }
                }
                for (si, sym) in eng.core.symbols.iter().enumerate() {
                    acc.set_mark_from("sim", sym, eng.core.sym[si].price, MarkSource::VenueMark, 0);
                }
                let want = &run["account"];
                assert_eq!(counter as i64, want["n_fills"].as_i64().unwrap(), "mirror: n_fills");
                assert_eq!(
                    f64_to_hex_bits(acc.balance),
                    want["balance"].as_str().unwrap(),
                    "mirror: balance"
                );
                assert_eq!(
                    f64_to_hex_bits(acc.realized_pnl),
                    want["realized_pnl"].as_str().unwrap(),
                    "mirror: realized_pnl"
                );
                assert_eq!(
                    f64_to_hex_bits(acc.fees_paid),
                    want["fees_paid"].as_str().unwrap(),
                    "mirror: fees_paid"
                );
                assert_eq!(
                    f64_to_hex_bits(acc.funding_paid),
                    want["funding_paid"].as_str().unwrap(),
                    "mirror: funding_paid"
                );
                assert_eq!(
                    f64_to_hex_bits(acc.equity_all(f(&p["cash"]))),
                    want["equity_all_seed"].as_str().unwrap(),
                    "mirror: equity_all"
                );
                let want_pos = want["positions"].as_array().unwrap();
                assert_eq!(acc.positions.len(), want_pos.len(), "mirror: position count");
                for (i, ((key, pos), w)) in acc.positions.iter().zip(want_pos).enumerate() {
                    assert_eq!(
                        key.1,
                        w["symbol"].as_str().unwrap(),
                        "mirror: positions[{i}].symbol"
                    );
                    assert_eq!(
                        f64_to_hex_bits(pos.size),
                        w["size"].as_str().unwrap(),
                        "mirror: positions[{i}].size"
                    );
                    assert_eq!(
                        f64_to_hex_bits(pos.avg_px),
                        w["avg_px"].as_str().unwrap(),
                        "mirror: positions[{i}].avg_px"
                    );
                }
            }
            other => panic!("unknown run {other}"),
        }
    }
}
