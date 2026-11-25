//! The section alias, queue/fill models, timeframes, the data slice and series, impact, funding.

use super::*;
use std::assert_matches;

/// The grid section is `[paramscan]`, and only that: the owner's ruling 20 (phase 4) put the one word
/// on the wire and in every profile, so the `sweep` alias that kept the R2 rename free is gone and a
/// profile still writing `[sweep]` is refused at LOAD ([`ProfileBacktest`] is `deny_unknown_fields`).
#[test]
fn the_grid_section_is_paramscan_and_sweep_is_refused() {
    let grid = "\nfast = [5, 10]\nslow = [20, 30]\n";
    let new = ProfileBacktest::from_toml_str(&format!("{BAR_TOML}\n[paramscan]{grid}"))
        .expect("[paramscan] is the name")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(new.is_paramscan(), "it declares a parameter search");
    assert_eq!(
        new.paramscan.as_ref().map(vike_model::params::Params::len),
        Some(2),
        "…and the grid is the one that was written, not an empty table"
    );
    let old_word = "sweep";
    let err = ProfileBacktest::from_toml_str(&format!("{BAR_TOML}\n[{old_word}]{grid}"))
        .expect_err("the legacy section is refused since ruling 20");
    assert!(err.to_string().contains(old_word), "the refusal names the key: {err}");
}

/// ⚠ **`vike_model::run_request::paramscan::PARAMSCAN_SECTIONS` is exactly the set of section names
/// this profile LOADS a grid under**, and `declares_a_grid` answers as `is_paramscan` does.
///
/// The const once held the serde alias's spelling too (removed by the owner's ruling 20); this test
/// is what holds the const and the engine together. Driven through the
/// real deserializer: every const member loads as a grid when it holds a key and is no grid when
/// it is a bare header (the "present AND non-empty" rule), and a name OUTSIDE the const is
/// refused as an unknown field — so a third spelling cannot be added to the engine (or dropped
/// from it) without this test, and the CLI's route pre-parse that reads the const, going red.
#[test]
fn the_paramscan_sections_are_exactly_the_spellings_the_engine_loads() {
    use vike_model::run_request::paramscan::{
        PARAMSCAN_SECTIONS, ParamscanSectionShape, declares_a_grid,
    };

    for name in PARAMSCAN_SECTIONS {
        let full =
            ProfileBacktest::from_toml_str(&format!("{BAR_TOML}\n[{name}]\nfast = [5, 10]\n"))
                .unwrap_or_else(|e| {
                    panic!("[{name}] is a PARAMSCAN_SECTIONS member and must load: {e}")
                })
                .with_stand_in(vike_strategy_lab::stand_in);
        assert!(full.is_paramscan(), "[{name}] with a key is a grid");
        let bare = ProfileBacktest::from_toml_str(&format!("{BAR_TOML}\n[{name}]\n"))
            .unwrap_or_else(|e| panic!("a bare [{name}] must load: {e}"))
            .with_stand_in(vike_strategy_lab::stand_in);
        assert!(!bare.is_paramscan(), "a present-and-EMPTY [{name}] is no grid");

        // The shared predicate gives the same two answers for the same two shapes.
        let shape =
            |empty| move |n: &str| (n == name).then_some(ParamscanSectionShape::Table { empty });
        assert_eq!(declares_a_grid(shape(false)), Some(full.is_paramscan()));
        assert_eq!(declares_a_grid(shape(true)), Some(bare.is_paramscan()));
    }
    // A section name the const does not list is not a grid section: the profile refuses it.
    refused_at_load(
        &format!("{BAR_TOML}\n[paramscan_extra]\nfast = [5]\n"),
        HarnessError::Parse,
        &[],
    );
}

/// ⚠ **`vike_model::run_request::profile::PROFILE_TOP_LEVEL_KEYS` is exactly the set of top-level keys
/// this profile LOADS**, both ways: the CLI's `--set` grammar and its published roster read that const
/// and cannot name this crate, so this holds it to the schema the profile's own serde attributes derive
/// (a skipped field is no key).
#[test]
fn the_profile_top_level_keys_are_exactly_the_keys_the_profile_loads() {
    use std::collections::BTreeSet;
    use vike_model::run_request::profile::PROFILE_TOP_LEVEL_KEYS;

    let schema = schemars::schema_for!(ProfileBacktest).to_value();
    let loads: BTreeSet<&str> = schema["properties"]
        .as_object()
        .expect("the profile schema lists its properties")
        .keys()
        .map(String::as_str)
        .collect();
    let declared: BTreeSet<&str> = PROFILE_TOP_LEVEL_KEYS.into_iter().collect();
    assert_eq!(loads, declared, "PROFILE_TOP_LEVEL_KEYS and ProfileBacktest's keys disagree");
}

/// `[strategy.params]` and `[paramscan]` are TABLES of parameters (`vike_model::params::Params`),
/// so a scalar or a list in their place is refused when the profile LOADS. Before the params
/// vocabulary a scalar `strategy.params` parsed and every reader saw it as an empty bag.
#[test]
fn a_params_or_paramscan_that_is_not_a_table_is_refused_at_load() {
    let tabled = "[strategy.params]\nfast = 10\nslow = 20\n";
    assert!(BAR_TOML.contains(tabled), "the BAR_TOML anchor must still match");
    for scalar in ["params = 5\n", "params = \"fast\"\n", "params = [1, 2]\n"] {
        let toml = BAR_TOML.replace(tabled, scalar);
        refused_at_load(&toml, HarnessError::Parse, &[]);
    }
    for scalar in ["paramscan = 5\n", "sweep = 5\n", "paramscan = [1, 2]\n"] {
        refused_at_load(&format!("{scalar}{BAR_TOML}"), HarnessError::Parse, &[]);
    }
    // The default is still an empty table: a `[strategy]` with no params loads.
    let bare = ProfileBacktest::from_toml_str(&BAR_TOML.replace(tabled, ""))
        .expect("a strategy with no params loads")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(bare.strategy.as_ref().unwrap().params.is_empty());
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
file = "trend/buy_hold.rs"
"#;

#[test]
fn queue_model_parses_resolves_and_is_tick_only() {
    // Each accepted string resolves to the right kind on a tick profile.
    let kind = |qm: &str| {
        let toml =
            TICK_TOML.replace("snap_to_properties = true", &format!("queue_model = \"{qm}\""));
        ProfileBacktest::from_toml_str(&toml)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in)
            .engine
            .queue_model_kind()
            .unwrap()
    };
    assert_matches!(kind("risk_averse"), Some(QueueModelKind::RiskAverse));
    assert_matches!(kind("prob_log"), Some(QueueModelKind::ProbLog));
    assert_matches!(kind("prob_power"), Some(QueueModelKind::ProbPower(n)) if n == 1.0);
    assert_matches!(kind("prob_power:2.5"), Some(QueueModelKind::ProbPower(n)) if n == 2.5);
    // Absent ⇒ None (the frozen simple-crossing fill).
    assert!(
        ProfileBacktest::from_toml_str(TICK_TOML)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in)
            .engine
            .queue_model_kind()
            .unwrap()
            .is_none()
    );
    // An unrecognized string fails validation at LOAD (fail-fast, not a silent fallback).
    let bad = TICK_TOML.replace("snap_to_properties = true", "queue_model = \"fifo\"");
    assert!(ProfileBacktest::from_toml_str(&bad).is_err(), "unknown queue_model rejected");
    // The old misspelling is refused, and the refusal names the word to write instead.
    let old = TICK_TOML.replace("snap_to_properties = true", "queue_model = \"risk_adverse\"");
    let err = ProfileBacktest::from_toml_str(&old).unwrap_err();
    assert!(
        format!("{err}").contains("spelled \"risk_averse\""),
        "the hint names the new word: {err}"
    );
    // Tick-mode only: a bar profile setting it is rejected with a clear message.
    let barq = BAR_TOML.replace("# engine", "queue_model = \"risk_averse\"");
    let err = ProfileBacktest::from_toml_str(&barq).unwrap_err();
    assert!(format!("{err}").contains("tick-mode only"), "bar-mode queue_model rejected: {err}");
}

/// The tick-lane `fill_model` knob parses STRICTLY, like `queue_model`: absent ⇒ the default
/// `Tick`, the one accepted spelling resolves to `L2Book`, and a typo fails at LOAD with the
/// valid set named — never the old silent optimistic-`Tick` fallback that undid the #819
/// depth-cap realism knob.
#[test]
fn fill_model_parses_strictly_and_defaults_to_tick() {
    // Absent ⇒ the default L1 spread-crossing Tick model.
    let p = ProfileBacktest::from_toml_str(TICK_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.fill_model_kind().unwrap(), FillModelKind::Tick);

    // The accepted spelling resolves to the depth-capped model; case/whitespace normalize
    // (the same trim + lowercase idiom `queue_model_kind` uses).
    let kind = |fm: &str| {
        let toml =
            TICK_TOML.replace("snap_to_properties = true", &format!("fill_model = \"{fm}\""));
        ProfileBacktest::from_toml_str(&toml)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in)
            .engine
            .fill_model_kind()
            .unwrap()
    };
    assert_eq!(kind("l2book"), FillModelKind::L2Book);
    assert_eq!(kind("L2Book"), FillModelKind::L2Book);
    assert_eq!(kind(" l2book "), FillModelKind::L2Book);

    // A typo fails validation at LOAD, naming the valid set (fail-fast, not a silent
    // fallback). "l2_book" is exactly the audit-finding footgun.
    for junk in ["l2_book", "tick", "book"] {
        let bad =
            TICK_TOML.replace("snap_to_properties = true", &format!("fill_model = \"{junk}\""));
        let err = ProfileBacktest::from_toml_str(&bad).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown fill_model") && msg.contains("l2book"),
            "junk fill_model {junk:?} rejected with the valid set named: {msg}"
        );
    }
}

#[test]
fn timeframes_reach_the_profile() {
    let toml = BAR_TOML.replace("# engine", "# engine\ntimeframes = [\"4h\", \"1d\"]");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.timeframes, vec!["4h".to_string(), "1d".to_string()]);
}

#[test]
fn timeframes_default_to_empty() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .expect("parses")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.engine.timeframes.is_empty());
}

/// An unparseable timeframe is refused at LOAD. Left to the engine it is
/// `parse_timeframe(tf).expect("valid timeframe")` — a process abort, where every neighbouring
/// `[engine]` key gives a named error.
#[test]
fn an_unparseable_timeframe_is_rejected_at_load() {
    let toml = BAR_TOML.replace("# engine", "# engine\ntimeframes = [\"4hh\"]");
    refused_at_load(&toml, HarnessError::Validation, &["timeframes", "4hh"]);
}

/// `interval_ms` accepts a zero count, so `"0h"` parses to 0 ms and would make every window
/// boundary the same instant. Refused here because nothing downstream checks it.
#[test]
fn a_zero_length_timeframe_is_rejected_at_load() {
    let toml = BAR_TOML.replace("# engine", "# engine\ntimeframes = [\"0h\"]");
    refused_at_load(&toml, HarnessError::Validation, &["timeframes"]);
}

/// A coarser-than-base timeframe is the whole point; a FINER one cannot be synthesised from the
/// base stream and would silently return the base bars re-labelled.
#[test]
fn a_timeframe_finer_than_the_base_interval_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\ntimeframes = [\"1m\"]");
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
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.name.as_deref(), Some("bar-demo"));
    assert_eq!(p.data.venue.as_deref(), Some("binance"));
    assert_eq!(p.data.symbols, vec!["BTCUSDT", "ETHUSDT"]);
    assert_eq!(p.data.kind, ReplayKind::Bar);
    assert_eq!(p.data.interval, "1h");
    assert_eq!(p.engine.cash, 100000.0);
    // defaults
    assert!(!p.engine.snap_to_properties);
    assert_eq!(p.engine.seed_bar_interval_ms, None);
    // The harness default for the stop-verb release knob is `false`, matching the raw
    // `StrategyEngineConfig` default — see `default_emulator_release_stops`'s doc for why this is not
    // the mirror-live `true` its doc used to claim.
    assert!(!p.engine.emulator_release_stops);
    assert_eq!(p.strategy.as_ref().unwrap().file, "trend/sma_cross.rs");

    // 2026-01-01T00:00 UTC and 2026-01-02T00:00 UTC (verified against `date -u -d`).
    let r = p.range().unwrap();
    assert_eq!(r.start, Some(1_767_225_600_000));
    assert_eq!(r.end, Some(1_767_312_000_000));
}

/// The harness stop-verb release knob defaults to `false` (matching
/// `StrategyEngineConfig::default()`) and an explicit `emulator_release_stops = true` parses through
/// to arm the opt-in mirror-live release. ⚠ This test previously asserted the OPPOSITE default
/// (`true`) — that default never reached the engine through either `StrategyEngineConfig`
/// construction site in `run`, so every harness run was `false` regardless; see
/// `default_emulator_release_stops`'s doc.
#[test]
fn emulator_release_stops_defaults_false_and_parses_true() {
    // absent -> the harness default, false (matches StrategyEngineConfig::default())
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(!p.engine.emulator_release_stops);

    // explicit true threads through
    let toml = BAR_TOML.replace("# engine", "# engine\nemulator_release_stops = true");
    let p =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.engine.emulator_release_stops);

    // explicit false also parses
    let toml = BAR_TOML.replace("# engine", "# engine\nemulator_release_stops = false");
    let p =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert!(!p.engine.emulator_release_stops);
}

#[test]
fn parses_tick_profile() {
    let p = ProfileBacktest::from_toml_str(TICK_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.data.kind, ReplayKind::Tick);
    assert_eq!(p.data.symbols, vec!["0xTOK"]);
    assert!(p.engine.snap_to_properties);
    // interval defaults even though tick data doesn't consume it.
    assert_eq!(p.data.interval, "1d");

    let r = p.range().unwrap();
    // 2026-04-13T19:00 / 20:00 UTC (verified against `date -u -d`).
    assert_eq!(r.start, Some(1_776_106_800_000));
    assert_eq!(r.end, Some(1_776_110_400_000));
}

/// A tick profile keeps its equity curve by TIME unless it names a density (decision 0078's amendment of
/// 2026-11-20): at its seed bars' step, else a minute; `1` keeps every tick, as an absent key did before.
#[test]
fn a_tick_profile_keeps_its_curve_by_time_unless_it_names_a_density() {
    use vike_simulator::EquitySampling;
    let sampling = |engine: &str| {
        let toml = TICK_TOML.replace("snap_to_properties = true", engine);
        let p = ProfileBacktest::from_toml_str(&toml).unwrap();
        p.with_stand_in(vike_strategy_lab::stand_in).engine.equity_sampling()
    };
    assert_eq!(sampling(""), EquitySampling::ByTime(60_000));
    assert_eq!(sampling("seed_bar_interval_ms = 300000"), EquitySampling::ByTime(300_000));
    assert_eq!(sampling("equity_sample_every = 1"), EquitySampling::EveryTick);
    assert_eq!(sampling("equity_sample_every = 0"), EquitySampling::Off);
    let thinned = "equity_sample_every = 7\nseed_bar_interval_ms = 300000";
    assert_eq!(sampling(thinned), EquitySampling::EveryN(7));
}

#[test]
fn range_accepts_bare_epoch_ms() {
    let p = ProfileBacktest::from_toml_str(BARE_MS_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    let r = p.range().unwrap();
    assert_eq!(r.start, Some(1_700_000_000_000));
    assert_eq!(r.end, Some(1_700_003_600_000));
}

/// A plain `YYYY-MM-DD` (`backtest run --from 2025-06-01`) is that day's midnight UTC, the same instant as its `T00`
/// hour label; a day the month does not have is refused (already at the load), naming the three spellings.
#[test]
fn range_accepts_a_plain_date_as_midnight_utc() {
    let range = |from: &str, to: &str| {
        let toml = BARE_MS_TOML
            .replace("\"1700000000000\"", &format!("\"{from}\""))
            .replace("\"1700003600000\"", &format!("\"{to}\""));
        ProfileBacktest::from_toml_str(&toml).and_then(|p| p.range())
    };
    // 2025-06-01T00:00 and 2025-06-02T00:00 UTC (verified against `date -u -d`).
    let r = range("2025-06-01", "2025-06-02").unwrap();
    assert_eq!(r.start, Some(1_748_736_000_000));
    assert_eq!(r.end, Some(1_748_822_400_000));
    assert_eq!(range("2025-06-01T00", "2025-06-02T00").unwrap(), r);
    let err = range("2025-02-31", "2025-06-02").unwrap_err().to_string();
    assert!(err.contains("YYYY-MM-DD or YYYY-MM-DDTHH"), "{err}");
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
file = "trend/buy_hold.rs"
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
file = "trend/buy_hold.rs"
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
file = "trend/buy_hold.rs"
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
file = "trend/buy_hold.rs"
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
file = "trend/buy_hold.rs"
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
file = "trend/buy_hold.rs"
"#;
    refused_at_load(toml, HarnessError::Parse, &[]);
}

// --- the one slippage table (`vike_model::ProfileEngineSlippage`, the owner's ruling of 2026-11-10) ---
// A profile states none since the owner's ruling of 2026-11-18 (a run's slippage is a strategy param,
// `crate::run::costs`); the table is what a strategy states and what the paper row is, so these hold it
// alone.

/// The table of `body`, judged by its own check.
fn table_of(body: &str) -> Result<vike_model::ProfileEngineSlippage, String> {
    let table: vike_model::ProfileEngineSlippage =
        toml::from_str(body).map_err(|e| e.to_string())?;
    table.check()?;
    Ok(table)
}

/// The engine fields a `body` resolves to, through the one resolver both sites call.
fn terms_of(body: &str) -> vike_fills::market_impact::ProfileEngineSlippageTerms {
    let table = table_of(body).expect("the table must load");
    vike_fills::market_impact::slippage_terms(Some(&table)).expect("the table resolves")
}

/// No table is no slippage: the engine's three fields at `StrategyEngineConfig::default()`'s, bit for bit,
/// which is what every profile that never named slippage ran.
#[test]
fn slippage_is_none_unless_configured() {
    let t = vike_fills::market_impact::slippage_terms(None).unwrap();
    let d = vike_simulator::StrategyEngineConfig::default();
    assert_eq!(t.fraction.to_bits(), d.slippage.to_bits());
    assert_eq!(t.amount.to_bits(), d.slippage_amount.to_bits());
    assert!(t.impact.is_none());
}

/// Each model fills its own engine field and leaves the other two at none; `none` fills nothing.
#[test]
fn each_slippage_model_reaches_its_one_engine_field() {
    let none = terms_of("model = \"none\"");
    assert_eq!((none.fraction, none.amount, none.impact.is_none()), (0.0, 0.0, true));

    let percent = terms_of("model = \"percent\"\nvalue = 0.0005");
    assert_eq!(percent.fraction.to_bits(), 0.0005f64.to_bits(), "the old flat fraction's bits");
    assert_eq!((percent.amount, percent.impact.is_none()), (0.0, true));

    // `ticks * tick`, in the lab's order (`Terms::slip_ticks * Terms::tick`), from a TOML integer.
    let ticks = terms_of("model = \"ticks\"\nticks = 2\ntick = 0.01");
    assert_eq!(ticks.amount.to_bits(), (2.0f64 * 0.01).to_bits());
    assert_eq!((ticks.fraction, ticks.impact.is_none()), (0.0, true));

    let impact = terms_of("model = \"market_impact\"\nbase = 0.001\nexec_time = 0.5\nwindow = 60");
    assert_eq!(impact.fraction.to_bits(), 0.001f64.to_bits(), "base is the old flat fraction");
    assert_eq!(impact.amount, 0.0);
    let (_, window) = impact.impact.expect("the impact model is armed");
    assert_eq!(window, 60);
    let bare = terms_of("model = \"market_impact\"");
    assert_eq!(bare.fraction, 0.0, "no base, no constant part");
    assert_eq!(bare.impact.expect("armed").1, vike_fills::market_impact::DEFAULT_IMPACT_WINDOW);
}

/// A key of another model is refused in words: one table never prices a fill two ways.
#[test]
fn two_slippage_models_at_once_are_refused() {
    for (body, needle) in [
        (
            "model = \"percent\"\nvalue = 0.001\nticks = 2",
            "slippage key ticks belongs to the ticks model",
        ),
        (
            "model = \"ticks\"\nticks = 2\ntick = 0.01\nvalue = 0.001",
            "slippage key value belongs to the percent model",
        ),
        (
            "model = \"percent\"\nvalue = 0.001\nwindow = 60",
            "slippage key window belongs to the market_impact model",
        ),
        ("model = \"market_impact\"\ntick = 0.01", "slippage key tick belongs to the ticks model"),
        ("model = \"none\"\nvalue = 0.001", "slippage key value belongs to the percent model"),
    ] {
        let err = table_of(body).expect_err("two models are refused");
        assert!(err.contains(needle) && err.contains("one model at a time"), "{body}: {err}");
    }
}

/// A profile's `[engine.slippage]`, its old scalar and the old `[engine.impact]` table are REFUSED, never
/// read, each naming the strategy param that replaced them (no migration: the owner's rule).
#[test]
fn a_profile_slippage_in_any_spelling_is_refused_naming_the_param() {
    for engine in [
        "# engine\n\n[engine.slippage]\nmodel = \"percent\"\nvalue = 0.0005",
        "# engine\nslippage = 0.0005",
        "# engine\n\n[engine.impact]\nmodel = \"almgren_chriss\"\nwindow = 60",
        "# engine\nslippage = 0.0002\n\n[engine.impact]\nmodel = \"almgren_chriss\"",
    ] {
        refused_at_load(
            &BAR_TOML.replace("# engine", engine),
            HarnessError::Validation,
            &["is no longer read", "slippage is a strategy parameter now", "slippage_unit"],
        );
    }
}

/// An unknown model, a model missing its key and a number that cannot be a price move are refused
/// by name: a typo must never price fills at no slippage.
#[test]
fn a_bad_slippage_table_is_refused_by_name() {
    for (body, needle) in [
        ("model = \"almgren_chriss\"", "unknown slippage model"),
        ("model = \"percent\"", "needs value"),
        ("model = \"ticks\"\nticks = 2", "needs tick"),
        ("model = \"ticks\"\ntick = 0.01", "needs ticks"),
        ("model = \"percent\"\nvalue = -0.001", "slippage value must be a number >= 0"),
        ("model = \"ticks\"\nticks = 2\ntick = 0.0", "slippage tick must be a price > 0"),
        ("model = \"market_impact\"\nexec_time = 0.0", "slippage exec_time must be > 0"),
        ("model = \"market_impact\"\nwindow = 2", "slippage window must be >= 3"),
        ("model = \"market_impact\"\ngamma = 0.0", "slippage gamma must be > 0"),
        ("model = \"market_impact\"\neta = -1e9", "slippage eta must be > 0"),
    ] {
        let err = table_of(body).expect_err("a bad table is refused");
        assert!(err.contains(needle), "{body}: {err}");
    }
}

/// The coefficient knobs default to the PUBLISHED constants and are settable, asserted end to end
/// (load -> the resolver -> a MOVED number): a field that parsed and was dropped before
/// `ImpactModelAlmgrenChriss` would pass a field-equality test.
#[test]
fn the_impact_coefficients_default_to_the_published_pair_and_are_settable() {
    use vike_fills::market_impact::{ImpactInputs, ImpactTerms};
    let ctx = ImpactInputs { qty: 3_000.0, average_volume: 80_000.0, sigma: 0.02 };
    let charge = |extra: &str| {
        let (model, _) = terms_of(&format!("model = \"market_impact\"{extra}")).impact.unwrap();
        model.impact_fraction_for(&ctx, ImpactTerms::PermanentOnly)
    };
    let published = charge("");
    let explicit = charge(&format!(
        "\ngamma = {:?}\neta = {:?}",
        vike_fills::market_impact::AC_GAMMA,
        vike_fills::market_impact::AC_ETA
    ));
    assert_eq!(published.to_bits(), explicit.to_bits(), "absent is the paper's pair");
    assert!(published > 0.0, "the fixture must produce a real charge");
    // The L2 lane charges PermanentOnly: halving gamma must halve the charge.
    let halved = charge("\ngamma = 0.157");
    assert!((halved - published / 2.0).abs() < 1e-15, "gamma did not reach the model: {halved}");
}

/// `[engine.margin_call]` IS the engine's `ShortfallCover`, field for field, and is refused on the
/// tick lane, beside the `maint_margin` liquidation it replaces, and with a number it cannot be.
#[test]
fn the_margin_call_table_reaches_the_engine_and_is_refused_where_it_cannot_run() {
    let table = "\n\n[engine.margin_call]\ncover = 4.0\nmargin_long = 0.2\nmargin_short = 0.25\nstep = 0.001";
    let p =
        ProfileBacktest::from_toml_str(&BAR_TOML.replace("# engine", &format!("# engine{table}")))
            .expect("a bar profile with a margin call loads")
            .with_stand_in(vike_strategy_lab::stand_in);
    let call = p.engine.margin_call.expect("configured");
    assert_eq!(
        call,
        vike_simulator::ShortfallCover {
            cover: 4.0,
            margin_long: 0.2,
            margin_short: 0.25,
            step: 0.001
        }
    );
    let tick = TICK_TOML
        .replace("snap_to_properties = true", &format!("snap_to_properties = true{table}"));
    refused_at_load(&tick, HarnessError::Validation, &["engine.margin_call is bar-mode only"]);
    let both = BAR_TOML.replace("# engine", &format!("# engine\nmaint_margin = 0.05{table}"));
    refused_at_load(
        &both,
        HarnessError::Validation,
        &["replaces the engine.maint_margin liquidation"],
    );
    let bad = BAR_TOML.replace(
        "# engine",
        "# engine\n\n[engine.margin_call]\ncover = 0.0\nmargin_long = 0.2\nmargin_short = 0.2",
    );
    refused_at_load(&bad, HarnessError::Validation, &["engine.margin_call.cover must be > 0"]);
}

/// `engine.whole_entry_fee` loads with the closed-trade block on (its default) and is refused with
/// the block off, where it would book nothing.
#[test]
fn whole_entry_fee_needs_the_closed_trade_block() {
    let on = BAR_TOML.replace("# engine", "# engine\nwhole_entry_fee = true");
    assert!(
        ProfileBacktest::from_toml_str(&on)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in)
            .engine
            .whole_entry_fee
    );
    let off =
        BAR_TOML.replace("# engine", "# engine\nwhole_entry_fee = true\nclosed_trades = false");
    refused_at_load(&off, HarnessError::Validation, &["engine.whole_entry_fee"]);
}

/// A bar series carries no `local_timestamp`, so feed-latency delivery has nothing to order by — the
/// combination is rejected rather than silently ignored (the mirror of the bar-only `attach_funding`
/// rule above).
#[test]
fn feed_latency_on_the_bar_lane_is_rejected() {
    let toml = BAR_TOML.replace("# engine", "# engine\nfeed_latency = true");
    assert!(toml.contains("feed_latency = true"), "the BAR_TOML anchor must still match");
    refused_at_load(&toml, HarnessError::Validation, &["tick-mode only"]);
}

/// Absent = OFF (today's venue-ordered replay); accepted on the tick lane.
#[test]
fn feed_latency_defaults_off_and_is_accepted_on_the_tick_lane() {
    let off = ProfileBacktest::from_toml_str(TICK_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert!(!off.engine.feed_latency, "absent must mean the frozen venue-ordered replay");

    let toml = TICK_TOML
        .replace("snap_to_properties = true", "snap_to_properties = true\nfeed_latency = true");
    let on =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert!(on.engine.feed_latency);
}

// --- the opt-in market-funding join (`engine.attach_funding`) -----------------------------

/// Absent `[engine].attach_funding` means OFF for every product (the owner's ruling of 2026-11-21);
/// `true` and `false` say it for every series. [`crate::run::names_a_perpetual`], which a report's funding
/// words read, still tells a perpetual's name from any other.
#[test]
fn attach_funding_defaults_off_and_parses_both_words() {
    let p = ProfileBacktest::from_toml_str(BAR_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.attach_funding, None);
    assert!(!p.engine.attaches_funding(), "absent is off, a perpetual's included");
    for (venue, symbol) in [
        ("binance", "BTCUSDT.P"),
        ("bybit", "BTCUSD.P"),
        ("aster", "ETHUSDT.P"),
        ("okx", "BTCUSDT.P"),
        ("deribit", "BTCUSD.P"),
        ("hyperliquid", "BTCUSDC.P"),
        ("hyperliquid", "XYZ:TSLAUSDC.P"),
    ] {
        assert!(crate::run::names_a_perpetual(venue, symbol), "{venue}:{symbol} is a perpetual");
    }
    for (venue, symbol) in [
        ("binance", "BTCUSDT"),
        ("okx", "BTCUSDT"),
        ("deribit", "BTCUSD_261127"),
        ("hyperliquid", "HYPEUSDC"),
        ("hyperliquid", "HYPE/USDC"),
        ("hyperliquid", "@107"),
        ("oanda", "EUR_USD"),
    ] {
        assert!(!crate::run::names_a_perpetual(venue, symbol), "{venue}:{symbol} is not");
    }

    let on = BAR_TOML.replace("# engine", "# engine\nattach_funding = true");
    let p = ProfileBacktest::from_toml_str(&on).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.engine.attaches_funding(), "`true` reads it, for a spot name too");
    let off = BAR_TOML.replace("# engine", "# engine\nattach_funding = false");
    let p =
        ProfileBacktest::from_toml_str(&off).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert!(!p.engine.attaches_funding(), "`false` is off");
}

/// The tick lane charges the funding schedule too (`vike_simulator::StrategyEngineConfig::funding_schedule` on
/// `run_ticks`), so the key loads there; it was refused as bar-mode only until 2026-11-08.
#[test]
fn attach_funding_loads_on_the_tick_lane() {
    let toml = TICK_TOML
        .replace("snap_to_properties = true", "snap_to_properties = true\nattach_funding = true");
    assert!(toml.contains("attach_funding = true"), "the TICK_TOML anchor must still match");
    let p = ProfileBacktest::from_toml_str(&toml)
        .expect("the tick lane takes attach_funding")
        .with_stand_in(vike_strategy_lab::stand_in);
    assert_eq!(p.engine.attach_funding, Some(true));
}

/// The OLD single-venue form still loads, unchanged — the whole back-compat claim.
#[test]
fn the_single_venue_form_still_loads_and_expands_to_series() {
    for toml in [BAR_TOML, TICK_TOML, BARE_MS_TOML] {
        let p = ProfileBacktest::from_toml_str(toml)
            .unwrap()
            .with_stand_in(vike_strategy_lab::stand_in);
        assert!(p.data.series.is_empty(), "no [[data.series]] table was written");
        let series = p.data.resolved_series();
        assert_eq!(series.len(), p.data.symbols.len());
        for (s, symbol) in series.iter().zip(&p.data.symbols) {
            assert_eq!(&s.venue, p.data.venue.as_ref().unwrap());
            assert_eq!(&s.symbol, symbol);
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
    let p = ProfileBacktest::from_toml_str(CROSS_VENUE_TOML)
        .unwrap()
        .with_stand_in(vike_strategy_lab::stand_in);
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
    refused_at_load(&toml, HarnessError::Validation, &["duplicate symbol", "account = "]);
}

/// ONE user name on two venues (decision 0293): the series of the other venue names the account of the
/// strategy's leg there, and is that leg's slot, `SYMBOL@account`; an account of another venue is refused.
#[test]
fn one_symbol_on_two_venues_loads_once_the_other_leg_names_its_account() {
    let named = CROSS_VENUE_TOML.replace(
        "symbol = \"btc-updown-5m-1775001600#1\"",
        "symbol = \"BTCUSDT\"\naccount = \"polymarket/DEMO\"",
    );
    let p = ProfileBacktest::from_toml_str(&named).expect("two slots: BTCUSDT and its leg");
    let slots: Vec<String> = p.data.resolved_series().iter().map(|s| s.leg_name()).collect();
    assert_eq!(slots[0], "BTCUSDT");
    assert_eq!(slots[2], "BTCUSDT@polymarket/DEMO");
    let foreign = named.replace("polymarket/DEMO", "bybit/DEMO");
    refused_at_load(
        &foreign,
        HarnessError::Validation,
        &["must be an account of the series' own venue"],
    );
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
file = "trend/buy_hold.rs"
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

/// [`CROSS_VENUE_TOML`] (own venue `spot`) with `heard` written before its `[engine]` table and
/// `data.feed_status` on.
fn heard_toml(heard: &str) -> String {
    let toml = CROSS_VENUE_TOML
        .replace("[data]\nkind = \"tick\"", "[data]\nkind = \"tick\"\nfeed_status = true")
        .replace("[engine]\ncash", &format!("{heard}\n[engine]\ncash"));
    assert!(toml.contains("feed_status = true") && toml.contains(heard), "the anchors must match");
    toml
}

/// Book W34: another venue's touch (`[[data.reference]]`, of a name the slice also holds on its own
/// venue, or a traded leg of its own name), an underlying's mark (`[[data.mark]]`) and the feed's
/// status load on the tick lane; only the traded reference is a symbol slot of the run.
#[test]
fn heard_series_load_and_only_a_traded_reference_is_a_slot() {
    let toml = heard_toml(
        "[[data.reference]]\nvenue = \"binance\"\nsymbol = \"BTCUSDT\"\nkind = \"quote\"\ndelay_ms = 40\n\
         [[data.reference]]\nvenue = \"binance\"\nsymbol = \"BTCUSDC\"\ntrade = true\n\
         [[data.mark]]\nvenue = \"binance\"\nsymbol = \"ETHUSDT\"\ndelay_ms = 0\n",
    );
    let p =
        ProfileBacktest::from_toml_str(&toml).unwrap().with_stand_in(vike_strategy_lab::stand_in);
    assert!(p.data.feed_status);
    assert_eq!(p.data.reference.len(), 2);
    assert_eq!(p.data.mark.len(), 1);
    let delays =
        (p.data.reference[0].delay_ms, p.data.reference[1].delay_ms, p.data.mark[0].delay_ms);
    assert_eq!(delays, (Some(40), None, Some(0)), "as stated; the replay judges an absent one");
    let series = p.data.resolved_series();
    assert_eq!(series.len(), 4, "the three series, then the traded reference");
    assert_eq!((series[3].venue.as_str(), series[3].symbol.as_str()), ("binance", "BTCUSDC"));
    assert_eq!(p.data.default_venue(), "spot", "a reference never moves the run's own venue");
}

#[test]
fn a_reference_the_run_cannot_hear_as_live_does_is_rejected() {
    let reference = |venue: &str, symbol: &str, extra: &str| {
        format!("[[data.reference]]\nvenue = \"{venue}\"\nsymbol = \"{symbol}\"\n{extra}")
    };
    let mark = "[[data.mark]]\nvenue = \"binance\"\nsymbol = \"ETHUSDT\"\ndelay_ms = 1.5\n";
    let cases = [
        (reference("spot", "ETHUSDT", ""), "the run's own venue"),
        (reference("binance", "BTCUSDT", "kind = \"trade\"\n"), "hears nothing"),
        (reference("binance", "BTCUSDT", "").repeat(2), "listed twice"),
        // traded, it is a slot, and the slice already holds the name on `spot`
        (reference("binance", "BTCUSDT", "trade = true\n"), "duplicate symbol"),
        // a stated hop to this box is a whole number of milliseconds, never negative
        (reference("binance", "BTCUSDT", "delay_ms = -5\n"), "\"BTCUSDT\" states delay_ms = -5"),
        (mark.to_string(), "data.mark \"ETHUSDT\" states delay_ms = 1.5"),
    ];
    for (heard, needle) in cases {
        refused_at_load(&heard_toml(&heard), HarnessError::Validation, &[needle]);
    }
    let bar = BAR_TOML.replace("kind = \"bar\"", "kind = \"bar\"\nfeed_status = true");
    refused_at_load(&bar, HarnessError::Validation, &["tick-mode only"]);
}
