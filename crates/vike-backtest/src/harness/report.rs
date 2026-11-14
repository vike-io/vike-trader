//! The harness's view of the metrics summary: the ONE piece of the report that genuinely
//! needs the harness's own profile parser — [`periods_per_year`] — and the realism stamp.
//!
//! The report struct itself, [`vike_analytics::report::BacktestReport`], and its annualization
//! constants live in `vike-analytics`, OUTSIDE this crate entirely: its composition needs
//! only `serde` + `vike_analytics::metrics`, so putting it here would have forced every
//! DataFusion-free consumer to re-assemble its own copy (which is exactly what `vike-report`'s
//! `Tearsheet` used to do). Callers name them there. ⚠ This module RE-EXPORTED all six
//! (`BacktestReport`, `DAILY_PERIODS_PER_YEAR`, `DEFAULT_PERIODS_PER_YEAR`, `BacktestReportExtendedMetrics`,
//! `BacktestReportHonesty`, `periods_per_year_for_interval`) — and `harness` re-exported
//! `BacktestReport` once more — until 2026-09-27, making `harness::report::BacktestReport`,
//! `harness::BacktestReport` and the crate root's `report::BacktestReport` three second names for
//! one struct. The root `CLAUDE.md`'s one-name rule retired all of them with the simulator split
//! (docs/decisions/0087).

use vike_analytics::realism::RealismStampCosts;
use vike_analytics::report::periods_per_year_for;
use vike_analytics::{BacktestResult, RealismStamp};
use vike_model::FeeSchedule;

use super::profile::{ProfileBacktest, ProfileData, ProfileEngine};
use vike_model::run_request::replay::ReplayKind;

/// The `periods_per_year` to feed [`vike_analytics::report::BacktestReport::from_result`] for a
/// profile — SINGLE source of truth so the single-run bin and the sweep rank the SAME profile on the
/// same Sharpe scale.
///
/// The only profile-coupled part of the report, and therefore the only part that stays in this
/// crate's harness rather than in `vike-analytics` (it names [`ProfileBacktest`], a harness type).
///
/// # This is a WRAPPER, not the derivation
///
/// The interval -> observations-per-year scale, the reason it is derived rather than matched
/// against `"1d"`, the preserved daily anchor and the unparseable-interval fallback all live on
/// `vike_analytics::report::periods_per_year_for_interval` — beside the constants they scale, and
/// BELOW both planes that need them — and so does the TICK branch ([`periods_per_year_for`] with
/// `None`: a tick stream has no fixed period, so it takes the default outright), which the Studio's
/// picked-slice door asks through the same function. This wrapper adds the one thing that needs the
/// gated profile parser: reading whether the profile holds bars or ticks.
///
/// ⚠ This doc used to carry the whole derivation AND the claim that the Studio's walk-forward
/// made "the same two derivations". The MODE half was true; the annualization half was false —
/// the Studio passed a bare `252.0`. The cure was the workspace's own: one home below both.
pub fn periods_per_year(profile: &ProfileBacktest) -> f64 {
    let bars = matches!(profile.data.kind, ReplayKind::Bar);
    periods_per_year_for(bars.then_some(profile.data.interval.as_str()))
}

/// The REALISM STAMP for a profile: every resolved `[engine]`, `[engine.fee]`, `[engine.slippage]`,
/// `[engine.margin_call]`, `[engine.resolution]`, `[engine.sizer]` and cost-relevant `[data]` value, plus what the run was
/// charged in words (`vike_analytics::realism::RealismStampCosts`: fees, slippage, funding). See
/// [`vike_analytics::realism`] for what the stamp is FOR; this function is the one producer (and
/// [`realism_stamp_of_run`] the same stamp with the run's funding outcome), because only a holder of
/// the gated [`ProfileBacktest`] knows what resolved.
///
/// # RESOLVED, not "written in the file"
///
/// Every key below is read off the already-deserialized struct, so a key the profile never
/// mentioned still contributes its serde default — which is the whole point. `fee_rate` and
/// `slippage` are the pair that matters: both default to nothing, and a profile that omits the fee is
/// charged its venue's table (`super::run::fee_schedule_for_run`), which a reader cannot tell from
/// a free run unless the stamp says which table it was.
///
/// # What it deliberately does NOT record, and why
///
/// * `[risk]`. Those are CEILINGS, not prices: a tighter `max_leverage` refuses orders rather than
///   charging for them, and a refusal already reaches the report through
///   [`vike_analytics::report::BacktestReportHonesty::denials`]. Recording them here would make two runs
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
/// Rust cannot iterate a struct's fields, so a knob added to [`ProfileEngine`] does not appear here
/// until somebody adds a line — the rot shape this workspace pays for repeatedly, and the reason
/// `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs` exists for the ENGINE side of the same
/// problem.
///
/// ⚠ This paragraph used to name the cure as merely AVAILABLE — "`crate::profile_surface`-style
/// text scanning of `ProfileEngine`'s declaration against the keys below" — while the only test over
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
pub fn realism_stamp(profile: &ProfileBacktest) -> RealismStamp {
    let e = &profile.engine;
    let mut v: Vec<(String, String)> = Vec::with_capacity(64);
    let mut put = |k: &str, val: String| v.push((k.to_string(), val));

    // --- what a fill COSTS ---------------------------------------------------------------------
    put("engine.cash", fmt_f(e.cash));
    // The currency `cash` and every money number of the run is written in, RESOLVED (`USD` for a
    // profile that states none). A label only: the engine converts nothing, and a human table prints
    // it only off the dollar (`RealismStamp::off_dollar_ledger`). A profile that reached a run has
    // passed `validate`, so the `(invalid)` arm is a stamp built by hand from a bad profile.
    // A coin ledger's money is its settle coin (`super::run::run_ledger_coin`, decision 0258), which the human
    // table then names as it names any ledger off the dollar.
    let series = profile.data.resolved_series();
    let coin = super::run::run_ledger_coin(
        series.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())),
        &profile.kept,
    )
    .ok()
    .flatten();
    put(
        vike_analytics::realism::ACCOUNT_CURRENCY_KEY,
        match coin {
            Some(ledger) => ledger.coin,
            None => super::profile::resolve_ledger_currency(e.account_currency.as_deref())
                .map_or_else(|_| "(invalid)".to_string(), str::to_string),
        },
    );
    // The conversion pair of each quote currency that is not the ledger's, as written (decision 0243):
    // two runs whose quotes are booked through different pairs booked different money. `(none)` books
    // every quote as the ledger's own unit.
    put(
        "engine.conversion",
        if e.conversion.is_empty() {
            "(none)".to_string()
        } else {
            e.conversion.iter().map(|(q, s)| format!("{q}={s}")).collect::<Vec<_>>().join(",")
        },
    );
    put("engine.fee_rate", fmt_f(e.fee_rate));
    // The slippage MODEL, resolved (`none` for a profile with no table, which prices the same), then
    // the keys that model owns: the one table, one key set per model.
    put(
        "engine.slippage",
        e.slippage.as_ref().map_or_else(|| "none".to_string(), |s| s.model.clone()),
    );
    if let Some(s) = &e.slippage {
        for (key, v) in [("value", s.value), ("ticks", s.ticks), ("tick", s.tick), ("base", s.base)]
        {
            if let Some(v) = v {
                put(&format!("engine.slippage.{key}"), fmt_f(v));
            }
        }
        if s.model == "market_impact" {
            let (exec_time, window, gamma, eta) = s.impact_knobs();
            put("engine.slippage.exec_time", fmt_f(exec_time));
            put("engine.slippage.window", window.to_string());
            put("engine.slippage.gamma", fmt_f(gamma));
            put("engine.slippage.eta", fmt_f(eta));
        }
    }
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
    // `crates/vike-simulator/src/engine/bar_fills.rs`'s `fill_step_gated`, implies `cash_gate` and DISABLES
    // the granular sub-bar lane, so two runs agreeing on every other key here were still priced by
    // different code (`crates/vike-backtest/src/harness/profile/engine_config.rs`'s `ProfileEngine` argues
    // both consequences at the field itself). It was recorded by nothing until
    // `a_bare_profile_stamps_every_declared_engine_field` derived the field list from that
    // declaration and found the one key no `put` covered.
    put("engine.decide", fmt_os(e.decide.as_deref()));
    // The law a tagged plain limit filled by, RESOLVED: the profile's word, else the strategy
    // file's header's, so a run never fills by a law it does not name.
    let (optimistic, stated) = super::profile::optimistic_quotes(e.quotes, &profile.strategy);
    let by_header = if stated { "" } else { " (absent: the strategy file's header)" };
    put("engine.quotes", format!("optimistic={optimistic}{by_header}"));

    // --- what the account allows ---------------------------------------------------------------
    // The account's margin mode, RESOLVED: `cash` when the spot law judges any symbol of the run (`cash` stated,
    // or none stated and a series whose name says spot: `super::profile::cash_symbols`), `cross` otherwise. A
    // `cash` (spot) account refuses a sell past the long that a `cross` one fills short, so two runs differing
    // only here were filled by different rules. `(invalid)` as `account_currency`'s.
    let cash =
        super::profile::cash_symbols(e.margin_mode.as_deref(), &profile.data.resolved_series());
    put(
        "engine.margin_mode",
        match (super::profile::margin_mode(e.margin_mode.as_deref()), cash) {
            (Ok(vike_model::MarginMode::Cash), _) => "cash",
            (Ok(_), Ok(cash)) if !cash.is_empty() => "cash",
            (Ok(_), _) => "cross",
            (Err(_), _) => "(invalid)",
        }
        .to_string(),
    );
    put("engine.cash_gate", e.cash_gate.to_string());
    // Prices nothing (a report block over the same fills), stamped because every key is.
    put("engine.closed_trades", e.closed_trades.to_string());
    put("engine.risk_free_rate", fmt_of(e.risk_free_rate));
    put("engine.whole_entry_fee", e.whole_entry_fee.to_string());
    put("engine.maint_margin", fmt_f(e.maint_margin));
    put("engine.liquidation_buffer", fmt_f(e.liquidation_buffer));
    put("engine.venue_style_liquidation", e.venue_style_liquidation.to_string());
    match &e.margin_call {
        None => put("engine.margin_call", "(absent)".to_string()),
        Some(c) => {
            put("engine.margin_call.cover", fmt_f(c.cover));
            put("engine.margin_call.margin_long", fmt_f(c.margin_long));
            put("engine.margin_call.margin_short", fmt_f(c.margin_short));
            put("engine.margin_call.step", fmt_f(c.step));
        }
    }
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
    put(
        "engine.attach_funding",
        e.attach_funding.map_or_else(|| "(absent: perpetuals)".to_string(), |b| b.to_string()),
    );
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
            put("engine.fee.max_percent", fmt_of(f.max_percent));
            put("engine.fee.bps", fmt_of(f.bps));
            put("engine.fee.premium_cap_percent", fmt_of(f.premium_cap_percent));
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
    // The in-bar fill rules decide the order a bar's orders fill in, so two runs differing in one
    // were filled by different rules.
    match &e.in_bar {
        None => put("engine.in_bar", "(absent)".to_string()),
        Some(r) => {
            put("engine.in_bar.path_walk", r.path_walk.to_string());
            put("engine.in_bar.fill_at_close", r.fill_at_close.to_string());
            put("engine.in_bar.price_order", r.price_order.to_string());
            put("engine.in_bar.queue_order", r.queue_order.to_string());
            put("engine.in_bar.committed_stops", r.committed_stops.to_string());
            put("engine.in_bar.trail_from_fill", r.trail_from_fill.to_string());
            put("engine.in_bar.reduce_only_at_fill", r.reduce_only_at_fill.to_string());
        }
    }
    match &e.resolution {
        None => put("engine.resolution", "(absent)".to_string()),
        Some(r) => {
            put("engine.resolution.kind", r.kind.clone());
            put("engine.resolution.window_secs", r.window_secs.to_string());
            put("engine.resolution.path", fmt_os(r.path.as_deref()));
            put("engine.resolution.end_timestamp", fmt_os(r.end_timestamp.as_deref()));
            put("engine.resolution.winners", r.winners.len().to_string());
        }
    }
    match &e.sizer {
        None => put("engine.sizer", "(absent)".to_string()),
        Some(s) => put("engine.sizer.kind", s.kind.clone()),
    }

    // --- what the run SAW. Not a cost, but a run priced identically over a different tape is not
    // a comparable result either, and `data.warmup`/`data.detail_interval` change the fills.
    let d: &ProfileData = &profile.data;
    put(
        "data.kind",
        match d.kind {
            ReplayKind::Bar => "bar".to_string(),
            ReplayKind::Tick => "tick".to_string(),
        },
    );
    put("data.interval", d.interval.clone());
    put("data.from", d.from.clone());
    put("data.to", d.to.clone());
    put("data.warmup", fmt_os(d.warmup.as_deref()));
    put("data.detail_interval", fmt_os(d.detail_interval.as_deref()));
    // Where the bars came from (decision 0268): the lane, or `file:<path>`. Stamped only off the
    // venue's own candles, so every stamp already stored (all of which read them) still agrees with a
    // new run that read them too, and a lane's or a file's run diverges from both on this one key.
    if let Some(source) = super::run::source_label(profile) {
        put("data.source", source);
    }
    put(
        "data.series",
        d.resolved_series()
            .iter()
            .map(|s| format!("{}:{}", s.venue, s.symbol))
            .collect::<Vec<_>>()
            .join(","),
    );

    let (fees, fees_free) = fee_words(profile);
    let charged = RealismStampCosts {
        fees,
        slippage: slippage_words(e),
        funding: funding_words(profile, None),
    };
    // Every fill free: the fee schedules the run charges are all free AND no slippage of any model.
    let charged_nothing = (fees_free && charged.slippage == "none")
        .then(|| format!("every fill was free: fees {}, slippage none", charged.fees));
    RealismStamp::new(v, charged, charged_nothing)
}

/// [`realism_stamp`] of a run that HAPPENED: its funding term reads the run's own data warnings, so
/// a perpetual the store held no funding history for says `no funding history` rather than
/// `charged`. The single-run doors stamp with this (`backtest_cli`'s run and the backtesthub's
/// `run_backtest` verb); a search, which is no one run, stamps its base profile with
/// [`realism_stamp`].
pub fn realism_stamp_of_run(profile: &ProfileBacktest, result: &BacktestResult) -> RealismStamp {
    let mut stamp = realism_stamp(profile);
    if let Some(charged) = stamp.charged.as_mut() {
        charged.funding = funding_words(profile, Some(&result.data_warnings));
    }
    stamp
}

/// What the run's fills paid in FEES, in words, read off the very schedules the run charges with
/// (`super::run::fee_schedule_for_run`, both `StrategyEngineConfig` sites' call), and whether every one of
/// them is free.
///
/// ⚠ A profile that states NO fee is charged its venue's table (owner, 2026-11-08), so it reads
/// `venue fees: …`, never `no fees`: that word is only for a table that states `kind = "free"`.
fn fee_words(profile: &ProfileBacktest) -> (String, bool) {
    let e = &profile.engine;
    let series = profile.data.resolved_series();
    let ledger = e.off_dollar_ledger();
    let (schedule, per_series) = match super::run::fee_schedule_for_run(
        e.fee.as_ref(),
        e.fee_rate,
        ledger,
        &series,
        &profile.kept.rows,
    ) {
        Ok(fees) => fees,
        Err(err) => return (format!("refused: {}", err.message()), false),
    };
    let Some(schedule) = schedule else {
        // The flat chain: a stated `fee_rate`, or (a zero rate) a run that names no series.
        return match e.fee_rate {
            0.0 => ("none (the run names no series to read a venue fee table from)".into(), true),
            rate => (format!("flat fee {}", percent(rate)), false),
        };
    };
    let free = std::iter::once(&schedule)
        .chain(per_series.iter().map(|(_, s)| s))
        .all(|s| matches!(s, FeeSchedule::Free));
    let words = match e.fee.as_ref().map(|f| f.kind.as_str()) {
        Some("free") => "no fees".to_string(),
        Some("venue") | None => {
            format!(
                "venue fees: {}",
                venue_lane_words(e, &series, &profile.kept.rows, &schedule, &per_series)
            )
        }
        Some(kind) => format!("fee table ({kind}): {}", schedule_words(&schedule)),
    };
    (words, free)
}

/// `<lane>, <schedule>` for the fee lane a `kind = "venue"` run pays, or, when its series straddle
/// lanes (each symbol at its own row), `<lane> (<symbols>), <schedule>` per lane, `; `-joined.
fn venue_lane_words(
    e: &ProfileEngine,
    series: &[crate::tick_replay::ProfileDataSeries],
    kept: &[vike_model::Instrument],
    schedule: &FeeSchedule,
    per_series: &[(String, FeeSchedule)],
) -> String {
    use super::profile::kept_lane;
    fn lane_of<'s>(
        s: &'s crate::tick_replay::ProfileDataSeries,
        kept: &[vike_model::Instrument],
    ) -> &'s str {
        super::profile::kept_lane(&s.venue, &s.symbol, kept)
    }
    if per_series.is_empty() {
        // One row for the whole run: the lane `ProfileEngineFee::build_for` read it from (the stated venue's
        // symbol or first series on it; for a profile that states no fee, the first series').
        let lane = match e.fee.as_ref() {
            Some(fee) => {
                let venue = fee.venue.as_deref().unwrap_or("");
                match fee.symbol.as_deref() {
                    Some(symbol) => kept_lane(venue, symbol, kept),
                    None => series.iter().find(|s| s.venue == venue).map_or_else(
                        || vike_venues::fee_lane(venue, ""),
                        |s| kept_lane(venue, &s.symbol, kept),
                    ),
                }
            }
            None => series.first().map_or("", |s| lane_of(s, kept)),
        };
        return format!("{lane}, {}", schedule_words(schedule));
    }
    let mut lanes: Vec<(&str, Vec<&str>, &FeeSchedule)> = Vec::new();
    for (symbol, sched) in per_series {
        let lane = series
            .iter()
            .find(|s| &s.symbol == symbol)
            .map_or(symbol.as_str(), |s| lane_of(s, kept));
        match lanes.iter_mut().find(|(l, _, _)| *l == lane) {
            Some((_, symbols, _)) => symbols.push(symbol),
            None => lanes.push((lane, vec![symbol], sched)),
        }
    }
    lanes
        .iter()
        .map(|(lane, symbols, sched)| {
            format!("{lane} ({}), {}", symbols.join(", "), schedule_words(sched))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// One fee schedule in words: the rates as percents, the currency amounts as numbers in the run's
/// own money.
fn schedule_words(schedule: &FeeSchedule) -> String {
    match *schedule {
        FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => format!(
            "taker {}, maker {}",
            percent(taker_bps / 10_000.0),
            percent(maker_bps / 10_000.0)
        ),
        FeeSchedule::PerShareWithFloor { per_share, min, max_percent: max_pct } => {
            let cap = if max_pct > 0.0 {
                format!(", at most {} of the order's value", percent(max_pct))
            } else {
                String::new()
            };
            format!("{per_share} a share, at least {min} an order{cap}")
        }
        FeeSchedule::PercentOfUnderlying { bps, premium_cap_percent: premium_cap_pct } => format!(
            "{} of the underlying, at most {} of the premium",
            percent(bps / 10_000.0),
            percent(premium_cap_pct)
        ),
        FeeSchedule::BpsWithFloor { bps, min } => {
            format!("{} of the order's value, at least {min} an order", percent(bps / 10_000.0))
        }
        FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share } => {
            let rebate = if maker_rebate_share > 0.0 {
                format!(", a maker rebate of {} of the taker fee", percent(maker_rebate_share))
            } else {
                String::new()
            };
            format!("taker {taker_rate} x p(1-p) a share, maker {maker_rate} x p(1-p){rebate}")
        }
        FeeSchedule::Free => "free".to_string(),
    }
}

/// What a fill paid beyond its fee, in words, by the slippage model: `none` (no table, `none`, or a
/// model whose amount is zero), a share of the price, a number of ticks, or the market impact model
/// with its constant part.
fn slippage_words(e: &ProfileEngine) -> String {
    let Some(s) = &e.slippage else { return "none".to_string() };
    let positive = |v: Option<f64>| v.filter(|v| *v != 0.0);
    match s.model.as_str() {
        "percent" => positive(s.value)
            .map_or_else(|| "none".to_string(), |v| format!("{} of the price a fill", percent(v))),
        "ticks" => positive(s.ticks).map_or_else(
            || "none".to_string(),
            |n| format!("{n} ticks of {} a fill", fmt_f(s.tick.unwrap_or(0.0))),
        ),
        "market_impact" => match positive(s.base) {
            Some(b) => format!("{} of the price a fill + the market impact model", percent(b)),
            None => "the market impact model".to_string(),
        },
        _ => "none".to_string(),
    }
}

/// The run's FUNDING in words, by the run's own rule (`super::run::attaches_funding`): `not
/// applicable` when nothing it trades pays funding, `off (…)` for a perpetual a profile turned it off
/// for, else `charged` — or, read off a finished run's `data_warnings` (`Some`), `no funding history
/// (<symbols>)` for each series the store held none for (`super::run::NO_FUNDING_HISTORY`).
fn funding_words(profile: &ProfileBacktest, data_warnings: Option<&[String]>) -> String {
    let attach = profile.engine.attach_funding;
    let series = profile.data.resolved_series();
    let attached: Vec<&str> = series
        .iter()
        .filter(|s| super::run::attaches_funding(attach, &s.venue, &s.symbol))
        .map(|s| s.symbol.as_str())
        .collect();
    if attached.is_empty() {
        let perp = series.iter().any(|s| super::run::names_a_perpetual(&s.venue, &s.symbol));
        return if perp { "off (engine.attach_funding = false)" } else { "not applicable" }
            .to_string();
    }
    let warnings = data_warnings.unwrap_or_default();
    let missing: Vec<&str> = attached
        .iter()
        .copied()
        .filter(|symbol| {
            let head = format!("{symbol}: ");
            warnings
                .iter()
                .any(|l| l.starts_with(&head) && l.contains(super::run::NO_FUNDING_HISTORY))
        })
        .collect();
    match missing.len() {
        0 => "charged".to_string(),
        n if n == attached.len() => format!("no funding history ({})", missing.join(", ")),
        _ => format!("charged; no funding history ({})", missing.join(", ")),
    }
}

/// `fraction` as a percent, at most six decimals and no trailing zeros: `0.001` is `0.1%`.
fn percent(fraction: f64) -> String {
    let digits = format!("{:.6}", fraction * 100.0);
    format!("{}%", digits.trim_end_matches('0').trim_end_matches('.'))
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
