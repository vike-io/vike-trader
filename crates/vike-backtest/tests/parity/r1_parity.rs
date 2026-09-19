//! R1 golden gate against the FROZEN `fixtures/r1/*` bytes: broker_sim scalars, order
//! fill-triggers (+ trailing ratchet), granular sub-bar resolution, fill models,
//! intrabar resolution, tick consolidators. Bit-for-bit on every f64.
//!
//! ⚠ This line read "R1 golden parity vs the Python oracle" until 2026-08-28, which reads as an
//! ongoing cross-implementation check. It is not one. The fixtures' PROVENANCE is unchanged and
//! still worth knowing — they were exported from the Python app, and `manifest.source_sha` pins
//! the SHA that exported them — but every exporter was deleted by `751de662`, so the committed
//! bytes ARE the oracle and nothing here compares against a running Python. What the assertions
//! below actually prove: this code has not changed its arithmetic unnoticed.
//! `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is the verdict, and
//! `crates/vike-backtest/tests/parity.rs`'s `r1_parity` bullet carries why these particular bits
//! are also platform-portable.

use serde_json::Value;
use std::path::PathBuf;
use vike_backtest::{
    BarFillModel, FillModel, TickFillModel, adverse_fill_price, consolidate_quotes,
    consolidate_trades, fee, funding_charge, resolve_intrabar_fills,
};
use vike_model::{
    Bar, OrderKind, QuoteTick, TradeTick, WorkingOrder, f64_from_hex_bits, f64_to_hex_bits,
    order_fill_price, order_fill_price_granular,
};

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r1").join(name);
    let text = std::fs::read_to_string(&path)
        // ⚠ Not "run the exporter": scripts/export_r1_fixtures.py was deleted by `751de662` and
        // these bytes cannot be regenerated. A miss here is a deleted or moved fixture, not a
        // stale one. The dead path is NAMED anyway — it is the evidence for where the bytes came
        // from, and `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS` carries it.
        .unwrap_or_else(|_| panic!("{name} missing — exported by scripts/export_r1_fixtures.py"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["manifest"]["source_sha"].is_string(), "manifest must record the oracle SHA");
    v
}

fn f(v: &Value) -> f64 {
    f64_from_hex_bits(v.as_str().unwrap()).unwrap()
}

fn f_opt(v: &Value) -> Option<f64> {
    if v.is_null() { None } else { Some(f(v)) }
}

fn bits_opt(x: Option<f64>) -> Option<String> {
    x.map(f64_to_hex_bits)
}

fn want_opt(v: &Value) -> Option<String> {
    v.as_str().map(|s| s.to_string())
}

fn parse_order(v: &Value) -> WorkingOrder {
    let kind: OrderKind = serde_json::from_value(v["kind"].clone()).unwrap();
    WorkingOrder {
        kind,
        side: v["side"].as_i64().unwrap() as i32,
        size: f(&v["size"]),
        price: f_opt(&v["price"]),
        trail: f_opt(&v["trail"]),
        extreme: f_opt(&v["extreme"]),
        weight: f(&v["weight"]),
        stop: f_opt(&v["stop"]),
        // engine-local identity handle; never exported by the oracle, never read here
        qid: 0,
    }
}

fn parse_bar(v: &Value) -> Bar {
    Bar {
        ts: v["ts"].as_i64().unwrap(),
        open: f(&v["open"]),
        high: f(&v["high"]),
        low: f(&v["low"]),
        close: f(&v["close"]),
        volume: f(&v["volume"]),
        funding: None,
        bid: f_opt(&v["bid"]),
        ask: f_opt(&v["ask"]),
        symbol: None,
    }
}

#[test]
fn broker_sim_bit_parity() {
    let fx = load("broker_sim.json");
    for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let a = &c["adverse"];
        let got =
            adverse_fill_price(f(&a["raw"]), a["side"].as_i64().unwrap() as i32, f(&a["slippage"]));
        assert_eq!(f64_to_hex_bits(got), a["out"].as_str().unwrap(), "case {i}: adverse");
        let fe = &c["fee"];
        let got = fee(f(&fe["size"]), f(&fe["price"]), f(&fe["rate"]), f(&fe["mult"]));
        assert_eq!(f64_to_hex_bits(got), fe["out"].as_str().unwrap(), "case {i}: fee");
        let fu = &c["funding"];
        let got = funding_charge(f(&fu["pos"]), f(&fu["mark"]), f(&fu["rate"]), f(&fu["mult"]));
        assert_eq!(f64_to_hex_bits(got), fu["out"].as_str().unwrap(), "case {i}: funding");
    }
}

#[test]
fn order_fill_price_bit_parity() {
    let fx = load("order_fill_price.json");
    for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let mut o = parse_order(&c["order"]);
        let bar = parse_bar(&c["bar"]);
        let fp = order_fill_price(&mut o, &bar);
        assert_eq!(bits_opt(fp), want_opt(&c["out"]["fill"]), "case {i}: fill");
        assert_eq!(bits_opt(o.extreme), want_opt(&c["out"]["extreme_after"]), "case {i}: extreme");
    }
    // trailing multi-bar ratchet sequences: the extreme must evolve identically bar-by-bar
    for (i, s) in fx["trailing_seqs"].as_array().unwrap().iter().enumerate() {
        let mut o = parse_order(&s["order"]);
        let bars: Vec<Bar> = s["bars"].as_array().unwrap().iter().map(parse_bar).collect();
        for (j, step) in s["steps"].as_array().unwrap().iter().enumerate() {
            let fp = order_fill_price(&mut o, &bars[j]);
            assert_eq!(bits_opt(fp), want_opt(&step["fill"]), "seq {i} step {j}: fill");
            assert_eq!(
                bits_opt(o.extreme),
                want_opt(&step["extreme_after"]),
                "seq {i} step {j}: extreme"
            );
        }
    }
}

#[test]
fn order_fill_price_granular_bit_parity() {
    let fx = load("order_fill_price_granular.json");
    for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let mut o = parse_order(&c["order"]);
        let subs: Vec<Bar> = c["sub_bars"].as_array().unwrap().iter().map(parse_bar).collect();
        let res = order_fill_price_granular(&mut o, &subs);
        assert_eq!(bits_opt(res.map(|r| r.0)), want_opt(&c["out"]["fill"]), "case {i}: fill");
        assert_eq!(res.map(|r| r.1 as i64), c["out"]["index"].as_i64(), "case {i}: sub index");
        assert_eq!(bits_opt(o.extreme), want_opt(&c["out"]["extreme_after"]), "case {i}: extreme");
    }
}

#[test]
fn fill_models_bit_parity() {
    let fx = load("fill_models.json");
    let bar_model = BarFillModel;
    let tick_model = TickFillModel;
    for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let mut o = parse_order(&c["order"]);
        let bar = parse_bar(&c["bar"]);
        let fp = match c["model"].as_str().unwrap() {
            "bar" => bar_model.fill_price(&mut o, &bar),
            _ => tick_model.fill_price(&mut o, &bar),
        };
        assert_eq!(bits_opt(fp), want_opt(&c["out"]["fill"]), "case {i}: fill");
        assert_eq!(bits_opt(o.extreme), want_opt(&c["out"]["extreme_after"]), "case {i}: extreme");
    }
}

#[test]
fn fill_resolution_bit_parity() {
    let fx = load("fill_resolution.json");
    for (i, c) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let triggered: Vec<(WorkingOrder, f64)> = c["triggered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (parse_order(&t["order"]), f(&t["fill_price"])))
            .collect();
        let (resolved, both_hit) = resolve_intrabar_fills(triggered, f(&c["position_size"]));
        let want = c["out"]["resolved"].as_array().unwrap();
        assert_eq!(resolved.len(), want.len(), "case {i}: count");
        assert_eq!(both_hit as i64, c["out"]["both_hit"].as_i64().unwrap(), "case {i}: both_hit");
        for (j, ((o, fp), w)) in resolved.iter().zip(want).enumerate() {
            let kind: OrderKind = serde_json::from_value(w["kind"].clone()).unwrap();
            assert_eq!(o.kind, kind, "case {i}[{j}]: kind (order of resolution)");
            assert_eq!(o.side as i64, w["side"].as_i64().unwrap(), "case {i}[{j}]: side");
            assert_eq!(f64_to_hex_bits(o.size), w["size"].as_str().unwrap(), "case {i}[{j}]: size");
            assert_eq!(
                f64_to_hex_bits(*fp),
                w["fill_price"].as_str().unwrap(),
                "case {i}[{j}]: fill_price"
            );
        }
    }
}

#[test]
fn consolidators_bit_parity() {
    let fx = load("consolidators.json");
    for (i, s) in fx["scenarios"].as_array().unwrap().iter().enumerate() {
        let step = s["step_ms"].as_i64().unwrap();
        let quotes: Vec<QuoteTick> = s["quotes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| QuoteTick {
                ts: q["ts"].as_i64().unwrap(),
                local_ts: 0,
                bid: f(&q["bid"]),
                ask: f(&q["ask"]),
                bid_size: f(&q["bid_size"]),
                ask_size: f(&q["ask_size"]),
                symbol: String::new(),
            })
            .collect();
        let trades: Vec<TradeTick> = s["trades"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| TradeTick {
                ts: t["ts"].as_i64().unwrap(),
                local_ts: 0,
                price: f(&t["price"]),
                size: f(&t["size"]),
                is_buyer_maker: t["is_buyer_maker"].as_bool().unwrap(),
                symbol: String::new(),
            })
            .collect();
        for (which, got, want) in [
            ("quote", consolidate_quotes(&quotes, step), s["quote_bars"].as_array().unwrap()),
            ("trade", consolidate_trades(&trades, step), s["trade_bars"].as_array().unwrap()),
        ] {
            assert_eq!(got.len(), want.len(), "scenario {i}: {which} bar count");
            for (j, (g, w)) in got.iter().zip(want).enumerate() {
                assert_eq!(g.ts, w["ts"].as_i64().unwrap(), "scenario {i} {which}[{j}]: ts");
                for (field, val) in [
                    ("open", g.open),
                    ("high", g.high),
                    ("low", g.low),
                    ("close", g.close),
                    ("volume", g.volume),
                ] {
                    assert_eq!(
                        f64_to_hex_bits(val),
                        w[field].as_str().unwrap(),
                        "scenario {i} {which}[{j}]: {field}"
                    );
                }
                assert_eq!(bits_opt(g.bid), want_opt(&w["bid"]), "scenario {i} {which}[{j}]: bid");
                assert_eq!(bits_opt(g.ask), want_opt(&w["ask"]), "scenario {i} {which}[{j}]: ask");
            }
        }
    }
}
