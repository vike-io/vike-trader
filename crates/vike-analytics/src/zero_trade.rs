//! `zero_trade` — the zero / near-zero-trade cause analyzer (a LEAN `FlatEquityCurveAnalysis`
//! analog). A PURE report-side composition over diagnostic counters the engine ALREADY
//! accumulates on [`BacktestResult`]; it adds NO counting to any fill lane.
//!
//! When a run closes zero trades AND its equity curve never moved off the starting cash, the bare
//! metrics table ("total_return 0%, sharpe NaN, max_drawdown 0%, …") says nothing about WHY. This
//! module turns the signals that lane already recorded — the gate-drop channel
//! (`vike_backtest::SimBroker::dropped`: `RiskGate` / cash-gate / `"volume_cap"` / `"latency_reject"`),
//! the stale-price wait deferrals ([`BacktestResult::stale_deferrals`]), the session-gate skips
//! ([`BacktestResult::session_deferrals`]), the strategy's warm-up requirement
//! ([`BacktestResult::warmup`]), and the raw step count ([`BacktestResult::equity_curve`] length)
//! — into a RANKED list of probable causes with actionable messages.
//!
//! OFF / inert by construction: [`ZeroTradeReport::analyze`] returns `None` for any run that
//! closed a trade OR moved equity, so a [`crate::BacktestReport`] composed for a normal run carries
//! no diagnosis and is byte-identical to before this module existed. The analyzer only ever fires
//! on the all-zero / flat case — which is why an open-and-hold position (0 CLOSED trades but a real,
//! marked position) is deliberately NOT flagged: its equity curve moved.
//!
//! FEATURE-FREE like [`crate::report`] and `vike_backtest::objective`: the composition needs only
//! `serde` + `indexmap` + [`crate::result`], which is why it now lives in this vike-model-only
//! crate rather than behind vike-backtest's `hist-replay` feature — the DataFusion-free leaf
//! crates that reuse `BacktestReport` inherit the diagnosis for free.

use serde::Serialize;

use indexmap::IndexMap;

use crate::result::BacktestResult;

/// One diagnosed probable cause of a zero-trade / flat-equity run. Its position inside
/// [`ZeroTradeReport::causes`] IS the ranking (most-probable first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZeroTradeCause {
    /// Stable machine tag (kebab-case): `"no-data"`, `"warmup-shortfall"`, `"orders-denied"`,
    /// `"stale-price"`, `"session-closed"`, `"no-orders"`.
    pub code: &'static str,
    /// One-line human headline.
    pub headline: String,
    /// Actionable detail — what to check or change.
    pub detail: String,
}

/// The ranked diagnosis of a zero-trade / flat-equity run. `causes` is NEVER empty (a catch-all
/// `"no-orders"` cause is emitted when nothing more specific applies) and is ordered
/// most-probable-first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZeroTradeReport {
    pub causes: Vec<ZeroTradeCause>,
}

/// The already-accumulated signals [`rank_causes`] ranks — extracted from a [`BacktestResult`] by
/// [`ZeroTradeReport::analyze`], or built directly in a test. Nothing here is NEWLY counted: every
/// field mirrors a counter the fill lane recorded during the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZeroTradeInputs {
    /// Equity-curve steps = bars/ticks processed. `0` = the data slice was empty.
    pub steps: usize,
    /// Market / market-close orders the stale-price wait gate deferred
    /// ([`BacktestResult::stale_deferrals`]).
    pub stale_deferrals: u64,
    /// Fills the session gate skipped because the venue was closed
    /// ([`BacktestResult::session_deferrals`]).
    pub session_deferrals: u64,
    /// Gate-dropped orders aggregated by reason, first-seen order preserved (from
    /// [`BacktestResult::dropped`] via [`aggregate_denials`]): `RiskGate` reason strings,
    /// order-kind cash-gate drops, `"volume_cap"`, `"latency_reject"`.
    pub denials: Vec<(String, u64)>,
    /// The strategy's warm-up requirement in bars/ticks ([`BacktestResult::warmup`]); `0` = none.
    pub warmup: usize,
}

/// Aggregate the raw gate-drop channel ([`BacktestResult::dropped`]) into `reason -> count`,
/// FIRST-SEEN order preserved (`IndexMap`, the repo's order-stable map — the same rule the engine's
/// own `dropped` push order follows). Each `dropped` tuple is `(symbol, reason, size, weight)`;
/// only the reason is counted here.
pub fn aggregate_denials(dropped: &[(String, String, f64, f64)]) -> Vec<(String, u64)> {
    let mut by_reason: IndexMap<String, u64> = IndexMap::new();
    for (_symbol, reason, _size, _weight) in dropped.iter() {
        *by_reason.entry(reason.clone()).or_insert(0) += 1;
    }
    by_reason.into_iter().collect()
}

/// Render the denial reasons as `"reason (count), …"` ordered by count desc (ties keep first-seen
/// order — a STABLE sort over the aggregation's insertion order).
fn format_reasons(denials: &[(String, u64)]) -> String {
    let mut sorted: Vec<&(String, u64)> = denials.iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    sorted
        .into_iter()
        .map(|(reason, count)| format!("{reason} ({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rank the probable causes of a zero-trade run, most-probable first. PURE: deterministic, no I/O,
/// TOTAL (always ≥ 1 cause). Ranking is by evidence weight — a near-certain structural explanation
/// (empty data, an unsatisfiable warm-up) outranks a "the strategy tried but was blocked" signal,
/// which outranks the catch-all. Ties keep the fixed push order below (denials, then stale, then
/// session), because `slice::sort_by` is stable.
pub fn rank_causes(inputs: &ZeroTradeInputs) -> Vec<ZeroTradeCause> {
    // (weight, cause) — a STABLE sort by weight desc then keeps the push order for ties.
    let mut scored: Vec<(u64, ZeroTradeCause)> = Vec::new();

    // --- structural certainties (sentinel weights so they always lead when present) ---
    if inputs.steps == 0 {
        scored.push((
            u64::MAX,
            ZeroTradeCause {
                code: "no-data",
                headline: "No market data was loaded for this run.".to_string(),
                detail: "0 bars/ticks were processed - the strategy never saw a single event. \
                         Check the profile's symbol, venue, and date range against what the hist \
                         store actually holds."
                    .to_string(),
            },
        ));
    } else if inputs.warmup >= inputs.steps {
        // `steps > 0` here, so this needs a real warm-up: `warmup >= steps` means the engine's
        // `i >= strategy.warmup()` dispatch gate never opened, so on_bar/on_tick ran ZERO times.
        // A strategy with the default `warmup() == 0` never trips this (`0 >= steps>0` is false).
        scored.push((
            u64::MAX - 1,
            ZeroTradeCause {
                code: "warmup-shortfall",
                headline: format!(
                    "Strategy warm-up ({} bars) exceeds the {} bar(s) loaded.",
                    inputs.warmup, inputs.steps
                ),
                detail: format!(
                    "The strategy stayed in warm-up for the whole run: it needs {} warm-up \
                     bars/ticks but only {} were available, so its on_bar/on_tick logic never ran. \
                     Widen the date range or reduce the indicator lookback.",
                    inputs.warmup, inputs.steps
                ),
            },
        ));
    }

    // --- "the strategy tried but was blocked" — ranked by how many orders/fills each blocked ---
    let denied: u64 = inputs.denials.iter().map(|(_, count)| *count).sum();
    if denied > 0 {
        scored.push((
            denied,
            ZeroTradeCause {
                code: "orders-denied",
                headline: format!("All {denied} submitted order(s) were rejected before filling."),
                detail: format!(
                    "Rejections by reason: {}. These are RiskGate / cash-gate / volume-cap / \
                     latency drops - the strategy submitted orders but none was admitted. Review \
                     the gate limits (margin, min-qty/notional, price collar, buying power) or the \
                     order sizes.",
                    format_reasons(&inputs.denials)
                ),
            },
        ));
    }
    if inputs.stale_deferrals > 0 {
        scored.push((
            inputs.stale_deferrals,
            ZeroTradeCause {
                code: "stale-price",
                headline: format!(
                    "{} market order(s) were deferred waiting for a fresh price print.",
                    inputs.stale_deferrals
                ),
                detail: "The stale-price wait gate (max_price_staleness_ms) held every market \
                         order because the tape never printed fresh data within the bound - the \
                         data may be forward-filled / zero-volume across this window."
                    .to_string(),
            },
        ));
    }
    if inputs.session_deferrals > 0 {
        scored.push((
            inputs.session_deferrals,
            ZeroTradeCause {
                code: "session-closed",
                headline: format!(
                    "{} fill(s) were skipped because the market was closed.",
                    inputs.session_deferrals
                ),
                detail: "The session gate refused fills for the whole window - the backtest range \
                         may fall entirely outside the venue's trading hours (weekend / holiday / \
                         after-hours)."
                    .to_string(),
            },
        ));
    }

    // Stable, highest weight first. Ties keep push order (denials, stale, session).
    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut causes: Vec<ZeroTradeCause> = scored.into_iter().map(|(_, cause)| cause).collect();

    // Catch-all: nothing more specific fired, so the strategy simply never submitted. This is also
    // where the bar-mode `SYMBOL.VENUE` symbol-routing footgun surfaces (a strategy routing on the
    // bare symbol while `snap_to_properties` re-tagged the bars submits nothing that routes).
    if causes.is_empty() {
        causes.push(ZeroTradeCause {
            code: "no-orders",
            headline: "No order-blocking cause was detected.".to_string(),
            // Worded to stay accurate for the one case the result cannot disambiguate: an
            // open-and-hold position in a market that never moved is flat + zero-CLOSED-trades too,
            // and the result carries no fill counter to tell it apart from "never submitted".
            detail: format!(
                "Over {} bar(s)/tick(s) nothing was denied, deferred, or gated and no trade \
                 closed, so no gate explains the flat result. The strategy most likely never \
                 submitted an order - check its entry condition and that it reads the SAME symbol \
                 the data is tagged with (bar-mode `snap_to_properties` re-tags bars as \
                 `SYMBOL.VENUE`, which a strategy routing on the bare symbol will miss).",
                inputs.steps
            ),
        });
    }

    causes
}

impl ZeroTradeReport {
    /// Diagnose a run that closed NO trades and whose equity never moved off the start. Returns
    /// `None` for any run that closed a trade (`n_trades > 0`) OR moved equity — so a
    /// [`crate::BacktestReport`] composed for a normal run carries no diagnosis and is
    /// byte-identical to before this analyzer existed.
    ///
    /// The trigger is deliberately `n_trades == 0 AND flat equity`: an open-and-hold position moves
    /// the marked equity curve, so a buy-and-hold run (0 CLOSED trades but a real position) is NOT
    /// flagged — the analyzer fires only when nothing at all happened.
    pub fn analyze(result: &BacktestResult) -> Option<ZeroTradeReport> {
        if result.n_trades != 0 || !equity_is_flat(&result.equity_curve) {
            return None;
        }
        let inputs = ZeroTradeInputs {
            steps: result.equity_curve.len(),
            stale_deferrals: result.stale_deferrals,
            session_deferrals: result.session_deferrals,
            denials: aggregate_denials(&result.dropped),
            warmup: result.warmup,
        };
        Some(ZeroTradeReport { causes: rank_causes(&inputs) })
    }
}

/// A run's equity is "flat" when it never moved off its first value — the signature of a run in
/// which no fill ever changed the account. Empty (no data) counts as flat. Exact equality is
/// correct here: with no open position `equity_now() = cash` bit-for-bit every step, so a genuinely
/// idle run has a perfectly constant curve (no float drift to tolerance-away).
fn equity_is_flat(curve: &[f64]) -> bool {
    match curve.first() {
        None => true,
        Some(&first) => curve.iter().all(|&e| e == first),
    }
}

impl std::fmt::Display for ZeroTradeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "no trades executed - probable cause(s), most likely first:")?;
        for (i, cause) in self.causes.iter().enumerate() {
            writeln!(f, "  [{}] {}", i + 1, cause.headline)?;
            writeln!(f, "      {}", cause.detail)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ZeroTradeInputs {
        ZeroTradeInputs { steps: 100, ..Default::default() }
    }

    #[test]
    fn empty_data_is_the_top_cause() {
        let got = rank_causes(&ZeroTradeInputs { steps: 0, ..Default::default() });
        assert_eq!(got[0].code, "no-data");
    }

    #[test]
    fn empty_data_outranks_a_stale_signal() {
        // steps == 0 is a structural certainty (sentinel weight) — it must lead even if some other
        // counter is somehow non-zero.
        let got = rank_causes(&ZeroTradeInputs {
            steps: 0,
            stale_deferrals: 9_999,
            ..Default::default()
        });
        assert_eq!(got[0].code, "no-data", "empty data is the definitive cause");
    }

    #[test]
    fn warmup_shortfall_fires_when_warmup_ge_steps() {
        let got = rank_causes(&ZeroTradeInputs { steps: 10, warmup: 50, ..Default::default() });
        assert_eq!(got[0].code, "warmup-shortfall");
        assert!(got[0].headline.contains("50"));
        assert!(got[0].headline.contains("10"));
    }

    #[test]
    fn warmup_shortfall_does_not_fire_at_boundary_below() {
        // warmup < steps ⇒ the dispatch gate DID open ⇒ not a warm-up shortfall.
        let got = rank_causes(&ZeroTradeInputs { steps: 10, warmup: 9, ..Default::default() });
        assert!(got.iter().all(|c| c.code != "warmup-shortfall"));
        // nothing else applies -> the catch-all.
        assert_eq!(got[0].code, "no-orders");
    }

    #[test]
    fn default_warmup_never_trips_the_shortfall() {
        // The default `Strategy::warmup() == 0` must never be read as a shortfall.
        let got = rank_causes(&ZeroTradeInputs { steps: 5, warmup: 0, ..Default::default() });
        assert!(got.iter().all(|c| c.code != "warmup-shortfall"));
    }

    #[test]
    fn denials_are_the_flagship_cause_with_a_reason_breakdown() {
        let got = rank_causes(&ZeroTradeInputs {
            denials: vec![("insufficient-margin".into(), 12), ("below-min-qty".into(), 3)],
            ..inputs()
        });
        assert_eq!(got[0].code, "orders-denied");
        assert!(got[0].headline.contains("15"), "15 = 12 + 3 total denials: {}", got[0].headline);
        // reasons rendered biggest-first
        assert!(got[0].detail.contains("insufficient-margin (12)"));
        assert!(got[0].detail.contains("below-min-qty (3)"));
        let margin_at = got[0].detail.find("insufficient-margin").unwrap();
        let minqty_at = got[0].detail.find("below-min-qty").unwrap();
        assert!(margin_at < minqty_at, "reasons ordered by count desc");
    }

    #[test]
    fn causes_rank_by_evidence_weight() {
        // stale (20) > denials (5) > session (2): the blocked-order signals rank by count.
        let got = rank_causes(&ZeroTradeInputs {
            stale_deferrals: 20,
            session_deferrals: 2,
            denials: vec![("insufficient-margin".into(), 5)],
            ..inputs()
        });
        let order: Vec<&str> = got.iter().map(|c| c.code).collect();
        assert_eq!(order, vec!["stale-price", "orders-denied", "session-closed"]);
    }

    #[test]
    fn ties_keep_push_order_denials_then_stale_then_session() {
        // Equal weights: a stable sort must keep the fixed push order.
        let got = rank_causes(&ZeroTradeInputs {
            stale_deferrals: 7,
            session_deferrals: 7,
            denials: vec![("insufficient-margin".into(), 7)],
            ..inputs()
        });
        let order: Vec<&str> = got.iter().map(|c| c.code).collect();
        assert_eq!(order, vec!["orders-denied", "stale-price", "session-closed"]);
    }

    #[test]
    fn fallback_no_orders_when_nothing_specific_applies() {
        let got = rank_causes(&inputs());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].code, "no-orders");
        assert!(got[0].detail.contains("100"), "mentions the bar count");
        // the symbol-route footgun hint rides in the catch-all detail
        assert!(got[0].detail.contains("SYMBOL.VENUE"));
    }

    #[test]
    fn aggregate_denials_counts_by_reason_first_seen_order() {
        let dropped = vec![
            ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 1.0, 0.0),
            ("BTCUSDT".to_string(), "below-min-qty".to_string(), 1.0, 0.0),
            ("BTCUSDT".to_string(), "insufficient-margin".to_string(), 2.0, 0.0),
        ];
        let got = aggregate_denials(&dropped);
        assert_eq!(
            got,
            vec![("insufficient-margin".to_string(), 2), ("below-min-qty".to_string(), 1)]
        );
    }

    #[test]
    fn analyze_returns_none_for_a_run_with_trades() {
        // The OFF path: a run that closed a trade is never diagnosed -> its report is unchanged.
        let r = BacktestResult {
            n_trades: 3,
            equity_curve: vec![1000.0, 1000.0, 1000.0],
            ..Default::default()
        };
        assert!(ZeroTradeReport::analyze(&r).is_none());
    }

    #[test]
    fn analyze_returns_none_when_equity_moved() {
        // A buy-and-hold: 0 CLOSED trades but the marked equity moved -> NOT flagged.
        let r = BacktestResult {
            n_trades: 0,
            equity_curve: vec![1000.0, 1001.0, 1002.0],
            ..Default::default()
        };
        assert!(ZeroTradeReport::analyze(&r).is_none());
    }

    #[test]
    fn analyze_fires_on_a_flat_zero_trade_run_with_denials() {
        let r = BacktestResult {
            n_trades: 0,
            equity_curve: vec![1000.0, 1000.0, 1000.0],
            dropped: vec![("BTCUSDT".to_string(), "insufficient-margin".to_string(), 1.0, 0.0)],
            ..Default::default()
        };
        let report = ZeroTradeReport::analyze(&r).expect("flat + zero trades must diagnose");
        assert_eq!(report.causes[0].code, "orders-denied");
    }

    #[test]
    fn analyze_threads_the_warmup_field_through() {
        // Proves `BacktestResult::warmup` reaches the diagnosis: warmup (50) > the 10 flat steps.
        let r = BacktestResult {
            n_trades: 0,
            equity_curve: vec![1000.0; 10],
            warmup: 50,
            ..Default::default()
        };
        let report = ZeroTradeReport::analyze(&r).expect("flat + zero trades must diagnose");
        assert_eq!(report.causes[0].code, "warmup-shortfall");
    }

    #[test]
    fn analyze_empty_curve_is_no_data() {
        let r = BacktestResult { n_trades: 0, equity_curve: Vec::new(), ..Default::default() };
        let report = ZeroTradeReport::analyze(&r).expect("empty curve is flat + zero trades");
        assert_eq!(report.causes[0].code, "no-data");
    }

    #[test]
    fn equity_is_flat_recognizes_constant_and_empty_curves() {
        assert!(equity_is_flat(&[]));
        assert!(equity_is_flat(&[1000.0]));
        assert!(equity_is_flat(&[1000.0, 1000.0, 1000.0]));
        assert!(!equity_is_flat(&[1000.0, 1000.0, 1000.01]));
    }

    #[test]
    fn display_numbers_the_ranked_causes() {
        let report = ZeroTradeReport {
            causes: vec![
                ZeroTradeCause {
                    code: "orders-denied",
                    headline: "H1".to_string(),
                    detail: "D1".to_string(),
                },
                ZeroTradeCause {
                    code: "stale-price",
                    headline: "H2".to_string(),
                    detail: "D2".to_string(),
                },
            ],
        };
        let s = report.to_string();
        assert!(s.contains("probable cause"));
        assert!(s.contains("[1] H1"));
        assert!(s.contains("[2] H2"));
        assert!(s.contains("D1"));
    }

    #[test]
    fn report_serializes_causes_as_an_array() {
        let report = ZeroTradeReport::analyze(&BacktestResult {
            n_trades: 0,
            equity_curve: vec![1000.0, 1000.0],
            ..Default::default()
        })
        .unwrap();
        let json = serde_json::to_string(&report).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["causes"].is_array());
        assert_eq!(v["causes"][0]["code"], "no-orders");
    }
}
