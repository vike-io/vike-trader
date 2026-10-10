//! The scripted market `trace` plays: one price path, two feeds, a bar lane and a tick lane.

use vike_model::{Bar, QuoteTick};

/// A mean-reverting-ish path in `(0, 1)`, so the SAME script runs at unit scale (where `bounded01`
/// and `tick` mean something) and at a 100 scale (where the ordinary `step`/`band` knobs do).
const PATH: [f64; 12] = [0.50, 0.53, 0.47, 0.58, 0.42, 0.62, 0.38, 0.66, 0.34, 0.70, 0.30, 0.52];

/// Milliseconds between steps: the script spans `0..=110_000`, inside which `trailing_scalper`'s
/// entry cutoffs are compared against `market_open_ms`/`market_close_ms`.
const STEP_MS: i64 = 10_000;

/// Fill→react rounds folded after each market event: the grid cascade (entry fill → take-profit →
/// re-arm …) is bounded here; the comparison only needs both sides bounded identically.
pub(super) const ROUNDS: usize = 3;

/// Which feed the driver plays. It exists for ONE declared residual: `PairsZScore` reads
/// `funding_a`/`funding_b` only when the bar carries no funding, so a funding-BEARING feed makes
/// those keys inert for a reason no params-table gate could express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Feed {
    /// `A`/`B` (the pairs legs) carry no funding; `F1`/`F2` (the carry legs) do.
    Plain,
    /// ...and now `A`/`B` carry funding too.
    FundedPairLegs,
}

pub(super) enum Ev {
    Bar(Bar),
    Quote(QuoteTick),
}

fn a_bar(symbol: &str, ts: i64, close: f64, funding: Option<f64>) -> Bar {
    Bar {
        ts,
        open: close,
        high: close * 1.02,
        low: close * 0.98,
        close,
        volume: 1.0,
        funding,
        bid: None,
        ask: None,
        symbol: Some(symbol.to_string()),
    }
}

/// Which symbols a strategy's feed carries. ⚠ **Per strategy, and load-bearing**: fed another
/// symbol's bars, `Grid` (no `symbol`, so it takes each bar's own) hit its hard-stop band on the
/// SECOND bar and halted, making `band`, `step` and `size` look inert — three false instances.
///
/// `A`/`B` carry no funding (the `pairs_zscore` legs, whose absent funding keeps its
/// `funding_a`/`funding_b` fallbacks live); `F1`/`F2` do (the `funding_carry` venues and the
/// `funding_capture` decision points).
pub(super) fn feed_symbols(strategy: &str) -> &'static [&'static str] {
    match strategy {
        "funding_capture" => &["F1"],
        "funding_carry" => &["F1", "F2"],
        "pairs_zscore" => &["A", "B"],
        _ => &["A"],
    }
}

/// One pass of the script at `scale`, carrying only `symbols`.
pub(super) fn events(scale: f64, feed: Feed, symbols: &[&str]) -> Vec<Ev> {
    let leg_funding = match feed {
        Feed::Plain => None,
        Feed::FundedPairLegs => Some(0.002),
    };
    let mut out = Vec::new();
    for (i, frac) in PATH.iter().enumerate() {
        let ts = i as i64 * STEP_MS;
        let a = frac * scale;
        let b = PATH[(i + 5) % PATH.len()] * scale;
        let f2 = PATH[(i + 2) % PATH.len()] * scale;
        for (symbol, close, funding) in [
            ("A", a, leg_funding),
            ("B", b, leg_funding),
            ("F1", a, Some(0.01)),
            ("F2", f2, Some(-0.01)),
        ] {
            if symbols.contains(&symbol) {
                out.push(Ev::Bar(a_bar(symbol, ts, close, funding)));
            }
        }
        // The TICK lane, on whichever symbol this strategy's feed leads with —
        // `trailing_scalper` trades on quotes ALONE and would otherwise never run at all.
        if let Some(symbol) = symbols.first() {
            let mid = if *symbol == "F2" {
                f2
            } else if *symbol == "B" {
                b
            } else {
                a
            };
            out.push(Ev::Quote(QuoteTick {
                ts: ts + 1,
                local_ts: ts + 1,
                bid: mid * 0.99,
                ask: mid * 1.01,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: (*symbol).to_string(),
            }));
        }
    }
    out
}
