//! `ProfileBacktest` — the TOML config for the backtest harness (`BacktestNode`-lite).
//!
//! A profile names a venue/symbol/interval data slice, an engine cost/cash configuration, and a
//! strategy (its FILE under the project's `strategies/` folder + arbitrary TOML params:
//! [`ProfileStrategy`]). `from`/`to` accept a bare epoch-ms integer (as a string), a
//! `YYYY-MM-DDTHH` UTC hour label (the convention of `vike_model::time::hours_in_range`) or a
//! plain `YYYY-MM-DD` UTC date (its midnight); each is resolved to epoch-ms by
//! [`ProfileBacktest::range`] (`vike_data::store::hist::parse_timestamp`).
//!
//! # Profile widening (`cheap_np` port backlog G4/G6/G7)
//!
//! Three knobs the engine already had but TOML could not reach, added without changing any
//! existing profile's meaning:
//!
//! * **G4 — cross-venue / multi-series slices.** [`ProfileData`] accepts EITHER the frozen
//!   `venue = "…"` + `symbols = [ … ]` single-venue pair OR an `[[data.series]]` array whose
//!   entries each carry their own `venue`/`symbol`/`kind`. Exactly one of the two forms must be
//!   present — mixing them is a validation error, not a silent precedence rule.
//! * **G6 — binary-resolution settlement.** `[engine.resolution]` builds the
//!   [`vike_simulator::StrategyEngineConfig::resolution`] source (and its `resolution_end_timestamp`) that
//!   `OrderBrokerSimulator::settle_at_payout` has always consumed but no profile could configure.
//! * **The costs are the strategy's, not the profile's** (the owner's ruling of 2026-11-18): a run's
//!   commission and slippage are strategy PARAMETERS (`[strategy.params] commission`, `slippage`, …),
//!   defaulting to what the file states, else none, and no venue table is applied
//!   (`crate::run::costs`). The run-level `[engine.fee]`, `engine.fee_rate` and `[engine.slippage]`
//!   are refused in words naming the param. A file that needs a shape no unit names (Deribit's
//!   option fee on its underlying, a maker and taker split, a per-share floor and cap) states it as
//!   its own `vike_model::Strategy::stated_fee`.
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
//! [engine.resolution]
//! kind = "binary_outcome"
//! [engine.resolution.winners]
//! "btc-updown-5m-1775001600" = 0
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::StrategySource;
use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use vike_data::TimestampRange;
use vike_model::ProfileRisk;
use vike_model::params::Params;
use vike_model::run_request::decide::DecideMode;
use vike_model::run_request::replay::ReplayKind;
use vike_strategy::file::scan::{self, StrategyFileLanguage};

#[cfg(doc)]
use super::report;

mod data_cfg;
mod engine_config;
mod mounts_config;
mod permutation_config;
mod resolution_config;
mod sizer_config;
mod validate;
mod walkforward_config;

pub use data_cfg::{BarSource, ProfileData, ProfileDataOptions};
pub use engine_config::resolve_conversions;
pub use engine_config::{ProfileEngine, QuotesConfig};
pub(crate) use engine_config::{
    checked_margin_call, in_bar_rules, optimistic_quotes, resolve_ledger_currency,
};
pub use mounts_config::ProfileMount;
pub use permutation_config::ProfilePermutation;
pub use resolution_config::ProfileEngineResolution;
pub use sizer_config::{MAX_SIZER_DEPTH, ProfileEngineSizer, SizerKind};
pub use walkforward_config::{ProfileWalkforward, WalkforwardWindowForm};

#[cfg(feature = "profile-schema")]
use crate::run::profile_surface::ParamsTable;

/// The top-level backtest profile: what data to run, how the engine is configured, and which
/// strategy to run. Unknown top-level keys are a hard parse error (typos should not silently
/// no-op).
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
// The fields serde never reads, each with the first paragraph of its doc (the schema leaves them out).
#[cfg_attr(feature = "profile-schema", schemars(extend("x-skipped-fields" = [
    {"field": "base_dir", "why": "Directory the profile FILE was read from — set by \
        [`ProfileBacktest::from_path`], `None` for a profile parsed from a string. Relative paths \
        inside the profile (today just `[engine.resolution].path`) resolve against it, so a profile + \
        its sidecar CSV move together instead of depending on the caller's CWD. Never a TOML key \
        (`#[serde(skip)]`)."},
    {"field": "kept", "why": "**The KEPT instrument rows the run reads its per-symbol facts from**: an \
        input's market class (its coverage's closed-time rule), a symbol's fiat quote (the ledger \
        guard) and an inverse contract's face (the coin ledger). Asked of the datahub ONCE per run by \
        the door that resolves the profile ([`ProfileBacktest::read_kept_rows`]: the backtesthub's \
        verbs, its local CLI, the Studio and a study; a sandboxed child asks its hub for them). Empty \
        (no door asked: a test, an in-process caller) reads every fact as before the lists reached a \
        backtest. Never a TOML key (`#[serde(skip)]`)."}
])))]
#[serde(deny_unknown_fields)]
pub struct ProfileBacktest {
    /// Free-form label (logs/report titles only — not used for lookup).
    #[serde(default)]
    pub name: Option<String>,
    pub data: ProfileData,
    pub engine: ProfileEngine,
    /// The one strategy the run trades. A profile names exactly one of this and [`Self::mounts`]
    /// (`ProfileBacktest::mount_refusals`); a door that runs one strategy reads it through
    /// [`ProfileBacktest::one_strategy`], every other door through [`ProfileBacktest::strategies`].
    #[serde(default)]
    pub strategy: Option<ProfileStrategy>,
    /// A PORTFOLIO: several strategy files over the run's one simulated account, dispatched in list
    /// order, as the live drain dispatches a node's mounts ([`ProfileMount`]). Empty (the default) is
    /// the one-mount run of [`Self::strategy`], byte-identical to a profile written before the key.
    #[serde(default)]
    pub mounts: Vec<ProfileMount>,
    /// Optional pre-trade `RiskGate` limits (runprofile-wiring-step2). The SAME `[risk]` schema
    /// AND the SAME compile-checked converter ([`vike_model::ProfileRisk::to_risk_limits`])
    /// `vike-core`'s `ProfileRun` uses for paper/live — so a limit proven here (via
    /// [`ProfileBacktest::validate`] + a real `OrderBrokerSimulator` denial) carries unchanged into paper and
    /// live, rather than being re-derived by a parallel converter that could drift. Absent (the
    /// default) ⇒ [`vike_simulator::StrategyEngineConfig::risk_limits`] stays `None` ⇒ byte-identical to every
    /// profile written before this field existed: `[engine]` today has no `leverage` knob either,
    /// so `[risk]` is the ONLY way a harness profile arms the gate at all
    /// ([`vike_simulator::OrderBrokerSimulator::build_risk_gate`]'s `(None, None) => None` arm).
    ///
    /// ONE FIELD IS REJECTED, not silently ignored: `risk.max_orders_per_window` is a WALL-CLOCK
    /// order-rate throttle, and sim time is not wall time (`build_risk_gate` always disarms it
    /// live-side too, for the identical reason) — see [`ProfileBacktest::validate`]. Every other
    /// `risk.*` limit is honored exactly as the live gate honors it.
    #[serde(default)]
    pub risk: Option<ProfileRisk>,
    /// Optional PARAMETER-SEARCH grid: each key is a `strategy.params` field name, each value an
    /// array of TOML values to cross-product over (expansion lives in
    /// [`crate::search::paramscan::expand_paramscan`]). Absent or empty means "not a parameter search".
    ///
    /// # `[paramscan]` is the name, and the only one
    ///
    /// Ruling R2 of `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` renamed the
    /// SECTION: nineteen competitor CLIs were measured and exactly one uses the word "sweep" (an
    /// npm package), while QuantRocket calls it `paramscan`, LEAN `optimize` and Freqtrade
    /// `hyperopt` — the last of which would LIE here, because it names Bayesian search specifically
    /// and this engine has grid, euler, tpe and genetic.
    ///
    /// ⚠ The rename first landed as a serde alias, so a `[sweep]` profile kept loading. The owner's
    /// ruling 20 (phase 4, 2026-11-12) removed it with the wire's `sweep` tags: one word for one
    /// thing. [`ProfileBacktest`] is `#[serde(deny_unknown_fields)]`, so a profile that still writes
    /// `[sweep]` is REFUSED at load with "unknown field `sweep`" (announced once); rename the table.
    ///
    /// The section is read by serde STRAIGHT into [`Params`] (the strategy-parameter vocabulary,
    /// `vike_model::params`), so a `paramscan = 5` scalar is a LOAD refusal (a parse error), not a
    /// grid that silently expands to nothing.
    #[cfg_attr(feature = "profile-schema", schemars(with = "Option<ParamsTable>"))]
    #[serde(default)]
    pub paramscan: Option<Params>,
    /// Optional anchored WALK-FORWARD config — the `[paramscan]` sibling: present means this profile
    /// can ALSO be run through [`crate::walkforward::runner::run_walkforward`] over
    /// `walkforward.n_splits` out-of-sample windows. Absent (the default) is byte-identical to
    /// every profile written before this field existed, and `run_backtest`/`run_paramscan` ignore the
    /// section entirely — exactly as they ignore a `[sweep]` table they were not asked to expand.
    ///
    /// It lives IN the profile (rather than riding a wire field) because [`ProfileBacktest`] is
    /// `deny_unknown_fields`: a profile carrying `[walkforward]` must parse for the whole TOML to
    /// be shippable verbatim to a remote runner, which is the point of the compute-to-data
    /// `RunWalkforwardProfile` verb.
    #[serde(default)]
    pub walkforward: Option<ProfileWalkforward>,
    /// Optional Monte Carlo PERMUTATION TEST of this profile (`[permutation]`,
    /// [`ProfilePermutation`]): present, the run is the real history scored against permuted ones
    /// (`crate::permutation::run_permutation_profile`, which the backtesthub's
    /// `RunBacktest` runs for such a profile); absent (the default) is byte-identical to every
    /// profile written before this field existed.
    #[serde(default)]
    pub permutation: Option<ProfilePermutation>,
    /// Optional REMAP of the series the strategy reads by name (decision 0271): each row is
    /// `"<the name the strategy's code writes>" = "<the series to read instead>"`, both in the input
    /// grammar, `VENUE:SYMBOL:INTERVAL` or `LANE:VENUE:SYMBOL:INTERVAL` (a user's CSV import is the
    /// lane `user-<label>`). A name the code writes works without a row, so absent (the default) reads
    /// every input as its code names it, and a profile written before this table is byte-identical. A
    /// row naming no input the strategy reads is refused when the run builds it
    /// (`crate::run::data_plan::resolve_inputs`).
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// Directory the profile FILE was read from — set by [`ProfileBacktest::from_path`], `None`
    /// for a profile parsed from a string. Relative paths inside the profile (today just
    /// `[engine.resolution].path`) resolve against it, so a profile + its sidecar CSV move
    /// together instead of depending on the caller's CWD. Never a TOML key (`#[serde(skip)]`).
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
    /// **The KEPT instrument rows the run reads its per-symbol facts from**: an input's market class (its coverage's
    /// closed-time rule), a symbol's fiat quote (the ledger guard) and an inverse contract's face (the coin ledger).
    /// Asked of the datahub ONCE per run by the door that resolves the profile ([`ProfileBacktest::read_kept_rows`]:
    /// the backtesthub's verbs, its local CLI, the Studio and a study; a sandboxed child asks its hub for them).
    /// Empty (no door asked: a test, an in-process caller) reads every fact as before the lists reached a
    /// backtest. Never a TOML key (`#[serde(skip)]`).
    #[cfg_attr(feature = "profile-schema", schemars(skip))]
    #[serde(skip)]
    pub kept: super::KeptRows,
}

/// Strategy selection: the strategy FILE plus the params table it reads.
///
/// `params` is the [`Params`] table serde reads straight out of `[strategy.params]`; a non-table
/// `params = 5` is a LOAD refusal (a parse error) rather than a bag every reader sees as empty.
/// The retired selection keys `name` and `rhai` are refused before the typed read, with the fix
/// ([`refuse_retired_selection`]).
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
// The fields serde never reads, each with the first paragraph of its doc (the schema leaves them out).
#[cfg_attr(feature = "profile-schema", schemars(extend("x-skipped-fields" = [
    {"field": "builder", "why": "How the strategy is built where this process does not build the file \
        itself: set by the door that owns one (a Studio editor buffer, the sandboxed child running a \
        `.rs` file); `ProfileStrategy::build` reads it first. Never a TOML key (`#[serde(skip)]`)."},
    {"field": "source", "why": "The file as a door read it (`ProfileStrategy::read_file`): its text and \
        sha256, which every result records. Never a TOML key (`#[serde(skip)]`)."}
])))]
#[serde(deny_unknown_fields)]
pub struct ProfileStrategy {
    /// The strategy file: its path under the project's `strategies/` folder (`trend/momentum.rs`).
    /// A `.rhai` file runs in this process; a `.rs` file runs in a sandboxed child process. The one
    /// selection key of the backtest and the daemon profile.
    pub file: String,
    #[cfg_attr(
        feature = "profile-schema",
        schemars(with = "ParamsTable", extend("x-default-function" = "default_params"))
    )]
    #[serde(default = "default_params")]
    pub params: Params,
    /// The SDK a `.rs` file is built against for THIS backtest (decision 0296): set, it overrides the
    /// file's own `[strategy] sdk` pin and the box's newest SDK, so one file runs against two SDKs
    /// (`vike-cli strategy sdk-diff`). Absent (the default) builds as the file says. A backtest key
    /// only: a daemon mount follows its file. Refused beside a `.rhai` file, which runs on this vike's
    /// own code.
    #[serde(default)]
    pub sdk: Option<u32>,
    /// How the strategy is built where this process does not build the file itself: set by the
    /// door that owns one (a Studio editor buffer, the sandboxed child running a `.rs` file);
    /// `ProfileStrategy::build` reads it first. Never a TOML key (`#[serde(skip)]`).
    #[cfg_attr(feature = "profile-schema", schemars(skip))]
    #[serde(skip)]
    pub builder: Option<super::strategy_builder::StrategyBuilder>,
    /// The file as a door read it (`ProfileStrategy::read_file`): its text and sha256, which every
    /// result records. Never a TOML key (`#[serde(skip)]`).
    #[cfg_attr(feature = "profile-schema", schemars(skip))]
    #[serde(skip)]
    pub source: Option<StrategySource>,
}

impl ProfileStrategy {
    /// A table naming `file` with `params` and nothing a door sets.
    pub fn new(file: impl Into<String>, params: Params) -> Self {
        ProfileStrategy { file: file.into(), params, sdk: None, builder: None, source: None }
    }

    /// Which language [`Self::file`] is written in, or the refusal of a path that is not a
    /// strategy file inside the `strategies/` folder (`vike_strategy::file::scan::resolve`'s rule:
    /// empty, absolute, climbing out with `..`, or neither `.rs` nor `.rhai`).
    pub fn language(&self) -> Result<StrategyFileLanguage, HarnessError> {
        let path = self.path_under(Path::new(""))?;
        // `resolve` refused every path `language_of` cannot name; Rust is the side never run here.
        Ok(scan::language_of(&path).unwrap_or(StrategyFileLanguage::Rust))
    }

    /// [`Self::file`] as a path under `strategies_dir`, refused as [`Self::language`] says.
    fn path_under(&self, strategies_dir: &Path) -> Result<std::path::PathBuf, HarnessError> {
        scan::resolve(strategies_dir, &self.file)
            .map_err(|e| HarnessError::from(ProfileRefusal::StrategyFilePathRefused { e }))
    }

    /// Read [`Self::file`] under `strategies_dir` (the project's `strategies/` folder) into
    /// [`Self::source`]. A file that cannot be read, or whose header does not parse, is refused
    /// naming the file, never run as another.
    pub fn read_file(&mut self, strategies_dir: &Path) -> Result<(), HarnessError> {
        let path = self.path_under(strategies_dir)?;
        // The folder scan lists a link unread; a run reads none either (a wire profile names this).
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(HarnessError::from(ProfileRefusal::StrategyFileIsLink {
                file: self.file.clone(),
            }));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            HarnessError::from(ProfileRefusal::StrategyFileUnreadable {
                file: self.file.clone(),
                path: path.display().to_string(),
                e: e.to_string(),
            })
        })?;
        self.accept_text(text)
    }

    /// [`Self::read_file`] over a text already read: the header's checks over the params, then the text
    /// as [`Self::source`]. The sandboxed child's door, which reads no folder: the hub read the file and
    /// sent its text, and the checks and the recorded sha256 are the same either side.
    pub fn accept_text(&mut self, text: String) -> Result<(), HarnessError> {
        let (source, header) = StrategySource::read(text, self.language()?).map_err(|e| {
            HarnessError::from(ProfileRefusal::StrategyFileHeaderRefused {
                file: self.file.clone(),
                e,
            })
        })?;
        // The header's load-time refusals over the params (`vike_strategy::file`'s format note): a
        // key it does not declare, a value of the wrong type, a value outside its range. An unmet
        // `needs` is reported by the live mount, never refused (today's gate rule). The cost params
        // every file has are the run's, judged where they are read (`super::costs`).
        let mut params = self.params.clone();
        for key in vike_model::COST_PARAMS {
            params.remove(key);
        }
        let mut refused: Vec<String> = Vec::new();
        let unknown = header.unknown_params(&params);
        if !unknown.is_empty() {
            refused.push(format!(
                "{} not declared by the file's header (it reads {})",
                unknown.join(", "),
                header.readable_keys()
            ));
        }
        refused.extend(header.mistyped_params(&params).iter().map(ToString::to_string));
        refused.extend(header.out_of_range_params(&params).iter().map(ToString::to_string));
        if !refused.is_empty() {
            return Err(HarnessError::from(ProfileRefusal::StrategyParamsRefused {
                file: self.file.clone(),
                refused: refused.join("; "),
            }));
        }
        self.source = Some(source);
        Ok(())
    }

    /// The sha256 a result records: the file's as read, or empty where no door read one (a
    /// stand-in built in this process).
    pub fn sha256(&self) -> &str {
        self.source.as_ref().map_or("", |s| s.sha256.as_str())
    }

    /// The SDK the file's artifact was BUILT against ([`StrategySource::sdk`], read off the artifact the
    /// sandboxed child loaded), `None` where none was recorded: what a result records beside
    /// [`Self::sha256`]. Not [`Self::sdk`], the SDK a profile ASKS for.
    pub fn built_sdk(&self) -> Option<u32> {
        self.source.as_ref().and_then(|s| s.sdk)
    }

    /// A `.rhai` file no door read: this process was handed a path, never its text.
    pub(crate) fn unread_file(&self) -> HarnessError {
        HarnessError::from(ProfileRefusal::StrategyFileNotRead { file: self.file.clone() })
    }

    /// A `.rhai` file whose script does not compile.
    pub(crate) fn rhai_compile_error(&self, error: &str) -> HarnessError {
        HarnessError::from(ProfileRefusal::StrategyFileRhaiCompileError {
            file: self.file.clone(),
            error: error.to_string(),
        })
    }

    /// A `.rs` file reached a door that has no sandboxed child to run it in. The seam the sandbox
    /// route fills: that door hands the run to a child, whose own harness is given the builder.
    pub(crate) fn no_sandbox_route(&self) -> HarnessError {
        HarnessError::from(ProfileRefusal::StrategyFileNoSandboxRoute { file: self.file.clone() })
    }
}

/// The retired selection keys, refused BEFORE the typed read so the refusal carries the fix
/// rather than serde's "unknown field": `[strategy] name` (a registry name; `"rhai"` with an
/// inline `src`) and `[strategy] rhai` (the daemon's old script path). One key, `file`, selects
/// a strategy now. `strategies_dir`, where a door knows it, is scanned for the file the old name
/// meant.
pub(crate) fn refuse_retired_selection(
    doc: &toml::Table,
    strategies_dir: Option<&Path>,
) -> Result<(), HarnessError> {
    let Some(strategy) = doc.get("strategy").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    if let Some(name) = strategy.get("name") {
        let name = name.as_str().unwrap_or_default();
        if name == "rhai" {
            return Err(HarnessError::from(ProfileRefusal::StrategyNameRhaiNoLongerRead));
        }
        let found = strategies_dir.and_then(|dir| retired_name_file(dir, name));
        let file = found.unwrap_or_else(|| format!("<folder>/{name}.rs"));
        return Err(HarnessError::from(ProfileRefusal::StrategyNameNoLongerRead {
            name: name.to_string(),
            file,
        }));
    }
    if let Some(path) = strategy.get("rhai") {
        let path = path.as_str().unwrap_or_default();
        return Err(HarnessError::from(ProfileRefusal::StrategyRhaiNoLongerRead {
            path: path.to_string(),
        }));
    }
    Ok(())
}

/// The file under `strategies_dir` a retired `name` meant: the first one the scan lists whose
/// file name is `<name>.rs` or `<name>.rhai`.
fn retired_name_file(strategies_dir: &Path, name: &str) -> Option<String> {
    let wanted = [format!("{name}.rs"), format!("{name}.rhai")];
    scan::scan(strategies_dir, None, None).files.into_iter().map(|entry| entry.path).find(|path| {
        let file = path.rsplit('/').next().unwrap_or(path);
        wanted.iter().any(|w| w == file)
    })
}

fn default_params() -> Params {
    Params::new()
}

impl ProfileBacktest {
    /// Parse + validate a profile from a TOML string. The run-level cost keys a strategy param
    /// replaced are refused first, in words naming the param (`crate::run::costs`'s
    /// `refuse_run_level_costs`), rather than as the type error the typed read would give them, and so
    /// are a heard stream's negative `delay_ms` (`data_cfg`'s `refuse_invalid_delays`) and a
    /// `[strategy]` table beside `[[mounts]]` (`mounts_config`'s `refuse_strategy_beside_mounts`).
    pub fn from_toml_str(s: &str) -> Result<Self, HarnessError> {
        Self::parse(s, None)
    }

    /// [`Self::from_toml_str`] for a process holding a project: a retired `name` is answered with
    /// the file the scan of `strategies_dir` finds. The door then reads the strategy file itself
    /// ([`ProfileStrategy::read_file`]), or hands a builder where it builds a `.rs` file another way.
    pub fn from_toml_str_in(s: &str, strategies_dir: &Path) -> Result<Self, HarnessError> {
        Self::parse(s, Some(strategies_dir))
    }

    fn parse(s: &str, strategies_dir: Option<&Path>) -> Result<Self, HarnessError> {
        if let Ok(doc) = s.parse::<toml::Table>() {
            super::costs::refuse_run_level_costs(&doc)?;
            refuse_retired_selection(&doc, strategies_dir)?;
            mounts_config::refuse_strategy_beside_mounts(&doc)?;
            data_cfg::refuse_invalid_delays(&doc)?;
        }
        let profile: ProfileBacktest =
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

    /// Resolve `data.from`/`data.to` to an inclusive epoch-ms [`TimestampRange`]. An EMPTY bound is an
    /// open end, the shape of a picked slice with no edge ([`Self::for_slice`]).
    pub fn range(&self) -> Result<TimestampRange, HarnessError> {
        let bound = |s: &str| (!s.is_empty()).then(|| parse_timestamp(s)).transpose();
        Ok(TimestampRange { start: bound(&self.data.from)?, end: bound(&self.data.to)? })
    }

    /// **The profile a picked SLICE runs as**, so a Studio run and a `vike-cli backtest run` of the
    /// same slice are one run: `symbols` of one `venue` over `range` (an open end is an empty
    /// `data.from`/`data.to`), bars at `interval` or the recorded ticks, `strategy`, and the
    /// cash and the funding switch the Studio's DTO carries. Every other key is a profile's default.
    ///
    /// The DTO carries `cash` (an absent one is [`vike_simulator::StrategyEngineConfig`]' default) and
    /// `attach_funding`, written as `engine.attach_funding` only when it is set (absent: OFF, as a profile
    /// reads it since 2026-11-21): a run's commission and slippage are strategy params, so they ride in
    /// `strategy`'s params like any other (`crate::run::costs`).
    pub fn for_slice(
        venue: &str,
        symbols: &[String],
        interval: &str,
        range: TimestampRange,
        kind: ReplayKind,
        costs: Option<&vike_backtesthub_client::wire_studio::StrategyEngineConfigWire>,
        strategy: ProfileStrategy,
    ) -> Result<Self, HarnessError> {
        // The text names the file and its SDK alone (so a `.rhai` file naming an SDK is refused as a
        // profile's is); the table itself (its params, how it builds) is put in after.
        let file_only = ProfileStrategy {
            sdk: strategy.sdk,
            ..ProfileStrategy::new(strategy.file.clone(), Params::new())
        };
        let text = Self::slice_text(venue, symbols, interval, range, kind, costs, &file_only)?;
        let mut profile = Self::from_toml_str(&text)?;
        profile.strategy = Some(strategy);
        Ok(profile)
    }

    /// [`Self::for_slice`]'s profile as TEXT, with `strategy`'s params under `[strategy.params]` (an empty table
    /// writes none) and its `sdk` when it names one: what the backtesthub keeps as a saved Studio run's `config.toml`, so the run reads back,
    /// and runs again, as the profile it was.
    pub fn slice_text(
        venue: &str,
        symbols: &[String],
        interval: &str,
        range: TimestampRange,
        kind: ReplayKind,
        costs: Option<&vike_backtesthub_client::wire_studio::StrategyEngineConfigWire>,
        strategy: &ProfileStrategy,
    ) -> Result<String, HarnessError> {
        let costs = costs.cloned().unwrap_or_default();
        // The wire's old cost fields (an older Studio's) are refused in words, never read.
        if costs.fee_rate.is_some() {
            return Err(HarnessError::from(ProfileRefusal::EngineStudioFeeNoLongerRead));
        }
        if costs.slippage.is_some()
            || costs.slippage_model.is_some()
            || costs.slippage_tick.is_some()
        {
            return Err(HarnessError::from(ProfileRefusal::EngineStudioSlippageNoLongerRead));
        }
        let defaults = vike_simulator::StrategyEngineConfig::default();
        let bound = |b: Option<i64>| b.map_or_else(String::new, |ms| ms.to_string());
        let mut data = toml::Table::new();
        data.insert("venue".into(), venue.into());
        data.insert("symbols".into(), symbols.to_vec().into());
        data.insert("from".into(), bound(range.start).into());
        data.insert("to".into(), bound(range.end).into());
        data.insert("kind".into(), kind.word().into());
        if kind == ReplayKind::Bar {
            data.insert("interval".into(), interval.into());
        }
        let mut engine = toml::Table::new();
        engine.insert("cash".into(), costs.cash.unwrap_or(defaults.cash).into());
        if let Some(attach_funding) = costs.attach_funding {
            engine.insert("attach_funding".into(), attach_funding.into());
        }
        let mut strategy_table = toml::Table::new();
        strategy_table.insert("file".into(), strategy.file.as_str().into());
        if let Some(sdk) = strategy.sdk {
            strategy_table.insert("sdk".into(), i64::from(sdk).into());
        }
        if !strategy.params.is_empty() {
            let params = toml::Value::try_from(&strategy.params)
                .map_err(|e| HarnessError::Parse(e.to_string()))?;
            strategy_table.insert("params".into(), params);
        }
        let mut doc = toml::Table::new();
        doc.insert("data".into(), data.into());
        doc.insert("engine".into(), engine.into());
        doc.insert("strategy".into(), strategy_table.into());
        toml::to_string(&doc).map_err(|e| HarnessError::Parse(e.to_string()))
    }
}

impl ProfileBacktest {
    /// True if `paramscan` is present and non-empty — the marker that this profile expands into a
    /// parameter grid ([`crate::search::paramscan::expand_paramscan`]) rather than running as a single backtest.
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

    /// **Ask `store` for the KEPT rows this run reads, once** ([`Self::kept`]), and for the operator's share splits:
    /// the door's call where it resolves the profile, before anything runs. Only the venues the run reads are asked
    /// (`crate::run::data_plan::kept_venues`), every venue's when they cannot be named here. The profile's symbols are
    /// first read as decision 0290 reads a name, each becoming its STORE name (`super::KeptRows::store_name`).
    ///
    /// # Errors
    /// [`HarnessError::Data`] for a symbol its venue's kept list names no instrument by.
    pub fn read_kept_rows(
        &mut self,
        store: &dyn vike_data::HistoryStore,
    ) -> Result<(), HarnessError> {
        let (kept, splits) = match crate::run::data_plan::kept_venues(self) {
            Some(venues) => store.kept_instruments_and_splits_of(&venues),
            None => store.kept_instruments_and_splits(),
        };
        self.hold_kept_rows(kept, splits.as_ref().map(Option::as_deref))
    }

    /// [`Self::read_kept_rows`] over the store's two answers already in hand: what a host that asks once for many
    /// runs (a study's sims) resolves each run's profile with.
    ///
    /// An `engine.multipliers` key naming a data symbol as the profile typed it follows that symbol to its store
    /// name, the exact name the engine matches a row by, and so does a `[[mounts]]` entry's slot (`SYMBOL` or
    /// `SYMBOL@account`, as the profile typed it). The profile is then validated again with the rows in hand,
    /// so a rule that reads a kept row (a stated multiplier against the catalog's unit: an ibkr stock's, decision
    /// 0290, is its row's) is answered, and the keys are judged against the store names.
    ///
    /// # Errors
    /// [`Self::read_kept_rows`]', and [`Self::validate`]'s first refusal over the store names and the held rows.
    pub fn hold_kept_rows(
        &mut self,
        lists: Result<Option<Vec<vike_model::Instrument>>, vike_data::HistoryStoreError>,
        splits: Result<Option<&[vike_model::StockSplit]>, &vike_data::HistoryStoreError>,
    ) -> Result<(), HarnessError> {
        let typed: Vec<(String, String)> =
            self.data.resolved_series().into_iter().map(|s| (s.leg_name(), s.symbol)).collect();
        super::kept_rows::read_store_names(
            &mut self.data,
            lists.as_ref().ok().and_then(Option::as_deref),
        )?;
        let resolved = self.data.resolved_series();
        let series: Vec<(String, String)> =
            resolved.iter().map(|s| (s.venue.clone(), s.symbol.clone())).collect();
        for ((_, typed), store) in typed.iter().zip(&resolved) {
            if *typed != store.symbol
                && let Some(multiplier) = self.engine.multipliers.remove(typed)
            {
                self.engine.multipliers.insert(store.symbol.clone(), multiplier);
            }
        }
        // A mount names a slot as the profile typed it, so it follows the slot to its store name too.
        for symbol in self.mounts.iter_mut().flat_map(|m| m.symbols.iter_mut()) {
            if let Some(at) = typed.iter().position(|(leg, _)| leg == symbol) {
                *symbol = resolved[at].leg_name();
            }
        }
        self.kept = super::KeptRows::answered(lists, &series).with_splits(splits);
        self.validate()
    }
}

/// Resolve [`ProfileEngine::decide`] to the engine's own [`DecideMode`].
///
/// A FREE function taking the raw string rather than a `&self` method beside
/// [`ProfileEngine::fill_model_kind`], for one mechanical reason worth stating so nobody "tidies" it
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
    let other = raw.trim().to_ascii_lowercase();
    DecideMode::from_word(&other)
        .ok_or(HarnessError::from(ProfileRefusal::EngineDecideUnknown { other }))
}

/// Resolve [`ProfileEngine::margin_mode`] to the account's [`vike_model::MarginMode`], a free function
/// for [`decide_mode`]'s reason (the construction sites spell the read `profile.engine.margin_mode`).
/// The words are the mode's own, exactly: absent or `cross` is [`vike_model::MarginMode::Cross`],
/// byte-identical to before the key existed; `cash` is a spot account. `isolated` is a mode vike
/// does not run and is refused in words, as is any other word: never read as the default.
pub(crate) fn margin_mode(raw: Option<&str>) -> Result<vike_model::MarginMode, HarnessError> {
    match raw {
        None | Some("cross") => Ok(vike_model::MarginMode::Cross),
        Some("cash") => Ok(vike_model::MarginMode::Cash),
        Some("isolated") => Err(HarnessError::from(ProfileRefusal::EngineMarginModeIsolatedNotRun)),
        Some(other) => Err(HarnessError::from(ProfileRefusal::EngineMarginModeUnknown {
            other: other.to_string(),
        })),
    }
}

/// **The symbols the run judges by the spot law** (`vike_simulator::StrategyEngineConfig::cash_symbols`: a sale past the long is
/// refused, in the live engine's words). `cash` stated: every symbol of `series`. `cross` WRITTEN OUT: none, the old
/// margin account (a short on a spot pair fills). Absent (the spot rule, owner 2026-11-11: backtest, paper and live
/// apply one spot law): every series `vike_catalog::books_cash` names (a series whose NAME says spot, the rule the live
/// mount and the paper builders read, and every series of a venue whose ONLY margin mode is `Cash`: polymarket's
/// outcome tokens, which live never shorts), none for a run of derivatives only, byte-identical to before the rule.
pub(crate) fn cash_symbols(
    raw: Option<&str>,
    series: &[crate::run::tick_replay::ProfileDataSeries],
) -> Result<Vec<String>, HarnessError> {
    Ok(match (raw, margin_mode(raw)?) {
        (_, vike_model::MarginMode::Cash) => series.iter().map(|s| s.leg_name()).collect(),
        (Some(_), _) => Vec::new(),
        (None, _) => series
            .iter()
            .filter(|s| vike_catalog::books_cash(&s.venue, &s.symbol))
            .map(|s| s.leg_name())
            .collect(),
    })
}

/// Parse a `from`/`to` field, with this module's error: `vike_data::store::hist::parse_timestamp`'s grammar.
fn parse_timestamp(s: &str) -> Result<i64, HarnessError> {
    vike_data::store::hist::parse_timestamp(s).map_err(HarnessError::Parse)
}

#[cfg(test)]
use crate::run::tick_replay::SeriesKind;
#[cfg(test)]
use vike_simulator::{FillModelKind, QueueModelKind};

#[cfg(test)]
mod tests;
