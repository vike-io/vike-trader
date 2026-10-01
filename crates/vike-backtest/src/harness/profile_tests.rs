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

const BAR_TOML: &str = r#"
name = "bar-demo"

[data]
venue = "binance"
symbols = ["BTCUSDT", "ETHUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-01T00"
to = "2026-01-02T00"

[engine]
cash = 100000.0
fee_rate = 0.001

[strategy]
name = "sma_cross"
[strategy.params]
fast = 10
slow = 20
"#;

const TICK_TOML: &str = r#"
name = "demo"

[data]
venue = "polymarket"
symbols = ["0xTOK"]
kind = "tick"
from = "2026-04-13T19"
to = "2026-04-13T20"

[engine]
cash = 10000.0
snap_to_properties = true

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
"#;

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
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("timeframes"), "{m}");
            assert!(m.contains("4hh"), "names the offending value: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// `interval_ms` accepts a zero count, so `"0h"` parses to 0 ms and would make every window
/// boundary the same instant. Refused here because nothing downstream checks it.
#[test]
fn a_zero_length_timeframe_is_rejected_at_load() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"0h\"]");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("timeframes"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// A coarser-than-base timeframe is the whole point; a FINER one cannot be synthesised from the
/// base stream and would silently return the base bars re-labelled.
#[test]
fn a_timeframe_finer_than_the_base_interval_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ntimeframes = [\"1m\"]");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("timeframes"), "{m}");
            assert!(m.contains("coarser"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// Tick mode synthesises no bars, so a declared timeframe there configures nothing.
#[test]
fn timeframes_are_rejected_on_the_tick_lane() {
    let toml = TICK_TOML
        .replace("snap_to_properties = true", "snap_to_properties = true\ntimeframes = [\"4h\"]");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("timeframes"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
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
    let err = BacktestProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
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
    let err = BacktestProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
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
    let err = BacktestProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
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
    let err = BacktestProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
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
    let err = BacktestProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
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
    let err = BacktestProfile::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
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
    let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
    assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
}

#[test]
fn nonsensical_impact_numbers_are_rejected() {
    for bad in ["exec_time = 0.0", "exec_time = -1.0", "window = 2"] {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            &format!("fee_rate = 0.001\n\n[engine.impact]\nmodel = \"almgren_chriss\"\n{bad}"),
        );
        let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
        assert!(matches!(err, HarnessError::Validation(_)), "{bad}: got {err:?}");
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
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => assert!(
                m.contains(&format!("engine.impact.{key}")),
                "{key}={value} was refused without naming the key: {m}"
            ),
            other => panic!("{key}={value} expected a validation error, got {other:?}"),
        }
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
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("unknown engine.impact.model"), "{m}")
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// A bar series carries no `local_ts`, so feed-latency delivery has nothing to order by — the
/// combination is rejected rather than silently ignored (the mirror of the bar-only `attach_funding`
/// rule above).
#[test]
fn feed_latency_on_the_bar_lane_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nfeed_latency = true");
    assert!(toml.contains("feed_latency = true"), "the BAR_TOML anchor must still match");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("tick-mode only"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
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
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("bar-mode only"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// --- G4: the cross-venue `[[data.series]]` slice ------------------------------------------

const CROSS_VENUE_TOML: &str = r#"
name = "cheap-np-window"

[data]
kind = "tick"
from = "1774999800000"
to = "1775002500000"

[[data.series]]
venue = "spot"
symbol = "BTCUSDT"
kind = "quote"

[[data.series]]
venue = "polymarket"
symbol = "btc-updown-5m-1775001600#0"
kind = "trade"

[[data.series]]
venue = "polymarket"
symbol = "btc-updown-5m-1775001600#1"
kind = "trade"

[engine]
cash = 1000.0

[engine.fee]
kind = "probability_scaled"
taker_rate = 0.072

[engine.resolution]
kind = "binary_outcome"
[engine.resolution.winners]
"btc-updown-5m-1775001600" = 0

[strategy]
name = "cheap_catch_updown_fair_value"
[strategy.params]
spot_symbol = "BTCUSDT"
"#;

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
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("exactly one"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_duplicate_symbol_across_series_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("btc-updown-5m-1775001600#1", "BTCUSDT");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("duplicate symbol"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
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
    match BacktestProfile::from_toml_str(toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("tick-mode only"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// One `default_venue` + many venues = the wrong grid for every series but the first.
#[test]
fn snapping_a_cross_venue_slice_is_rejected() {
    let toml =
        CROSS_VENUE_TOML.replace("cash = 1000.0", "cash = 1000.0\nsnap_to_properties = true");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("single-venue only"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// --- G7: the fee schedule -----------------------------------------------------------------

#[test]
fn fee_is_absent_unless_configured() {
    assert!(BacktestProfile::from_toml_str(BAR_TOML).unwrap().engine.fee.is_none());
}

#[test]
fn probability_scaled_fee_builds_the_curve() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    let schedule = p.engine.fee.as_ref().unwrap().build().unwrap();
    assert_eq!(
        schedule,
        FeeSchedule::ProbabilityScaled {
            taker_rate: 0.072,
            maker_rate: 0.0,
            maker_rebate_share: 0.0,
        }
    );
    // The number the live bot pays: 0.072·p·(1−p) per share, at the fill's own price.
    assert_eq!(schedule.commission(false, 1.0, 0.25), 0.072 * 0.25 * 0.75);
}

#[test]
fn an_unknown_fee_kind_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("\"probability_scaled\"", "\"prob_scaled\"");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("unknown engine.fee.kind"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_fee_schedule_plus_a_flat_fee_rate_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("cash = 1000.0", "cash = 1000.0\nfee_rate = 0.001");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("two different cost models"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// --- realism/fee-shape-family + realism/venue-fee-schedule-lookup -------------------------
//
// The G7 table above reached ONE of `vike_model::FeeSchedule`'s five shapes. These cover the
// rest, and `kind = "venue"` — the venue's own published schedule, resolved through the pair
// the paper mount resolves it through.

/// An `[engine.fee]` body, parsed on its own.
fn fee_cfg(body: &str) -> FeeCfg {
    toml::from_str(body).expect("the [engine.fee] body parses")
}

/// A resolved data slice, as `(venue, symbol)` rows.
fn fee_series(rows: &[(&str, &str)]) -> Vec<SeriesRef> {
    rows.iter().map(|(v, s)| SeriesRef::new(*v, *s)).collect()
}

/// The headline of the per-share family: the FLOOR reaches the fill. `maker_taker_rates()`
/// answers `(0.0, 0.0)` for this shape, so the pre-existing flatten-everything-but-the-curve
/// routing would have charged a configured IBKR schedule exactly NOTHING — which is why the
/// second assertion is here rather than left implicit.
#[test]
fn the_per_share_floor_shape_builds_and_its_floor_is_what_a_flat_rate_cannot_express() {
    let schedule =
        fee_cfg("kind = \"per_share_with_floor\"\nper_share = 0.005\nmin = 1.0\nmax_pct = 0.005\n")
            .build()
            .expect("the IBKR-shaped schedule builds");
    assert_eq!(
        schedule,
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
    );
    // 10 shares × $0.005 is $0.05 of per-share fee, and the order pays the $1.00 minimum —
    // the cost a small-size high-frequency configuration is eaten by live.
    assert_eq!(schedule.commission(false, 10.0, 100.0), 1.0);
    // ...and the reason the engine must not flatten it.
    assert_eq!(schedule.maker_taker_rates(), (0.0, 0.0));
}

/// The floor is REQUIRED, because a per-share fee with no minimum is a flat rate wearing a
/// different name — the configuration the shape exists to escape.
#[test]
fn a_per_share_shape_with_no_floor_is_refused() {
    let err = fee_cfg("kind = \"per_share_with_floor\"\nper_share = 0.005\n")
        .build()
        .expect_err("a floorless floor shape is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("needs BOTH per_share and min"), "{m}")
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// `min = 0.0` is accepted — the zero is then an ASSERTION that there is no floor rather than
/// an omission, which is the whole difference the refusal above buys.
#[test]
fn an_explicit_zero_floor_is_accepted() {
    let schedule = fee_cfg("kind = \"per_share_with_floor\"\nper_share = 0.005\nmin = 0.0\n")
        .build()
        .expect("an explicit zero floor is a legal assertion");
    assert_eq!(
        schedule,
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 0.0, max_pct: 0.0 }
    );
}

/// Deribit's shape, and the trap that makes its cap REQUIRED: `commission` is
/// `min(bps × premium, cap × premium)`, so a zero cap zeroes the whole commission.
#[test]
fn the_percent_of_underlying_shape_builds_and_a_zero_cap_is_refused() {
    let schedule =
        fee_cfg("kind = \"percent_of_underlying\"\nbps = 3.0\npremium_cap_pct = 0.125\n")
            .build()
            .expect("the Deribit-shaped schedule builds");
    assert_eq!(schedule, FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 });
    // The cap binds for a cheap deep-OTM option, which is what a flat rate could not say.
    assert_eq!(schedule.commission_with_underlying(1.0, 100.0, 60_000.0), 100.0 * 0.125);

    let err = fee_cfg("kind = \"percent_of_underlying\"\nbps = 3.0\npremium_cap_pct = 0.0\n")
        .build()
        .expect_err("a zero cap charges nothing and is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("premium_cap_pct must be > 0"), "{m}");
            assert!(m.contains("no fee at all"), "the refusal says what it costs: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The two-sided flat shape, which `engine.fee_rate` can only charge to both sides at once.
#[test]
fn the_percent_maker_taker_shape_says_the_two_sides_apart() {
    let schedule = fee_cfg("kind = \"percent_maker_taker\"\nmaker_bps = 2.0\ntaker_bps = 5.0\n")
        .build()
        .expect("the two-sided flat shape builds");
    assert_eq!(schedule, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 });
    // A table naming NEITHER side describes no cost, which `kind = "free"` already says.
    let err = fee_cfg("kind = \"percent_maker_taker\"\n")
        .build()
        .expect_err("a sideless percent shape is refused");
    assert!(matches!(err, HarnessError::Validation(_)));
    assert_eq!(fee_cfg("kind = \"free\"\n").build().unwrap(), FeeSchedule::Free);
}

/// A knob written under the wrong `kind` is REFUSED rather than ignored — the
/// "a key nothing reads is worse than an unimplemented feature" rule, applied inside one
/// table, where the cost of ignoring it is a run priced at something nobody wrote.
#[test]
fn a_knob_belonging_to_another_kind_is_refused_by_name() {
    let err = fee_cfg("kind = \"free\"\nper_share = 0.005\n")
        .build()
        .expect_err("a per-share knob under kind = free is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("engine.fee.per_share"), "it names the knob: {m}");
            assert!(m.contains("per_share_with_floor"), "...and the kind that reads it: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
    // The three pre-existing curve rates are plain defaulted scalars, so only a NON-ZERO one
    // is detectable — and a written zero configures nothing under either kind anyway.
    assert!(fee_cfg("kind = \"free\"\ntaker_rate = 0.0\n").build().is_ok());
    assert!(fee_cfg("kind = \"free\"\ntaker_rate = 0.07\n").build().is_err());
}

/// ⚠ **The venue lookup's whole point: the `.P` LANE.** A `BTCUSDT.P` run on binance is
/// costed at the PERP row, which is priced completely differently from the venue's spot row —
/// and the answer is the one `crates/vike-mount/src/lib.rs`'s `make_engine` computes for its
/// own `static_default`, asserted here as that same expression rather than as a copied
/// number.
#[test]
fn the_venue_kind_resolves_the_perp_lane_from_the_runs_own_symbol() {
    let cfg = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\n");
    let perp =
        cfg.build_for(&fee_series(&[("binance", "BTCUSDT.P")])).expect("the perp lane resolves");
    assert_eq!(perp, vike_model::fee_schedule_for(vike_catalog::fee_lane("binance", "BTCUSDT.P")));
    let spot =
        cfg.build_for(&fee_series(&[("binance", "BTCUSDT")])).expect("the spot lane resolves");
    assert_eq!(spot, vike_model::fee_schedule_for("binance"));
    assert_ne!(perp, spot, "the two lanes are priced apart — that IS the lookup");
    // The slice-free door (`build`, which `refusals` drives at load) answers the BARE lane.
    assert_eq!(cfg.build().unwrap(), spot);
}

/// A venue the run does not trade has no symbol to read a lane from, and answering with the
/// bare-venue row would charge a perp run at spot fees — the mispricing the lane key ended.
#[test]
fn a_venue_the_run_does_not_load_is_refused() {
    let err = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\n")
        .build_for(&fee_series(&[("polymarket", "0xTOK")]))
        .expect_err("a venue the slice does not trade is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("does not load"), "{m}");
            assert!(m.contains("polymarket"), "it names what the run DOES trade: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// One `EngineParams::fee_schedule` cannot serve two lanes, so a run holding both is refused
/// rather than half-mispriced — and `engine.fee.symbol` is the named way out.
#[test]
fn a_run_straddling_two_fee_lanes_is_refused_and_an_explicit_symbol_resolves_it() {
    let cfg = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\n");
    let both = fee_series(&[("binance", "BTCUSDT"), ("binance", "ETHUSDT.P")]);
    match cfg.build_for(&both).expect_err("two lanes, one schedule") {
        HarnessError::Validation(m) => {
            assert!(m.contains("straddle"), "{m}");
            assert!(m.contains("engine.fee.symbol"), "it names the way out: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
    let pinned = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\nsymbol = \"ETHUSDT.P\"\n");
    assert_eq!(
        pinned.build_for(&both).expect("an explicit symbol names the lane"),
        vike_model::fee_schedule_for("binance-perp")
    );
}

/// `pm_curve` exists because `fee_schedule_for("polymarket")` is a deliberate `Free`, so the
/// venue lookup would otherwise cost a prediction-market run at exactly zero. On any other
/// venue the flag reaches a function that delegates, so it would configure nothing — refused.
#[test]
fn pm_curve_opts_into_the_v2_regime_and_is_refused_off_polymarket() {
    let poly = fee_series(&[("polymarket", "0xTOK")]);
    let plain = fee_cfg("kind = \"venue\"\nvenue = \"polymarket\"\n");
    assert_eq!(plain.build_for(&poly).unwrap(), FeeSchedule::Free);
    let curved = fee_cfg("kind = \"venue\"\nvenue = \"polymarket\"\npm_curve = true\n");
    assert_eq!(curved.build_for(&poly).unwrap(), vike_model::POLYMARKET_V2_FEE_CURVE);
    let err = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\npm_curve = true\n")
        .build()
        .expect_err("pm_curve is polymarket-only");
    match err {
        HarnessError::Validation(m) => assert!(m.contains("polymarket-only"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The unknown-`kind` refusal is BUILT from [`FeeCfg::KINDS`], so an operator who typos is
/// never shown a roster the code does not accept. This is the `engine.sizer` rule
/// (`profile_surface`'s `the_unknown_sizer_kind_message_names_every_arm`) asserted on this
/// side, where the roster is a const rather than a match this crate can parse.
#[test]
fn the_unknown_fee_kind_message_names_every_accepted_kind() {
    let err = fee_cfg("kind = \"prob_scaled\"\n").build().expect_err("a typo is refused");
    let HarnessError::Validation(msg) = err else { panic!("expected a validation error") };
    assert!(msg.contains("unknown engine.fee.kind"), "{msg}");
    for kind in FeeCfg::KINDS {
        assert!(msg.contains(kind), "the refusal does not name {kind:?}: {msg}");
        // ...and every named kind is one `build` actually accepts, or the roster is a lie.
        assert!(
            fee_cfg(&format!("kind = {kind:?}\n")).validate_shape().is_err()
                || fee_cfg(&format!("kind = {kind:?}\n")).build().is_ok(),
            "{kind:?} is neither buildable bare nor refused with its own reason"
        );
    }
}

/// A `kind = "venue"` table with no venue is refused at LOAD, through the door
/// `BacktestProfile::refusals` drives — so the one venue-kind rule that CAN be answered
/// without the data slice is answered there.
#[test]
fn the_venue_kind_needs_a_venue_and_says_so_at_load() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "[engine.fee]\nkind = \"venue\"");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("needs engine.fee.venue"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// --- Task 12: [engine.sizer] ---------------------------------------------------------------

#[test]
fn no_sizer_is_the_default() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(p.engine.sizer.is_none());
}

#[test]
fn a_sizer_reaches_the_profile_and_builds() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"pass_through\"");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    let cfg = p.engine.sizer.as_ref().expect("sizer present");
    assert!(cfg.build().is_ok());
}

#[test]
fn an_unknown_sizer_kind_is_rejected() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"telepathy\"");
    match BacktestProfile::from_toml_str(&toml) {
        Err(HarnessError::Validation(m)) => assert!(m.contains("telepathy"), "{m}"),
        Ok(p) => match p.engine.sizer.as_ref().expect("present").build() {
            Err(HarnessError::Validation(m)) => assert!(m.contains("telepathy"), "{m}"),
            Err(other) => panic!("expected a validation error, got {other:?}"),
            // `Box<dyn PositionSizer>` doesn't implement `Debug`, so this arm can't be
            // printed — the assertion itself is the diagnostic.
            Ok(_) => panic!("expected build() to reject an unknown kind"),
        },
        Err(other) => panic!("expected a validation error, got {other:?}"),
    }
}

/// Every scalar (non-wrapping) sizer kind builds given its own required knob.
#[test]
fn every_scalar_sizer_kind_builds() {
    for (kind, extra) in [
        ("fixed_dollar", "amount = 1000.0"),
        ("fixed_shares", "shares = 10.0"),
        ("pct_equity", "pct = 0.1"),
        ("pct_volatility", "pct = 0.1"),
        ("max_risk_pct", "pct = 0.02"),
    ] {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            &format!("fee_rate = 0.001\n\n[engine.sizer]\nkind = \"{kind}\"\n{extra}"),
        );
        let p = BacktestProfile::from_toml_str(&toml)
            .unwrap_or_else(|e| panic!("{kind} should parse: {e:?}"));
        p.engine
            .sizer
            .as_ref()
            .unwrap()
            .build()
            .unwrap_or_else(|e| panic!("{kind} should build: {e:?}"));
    }
}

#[test]
fn a_scalar_sizer_kind_missing_its_knob_is_rejected() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"fixed_dollar\"");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("engine.sizer.amount"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_sizer_portfolio_heat_missing_its_base_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("[engine.sizer.base]"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_sizer_drawdown_throttle_missing_its_base_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"drawdown_throttle\"\n\
             sensitivity = 0.5\nfloor = 0.2",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("[engine.sizer.base]"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// Only the two WRAPPING kinds read `SizerCfg::base`. Under a scalar kind the table is read by
/// nobody, so the run installs ONE sizer while the operator reads the profile as a chain —
/// the same silent-no-op class every other rule in this validator refuses.
#[test]
fn a_base_under_a_non_wrapping_sizer_kind_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"fixed_dollar\"\namount = 1000.0\n\n\
             [engine.sizer.base]\nkind = \"pass_through\"",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("fixed_dollar"), "{m}");
            assert!(m.contains("[engine.sizer.base]"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The same refusal one level down, so the rule is not merely a top-level check: a base under
/// a scalar kind nested inside a legitimate wrapper is refused, and the message names the
/// nested path rather than the outer one.
#[test]
fn a_base_under_a_nested_non_wrapping_sizer_kind_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1\n\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0\n\n\
             [engine.sizer.base.base]\nkind = \"pass_through\"",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("[engine.sizer.base.base]"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_sizer_portfolio_heat_builds_with_a_nested_base() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
}

#[test]
fn a_sizer_drawdown_throttle_builds_with_a_nested_base() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"drawdown_throttle\"\n\
             sensitivity = 0.5\nfloor = 0.2\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
}

/// A chain of exactly `depth` `SizerCfg` nodes: `depth - 1` `"portfolio_heat"` wrappers ending
/// in one terminal `"pass_through"` leaf — so the whole chain always BUILDS (not just parses)
/// whenever `depth <= MAX_SIZER_DEPTH`. `depth` counts the same way `SizerCfg::build_at_depth`
/// does: the top-level `[engine.sizer]` table is depth 1.
fn nested_sizer_toml(depth: usize) -> String {
    assert!(depth >= 1);
    let mut path = "engine.sizer".to_string();
    let mut out = String::new();
    for level in 1..=depth {
        out.push_str(&format!("[{path}]\n"));
        if level < depth {
            out.push_str("kind = \"portfolio_heat\"\nmax_heat = 0.5\n");
            path.push_str(".base");
        } else {
            out.push_str("kind = \"pass_through\"\n");
        }
    }
    out
}

#[test]
fn a_sizer_chain_at_the_depth_limit_builds() {
    let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(MAX_SIZER_DEPTH));
    let p = BacktestProfile::from_toml_str(&toml).expect("parses and validates");
    assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
}

#[test]
fn a_sizer_chain_past_the_depth_limit_is_rejected() {
    let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(MAX_SIZER_DEPTH + 1));
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("MAX_SIZER_DEPTH"), "{m}");
            assert!(m.contains(&(MAX_SIZER_DEPTH + 1).to_string()), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// Manual measurement, not a CI regression test: does deserializing a `[engine.sizer]` chain
/// far past `MAX_SIZER_DEPTH` blow the stack DURING PARSING — before `SizerCfg::build`'s own
/// bound ever gets a chance to run — or does `toml`'s recursive descent handle a deep chain
/// fine, in which case `build` (called from `validate`) catches it cleanly?
///
/// Escalates depth and times each `from_toml_str` call, bailing out (and always failing, via
/// `panic!`, so nextest prints the captured measurements regardless of outcome) once a level
/// is already slow — no point re-proving a hang at a bigger number once one is observed.
/// `#[ignore]`d because the answer at the top of this escalation is a MULTI-MINUTE HANG (see
/// the Task 12 fix-round report — a first flat run at depth 50,000 hit nextest's own slow-test
/// timeout at ~244s with no result either way), which would make the default suite
/// unusable if this ran by default. Run explicitly:
/// `cargo nextest run -p vike-backtest --run-ignored ignored-only sizer_depth_probe`.
#[test]
#[ignore]
fn sizer_depth_probe() {
    use std::fmt::Write as _;
    use std::time::Instant;
    let mut report = String::new();
    for depth in [10usize, 20, 30, 50, 80, 100, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000] {
        let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(depth));
        let start = Instant::now();
        let result = BacktestProfile::from_toml_str(&toml);
        let elapsed = start.elapsed();
        let outcome = match &result {
            Ok(_) => "Ok (build's own MAX_SIZER_DEPTH check should have refused this — bug if \
                          so)"
            .to_string(),
            Err(e) => format!("Err({e:?})"),
        };
        let _ = writeln!(report, "depth={depth:>6} elapsed={elapsed:>10.2?} outcome={outcome}");
        // Growth here is not linear (see the report) — once one level is already slow, a
        // bigger one will not finish inside this test's own runtime budget.
        if elapsed.as_secs() >= 3 {
            let _ = writeln!(report, "(stopping escalation: depth={depth} was already slow)");
            break;
        }
    }
    panic!("sizer_depth_probe measurements (this failure is expected — see doc):\n{report}");
}

// --- G6: the settlement source ------------------------------------------------------------

#[test]
fn resolution_is_absent_unless_configured() {
    assert!(BacktestProfile::from_toml_str(BAR_TOML).unwrap().engine.resolution.is_none());
}

/// The source pays 1.0 to the winning outcome and 0.0 to the loser, and ONLY from the
/// window close onward — a spot symbol or another window's token is never touched.
#[test]
fn binary_outcome_source_pays_the_winner_from_the_close() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    let symbols: Vec<String> = p.data.resolved_series().into_iter().map(|s| s.symbol).collect();
    let (src, end_ts) = p.engine.resolution.as_ref().unwrap().build(None, &symbols).unwrap();

    let res_ms = (1_775_001_600 + 300) * 1000;
    assert_eq!(end_ts, Some(res_ms), "the sweep probes at THIS window's close, not a sentinel");

    // still trading
    assert_eq!(src("btc-updown-5m-1775001600#0", res_ms - 1), None);
    // resolved: outcome 0 won
    assert_eq!(src("btc-updown-5m-1775001600#0", res_ms), Some(1.0));
    assert_eq!(src("btc-updown-5m-1775001600#1", res_ms), Some(0.0));
    // the reference series is never settled or latched (port backlog G5)
    assert_eq!(src("BTCUSDT", res_ms), None);
    // a window with no winner row is never invented
    assert_eq!(src("btc-updown-5m-1775001900#0", i64::MAX / 4), None);
}

/// A non-binary INLINE `winning_index` is an authoring mistake, not a data fact — reject it
/// rather than coerce it to "both sides lose".
#[test]
fn a_non_binary_inline_winning_index_is_rejected() {
    for bad in ["2", "-1", "4"] {
        let toml = CROSS_VENUE_TOML.replace(
            "\"btc-updown-5m-1775001600\" = 0",
            &format!("\"btc-updown-5m-1775001600\" = {bad}"),
        );
        match BacktestProfile::from_toml_str(&toml).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("not a binary outcome index"), "{bad}: {m}")
            }
            other => panic!("{bad}: expected a validation error, got {other:?}"),
        }
    }
}

/// A window token in the slice with no payout would end the run marked at its last traded
/// price. That must be loud.
#[test]
fn a_window_with_no_resolution_row_is_rejected_at_build() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    let symbols = vec![
        "BTCUSDT".to_string(),
        "btc-updown-5m-1775001600#0".to_string(),
        "btc-updown-5m-1775001900#0".to_string(), // no winner row
    ];
    // `ResolutionSource` is a boxed closure and therefore not `Debug`, so match on the
    // Result rather than `unwrap_err`.
    match p.engine.resolution.as_ref().unwrap().build(None, &symbols) {
        Err(HarnessError::Validation(m)) => {
            assert!(m.contains("no binary winning_index"), "{m}");
            assert!(m.contains("btc-updown-5m-1775001900"), "{m}");
        }
        Err(other) => panic!("expected a validation error, got {other:?}"),
        Ok(_) => panic!("expected a validation error, got Ok"),
    }
}

/// The `slug,winning_index` CSV the ClickHouse export produces: header row, quoted slugs,
/// and non-binary rows that are DROPPED (a real on-chain fact) rather than coerced.
#[test]
fn resolution_reads_the_clickhouse_csv_and_drops_non_binary_rows() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("res.csv"),
        "\"slug\",\"winning_index\"\n\
             \"btc-updown-5m-1775001600\",1\n\
             \"btc-updown-5m-1775001900\",4\n",
    )
    .unwrap();

    let toml = CROSS_VENUE_TOML.replace(
        "[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0",
        "path = \"res.csv\"",
    );
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
    let (src, _) = p.engine.resolution.as_ref().unwrap().build(Some(dir.path()), &symbols).unwrap();
    // The slug is UNQUOTED before use — a quoted key matches no token symbol at all.
    assert_eq!(src("btc-updown-5m-1775001600#0", i64::MAX / 4), Some(0.0));
    assert_eq!(src("btc-updown-5m-1775001600#1", i64::MAX / 4), Some(1.0));
    // The `winning_index = 4` row was dropped, so that window has no payout...
    assert_eq!(src("btc-updown-5m-1775001900#0", i64::MAX / 4), None);
    // ...and asking to RUN it is an error, not a silent unsettled position.
    assert!(
        p.engine
            .resolution
            .as_ref()
            .unwrap()
            .build(Some(dir.path()), &["btc-updown-5m-1775001900#0".to_string()])
            .is_err()
    );
}

/// A relative sidecar path resolves next to the PROFILE, not against the CWD.
#[test]
fn a_relative_resolution_path_resolves_against_the_profile_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("res.csv"), "btc-updown-5m-1775001600,0\n").unwrap();
    let toml = CROSS_VENUE_TOML.replace(
        "[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0",
        "path = \"res.csv\"",
    );
    let profile_path = dir.path().join("run.toml");
    std::fs::write(&profile_path, &toml).unwrap();

    let p = BacktestProfile::from_path(&profile_path).unwrap();
    assert_eq!(p.base_dir.as_deref(), Some(dir.path()));
    let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
    assert!(p.engine.resolution.as_ref().unwrap().build(p.base_dir.as_deref(), &symbols).is_ok());
}

#[test]
fn an_unknown_resolution_kind_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("\"binary_outcome\"", "\"binary\"");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("unknown engine.resolution.kind"), "{m}")
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// An explicit `end_ts` overrides the derived "latest window close in the slice".
#[test]
fn an_explicit_resolution_end_ts_overrides_the_derived_one() {
    let toml = CROSS_VENUE_TOML
        .replace("kind = \"binary_outcome\"", "kind = \"binary_outcome\"\nend_ts = \"12345\"");
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let symbols = vec!["btc-updown-5m-1775001600#0".to_string()];
    let (_, end) = p.engine.resolution.as_ref().unwrap().build(None, &symbols).unwrap();
    assert_eq!(end, Some(12_345));
}

// --- the opt-in `[risk]` section (runprofile-wiring-step2) --------------------------------

/// Absent `[risk]` must parse to `None` — the byte-identical-default claim: nothing in
/// `BAR_TOML`/`TICK_TOML` sets it, so every profile written before this field existed keeps
/// parsing exactly as before.
#[test]
fn risk_is_absent_unless_configured() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).unwrap();
    assert!(p.risk.is_none());
    let p = BacktestProfile::from_toml_str(TICK_TOML).unwrap();
    assert!(p.risk.is_none());
}

/// A `[risk]` section parses into the SAME `vike_exec::ProfileRisk` fields paper/live use, and
/// `ProfileRisk::to_risk_limits` maps them onto the real `RiskLimits` the engine reads.
#[test]
fn risk_section_parses_into_profile_risk() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[risk]\nmax_notional_per_order = 250000.0\nmax_leverage = 2.0\n\
             min_qty = 0.001",
    );
    let p = BacktestProfile::from_toml_str(&toml).unwrap();
    let risk = p.risk.as_ref().expect("`[risk]` configured");
    assert_eq!(risk.max_notional_per_order, Some(250_000.0));
    assert_eq!(risk.max_leverage, Some(2.0));
    assert_eq!(risk.min_qty, Some(0.001));

    let limits = risk.to_risk_limits();
    assert_eq!(limits.max_notional_per_order, Some(250_000.0));
    assert_eq!(limits.max_leverage, Some(2.0));
    // A backtest `[risk] max_leverage` arms the SAME buying-power check paper/live arm
    // (issue #822): 2x ⇒ 50% initial margin. `SimBroker::build_risk_gate` already maps its own
    // `EngineParams::leverage` this way, so the two edges now agree on what "2x" means.
    assert_eq!(limits.im_requirement, Some(0.5));
    assert_eq!(limits.min_qty, Some(0.001));
}

/// A typo'd `[risk]` key must fail the profile — `ProfileRisk` carries its own
/// `deny_unknown_fields`, so the nested-denial policy applies inside `[risk]` too, exactly as
/// it does for `vike-core`'s `RunProfile`.
#[test]
fn unknown_risk_key_is_rejected() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[risk]\nmax_levarage = 10.0");
    let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
    assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
}

/// `risk.max_orders_per_window` is a wall-clock throttle the sim gate always disarms
/// (`SimBroker::build_risk_gate`) — REJECTED at load rather than silently ignored, the
/// documented divergence this wiring step must surface loudly instead of papering over.
#[test]
fn risk_max_orders_per_window_is_rejected_at_load() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[risk]\nmax_orders_per_window = 5\nwindow_ms = 1000",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("max_orders_per_window"), "{m}");
            assert!(m.contains("wall-clock"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The rejection fires from the TICK lane too — the throttle is meaningless in sim time
/// regardless of bar/tick mode.
#[test]
fn risk_max_orders_per_window_is_rejected_on_the_tick_lane_too() {
    let toml = TICK_TOML.replace(
        "snap_to_properties = true",
        "snap_to_properties = true\n\n[risk]\nmax_orders_per_window = 1\nwindow_ms = 1000",
    );
    let err = BacktestProfile::from_toml_str(&toml).unwrap_err();
    assert!(matches!(err, HarnessError::Validation(_)), "got {err:?}");
}

/// Every OTHER `risk.*` limit is unaffected by the `max_orders_per_window` gate — a profile
/// setting only operator-budget fields (no throttle) parses and validates cleanly.
#[test]
fn risk_without_max_orders_per_window_is_accepted() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[risk]\nmax_notional_per_order = 1000.0\nmax_total_exposure = 5000.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("no throttle set -> accepted");
    assert_eq!(p.risk.unwrap().max_notional_per_order, Some(1000.0));
}

#[test]
fn cash_gate_reaches_the_profile() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\ncash_gate = true");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.cash_gate);
}

/// `cash_gate`'s ONLY readers are `StrategyEngine::run`'s `granular` mask and its
/// `fill_step_gated` branch — both inside the BAR loop — and `fill_step_gated`'s single caller
/// is that branch. `run_ticks` never consults it, so a tick profile setting it would parse,
/// validate, reach `EngineParams` and change nothing: the defect this whole surface exists to
/// refuse, which is why it now joins the lane-asymmetric refusals.
#[test]
fn cash_gate_is_bar_mode_only() {
    let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = true");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("cash_gate"), "{m}");
            assert!(m.contains("bar-mode only"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The companion: an explicit `false` on a tick profile configures nothing and must keep
/// validating — the refusal is on the value that would mislead, not on the key's presence.
#[test]
fn cash_gate_false_on_a_tick_profile_is_accepted() {
    let toml = TICK_TOML.replace("snap_to_properties = true", "cash_gate = false");
    let p = BacktestProfile::from_toml_str(&toml).expect("an explicit false configures nothing");
    assert!(!p.engine.cash_gate);
}

#[test]
fn cash_gate_defaults_off() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(!p.engine.cash_gate, "the default must stay byte-identical to before this key");
}

#[test]
fn margin_keys_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nmaint_margin = 0.005\nliq_buffer = 0.2\n\
             venue_style_liquidation = true",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.maint_margin, 0.005);
    assert_eq!(p.engine.liq_buffer, 0.2);
    assert!(p.engine.venue_style_liquidation);
}

/// `liq_buffer`'s engine default is 0.10, NOT 0.0 — a serde `Default::default()` here would
/// silently change every profile that omits the key.
#[test]
fn liq_buffer_defaults_to_the_engine_value() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.liq_buffer, 0.10);
    assert_eq!(p.engine.maint_margin, 0.0);
    assert!(!p.engine.venue_style_liquidation);
}

#[test]
fn a_negative_maint_margin_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nmaint_margin = -0.01");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("maint_margin"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn volume_limit_reaches_the_profile() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nvolume_limit = 0.05");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.volume_limit, Some(0.05));
}

#[test]
fn volume_limit_defaults_to_none() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.volume_limit, None);
}

/// A participation cap above 1.0 claims more than the whole bar traded.
#[test]
fn a_volume_limit_over_one_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nvolume_limit = 1.5");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("volume_limit"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn position_caps_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nmax_open_positions = 10\nmax_open_long = 6\nmax_open_short = 4",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.max_open_positions, 10);
    assert_eq!(p.engine.max_open_long, 6);
    assert_eq!(p.engine.max_open_short, 4);
}

/// 0 is the engine's "unlimited", which is what every profile did before these keys.
#[test]
fn position_caps_default_to_unlimited() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.max_open_positions, 0);
    assert_eq!(p.engine.max_open_long, 0);
    assert_eq!(p.engine.max_open_short, 0);
}

#[test]
fn multipliers_reach_the_profile() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nmultiplier = 1.0\n\n[engine.multipliers]\n\
             BTCUSDT = 50.0\nETHUSDT = 20.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.multiplier, 1.0);
    assert_eq!(p.engine.multipliers.get("BTCUSDT"), Some(&50.0));
    assert_eq!(p.engine.multipliers.get("ETHUSDT"), Some(&20.0));
}

#[test]
fn multiplier_defaults_to_one_and_the_table_is_empty() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.multiplier, 1.0);
    assert!(p.engine.multipliers.is_empty());
}

#[test]
fn a_zero_multiplier_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nmultiplier = 0.0");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("multiplier"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The per-symbol table gets its own negative-value test, distinct from the global
/// `multiplier` scalar above — `validate` is first-failure-wins, so this sets exactly one bad
/// per-symbol value and leaves the global scalar at its valid default. The key is a symbol
/// the slice ACTUALLY loads, so the value rule is what fails rather than the key rule below.
#[test]
fn a_zero_per_symbol_multiplier_is_rejected() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nBTCUSDT = 0.0");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("multipliers"), "{m}");
            assert!(m.contains("BTCUSDT"), "{m}");
            assert!(m.contains("> 0"), "the VALUE rule must be the one that fires: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// A `multipliers` key naming a symbol the run does not load is silently inert today:
/// `StrategyEngine::new` matches by EXACT name and otherwise falls back to the global
/// `multiplier`, so a typo'd row leaves every position sized at the global while the profile
/// reads as contract-sized. The slice is named back so the typo is visible in the message.
#[test]
fn a_multiplier_for_a_symbol_outside_the_slice_is_rejected() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nES = 50.0");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("engine.multipliers.ES"), "{m}");
            assert!(m.contains("BTCUSDT"), "the slice must be named back: {m}");
            assert!(m.contains("ETHUSDT"), "the slice must be named back: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// ...and the companion, so the rule above cannot be satisfied by refusing everything: a row
/// naming a symbol the slice DOES load still validates.
#[test]
fn a_multiplier_for_a_symbol_in_the_slice_is_accepted() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.multipliers]\nETHUSDT = 50.0");
    let p = BacktestProfile::from_toml_str(&toml).expect("the symbol is in the slice");
    assert_eq!(p.engine.multipliers.get("ETHUSDT"), Some(&50.0));
}

#[test]
fn leverage_keys_reach_the_profile() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 5.0\nclamp_to_leverage = true");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.leverage, Some(5.0));
    assert!(p.engine.clamp_to_leverage);
}

#[test]
fn leverage_defaults_to_none() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.leverage, None);
    assert!(!p.engine.clamp_to_leverage);
}

#[test]
fn a_non_positive_leverage_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 0.0");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("leverage"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// `clamp_to_leverage` with no `leverage` set has nothing to clamp to — the brief's step 4
/// adds this rule but its own step 1 test list omits a case for it, so this closes that gap.
#[test]
fn clamp_to_leverage_without_leverage_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nclamp_to_leverage = true");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("clamp_to_leverage"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The end state the two older rules left UNSTATED: `clamp_to_leverage` is unavailable to any
/// profile carrying `[risk]`. Before the reorder, this exact profile was told to add
/// `engine.leverage` — and adding it was then refused by the next rule, a dead end with the
/// governing constraint named nowhere.
#[test]
fn clamp_to_leverage_with_a_risk_table_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nclamp_to_leverage = true\n\n[risk]\nmax_notional_per_order = 1000.0",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("clamp_to_leverage"), "{m}");
            assert!(m.contains("[risk]"), "{m}");
            // The misdirection this replaces: never send the operator to add a key the next
            // rule refuses.
            assert!(
                !m.contains("needs engine.leverage"),
                "the old rule-18 misdirection must not fire first: {m}"
            );
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// ...and with `engine.leverage` supplied as well, the SAME refusal must answer — the clamp
/// rule is what governs, whichever way the operator arrived at the combination.
#[test]
fn clamp_to_leverage_with_a_risk_table_and_leverage_is_rejected_the_same_way() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nleverage = 5.0\nclamp_to_leverage = true\n\n[risk]\n\
             max_notional_per_order = 1000.0",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("clamp_to_leverage"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// `SimBroker::build_risk_gate` matches `(&p.risk_limits, p.leverage)`; with the clamp OFF its
/// `(Some(l), _)` arm takes `risk_limits` UNCONDITIONALLY whenever `[risk]` is present — even
/// one that never sets `max_leverage` — and silently drops `engine.leverage`. Declaring both
/// must be a named refusal, not a silent precedence pick. (The clamp-ON case has the opposite
/// precedence and its own refusal, two tests above.)
#[test]
fn engine_leverage_with_a_risk_table_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\nleverage = 5.0\n\n[risk]\nmax_notional_per_order = 1000.0",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("engine.leverage"), "{m}");
            assert!(m.contains("[risk]") || m.contains("risk"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The companion to the rejection above: `engine.leverage` with NO `[risk]` table is exactly
/// the shape `SimBroker::build_risk_gate`'s `(None, Some(lev))` arm consumes, and must keep
/// working.
#[test]
fn engine_leverage_alone_with_no_risk_table_still_works() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nleverage = 5.0");
    let p = BacktestProfile::from_toml_str(&toml).expect("leverage alone, no [risk] table");
    assert_eq!(p.engine.leverage, Some(5.0));
    assert!(p.risk.is_none());
}

#[test]
fn settlement_period_reaches_the_profile() {
    let toml =
        BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsettlement_period_ms = 86400000");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert_eq!(p.engine.settlement_period_ms, Some(86_400_000));
}

#[test]
fn settlement_period_defaults_to_none() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert_eq!(p.engine.settlement_period_ms, None);
}

#[test]
fn a_non_positive_settlement_period_is_rejected() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsettlement_period_ms = 0");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("settlement_period_ms"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn session_gate_reaches_the_profile() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsession_gate = true");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.session_gate);
}

#[test]
fn session_gate_defaults_off() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(!p.engine.session_gate);
}

/// RULED (see `EngineCfg::session_gate`'s doc): step 1 found no by-name `SessionCalendar`
/// lookup, so this task's deliverable is `session_gate` alone — no `[engine.sessions]` table.
/// The brief's own third test was written as a decision point that passed either way; this
/// replaces it with the real assertion for the scope that was actually chosen: the gate parses
/// and validates cleanly with no calendar configured.
///
/// ⚠ This comment used to add "fail-permissive always-open at runtime", and that was FALSE as
/// a general claim — `crate::hist_replay::replay_ticks` sets `default_venue` unconditionally
/// and `bar_engine_params` sets it under `snap_to_properties`, so each symbol resolves through
/// `vike_model::session_for`, which hands back a real `FX_WEEK` calendar for the FX/CFD
/// venues. Arming the gate with no calendar table is a MODELLING choice, not a no-op. What
/// this test asserts is only what its name says: no calendar table is needed to LOAD it.
#[test]
fn session_gate_without_a_calendar_parses_and_validates_cleanly() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "fee_rate = 0.001\nsession_gate = true");
    let p =
        BacktestProfile::from_toml_str(&toml).expect("no calendar table needed to arm the gate");
    assert!(p.engine.session_gate);
}

/// The text a profile was parsed FROM comes back with it, so the run record can store the bytes
/// that actually drove the run rather than re-reading a file that may have changed since.
#[test]
fn loading_a_profile_hands_back_the_exact_text_it_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sma.toml");
    std::fs::write(&path, BAR_TOML).unwrap();

    let (profile, text) = BacktestProfile::from_path_with_text(&path).unwrap();

    assert_eq!(text, BAR_TOML, "byte for byte, comments and blank lines included");
    assert_eq!(profile.data.interval, "1h");
    assert!(profile.base_dir.is_some(), "and it still records the sidecar base directory");
}

/// The old door keeps working and keeps meaning the same thing — it has callers this plan does
/// not touch, and it must not grow a second parse.
#[test]
fn the_path_loader_still_answers_and_delegates_to_the_same_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sma.toml");
    std::fs::write(&path, BAR_TOML).unwrap();

    let only = BacktestProfile::from_path(&path).unwrap();
    let (both, _) = BacktestProfile::from_path_with_text(&path).unwrap();

    assert_eq!(only.data.interval, both.data.interval);
    assert_eq!(only.base_dir, both.base_dir);
}

/// A profile with mistakes in four `[engine]` keys and one in `[data]`, kept deliberately
/// PARSEABLE so every one of them is semantic: this is what the accumulating door exists for,
/// and the exact KEY SEQUENCE is asserted rather than a count, because the sequence is the
/// contract `BacktestProfile::validate` reads element zero of.
const FIVE_MISTAKES_TOML: &str = r#"
name = "five-mistakes"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-02T00"
to = "2026-01-01T00"

[engine]
cash = 0.0
feed_latency = true
maint_margin = -1.0
multiplier = 0.0

[strategy]
name = "sma_cross"
"#;

/// ⚠ **Five mistakes, ONE pass** — the whole point of the second door. Before it existed an
/// operator learned about `engine.cash`, fixed it, learned about `engine.feed_latency`, fixed
/// that, and paid five edit-and-rerun cycles for one file; on the remote route each of those
/// was also a dial to a compute daemon.
#[test]
fn validate_all_reports_every_mistake_in_one_pass() {
    let p: BacktestProfile =
        toml::from_str(FIVE_MISTAKES_TOML).expect("it PARSES — every mistake here is semantic");
    let all = p.validate_all();
    let keys: Vec<&str> = all.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "engine.cash",
            "engine.feed_latency",
            "engine.maint_margin",
            "engine.multiplier",
            "data.from",
        ],
        "every rule that refused, in the order the rules run: {all:?}"
    );
    assert!(
        all.iter().all(|d| d.severity == vike_model::Severity::Error),
        "a load-time refusal is all this validator has to say, so every row is an Error: \
             {all:?}"
    );
    assert!(
        all[3].message.contains("scales every position's notional"),
        "and each row carries the validator's OWN sentence, not a summary of it: {:?}",
        all[3].message
    );
}

/// ⚠ **The first diagnostic is the error the first-error door returns.** The two doors are one
/// walk, so they cannot disagree about which mistake governs — and if a future edit reorders a
/// rule, this is what goes red rather than the published surface (which sorts its refusal rows
/// by message text and would notice nothing).
#[test]
fn the_first_diagnostic_is_what_validate_itself_returns() {
    let p: BacktestProfile = toml::from_str(FIVE_MISTAKES_TOML).expect("it parses");
    let err = p.validate().expect_err("five mistakes is not zero mistakes");
    let all = p.validate_all();
    assert_eq!(all[0].key, "engine.cash");
    assert_eq!(all[0].message, err.message(), "same walk, same governing refusal");
}

/// ⚠ **The accumulator carries the real [`HarnessError`], which is why the VARIANT survives.**
/// An unparseable stamp is a [`HarnessError::Parse`] — `rejects_unparsable_timestamp` asserts
/// exactly that — and rebuilding an error from a `Diagnostic`'s text would have quietly turned
/// it into a `Validation`, changing what every existing caller sees for a profile nobody
/// edited.
#[test]
fn the_accumulating_door_does_not_change_the_error_variant() {
    let toml = BAR_TOML.replace("from = \"2026-01-01T00\"", "from = \"not-a-timestamp\"");
    let p: BacktestProfile = toml::from_str(&toml).expect("it parses; the stamp is semantic");
    let err = p.validate().expect_err("an unparseable stamp is refused");
    assert!(matches!(err, HarnessError::Parse(_)), "got {err:?}");
    let all = p.validate_all();
    assert_eq!(all.len(), 1, "one mistake, one diagnostic: {all:?}");
    assert_eq!(all[0].key, "data", "anchored on the table, since the range spans two keys");
    assert_eq!(all[0].message, err.message());
}

/// A profile that loads collects nothing — the emptiness is the success signal, so a caller
/// can drive the accumulating door alone and never ask the other one.
#[test]
fn a_profile_that_loads_collects_no_diagnostics() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("the fixture loads");
    assert!(p.validate_all().is_empty(), "{:?}", p.validate_all());
}

/// ⚠ A refused `[data]` slice SKIPS the `engine.multipliers` key check rather than answering
/// it from wreckage: that rule names the loaded symbols back, and a "the data slice is: "
/// listing built from a malformed slice would be a second, wrong refusal chasing the first.
/// One diagnostic, not two.
#[test]
fn a_refused_slice_does_not_produce_a_second_refusal_about_the_symbols_it_never_resolved() {
    let toml = BAR_TOML
        .replace("\"BTCUSDT\", \"ETHUSDT\"", "\"BTCUSDT\", \"BTCUSDT\"")
        .replace("fee_rate = 0.001", "fee_rate = 0.001\nmultipliers = { NOPE = 50.0 }");
    let p: BacktestProfile = toml::from_str(&toml).expect("it parses; the duplicate is semantic");
    let all = p.validate_all();
    assert_eq!(all.len(), 1, "the duplicate symbol alone: {all:?}");
    assert_eq!(all[0].key, "data");
    assert!(all[0].message.contains("duplicate symbol"), "{:?}", all[0].message);
}
