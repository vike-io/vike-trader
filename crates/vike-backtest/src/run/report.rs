//! The harness's view of the metrics summary: the ONE piece of the report that genuinely
//! needs the harness's own profile parser — [`periods_per_year`] — and the realism stamp.
//!
//! The report struct itself, [`vike_analytics::results::TradingResults`], and its annualization
//! constants live in `vike-analytics`, OUTSIDE this crate entirely: its composition needs
//! only `serde` + `vike_analytics::metrics`, so putting it here would have forced every
//! DataFusion-free consumer to re-assemble its own copy (which is exactly what `vike-report`'s
//! `TradingResultsTable` used to do). Callers name them there. ⚠ This module RE-EXPORTED all six
//! (`TradingResults`, `DAILY_PERIODS_PER_YEAR`, `DEFAULT_PERIODS_PER_YEAR`, `TradingResultsExtendedMetrics`,
//! `TradingResultsHonesty`, `periods_per_year_for_interval`) — and `harness` re-exported
//! `TradingResults` once more — until 2026-09-27, making `run::report::TradingResults`,
//! `harness::TradingResults` and the crate root's `report::TradingResults` three second names for
//! one struct. The root `CLAUDE.md`'s one-name rule retired all of them with the simulator split
//! (docs/decisions/0087).

use vike_analytics::realism::RealismStampCosts;
use vike_analytics::results::{TradingResults, periods_per_year_for};
use vike_analytics::{RealismStamp, TradingRecord};
use vike_model::FeeSchedule;

use super::profile::{ProfileBacktest, ProfileData};
use vike_model::run_request::replay::ReplayKind;

/// The `periods_per_year` to feed [`vike_analytics::results::TradingResults::from_record`] for a
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
/// `vike_analytics::results::periods_per_year_for_interval` — beside the constants they scale, and
/// BELOW both planes that need them — and so does the TICK branch ([`periods_per_year_for`] with
/// `None`: a tick stream has no fixed period, so it takes the default outright), which the Studio's
/// picked-slice door asks through the same function. This wrapper adds the one thing that needs the
/// gated profile parser: reading whether the profile holds bars or ticks. A tick run's record kept by
/// time (the default since 2026-11-20) carries its own step and `from_record` annualizes it at that
/// (`vike_analytics::TradingRecord::periods_per_year`), so the tick default reaches only a tick run
/// that kept every event (`engine.equity_sample_every`).
///
/// ⚠ This doc used to carry the whole derivation AND the claim that the Studio's walk-forward
/// made "the same two derivations". The MODE half was true; the annualization half was false —
/// the Studio passed a bare `252.0`. The cure was the workspace's own: one home below both.
pub fn periods_per_year(profile: &ProfileBacktest) -> f64 {
    let bars = matches!(profile.data.kind, ReplayKind::Bar);
    periods_per_year_for(bars.then_some(profile.data.interval.as_str()))
}

/// The REALISM STAMP for a profile: every resolved `[engine]`, `[engine.margin_call]`,
/// `[engine.resolution]`, `[engine.sizer]` and cost-relevant `[data]` value and the strategy's commission and
/// slippage, plus what the run was
/// charged in words (`vike_analytics::realism::RealismStampCosts`: fees, slippage, funding). See
/// [`vike_analytics::realism`] for what the stamp is FOR; this function is the one producer (and
/// [`realism_stamp_of_run`] the same stamp with the run's funding outcome), because only a holder of
/// the gated [`ProfileBacktest`] knows what resolved.
///
/// # RESOLVED, not "written in the file"
///
/// Every key below is read off the already-deserialized struct, so a key the profile never
/// mentioned still contributes its serde default — which is the whole point. `commission` and
/// `slippage` are the pair that matters: a profile that names neither is charged what its strategy
/// states (`super::costs`), which a reader cannot tell from a free run unless the stamp says so.
///
/// # What it deliberately does NOT record, and why
///
/// * `[risk]`. Those are CEILINGS, not prices: a tighter `max_leverage` refuses orders rather than
///   charging for them, and a refusal already reaches the report through
///   [`vike_analytics::results::TradingResultsHonesty::denials`]. Recording them here would make two runs
///   with different risk ceilings read as "different cost models", which is a false statement about
///   what the fills were priced at.
/// * `[strategy]` and `[paramscan]`. They change WHAT was traded, which is the question the run
///   manifest's own config section answers; the stamp answers what trading it COST. The one
///   exception is a `[paramscan]` axis over a cost param (`commission`, `slippage`, their units):
///   that search paid no one cost, so the cost reads `varies by point (axis: …)` and the stamp never
///   says the search was free (`super::costs::swept_cost_params`).
/// * `[[data.series]]` row by row. The resolved `(venue, symbol)` set is recorded as one joined
///   value instead, because a per-row key set would make the stamp's key SET depend on the
///   universe size and two runs over different universes would diverge on every key rather than on
///   the one that says so.
///
/// # ⚠ This function enumerates fields by hand, and the mitigation is BUILT rather than available
///
/// Rust cannot iterate a struct's fields, so a knob added to [`super::profile::ProfileEngine`] does not appear here
/// until somebody adds a line — the rot shape this workspace pays for repeatedly, and the reason
/// `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs` exists for the ENGINE side of the same
/// problem.
///
/// ⚠ This paragraph used to name the cure as merely AVAILABLE — "`crate::run::profile_surface`-style
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
    // A coin ledger's money is its settle coin (`super::run_ledger_coin`, decision 0258), which the human
    // table then names as it names any ledger off the dollar.
    let series = profile.data.resolved_series();
    let coin = super::run_ledger_coin(
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
    // The strategy's commission and slippage, RESOLVED (`super::costs`: its params, else what it
    // states, else none): what each fill paid, whoever said so.
    let costs = super::costs::run_costs(profile);
    // A `[paramscan]` axis over a cost param is no one cost: each point paid its own.
    let (swept_commission, swept_slippage) = super::costs::swept_cost_params(
        profile.paramscan.iter().flat_map(|g| g.keys()).map(String::as_str),
    );
    let (commission, slippage) = match &costs {
        Ok(c) => (
            c.commission.words(schedule_words),
            c.slippage.as_ref().map_or_else(|| "none".to_string(), slippage_key_words),
        ),
        Err(err) => (format!("refused: {}", err.message()), format!("refused: {}", err.message())),
    };
    let varied = |axes: &[&str], words: String| {
        if axes.is_empty() { words } else { super::costs::varies_by_point(axes) }
    };
    let commission = varied(&swept_commission, commission);
    put("strategy.params.commission", commission.clone());
    put("strategy.params.slippage", varied(&swept_slippage, slippage));
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
    // different code (`crates/vike-backtest/src/run/profile/engine_config.rs`'s `ProfileEngine` argues
    // both consequences at the field itself). It was recorded by nothing until
    // `a_bare_profile_stamps_every_declared_engine_field` derived the field list from that
    // declaration and found the one key no `put` covered.
    put("engine.decide", fmt_os(e.decide.as_deref()));
    // The law a tagged plain limit filled by, RESOLVED: the profile's word, else the strategy
    // file's header's, so a run never fills by a law it does not name.
    let (optimistic, stated) = super::profile::optimistic_quotes(e.quotes, profile.law_strategy());
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
    put("engine.max_mount_leverage", fmt_of(e.max_mount_leverage));
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
        e.attach_funding.map_or_else(|| "(absent: off)".to_string(), |b| b.to_string()),
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
    // "this run had no such table" and "this stamp predates the key" must not be the same bytes,
    // and `RealismStamp::divergence` is what would otherwise conflate them.
    // The in-bar fill rules decide the order a bar's orders fill in, so two runs differing in one
    // were filled by different rules. `fill_at_close` RESOLVED: the profile's override, else the strategy
    // file's header (`fill = "close"`), so a run never fills at a close it does not name.
    let (rules, by_header) = super::profile::in_bar_rules(e.in_bar, profile.law_strategy());
    match (&e.in_bar, by_header) {
        (None, false) => put("engine.in_bar", "(absent)".to_string()),
        (_, by_header) => {
            let r = rules;
            let header = if by_header { " (the strategy file's header)" } else { "" };
            put("engine.in_bar.path_walk", r.path_walk.to_string());
            put("engine.in_bar.fill_at_close", format!("{}{header}", r.fill_at_close));
            put("engine.in_bar.price_order", r.price_order.to_string());
            put("engine.in_bar.ties_in_placement_order", r.ties_in_placement_order.to_string());
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
    put("data.kind", d.kind.word().to_string());
    put("data.interval", d.interval.clone());
    put("data.from", d.from.clone());
    put("data.to", d.to.clone());
    put("data.warmup", fmt_os(d.warmup.as_deref()));
    put("data.detail_interval", fmt_os(d.detail_interval.as_deref()));
    // Where the bars came from (decision 0268): the lane, or `file:<path>`. Stamped only off the
    // venue's own candles, so every stamp already stored (all of which read them) still agrees with a
    // new run that read them too, and a lane's or a file's run diverges from both on this one key.
    if let Some(source) = super::source_label(profile) {
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

    // The same two in words, each said whose (a `strategy.params` value, or the strategy's own).
    let said = |words: String, set: bool, axes: &[&str]| match words.as_str() {
        _ if !axes.is_empty() => super::costs::varies_by_point(axes),
        "none" => words,
        _ => format!("{words} ({})", super::costs::whose(set)),
    };
    let (fees, slippage) = match &costs {
        Ok(c) => (
            said(commission, c.commission_set, &swept_commission),
            said(
                c.slippage.as_ref().map_or_else(|| "none".to_string(), slippage_words),
                c.slippage_set,
                &swept_slippage,
            ),
        ),
        Err(_) => (commission.clone(), commission),
    };
    let charged = RealismStampCosts { fees, slippage, funding: funding_words(profile, None) };
    // Every fill free: no commission AND no slippage of any model, and no grid varying either.
    let charged_nothing = (charged.fees == "none" && charged.slippage == "none")
        .then(|| "every fill was free: fees none, slippage none".to_string());
    RealismStamp::new(v, charged, charged_nothing)
}

/// [`realism_stamp`] of a run that HAPPENED: its funding term reads the run's own data warnings, so
/// a perpetual the store held no funding history for says `no funding history` rather than
/// `charged`. The single-run doors stamp with this (`backtest_cli`'s run and the backtesthub's
/// `run_backtest` verb); a search, which is no one run, stamps its base profile with
/// [`realism_stamp`].
pub fn realism_stamp_of_run(profile: &ProfileBacktest, result: &TradingRecord) -> RealismStamp {
    let mut stamp = realism_stamp(profile);
    if let Some(charged) = stamp.charged.as_mut() {
        charged.funding = funding_words(profile, Some(&result.data_warnings));
    }
    stamp
}

/// **A single run's report, named and stamped the one way every door stamps it**: `report` (the run's
/// `TradingResults::from_record` at [`periods_per_year`], its name whatever it was) takes `profile`'s name,
/// the stamp of the run ([`realism_stamp_of_run`]) and the strategy code it ran: path, sha256 and the SDK its
/// artifact was built against (`ProfileBacktest::strategy_identity`, a portfolio's over its mounts). The report
/// is a PARAMETER rather than built here because a summarized run computes it once and shares it, unstamped,
/// with its summary, so the saved report's numbers are the summary's own.
pub fn stamp_run_report(
    profile: &ProfileBacktest,
    result: &TradingRecord,
    mut report: TradingResults,
) -> TradingResults {
    report.name = profile.name.clone();
    let (file, sha256, sdk) = profile.strategy_identity();
    report.with_realism(realism_stamp_of_run(profile, result)).with_strategy(&file, &sha256, sdk)
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

/// What a fill paid beyond its fee, in words, by the slippage model of the table `s`: `none` (`none`,
/// or a model whose amount is zero), a share of the price, a number of ticks, or the market impact model
/// with its constant part.
fn slippage_words(s: &vike_model::ProfileEngineSlippage) -> String {
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

/// [`slippage_words`] for the stamp's key, with the market impact model's knobs as resolved: two runs at
/// different coefficients charged different slippage.
fn slippage_key_words(s: &vike_model::ProfileEngineSlippage) -> String {
    let words = slippage_words(s);
    if s.model != "market_impact" {
        return words;
    }
    let (exec_time, window, gamma, eta) = vike_fills::market_impact::impact_knobs(s);
    format!(
        "{words} (exec_time {}, window {window}, gamma {}, eta {})",
        fmt_f(exec_time),
        fmt_f(gamma),
        fmt_f(eta)
    )
}

/// The run's FUNDING in words, by the run's own rule (`super::attaches_funding`): `not
/// applicable` when nothing it trades pays funding and the profile did not ask, `off (…)` for a
/// perpetual the profile did not turn it on for (absent is off), else `charged` — or, read off a
/// finished run's `data_warnings` (`Some`), `no funding history (<symbols>)` for each series the store
/// held none for (`super::NO_FUNDING_HISTORY`).
fn funding_words(profile: &ProfileBacktest, data_warnings: Option<&[String]>) -> String {
    let attach = profile.engine.attach_funding;
    let series = profile.data.resolved_series();
    if !super::attaches_funding(attach) {
        let perp = series.iter().any(|s| super::names_a_perpetual(&s.venue, &s.symbol));
        return match (perp, attach) {
            (false, _) => "not applicable",
            (true, Some(false)) => "off (engine.attach_funding = false)",
            (true, _) => "off (engine.attach_funding absent)",
        }
        .to_string();
    }
    let attached: Vec<&str> = series.iter().map(|s| s.symbol.as_str()).collect();
    let warnings = data_warnings.unwrap_or_default();
    let missing: Vec<&str> = attached
        .iter()
        .copied()
        .filter(|symbol| {
            let head = format!("{symbol}: ");
            warnings.iter().any(|l| l.starts_with(&head) && l.contains(super::NO_FUNDING_HISTORY))
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
