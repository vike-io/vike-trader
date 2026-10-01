use super::*;

const HOUR_ISO: &str = "2025-08-09T13:00:00Z";
const HOUR_SECS: i64 = 1_754_744_400;

fn metric(cohort: &str, total: f64, long: f64) -> MetricRow {
    MetricRow {
        ts: HOUR_ISO.to_string(),
        cohort: cohort.to_string(),
        total_position_value: Some(total),
        total_position_value_long: Some(long),
        label_basis: None,
    }
}

// ---- the taxonomy fold ---------------------------------------------------------------------

#[test]
fn capitalize_lowercases_the_tail_like_python_does() {
    // Rust's obvious port — upper the first char, leave the rest — returns "SHRIMP" here, and
    // the taxonomy filter then drops a real cohort.
    assert_eq!(normalize_cohort(Axis::Size, "SHRIMP"), "Shrimp");
    assert_eq!(normalize_cohort(Axis::Pnl, "wHaLe"), "Whale");
    assert_eq!(normalize_cohort(Axis::Size, "4x whale"), "4xWhale");
    assert_eq!(normalize_cohort(Axis::Pnl, "3x rekt"), "3xRekt");
}

#[test]
fn a_multiplier_prefix_stays_lowercase_but_an_x_word_does_not() {
    assert_eq!(normalize_cohort(Axis::Size, "2x fish"), "2xFish");
    assert_eq!(normalize_cohort(Axis::Size, "max fish"), "MaxFish", "'max' is not a multiplier");
    assert_eq!(normalize_cohort(Axis::Size, "x fish"), "XFish", "a bare 'x' has no digits");
}

#[test]
fn the_empty_label_survives_normalisation_so_the_filter_is_what_drops_it() {
    for axis in [Axis::Size, Axis::Pnl, Axis::Tier] {
        assert_eq!(normalize_cohort(axis, ""), "");
        assert!(!is_known(axis, ""));
    }
}

#[test]
fn the_unrankable_bucket_is_admitted_on_the_pnl_ladder_and_only_there() {
    assert_eq!(normalize_cohort(Axis::Pnl, "unknown"), "Unknown");
    assert!(is_known(Axis::Pnl, "Unknown"));
    assert!(is_known(Axis::Pnl, "Neutral"));
    assert!(!is_known(Axis::Size, "Unknown"), "there is no unrankable SIZE");
    assert!(!is_known(Axis::Tier, "Unknown"));
}

#[test]
fn every_tier_wire_label_maps_to_its_own_admitted_slug() {
    const RAW: [&str; 10] = [
        ">$2.5m",
        "$1m-$2.5m",
        "$500k-$1m",
        "$250k-$500k",
        "$100k-$250k",
        "$50k-$100k",
        "$25k-$50k",
        "$10k-$25k",
        "$1k-$10k",
        "<$1k",
    ];
    for (raw, want) in RAW.iter().zip(TIER_COHORTS.iter()) {
        let got = normalize_cohort(Axis::Tier, raw);
        assert_eq!(&got.as_str(), want, "{raw}");
        assert!(is_known(Axis::Tier, &got));
        assert!(
            got.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "{got} is not column-safe"
        );
    }
}

#[test]
fn a_legacy_spelling_is_dropped_and_counted_never_folded_onto_a_rung() {
    // The property that REPLACES alias resolution. If an alias step comes back, "BigWhale"
    // resolves to "2xWhale" again, IS admitted, and this names the regression.
    for raw in ["big whale", "BigWhale", "SmallDolphin", "Micro", "Large"] {
        assert!(!is_known(Axis::Size, &normalize_cohort(Axis::Size, raw)), "{raw}");
    }
    for raw in ["smart money", "SmartMoney", "Big_loss", "looser"] {
        assert!(!is_known(Axis::Pnl, &normalize_cohort(Axis::Pnl, raw)), "{raw}");
    }
}

// ---- the guards ----------------------------------------------------------------------------

#[test]
fn an_absent_label_basis_is_a_hard_failure_not_a_pass() {
    // THE lookahead case, and the one a "present but wrong" guard waves through: a server that
    // predates `labelBasis` answers 200 with query-time labels and no field at all.
    let page = CohortPage { label_basis: None, ..Default::default() };
    let err = guard_label_basis(&page, "u").unwrap_err().to_string();
    assert!(err.contains("<field absent>"), "{err}");
    assert!(err.contains("lookahead"), "the message has to say WHY: {err}");
}

#[test]
fn a_disagreeing_label_basis_is_a_hard_failure_in_the_envelope_and_in_a_row() {
    let page = CohortPage { label_basis: Some("current".into()), ..Default::default() };
    assert!(guard_label_basis(&page, "u").unwrap_err().to_string().contains("current"));

    let mut row = metric("4xWhale", 100.0, 60.0);
    row.label_basis = Some("current".into());
    let page = CohortPage {
        label_basis: Some(POINT_IN_TIME.into()),
        metrics: Some(vec![metric("Shrimp", 3.0, 1.0), row]),
        ..Default::default()
    };
    let err = guard_label_basis(&page, "u").unwrap_err().to_string();
    assert!(err.contains("row 1"), "the message names the offending index: {err}");
    assert!(err.contains("current"), "{err}");
}

#[test]
fn a_point_in_time_envelope_with_agreeing_rows_passes() {
    let mut row = metric("4xWhale", 100.0, 60.0);
    row.label_basis = Some(POINT_IN_TIME.into());
    let page = CohortPage {
        label_basis: Some(POINT_IN_TIME.into()),
        metrics: Some(vec![row, metric("Shrimp", 3.0, 1.0)]),
        ..Default::default()
    };
    assert!(guard_label_basis(&page, "u").is_ok(), "a row that echoes NOTHING is not a failure");
}

#[test]
fn an_absent_grading_echo_passes_only_for_the_default_grading() {
    let page = CohortPage { grading: None, ..Default::default() };
    assert!(guard_grading(&page, "u", Grading::Realized).is_ok());
    for g in [Grading::RealizedPit, Grading::Unrealized] {
        let err = guard_grading(&page, "u", g).unwrap_err().to_string();
        assert!(err.contains("field absent"), "{err}");
        assert!(err.contains(g.echoed()), "{err}");
    }
}

#[test]
fn a_grading_echo_that_names_another_question_is_refused() {
    let page = CohortPage { grading: Some("realized".into()), ..Default::default() };
    assert!(guard_grading(&page, "u", Grading::Realized).is_ok());
    let err = guard_grading(&page, "u", Grading::Unrealized).unwrap_err().to_string();
    assert!(err.contains("unrealized") && err.contains("realized"), "{err}");
}

// ---- the hour, and the millisecond ---------------------------------------------------------

#[test]
fn a_bucket_label_parses_to_whole_seconds_on_the_hour() {
    assert_eq!(parse_hour(HOUR_ISO).unwrap(), HOUR_SECS);
    assert_eq!(parse_hour("2025-08-09T13:00:00.000Z").unwrap(), HOUR_SECS);
    assert_eq!(parse_hour("2025-08-09T13:00:00+00:00").unwrap(), HOUR_SECS);
    assert_eq!(parse_hour("2024-02-29T00:00:00Z").unwrap(), 1_709_164_800);
}

#[test]
fn an_off_the_hour_or_off_utc_label_is_refused_rather_than_rounded() {
    for bad in [
        "2025-08-09T13:02:17Z",
        "2025-08-09T13:00:30Z",
        "2025-08-09T13:00:00.500Z",
        "2025-08-09T13:00:00+02:00",
        "2025-08-09T13:00",
        "not-a-time",
        "",
    ] {
        assert!(parse_hour(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn the_hour_reaches_the_row_in_milliseconds_and_an_unmultiplied_one_would_land_in_1970() {
    let ms = hour_ms(HOUR_SECS).unwrap();
    assert_eq!(ms, HOUR_SECS * 1_000);
    assert_eq!(vike_model::time::epoch_ms_to_utc_date(ms), "2025-08-09");
    // ...and the defect this conversion exists to prevent, spelled out: the SECONDS value read
    // as milliseconds partitions in 1970, silently and only once (the commit key is then spent).
    assert_eq!(vike_model::time::epoch_ms_to_utc_date(HOUR_SECS), "1970-01-21");
}

#[test]
fn an_off_the_hour_integer_is_refused_by_the_second_guard_too() {
    // A different input from the off-the-hour STRING above: this guards a caller that COMPUTED
    // a bucket rather than parsing one.
    assert!(hour_ms(HOUR_SECS + 1).is_err());
    assert!(hour_ms(HOUR_SECS + 1_800).is_err());
    assert!(hour_ms(-SECS_PER_HOUR).is_ok(), "pre-epoch on the hour is still on the hour");
}

// ---- the snap ------------------------------------------------------------------------------

/// Two spellings of ONE aggregate, as two executions of a byte-identical URL returned them.
///
/// Spelled as SOURCE TEXT rather than as `f64` literals on purpose: the divergence is in the
/// JSON's decimal digits, so a test that started from two f64s would be assuming the very thing
/// it exists to demonstrate. (The same reasoning, and the same measured pairs, as its
/// predecessor `crates/vike-research/src/sources/api.rs`'s `OBSERVED_JITTER`.)
const OBSERVED_JITTER: [(&str, &str); 4] = [
    ("95658388.82664996", "95658388.82665"),
    ("74162423.71964997", "74162423.71965"),
    ("64750303.66825", "64750303.66825001"),
    ("45320236.67625", "45320236.67625001"),
];

#[test]
fn the_endpoints_two_spellings_of_one_notional_snap_to_one_f64() {
    for (a, b) in OBSERVED_JITTER {
        let (fa, fb) = (a.parse::<f64>().unwrap(), b.parse::<f64>().unwrap());
        // The control: they really are different f64s, so the snap has work to do.
        assert_ne!(fa.to_bits(), fb.to_bits(), "{a} and {b} already parse the same");
        let rel = (fa - fb).abs() / fa.abs();
        assert!(rel < 1e-15, "{a} vs {b}: {rel:e} is larger than the noise this cures");
        assert_eq!(
            canonical_notional(fa).to_bits(),
            canonical_notional(fb).to_bits(),
            "{a} and {b} still differ after the snap"
        );
    }
}

/// Measured MIDPOINT pairs — values whose decimal expansion terminates exactly on the ten-digit
/// grid's midpoint, so plain rounding sends the two spellings opposite ways. These are what
/// turned [`MIDPOINT_NUDGE`] from an argument into a measurement.
const OBSERVED_STRADDLES: [(&str, &str); 4] = [
    ("4960365.979499999", "4960365.9795"),
    ("136029.34495000003", "136029.34495"),
    ("111549.10795", "111549.10794999999"),
    ("95753097.055", "95753097.05499998"),
];

/// [`MIDPOINT_NUDGE`] is LOAD-BEARING, and this is where it is defended: these pairs survive
/// the snap WITH it and provably do not survive the same grid WITHOUT it. Delete the second
/// half and the nudge could be removed with every other test in this file still green.
#[test]
fn the_midpoint_nudge_absorbs_the_straddles_plain_rounding_cannot() {
    for (a, b) in OBSERVED_STRADDLES {
        let (fa, fb) = (a.parse::<f64>().unwrap(), b.parse::<f64>().unwrap());
        assert_ne!(fa.to_bits(), fb.to_bits(), "{a} and {b} already parse the same");
        assert_eq!(
            canonical_notional(fa).to_bits(),
            canonical_notional(fb).to_bits(),
            "{a} and {b} still differ after the snap"
        );
        assert_ne!(
            snap_with(fa, NOTIONAL_SIG_DIGITS, 0.0).to_bits(),
            snap_with(fb, NOTIONAL_SIG_DIGITS, 0.0).to_bits(),
            "{a} and {b} no longer straddle the un-nudged grid — this pair has stopped being \
                 evidence for the nudge, so the nudge is now untested rather than proven"
        );
    }
}

/// The nudge is a TIE-BREAK, not a bias: it may only move a value within a hair of a midpoint,
/// and must leave every ordinary value exactly where plain rounding put it. ~2.3% of sampled
/// notionals sit on a midpoint, so 97% must be untouched — a nudge big enough to move an
/// ordinary value would be a silent precision change dressed as a tie-break.
#[test]
fn the_nudge_moves_only_the_knife_edge_and_sits_between_the_two_scales_it_separates() {
    for v in [95_658_388.826_649_96_f64, 7_053.021_999_999_997, 0.004, 1.0, 9.87e12, 12.34] {
        assert_eq!(
            canonical_notional(v).to_bits(),
            snap_with(v, NOTIONAL_SIG_DIGITS, 0.0).to_bits(),
            "the nudge moved {v}, which is not on a midpoint"
        );
    }
    const NOISE: f64 = 4e-16; // the endpoint's measured reduction jitter
    const GRID: f64 = 1e-9; // 10^-(NOTIONAL_SIG_DIGITS - 1)
    const {
        assert!(MIDPOINT_NUDGE > 100.0 * NOISE, "the nudge is inside the noise it must clear");
        assert!(MIDPOINT_NUDGE * 100.0 < GRID, "the nudge is a real fraction of a grid step");
    }
    assert_eq!(
        GRID,
        10f64.powi(-(NOTIONAL_SIG_DIGITS as i32 - 1)),
        "GRID above no longer matches NOTIONAL_SIG_DIGITS, so the two bounds gate nothing"
    );
}

#[test]
fn snapping_leaves_a_non_finite_and_a_signed_zero_alone() {
    assert!(canonical_notional(f64::NAN).is_nan());
    assert_eq!(canonical_notional(f64::INFINITY), f64::INFINITY);
    assert!(canonical_notional(-0.0).is_sign_negative());
}

// ---- rows ----------------------------------------------------------------------------------

#[test]
fn a_row_carries_the_fetchs_axis_grading_and_basis_rather_than_its_own() {
    let (rows, dropped) = rows_from_metrics(
        "BTC",
        Axis::Size,
        Grading::Realized,
        POINT_IN_TIME,
        &[metric("4x whale", 100.0, 60.0)],
    )
    .unwrap();
    assert!(dropped.is_empty());
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.ts, HOUR_SECS * 1_000);
    assert_eq!(r.asset, "BTC");
    assert_eq!(r.axis, "size");
    assert_eq!(r.cohort, "4xWhale");
    assert_eq!(r.grading, "realized");
    assert_eq!(r.label_basis, POINT_IN_TIME);
    assert_eq!(r.long_usd, 60.0);
    assert_eq!(r.total_usd, 100.0);
    assert_eq!(r.short_usd(), 40.0, "the short side is DERIVED at read time, never stored");
}

#[test]
fn a_notional_is_snapped_on_the_way_into_the_row_not_on_the_way_out() {
    // Batch idempotency makes the first write authoritative forever, so the snap has to happen
    // before the append — not in a reader.
    let (rows, _) = rows_from_metrics(
        "BTC",
        Axis::Size,
        Grading::Realized,
        POINT_IN_TIME,
        &[metric("whale", 95_658_388.826_649_96, 4_960_365.979_499_999)],
    )
    .unwrap();
    assert_eq!(rows[0].total_usd, canonical_notional(95_658_388.826_649_96));
    assert_eq!(rows[0].long_usd, canonical_notional(4_960_365.979_499_999));
    assert_ne!(rows[0].long_usd, 4_960_365.979_499_999, "the raw spelling did not survive");
}

#[test]
fn an_unknown_label_is_dropped_and_counted_by_its_folded_name() {
    let (rows, dropped) = rows_from_metrics(
        "BTC",
        Axis::Size,
        Grading::Realized,
        POINT_IN_TIME,
        &[
            metric("4x whale", 100.0, 60.0),
            metric("big whale", 9.0, 9.0),
            metric("big whale", 8.0, 8.0),
            metric("", 1.0, 1.0),
        ],
    )
    .unwrap();
    assert_eq!(rows.len(), 1, "only the admitted rung is stored");
    assert_eq!(dropped.get("BigWhale"), Some(&2), "counted by its FOLDED name");
    assert_eq!(dropped.get(""), Some(&1), "the empty label is a drop, not a silent skip");
}

#[test]
fn a_missing_notional_refuses_the_fetch_rather_than_inventing_a_zero() {
    let mut row = metric("4x whale", 100.0, 60.0);
    row.total_position_value = None;
    let err = rows_from_metrics("BTC", Axis::Size, Grading::Realized, POINT_IN_TIME, &[row])
        .unwrap_err()
        .to_string();
    assert!(err.contains("REFUSE"), "{err}");
}

#[test]
fn a_total_below_its_long_side_is_a_source_contradiction_and_stops_the_run() {
    let err = rows_from_metrics(
        "BTC",
        Axis::Size,
        Grading::Realized,
        POINT_IN_TIME,
        &[metric("4x whale", 60.0, 100.0)],
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("LESS than"), "{err}");
    // ...but a sub-threshold negative on a nine-figure notional is not a contradiction, and it
    // is stored AS IT CAME: this kind stores the PAIR, so clamping would mean writing a number
    // the wire never served. The tolerance is 1e-6 relative — ~95.7 on this total — and the
    // snap's own grid here is 0.01, so a 1.0 excess is comfortably inside one and outside the
    // other, i.e. it really does reach the check rather than being rounded away first.
    let total = 95_658_388.826_65_f64;
    let (rows, _) = rows_from_metrics(
        "BTC",
        Axis::Size,
        Grading::Realized,
        POINT_IN_TIME,
        &[metric("4x whale", total, total + 1.0)],
    )
    .unwrap();
    assert!(rows[0].short_usd() < 0.0, "the reader's accessor is what surfaces it, by design");
    assert!(rows[0].short_usd() > -2.0, "…and it is the wire's own excess, not a fabrication");
}

#[test]
fn a_repeated_ts_cohort_across_a_page_overlap_keeps_the_first_and_never_sums() {
    let mk = |ts: i64, cohort: &str, total: f64| CohortRow {
        ts,
        asset: "BTC".into(),
        axis: "size".into(),
        cohort: cohort.into(),
        grading: "realized".into(),
        label_basis: POINT_IN_TIME.into(),
        long_usd: total / 2.0,
        total_usd: total,
    };
    // Pages arrive newest-first, so the FIRST sighting is the one to keep.
    let out = dedupe_first_wins(vec![
        mk(2_000, "Whale", 10.0),
        mk(1_000, "Whale", 20.0),
        mk(2_000, "Whale", 99.0), // the overlap
        mk(1_000, "Shrimp", 1.0),
    ]);
    assert_eq!(out.len(), 3);
    assert_eq!(
        out.iter().map(|r| (r.ts, r.cohort.as_str())).collect::<Vec<_>>(),
        vec![(1_000, "Shrimp"), (1_000, "Whale"), (2_000, "Whale")],
        "ascending by (ts, cohort)"
    );
    assert_eq!(out[2].total_usd, 10.0, "first wins — NOT 99.0, and never 109.0");
}

// ---- the page shape -------------------------------------------------------------------------

#[test]
fn a_page_decodes_its_envelope_its_rows_and_its_cursor() {
    let body = r#"{
            "label_basis": "point_in_time",
            "grading": "realized",
            "nextCursor": "1000:5",
            "metrics": [
                {"ts": "2025-08-09T13:00:00Z", "cohort": "4x whale",
                 "total_position_value": 100.0, "total_position_value_long": 60.0}
            ]
        }"#;
    let page = parse_page(body, "u").unwrap();
    assert_eq!(page.label_basis.as_deref(), Some(POINT_IN_TIME));
    assert_eq!(page.grading.as_deref(), Some("realized"));
    assert_eq!(page.next_cursor.as_deref(), Some("1000:5"));
    assert_eq!(page.metrics.as_ref().unwrap().len(), 1);
}

#[test]
fn an_empty_envelope_decodes_to_no_rows_and_no_cursor_rather_than_failing() {
    // The walk's second stop needs this shape to exist: a page with `metrics: []` and a cursor
    // is how the endpoint says "nothing more".
    let page = parse_page(r#"{"metrics": [], "nextCursor": "abc"}"#, "u").unwrap();
    assert!(page.metrics.unwrap().is_empty());
    assert_eq!(page.next_cursor.as_deref(), Some("abc"));
    let page = parse_page("{}", "u").unwrap();
    assert!(page.metrics.is_none() && page.next_cursor.is_none());
}

#[test]
fn a_body_that_is_not_this_endpoints_json_names_the_url_it_came_from() {
    let err = parse_page("<html>502</html>", "https://data.vike.io/v1/x").unwrap_err();
    assert!(err.to_string().contains("https://data.vike.io/v1/x"), "{err}");
}
