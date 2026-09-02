//! R3 golden gate against the FROZEN `fixtures/r3/*` bytes: the (T×S) portfolio kernel
//! (VectorBacktestEngine) — bit-for-bit.
//!
//! ⚠ This line read "R3 golden parity vs the Python oracle" until 2026-08-28. The fixtures'
//! PROVENANCE is unchanged — exported from the Python app, `manifest.source_sha` pinning the SHA
//! that exported them — but the exporters were deleted by `751de662`, so the committed bytes ARE
//! the oracle and nothing here compares against a running Python. What the assertions below prove
//! is that this kernel has not changed its arithmetic unnoticed; see
//! `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` and
//! `crates/vike-backtest/tests/parity.rs`'s `r3_parity` bullet.
//!
//! Note: the single-asset fastsim kernel + engine↔kernel reconciliation were retired along with
//! `fast_backtest` / `SingleSymbolEngine`; only the portfolio (weight) kernel remains.

use serde_json::Value;
use std::path::PathBuf;
use vike_backtest::{fast_portfolio_backtest, Matrix};
use vike_model::{f64_from_hex_bits, f64_to_hex_bits};

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r3").join(name);
    let text = std::fs::read_to_string(&path)
        // ⚠ Not "run the exporter": scripts/export_r3_fixtures.py was deleted by `751de662` and
        // these bytes cannot be regenerated. The dead path stays NAMED because it is the evidence
        // for where the bytes came from — `crates/vike-ops/tests/citation_gate.rs`'s
        // `DEAD_PATH_EXCEPTIONS` carries the matching row.
        .unwrap_or_else(|_| panic!("{name} missing — exported by scripts/export_r3_fixtures.py"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v["manifest"]["source_sha"].is_string());
    v
}

fn f(v: &Value) -> f64 {
    f64_from_hex_bits(v.as_str().unwrap()).unwrap()
}

fn assert_trades_curve(
    name: &str,
    curve: &[f64],
    trades: &[vike_model::Trade],
    n_trades: usize,
    final_equity: f64,
    want: &Value,
) {
    assert_eq!(
        f64_to_hex_bits(final_equity),
        want["final_equity"].as_str().unwrap(),
        "{name}: final_equity (got {final_equity})"
    );
    assert_eq!(n_trades as i64, want["n_trades"].as_i64().unwrap(), "{name}: n_trades");
    let want_curve = want["equity_curve"].as_array().unwrap();
    assert_eq!(curve.len(), want_curve.len(), "{name}: curve len");
    for (i, (g, w)) in curve.iter().zip(want_curve).enumerate() {
        assert_eq!(&f64_to_hex_bits(*g), w.as_str().unwrap(), "{name}: curve[{i}] (got {g})");
    }
    let want_trades = want["trades"].as_array().unwrap();
    assert_eq!(trades.len(), want_trades.len(), "{name}: trade count");
    for (i, (t, w)) in trades.iter().zip(want_trades).enumerate() {
        for (field, val) in [
            ("entry_price", t.entry_price),
            ("exit_price", t.exit_price),
            ("size", t.size),
            ("pnl", t.pnl),
            ("fees", t.fees),
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

#[test]
fn fastsim_portfolio_bit_parity() {
    let fx = load("fastsim_portfolio.json");
    for run in fx["runs"].as_array().unwrap() {
        let name = run["name"].as_str().unwrap();
        let t = run["T"].as_u64().unwrap() as usize;
        let s = run["S"].as_u64().unwrap() as usize;
        let m = &run["matrices"];
        let parse_mat = |key: &str| -> Matrix {
            let rows = m[key].as_array().unwrap();
            let mut data = Vec::with_capacity(t * s);
            for row in rows {
                for v in row.as_array().unwrap() {
                    data.push(if v.is_null() { f64::NAN } else { f(v) });
                }
            }
            Matrix::new(data, t, s)
        };
        let opens = parse_mat("opens");
        let closes = parse_mat("closes");
        let funding = parse_mat("funding");
        let weights = parse_mat("weights");
        let ts: Vec<i64> =
            run["ts"].as_array().unwrap().iter().map(|x| x.as_i64().unwrap()).collect();
        let symbols: Vec<String> = run["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        let p = &run["params"];
        let got = fast_portfolio_backtest(
            &opens,
            &closes,
            &funding,
            &ts,
            &weights,
            // No maker-classified fill exists in this kernel (see vector_engine's module doc) —
            // the value is dead; reuse taker_fee so the golden fixture (which has no maker_fee
            // column) needs no change.
            f(&p["taker_fee"]),
            f(&p["taker_fee"]),
            f(&p["slippage"]),
            f(&p["init_cash"]),
            f(&p["multiplier"]),
            Some(&symbols),
            true,
        );
        assert_trades_curve(
            name,
            &got.equity_curve,
            &got.trades,
            got.n_trades,
            got.final_equity,
            &run["result"],
        );
    }
}
