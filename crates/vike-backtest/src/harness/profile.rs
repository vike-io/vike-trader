//! `BacktestProfile` — the TOML config for the backtest harness (`BacktestNode`-lite).
//!
//! A profile names a venue/symbol/interval data slice, an engine cost/cash configuration, and a
//! strategy (by registry name + arbitrary TOML params — the name resolves through
//! [`super::registry`]). `from`/`to` accept either a bare epoch-ms integer (as a string) or a
//! `YYYY-MM-DDTHH` UTC hour label (mirrors `vike-backfill`'s `pmxt_backfill` hour-range
//! convention); both are resolved to epoch-ms by [`BacktestProfile::range`].
//!
//! # Profile widening (`cheap_np` port backlog G4/G6/G7)
//!
//! Three knobs the engine already had but TOML could not reach, added without changing any
//! existing profile's meaning:
//!
//! * **G4 — cross-venue / multi-series slices.** [`DataCfg`] accepts EITHER the frozen
//!   `venue = "…"` + `symbols = [ … ]` single-venue pair OR an `[[data.series]]` array whose
//!   entries each carry their own `venue`/`symbol`/`kind`. Exactly one of the two forms must be
//!   present — mixing them is a validation error, not a silent precedence rule.
//! * **G6 — binary-resolution settlement.** `[engine.resolution]` builds the
//!   [`vike_sim::EngineParams::resolution`] source (and its `resolution_end_ts`) that
//!   `SimBroker::settle_at_payout` has always consumed but no profile could configure.
//! * **G7 — a real fee SCHEDULE, not just a flat rate.** `[engine.fee]` selects a
//!   [`vike_model::FeeSchedule`]; the prediction-market `probability_scaled` curve
//!   (`qty × rate × p(1−p)`) is the shape a flat `fee_rate` cannot express at all.
//!   ⚠ It was the ONLY shape this table reached for a long time, so the equities per-share +
//!   MINIMUM shape and Deribit's premium-capped options shape were costs a profile could not
//!   name. [`FeeCfg`] reaches all five now, plus `kind = "venue"` — the venue's own published
//!   schedule, resolved through the same `fee_lane` + `fee_schedule_for` pair the paper mount
//!   uses, so a backtest and the paper mount of one instrument cannot disagree about cost for no
//!   reason but a missing lookup. That type's doc carries the argument for each.
//!
//! ```toml
//! [data]
//! kind = "tick"
//! from = "1775000000000"
//! to   = "1775002000000"
//!
//! [[data.series]]                       # the reference series: quotes only, its own venue
//! venue = "spot"
//! symbol = "BTCUSDT"
//! kind = "quote"
//!
//! [[data.series]]                       # the tradeable outcome tokens: the taker tape only
//! venue = "polymarket"
//! symbol = "btc-updown-5m-1775001600#0"
//! kind = "trade"
//!
//! [engine]
//! cash = 1000.0
//!
//! [engine.fee]
//! kind = "probability_scaled"
//! taker_rate = 0.072
//!
//! [engine.resolution]
//! kind = "binary_outcome"
//! [engine.resolution.winners]
//! "btc-updown-5m-1775001600" = 0
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::HarnessError;
use vike_data::TsRange;
use vike_exec::ProfileRisk;
use vike_sim::DecideMode;

#[cfg(doc)]
use super::report;

mod data_cfg;
mod engine_cfg;
mod fee_cfg;
mod impact_cfg;
mod resolution_cfg;
mod sizer_cfg;
mod validate;
mod walkforward_cfg;

pub use data_cfg::{DataCfg, DataKind};
pub use engine_cfg::EngineCfg;
pub use fee_cfg::FeeCfg;
pub use impact_cfg::ImpactCfg;
pub use resolution_cfg::ResolutionCfg;
pub use sizer_cfg::{MAX_SIZER_DEPTH, SizerCfg};
pub use walkforward_cfg::{WalkforwardCfg, WindowForm};

/// The top-level backtest profile: what data to run, how the engine is configured, and which
/// strategy to run. Unknown top-level keys are a hard parse error (typos should not silently
/// no-op).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestProfile {
    /// Free-form label (logs/report titles only — not used for lookup).
    #[serde(default)]
    pub name: Option<String>,
    pub data: DataCfg,
    pub engine: EngineCfg,
    pub strategy: StrategyCfg,
    /// Optional pre-trade `RiskGate` limits (runprofile-wiring-step2). The SAME `[risk]` schema
    /// AND the SAME compile-checked converter ([`vike_exec::ProfileRisk::to_risk_limits`])
    /// `vike-core`'s `RunProfile` uses for paper/live — so a limit proven here (via
    /// [`BacktestProfile::validate`] + a real `SimBroker` denial) carries unchanged into paper and
    /// live, rather than being re-derived by a parallel converter that could drift. Absent (the
    /// default) ⇒ [`vike_sim::EngineParams::risk_limits`] stays `None` ⇒ byte-identical to every
    /// profile written before this field existed: `[engine]` today has no `leverage` knob either,
    /// so `[risk]` is the ONLY way a harness profile arms the gate at all
    /// ([`vike_sim::SimBroker::build_risk_gate`]'s `(None, None) => None` arm).
    ///
    /// ONE FIELD IS REJECTED, not silently ignored: `risk.max_orders_per_window` is a WALL-CLOCK
    /// order-rate throttle, and sim time is not wall time (`build_risk_gate` always disarms it
    /// live-side too, for the identical reason) — see [`BacktestProfile::validate`]. Every other
    /// `risk.*` limit is honored exactly as the live gate honors it.
    #[serde(default)]
    pub risk: Option<ProfileRisk>,
    /// Optional PARAMETER-SEARCH grid: each key is a `strategy.params` field name, each value an
    /// array of TOML values to cross-product over (expansion lives in
    /// [`super::sweep::expand_paramscan`]). Absent or empty means "not a parameter search".
    ///
    /// # `[paramscan]` is the name; `[sweep]` loads FOREVER
    ///
    /// Ruling R2 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` renamed the
    /// SECTION: nineteen competitor CLIs were measured and exactly one uses the word "sweep" (an
    /// npm package), while QuantRocket calls it `paramscan`, LEAN `optimize` and Freqtrade
    /// `hyperopt` — the last of which would LIE here, because it names Bayesian search specifically
    /// and this engine has grid, euler, tpe and genetic.
    ///
    /// ⚠ **`#[serde(alias = "sweep")]` is PERMANENT and is not a deprecation with an end date.**
    /// Profiles exist on operators' disks and in this repository's own `profiles/*.toml` fixtures,
    /// and [`BacktestProfile`] is `#[serde(deny_unknown_fields)]` — so without the alias every
    /// `[sweep]` profile in the world would fail to load with "unknown field", which is the
    /// `VIKE_MAX_ORDER_NOTIONAL` shape (a written value an operator already has, refused) applied
    /// to a file rather than to an environment variable. The alias costs one attribute and makes
    /// the rename free. Removing it is not a future tidy-up; it is a breaking change to every
    /// profile ever written.
    ///
    /// ⚠ Writing BOTH spellings in one file is a serde duplicate-field error, which is the correct
    /// answer: two grids in one profile has no meaning, and a silent winner would be the
    /// different-answer defect this whole stage exists to end.
    #[serde(default, alias = "sweep")]
    pub paramscan: Option<toml::Table>,
    /// Optional anchored WALK-FORWARD config — the `[paramscan]` sibling: present means this profile
    /// can ALSO be run through [`crate::walkforward::runner::run_walkforward`] over
    /// `walkforward.n_splits` out-of-sample windows. Absent (the default) is byte-identical to
    /// every profile written before this field existed, and `run_backtest`/`run_paramscan` ignore the
    /// section entirely — exactly as they ignore a `[sweep]` table they were not asked to expand.
    ///
    /// It lives IN the profile (rather than riding a wire field) because [`BacktestProfile`] is
    /// `deny_unknown_fields`: a profile carrying `[walkforward]` must parse for the whole TOML to
    /// be shippable verbatim to a remote runner, which is the point of the compute-to-data
    /// `RunWalkforwardProfile` verb.
    #[serde(default)]
    pub walkforward: Option<WalkforwardCfg>,
    /// Directory the profile FILE was read from — set by [`BacktestProfile::from_path`], `None`
    /// for a profile parsed from a string. Relative paths inside the profile (today just
    /// `[engine.resolution].path`) resolve against it, so a profile + its sidecar CSV move
    /// together instead of depending on the caller's CWD. Never a TOML key (`#[serde(skip)]`).
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

/// Strategy selection: a registry name (resolved by [`super::registry`]) plus arbitrary TOML params
/// the strategy constructor interprets itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyCfg {
    pub name: String,
    #[serde(default = "default_params")]
    pub params: toml::Value,
}

fn default_params() -> toml::Value {
    toml::Value::Table(Default::default())
}

impl BacktestProfile {
    /// Parse + validate a profile from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, HarnessError> {
        let profile: BacktestProfile =
            toml::from_str(s).map_err(|e| HarnessError::Parse(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Parse + validate a profile from a file on disk, handing back the TEXT it was parsed from.
    ///
    /// Records the file's directory as [`Self::base_dir`] so relative sidecar paths (e.g.
    /// `[engine.resolution].path`) resolve next to the profile rather than against the caller's CWD.
    ///
    /// ⚠ **The text is returned rather than discarded so a run record can store the bytes that
    /// actually drove the run.** Re-reading the file at persist time would be a different read: an
    /// operator editing a profile while a long backtest runs is ordinary, and a record holding
    /// bytes the run did NOT use is worse than no record, because it looks authoritative.
    pub fn from_path_with_text(path: &Path) -> Result<(Self, String), HarnessError> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| HarnessError::Io(format!("{}: {e}", path.display())))?;
        let mut profile = Self::from_toml_str(&s)?;
        profile.base_dir = path.parent().map(Path::to_path_buf);
        Ok((profile, s))
    }

    /// Parse + validate a profile from a file on disk. [`Self::from_path_with_text`] when the
    /// caller also wants the text; this is the door for callers that do not, and it DELEGATES so
    /// there is one parse and one `base_dir` rule rather than two.
    pub fn from_path(path: &Path) -> Result<Self, HarnessError> {
        Self::from_path_with_text(path).map(|(profile, _)| profile)
    }

    /// Resolve `data.from`/`data.to` to an inclusive epoch-ms [`TsRange`].
    pub fn range(&self) -> Result<TsRange, HarnessError> {
        let start = parse_ts(&self.data.from)?;
        let end = parse_ts(&self.data.to)?;
        Ok(TsRange::of(start, end))
    }
}

impl BacktestProfile {
    /// True if `paramscan` is present and non-empty — the marker that this profile expands into a
    /// parameter grid ([`super::sweep::expand_paramscan`]) rather than running as a single backtest.
    ///
    /// ⚠ **NOT to be confused with `vike_data::store::removal::SeriesSelector::is_sweep`**, which is a
    /// different concept in a different crate and, until this rename, wore the identical bare name
    /// and call syntax: how broadly a data-store DELETION selector reaches. That one is a
    /// bulk-delete safety gate and KEEPS its name — `crates/vike-datahub/src/server/delete.rs`'s
    /// `delete_series_verb` still calls `selector.is_sweep()`, and a mechanical rename that took
    /// both would have silently re-scoped a bulk delete. This one is the parameter search, and
    /// renaming it is what disambiguates the two. Both spellings COMPILE at either site, so the
    /// only thing separating them is which meaning was intended.
    pub fn is_paramscan(&self) -> bool {
        self.paramscan.as_ref().is_some_and(|t| !t.is_empty())
    }
}

/// Resolve [`EngineCfg::decide`] to the engine's own [`DecideMode`].
///
/// A FREE function taking the raw string rather than a `&self` method beside
/// [`EngineCfg::fill_model_kind`], for one mechanical reason worth stating so nobody "tidies" it
/// back: `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs` requires the literal
/// `profile.engine.<field>` to appear as a WHOLE identifier at each construction site, and
/// `profile.engine.decide_mode()` is `decide` followed by `_` — not an identifier boundary, so the
/// gate reads the field as unreached and the only cure is a hand-written exemption row in a crate
/// this one cannot see. Passing the value (`decide_mode(profile.engine.decide.as_deref())`) leaves
/// the read spelled the way the gate can check, at no cost to the parse.
///
/// Absent ⇒ [`DecideMode::Sequential`], byte-identical to before the key existed. An unrecognised
/// spelling is REFUSED rather than falling back: falling back would hand a typo the default and
/// report success, and the whole point of the key is that a run says out loud which of the two
/// cross-section answers it computed.
pub(crate) fn decide_mode(raw: Option<&str>) -> Result<DecideMode, HarnessError> {
    let Some(raw) = raw else {
        return Ok(DecideMode::Sequential);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "sequential" => Ok(DecideMode::Sequential),
        "simultaneous" => Ok(DecideMode::Simultaneous),
        other => Err(HarnessError::Validation(format!(
            "unknown engine.decide {other:?} (want sequential | simultaneous; absent = sequential, \
             the per-symbol walk in the order data.symbols lists them)"
        ))),
    }
}

/// Parse a `from`/`to` field: a bare epoch-ms integer first, else a `YYYY-MM-DDTHH` UTC hour
/// label (Howard-Hinnant civil-calendar math, mirrors `vike-backfill`'s `pmxt_backfill` hour
/// range helpers). An hour label with no minutes/seconds means `:00:00`.
pub(crate) fn parse_ts(s: &str) -> Result<i64, HarnessError> {
    if let Ok(ms) = s.parse::<i64>() {
        return Ok(ms);
    }
    vike_model::time::parse_hour_label(s)
        .map(|(y, m, d, h)| {
            vike_model::time::days_from_civil(y, m, d) * 86_400_000 + h as i64 * 3_600_000
        })
        .ok_or_else(|| {
            let mut msg = String::new();
            let _ = write!(msg, "invalid timestamp {s:?}: expected epoch-ms or YYYY-MM-DDTHH");
            HarnessError::Parse(msg)
        })
}

#[cfg(test)]
use crate::hist_replay::{SeriesKind, SeriesRef};
#[cfg(test)]
use vike_model::FeeSchedule;
#[cfg(test)]
use vike_sim::{FillModelKind, QueueModelKind};

#[cfg(test)]
mod tests;
