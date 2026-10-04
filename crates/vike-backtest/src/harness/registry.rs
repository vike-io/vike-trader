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
//! strategies (see `vike_sim`'s `ref_strategies` module doc for the simulator machinery each names),
//! two portable strategies (`vike_strategy::strategies::{cheap_np, sport_taker}`) that have no
//! portable-registry row yet, and the `rhai` script arm. ⚠ That arm's reason for staying here CHANGED on 2026-09-23 and the
//! sentence here used to carry the old one: `vike-script` sat at the SAME layer rank as
//! `vike-strategy`, so the portable registry could not name it and the layer gate said so on every
//! PR. `vike-script` moved to `domain` (20) — its own ceiling, and a tightening of what a user's
//! Rhai script may reach — so that edge is now PERMITTED by rank. What refuses it instead is
//! `crates/vike-ops/tests/named_run_closure_gate.rs`, which walks the whole transitive normal
//! closure of `vike-user-strategies` and forbids `vike-script` anywhere in it — and `vike-strategy`
//! is IN that closure. The protection is unchanged in force and stronger in kind: it was a rank
//! coincidence, it is now a gate written for the purpose. [`STRATEGIES`] is the UNION of both
//! halves and is unchanged — same names, same order (`the_two_registry_halves_partition_the_roster`
//! is what says so, rather than a count in this sentence) — so every downstream consumer
//! (`vike_datahub::server`'s `Response::Strategies`, `bin/backtest.rs`'s `--list`,
//! `vike_studio_core`) sees exactly what it saw before.
//!
//! Dispatch through this registry is deliberately `dyn` (the module doc on
//! `vike_model::strategy`'s blanket `impl<B: Broker> Strategy<B> for Box<dyn Strategy<B>>`
//! explains why that is safe off the hot loop): the harness runs ONE strategy per profile, so a
//! single vtable indirection per bar/tick is noise next to loading and replaying the data slice.

use toml::Value;

use vike_model::{RESERVED_SRC_KEY, Strategy};

use super::HarnessError;
use vike_script::RhaiStrategy;
use vike_sim::{
    BracketPerSymbol, CapsSizersMask, GatedWeights, RotationTopK, SimBroker, TickPairMse,
};
use vike_strategy::CheapNp;
use vike_strategy::strategies::sport_taker::SportTaker;

// ⚠ `pub use vike_strategy::BuyHold;` stood here "so `vike_backtest::harness::BuyHold` still
// resolves" after the registry's smoke fixture moved to `vike-strategy` with the portable registry
// it smoke-tests — and `harness/mod.rs` re-exported it a second time. Both were the alias shape the
// root `CLAUDE.md`'s one-name rule forbids, with no caller outside this crate, and both were retired
// with the simulator split (docs/decisions/0087). Its only reader here is this file's own tests,
// which name `vike_strategy::BuyHold`.

/// The NATIVE (compiled) strategy roster: every name [`strategy_by_name`] resolves WITH DEFAULT
/// PARAMS — kept in sync with that function's `match` PLUS the delegated
/// [`vike_strategy::PORTABLE_STRATEGIES`] half by `registry_lists_every_match_arm`, and consumed
/// downstream as the Studio's native-strategy list (`vike_studio_core::native_strategies`).
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
/// (legal because `vike_sim`'s `impl HftBroker for SimBroker` satisfies that
/// resolver's bound). The `+ Send` box it returns unsize-coerces to the plain
/// `Box<dyn Strategy<SimBroker>>` this signature promises, which is why the split needed no change
/// at any call site.
pub fn strategy_by_name(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<SimBroker>>, HarnessError> {
    match name {
        // The reference strategies (`vike_sim::{RotationTopK, …}`) are all `#[derive(Default)]`
        // with no harness-tunable knobs today — `Default::default()` IS their documented ctor. All
        // five are `impl Strategy<SimBroker>` BY DESIGN (that module's doc names the simulator
        // machinery each reaches), which is why they stay in this crate.
        "rotation_top_k" => Ok(Box::new(RotationTopK::default())),
        "bracket_per_symbol" => Ok(Box::new(BracketPerSymbol)),
        "gated_weights" => Ok(Box::new(GatedWeights)),
        "caps_sizers_mask" => Ok(Box::new(CapsSizersMask)),
        "tick_pair_mse" => Ok(Box::new(TickPairMse::default())),
        // The Polymarket 5m up/down fair-value taker. Portable in principle (`impl<B: Broker>`) and
        // already in `vike-strategy`, but with no portable-registry row yet
        // (`vike_strategy::SIMULATOR_ONLY` says why), so it resolves here. Genuinely
        // param-driven (`spot_symbol` at minimum — see `CheapNp::from_params`), so it follows the
        // `BuyHold` reader convention rather than `Default::default()`.
        "cheap_catch_updown_fair_value" => Ok(Box::new(CheapNp::from_params(params))),
        // The Polymarket sports/esports copy-trading taker. Param-driven like `cheap_np` (its
        // `wallets` allow-list and `conviction_usd` threshold are the knobs a sweep varies), and
        // it reads its signal off a SIGNAL series whose symbol carries the copied wallet — see
        // `vike_strategy::strategies::sport_taker`'s module doc for the grammar.
        "sport_copy_follower" => Ok(Box::new(SportTaker::from_params(params))),
        // The AUTHORED-strategy arm (the create->backtest keystone): compile an inline Rhai `src`
        // into a `RhaiStrategy<SimBroker>` — which is a `Strategy<SimBroker>` like every compiled
        // reference strategy, so it backtests headlessly through this SAME resolution path (no GUI
        // Studio). `src` is REQUIRED (a missing one is a fail-fast Validation error, never a silent
        // no-op); every OTHER numeric param is a `param(name, default)` override (the same knobs a
        // `[sweep]` varies), baked into the script scope at compile via `compile_with_params`. A
        // Rhai parse/compile error surfaces as `Validation` at load time, not a mid-backtest panic.
        //
        // It stays in THIS crate because the portable registry cannot name `vike-script`. ⚠ WHAT
        // REFUSES THAT EDGE CHANGED on 2026-09-23, and this comment used to name the old refusal:
        // `vike-script` declared the SAME layer rank (30) as `vike-strategy`, so `layer_gate.rs`
        // failed the edge on every PR. `vike-script` moved to `domain` (20) — its own ceiling, and
        // a tightening of what a user's Rhai script may reach — so by RANK that edge is now legal.
        // It is refused instead by `crates/vike-ops/tests/named_run_closure_gate.rs`, which walks
        // the whole transitive normal closure of `vike-user-strategies` and forbids `vike-script`
        // anywhere in it; `vike-strategy` is in that closure, so an edge added there fails that
        // gate. Same force, better kind: a rank coincidence became a gate written for the purpose.
        // The supply-chain half of the
        // old rationale — "a scripting engine in the binary that signs real orders is a decision,
        // not a default" — was DECIDED on 2026-08-18: the daemon links vike-script and mounts
        // scripts live, by PATH (`[strategy] rhai = "<path>"`), under the rails
        // `docs/decisions/0024-rhai-strategies-live.md` names. This arm remains the BACKTEST
        // spelling (inline `src`), unchanged.
        "rhai" => {
            let src = params.get(RESERVED_SRC_KEY).and_then(Value::as_str).ok_or_else(|| {
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
                .filter(|(k, _)| k.as_str() != RESERVED_SRC_KEY)
                .filter_map(|(k, v)| as_f64(v).map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default()
}

#[path = "registry_tests.rs"]
#[cfg(test)]
mod registry_tests;
