use super::*;

fn cov(first_ts: i64, last_ts: i64, rows: u64) -> SeriesCoverage {
    SeriesCoverage { first_ts, last_ts, rows, bytes: 1, parts: 1, dates: 1 }
}

fn bar(symbol: &str) -> SeriesId {
    SeriesId::per_symbol("bar", "binance", symbol, Some("1h".to_string()))
}

fn planned(
    id: SeriesId,
    coverage: Option<SeriesCoverage>,
    from: Option<i64>,
    to: Option<i64>,
    gaps: &[(i64, i64)],
) -> PlannedSeries {
    let recorded = coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_ts, c.last_ts));
    PlannedSeries {
        missing: window_shortfall(from, to, recorded, gaps),
        id,
        role: SeriesRole::Price,
        coverage,
    }
}

fn plan(series: Vec<PlannedSeries>, enumerable: bool) -> DataPlan {
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
    assert_eq!(v.findings[0].spans[0].kind, Shortfall::Leading);
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
    assert_eq!(v.findings[0].spans[0].kind, Shortfall::Everything);
}

/// An INVENTORIED series with zero rows is not a 1970 tape.
#[test]
fn a_present_but_empty_series_reads_as_wholly_missing() {
    let p =
        plan(vec![planned(bar("BTCUSDT"), Some(cov(0, 0, 0)), Some(0), Some(1_000), &[])], true);
    let v = coverage_verdict(&p, &ARMED);
    assert_eq!(v.findings[0].spans[0].kind, Shortfall::Everything);
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

#[test]
fn the_refusal_offers_a_fetch_command_for_a_bar_series_and_none_for_a_group() {
    let id = bar("BTCUSDT");
    let span = MissingSpan { start_ms: 0, end_ms: 86_400_000, kind: Shortfall::Leading };
    let hint = fetch_hint(&id, Some(&span)).expect("a per-symbol bar series can be fetched");
    assert!(hint.starts_with("vike-cli data hist fetch binance:BTCUSDT:1h --from "));
    let grouped = SeriesId::grouped("trade", "polymarket", "g1");
    assert!(
        fetch_hint(&grouped, Some(&span)).is_none(),
        "`data fetch` cannot write a grouped series, so suggesting it would be worse than \
             saying nothing"
    );
}

#[test]
fn no_fetch_command_is_offered_for_an_unbounded_span() {
    let span = MissingSpan { start_ms: i64::MIN, end_ms: i64::MAX, kind: Shortfall::Everything };
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
    let s =
        planned(SeriesId::grouped("trade", "polymarket", "g1"), None, Some(0), Some(1_000), &[]);
    let p = plan(vec![s], true);
    let v = universe_verdict(&p, UniverseMode::Strict);
    assert!(v.is_clean());
    assert!(v.present.is_empty(), "an empty `present` is not a clean bill of health");
}

#[test]
fn a_non_price_series_is_not_a_universe_finding() {
    let mut s = planned(
        SeriesId::per_symbol("properties", "binance", "BTCUSDT", None),
        None,
        Some(0),
        Some(1_000),
        &[],
    );
    s.role = SeriesRole::Properties;
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

/// ⚠ **The roster, the resolver and the refusal are ONE thing — proven, because a mutation
/// showed they were three.**
///
/// The hole this closes, in the order it was found: `harness::profile`'s `DataCfg::on_gap`
/// matched three string arms; its refusal spelled the same three by hand; and
/// `crates/vike-cli/src/surface.rs`'s `gap_dispositions` row spelled them a third time. A
/// CLI-side test compared the roster it PARSED OUT OF THE REFUSAL to the surface row — and a
/// fourth arm planted in the resolver left that test GREEN, because the message it read is a
/// copy too. Copy-against-copy is not a gate.
///
/// Now [`OnGap::parse`] walks [`OnGap::NAMES`] instead of matching, the refusal renders
/// [`OnGap::roster`], and this test holds the last edge: every name resolves, and every
/// resolved value names itself back. Combined with [`OnGap::name`] being an exhaustive `match`
/// on the VARIANT — a new variant fails to compile there — a spelling cannot be accepted
/// without a row, and a row cannot exist without a variant that answers for it.
#[test]
fn the_coverage_rosters_are_the_one_door_their_resolvers_walk() {
    for name in OnGap::NAMES {
        let v = OnGap::parse(name)
            .unwrap_or_else(|| panic!("`{name}` is in NAMES and parse refuses it"));
        assert_eq!(v.name(), name, "`{name}` resolves to a value that names itself differently");
    }
    for name in UniverseMode::NAMES {
        let v = UniverseMode::parse(name)
            .unwrap_or_else(|| panic!("`{name}` is in NAMES and parse refuses it"));
        assert_eq!(v.name(), name, "`{name}` resolves to a value that names itself differently");
    }
    // The acceptance is case- and space-insensitive, which is what the CLI's own
    // `flag_vocab` ascii_ci rule relies on — and it is the roster that decides, not a
    // second lowercase list.
    assert_eq!(OnGap::parse("  REFUSE "), Some(OnGap::Refuse));
    assert_eq!(UniverseMode::parse("Strict"), Some(UniverseMode::Strict));
    // ...and nothing outside the roster is accepted by any path.
    assert_eq!(OnGap::parse("fill"), None);
    assert_eq!(UniverseMode::parse("all"), None);
    // The rendered set is the roster verbatim, so no refusal can spell it by hand again.
    assert_eq!(OnGap::roster(), "refuse | warn | run");
    assert_eq!(UniverseMode::roster(), "declared | covered | strict");
}
