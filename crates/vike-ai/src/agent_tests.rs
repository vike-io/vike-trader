use super::*;

/// The daily anchor, kept HERE rather than on the module because the real path no longer has a
/// fixed annualization — `run_authoring_loop` derives it from the slice's interval.
///
/// These tests keep 252 deliberately: every one of them checks a SCALE-INVARIANT property (that
/// the displayed annualized Sharpe never reaches `overfit::`, and that the deflation runs on
/// per-observation values), and a scale-invariant claim is clearest when the scale is a
/// constant the reader can see.
const PPY: f64 = 252.0;
use crate::client::{FakeClient, FakeTurn, LlmError, ToolCall};
// The module itself here (no local named `ledger` in this scope) — the tests drive the ledger
// files directly to set up prior state.
use crate::ledger::{self, TrialLedger};
use serde_json::json;
use std::sync::Arc;
use vike_data::{HistStore, MemHistStore};
use vike_marketdata::Bar;

const CROSS: &str = r#"
const QTY = 1.0;
fn on_bar() { let f=sma(5); let s=sma(20); if s.is_nan(){return;} let tgt=if f>s{QTY}else{-QTY}; let d=tgt-position(); if abs(d)>1e-12 { market(if d>0.0{1}else{-1}, abs(d)); } }
"#;

/// An sma-cross script with the given (fast, slow) windows — used to give
/// `develop_strategies_deflates_every_accepted_candidate` a trial set with real variance
/// (different windows -> different OOS Sharpes on the same seeded data), unlike 3 identical
/// `CROSS` submissions which would give the trial set zero variance.
fn cross_script(fast: u32, slow: u32) -> String {
    format!(
        "const QTY = 1.0;\nfn on_bar() {{ let f=sma({fast}); let s=sma({slow}); if s.is_nan(){{return;}} let tgt=if f>s{{QTY}}else{{-QTY}}; let d=tgt-position(); if abs(d)>1e-12 {{ market(if d>0.0{{1}}else{{-1}}, abs(d)); }} }}"
    )
}

/// A bar-seeded `MemHistStore` (real in-memory bar storage behind `test-support` — no
/// DataFusion in this crate's dev graph) plus the TempDir the ledger tests park their files
/// under: the store no longer owns a directory, so the dir is the tests' own.
fn seeded() -> (tempfile::TempDir, Arc<MemHistStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = MemHistStore::new();
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store))
}

fn submit_call(code: &str) -> ToolCall {
    ToolCall {
        id: "t".into(),
        name: "submit_strategy".into(),
        input: json!({"code":code,"explanation":"e"}),
    }
}

fn submit(code: &str) -> FakeTurn {
    FakeTurn::Tool(submit_call(code))
}

/// A `FakeClient` that also RECORDS what the loop handed it: the user prompt (for the
/// grounding tests) and the advertised tool names (for the `record_learning` gating test).
/// `FakeClient` deliberately ignores both, so those two claims need their own double.
#[derive(Default)]
struct CapturingClient {
    /// The user prompt of every `run` call, in order.
    prompts: RefCell<Vec<String>>,
    /// The names of every tool advertised on every `run` call.
    tools: RefCell<Vec<String>>,
    /// Tool calls to emit once, on the first `run`.
    script: RefCell<Vec<ToolCall>>,
}

impl CapturingClient {
    fn scripted(calls: Vec<ToolCall>) -> Self {
        Self { script: RefCell::new(calls), ..Default::default() }
    }
    fn prompt(&self) -> String {
        self.prompts.borrow().first().cloned().expect("the loop calls the client exactly once")
    }
}

impl LlmClient for CapturingClient {
    fn run(
        &self,
        _system: &str,
        user: &str,
        tools: &[ToolSpec],
        dispatch: &mut dyn FnMut(ToolCall) -> String,
    ) -> Result<String, LlmError> {
        self.prompts.borrow_mut().push(user.to_string());
        self.tools.borrow_mut().extend(tools.iter().map(|t| t.name.clone()));
        let calls: Vec<ToolCall> = self.script.borrow_mut().drain(..).collect();
        for c in calls {
            let _ = dispatch(c);
        }
        Ok(String::new())
    }
}

/// A prior ACCEPTED trial on the seeded slice, carrying only the scalar the deflation trial
/// set actually reads (`sr_per_obs`) — the curve-pruned shape a ledger row eventually takes.
fn prior_accepted(i: usize, sr_per_obs: f64, oos_sharpe: f64) -> Trial {
    Trial {
        ts_ms: 1_000 + i as i64,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        explanation: format!("prior attempt {i}"),
        accepted: true,
        attempts: 1,
        oos_sharpe,
        n_trades: 5 + i,
        sr_per_obs,
        ..Default::default()
    }
}

#[test]
fn happy_path_accepts_a_valid_strategy() {
    let (_d, store) = seeded();
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
    );
    assert!(r.accepted);
    assert_eq!(r.attempts, 1);
    assert!(r.n_trades > 0);
}

#[test]
fn repair_path_recovers_after_a_compile_error() {
    let (_d, store) = seeded();
    let client =
        FakeClient::new(vec![submit("fn on_bar( {"), submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
    assert!(r.accepted);
    assert_eq!(r.attempts, 2);
    assert!(!r.problems.is_empty());
}

#[test]
fn give_up_after_max_repairs() {
    let (_d, store) = seeded();
    // Script MORE broken submissions than max_repairs allows -- if the cap were removed (or
    // merely decorative, as `_max_repairs` was pre-fix), every one of these 5 would be
    // compiled and `attempts` would end up at 5, not the capped 2.
    let max_repairs = 2;
    let client = FakeClient::new(vec![
        submit("fn on_bar( {"),
        submit("also broken ("),
        submit("still broken ("),
        submit("broken again ("),
        submit("broken once more ("),
        FakeTurn::Text("gave up".into()),
    ]);
    let r = develop_strategy(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        max_repairs,
        0.3,
    );
    assert!(!r.accepted);
    assert_eq!(
        r.attempts, max_repairs,
        "attempts must be bounded by max_repairs, not by the 5-submission script"
    );
    assert!(!r.problems.is_empty());
}

#[test]
fn repair_path_still_accepts_on_the_last_allowed_attempt() {
    // max_repairs=2, broken-then-valid: the 2nd (valid) submission must still land inside the
    // cap and be accepted -- the cap must not be off-by-one against the happy/repair paths.
    let (_d, store) = seeded();
    let client =
        FakeClient::new(vec![submit("fn on_bar( {"), submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
    assert!(r.accepted);
    assert_eq!(r.attempts, 2);
}

/// The Copilot pane's displayed Sharpe follows the SLICE's interval, not a hardcoded 252.
///
/// This field is rendered as "OOS Sharpe" by `crates/vike-studio/src/panes/chat.rs`'s `summary_of`,
/// beside a Performance tab that annualizes off the picked slice. While this read 252 at every
/// interval, the two panes printed numbers `sqrt(1440) ≈ 37.9x` apart for one strategy on one
/// 1m slice, inside one window.
///
/// ⚠ The `assert_ne!` is what makes this bite. The equality alone compares two expressions that
/// would both move if the derivation regressed; only the inequality with the daily anchor fails
/// when the literal comes back. The precondition guards vacuity — `metrics::sharpe` returns
/// `0.0` at EVERY factor for a dispersion-free curve, which would satisfy the inequality for
/// the wrong reason.
#[test]
fn the_displayed_sharpe_annualizes_off_the_slice_interval_not_a_bare_252() {
    let (_d, store) = seeded();
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 0.3);
    assert!(r.accepted, "the fixture must produce an accepted candidate to have a Sharpe");

    let at_daily = metrics::sharpe(&r.oos_equity_curve, PPY);
    assert!(
        at_daily != 0.0 && at_daily.is_finite(),
        "the fixture curve must have dispersion, else every factor yields 0.0: {at_daily}"
    );

    assert_eq!(
        r.oos_sharpe.to_bits(),
        metrics::sharpe(&r.oos_equity_curve, periods_per_year_for_interval("1m")).to_bits(),
        "the displayed Sharpe IS the OOS curve annualized at the 1m factor"
    );
    assert_ne!(
        r.oos_sharpe, at_daily,
        "...and NOT at the daily anchor — the regression this test exists to catch"
    );
}

#[test]
fn develop_strategies_deflates_every_accepted_candidate() {
    let (_d, store) = seeded();
    // Three DIFFERENT sma-cross variants over the same seeded data -> different OOS Sharpes,
    // giving the trial set real variance (not a degenerate all-identical case).
    let variants = [(5u32, 20u32), (8, 25), (3, 15)];
    let mut turns = Vec::new();
    for (i, (fast, slow)) in variants.iter().enumerate() {
        turns.push(submit(&cross_script(*fast, *slow)));
        turns.push(FakeTurn::Text(format!("done{i}")));
    }
    let client = FakeClient::new(turns);

    let results =
        develop_strategies("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 3, 2, 0.3);
    assert_eq!(results.len(), 3);

    let accepted: Vec<&AgentResult> = results.iter().filter(|r| r.accepted).collect();
    assert!(
        accepted.len() >= 2,
        "test setup must produce >=2 accepted trials for deflation to run at all"
    );

    let trial_sharpes: Vec<f64> = accepted.iter().map(|r| r.oos_sharpe).collect();
    assert!(
        trial_sharpes.iter().any(|&s| (s - trial_sharpes[0]).abs() > 1e-9),
        "test setup must give the trial set real variance, not all-identical sharpes"
    );

    let best = accepted.iter().max_by(|a, b| a.oos_sharpe.total_cmp(&b.oos_sharpe)).unwrap();
    assert!(best.deflated_sharpe.is_finite());
    assert!((0.0..=1.0).contains(&best.deflated_sharpe));
    assert_ne!(
        best.deflated_sharpe, 0.0,
        "deflated_sharpe must actually be computed, not left at the Default"
    );

    // overfit.rs's guarantee: benchmarking PSR against the trials' expected-max-Sharpe can only
    // match or REDUCE significance vs. a plain zero-benchmark PSR (deflated_sharpe_below_psr_
    // when_many_trials in overfit.rs). Recompute that same zero-benchmark PSR here and check
    // the direction holds for the winner. Both must use the PER-PERIOD Sharpe
    // (risk_return_ratio), same units DSR is computed in — feeding the annualized oos_sharpe
    // here would make raw_psr saturate to ~1.0 and the direction check vacuous.
    let observed_pp = metrics::risk_return_ratio(&best.oos_equity_curve);
    let n_obs = metrics::returns(&best.oos_equity_curve).len().max(2);
    let skew = metrics::returns_skewness(&best.oos_equity_curve);
    let kurt = 3.0 + metrics::returns_kurtosis(&best.oos_equity_curve);
    let raw_psr = overfit::probabilistic_sharpe_ratio(observed_pp, n_obs, 0.0, skew, kurt);
    assert!(
        best.deflated_sharpe <= raw_psr + 1e-9,
        "deflated {} should not exceed the plain zero-benchmark PSR {raw_psr}",
        best.deflated_sharpe
    );
}

/// Build an equity curve (starting at 100.0) whose per-bar simple returns are exactly `rets`.
fn curve_from_returns(rets: &[f64]) -> Vec<f64> {
    let mut eq = vec![100.0];
    for &r in rets {
        let last = *eq.last().unwrap();
        eq.push(last * (1.0 + r));
    }
    eq
}

fn accepted_with_curve(curve: Vec<f64>) -> AgentResult {
    AgentResult {
        accepted: true,
        // oos_sharpe is the ANNUALIZED value the field displays; deflate_batch must NOT use it.
        oos_sharpe: metrics::sharpe(&curve, PPY),
        oos_equity_curve: curve,
        ..Default::default()
    }
}

/// MAGNITUDE pin (unit test on `deflate_batch` directly, overfit.rs's own test style): a fixed,
/// hand-reasonable equity curve whose deflated Sharpe is pinned to a known per-period value.
/// This is the regression guard the reviewer asked for — it FAILS if DSR is computed from the
/// annualized Sharpe (which saturates to ~1.0) instead of the per-period one.
#[test]
fn deflate_batch_pins_per_period_dsr_magnitude() {
    // Two deterministic candidates with different, modest per-period Sharpes (positive drift +
    // deterministic zig-zag noise). 48 returns each -> n_obs=48, enough for skew/kurt.
    let rets_a: Vec<f64> = (0..48).map(|i| 0.002 + 0.01 * ((i % 4) as f64 - 1.5)).collect();
    let rets_b: Vec<f64> = (0..48).map(|i| 0.001 + 0.008 * ((i % 3) as f64 - 1.0)).collect();
    let curve_a = curve_from_returns(&rets_a);
    let curve_b = curve_from_returns(&rets_b);

    // The correct per-period inputs the fixed implementation must feed overfit:: with.
    let pp_a = metrics::risk_return_ratio(&curve_a);
    let pp_b = metrics::risk_return_ratio(&curve_b);
    let n_obs = metrics::returns(&curve_a).len().max(2); // = 48
    let skew = metrics::returns_skewness(&curve_a);
    let kurt = 3.0 + metrics::returns_kurtosis(&curve_a);
    let expected = overfit::deflated_sharpe_ratio(pp_a, &[pp_a, pp_b], n_obs, skew, kurt);
    // What the OLD (buggy) wiring computed: annualized sr fed where per-period is required.
    let ann_a = metrics::sharpe(&curve_a, PPY);
    let ann_b = metrics::sharpe(&curve_b, PPY);
    let buggy = overfit::deflated_sharpe_ratio(ann_a, &[ann_a, ann_b], n_obs, skew, kurt);

    let mut results = vec![accepted_with_curve(curve_a), accepted_with_curve(curve_b)];
    deflate_batch(&mut results);

    // (1) The implementation must match the per-period computation exactly.
    assert!(
        (results[0].deflated_sharpe - expected).abs() < 1e-12,
        "deflated {} must equal the per-period DSR {expected}",
        results[0].deflated_sharpe
    );
    // (2) Pinned magnitude: this curve's per-period DSR sits well inside the unit interval and
    //     is NOT saturated. Value pinned from the fixed wiring; the buggy annualized wiring
    //     saturates to ~1.0, so this bound is what makes the test a real regression catcher.
    assert!(
        (results[0].deflated_sharpe - 0.874_192).abs() < 1e-5,
        "pinned per-period DSR magnitude drifted: {}",
        results[0].deflated_sharpe
    );
    // (3) The buggy annualized wiring would have saturated near 1.0 -> materially different, so
    //     assertions (1)/(2) genuinely distinguish the two unit conventions.
    assert!(
        buggy > 0.999 && (buggy - results[0].deflated_sharpe).abs() > 0.1,
        "annualized wiring should saturate (~1.0); got buggy={buggy}, actual={}",
        results[0].deflated_sharpe
    );
}

#[test]
fn develop_strategies_leaves_deflated_sharpe_zero_below_two_accepted() {
    let (_d, store) = seeded();
    // Only 1 candidate ever accepts (the other gives up) -> deflation is undefined, must stay
    // at the Default 0.0 rather than compute something from a single-element trial set.
    let client = FakeClient::new(vec![
        submit(CROSS),
        FakeTurn::Text("done0".into()),
        submit("fn on_bar( {"),
        submit("still broken ("),
        FakeTurn::Text("gave up".into()),
    ]);
    let results =
        develop_strategies("x", "binance", "BTCUSDT", "1m", store.as_ref(), &client, 2, 2, 0.3);
    assert_eq!(results.iter().filter(|r| r.accepted).count(), 1);
    for r in &results {
        assert_eq!(r.deflated_sharpe, 0.0);
    }
}

/// **The acceptance test for the whole trial-ledger item.** One accepted candidate — the
/// production shape, which before this could never reach the ≥2 trials deflation needs — plus
/// 4 prior accepted trials recorded on the same slice, must produce a real `deflated_sharpe`.
/// The bare (ledger-less) control run over the same data still reports `0.0`, which is exactly
/// the bug: the single most important overfitting control was inert on the production path.
#[test]
fn deflation_uses_prior_trials() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    // Four prior trials with REAL spread — a zero-variance trial set yields sr_star == 0 and
    // would make this test pass without the prior half being consulted at all.
    let prior = TrialLedger {
        trials: vec![
            prior_accepted(0, 0.02, 0.31),
            prior_accepted(1, 0.05, 0.79),
            prior_accepted(2, 0.03, 0.47),
            prior_accepted(3, 0.09, 1.42),
        ],
    };
    ledger::save_trials(&prior, &paths.trials);

    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
        Some(&paths),
    );
    assert!(r.accepted);
    assert!(r.deflated_sharpe.is_finite());
    assert!((0.0..=1.0).contains(&r.deflated_sharpe));
    assert_ne!(
        r.deflated_sharpe, 0.0,
        "a single accepted candidate + prior trials must deflate — this is the whole item"
    );

    // Exact: the trial set is {this candidate} ∪ {the 4 prior}, in that order.
    let m = overfit::sharpe_moments(&r.oos_equity_curve);
    let mut set = vec![m.sr_per_obs];
    set.extend(prior.trials.iter().map(|t| t.sr_per_obs));
    let expected = overfit::deflated_sharpe_ratio(m.sr_per_obs, &set, m.n_obs, m.skew, m.kurt);
    assert_eq!(r.deflated_sharpe, expected, "prior trials must join the trial set verbatim");

    // The run is appended (with its deflated value) — the NEXT session's prior set.
    let after = ledger::load_trials(&paths.trials);
    assert_eq!(after.trials.len(), 5, "the run's own trial is recorded");
    let last = after.trials.last().unwrap();
    assert!(last.accepted);
    assert_eq!(last.deflated_sharpe, expected);
    assert_eq!(last.sr_per_obs, m.sr_per_obs, "the deflation scalar survives curve pruning");
    assert_eq!(after.prior_sharpes_for("binance", "BTCUSDT", "1m").len(), 5);

    // Control — the pre-ledger path over the SAME data reports nothing.
    let bare_client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let bare = develop_strategy(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &bare_client,
        2,
        0.3,
    );
    assert!(bare.accepted);
    assert_eq!(bare.deflated_sharpe, 0.0, "ledger-less is byte-identical to pre-ledger");
}

/// A prior trial on a DIFFERENT slice must not widen this slice's multiple-testing correction
/// — the trial set is per-`(venue, symbol, interval)`, not global.
#[test]
fn deflation_ignores_other_slices() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let mut t = prior_accepted(0, 0.05, 0.79);
    t.symbol = "ETHUSDT".into();
    ledger::save_trials(&TrialLedger { trials: vec![t] }, &paths.trials);

    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
        Some(&paths),
    );
    assert!(r.accepted);
    assert_eq!(r.deflated_sharpe, 0.0, "1 candidate + 0 same-slice priors -> still undefined");
}

/// Payoff (a): the grounding block naming a prior trial's Sharpe, its explanation, and the
/// matching learnings reaches the model — and the user's own request is preserved beneath it.
#[test]
fn prompt_includes_prior_trials() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let mut t = prior_accepted(0, 0.05, 1.23);
    t.explanation = "ema pullback".into();
    ledger::save_trials(&TrialLedger { trials: vec![t] }, &paths.trials);
    ledger::append_learning(
        &paths.learnings,
        Learning {
            ts_ms: 1,
            scope: LearningScope::Global,
            text: "fees dominate below 20 trades".into(),
        },
    );

    let client = CapturingClient::default();
    let _ = develop_strategy_with_ledger(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
        Some(&paths),
    );
    let prompt = client.prompt();
    assert!(prompt.contains("1.23"), "the prior trial's OOS Sharpe must be named: {prompt}");
    assert!(prompt.contains("ema pullback"), "…and what it was: {prompt}");
    assert!(prompt.contains("fees dominate below 20 trades"), "the learning: {prompt}");
    assert!(prompt.contains("make an sma cross"), "the user's request survives: {prompt}");

    // Control — no ledger means the prompt is the caller's, verbatim.
    let bare = CapturingClient::default();
    let _ = develop_strategy(
        "make an sma cross",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &bare,
        2,
        0.3,
    );
    assert_eq!(bare.prompt(), "make an sma cross");
}

/// A fresh ledger has nothing to say, so the prompt stays byte-identical to the ledger-less
/// one — grounding must not inject an empty header on a slice with no history.
#[test]
fn empty_ledger_leaves_the_prompt_untouched() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let client = CapturingClient::default();
    let _ = develop_strategy_with_ledger(
        "hello",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
        Some(&paths),
    );
    assert_eq!(client.prompt(), "hello");
}

/// The `record_learning` tool: advertised only WITH a ledger, writes a slice-scoped note, and
/// does not consume a repair attempt.
#[test]
fn record_learning_tool_is_gated_on_the_ledger_and_writes_the_note() {
    let (d, store) = seeded();
    let paths = LedgerPaths::under(d.path());
    let client = CapturingClient::scripted(vec![
        ToolCall {
            id: "l".into(),
            name: "record_learning".into(),
            input: json!({"scope":"slice","text":"rsi mean-reversion overtrades here"}),
        },
        submit_call(CROSS),
    ]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
        Some(&paths),
    );
    assert!(client.tools.borrow().iter().any(|t| t == "record_learning"));
    assert!(r.accepted, "the submission after the note still runs");
    assert_eq!(r.attempts, 1, "a learning call must not burn a repair attempt");

    let notes = ledger::load_learnings(&paths.learnings);
    assert_eq!(notes.learnings.len(), 1);
    assert_eq!(notes.learnings[0].text, "rsi mean-reversion overtrades here");
    assert_eq!(
        notes.learnings[0].scope,
        LearningScope::Slice {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
        }
    );

    // Ledger-less: the tool is not offered at all.
    let bare = CapturingClient::default();
    let _ = develop_strategy("x", "binance", "BTCUSDT", "1m", store.as_ref(), &bare, 2, 0.3);
    assert_eq!(*bare.tools.borrow(), vec!["submit_strategy".to_string()]);
}

/// An unwritable ledger directory must cost the note/trial, never the run — the best-effort
/// contract the whole module is built on.
#[test]
fn an_unwritable_ledger_never_fails_the_loop() {
    let (d, store) = seeded();
    // A path whose PARENT does not exist: every read misses and every write errors.
    let missing = d.path().join("no").join("such").join("dir");
    let paths = LedgerPaths::under(&missing);
    let client = FakeClient::new(vec![submit(CROSS), FakeTurn::Text("done".into())]);
    let r = develop_strategy_with_ledger(
        "x",
        "binance",
        "BTCUSDT",
        "1m",
        store.as_ref(),
        &client,
        2,
        0.3,
        Some(&paths),
    );
    assert!(r.accepted, "the authoring loop must survive an unavailable ledger");
    assert!(!paths.trials.exists());
}
