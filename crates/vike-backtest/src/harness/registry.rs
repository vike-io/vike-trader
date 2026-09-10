//! The backtest harness strategy registry: a name -> `Box<dyn Strategy<SimBroker>>` lookup the
//! profile's `strategy.name` (see [`super::profile::StrategyCfg`]) resolves through.
//!
//! ## This file is now the SIMULATOR HALF of a two-part registry
//! The PORTABLE half moved DOWN to `vike_strategy::registry`, where it is generic over the broker
//! (`strategy_by_name<B: HftBroker>`). The reason is a dependency one: this module's return type
//! named `SimBroker`, so "run the strategy I backtested" was impossible for `vike-tradehub` (the
//! headless daemon on the box that signs real orders) without depending on `vike-backtest` — which
//! drags the whole simulator and, under `datafusion-store`, the Arrow/DataFusion tree into that
//! binary. That is the exact cost `vike-ops` and `vike-alerting` were split out to avoid.
//!
//! What stays HERE is what genuinely cannot leave: five `impl Strategy<SimBroker>` reference
//! strategies (see [`crate::ref_strategies`]'s module doc for the simulator machinery each names),
//! two portable strategies that reach this crate's own internal modules (`cheap_np`, `sport_taker`),
//! and the `rhai` script arm (whose `vike-script` dependency sits at the SAME layer rank as
//! `vike-strategy`, so that crate cannot name it). [`STRATEGIES`] is the UNION of both halves and is
//! unchanged — same names, same order (`the_two_registry_halves_partition_the_roster` is what says
//! so, rather than a count in this sentence) — so every downstream consumer
//! (`vike_datahub::server`'s `Response::Strategies`, `bin/backtest.rs`'s `--list`,
//! `vike_studio_core`) sees exactly what it saw before.
//!
//! Dispatch through this registry is deliberately `dyn` (the module doc on
//! `vike_model::strategy`'s blanket `impl<B: Broker> Strategy<B> for Box<dyn Strategy<B>>`
//! explains why that is safe off the hot loop): the harness runs ONE strategy per profile, so a
//! single vtable indirection per bar/tick is noise next to loading and replaying the data slice.

use toml::Value;

use vike_model::Strategy;

use super::HarnessError;
use crate::cheap_np::CheapNp;
use crate::engine::SimBroker;
use crate::ref_strategies::{
    BracketPerSymbol, CapsSizersMask, GatedWeights, RotationTopK, TickPairMse,
};
use crate::sport_taker::SportTaker;
use vike_script::RhaiStrategy;

/// The registry's smoke fixture, re-exported at its historical path so
/// `vike_backtest::harness::BuyHold` still resolves. It moved to `vike-strategy` WITH the portable
/// registry it exists to smoke-test.
pub use vike_strategy::BuyHold;

/// The NATIVE (compiled) strategy roster: every name [`strategy_by_name`] resolves WITH DEFAULT
/// PARAMS — kept in sync with that function's `match` PLUS the delegated
/// [`vike_strategy::PORTABLE_STRATEGIES`] half by `registry_lists_every_match_arm`, and consumed
/// downstream as the Studio's native-strategy list (`vike_studio_core::spec::native_strategies`).
/// The `"rhai"` match arm is DELIBERATELY NOT here: it needs a `src` param (it is not
/// default-resolvable, and it is the SCRIPT path, not a native strategy), so listing it would break
/// every consumer that enumerates this roster and resolves each with empty params.
///
/// ⚠ The ORDER is a wire-visible property (`vike_datahub_client::proto`'s `Response::Strategies`
/// documents "in its declared order"), so the registry split deliberately kept it byte-identical
/// rather than grouping the two halves.
pub const STRATEGIES: &[&str] = &[
    "buy_hold",
    "rotation_top_k",
    "bracket_per_symbol",
    "gated_weights",
    "caps_sizers_mask",
    "tick_pair_mse",
    "cheap_catch_updown_fair_value",
    "sport_copy_follower",
    "grid",
    "dca_accumulate",
    "spread_maker",
    "gueant_maker",
    "trailing_scalper",
    "momentum",
    "funding_carry",
    "funding_capture",
    "pairs_zscore",
];

/// Resolve a registry `name` + its TOML `params` table to a boxed strategy. Unknown names are a
/// [`HarnessError::Validation`] (the harness fails a profile fast, at load time, rather than
/// silently no-opping on a typo'd `strategy.name`).
///
/// The `match` below holds ONLY the arms that cannot leave this crate; everything else falls
/// through to [`vike_strategy::strategy_by_name`] monomorphised at this engine's own [`SimBroker`]
/// (legal because `crate::engine::sim_broker`'s `impl HftBroker for SimBroker` satisfies that
/// resolver's bound). The `+ Send` box it returns unsize-coerces to the plain
/// `Box<dyn Strategy<SimBroker>>` this signature promises, which is why the split needed no change
/// at any call site.
pub fn strategy_by_name(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<SimBroker>>, HarnessError> {
    match name {
        // The reference strategies (`vike_backtest::ref_strategies`) are all `#[derive(Default)]`
        // with no harness-tunable knobs today — `Default::default()` IS their documented ctor. All
        // five are `impl Strategy<SimBroker>` BY DESIGN (that module's doc names the simulator
        // machinery each reaches), which is why they stay in this crate.
        "rotation_top_k" => Ok(Box::new(RotationTopK::default())),
        "bracket_per_symbol" => Ok(Box::new(BracketPerSymbol)),
        "gated_weights" => Ok(Box::new(GatedWeights)),
        "caps_sizers_mask" => Ok(Box::new(CapsSizersMask)),
        "tick_pair_mse" => Ok(Box::new(TickPairMse::default())),
        // The Polymarket 5m up/down fair-value taker. Portable in principle (`impl<B: Broker>`) but
        // reaches this crate's internal `cheap_np_ask`/`fair_value` modules, so it stays. Genuinely
        // param-driven (`spot_symbol` at minimum — see `CheapNp::from_params`), so it follows the
        // `BuyHold` reader convention rather than `Default::default()`.
        "cheap_catch_updown_fair_value" => Ok(Box::new(CheapNp::from_params(params))),
        // The Polymarket sports/esports copy-trading taker. Param-driven like `cheap_np` (its
        // `wallets` allow-list and `conviction_usd` threshold are the knobs a sweep varies), and
        // it reads its signal off a SIGNAL series whose symbol carries the copied wallet — see
        // `crate::sport_taker`'s module doc for the grammar.
        "sport_copy_follower" => Ok(Box::new(SportTaker::from_params(params))),
        // The AUTHORED-strategy arm (the create->backtest keystone): compile an inline Rhai `src`
        // into a `RhaiStrategy<SimBroker>` — which is a `Strategy<SimBroker>` like every compiled
        // reference strategy, so it backtests headlessly through this SAME resolution path (no GUI
        // Studio). `src` is REQUIRED (a missing one is a fail-fast Validation error, never a silent
        // no-op); every OTHER numeric param is a `param(name, default)` override (the same knobs a
        // `[sweep]` varies), baked into the script scope at compile via `compile_with_params`. A
        // Rhai parse/compile error surfaces as `Validation` at load time, not a mid-backtest panic.
        //
        // It stays in THIS crate because `vike-script` declares the SAME layer rank (30) as
        // `vike-strategy`, so the portable registry cannot name it. The supply-chain half of the
        // old rationale — "a scripting engine in the binary that signs real orders is a decision,
        // not a default" — was DECIDED on 2026-08-18: the daemon links vike-script and mounts
        // scripts live, by PATH (`[strategy] rhai = "<path>"`), under the rails
        // `docs/decisions/0024-rhai-strategies-live.md` names. This arm remains the BACKTEST
        // spelling (inline `src`), unchanged.
        "rhai" => {
            let src = params.get("src").and_then(Value::as_str).ok_or_else(|| {
                HarnessError::Validation(
                    "rhai strategy requires a `src` param (the inline Rhai script source)"
                        .to_string(),
                )
            })?;
            let strat = RhaiStrategy::<SimBroker>::compile_with_params(src, rhai_overrides(params))
                .map_err(|e| HarnessError::Validation(format!("rhai compile error: {e}")))?;
            Ok(Box::new(strat))
        }
        // Everything else is PORTABLE — resolved by the shared registry at `B = SimBroker`.
        other => match vike_strategy::strategy_by_name::<SimBroker>(other, params) {
            Ok(boxed) => {
                // The one coercion the split needs: `Box<dyn Strategy<SimBroker> + Send>` ->
                // `Box<dyn Strategy<SimBroker>>` (dropping an auto-trait is an unsizing coercion,
                // so this `let` is the whole conversion — no re-box, no allocation).
                let plain: Box<dyn Strategy<SimBroker>> = boxed;
                Ok(plain)
            }
            // Not a built-in: consult the USER registry (compiled from user_data/strategies/rust —
            // empty in any checkout without one) before giving up. Built-in arms were tried first,
            // so a user folder can never shadow a registry name.
            Err(vike_strategy::RegistryError::Unknown(n)) => {
                match vike_user_strategies::user_strategy_by_name::<SimBroker>(&n, params) {
                    Some(boxed) => {
                        let plain: Box<dyn Strategy<SimBroker>> = boxed;
                        Ok(plain)
                    }
                    // Re-word with THIS crate's roster: the portable half knows only its own 10
                    // names, but a backtest profile may legitimately name any of the built-ins —
                    // or a user strategy, whose roster is named separately so an operator can tell
                    // "not compiled in" from "typo'd a built-in".
                    None => Err(HarnessError::Validation(format!(
                        "unknown strategy {n:?} (known: {}; user: {})",
                        STRATEGIES.join(", "),
                        if vike_user_strategies::USER_STRATEGIES.is_empty() {
                            "none compiled in".to_string()
                        } else {
                            vike_user_strategies::USER_STRATEGIES.join(", ")
                        },
                    ))),
                }
            }
            Err(e) => Err(HarnessError::Validation(e.to_string())),
        },
    }
}

/// Read a TOML value as `f64`, accepting either a TOML float or a TOML integer (`size = 1` reads
/// the same as `size = 1.0` — a profile author should not have to remember which).
fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Collect a Rhai strategy's numeric params (every key EXCEPT the reserved `src`) into the
/// `param(name, default)` override map [`RhaiStrategy::compile_with_params`] bakes into the script
/// scope at compile — the same knobs a `[sweep]` grids over. Non-numeric params (e.g. a stray
/// string) are ignored, matching the lenient reader convention the compiled arms use.
fn rhai_overrides(params: &Value) -> indexmap::IndexMap<String, f64> {
    params
        .as_table()
        .map(|t| {
            t.iter()
                .filter(|(k, _)| k.as_str() != "src")
                .filter_map(|(k, v)| as_f64(v).map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineParams, StrategyEngine};
    use vike_model::{Bar, Broker};

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
        let sim_only: BTreeSet<&str> =
            vike_strategy::SIMULATOR_ONLY.iter().map(|(n, _)| *n).collect();
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
        let params: Value =
            toml::from_str("src = \"fn on_bar() { buy(1.0); }\"\nqty = 2.0\n").unwrap();
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
        let sym_bar =
            |ts: i64, close: f64| Bar { symbol: Some("BTCUSDT".into()), ..bar(ts, close) };
        let bars: Vec<Bar> = (0..6).map(|i| sym_bar(60_000 * i, 100.0 + i as f64)).collect();
        let result = StrategyEngine::new(
            vec![("BTCUSDT".to_string(), bars)],
            strat,
            EngineParams::default(),
        )
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
        let s = crate::cheap_np::CheapNp::from_params(&params);
        assert_eq!(s.spot_symbol, "BTCUSDT");
        assert_eq!(s.mode, crate::cheap_np::CheapNpMode::Flip);
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

        let d: Value =
            toml::from_str("side = \"short\"\nstep = 1.5\nrungs = 3\ntp = 0.07\n").unwrap();
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
}
