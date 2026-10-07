//! The harness's view of the metrics summary: the ONE piece of the report that genuinely
//! needs the harness's own profile parser — [`periods_per_year`] — and the realism stamp.
//!
//! The report struct itself, [`vike_analytics::report::BacktestReport`], and its annualization
//! constants live in `vike-analytics`, OUTSIDE this crate entirely: its composition needs
//! only `serde` + `vike_analytics::metrics`, so putting it here would have forced every
//! DataFusion-free consumer to re-assemble its own copy (which is exactly what `vike-report`'s
//! `LiveTearsheet` used to do). Callers name them there. ⚠ This module RE-EXPORTED all six
//! (`BacktestReport`, `DAILY_PERIODS_PER_YEAR`, `DEFAULT_PERIODS_PER_YEAR`, `ExtendedMetrics`,
//! `HonestyCounters`, `periods_per_year_for_interval`) — and `harness` re-exported
//! `BacktestReport` once more — until 2026-09-27, making `harness::report::BacktestReport`,
//! `harness::BacktestReport` and the crate root's `report::BacktestReport` three second names for
//! one struct. The root `CLAUDE.md`'s one-name rule retired all of them with the simulator split
//! (docs/decisions/0087).

use vike_analytics::RealismStamp;
use vike_analytics::report::{DEFAULT_PERIODS_PER_YEAR, periods_per_year_for_interval};

use super::profile::{BacktestProfile, DataCfg, DataKind, EngineCfg};

/// The `periods_per_year` to feed [`vike_analytics::report::BacktestReport::from_result`] for a
/// profile — SINGLE source of truth so the single-run bin and the sweep rank the SAME profile on the
/// same Sharpe scale.
///
/// The only profile-coupled part of the report, and therefore the only part that stays in this
/// crate's harness rather than in `vike-analytics` (it names [`BacktestProfile`], a harness type).
///
/// # This is a WRAPPER, not the derivation
///
/// The interval -> observations-per-year scale, the reason it is derived rather than matched
/// against `"1d"`, the preserved daily anchor and the unparseable-interval fallback all live on
/// [`periods_per_year_for_interval`] in vike-analytics — beside the constants they scale, and
/// BELOW both planes that need them. This function adds exactly one thing that genuinely needs
/// the gated profile parser: the TICK branch. A tick stream has no fixed period, so there is no
/// honest observation count to derive and it takes [`DEFAULT_PERIODS_PER_YEAR`] outright — and
/// only a caller holding a `BacktestProfile` knows it is holding ticks.
///
/// ⚠ This doc used to carry the whole derivation AND the claim that the Studio's walk-forward
/// made "the same two derivations". The MODE half was true; the annualization half was false —
/// the Studio passed a bare `252.0`. The cure was the workspace's own: one home below both.
pub fn periods_per_year(profile: &BacktestProfile) -> f64 {
    let DataKind::Bar = profile.data.kind else {
        return DEFAULT_PERIODS_PER_YEAR;
    };
    periods_per_year_for_interval(&profile.data.interval)
}

/// The REALISM STAMP for a profile: every resolved `[engine]`, `[engine.fee]`, `[engine.impact]`,
/// `[engine.resolution]`, `[engine.sizer]` and cost-relevant `[data]` value, plus the frictionless
/// verdict. See [`vike_analytics::realism`] for what the stamp is FOR; this function is the one
/// producer, because only a holder of the gated [`BacktestProfile`] knows what resolved.
///
/// # RESOLVED, not "written in the file"
///
/// Every key below is read off the already-deserialized struct, so a key the profile never
/// mentioned still contributes its serde default — which is the whole point. `fee_rate` and
/// `slippage` are the pair that matters: both default to `0.0`, so a profile that omits them
/// describes a market where trading is free, and that is indistinguishable in a report from a
/// deliberate frictionless run unless somebody writes the resolved number down.
///
/// # What it deliberately does NOT record, and why
///
/// * `[risk]`. Those are CEILINGS, not prices: a tighter `max_leverage` refuses orders rather than
///   charging for them, and a refusal already reaches the report through
///   [`vike_analytics::report::HonestyCounters::denials`]. Recording them here would make two runs
///   with different risk ceilings read as "different cost models", which is a false statement about
///   what the fills were priced at.
/// * `[strategy]` and `[paramscan]`. They change WHAT was traded, which is the question the run
///   manifest's own config section answers; the stamp answers what trading it COST.
/// * `[[data.series]]` row by row. The resolved `(venue, symbol)` set is recorded as one joined
///   value instead, because a per-row key set would make the stamp's key SET depend on the
///   universe size and two runs over different universes would diverge on every key rather than on
///   the one that says so.
///
/// # ⚠ This function enumerates fields by hand, and the mitigation is BUILT rather than available
///
/// Rust cannot iterate a struct's fields, so a knob added to [`EngineCfg`] does not appear here
/// until somebody adds a line — the rot shape this workspace pays for repeatedly, and the reason
/// `crates/vike-ops/tests/wiring/engine_cfg_reaches_the_engine.rs` exists for the ENGINE side of the same
/// problem.
///
/// ⚠ This paragraph used to name the cure as merely AVAILABLE — "[`crate::profile_surface`]-style
/// text scanning of `EngineCfg`'s declaration against the keys below" — while the only test over
/// the stamp pinned `values.len()` against a literal. A length pin cannot see that direction at
/// all: a field added with no `put` leaves the length where it was. The scan now runs, in
/// `tests::a_bare_profile_stamps_every_declared_engine_field`, and it found `engine.decide`
/// recorded by nothing on the day it was written.
///
/// The honesty half of the old argument stands, and it is why the hand enumeration was a cost
/// rather than a bug: a key this stamp does not carry reads as `None`, and
/// [`vike_analytics::RealismStamp::divergence`] reports a one-sided key as a divergence rather than
/// as agreement. ⚠ That covers an OLD document read by a NEW binary and NOT a knob absent from
/// both stamps — which enters no key list and makes two differently-priced runs read as agreeing.
/// Closing that second case is what the scan is for.
pub fn realism_stamp(profile: &BacktestProfile) -> RealismStamp {
    let e = &profile.engine;
    let mut v: Vec<(String, String)> = Vec::with_capacity(64);
    let mut put = |k: &str, val: String| v.push((k.to_string(), val));

    // --- what a fill COSTS ---------------------------------------------------------------------
    put("engine.cash", fmt_f(e.cash));
    put("engine.fee_rate", fmt_f(e.fee_rate));
    put("engine.slippage", fmt_f(e.slippage));
    put("engine.snap_to_properties", e.snap_to_properties.to_string());
    put("engine.volume_limit", fmt_of(e.volume_limit));
    put("engine.fill_model", fmt_os(e.fill_model.as_deref()));
    put("engine.seed_bar_interval_ms", fmt_oi(e.seed_bar_interval_ms));
    put("engine.emulator_release_stops", e.emulator_release_stops.to_string());

    // --- when a fill happens -------------------------------------------------------------------
    put("engine.feed_latency", e.feed_latency.to_string());
    put("engine.order_latency_ms", e.order_latency_ms.to_string());
    put("engine.fill_latency_ms", e.fill_latency_ms.to_string());
    put("engine.queue_model", fmt_os(e.queue_model.as_deref()));
    put("engine.queue_seed_depth", fmt_of(e.queue_seed_depth));
    put("engine.queue_min_hold_ms", fmt_oi(e.queue_min_hold_ms));
    put("engine.session_gate", e.session_gate.to_string());
    // ⚠ `decide` is a COST key, not a "what was traded" one — so it belongs here and not with the
    // `[strategy]` row of this function's exclusion list: `"simultaneous"` routes the step through
    // `crates/vike-sim/src/engine.rs`'s `fill_step_gated`, implies `cash_gate` and DISABLES
    // the granular sub-bar lane, so two runs agreeing on every other key here were still priced by
    // different code (`crates/vike-backtest/src/harness/profile/engine_cfg.rs`'s `EngineCfg` argues
    // both consequences at the field itself). It was recorded by nothing until
    // `a_bare_profile_stamps_every_declared_engine_field` derived the field list from that
    // declaration and found the one key no `put` covered.
    put("engine.decide", fmt_os(e.decide.as_deref()));

    // --- what the account allows ---------------------------------------------------------------
    put("engine.cash_gate", e.cash_gate.to_string());
    put("engine.maint_margin", fmt_f(e.maint_margin));
    put("engine.liq_buffer", fmt_f(e.liq_buffer));
    put("engine.venue_style_liquidation", e.venue_style_liquidation.to_string());
    put("engine.leverage", fmt_of(e.leverage));
    put("engine.clamp_to_leverage", e.clamp_to_leverage.to_string());
    put("engine.max_open_positions", e.max_open_positions.to_string());
    put("engine.max_open_long", e.max_open_long.to_string());
    put("engine.max_open_short", e.max_open_short.to_string());
    put("engine.multiplier", fmt_f(e.multiplier));
    put(
        "engine.multipliers",
        if e.multipliers.is_empty() {
            "(none)".to_string()
        } else {
            e.multipliers
                .iter()
                .map(|(k, m)| format!("{k}={}", fmt_f(*m)))
                .collect::<Vec<_>>()
                .join(",")
        },
    );
    put("engine.settlement_period_ms", fmt_oi(e.settlement_period_ms));
    put("engine.attach_funding", e.attach_funding.to_string());
    put(
        "engine.timeframes",
        if e.timeframes.is_empty() { "(none)".to_string() } else { e.timeframes.join(",") },
    );
    put(
        "engine.equity_sample_every",
        e.equity_sample_every.map_or_else(|| "-".to_string(), |n| n.to_string()),
    );

    // --- the opt-in sub-tables. An ABSENT table is recorded as `(absent)` rather than skipped:
    // "this run had no fee schedule" and "this stamp predates the key" must not be the same bytes,
    // and `RealismStamp::divergence` is what would otherwise conflate them.
    match &e.fee {
        None => put("engine.fee", "(absent)".to_string()),
        Some(f) => {
            put("engine.fee.kind", f.kind.clone());
            put("engine.fee.taker_rate", fmt_f(f.taker_rate));
            put("engine.fee.maker_rate", fmt_f(f.maker_rate));
            put("engine.fee.maker_rebate_share", fmt_f(f.maker_rebate_share));
            put("engine.fee.per_share", fmt_of(f.per_share));
            put("engine.fee.min", fmt_of(f.min));
            put("engine.fee.max_pct", fmt_of(f.max_pct));
            put("engine.fee.bps", fmt_of(f.bps));
            put("engine.fee.premium_cap_pct", fmt_of(f.premium_cap_pct));
            put("engine.fee.maker_bps", fmt_of(f.maker_bps));
            put("engine.fee.taker_bps", fmt_of(f.taker_bps));
            put("engine.fee.venue", fmt_os(f.venue.as_deref()));
            put("engine.fee.symbol", fmt_os(f.symbol.as_deref()));
            put(
                "engine.fee.pm_curve",
                f.pm_curve.map_or_else(|| "-".to_string(), |b| b.to_string()),
            );
        }
    }
    match &e.impact {
        None => put("engine.impact", "(absent)".to_string()),
        Some(i) => {
            put("engine.impact.model", i.model.clone());
            put("engine.impact.exec_time", fmt_f(i.exec_time));
            put("engine.impact.window", i.window.to_string());
            put("engine.impact.gamma", fmt_f(i.gamma));
            put("engine.impact.eta", fmt_f(i.eta));
        }
    }
    match &e.resolution {
        None => put("engine.resolution", "(absent)".to_string()),
        Some(r) => {
            put("engine.resolution.kind", r.kind.clone());
            put("engine.resolution.window_secs", r.window_secs.to_string());
            put("engine.resolution.path", fmt_os(r.path.as_deref()));
            put("engine.resolution.end_ts", fmt_os(r.end_ts.as_deref()));
            put("engine.resolution.winners", r.winners.len().to_string());
        }
    }
    match &e.sizer {
        None => put("engine.sizer", "(absent)".to_string()),
        Some(s) => put("engine.sizer.kind", s.kind.clone()),
    }

    // --- what the run SAW. Not a cost, but a run priced identically over a different tape is not
    // a comparable result either, and `data.warmup`/`data.detail_interval` change the fills.
    let d: &DataCfg = &profile.data;
    put(
        "data.kind",
        match d.kind {
            DataKind::Bar => "bar".to_string(),
            DataKind::Tick => "tick".to_string(),
        },
    );
    put("data.interval", d.interval.clone());
    put("data.from", d.from.clone());
    put("data.to", d.to.clone());
    put("data.warmup", fmt_os(d.warmup.as_deref()));
    put("data.detail_interval", fmt_os(d.detail_interval.as_deref()));
    put(
        "data.series",
        d.resolved_series()
            .iter()
            .map(|s| format!("{}:{}", s.venue, s.symbol))
            .collect::<Vec<_>>()
            .join(","),
    );

    RealismStamp::new(v, frictionless_reason(e))
}

/// `Some(reason)` when this configuration charges NOTHING for trading.
///
/// Four independent cost channels, and the run is frictionless only when all four are off: the flat
/// `fee_rate`, the `[engine.fee]` SCHEDULE (which supersedes the flat rate), the flat `slippage`,
/// and the `[engine.impact]` model. A configuration with any one of them armed has priced something
/// and is not the case this verdict exists to flag.
///
/// ⚠ The reason NAMES the four, because "frictionless" alone leaves an operator guessing which knob
/// they forgot — and the commonest cause is a profile that set `slippage` and assumed a fee came
/// with it.
fn frictionless_reason(e: &EngineCfg) -> Option<String> {
    let free_fee = e.fee_rate == 0.0 && e.fee.is_none();
    let free_slip = e.slippage == 0.0 && e.impact.is_none();
    (free_fee && free_slip).then(|| {
        "fee_rate 0, no [engine.fee] schedule, slippage 0 and no [engine.impact] model — every \
         fill was free"
            .to_string()
    })
}

/// Render an `f64` for the stamp.
///
/// ⚠ `{}` rather than a fixed precision, deliberately: a fixed `{:.4}` would print a `fee_rate` of
/// `0.00002` (two tenths of a basis point, a real maker rebate) as `0.0000` and make it
/// indistinguishable from free — the exact conflation this whole stamp exists to prevent. `{}` on
/// an `f64` is Rust's shortest round-trippable rendering, so the value can be read back and
/// compared.
fn fmt_f(v: f64) -> String {
    format!("{v}")
}

/// An absent optional renders as `-`, never as `0`: `RealismStamp` compares values as STRINGS, and
/// `"0"` would make "this knob was off" and "this knob was set to zero" the same bytes on a diff.
fn fmt_of(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_string(), fmt_f)
}

/// The `Option<i64>` twin of [`fmt_of`], on the same terms.
fn fmt_oi(v: Option<i64>) -> String {
    v.map_or_else(|| "-".to_string(), |n| n.to_string())
}

/// The `Option<&str>` twin of [`fmt_of`], on the same terms.
fn fmt_os(v: Option<&str>) -> String {
    v.map_or_else(|| "-".to_string(), str::to_string)
}

#[path = "report_tests.rs"]
#[cfg(test)]
mod report_tests;
