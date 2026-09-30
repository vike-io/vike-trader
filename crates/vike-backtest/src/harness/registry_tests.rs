use super::*;
use vike_model::{Bar, Broker};
use vike_sim::{EngineParams, StrategyEngine};
use vike_strategy::BuyHold;

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn buy_hold_resolves_ok_with_default_params() {
    let params = Value::Table(Default::default());
    let strat = strategy_by_name("buy_hold", &params);
    assert!(strat.is_ok());
}

#[test]
fn unknown_name_is_a_validation_error() {
    let params = Value::Table(Default::default());
    // `Box<dyn Strategy<SimBroker>>` isn't `Debug`, so match instead of `unwrap_err`.
    match strategy_by_name("nope", &params) {
        Err(HarnessError::Validation(msg)) => {
            // The message must name THIS crate's full roster, not the portable half's 10 —
            // otherwise a profile naming a simulator-only strategy would be told it does not
            // exist. This is the one thing the delegation could plausibly get wrong.
            assert!(msg.contains("rotation_top_k"), "names the sim-only half too: {msg}");
            assert!(msg.contains("buy_hold"), "names the portable half too: {msg}");
        }
        other => panic!("expected Validation error, got Ok={}", other.is_ok()),
    }
}

#[test]
fn strategies_const_lists_buy_hold() {
    assert!(STRATEGIES.contains(&"buy_hold"));
}

/// COMPILE-TIME proof that [`BuyHold`] really is portable, not merely spelled that way.
///
/// The proof is `probe`'s GENERIC parameter: a generic function's body is type-checked at its
/// DEFINITION, so `BuyHold: Strategy<B>` must hold for EVERY `B: Broker` for this to build.
/// Reaching for one `SimBroker`-only verb narrows the impl back to `Strategy<SimBroker>` and
/// this stops compiling — which the `strategy_by_name` resolve tests, which only ever see the
/// concrete `SimBroker`, would not catch. Instantiating at `SimBroker` merely gives the test
/// something to run. Twin of `ref_strategies`' `tick_pair_mse_mounts_on_any_broker`.
#[test]
fn buy_hold_mounts_on_any_broker() {
    fn mounts<B: Broker, S: Strategy<B>>(_s: S) {}
    fn probe<B: Broker>() {
        mounts::<B, BuyHold>(BuyHold::new(1.0, None));
    }
    probe::<SimBroker>();
}

#[test]
fn registry_lists_every_match_arm() {
    // Every name STRATEGIES advertises must actually resolve (keeps the const in sync with
    // the match AND the delegated portable half by construction, not just by convention).
    for name in STRATEGIES {
        let params = Value::Table(Default::default());
        assert!(strategy_by_name(name, &params).is_ok(), "{name} should resolve");
    }
}

/// The SPLIT completeness gate (the CLAUDE.md capability-map STEP-1 pattern): this crate's
/// roster is EXACTLY the portable half plus the simulator-only half, with nothing in both and
/// nothing in neither. Without it, a name could be added to one side and silently drop out of
/// `--list`, or a strategy could move down and leave a stale arm here that shadows it.
#[test]
fn the_two_registry_halves_partition_the_roster() {
    use std::collections::BTreeSet;
    let roster: BTreeSet<&str> = STRATEGIES.iter().copied().collect();
    let portable: BTreeSet<&str> = vike_strategy::PORTABLE_STRATEGIES.iter().copied().collect();
    let sim_only: BTreeSet<&str> = vike_strategy::SIMULATOR_ONLY.iter().map(|(n, _)| *n).collect();
    assert!(
        portable.is_disjoint(&sim_only),
        "a name is claimed by BOTH halves: {:?}",
        portable.intersection(&sim_only).collect::<Vec<_>>()
    );
    let union: BTreeSet<&str> = portable.union(&sim_only).copied().collect();
    assert_eq!(
        roster,
        union,
        "STRATEGIES must be exactly PORTABLE_STRATEGIES ∪ SIMULATOR_ONLY — \
             only-in-roster: {:?}, only-in-halves: {:?}",
        roster.difference(&union).collect::<Vec<_>>(),
        union.difference(&roster).collect::<Vec<_>>()
    );
}

/// The other direction of the same gate: every name `vike_strategy::SIMULATOR_ONLY` claims must
/// really be an arm THIS `match` still owns — i.e. the portable resolver must NOT resolve it.
/// A strategy that moved down without its table row being deleted would otherwise keep telling
/// the daemon "simulator-only" about something it could actually mount.
#[test]
fn simulator_only_table_names_the_retained_arms() {
    let params = Value::Table(Default::default());
    for (name, why) in vike_strategy::SIMULATOR_ONLY {
        assert!(
            vike_strategy::strategy_by_name::<SimBroker>(name, &params).is_err(),
            "{name} is declared simulator-only but the PORTABLE registry resolves it — \
                 delete its SIMULATOR_ONLY row"
        );
        assert!(
            strategy_by_name(name, &params).is_ok(),
            "{name} is declared simulator-only but THIS registry does not resolve it either"
        );
        assert!(!why.is_empty(), "{name}'s simulator-only reason must not be empty");
    }
}

/// The THIRD row-set, gated in both directions. `vike_strategy::SCRIPT_ONLY` names strategies
/// that resolve HERE and not in the portable registry — the shape [`STRATEGIES`] deliberately
/// cannot express, because a script arm needs a `src` param and every roster consumer resolves
/// each name with EMPTY params.
///
/// Without this the table would be a claim about another crate that nothing checks, and
/// `vike_strategy::capability` would keep telling an operator "it backtests, this daemon does
/// not link the script host" long after the arm moved or died.
#[test]
fn script_only_names_resolve_there_and_not_here() {
    let empty = Value::Table(Default::default());
    for (name, why) in vike_strategy::SCRIPT_ONLY {
        assert!(
            !STRATEGIES.contains(name),
            "{name} is on the native roster — it belongs in the partition test above, not in \
                 SCRIPT_ONLY"
        );
        assert!(
            vike_strategy::strategy_by_name::<SimBroker>(name, &empty).is_err(),
            "{name} is declared script-only but the PORTABLE registry resolves it"
        );
        // ...and THIS registry owns the arm: it fails on the MISSING PARAM, not on the name.
        match strategy_by_name(name, &empty) {
            Err(HarnessError::Validation(msg)) => assert!(
                msg.contains("src"),
                "{name}: this registry must own the arm and fail on its required param, got: \
                     {msg}"
            ),
            other => {
                panic!("{name}: expected a Validation error here, got Ok={}", other.is_ok())
            }
        }
        assert!(!why.is_empty(), "{name}'s script-only reason must not be empty");
    }
}

#[test]
fn rhai_resolves_through_the_registry_with_inline_src() {
    // An authored Rhai strategy backtests headlessly by resolving through the SAME
    // strategy_by_name path a compiled reference strategy does — the create->backtest keystone.
    let params: Value = toml::from_str("src = \"fn on_bar() { buy(1.0); }\"\nqty = 2.0\n").unwrap();
    assert!(strategy_by_name("rhai", &params).is_ok(), "inline rhai src should resolve");
}

#[test]
fn rhai_missing_src_is_a_validation_error() {
    // A profile that names `rhai` but forgot the script is a fail-fast Validation error at load
    // time — never a silent no-op strategy.
    let params = Value::Table(Default::default());
    match strategy_by_name("rhai", &params) {
        Err(HarnessError::Validation(msg)) => {
            assert!(msg.contains("src"), "names the gap: {msg}")
        }
        other => panic!("expected Validation error, got Ok={}", other.is_ok()),
    }
}

#[test]
fn rhai_compile_error_is_a_validation_error() {
    // A malformed script fails the profile fast (a Rhai parse error → HarnessError::Validation),
    // not a panic mid-backtest.
    let params: Value = toml::from_str("src = \"fn on_bar( { syntax error\"\n").unwrap();
    match strategy_by_name("rhai", &params) {
        Err(HarnessError::Validation(_)) => {}
        other => panic!("expected Validation error, got Ok={}", other.is_ok()),
    }
}

#[test]
fn rhai_is_resolvable_but_not_in_the_native_roster() {
    // The `rhai` arm resolves (with a src) but is DELIBERATELY excluded from STRATEGIES — it
    // needs a src, so it is not part of the default-resolvable native roster that downstream
    // consumers (the Studio native dropdown) enumerate. This pins that separation.
    assert!(!STRATEGIES.contains(&"rhai"), "rhai must NOT be in the native roster");
    let params: Value = toml::from_str("src = \"fn on_bar() {}\"\n").unwrap();
    assert!(strategy_by_name("rhai", &params).is_ok(), "yet the rhai arm still resolves");
}

#[test]
fn rhai_strategy_runs_end_to_end_through_the_engine() {
    // End-to-end: a registry-resolved Rhai strategy runs in the REAL batch StrategyEngine over
    // EVERY bar without panicking — proving the whole authored-strategy path (resolve -> box ->
    // engine -> order verbs). The script exercises the order path each bar (flip long/flat via
    // `market`/`position`); the ROBUST invariant asserted here is that the run completes every
    // bar. The completed-trade COUNT is fill-timing-dependent, so it is NOT asserted — the
    // byte-parity gate `tests/parity/rhai_parity.rs` already proves a RhaiStrategy trades
    // correctly.
    // Bars MUST carry a symbol (a `None`-symbol bar routes `market`/`position` through `""`,
    // which SimBroker panics on — see rhai_parity's on_start_hook_does_not_panic_sim_broker).
    let params: Value = toml::from_str(
            "src = \"fn on_bar() { let p = position(); if p > 0.0 { market(-1, p); } else { market(1, 1.0); } }\"\n",
        )
        .unwrap();
    let strat = strategy_by_name("rhai", &params).expect("resolves");
    let sym_bar = |ts: i64, close: f64| Bar { symbol: Some("BTCUSDT".into()), ..bar(ts, close) };
    let bars: Vec<Bar> = (0..6).map(|i| sym_bar(60_000 * i, 100.0 + i as f64)).collect();
    let result =
        StrategyEngine::new(vec![("BTCUSDT".to_string(), bars)], strat, EngineParams::default())
            .run();
    assert_eq!(result.equity_curve.len(), 6, "the resolved rhai strategy runs every bar");
}

#[test]
fn buy_hold_from_params_reads_size_and_symbol() {
    let toml = r#"
size = 2.5
symbol = "ETHUSDT"
"#;
    // `toml::from_str` (a document parse), NOT `str::parse::<Value>` (a single-value parse
    // — it rejects multiple top-level `key = value` lines).
    let params: Value = toml::from_str(toml).unwrap();
    let strat = BuyHold::from_params(&params);
    assert_eq!(strat.size, 2.5);
    assert_eq!(strat.symbol.as_deref(), Some("ETHUSDT"));
}

#[test]
fn cheap_np_resolves_through_the_registry_with_its_params() {
    // The one registry entry that genuinely reads `params` beyond `buy_hold` — a typo'd
    // `spot_symbol` would silently never trade, so prove the table reaches the strategy.
    let params: Value = toml::from_str("spot_symbol = \"BTCUSDT\"\nmode = \"flip\"\n").unwrap();
    assert!(strategy_by_name("cheap_catch_updown_fair_value", &params).is_ok());
    let s = vike_strategy::cheap_np::CheapNp::from_params(&params);
    assert_eq!(s.spot_symbol, "BTCUSDT");
    assert_eq!(s.mode, vike_strategy::cheap_np::CheapNpMode::Flip);
}

#[test]
fn sport_taker_resolves_through_the_registry_with_its_params() {
    // Same concern as the `cheap_np` arm: a typo'd `wallets`/`conviction_usd` would silently
    // fall back to the Python defaults and never be noticed, so prove the table reaches it.
    let params: toml::Value =
        toml::from_str("wallets = [\"0xaaa\"]\nconviction_usd = 500.0\n").unwrap();
    assert!(strategy_by_name("sport_copy_follower", &params).is_ok());
    let s = SportTaker::from_params(&params);
    assert_eq!(s.wallets, vec!["0xaaa".to_string()]);
    assert_eq!(s.tracker().threshold(), 500.0);
}

#[test]
fn delegated_arms_still_read_their_params_through_this_registry() {
    // The DELEGATION's own risk: params reaching the portable resolver but the wrong arm (or an
    // empty table) reaching the strategy. One probe per param-driven delegated family, keeping
    // the coverage these tests had before the split.
    let g: Value = toml::from_str("step = 0.5\nrungs = 4\nsize = 2.0\nband = 3.0\n").unwrap();
    assert!(strategy_by_name("grid", &g).is_ok());
    let grid = vike_strategy::Grid::from_params(&g);
    assert_eq!(grid.step, 0.5);
    assert_eq!(grid.rungs, 4);
    assert_eq!(grid.size, 2.0);
    assert_eq!(grid.band, 3.0);

    let d: Value = toml::from_str("side = \"short\"\nstep = 1.5\nrungs = 3\ntp = 0.07\n").unwrap();
    assert!(strategy_by_name("dca_accumulate", &d).is_ok());
    assert_eq!(vike_strategy::DcaAccumulate::from_params(&d).side, -1);

    let m: Value = toml::from_str("qty = 5.0\ntick_size = 0.01\ngamma = 0.2\n").unwrap();
    assert!(strategy_by_name("spread_maker", &m).is_ok());
    assert_eq!(vike_mm::SpreadMaker::from_params(&m).unwrap().params().qty, 5.0);

    let mo: Value = toml::from_str("qty = 2.0\nthreshold = 5.0\n").unwrap();
    assert!(strategy_by_name("momentum", &mo).is_ok());
    assert_eq!(vike_strategy::MomentumController::from_params(&mo).qty, 2.0);

    let fcy: Value =
        toml::from_str("symbol = \"BTCUSDT\"\nqty = 3.0\nentry_threshold = 0.001\n").unwrap();
    assert!(strategy_by_name("funding_carry", &fcy).is_ok());
    assert_eq!(vike_strategy::FundingCarryController::from_params(&fcy).qty(), 3.0);

    let fc: Value =
        toml::from_str("threshold = 0.0005\nqty = 4.0\nsymbol = \"BTCUSDT\"\n").unwrap();
    assert!(strategy_by_name("funding_capture", &fc).is_ok());
    assert_eq!(vike_strategy::FundingCapture::from_params(&fc).qty, 4.0);
}

#[test]
fn buy_hold_boxed_via_registry_buys_once_on_first_bar_and_holds() {
    let params: Value = toml::from_str("size = 2.0\n").unwrap();
    let strategy: Box<dyn Strategy<SimBroker>> = strategy_by_name("buy_hold", &params).unwrap();
    let bars = vec![("BTCUSDT".to_string(), vec![bar(0, 100.0), bar(1, 101.0), bar(2, 102.0)])];
    let mut engine = StrategyEngine::new(bars, strategy, EngineParams::default());
    engine.run();
    // Exactly `size` (2.0), not `size * bar_count` — proves it bought ONCE and held rather
    // than re-submitting on every subsequent bar (`trades` only records CLOSED round-trips,
    // so an open-and-hold position is the observable signal here).
    assert_eq!(engine.core.position_of("BTCUSDT").size, 2.0);
}
