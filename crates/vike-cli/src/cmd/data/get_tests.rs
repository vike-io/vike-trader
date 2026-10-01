use super::*;

const DAY_MS: i64 = 86_400_000;

fn spec() -> Spec {
    Spec { venue: "binance".to_string(), symbol: "BTCUSDT".to_string(), interval: "1h".to_string() }
}

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The spec is the THREE-part one, and the shape rule is `check_spec`'s rather than a second
/// grammar — so the shapes `gate` accepts are refused here, by the message `fetch` gives.
#[test]
fn the_spec_is_three_parts_and_reuses_the_plane_s_one_shape_check() {
    let s = parse_spec(" binance : BTCUSDT : 1h ").expect("whitespace is trimmed");
    assert_eq!(s, spec());
    assert_eq!(s.text(), "binance:BTCUSDT:1h", "the text is REBUILT, never echoed");
    for bad in ["binance:BTCUSDT", "binance:@group", "a:b:c:d", "binance::1h"] {
        let err = parse_spec(bad).expect_err("not a three-part spec");
        assert!(err.contains("VENUE:SYMBOL:INTERVAL"), "{bad}: {err}");
    }
}

/// **§8.2 RULE 1, the window half.** No window at all is refused BY NAME, a single bound is
/// accepted (§7.0's own worked example), and the two forms may not be mixed.
///
/// ⚠ The anti-vacuity control is the SECOND half of each pair: a parser that refused everything
/// would satisfy every refusal assertion here, so each one is paired with a line that must
/// PARSE.
#[test]
fn a_window_is_required_by_name_and_one_bound_is_a_window() {
    let err = parse_window(None, None, None).expect_err("an unbounded read is refused");
    assert!(err.contains("get needs a window"), "{err}");
    assert!(err.contains("--days") && err.contains("--from"), "…naming both forms: {err}");
    assert!(err.contains("export"), "…and the verb bulk extraction belongs to: {err}");

    // THE CONTROLS: each of the three forms parses, so the refusal above is about absence.
    assert_eq!(parse_window(Some("7"), None, None).expect("--days"), Window::Days(7));
    assert_eq!(
        parse_window(None, Some("0"), None).expect("--from alone is §7.0's example"),
        Window::Range { start: Some(0), end: None }
    );
    assert_eq!(
        parse_window(None, None, Some("0")).expect(
            "--to alone is half-open, and bounded by \
                                                       the ceiling"
        ),
        Window::Range { start: None, end: Some(0) }
    );

    let err = parse_window(Some("7"), Some("0"), None).expect_err("two ways to say one thing");
    assert!(err.contains("pass one"), "{err}");
}

/// The bounds a `--days` window resolves to are computed from the CALLER's clock reading, so
/// the arithmetic is pinned against a fixed instant rather than against whenever this ran.
#[test]
fn the_days_window_counts_back_from_the_callers_clock() {
    let now = 10 * DAY_MS;
    assert_eq!(Window::Days(3).bounds(now), (Some(7 * DAY_MS), Some(now)));
    // ...and an explicit range is carried through untouched, including its half-open forms.
    assert_eq!(
        Window::Range { start: Some(5), end: None }.bounds(now),
        (Some(5), None),
        "a resolved range does not consult the clock at all"
    );
}

/// The window refusals that are about ARITHMETIC rather than about shape, each paired with the
/// value on the other side of the boundary.
#[test]
fn a_window_that_cannot_be_computed_is_refused_rather_than_wrapped() {
    assert!(parse_window(Some("0"), None, None).unwrap_err().contains("no time at all"));
    assert!(parse_window(Some("-1"), None, None).unwrap_err().contains("no time at all"));
    assert!(parse_window(Some("x"), None, None).unwrap_err().contains("whole number"));
    // The `checked_mul` rung: `i64::MAX` days is more milliseconds than an i64 holds, and a
    // release build would WRAP it into a start bound in the far future.
    let err = parse_window(Some("200000000000"), None, None).expect_err("does not fit");
    assert!(err.contains("does not fit"), "{err}");
    assert_eq!(
        parse_window(Some("1"), None, None).expect("one day still fits"),
        Window::Days(1),
        "the control: the refusal is about the size, not about --days"
    );
    // An inverted range is REFUSED, never swapped.
    let err = parse_window(None, Some("2026-02-01"), Some("2026-01-01")).expect_err("inverted");
    assert!(err.contains("other way round"), "{err}");
    assert!(
        parse_window(None, Some("2026-01-01"), Some("2026-02-01")).is_ok(),
        "the control: the same two labels the right way round"
    );
}

/// **§8.2 RULE 1, the ceiling half.** `--limit` LOWERS and may not exceed, and the refusal names
/// the ceiling and the verb that does bulk.
#[test]
fn the_limit_may_lower_the_ceiling_and_may_not_exceed_it() {
    assert_eq!(parse_limit(None).expect("the default IS the ceiling"), ROW_CEILING);
    assert_eq!(parse_limit(Some("10")).expect("lowering is the point"), 10);
    assert_eq!(
        parse_limit(Some(&ROW_CEILING.to_string())).expect("the ceiling itself is allowed"),
        ROW_CEILING
    );
    let err = parse_limit(Some(&(ROW_CEILING + 1).to_string())).expect_err("above the ceiling");
    assert!(err.contains(&ROW_CEILING.to_string()), "the ceiling is NAMED: {err}");
    assert!(err.contains("export"), "…and so is the verb that does bulk: {err}");
    assert!(parse_limit(Some("0")).unwrap_err().contains("no rows at all"));
    assert!(parse_limit(Some("-1")).unwrap_err().contains("whole number"));
}

/// **§8.2 RULE 1, the disclosure.** Hitting the ceiling is REPORTED with the exact count
/// withheld, and NOT reported when nothing was.
#[test]
fn the_ceiling_is_reported_with_what_it_withheld_and_is_silent_otherwise() {
    let note = ceiling_note(1_500, 1_000, true).expect("500 rows were cut");
    assert!(note.contains("500 more rows"), "the EXACT count, not `there may be more`: {note}");
    assert!(note.contains("default ceiling"), "…and which ceiling it was: {note}");
    let named = ceiling_note(50, 10, false).expect("40 rows were cut");
    assert!(named.contains("--limit 10"), "a named limit says so rather than `default`: {named}");
    // THE CONTROLS: nothing withheld is no note, on either side of the boundary.
    assert_eq!(ceiling_note(1_000, 1_000, true), None, "a full answer is not a cut one");
    assert_eq!(ceiling_note(3, 10, false), None);
}

/// An empty answer states BOTH readings, because the wire carries only one.
///
/// ⚠ **…in EVERY rendering, which is the half that shipped broken.** `table` printed the note
/// and `jsonl` put it on stderr, while `json` carried it nowhere at all — so the one form a
/// SCRIPT reads was the one form that let a typo'd symbol read as a gap in the data. The
/// document's `note` field is asserted here, beside the string, so the two cannot part again.
#[test]
fn an_empty_answer_never_claims_the_series_is_empty() {
    let note = empty_note(&spec());
    assert!(note.contains("binance:BTCUSDT:1h"), "{note}");
    assert!(note.contains("ONE of two facts"), "the ambiguity is stated: {note}");
    assert!(note.contains("data hist ls"), "…and the verb that answers the other: {note}");

    let text = json_doc("get", "a", &spec(), (Some(0), None), ROW_CEILING, 0, &[]);
    let doc: Value = serde_json::from_str(&text).expect("one document");
    assert_eq!(doc["returned"], 0, "the case is an EMPTY answer");
    assert_eq!(doc["note"], note, "…and it carries the SAME sentence, not a second wording");
    // THE CONTROL: a non-empty answer carries no `note`, so the assertion above is about the
    // emptiness rather than about the key always being there.
    let bars = [bar(0, 1.0)];
    let text = json_doc("get", "a", &spec(), (Some(0), None), ROW_CEILING, 1, &bars);
    let doc: Value = serde_json::from_str(&text).expect("one document");
    assert!(doc.get("note").is_none(), "a note on every run stops being read: {doc}");
}

/// The format axis THIS verb serves — and `jsonl`, which is the reason it exists.
#[test]
fn the_row_verb_serves_jsonl_and_refuses_the_unserved_ones_by_name() {
    assert_eq!(parse_render("table").unwrap(), Render::Table);
    assert_eq!(parse_render("json").unwrap(), Render::Json);
    assert_eq!(parse_render("jsonl").unwrap(), Render::Jsonl);
    for (name, _) in UNSERVED_RENDERS {
        let err = parse_render(name).expect_err("not served here");
        assert!(err.contains(name), "{name}: {err}");
        assert!(err.contains("jsonl"), "{name} must name what IS served: {err}");
    }
    assert!(parse_render("yaml").unwrap_err().contains("unknown"));
    assert!(parse_render("").unwrap_err().contains("EMPTY"));
}

/// The default follows the PLANE, not the destination — `table` unless something said otherwise.
#[test]
fn the_default_render_is_the_table_and_json_is_the_shorthand() {
    assert_eq!(render_for(None, false).unwrap(), Render::Table);
    assert_eq!(render_for(None, true).unwrap(), Render::Json, "--json IS --format json");
    assert_eq!(render_for(Some("jsonl"), false).unwrap(), Render::Jsonl);
    // ⚠ `json_flag` is ignored whenever `--format` named something, and that is not this
    // function being lax: `crate::cmd::data`'s `parse` has already refused every way the two
    // can DISAGREE, so a `true` reaching here beside an explicit format is unreachable from a
    // real command line. Spelled out rather than left to be discovered, because the reachable
    // shape a reader would guess — "jsonl wins over --json" — is a rule nobody has to know.
    assert_eq!(render_for(Some("json"), true).unwrap(), Render::Json);
}

/// A row carries its own identity and omits the fields the store did not record — never a null
/// that reads as "unknown".
#[test]
fn a_row_is_self_describing_and_omits_what_was_not_recorded() {
    let plain = json_row(&spec(), &bar(DAY_MS, 3.5));
    assert_eq!(plain["venue"], "binance");
    assert_eq!(plain["symbol"], "BTCUSDT");
    assert_eq!(plain["interval"], "1h");
    assert_eq!(plain["close"], 3.5);
    assert!(plain["ts_utc"].as_str().expect("a rendered timestamp").contains("1970"));
    for absent in ["funding", "bid", "ask"] {
        assert!(plain.get(absent).is_none(), "an unrecorded {absent} is OMITTED: {plain}");
    }
    // THE CONTROL: a recorded one is present, so the omission above is about absence rather
    // than about the builder never emitting these keys.
    let funded = json_row(&spec(), &Bar { funding: Some(-0.0001), ..bar(0, 1.0) });
    assert_eq!(funded["funding"], -0.0001);
}

/// The document's counts are what let a consumer tell a complete answer from a cut one.
#[test]
fn the_document_carries_both_counts_and_derives_truncated_from_them() {
    let bars = [bar(0, 1.0), bar(DAY_MS, 2.0)];
    let text = json_doc("get", "127.0.0.1:7878", &spec(), (Some(0), Some(DAY_MS)), 2, 9, &bars);
    let doc: Value = serde_json::from_str(&text).expect("one document");
    assert_eq!(doc["subcommand"], "get");
    assert_eq!(doc["returned"], 9, "what the store answered with");
    assert_eq!(doc["shown"], 2, "…and what was printed");
    assert_eq!(doc["truncated"], true);
    assert_eq!(doc["bars"].as_array().expect("an array").len(), 2);
    assert_eq!(doc["window"]["from_date"], "1970-01-01");
    // THE CONTROL: an uncut answer says so, so `truncated` is derived rather than pinned true.
    let whole = json_doc("get", "a", &spec(), (None, Some(0)), 1000, 2, &bars);
    let doc: Value = serde_json::from_str(&whole).expect("one document");
    assert_eq!(doc["truncated"], false);
    assert_eq!(doc["window"]["from_ts"], Value::Null, "an unbounded side is null, not 0");
}

/// The table is rectangular, the header names every column it renders, and an OPTIONAL column
/// appears only when the answer has one.
#[test]
fn the_table_omits_an_optional_column_no_row_carries() {
    let plain = table_lines(&spec(), &[bar(0, 1.0), bar(DAY_MS, 2.0)]);
    assert!(plain[0].contains("binance:BTCUSDT:1h") && plain[0].contains("2 rows"));
    assert!(plain[1].contains("CLOSE") && plain[1].contains("VOLUME"));
    for absent in ["FUNDING", "BID", "ASK"] {
        assert!(!plain[1].contains(absent), "no row carries a {absent}: {}", plain[1]);
    }
    assert_eq!(plain.len(), 4, "a header line, a column header, and one line per bar");

    // THE CONTROL: with ONE row carrying a funding rate the column appears for EVERY row, and
    // the row that has none renders `-` rather than a zero.
    let mixed =
        table_lines(&spec(), &[Bar { funding: Some(0.25), ..bar(0, 1.0) }, bar(DAY_MS, 2.0)]);
    assert!(mixed[1].contains("FUNDING"), "{}", mixed[1]);
    assert!(mixed[2].contains("0.25"), "{}", mixed[2]);
    assert!(mixed[3].ends_with('-'), "an unrecorded cell is `-`, never 0: {}", mixed[3]);
}
