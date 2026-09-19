//! The harness's view of the metrics summary (Task 4): [`BacktestReport`] and its annualization
//! constants, plus the ONE piece that genuinely needs the (feature-gated) profile parser —
//! [`periods_per_year`].
//!
//! The report struct itself lives in `crate::report`, OUTSIDE the `hist-replay` feature: its
//! composition needs only `serde` + `crate::metrics`, so gating it here would have forced every
//! DataFusion-free consumer to re-assemble its own copy (which is exactly what `vike-report`'s
//! `LiveTearsheet` used to do). Everything is re-exported below, so `harness::report::*` and
//! `harness::BacktestReport` paths are unchanged for the `backtest` bin and `harness::sweep`.

pub use crate::report::{
    BacktestReport, DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR, ExtendedMetrics,
    HonestyCounters, periods_per_year_for_interval,
};

use vike_analytics::realism::RealismStamp;

use super::profile::{BacktestProfile, DataCfg, DataKind, EngineCfg};

/// The `periods_per_year` to feed [`BacktestReport::from_result`] for a profile — SINGLE source of
/// truth so the single-run bin and the sweep rank the SAME profile on the same Sharpe scale.
///
/// The only profile-coupled part of the report, and therefore the only part that stays behind
/// `hist-replay` (it names the gated [`BacktestProfile`]).
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
/// `crates/vike-ops/tests/engine_cfg_reaches_the_engine.rs` exists for the ENGINE side of the same
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
    // `crates/vike-backtest/src/engine.rs`'s `fill_step_gated`, implies `cash_gate` and DISABLES
    // the granular sub-bar lane, so two runs agreeing on every other key here were still priced by
    // different code (`crates/vike-backtest/src/harness/profile.rs`'s `EngineCfg` argues both
    // consequences at the field itself). It was recorded by nothing until
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal BAR profile at `interval`. Only the two fields `periods_per_year` reads
    /// are meaningful; the rest is the smallest thing that parses.
    fn bar_profile(interval: &str) -> BacktestProfile {
        let toml = format!(
            r#"
name = "ppy"
[data]
kind = "bar"
interval = "{interval}"
from = "0"
to = "1"
venue = "binance"
symbols = ["BTCUSDT"]
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#
        );
        toml::from_str(&toml).expect("fixture profile must parse")
    }

    /// The anchor does not move: a daily run reports exactly what it always did.
    #[test]
    fn a_daily_bar_profile_still_returns_the_daily_constant_exactly() {
        assert_eq!(
            periods_per_year(&bar_profile("1d")).to_bits(),
            DAILY_PERIODS_PER_YEAR.to_bits(),
            "the 1d anchor must be bit-identical to its pre-fix value"
        );
    }

    /// The bug this function exists to prevent: an intraday interval annualized as though daily.
    #[test]
    fn intraday_intervals_scale_off_the_daily_anchor_instead_of_collapsing_onto_it() {
        // 1440 one-minute bars per day, 24 one-hour bars per day.
        assert_eq!(periods_per_year(&bar_profile("1m")), 252.0 * 1440.0);
        assert_eq!(periods_per_year(&bar_profile("1h")), 252.0 * 24.0);
        assert_eq!(periods_per_year(&bar_profile("5m")), 252.0 * 288.0);

        // The regression itself: 1m must NOT equal the daily constant.
        assert_ne!(
            periods_per_year(&bar_profile("1m")),
            DAILY_PERIODS_PER_YEAR,
            "a 1m run annualized at 252 understates Sharpe by sqrt(1440)"
        );
    }

    /// Sharpe scales by sqrt(periods_per_year), so this pins the SIZE of the correction the fix
    /// applies — the number quoted in the doc comment above.
    #[test]
    fn the_one_minute_correction_is_sqrt_1440() {
        let ratio = periods_per_year(&bar_profile("1m")) / DAILY_PERIODS_PER_YEAR;
        assert_eq!(ratio, 1440.0);
        assert!(
            (ratio.sqrt() - 37.947).abs() < 0.001,
            "sharpe was understated by ~37.9x, got {}",
            ratio.sqrt()
        );
    }

    /// Longer-than-daily intervals scale DOWN off the same anchor — the arithmetic is not
    /// intraday-only.
    #[test]
    fn a_weekly_interval_scales_down_off_the_same_anchor() {
        assert_eq!(periods_per_year(&bar_profile("7d")), 252.0 / 7.0);
    }

    /// An unparseable interval must fall back, never fabricate a scale from a bad parse.
    #[test]
    fn an_unparseable_interval_falls_back_rather_than_fabricating_a_scale() {
        for bad in ["", "m", "1x", "xm", "-1m"] {
            assert_eq!(
                periods_per_year(&bar_profile(bad)),
                DEFAULT_PERIODS_PER_YEAR,
                "interval {bad:?} must fall back"
            );
        }
    }

    // --- the realism stamp ---------------------------------------------------------------------

    /// Parse a profile from a body appended under `[engine]`.
    fn profile_with_engine(extra: &str) -> BacktestProfile {
        let toml = format!(
            r#"
name = "realism"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "1"
venue = "binance"
symbols = ["BTCUSDT"]
[engine]
cash = 1000.0
{extra}
[strategy]
name = "buy_hold"
"#
        );
        toml::from_str(&toml).expect("fixture profile must parse")
    }

    /// ⚠ **THE DEFECT, as a test.** A profile that mentions neither `fee_rate` nor `slippage` is
    /// describing a market where trading is free, and the resolved numbers say so — which is what a
    /// report had no way to tell anybody.
    #[test]
    fn a_profile_that_mentions_no_cost_at_all_stamps_as_frictionless() {
        let s = realism_stamp(&profile_with_engine(""));
        let why = s.frictionless.as_deref().expect("a costless profile must be flagged");
        // The reason names all four channels, because "frictionless" alone leaves the operator
        // guessing which knob they forgot.
        for needle in ["fee_rate", "[engine.fee]", "slippage", "[engine.impact]"] {
            assert!(why.contains(needle), "the reason must name {needle}: {why}");
        }
        // ...and the values are RECORDED even though the file never wrote them. This is the half
        // "record the resolved configuration" buys over "record the file".
        assert_eq!(s.get("engine.fee_rate"), Some("0"));
        assert_eq!(s.get("engine.slippage"), Some("0"));
        assert_eq!(s.get("engine.fee"), Some("(absent)"));
        assert_eq!(s.get("engine.impact"), Some("(absent)"));
    }

    /// Any ONE armed cost channel ends the verdict — four independent channels, and the run has
    /// priced something as soon as one of them is on.
    #[test]
    fn any_one_armed_cost_channel_ends_the_frictionless_verdict() {
        assert!(realism_stamp(&profile_with_engine("fee_rate = 0.0004")).frictionless.is_none());
        assert!(realism_stamp(&profile_with_engine("slippage = 0.0001")).frictionless.is_none());
        assert!(
            realism_stamp(&profile_with_engine("[engine.fee]\nkind = \"flat\""))
                .frictionless
                .is_none(),
            "a fee SCHEDULE supersedes the flat rate and is a cost channel of its own"
        );
        assert!(
            realism_stamp(&profile_with_engine("[engine.impact]\nmodel = \"linear\""))
                .frictionless
                .is_none()
        );
    }

    /// ⚠ **A sub-basis-point rate must not render as free.** A fixed `{:.4}` would print a 0.2bp
    /// maker rebate as `0.0000`, which is exactly the conflation the stamp exists to prevent.
    #[test]
    fn a_sub_basis_point_rate_survives_the_rendering() {
        let s = realism_stamp(&profile_with_engine("fee_rate = 0.00002"));
        assert_eq!(s.get("engine.fee_rate"), Some("0.00002"));
        assert!(s.frictionless.is_none());
    }

    /// The comparability question the stamp exists for: two runs whose only difference is the fee
    /// diverge on exactly that key, by the TOML path an operator would edit.
    #[test]
    fn two_runs_differing_only_in_fee_diverge_on_exactly_that_key() {
        let free = realism_stamp(&profile_with_engine(""));
        let costed = realism_stamp(&profile_with_engine("fee_rate = 0.0004"));

        let d = free.divergence(&costed);
        assert_eq!(d.len(), 1, "expected one divergence, got {d:?}");
        assert_eq!(d[0].key, "engine.fee_rate");
    }

    /// An absent optional records as `-`, never as `0`. `RealismStamp` compares values as strings,
    /// so a zero would make "this knob was off" and "this knob was set to zero" one answer.
    #[test]
    fn an_absent_optional_records_as_a_dash_rather_than_a_zero() {
        let s = realism_stamp(&profile_with_engine(""));
        assert_eq!(s.get("engine.volume_limit"), Some("-"));
        assert_eq!(s.get("engine.leverage"), Some("-"));
        assert_eq!(s.get("data.warmup"), Some("-"));
        assert_eq!(s.get("data.detail_interval"), Some("-"));

        let set = realism_stamp(&profile_with_engine("volume_limit = 0.0"));
        assert_eq!(
            set.get("engine.volume_limit"),
            Some("0"),
            "a volume_limit of ZERO is a real (and drastic) setting and must not read as unset"
        );
    }

    /// Batch A's two `[data]` keys and the resolved universe are on the stamp: a run priced
    /// identically over a different tape, or with a different warm-up, is not a comparable result.
    #[test]
    fn the_data_keys_that_change_the_fills_are_on_the_stamp() {
        let s = realism_stamp(&profile_with_engine(""));
        assert_eq!(s.get("data.kind"), Some("bar"));
        assert_eq!(s.get("data.interval"), Some("1d"));
        assert_eq!(s.get("data.series"), Some("binance:BTCUSDT"));
    }

    /// The whole point of `[engine.fee]`'s ten-key surface: an ARMED schedule records every one of
    /// its resolved keys, so two runs on the same `kind` but different rates are still told apart.
    #[test]
    fn an_armed_fee_schedule_records_its_whole_resolved_surface() {
        let s = realism_stamp(&profile_with_engine(
            "[engine.fee]\nkind = \"flat\"\ntaker_rate = 0.0004\nmaker_rate = 0.0002",
        ));
        assert_eq!(s.get("engine.fee.kind"), Some("flat"));
        assert_eq!(s.get("engine.fee.taker_rate"), Some("0.0004"));
        assert_eq!(s.get("engine.fee.maker_rate"), Some("0.0002"));
        // The keys nobody wrote are recorded at their resolved defaults, which is the property
        // that makes a diff between two schedules complete.
        assert_eq!(s.get("engine.fee.maker_rebate_share"), Some("0"));
        assert_eq!(s.get("engine.fee.per_share"), Some("-"));
        assert_eq!(s.get("engine.fee.venue"), Some("-"));
        // ...and the table-absent sentinel is gone, so a diff against a no-schedule run reports
        // the schedule rather than agreeing with it.
        assert_eq!(s.get("engine.fee"), None);
    }

    /// Every `pub` field `EngineCfg` declares, read out of the source that declares it.
    ///
    /// ⚠ A TEXT SCAN, and deliberately the same one
    /// `crates/vike-ops/tests/engine_cfg_reaches_the_engine.rs`'s `declared_fields` performs for
    /// the ENGINE side of this problem: Rust cannot iterate a struct's fields, and a derive macro
    /// to make it possible is a dependency this crate will not take for one test. `include_str!`
    /// embeds the source at COMPILE time, so this cannot read a stale copy, and
    /// `crate::profile_surface` already reads the same file the same way for the schema export.
    ///
    /// Two residuals, both inherited from that helper. Only `pub ` fields are seen — a private
    /// `EngineCfg` field would still deserialize from TOML and would be invisible here rather than
    /// merely unchecked. And the block is delimited by the first column-0 `}` after the
    /// declaration rather than by a parse, which is sound only because nothing inside that
    /// declaration starts a line with a brace.
    fn declared_engine_fields() -> Vec<String> {
        const PROFILE_SRC: &str = include_str!("profile.rs");
        let start = PROFILE_SRC.find("pub struct EngineCfg {").expect("EngineCfg declaration");
        let body = &PROFILE_SRC[start..];
        let end = body.find("\n}").expect("end of EngineCfg");
        // `skip(1)` drops the declaration line itself, which would otherwise survive the `pub `
        // filter as a bogus field named `struct EngineCfg {`.
        body[..end]
            .lines()
            .skip(1)
            .filter_map(|l| l.trim().strip_prefix("pub "))
            .filter_map(|l| l.split(':').next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// ⚠ **Every field `EngineCfg` declares owes exactly one `engine.<field>` key** — the assertion
    /// the first version of this test claimed to make and could not.
    ///
    /// That version pinned `s.values.len()` against a literal `41`, under the doc "a knob added to
    /// `EngineCfg` and not to `realism_stamp` reddens here with the instruction rather than
    /// silently going unrecorded". A length pin cannot see that direction: a field added with no
    /// `put` leaves the length at 41 and the assert holds. It reddened only in the OPPOSITE case,
    /// when somebody had already added the line — a change-detector sold as a completeness gate,
    /// which is the class this tree corrects rather than tolerates. Deriving one side is the shape
    /// that works here as elsewhere, so the field list comes from [`declared_engine_fields`] and
    /// only the KEYS are local.
    ///
    /// Why a BARE profile makes the check total, and why there is deliberately NO exemption table:
    /// an absent `[engine.*]` sub-table stamps as `(absent)` rather than being skipped (the
    /// argument is at `realism_stamp`'s `match &e.fee` arm), so a struct-typed field owes a key
    /// here exactly as a scalar one does. A field this test cannot find is therefore a cost knob no
    /// operator can compare — the defect, not an exception to it.
    ///
    /// ⚠ It found one the moment it was written. `engine.decide` was declared, resolved, implied
    /// `cash_gate` and disabled the granular sub-bar lane, and reached no stamp.
    #[test]
    fn a_bare_profile_stamps_every_declared_engine_field() {
        let s = realism_stamp(&profile_with_engine(""));
        let fields = declared_engine_fields();
        // A FLOOR, not a pin: a scan that silently returned nothing would make the loop below
        // vacuous and this test green. `EngineCfg` declared 35 `pub` fields when this was written
        // and fields are only ever added, so a handful means the scan broke rather than the struct
        // shrinking.
        assert!(fields.len() > 30, "the scan found only {} fields — it is broken", fields.len());
        for field in fields {
            let key = format!("engine.{field}");
            assert!(
                s.get(&key).is_some(),
                "`EngineCfg::{field}` is declared and `realism_stamp` records no {key:?}. Add a \
                 `put({key:?}, …)` line beside the knobs it belongs with; if it genuinely prices \
                 nothing, argue that at `realism_stamp`'s \"What it deliberately does NOT record\" \
                 section first. A knob missing from the stamp is a cost model nobody can \
                 compare.\nkeys: {:?}",
                s.values.keys().collect::<Vec<_>>()
            );
        }
    }

    /// ⚠ The key COUNT, kept as a change-detector and no longer sold as anything more. Two things
    /// it still earns, neither of which the derived test above can see: the `data.*` half of the
    /// stamp is a deliberately chosen SUBSET of `DataCfg` (the cost-relevant keys — the argument is
    /// at `realism_stamp`'s `let d: &DataCfg` block), so nothing derives it; and a `put` whose key
    /// DUPLICATES another is silently collapsed by `RealismStamp::new`'s `BTreeMap`, which shows up
    /// here as a count that fell.
    #[test]
    fn a_bare_profile_stamps_a_pinned_key_count() {
        let s = realism_stamp(&profile_with_engine(""));
        assert_eq!(
            s.values.len(),
            42,
            "the stamp's key set changed. If you ADDED an `[engine]`/`[data]` line, bump this \
             number; if you removed one, drop it. If the count FELL without a line being removed, \
             two `put` calls now share a key and the map collapsed them.\nkeys: {:?}",
            s.values.keys().collect::<Vec<_>>()
        );
    }
}
