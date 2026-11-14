use super::*;
use vike_data::StoreKindId;
use vike_model::run_request::gap::UniverseMode;

fn cov(first_ts: i64, last_timestamp_value: i64, rows: u64) -> SeriesCoverage {
    SeriesCoverage {
        first_timestamp: first_ts,
        last_timestamp: last_timestamp_value,
        rows,
        bytes: 1,
        parts: 1,
        dates: 1,
    }
}

fn bar(symbol: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, "binance", symbol, Some("1h".to_string()))
}

fn planned(
    id: SeriesId,
    coverage: Option<SeriesCoverage>,
    from: Option<i64>,
    to: Option<i64>,
    gaps: &[(i64, i64)],
) -> DataPlanSeries {
    let recorded =
        coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_timestamp, c.last_timestamp));
    DataPlanSeries {
        missing: window_shortfall(from, to, recorded, gaps),
        id,
        role: DataPlanSeriesRole::Price,
        coverage,
        input: None,
        conversion: None,
    }
}

fn plan(series: Vec<DataPlanSeries>, enumerable: bool) -> DataPlan {
    DataPlan {
        schema: PLAN_SCHEMA,
        store: "/tmp/hist".to_string(),
        store_rung: None,
        store_rung_why: None,
        from_ms: Some(0),
        to_ms: Some(1_000),
        kind: "bar".to_string(),
        interval: "1h".to_string(),
        enumerable,
        gaps_unknown: false,
        series,
        notes: Vec::new(),
    }
}

const ARMED: CoverageGate = CoverageGate { armed: true, max_gap_ms: None, on_gap: OnGap::Refuse };

#[test]
fn an_unarmed_gate_finds_nothing_even_over_a_wholly_absent_slice() {
    let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], true);
    let gate = CoverageGate { armed: false, max_gap_ms: None, on_gap: OnGap::Refuse };
    assert!(coverage_verdict(&p, &gate).is_clean());
}

#[test]
fn a_covered_window_is_clean() {
    let p = plan(
        vec![planned(bar("BTCUSDT"), Some(cov(0, 1_000, 10)), Some(0), Some(1_000), &[])],
        true,
    );
    assert!(coverage_verdict(&p, &ARMED).is_clean());
}

/// The case `find_gaps` is blind to: a contiguous tape that simply starts late.
#[test]
fn a_late_starting_tape_is_a_finding_with_no_interior_hole_anywhere() {
    let p = plan(
        vec![planned(bar("BTCUSDT"), Some(cov(400, 1_000, 10)), Some(0), Some(1_000), &[])],
        true,
    );
    let v = coverage_verdict(&p, &ARMED);
    assert_eq!(v.findings.len(), 1);
    assert_eq!(v.findings[0].spans.len(), 1);
    assert_eq!(v.findings[0].spans[0].kind, SeriesShortfall::Leading);
    assert!(!v.findings[0].absent);
}

#[test]
fn max_gap_tolerates_a_small_span_and_not_a_large_one() {
    let small = plan(
        vec![planned(bar("BTCUSDT"), Some(cov(100, 1_000, 10)), Some(0), Some(1_000), &[])],
        true,
    );
    let tolerant = CoverageGate { armed: true, max_gap_ms: Some(500), on_gap: OnGap::Refuse };
    assert!(coverage_verdict(&small, &tolerant).is_clean(), "a 100ms span under a 500ms tolerance");
    let strict = CoverageGate { armed: true, max_gap_ms: Some(50), on_gap: OnGap::Refuse };
    assert_eq!(coverage_verdict(&small, &strict).findings.len(), 1);
}

/// The rule that keeps `max_gap` from disarming the gate for the case it most exists to catch.
#[test]
fn an_absent_series_is_never_tolerated_however_generous_max_gap_is() {
    let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], true);
    let generous = CoverageGate { armed: true, max_gap_ms: Some(i64::MAX), on_gap: OnGap::Refuse };
    let v = coverage_verdict(&p, &generous);
    assert_eq!(v.findings.len(), 1);
    assert!(v.findings[0].absent);
    assert_eq!(v.findings[0].spans[0].kind, SeriesShortfall::Everything);
}

/// An INVENTORIED series with zero rows is not a 1970 tape.
#[test]
fn a_present_but_empty_series_reads_as_wholly_missing() {
    let p =
        plan(vec![planned(bar("BTCUSDT"), Some(cov(0, 0, 0)), Some(0), Some(1_000), &[])], true);
    let v = coverage_verdict(&p, &ARMED);
    assert_eq!(v.findings[0].spans[0].kind, SeriesShortfall::Everything);
    assert!(v.findings[0].absent, "zero rows is not held, whatever the inventory listed");
}

/// "I found holes" and "I could not look" must not be the same answer.
#[test]
fn an_unenumerable_store_is_unprovable_rather_than_clean() {
    let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], false);
    let v = coverage_verdict(&p, &ARMED);
    assert!(v.unprovable);
    assert!(!v.is_clean());
    assert!(v.lines().iter().any(|l| l.contains("UNPROVABLE")));
}

/// Holds one series that covers `gated`'s whole window. `gaps_read: false` is the shape a dropped
/// connection or a manifest that stopped parsing leaves between the two calls: the inventory
/// answered and the holes did not.
struct OneSeries {
    gaps_read: bool,
}

impl vike_data::HistoryStore for OneSeries {
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, vike_data::HistoryStoreError> {
        Ok(vec![(bar("BTCUSDT"), cov(0, 1_000, 10))])
    }
    fn series_gaps(&self, _id: &SeriesId) -> Result<Vec<(i64, i64)>, vike_data::HistoryStoreError> {
        if self.gaps_read {
            return Ok(Vec::new());
        }
        Err(vike_data::HistoryStoreError::Query("the manifest would not parse".to_string()))
    }
    vike_data::hist_store_stubs!(inert: all);
}

/// A profile whose window is exactly what [`OneSeries`] holds, with the gate armed.
fn gated(data_extra: &str) -> crate::harness::ProfileBacktest {
    crate::harness::ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "0"
to = "1000"
require_coverage = true
{data_extra}

[engine]
cash = 1000.0

[strategy]
file = "trend/buy_hold.rs"
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategies_check::stand_in)
}

/// The hole this gate had: the holes could not be read, the note was read by nobody, and the armed
/// gate passed a series it had only checked at both ends.
#[test]
fn a_series_whose_holes_would_not_read_refuses_an_armed_gate() {
    let store = OneSeries { gaps_read: false };
    let why = enforce(&gated(""), &store, "test store").expect_err("nothing proved it").to_string();
    assert!(why.contains("UNPROVABLE"), "says it could not look: {why}");
    assert!(why.contains("the manifest would not parse"), "in the store's own words: {why}");
    assert!(why.contains("BTCUSDT"), "and names the series: {why}");
}

#[test]
fn a_warning_gate_says_the_holes_would_not_read_and_runs() {
    let store = OneSeries { gaps_read: false };
    let lines = enforce(&gated("on_gap = \"warn\""), &store, "test store").expect("warn runs");
    assert!(lines.iter().any(|l| l.contains("UNPROVABLE")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("the manifest would not parse")), "{lines:?}");
}

/// A store whose inventory fails, and a profile with no coverage gate armed: the universe rule alone
/// reaches the store's error.
struct WillNotList;

impl vike_data::HistoryStore for WillNotList {
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, vike_data::HistoryStoreError> {
        Err(vike_data::HistoryStoreError::Query("the datahub went away".to_string()))
    }
    vike_data::hist_store_stubs!(inert: all);
}

fn with_universe(mode: &str, extra: &str) -> crate::harness::ProfileBacktest {
    crate::harness::ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "0"
to = "1000"
universe = "{mode}"
{extra}

[engine]
cash = 1000.0

[strategy]
file = "trend/buy_hold.rs"
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategies_check::stand_in)
}

/// An inventory that failed read as "the store holds no bar series for it at all", with the store's
/// error dropped: a run went on with a survivorship finding nobody measured.
#[test]
fn a_covered_universe_over_a_store_that_would_not_list_says_so_and_not_that_a_member_is_gone() {
    let lines =
        enforce(&with_universe("covered", ""), &WillNotList, "test store").expect("it runs");
    assert!(lines.iter().any(|l| l.contains("the datahub went away")), "the error: {lines:?}");
    assert!(!lines.iter().any(|l| l.contains("holds no bar series")), "no finding: {lines:?}");
    assert!(!lines.iter().any(|l| l.contains("SURVIVORS")), "no survivorship line: {lines:?}");
}

/// ...and a strict refusal told the operator to move `data.from`, which cures nothing.
#[test]
fn a_strict_universe_over_a_store_that_would_not_list_refuses_on_the_error() {
    let why = enforce(&with_universe("strict", ""), &WillNotList, "test store")
        .expect_err("nothing was proved")
        .to_string();
    assert!(why.contains("the datahub went away"), "the error: {why}");
    assert!(!why.contains("holds no bar series"), "no finding: {why}");
    assert!(!why.contains("Move data.from"), "no advice for a problem it did not find: {why}");
}

/// A warning gate says the same notes when both rules are armed, and they reach the report once.
#[test]
fn a_warning_gate_and_a_covered_universe_say_the_stores_error_once() {
    let profile = with_universe("covered", "require_coverage = true\non_gap = \"warn\"");
    let lines = enforce(&profile, &WillNotList, "test store").expect("warn runs");
    let told = lines.iter().filter(|l| l.contains("the datahub went away")).count();
    assert_eq!(told, 1, "{lines:?}");
}

/// A store that cannot answer whether it holds a grid refuses a snapping run, as a failed lookup inside
/// the run does: nothing can be said about what the fills will skip.
#[test]
fn a_grid_that_would_not_read_refuses_a_snap_run() {
    struct GridWontRead;
    impl vike_data::HistoryStore for GridWontRead {
        fn properties_as_of(
            &self,
            _venue: &str,
            _symbol: &str,
            _ts: i64,
        ) -> Result<Option<vike_model::SymbolProperties>, vike_data::HistoryStoreError> {
            Err(vike_data::HistoryStoreError::Query("the part would not decode".to_string()))
        }
        vike_data::hist_store_stubs!(inert: all);
    }
    let mut profile = gated("");
    profile.engine.snap_to_properties = true;
    let why = enforce(&profile, &GridWontRead, "test store").expect_err("nothing to say");
    assert!(why.to_string().contains("BTCUSDT"), "{why}");
    assert!(why.to_string().contains("the part would not decode"), "{why}");
}

/// The plan reports the same thing the gate refuses on, and its closing line must not claim a
/// coverage nobody could check.
#[test]
fn a_plan_whose_holes_would_not_read_does_not_claim_the_window_is_covered() {
    let p =
        plan_data(&gated(""), &[], &OneSeries { gaps_read: false }, "test store", None).unwrap();
    assert_eq!(p.to_json()["gaps_unknown"], serde_json::json!(true));
    assert!(!p.lines().iter().any(|l| l.contains("covers every planned series")), "{p:?}");
    let doc = explain_document(&gated(""), &p).unwrap();
    assert_eq!(doc["coverage"]["unprovable"], serde_json::json!(true), "{doc}");
}

/// The other half: a store that answers both questions moves nothing, down to the document's keys.
#[test]
fn a_store_that_answers_both_questions_passes_and_adds_no_key_to_the_plan() {
    let store = OneSeries { gaps_read: true };
    assert_eq!(enforce(&gated(""), &store, "test store").unwrap(), Vec::<String>::new());
    let p = plan_data(&gated(""), &[], &store, "test store", None).unwrap();
    assert!(p.to_json().get("gaps_unknown").is_none(), "{}", p.to_json());
    assert!(p.lines().iter().any(|l| l.contains("covers every planned series")), "{p:?}");
}

#[test]
fn the_refusal_offers_a_fetch_command_for_a_bar_series_and_none_for_a_group() {
    let id = bar("BTCUSDT");
    let span =
        SeriesMissingSpan { start_ms: 0, end_ms: 86_400_000, kind: SeriesShortfall::Leading };
    let hint = fetch_hint(&id, Some(&span)).expect("a per-symbol bar series can be fetched");
    assert!(hint.starts_with("vike-cli data hist fetch binance:BTCUSDT:1h --from "));
    let grouped = SeriesId::grouped(StoreKindId::Trade, "polymarket", "g1");
    assert!(
        fetch_hint(&grouped, Some(&span)).is_none(),
        "`data fetch` cannot write a grouped series, so suggesting it would be worse than \
             saying nothing"
    );
}

#[test]
fn no_fetch_command_is_offered_for_an_unbounded_span() {
    let span = SeriesMissingSpan {
        start_ms: i64::MIN,
        end_ms: i64::MAX,
        kind: SeriesShortfall::Everything,
    };
    assert!(fetch_hint(&bar("BTCUSDT"), Some(&span)).is_none());
}

// ---- the universe rule -----------------------------------------------------------------------

#[test]
fn declared_mode_consults_nothing_and_names_nobody() {
    let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], true);
    let v = universe_verdict(&p, UniverseMode::Declared);
    assert!(v.is_clean());
    assert!(v.present.is_empty(), "declared mode does not even classify");
}

/// The survivorship signature: a member whose tape begins after the window does.
#[test]
fn a_member_listed_after_the_window_opened_is_named() {
    let p = plan(
        vec![
            planned(bar("BTCUSDT"), Some(cov(0, 1_000, 10)), Some(0), Some(1_000), &[]),
            planned(bar("NEWCOIN"), Some(cov(600, 1_000, 5)), Some(0), Some(1_000), &[]),
        ],
        true,
    );
    let v = universe_verdict(&p, UniverseMode::Covered);
    assert_eq!(v.present, vec!["BTCUSDT".to_string()]);
    assert_eq!(v.absent.len(), 1);
    assert_eq!(v.absent[0].0, "NEWCOIN");
    assert!(v.absent[0].1.contains("does not reach back"));
}

/// An INTERIOR hole is a recorder outage, not a listing date — the split that keeps this
/// warning worth reading.
#[test]
fn an_interior_hole_is_not_a_universe_finding() {
    let p = plan(
        vec![planned(bar("BTCUSDT"), Some(cov(0, 1_000, 10)), Some(0), Some(1_000), &[(400, 500)])],
        true,
    );
    assert!(universe_verdict(&p, UniverseMode::Strict).is_clean());
    // ...while the COVERAGE gate does find it, which is the whole reason they are two rules.
    assert_eq!(coverage_verdict(&p, &ARMED).findings.len(), 1);
}

#[test]
fn a_grouped_series_answers_no_question_about_one_member() {
    // A PRICE series (the role `planned` gives) that is nonetheless grouped: its coverage is
    // the whole group's, so it cannot answer for one member and must not be blamed for one.
    let s = planned(
        SeriesId::grouped(StoreKindId::Trade, "polymarket", "g1"),
        None,
        Some(0),
        Some(1_000),
        &[],
    );
    let p = plan(vec![s], true);
    let v = universe_verdict(&p, UniverseMode::Strict);
    assert!(v.is_clean());
    assert!(v.present.is_empty(), "an empty `present` is not a clean bill of health");
}

#[test]
fn a_non_price_series_is_not_a_universe_finding() {
    let mut s = planned(
        SeriesId::per_symbol(StoreKindId::Properties, "binance", "BTCUSDT", None),
        None,
        Some(0),
        Some(1_000),
        &[],
    );
    s.role = DataPlanSeriesRole::Properties;
    let p = plan(vec![s], true);
    assert!(universe_verdict(&p, UniverseMode::Strict).is_clean());
}

// ---- rendering -------------------------------------------------------------------------------

#[test]
fn an_unbounded_bound_renders_as_infinity_rather_than_a_calendar_year() {
    assert_eq!(render_window(None, None), "-inf .. +inf");
    assert_eq!(render_window(Some(i64::MIN), Some(i64::MAX)), "-inf .. +inf");
}

#[test]
fn durations_render_as_the_largest_whole_unit() {
    assert_eq!(human_ms(0), "0ms");
    assert_eq!(human_ms(999), "999ms");
    assert_eq!(human_ms(1_000), "1s");
    assert_eq!(human_ms(60_000), "1m");
    assert_eq!(human_ms(3_600_000), "1h");
    assert_eq!(human_ms(86_400_000), "1d");
    assert_eq!(human_ms(4 * 86_400_000), "4d");
}

#[test]
fn the_plan_leads_with_the_store_and_names_every_series() {
    let p = plan(
        vec![
            planned(bar("BTCUSDT"), Some(cov(400, 1_000, 10)), Some(0), Some(1_000), &[]),
            planned(bar("ETHUSDT"), None, Some(0), Some(1_000), &[]),
        ],
        true,
    );
    let lines = p.lines();
    assert!(lines[0].starts_with("store: /tmp/hist"), "the store LEADS: {:?}", lines[0]);
    assert!(lines.iter().any(|l| l.contains("symbol=BTCUSDT")));
    assert!(lines.iter().any(|l| l.contains("symbol=ETHUSDT") && l.contains("NOT HELD")));
    assert!(lines.iter().any(|l| l.contains("MISSING leading")));
    assert!(lines.iter().any(|l| l.contains("MISSING everything")));
    assert_eq!(p.rows(), 10);
    assert_eq!(p.held(), 1);
}

/// The document must carry its own sentences: on the remote route nothing else can render it.
#[test]
fn the_json_document_carries_the_rendered_lines_and_the_folded_totals() {
    let p = plan(
        vec![planned(bar("BTCUSDT"), Some(cov(400, 1_000, 10)), Some(0), Some(1_000), &[])],
        true,
    );
    let doc = p.to_json();
    assert_eq!(doc["schema"], serde_json::json!(PLAN_SCHEMA));
    assert_eq!(doc["rows"], serde_json::json!(10u64));
    assert_eq!(doc["held"], serde_json::json!(1usize));
    assert_eq!(doc["planned"], serde_json::json!(1usize));
    let explain = doc["explain"].as_array().expect("the lines ride inside the document");
    assert!(explain.iter().any(|l| l.as_str().is_some_and(|s| s.starts_with("store: "))));
}

#[test]
fn an_empty_and_a_missing_series_render_as_different_sentences() {
    let empty = planned(bar("A"), Some(cov(0, 0, 0)), Some(0), Some(1_000), &[]);
    let absent = planned(bar("B"), None, Some(0), Some(1_000), &[]);
    assert!(render_series_line(&empty).contains("PRESENT AND EMPTY"));
    assert!(render_series_line(&absent).contains("NOT HELD"));
}
