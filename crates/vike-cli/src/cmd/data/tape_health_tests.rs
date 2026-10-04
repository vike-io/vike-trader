use super::*;

/// A bar series the store could actually hold: hourly bars, on the grid, one row per slot.
fn healthy_bars() -> SeriesFacts {
    SeriesFacts {
        kind: "bar".to_string(),
        venue: "binance".to_string(),
        name: "BTCUSDT".to_string(),
        grouped: false,
        interval: Some("1h".to_string()),
        // 2026-01-01T00:00:00Z .. +9h, ten hourly slots, ten rows.
        first_ts: 1_767_225_600_000,
        last_ts: 1_767_225_600_000 + 9 * 3_600_000,
        rows: 10,
        parts: 1,
        dates: 1,
    }
}

fn codes(facts: &SeriesFacts) -> Vec<&'static str> {
    findings_for(facts).into_iter().map(|f| f.code).collect()
}

#[test]
fn a_well_formed_bar_series_produces_nothing() {
    assert_eq!(codes(&healthy_bars()), Vec::<&str>::new());
}

/// **The check this module exists for.** Eleven rows cannot fit in ten hourly slots, so at
/// least one timestamp is duplicated (or a finer bar was folded in) — proven by pigeonhole,
/// with no row on the wire. A backtest over this tape trades twice on one event.
#[test]
fn more_rows_than_the_grid_holds_is_a_duplicate_timestamp_proof() {
    let mut facts = healthy_bars();
    facts.rows = 11;
    let found = findings_for(&facts);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "rows-exceed-grid");
    assert_eq!(found[0].class, Class::Contradiction);
    // The evidence is IN the message: both counts and the excess, so an operator can check the
    // arithmetic rather than take the verdict on trust.
    assert!(found[0].detail.contains("11 rows"), "{}", found[0].detail);
    assert!(found[0].detail.contains("at most 10"), "{}", found[0].detail);
}

/// Exactly filling the grid is not a finding — the boundary is `>`, not `>=`, because a tape
/// with one bar per slot is the healthy case and firing on it would make the verb useless.
#[test]
fn exactly_filling_the_grid_is_clean() {
    let mut facts = healthy_bars();
    facts.rows = 10;
    assert_eq!(codes(&facts), Vec::<&str>::new());
    // …and FEWER rows than slots is ABSENCE, which `list --gaps` answers. Not this verb's
    // question, and reporting it here would duplicate a report that is already correct.
    facts.rows = 4;
    assert_eq!(codes(&facts), Vec::<&str>::new());
}

#[test]
fn an_inverted_span_is_a_contradiction() {
    let mut facts = healthy_bars();
    std::mem::swap(&mut facts.first_ts, &mut facts.last_ts);
    let found = findings_for(&facts);
    assert!(found.iter().any(|f| f.code == "span-inverted"), "{found:#?}");
    assert!(
        found.iter().all(|f| f.class == Class::Contradiction),
        "an impossible span is arithmetic, never a smell: {found:#?}"
    );
}

/// ⚠ The grid checks are SKIPPED on an inverted span rather than run on it. `last - first` is
/// negative there, so `slots` would be zero or negative and every row count would "exceed the
/// grid" — a second finding that says nothing the first did not, on numbers that are already
/// known to be wrong.
#[test]
fn an_inverted_span_does_not_also_manufacture_a_grid_finding() {
    let mut facts = healthy_bars();
    std::mem::swap(&mut facts.first_ts, &mut facts.last_ts);
    assert!(!codes(&facts).contains(&"rows-exceed-grid"), "{:#?}", findings_for(&facts));
}

/// The store's own empty fold — zero rows, all-zero coverage — is ABSENCE and produces nothing.
/// Treating it as a 1970 span would put a finding on every empty series in the store, which is
/// how a report earns its way into an operator's ignore list.
#[test]
fn the_stores_empty_fold_is_not_a_finding() {
    let facts = SeriesFacts {
        kind: "trade".to_string(),
        venue: "polymarket".to_string(),
        name: "TOK".to_string(),
        grouped: false,
        interval: None,
        first_ts: 0,
        last_ts: 0,
        rows: 0,
        parts: 0,
        dates: 0,
    };
    assert_eq!(codes(&facts), Vec::<&str>::new());
}

/// …but zero rows with a REAL span is the same fold contradicting itself, and it is reported.
#[test]
fn zero_rows_with_a_span_is_a_contradiction() {
    let mut facts = healthy_bars();
    facts.rows = 0;
    assert!(codes(&facts).contains(&"span-without-rows"), "{:#?}", findings_for(&facts));
}

/// Part files with no rows behind them: possible on its face, so SUSPECT — and it is still
/// reported on the empty-fold path, because that is the state it actually appears in.
#[test]
fn parts_without_rows_is_suspect_and_survives_the_empty_fold() {
    let facts = SeriesFacts {
        kind: "bar".to_string(),
        venue: "demo".to_string(),
        name: "DEMOUSDT".to_string(),
        grouped: false,
        interval: Some("1h".to_string()),
        first_ts: 0,
        last_ts: 0,
        rows: 0,
        parts: 3,
        dates: 0,
    };
    let found = findings_for(&facts);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "parts-without-rows");
    assert_eq!(found[0].class, Class::Suspect);
}

#[test]
fn more_partitions_than_the_span_has_days_is_a_contradiction() {
    let mut facts = healthy_bars();
    // Ten hours inside one UTC day cannot be spread across four `date=` directories.
    facts.dates = 4;
    let found = findings_for(&facts);
    assert!(found.iter().any(|f| f.code == "days-exceed-span"), "{found:#?}");
}

/// An endpoint off the UTC-midnight grid is SUSPECT — a resample anchored to its first row
/// rather than to the grid, whose bars will not line up with the venue's.
#[test]
fn an_off_grid_endpoint_is_suspect_when_the_step_divides_a_day() {
    let mut facts = healthy_bars();
    facts.first_ts += 137;
    facts.last_ts += 137;
    let found = findings_for(&facts);
    let off: Vec<&Finding> = found.iter().filter(|f| f.code == "endpoint-off-grid").collect();
    assert_eq!(off.len(), 2, "both endpoints are off the grid: {found:#?}");
    assert!(off.iter().all(|f| f.class == Class::Suspect), "{off:#?}");
    assert!(off[0].detail.contains("137"), "the offset is the evidence: {}", off[0].detail);
}

/// ⚠ And it is SKIPPED for a step that does not divide a UTC day. `7h` has no venue-owed grid
/// this arithmetic could test against, so the check would fire on legitimate data — which is
/// exactly the failure mode `Class::Suspect` is separated out to avoid.
#[test]
fn a_step_that_does_not_divide_a_day_is_not_grid_checked() {
    let mut facts = healthy_bars();
    facts.interval = Some("7h".to_string());
    facts.first_ts += 137;
    facts.last_ts += 137;
    assert!(!codes(&facts).contains(&"endpoint-off-grid"), "{:#?}", findings_for(&facts));
}

/// A tick-shaped kind carries no interval, so it has no grid: a second trade in the same
/// millisecond is ordinary market data, not a defect. Both grid checks are skipped, while the
/// span and partition checks still apply.
#[test]
fn a_tick_series_is_never_grid_checked_but_is_still_span_checked() {
    let mut facts = healthy_bars();
    facts.kind = "trade".to_string();
    facts.interval = None;
    facts.rows = 5_000_000;
    assert_eq!(codes(&facts), Vec::<&str>::new(), "no grid to exceed");

    facts.dates = 99;
    assert!(codes(&facts).contains(&"days-exceed-span"), "{:#?}", findings_for(&facts));
}

/// An unparseable interval is treated as NO grid rather than as a finding. Which interval
/// spellings exist is `vike_model::time::interval_ms`' business and a store may hold a kind
/// this CLI has never heard of — the same rule `super::check_spec` follows for a venue.
#[test]
fn an_unparseable_interval_is_not_itself_a_finding() {
    let mut facts = healthy_bars();
    facts.interval = Some("3q".to_string());
    facts.rows = 10_000;
    assert_eq!(codes(&facts), Vec::<&str>::new());
}

// ── the renderings ──────────────────────────────────────────────────────────────────────────

fn scan(facts: SeriesFacts) -> Scanned {
    let findings = findings_for(&facts);
    Scanned { facts, findings }
}

/// A clean scan SAYS SO, and says how many series it looked at — a silent exit could not be
/// told apart from a filter that matched nothing.
#[test]
fn a_clean_scan_states_the_count_it_covered() {
    let out = lines(&[scan(healthy_bars())], 1, false);
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(out[0].contains("1 series scanned"), "{}", out[0]);
    assert!(out[0].contains("nothing contradicts itself"), "{}", out[0]);
}

/// Only the offenders get rows, and the summary carries the two class counts SEPARATELY — a
/// single total would let one suspect read as one contradiction.
#[test]
fn only_offenders_are_rendered_and_the_classes_are_counted_apart() {
    let mut bad = healthy_bars();
    bad.name = "ETHUSDT".to_string();
    bad.rows = 11;
    bad.parts = 0;
    let mut smelly = healthy_bars();
    smelly.name = "SOLUSDT".to_string();
    smelly.first_ts += 137;
    smelly.last_ts += 137;

    let out = lines(&[scan(healthy_bars()), scan(bad), scan(smelly)], 3, false);
    let text = out.join("\n");
    assert!(!text.contains("BTCUSDT"), "the clean series is not rendered: {text}");
    assert!(text.contains("ETHUSDT") && text.contains("SOLUSDT"), "{text}");
    let summary = out.last().expect("a summary line is always last");
    assert!(summary.contains("3 series scanned"), "{summary}");
    assert!(summary.contains("2 with findings"), "{summary}");
    assert!(summary.contains("1 contradiction(s)"), "{summary}");
    assert!(summary.contains("2 suspect(s)"), "both endpoints: {summary}");
}

/// An empty scan is the FILTER's answer, not a health verdict — the two have different causes
/// and `super::empty_note` is the one place that distinction is drawn.
#[test]
fn nothing_matched_is_reported_as_a_filter_outcome() {
    let out = lines(&[], 12, true);
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("12 reported"), "{}", out[0]);
}

/// The document carries the inputs beside the verdict, for a clean series too, so a consumer
/// can re-derive the arithmetic instead of trusting it.
#[test]
fn the_document_carries_the_numbers_a_verdict_was_derived_from() {
    let doc = json_series(&[scan(healthy_bars())]);
    assert_eq!(doc.len(), 1);
    assert_eq!(doc[0]["healthy"], serde_json::json!(true));
    assert_eq!(doc[0]["coverage"]["rows"], serde_json::json!(10));
    assert_eq!(doc[0]["findings"].as_array().map(|f| f.len()), Some(0));
}

#[test]
fn the_totals_agree_with_the_rendered_summary() {
    let mut bad = healthy_bars();
    bad.rows = 11;
    bad.parts = 0;
    assert_eq!(totals(&[scan(healthy_bars()), scan(bad)]), (1, 0));
}
