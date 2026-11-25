//! The Studio compute-to-data BOUNDARY's CONVERSIONS: the wire DTOs into this crate's own run types
//! and back, each beside its other half — the client's encode (`to_wire_spec`, `to_wire_slice`,
//! `from_wire_*`, `run_error_from_wire`) and the server's decode (`to_strategy_spec`,
//! `to_data_slice`, `to_wire_*_result`; a failure's line is `RunError::wire_line`), so a field added
//! to one is in front of whoever adds it to the other.
//!
//! ⚠ **The RUNNERS left this module on 2026-11-10.** `run_slice_local`, `run_paramscan_local`,
//! `run_walkforward_local`, `run_study_local` and the cost stamp (`to_wire_cost_model`) run the
//! harness, so they are `vike_backtesthub::studio_run`'s now, beside the `backtesthub_server` that
//! calls them and the parity test that drives them: this crate is what the DESKTOP links, and the
//! desktop links no engine (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
//! amendment of that day). They still call every conversion here, so "local" and "over the wire" stay
//! one computation (the S2 parity guarantee).
//!
//! History: this module MOVED here from `vike-datahub` (ruling 7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`), when the three Studio verbs
//! left the data daemon for `vike-backend backtesthub --addr`. It is the #719 pattern, unchanged: the
//! wire schema (the `Wire*` DTOs) lives in the LIGHT client crate, the run types stay serde-free, and
//! the two meet HERE. Not feature-gated, and DataFusion-free by construction, as
//! `scripts/ci_feature_suite.sh`'s `studio-standalone` lane requires.

use vike_analytics::walkforward::WalkforwardResults;
use vike_data::TimestampRange;
use vike_model::run_request::replay::ReplayKind;

use crate::{DataSlice, ParamscanEntry, RunError, StrategySpec, StudioParamscan};

use vike_backtesthub_client::wire_studio::{
    DataSliceWire, ParamscanEntryWire, ReplayKindWire, StrategySpecWire, StudioParamscanWire,
    WalkforwardResultsWire,
};

/// [`StrategySpecWire`] → `StrategySpec`. A strategy file's params arrive as TOML TEXT and are parsed back
/// into the params table at THIS boundary (`StrategySpec::file_from_toml_str`) — a parse failure
/// becomes a [`RunError::Strategy`] (the "no such/invalid strategy" class), never a panic.
pub fn to_strategy_spec(spec: &StrategySpecWire) -> Result<StrategySpec, RunError> {
    match spec {
        StrategySpecWire::Rhai(src) => Ok(StrategySpec::rhai(src.clone())),
        // The buffer's params cross as TEXT, read back as a file's are.
        StrategySpecWire::RhaiWithParameters { source, parameters_toml } => {
            StrategySpec::rhai_from_toml_str(source.clone(), parameters_toml)
                .map_err(|e| RunError::Strategy(format!("params TOML parse failed: {e}")))
        }
        // ⚠ **Retired, and refused for EVERY name.** It named a compiled registry strategy, and no
        // strategy is compiled into the server any more, so there is nothing a name could resolve
        // to. Refusing every name also closes the door `docs/decisions/0064-a-named-run-carries-no-source.md`
        // recorded here (`Native { name: "rhai", params_toml: "src = '…'" }` reached the compiler):
        // no `Native` frame reaches any arm at all.
        StrategySpecWire::Native { name, .. } => Err(RunError::Strategy(format!(
            "a strategy is named by its FILE now, not by a registry name: send `StrategySpecWire::File` \
             with the file's path under the server's strategies/ folder (`{name}` is likely \
             `<folder>/{name}.rs`)"
        ))),
        // A file of the SERVER's folder, read there: the params cross as text, as they always did,
        // and nothing in them is compiled (a `.rhai` file's TEXT is, read from the server's disk).
        StrategySpecWire::File { file, params_toml, sdk } => {
            StrategySpec::file_from_toml_str(file.clone(), params_toml)
                .map(|spec| spec.with_sdk(*sdk))
                .map_err(|e| RunError::Strategy(format!("params TOML parse failed: {e}")))
        }
        // ⚠ **No name/`src` guard here, and that absence is deliberate rather than an oversight
        // copied past.** A `StrategySpecWire::Plugin`'s `name` is never looked up by name at all: it
        // identifies which loaded strategy file's `create` to call, a resolution that goes through
        // `vike_rust_strategy::loader::load` (Task 4), and no compiled strategy is left to touch (plan
        // 2b). So there is no compiling arm this name could alias into, and a `src`
        // key in `params_toml` is inert here for the same reason it is inert on the Named-run
        // door — nothing on this path reads it.
        //
        // The scope question this raises — does carrying a Strategy file spec change what credential may
        // send `RunSlice`/`RunSweep`/`RunWalkforward`? — is answered at
        // `crates/vike-backtesthub/src/backtesthub_server.rs`'s module doc, not here: that file's `Scope`
        // is per-VERB (via `vike_backtesthub_client::proto::required_scope`), not per-`StrategySpecWire`-variant,
        // so this arm's own scope is not a decision this function makes.
        StrategySpecWire::Plugin { name, sha, params_toml } => {
            StrategySpec::strategy_file_from_toml_str(name.clone(), sha.clone(), params_toml)
                .map_err(|e| RunError::Strategy(format!("params TOML parse failed: {e}")))
        }
    }
}

/// `StrategySpec` → [`StrategySpecWire`], the CLIENT's half of [`to_strategy_spec`] and kept beside it. A
/// file's `Params` are serialized to TOML TEXT (`params_toml`) — the exact text
/// `StrategySpec::file_from_toml_str` / `strategy_file_from_toml_str` parse back above, so a strategy
/// resolves to bit-identical params on either side of the wire.
pub fn to_wire_spec(spec: &StrategySpec) -> Result<StrategySpecWire, RunError> {
    let toml_of = |params: &vike_model::params::Params| {
        toml::to_string(params)
            .map_err(|e| RunError::Strategy(format!("serialize params to TOML failed: {e}")))
    };
    match spec {
        StrategySpec::Rhai(src) => Ok(StrategySpecWire::Rhai(src.clone())),
        // An empty table is the bare buffer's run, sent as an older client sends it: any daemon serves it, and its
        // saved-answer key is the bare run's.
        StrategySpec::RhaiWithParameters { source, parameters } if parameters.is_empty() => {
            Ok(StrategySpecWire::Rhai(source.clone()))
        }
        StrategySpec::RhaiWithParameters { source, parameters } => {
            Ok(StrategySpecWire::RhaiWithParameters {
                source: source.clone(),
                parameters_toml: toml_of(parameters)?,
            })
        }
        StrategySpec::File { file, params, sdk } => Ok(StrategySpecWire::File {
            file: file.clone(),
            params_toml: toml_of(params)?,
            sdk: *sdk,
        }),
        StrategySpec::StrategyFile { name, sha, params } => Ok(StrategySpecWire::Plugin {
            name: name.clone(),
            sha: sha.clone(),
            params_toml: toml_of(params)?,
        }),
    }
}

/// `DataSlice` → [`DataSliceWire`], decomposing the `TimestampRange` into `start`/`end` — the CLIENT's half of
/// [`to_data_slice`], kept beside it.
pub fn to_wire_slice(slice: &DataSlice) -> DataSliceWire {
    DataSliceWire {
        venue: slice.venue.clone(),
        symbols: slice.symbols.clone(),
        interval: slice.interval.clone(),
        start: slice.range.start,
        end: slice.range.end,
        kind: match slice.kind {
            ReplayKind::Bar => ReplayKindWire::Bars,
            ReplayKind::Tick => ReplayKindWire::Ticks,
        },
    }
}

/// [`DataSliceWire`] → `DataSlice`, recomposing the decomposed `start`/`end` into a `vike_data::TimestampRange`.
pub fn to_data_slice(slice: &DataSliceWire) -> DataSlice {
    DataSlice {
        venue: slice.venue.clone(),
        symbols: slice.symbols.clone(),
        interval: slice.interval.clone(),
        range: TimestampRange { start: slice.start, end: slice.end },
        kind: match slice.kind {
            ReplayKindWire::Bars => ReplayKind::Bar,
            ReplayKindWire::Ticks => ReplayKind::Tick,
        },
    }
}

/// The CLIENT's half of [`RunError::wire_line`]: a `Response::Error` line (`"{kind}: {message}"`)
/// back to the [`RunError`] it was, so a remote failure renders as the same failure run locally. A
/// line with no known kind prefix (a transport or protocol fault) is a [`RunError::Data`] carrying
/// the whole line.
pub fn run_error_from_wire(line: String) -> RunError {
    match line.split_once(": ") {
        Some(("compile", m)) => RunError::Compile(m.to_string()),
        Some(("data", m)) => RunError::Data(m.to_string()),
        Some(("strategy", m)) => RunError::Strategy(m.to_string()),
        _ => RunError::Data(line),
    }
}

/// Map a non-finite `f64` to `None`, a finite one to `Some` — the guard [`to_wire_paramscan_result`]
/// applies to `StudioParamscan`'s `dsr`/`pbo`. Both are plain `f64` that are `NaN` in NORMAL operation (a
/// single-point / `< 2`-trial grid leaves PBO unassessable), and `serde_json` encodes a `NaN` as JSON
/// `null` that then FAILS to decode back into a plain `f64` — so carrying them as `Option<f64>` (this
/// maps the non-finite case to `None`) is what keeps an ordinary sweep from becoming a client-side
/// decode error (see [`StudioParamscanWire`]'s docs).
fn finite_or_none(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// `ParamscanEntry` → [`ParamscanEntryWire`]: the point's overrides plus the rendered
/// `vike_backtesthub_client::wire_record::to_wire_record` of its `TradingRecord`.
fn to_wire_paramscan_entry(e: &ParamscanEntry) -> ParamscanEntryWire {
    ParamscanEntryWire {
        overrides: e.overrides.clone(),
        result: vike_backtesthub_client::wire_record::to_wire_record(&e.result),
    }
}

/// `StudioParamscan` → [`StudioParamscanWire`]: mirror each ranked entry and carry `dsr`/`pbo` through
/// [`finite_or_none`] so a non-assessable (`NaN`) score crosses the wire as `None`, not a
/// decode-breaking `null`.
pub fn to_wire_paramscan_result(s: &StudioParamscan) -> StudioParamscanWire {
    StudioParamscanWire {
        entries: s.entries.iter().map(to_wire_paramscan_entry).collect(),
        dsr: finite_or_none(s.dsr),
        pbo: finite_or_none(s.pbo),
        best_index: s.best_index,
        // Stamped by `run_paramscan_local`, which is the only caller that knows what the run was priced
        // under (this function sees a ranked result, not the params that produced it).
        cost_model: None,
    }
}

/// [`StudioParamscanWire`] → `StudioParamscan`, the CLIENT's half of [`to_wire_paramscan_result`]:
/// each ranked entry through `vike_backtesthub_client::wire_record::from_wire_record`, and a `None` `dsr`/`pbo`
/// back to `NaN` (the "not assessable" sentinel the Studio renders), the inverse of
/// [`finite_or_none`].
pub fn from_wire_paramscan_result(wire: StudioParamscanWire) -> StudioParamscan {
    let StudioParamscanWire { entries, dsr, pbo, best_index, cost_model: _ } = wire;
    StudioParamscan {
        entries: entries
            .into_iter()
            .map(|e| ParamscanEntry {
                overrides: e.overrides,
                result: vike_backtesthub_client::wire_record::from_wire_record(e.result),
            })
            .collect(),
        dsr: dsr.unwrap_or(f64::NAN),
        pbo: pbo.unwrap_or(f64::NAN),
        best_index,
    }
}

/// [`WalkforwardResultsWire`] → `WalkforwardResults`, the CLIENT's half of
/// [`to_wire_walkforward_result`]: every field crossed, a step's chosen values and the walk's
/// currency included. The results' source and period do not cross (they are not serialized), so
/// they read back as `None`.
pub fn from_wire_walkforward_result(wire: WalkforwardResultsWire) -> WalkforwardResults {
    let WalkforwardResultsWire {
        steps,
        overall,
        equity_curve,
        equity_timestamp,
        profitable_step_share,
        account_currency,
        cost_model: _,
    } = wire;
    WalkforwardResults {
        steps,
        overall,
        equity_curve,
        equity_timestamp,
        profitable_step_share,
        account_currency,
    }
}

/// `WalkforwardResults` → [`WalkforwardResultsWire`]: a FULL mirror — every step (including what
/// each one CHOSE, when the walk searched), the walk's overall results, the stitched equity curve
/// and the profitable step share. Each step crosses as itself, the parameters its own search CHOSE
/// included, each the `ParamValue` it was: on the PROFILE door they were parsed OUT of a `[sweep]`
/// array (`search::paramscan::expand_paramscan`), on THIS door each is a wire axis value, with the
/// kind the axis carried, picked by the server's searching driver; a fixed walk records none.
pub fn to_wire_walkforward_result(r: &WalkforwardResults) -> WalkforwardResultsWire {
    WalkforwardResultsWire {
        steps: r.steps.clone(),
        overall: r.overall.clone(),
        equity_curve: r.equity_curve.clone(),
        equity_timestamp: r.equity_timestamp.clone(),
        profitable_step_share: r.profitable_step_share,
        account_currency: r.account_currency.clone(),
        // Stamped by `run_walkforward_local` — the results carry no record of what priced them.
        cost_model: None,
    }
}

#[path = "wire_run_tests.rs"]
#[cfg(test)]
mod wire_run_tests;

#[path = "native_arm_is_not_a_source_door.rs"]
#[cfg(test)]
mod native_arm_is_not_a_source_door;
