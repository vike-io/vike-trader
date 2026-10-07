//! **The preset RESOLVER: `<strategy>/<preset>.toml` → the params a strategy is constructed
//! with.** The half that was missing while `Preset` was a type nothing ran.
//!
//! Ported from nothing: net-new Rust surface over the layout `load.rs` scans.
//!
//! # What a preset has to become, and why that differs per strategy kind
//!
//! There is exactly one destination: a `toml::Value` TABLE, the same one a profile's
//! `[strategy.params]` carries (`crates/vike-backtest/src/harness/profile.rs`'s `StrategyCfg`) and
//! the same one `crates/vike-backtest/src/harness/registry.rs`'s `strategy_by_name` hands to each
//! strategy's `from_params` reader. But the two [`StrategyBody`] kinds reach it by different roads,
//! and pretending otherwise is how a preset would silently do nothing:
//!
//! * **A built-in strategy takes the table WHOLE.** `StrategySpec::Native { name, params }` IS the
//!   construction call, so every key survives with its TOML type — `size = 3` an integer,
//!   `symbol = "BTCUSDT"` a string, a nested `[venues]` table a table.
//! * **A Rhai script can only receive NUMBERS.** Its knobs are `param(name, default)` calls, and
//!   `vike_script::RhaiStrategy::compile_with_params` bakes an `IndexMap<String, f64>` into the
//!   script scope — the same lane a `[sweep]` grid point uses, and the same one
//!   `registry.rs`'s `rhai_overrides` filters to. So a string or bool in a preset for a Rhai
//!   strategy has nowhere to go.
//!
//! ⚠ **The Rhai type gap is REPORTED, not swallowed.** `rhai_overrides` drops a non-numeric param
//! in silence ("Non-numeric params (e.g. a stray string) are ignored"), which is the lenient
//! convention for a params table somebody typed into a profile. It is the wrong answer for a
//! preset, whose whole promise is "these values, applied": a user who wrote `symbol = "BTCUSDT"` in
//! a preset for a Rhai script must be told it cannot arrive, not left to wonder why the run ignored
//! it. [`PresetRun::dropped`] carries those key names so the caller can say so.
//!
//! # This module resolves; it does not read the filesystem
//!
//! It takes an already-scanned [`LoadReport`] — the same shape as the rest of this tree: `load.rs`
//! reads, everything else works on what it found. That is also what makes "which presets DO exist"
//! answerable in the error ([`PresetError::UnknownPreset`] lists them), which is the whole
//! difference between a usable failure and `no such preset`.

use vike_model::Strategy;
use vike_sim::SimBroker;

use super::load::{LoadReport, Preset, StrategyBody, UserStrategy};
use crate::{RunError, StrategySpec, build_strategy_with};

/// Why a preset could not be resolved — both variants name what WAS found, because the question a
/// person is actually asking is "then what should I have typed".
#[derive(Debug, Clone, PartialEq)]
pub enum PresetError {
    /// No loaded strategy carries this name. `known` is every strategy that DID load, which is also
    /// how a user discovers that their folder failed to load at all (its name is simply absent —
    /// the reason is a [`super::LoadDiagnostic`] in the same report).
    UnknownStrategy { strategy: String, known: Vec<String> },
    /// The strategy loaded but has no preset by that name. `found` is every preset it does have.
    UnknownPreset { strategy: String, preset: String, found: Vec<String> },
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::UnknownStrategy { strategy, known } => {
                let list = if known.is_empty() {
                    "no strategy loaded at all — check the compile log for why".to_string()
                } else {
                    format!("loaded: {}", known.join(", "))
                };
                write!(f, "no strategy named '{strategy}' — {list}")
            }
            PresetError::UnknownPreset { strategy, preset, found } => {
                let list = if found.is_empty() {
                    format!("'{strategy}' has no presets at all")
                } else {
                    format!("'{strategy}' has: {}", found.join(", "))
                };
                write!(f, "no preset named '{preset}' — {list}")
            }
        }
    }
}

impl std::error::Error for PresetError {}

/// One preset, resolved into everything the two `run.rs` constructors need.
///
/// Deliberately NOT a `Box<dyn Strategy<SimBroker>>`: a caller usually wants to SHOW the resolution
/// (which params, which keys could not be carried) before or instead of building, and a sweep
/// builds one strategy per grid point from the same resolution. [`PresetRun::build`] is the one
/// call away.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetRun {
    /// The strategy to construct. Native carries the preset's table verbatim; Rhai carries the
    /// script source, its params riding in [`PresetRun::overrides`] instead — see the module doc.
    pub spec: StrategySpec,
    /// The `param(name, default)` overrides for the Rhai path, in the preset's key order. ALWAYS
    /// empty for a built-in strategy, whose params are already in `spec`.
    pub overrides: Vec<(String, f64)>,
    /// Preset keys that could NOT be carried — non-numeric values in a preset for a RHAI strategy.
    /// Always empty for a built-in one. A caller must surface these; see the module doc's ⚠.
    pub dropped: Vec<String>,
}

impl PresetRun {
    /// Construct the strategy this preset configures, through the SAME call a Run makes
    /// (`crates/vike-studio-core/src/run.rs`'s `build_strategy_with`) — so "the preset resolved"
    /// and "the preset runs" are one claim, exactly as the scan's compile check makes "it loaded"
    /// and "it will run" one claim.
    pub fn build(&self) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
        build_strategy_with(&self.spec, &self.overrides)
    }

    /// Did any preset key fail to reach the strategy? The one-line test a caller gates its warning
    /// on.
    pub fn is_lossless(&self) -> bool {
        self.dropped.is_empty()
    }
}

impl UserStrategy {
    /// This strategy's preset called `name`, or a [`PresetError::UnknownPreset`] listing the ones it
    /// does have.
    ///
    /// Case-INSENSITIVE, matching how `load.rs` claims preset names (`fast.toml` and `Fast.toml`
    /// are one preset, because on Windows and macOS they are one file).
    pub fn preset(&self, name: &str) -> Result<&Preset, PresetError> {
        self.presets.iter().find(|p| p.name.eq_ignore_ascii_case(name)).ok_or_else(|| {
            PresetError::UnknownPreset {
                strategy: self.name.clone(),
                preset: name.to_string(),
                found: self.presets.iter().map(|p| p.name.clone()).collect(),
            }
        })
    }

    /// This strategy, configured by `preset` — the resolution described in this module's doc.
    ///
    /// Takes any [`Preset`] rather than a name so a caller can resolve once and reuse; pass one of
    /// this strategy's own (see [`UserStrategy::preset`]), since a params table is only meaningful
    /// against the knobs the strategy actually reads.
    pub fn run_with(&self, preset: &Preset) -> PresetRun {
        match &self.body {
            // The table IS the construction argument — every key survives with its TOML type.
            StrategyBody::Native => PresetRun {
                spec: StrategySpec::native(&self.name, preset.params.clone()),
                overrides: Vec::new(),
                dropped: Vec::new(),
            },
            // Only numbers can reach a `param(name, default)`; the rest are named, not swallowed.
            StrategyBody::Rhai { source, .. } => {
                let (overrides, mut dropped) = split_numeric(&preset.params);
                dropped.extend(unread_keys(source, &overrides));
                dropped.sort();
                dropped.dedup();
                PresetRun { spec: StrategySpec::Rhai(source.clone()), overrides, dropped }
            }
        }
    }
}

/// Resolve `<strategy>/<preset>` against one scan.
///
/// The strategy is matched case-INSENSITIVELY across BOTH trees the scan covers, so
/// `resolve_preset(&report, "buy_hold", "aggressive")` finds
/// `user_data/strategies/rust/buy_hold/aggressive.toml` — a preset for a strategy whose code is in
/// the binary — exactly as it finds a Rhai one. Use `load_user_strategies` for the report: the
/// `rhai`-only scan cannot see the `rust/` tree at all.
pub fn resolve_preset(
    report: &LoadReport,
    strategy: &str,
    preset: &str,
) -> Result<PresetRun, PresetError> {
    let found = report.strategy(strategy).ok_or_else(|| PresetError::UnknownStrategy {
        strategy: strategy.to_string(),
        known: report.strategies.iter().map(|s| s.name.clone()).collect(),
    })?;
    let preset = found.preset(preset)?;
    Ok(found.run_with(preset))
}

/// Preset keys the script never reads — a TYPO, which is the failure this whole mechanism exists to
/// prevent.
///
/// ⚠ A numeric key with a misspelled name is the dangerous case, and `split_numeric` cannot see it:
/// `fastt = 5.0` is perfectly good TOML and perfectly numeric, so it lands in `overrides`, reaches
/// the engine, matches no `param(...)` call, and the run silently uses the DEFAULT. The user gets a
/// result for a configuration they did not ask for and nothing anywhere says so — precisely the
/// "silently ignored" outcome the design names as the thing to avoid.
///
/// It is catchable on this path and only on this path: a Rhai script DECLARES its knobs by calling
/// `param(name, default)`, and `vike_script::discover_params` runs the top level to collect them.
/// A NATIVE strategy has no such declaration — the registry has no param spec (see
/// `vike_studio_core::spec`'s module doc) — so its keys cannot be checked at all, and
/// [`UserStrategy::run_with`] deliberately does not try rather than pretending to.
///
/// A script that fails to compile yields NO declared set; the empty result means "cannot tell", so
/// nothing is reported rather than every key being flagged. The scan already refuses such a strategy
/// before a preset can reach it.
fn unread_keys(source: &str, overrides: &[(String, f64)]) -> Vec<String> {
    let Ok(declared) = vike_script::discover_params(source) else {
        return Vec::new();
    };
    if declared.is_empty() {
        return Vec::new();
    }
    overrides
        .iter()
        .map(|(k, _)| k)
        .filter(|k| !declared.iter().any(|(d, _)| d == *k))
        .cloned()
        .collect()
}

/// Split a params table into the f64-coercible pairs a Rhai script can receive and the key names it
/// cannot.
///
/// The numeric test is `registry.rs`'s `as_f64` — a TOML float OR integer — restated here because
/// that function is private to the harness. Order is the table's own (`toml::map::Map` is sorted),
/// so a resolution is deterministic and two runs of the same preset produce identical override
/// lists.
fn split_numeric(params: &toml::Value) -> (Vec<(String, f64)>, Vec<String>) {
    let mut numeric: Vec<(String, f64)> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let Some(table) = params.as_table() else {
        return (numeric, dropped);
    };
    for (key, value) in table {
        match value.as_float().or_else(|| value.as_integer().map(|i| i as f64)) {
            Some(n) => numeric.push((key.clone(), n)),
            None => dropped.push(key.clone()),
        }
    }
    (numeric, dropped)
}

#[path = "preset_tests.rs"]
#[cfg(test)]
mod preset_tests;
