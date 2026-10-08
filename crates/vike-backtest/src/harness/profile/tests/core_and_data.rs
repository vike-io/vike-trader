//! The section alias, queue/fill models, timeframes, the data slice and series, impact, funding.

use super::*;

/// ⚠ **THE ALIAS WARRANTY, and it is the whole of what makes the R2 rename safe.** Owner ruling
/// R2 renamed the section `[sweep]` → `[paramscan]`; [`BacktestProfile`] is
/// `deny_unknown_fields`, so without `#[serde(alias = "sweep")]` every profile already written
/// — on operators' disks, in this repository's own measurement records, in the `sweep.toml`
/// `vike-cli init` has scaffolded since it shipped — would fail to LOAD with "unknown field".
///
/// Both spellings must produce the IDENTICAL parsed grid, which is what this asserts. The alias
/// is PERMANENT: this is not a deprecation with an end date, and deleting it is a breaking
/// change to every profile ever written, not a tidy-up.
#[test]
fn both_the_new_section_and_the_legacy_one_load_and_parse_identically() {
    let grid = "\nfast = [5, 10]\nslow = [20, 30]\n";
    let new = BacktestProfile::from_toml_str(&format!("{BAR_TOML}\n[paramscan]{grid}"))
        .expect("[paramscan] is the name");
    let old = BacktestProfile::from_toml_str(&format!("{BAR_TOML}\n[sweep]{grid}"))
        .expect("[sweep] must keep loading FOREVER — every profile on disk spells it this way");
    assert_eq!(new.paramscan, old.paramscan, "one grid, two spellings");
    assert!(new.is_paramscan() && old.is_paramscan(), "both declare a parameter search");
    assert_eq!(
        new.paramscan.as_ref().map(toml::Table::len),
        Some(2),
        "…and the grid is the one that was written, not an empty table"
    );
}

/// ⚠ …and writing BOTH in one file is REFUSED, which is the right answer rather than a gap:
/// two grids in one profile has no meaning, and a silent winner would be the different-answer
/// defect the whole stage exists to end. serde produces this for a field with an alias.
#[test]
fn writing_both_spellings_in_one_profile_is_refused() {
    let err = BacktestProfile::from_toml_str(&format!(
        "{BAR_TOML}\n[paramscan]\nfast = [5]\n\n[sweep]\nslow = [20]\n"
    ))
    .expect_err("one profile, one grid");
    let msg = err.to_string();
    assert!(
        msg.contains("paramscan") || msg.contains("sweep") || msg.contains("duplicate"),
        "the refusal names the collision: {msg}"
    );
}

const BARE_MS_TOML: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "1700000000000"
to = "1700003600000"

[engine]
cash = 1000.0

[strategy]
name = "buy_hold"
"#;

#[test]
fn queue_model_parses_resolves_and_is_tick_only() {
    // Each accepted string resolves to the right kind on a tick profile.
    let kind = |qm: &str| {
        let toml =
            TICK_TOML.replace("snap_to_properties = true", &format!("queue_model = \"{qm}\""));
        BacktestProfile::from_toml_str(&toml).unwrap().engine.queue_model_kind().unwrap()
    };
    assert!(matches!(kind("risk_adverse"), Some(QueueModelKind::RiskAdverse)));
    assert!(matches!(kind("prob_log"), Some(QueueModelKind::ProbLog)));
    assert!(matches!(kind("prob_power"), Some(QueueModelKind::ProbPower(n)) if n == 1.0));
    assert!(matches!(kind("prob_power:2.5"), Some(QueueModelKind::ProbPower(n)) if n == 2.5));
    // Absent ⇒ None (the frozen simple-crossing fill).
    assert!(
        BacktestProfile::from_toml_str(TICK_TOML)
            .unwrap()
            .engine
            .queue_model_kind()
            .unwrap()
            .is_none()
    );
    // An unrecognized string fails validation at LOAD (fail-fast, not a silent fallback).
    let bad = TICK_TOML.replace("snap_to_properties = true", "queue_model = \"fifo\"");
    assert!(BacktestProfile::from_toml_str(&bad).is_err(), "unknown queue_model rejected");
    // Tick-mode only: a bar profile setting it is rejected with a clear message.
    let barq = BAR_TOML.replace("fee_rate = 0.001", "queue_model = \"risk_adverse\"");
    let err = BacktestProfile::from_toml_str(&barq).unwrap_err();
    assert!(format!("{err}").contains("tick-mode only"), "bar-mode queue_model rejected: {err}");
}

/// The tick-lane `fill_model` knob parses STRICTLY, like `queue_model`: absent ⇒ the default
/// `Tick`, the one accepted spelling resolves to `L2Book`, and a typo fails at LOAD with the
/// valid set named — never the old silent optimistic-`Tick` fallback that undid the #819
/// depth-cap realism knob.
#[test]
fn fill_model_parses_strictly_and_defaults_to_tick() {
    // Absent ⇒ the default L1 spread-crossing Tick model.
    let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
    assert_eq!(p.engine.fill_model_kind().unwrap(), FillModelKind::Tick);

    // The accepted spelling resolves to the depth-capped model; case/whitespace normalize
    // (the same trim + lowercase idiom `queue_model_kind` uses).
    let kind = |fm: &str| {
        let toml =
            TICK_TOML.replace("snap_to_properties = true", &format!("fill_model = \"{fm}\""));
        BacktestProfile::from_toml_str(&toml).unwrap().engine.fill_model_kind().unwrap()
    };
    assert_eq!(kind("l2book"), FillModelKind::L2Book);
    assert_eq!(kind("L2Book"), FillModelKind::L2Book);
    assert_eq!(kind(" l2book "), FillModelKind::L2Book);

    // A typo fails validation at LOAD, naming the valid set (fail-fast, not a silent
    // fallback). "l2_book" is exactly the audit-finding footgun.
    for junk in ["l2_book", "tick", "book"] {
        let bad =
            TICK_TOML.replace("snap_to_properties = true", &format!("fill_model = \"{junk}\""));
        let err = BacktestProfile::from_toml_str(&bad).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown fill_model") && msg.contains("l2book"),
            "junk fill_model {junk:?} rejected with the valid set named: {msg}"
        );
    }
}

#[test]
fn timeframes_reach_the_profile() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"4h\", \"1d\"]");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.timeframes, vec!["4h".to_string(), "1d".to_string()]);
}

#[test]
fn timeframes_default_to_empty() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(p.engine.timeframes.is_empty());
}

/// An unparseable timeframe is refused at LOAD. Left to the engine it is
/// `parse_timeframe(tf).expect("valid timeframe")` — a process abort, where every neighbouring
/// `[engine]` key gives a named error.
#[test]
fn an_unparseable_timeframe_is_rejected_at_load() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"4hh\"]");
    refused_at_load(&toml, HarnessError::Validation, &["timeframes", "4hh"]);
}

/// `interval_ms` accepts a zero count, so `"0h"` parses to 0 ms and would make every window
/// boundary the same instant. Refused here because nothing downstream checks it.
#[test]
fn a_zero_length_timeframe_is_rejected_at_load() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"0h\"]");
    refused_at_load(&toml, HarnessError::Validation, &["timeframes"]);
}

/// A coarser-than-base timeframe is the whole point; a FINER one cannot be synthesised from the
/// base stream and would silently return the base bars re-labelled.
#[test]
fn a_timeframe_finer_than_the_base_interval_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"1m\"]");
    refused_at_load(&toml, HarnessError::Validation, &["timeframes", "coarser"]);
}

/// Tick mode synthesises no bars, so a declared timeframe there configures nothing.
#[test]
fn timeframes_are_rejected_on_the_tick_lane() {
    let toml = TICK_TOML
        .replace("snap_to_properties = true", "snap_to_properties = true\ntimeframes = [\"4h\"]");
    refused_at_load(&toml, HarnessError::Validation, &["timeframes"]);
}

#[test]
fn parses_bar_profile() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
    assert_eq!(p.name.as_deref(), Some("bar-demo"));
    assert_eq!(p.data.venue.as_deref(), Some("binance"));
    assert_eq!(p.data.symbols, vec!["BTCUSDT", "ETHUSDT"]);
    assert_eq!(p.data.kind, DataKind::Bar);
    assert_eq!(p.data.interval, "1h");
    assert_eq!(p.engine.cash, 100000.0);
    assert_eq!(p.engine.fee_rate, 0.001);
    // defaults
    assert_eq!(p.engine.slippage, 0.0);
    assert!(!p.engine.snap_to_properties);
    assert_eq!(p.engine.seed_bar_interval_ms, None);
    // The harness default for the stop-verb release knob is `false`, matching the raw
    // `EngineParams` default — see `default_emulator_release_stops`'s doc for why this is not
    // the mirror-live `true` its doc used to claim.
    assert!(!p.engine.emulator_release_stops);
    assert_eq!(p.strategy.name, "sma_cross");

    // 2026-01-01T00:00 UTC and 2026-01-02T00:00 UTC (verified against `date -u -d`).
    let r = p.range().unwrap();
    assert_eq!(r.start, Some(1_767_225_600_000));
    assert_eq!(r.end, Some(1_767_312_000_000));
}

/// The harness stop-verb release knob defaults to `false` (matching
/// `EngineParams::default()`) and an explicit `emulator_release_stops = true` parses through
/// to arm the opt-in mirror-live release. ⚠ This test previously asserted the OPPOSITE default
/// (`true`) — that default never reached the engine through either `EngineParams`
/// construction site in `harness::run`, so every harness run was `false` regardless; see
/// `default_emulator_release_stops`'s doc.
#[test]
fn emulator_release_stops_defaults_false_and_parses_true() {
    // absent -> the harness default, false (matches EngineParams::default())
    let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
    assert!(!p.engine.emulator_release_stops);

    // explicit true threads through
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nemulator_release_stops = true");
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    assert!(p.engine.emulator_release_stops);

    // explicit false also parses
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nemulator_release_stops = false");
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    assert!(!p.engine.emulator_release_stops);
}

#[test]
fn parses_tick_profile() {
    let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
    assert_eq!(p.data.kind, DataKind::Tick);
    assert_eq!(p.data.symbols, vec!["0xTOK"]);
    assert!(p.engine.snap_to_properties);
    // interval defaults even though tick data doesn't consume it.
    assert_eq!(p.data.interval, "1d");

    let r = p.range().unwrap();
    // 2026-04-13T19:00 / 20:00 UTC (verified against `date -u -d`).
    assert_eq!(r.start, Some(1_776_106_800_000));
    assert_eq!(r.end, Some(1_776_110_400_000));
}

#[test]
fn range_accepts_bare_epoch_ms() {
    let p = BacktestProfile::from_toml_str(BARE_MS_TOML).unwrap();
    let r = p.range().unwrap();
    assert_eq!(r.start, Some(1_700_000_000_000));
    assert_eq!(r.end, Some(1_700_003_600_000));
}

#[test]
fn rejects_empty_symbols() {
    let toml = r#"
[data]
venue = "binance"
symbols = []
kind = "bar"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Validation, &[]);
}

#[test]
fn rejects_from_after_to() {
    let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "2026-01-02T00"
to = "2026-01-01T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Validation, &["data.from", "data.to"]);
}

#[test]
fn rejects_non_positive_cash() {
    let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 0.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Validation, &["engine.cash"]);
}

#[test]
fn rejects_unknown_data_kind() {
    let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "quote"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Parse, &[]);
}

#[test]
fn rejects_unknown_top_level_key() {
    let toml = r#"
bogus = "field"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "2026-01-01T00"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Parse, &[]);
}

#[test]
fn rejects_unparsable_timestamp() {
    let toml = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "not-a-timestamp"
to = "2026-01-02T00"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Parse, &[]);
}

// --- the opt-in market-impact knob (`[engine.impact]`) ---

/// Absent `[engine.impact]` must stay absent — the whole byte-identical-default claim starts
/// here, at the config layer.
#[test]
fn impact_is_absent_unless_configured() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
    assert!(p.engine.impact.is_none());
}

#[test]
fn impact_parses_with_published_defaults() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"",
    );
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let imp = p.engine.impact.expect("impact configured");
    assert_eq!(imp.model, "almgren_chriss");
    assert_eq!(imp.exec_time, 1.0);
    assert_eq!(imp.window, vike_sim::DEFAULT_IMPACT_WINDOW);
    assert!(imp.build().is_ok());
}

#[test]
fn impact_honors_explicit_exec_time_and_window() {
    let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            "fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"\nexec_time = 0.5\nwindow = 60",
        );
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let imp = p.engine.impact.unwrap();
    assert_eq!(imp.exec_time, 0.5);
    assert_eq!(imp.window, 60);
}

/// A typo'd model name must FAIL the profile, not silently price fills at zero impact.
#[test]
fn an_unknown_impact_model_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren-chris\"",
    );
    refused_at_load(&toml, HarnessError::Validation, &[]);
}

#[test]
fn nonsensical_impact_numbers_are_rejected() {
    for bad in ["exec_time = 0.0", "exec_time = -1.0", "window = 2"] {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            &format!("fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"\n{bad}"),
        );
        refused_at_load(&toml, HarnessError::Validation, &[]);
    }
}

/// The tick lane ACCEPTS `[engine.impact]` — and the acceptance is a load-bearing claim, not
/// a relaxation for its own sake. It was rejected on the premise that "the tick lane replays
/// a real book", which is false for an L1 tape (any size fills at the quote) and incomplete
/// for an L2 one (a recorded book never moves in response to the order). A profile that names
/// the knob in tick mode must therefore VALIDATE and must carry the model through, exactly as
/// the bar profile does.
#[test]
fn impact_on_the_tick_lane_is_accepted_and_carries_its_model() {
    let toml = TICK_TOML.replace(
        "snap_to_properties = true",
        "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren_chriss\"\nwindow = 500",
    );
    assert!(toml.contains("[engine.impact]"), "the TICK_TOML anchor must still match");
    let p = BacktestProfile::from_toml_str(&toml).expect("tick + impact must validate");
    assert_eq!(p.data.kind, DataKind::Tick, "the fixture must still be a TICK profile");
    let imp = p.engine.impact.expect("impact configured");
    assert_eq!(imp.window, 500, "the tick lane's window must reach the profile");
    assert!(imp.build().is_ok(), "the model must still be buildable in tick mode");
}

/// The coefficient knobs default to the PUBLISHED constants and are settable — the lever the
/// module docs point an operator at when a per-print context spends a daily-fitted
/// coefficient. Asserted end to end (parse -> `build` -> a MOVED number) rather than by
/// reading the field back, because a field that parses and is dropped on the floor between
/// here and `AlmgrenChriss` would pass a field-equality test.
#[test]
fn the_impact_coefficients_default_to_the_published_pair_and_are_settable() {
    // `ImpactModel` is deliberately NOT imported: `build()` hands back an `Arc<dyn ImpactModel>`,
    // and a trait OBJECT resolves its own trait's methods without the trait being in scope.
    use vike_sim::{AC_ETA, AC_GAMMA, ImpactInputs, ImpactTerms};
    let ctx = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.02 };
    let cfg = |extra: &str| {
        let toml = TICK_TOML.replace(
            "snap_to_properties = true",
            &format!(
                "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren_chriss\"{extra}"
            ),
        );
        BacktestProfile::from_toml_str(&toml).expect("must validate").engine.impact.unwrap()
    };

    let published = cfg("");
    assert_eq!(
        published.gamma.to_bits(),
        AC_GAMMA.to_bits(),
        "the gamma default must be the paper's"
    );
    assert_eq!(published.eta.to_bits(), AC_ETA.to_bits(), "the eta default must be the paper's");

    // The L2 lane charges PermanentOnly, so this is the selection an L2 operator's knob has
    // to reach — halving gamma must halve the charge, not leave it where it was.
    let base = published.build().unwrap().impact_frac_for(&ctx, ImpactTerms::PermanentOnly);
    assert!(base > 0.0, "the fixture must produce a real charge");
    let halved =
        cfg("\ngamma = 0.157").build().unwrap().impact_frac_for(&ctx, ImpactTerms::PermanentOnly);
    assert!(
        (halved - base / 2.0).abs() < 1e-15,
        "gamma did not reach the model: {halved} vs {}",
        base / 2.0
    );
}

/// A coefficient that would break the trait's contract is refused at PARSE time, by name.
/// Zero is refused with the rest: it disarms a half of the model silently, and on the L2 lane
/// `gamma = 0` disarms the ONLY term that lane charges — an operator would see an armed
/// profile priced exactly like an unarmed one.
#[test]
fn a_nonsensical_impact_coefficient_is_refused_by_name() {
    for (key, value) in [("gamma", "0.0"), ("gamma", "-0.5"), ("eta", "0.0"), ("eta", "-1e9")] {
        let toml = TICK_TOML.replace(
                "snap_to_properties = true",
                &format!(
                    "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren_chriss\"\n{key} = {value}"
                ),
            );
        refused_at_load(&toml, HarnessError::Validation, &[&format!("engine.impact.{key}")]);
    }
}

/// ...and a BAD model is still refused in tick mode. Removing the mode rejection must not
/// have removed the `build()` call that runs beside it — a typo'd model name silently pricing
/// fills at zero impact is the failure that check exists for, and it would now be reachable
/// on a whole extra lane.
#[test]
fn a_typod_impact_model_is_still_rejected_on_the_tick_lane() {
    let toml = TICK_TOML.replace(
        "snap_to_properties = true",
        "snap_to_properties = true\n\n[engine.impact]\nmodel = \"almgren-chriss\"",
    );
    refused_at_load(&toml, HarnessError::Validation, &["unknown engine.impact.model"]);
}

/// A bar series carries no `local_ts`, so feed-latency delivery has nothing to order by — the
/// combination is rejected rather than silently ignored (the mirror of the bar-only `attach_funding`
/// rule above).
#[test]
fn feed_latency_on_the_bar_lane_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nfeed_latency = true");
    assert!(toml.contains("feed_latency = true"), "the BAR_TOML anchor must still match");
    refused_at_load(&toml, HarnessError::Validation, &["tick-mode only"]);
}

/// Absent = OFF (today's venue-ordered replay); accepted on the tick lane.
#[test]
fn feed_latency_defaults_off_and_is_accepted_on_the_tick_lane() {
    let off = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
    assert!(!off.engine.feed_latency, "absent must mean the frozen venue-ordered replay");

    let toml = TICK_TOML
        .replace("snap_to_properties = true", "snap_to_properties = true\nfeed_latency = true");
    let on = BacktestProfile::from_toml_str(&toml).unwrap();
    assert!(on.engine.feed_latency);
}

// --- the opt-in market-funding join (`engine.attach_funding`) -----------------------------

/// Absent `[engine].attach_funding` must default to OFF — the byte-identical-default claim.
#[test]
fn attach_funding_defaults_off_and_parses_true() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
    assert!(!p.engine.attach_funding, "absent must mean OFF (funding series never read)");

    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nattach_funding = true");
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    assert!(p.engine.attach_funding);
}

/// The tick lane has no per-interval funding fold, so the join has no consumer there — the
/// combination is rejected rather than silently ignored (the mirror of the tick-only `feed_latency` rule).
#[test]
fn attach_funding_on_the_tick_lane_is_rejected() {
    let toml = TICK_TOML
        .replace("snap_to_properties = true", "snap_to_properties = true\nattach_funding = true");
    assert!(toml.contains("attach_funding = true"), "the TICK_TOML anchor must still match");
    refused_at_load(&toml, HarnessError::Validation, &["bar-mode only"]);
}

/// The OLD single-venue form still loads, unchanged — the whole back-compat claim.
#[test]
fn the_single_venue_form_still_loads_and_expands_to_series() {
    for toml in [BAR_TOML, TICK_TOML, BARE_MS_TOML] {
        let p = BacktestProfile::from_toml_str(toml).unwrap();
        assert!(p.data.series.is_empty(), "no [[data.series]] table was written");
        let series = p.data.resolved_series();
        assert_eq!(series.len(), p.data.symbols.len());
        for (s, sym) in series.iter().zip(&p.data.symbols) {
            assert_eq!(&s.venue, p.data.venue.as_ref().unwrap());
            assert_eq!(&s.symbol, sym);
            // The expansion is the WHOLE-LANE kind — quotes + trades + books, exactly what
            // `replay_ticks` loaded before the lane filter existed.
            assert_eq!(s.kind, SeriesKind::Tick);
        }
        assert_eq!(&p.data.default_venue(), p.data.venue.as_ref().unwrap());
        assert!(!p.data.is_cross_venue());
    }
}

#[test]
fn cross_venue_series_parse_in_order_with_lane_filters() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    assert!(p.data.venue.is_none());
    assert!(p.data.symbols.is_empty());
    let series = p.data.resolved_series();
    assert_eq!(series.len(), 3);
    // ORDER is meaningful: the reference series is first, so an equal-ts spot sample reaches
    // the strategy before the print it should inform (`run_ticks` ties break by stream order).
    assert_eq!((series[0].venue.as_str(), series[0].kind), ("spot", SeriesKind::Quote));
    assert_eq!((series[1].venue.as_str(), series[1].kind), ("polymarket", SeriesKind::Trade));
    assert_eq!(series[2].symbol, "btc-updown-5m-1775001600#1");
    assert!(p.data.is_cross_venue());
    assert_eq!(p.data.default_venue(), "spot");
}

#[test]
fn mixing_the_two_slice_forms_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("[data]\nkind", "[data]\nvenue = \"spot\"\nkind");
    refused_at_load(&toml, HarnessError::Validation, &["exactly one"]);
}

#[test]
fn a_duplicate_symbol_across_series_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("btc-updown-5m-1775001600#1", "BTCUSDT");
    refused_at_load(&toml, HarnessError::Validation, &["duplicate symbol"]);
}

/// A bar series has no quote/trade/book lanes, so a lane filter there is a typo, not a knob.
#[test]
fn a_lane_filter_on_the_bar_lane_is_rejected() {
    let toml = r#"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"
[[data.series]]
venue = "binance"
symbol = "BTCUSDT"
kind = "trade"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#;
    refused_at_load(toml, HarnessError::Validation, &["tick-mode only"]);
}

/// One `default_venue` + many venues = the wrong grid for every series but the first.
#[test]
fn snapping_a_cross_venue_slice_is_rejected() {
    let toml =
        CROSS_VENUE_TOML.replace("cash = 1000.0", "cash = 1000.0\nsnap_to_properties = true");
    refused_at_load(&toml, HarnessError::Validation, &["single-venue only"]);
}
