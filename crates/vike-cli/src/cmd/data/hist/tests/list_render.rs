//! The `list` rendering: its two fixture rows, the filter, the columns, gaps and `--class`.

use super::*;

/// A per-symbol bar row and a grouped tick row — every rendering property below is visible in
/// one pair: the four dimensions, the `symbol`/`group` alternative, and an absent interval.
fn bar_row() -> SeriesRow {
    SeriesRow {
        kind: "bar".to_string(),
        venue: "binance".to_string(),
        name: "BTCUSDT".to_string(),
        grouped: false,
        symbol: "BTCUSDT".to_string(),
        group: None,
        interval: Some("1h".to_string()),
        coverage: Coverage {
            first_ts: 0,
            last_ts: 86_400_000,
            rows: 48,
            bytes: 900,
            parts: 2,
            dates: 2,
        },
        gaps: None,
        gaps_error: None,
        class: None,
    }
}

fn grouped_row() -> SeriesRow {
    SeriesRow {
        kind: "trade".to_string(),
        venue: "polymarket".to_string(),
        name: "fam".to_string(),
        grouped: true,
        // ⚠ EMPTY, exactly as the store reports it for a grouped series. The whole point of
        // the fixture: anything that renders `symbol` would render nothing here.
        symbol: String::new(),
        group: Some("fam".to_string()),
        interval: None,
        coverage: Coverage {
            first_ts: 172_800_000,
            last_ts: 259_200_000,
            rows: 7,
            bytes: 40,
            parts: 1,
            dates: 2,
        },
        gaps: None,
        gaps_error: None,
        class: None,
    }
}

// ---- the filter ----

/// Case-insensitive SUBSTRING, ANDed, and an absent dimension matches everything. The grouped
/// case is the load-bearing one: `--name` matches a group, which is why it is not `--symbol`.
#[test]
fn the_filter_is_case_insensitive_substring_and_reaches_a_group_by_name() {
    let f = |args: &[&str]| parse_of(args).unwrap().filter;

    let all = Filter::default();
    assert!(all.matches(Some("bar"), "binance", "BTCUSDT"));
    assert!(all.is_empty());

    let by_name = f(&["hist", "ls", "--name", "btc"]);
    assert!(by_name.matches(Some("bar"), "binance", "BTCUSDT"), "case-insensitive substring");
    assert!(!by_name.matches(Some("bar"), "binance", "ETHUSDT"));
    assert!(!by_name.is_empty());

    // A GROUPED series' label is its group; `--name fam` must reach it.
    let grouped = f(&["hist", "ls", "--name", "FAM"]);
    assert!(grouped.matches(Some("trade"), "polymarket", "fam"));

    // ANDed: every named dimension has to agree.
    let both = f(&["hist", "ls", "--kind", "bar", "--venue", "okx"]);
    assert!(both.matches(Some("bar"), "okx", "BTC-USDT"));
    assert!(!both.matches(Some("trade"), "okx", "BTC-USDT"));
    assert!(!both.matches(Some("bar"), "binance", "BTCUSDT"));

    // A row with NO kind dimension (the `coverage` caller) passes the kind test — and `--kind`
    // cannot reach that path at all, because `parse` refuses it there.
    assert!(f(&["hist", "ls", "--kind", "bar"]).matches(None, "binance", "BTCUSDT"));
}

// ---- the `list` rendering ----

/// ⚠ The identity is rendered as COLUMNS, never as a colon-string: `kind` and a `SCOPE` cell
/// are their own cells, so a grouped series reads as a group rather than as a venue with two
/// empties after it.
#[test]
fn list_lines_render_four_dimensions_and_never_a_colon_string() {
    let rows = [bar_row(), grouped_row()];
    let lines = list_lines(&rows, 2, false, false, false);

    let cells =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_string).collect() };
    assert_eq!(cells(&lines[0])[..5], ["KIND", "VENUE", "SCOPE", "NAME", "INTERVAL"]);
    assert_eq!(cells(&lines[1])[..7], ["bar", "binance", "symbol", "BTCUSDT", "1h", "48", "2"]);
    // The grouped row: its NAME comes from the group, and its interval is genuinely absent
    // rather than an empty cell — the two things a colon-string identity gets wrong.
    assert_eq!(cells(&lines[2])[..7], ["trade", "polymarket", "group", "fam", "-", "7", "2"]);
    // …and the columns actually line up, which is what makes the table skimmable at all.
    let scope_col = lines[0].find("SCOPE").expect("the header names the column");
    assert_eq!(lines[1].find("symbol"), Some(scope_col), "{}", lines[1]);
    assert_eq!(lines[2].find("group"), Some(scope_col), "{}", lines[2]);
    for line in &lines {
        assert!(!line.contains("binance:BTCUSDT"), "no colon-string identity: {line}");
    }
    assert_eq!(lines.last().unwrap(), "2 series · 55 rows");
}

/// A filtered listing says how many the SERVER reported beside how many survived — otherwise
/// "3 series" is unreadable without knowing whether it was 3 of 3 or 3 of 3000.
#[test]
fn a_filtered_listing_counts_both_sides() {
    let lines = list_lines(&[bar_row()], 9, true, false, false);
    assert_eq!(lines.last().unwrap(), "1 of 9 series · 48 rows");
}

/// A zero-row series renders `-` for its span rather than a well-formed `1970-01-01` folded
/// from an all-zero coverage — while [`list_json`] keeps the store's own numbers untouched.
#[test]
fn a_zero_row_series_shows_a_dash_span_and_the_document_still_carries_the_zeroes() {
    let mut row = bar_row();
    row.coverage = Coverage::default();
    let lines = list_lines(std::slice::from_ref(&row), 1, false, false, false);
    assert!(!lines[1].contains("1970"), "a sentinel span is not a date: {}", lines[1]);
    let cells: Vec<&str> = lines[1].split_whitespace().collect();
    assert_eq!(&cells[cells.len() - 2..], ["-", "-"], "both span cells: {}", lines[1]);

    let args = parse_of(&["hist", "ls", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&list_json(&args, std::slice::from_ref(&row), 1)).unwrap();
    assert_eq!(doc["series"][0]["coverage"]["first_ts"], 0, "the document is not edited");
    assert_eq!(doc["series"][0]["coverage"]["rows"], 0);
}

/// All THREE gap outcomes are said out loud. An unasked question, a clean series and a probe
/// that failed must not share a rendering — the middle one is what an operator ran `gaps`
/// to learn.
#[test]
fn gap_lines_distinguish_holes_from_none_from_unanswerable() {
    let mut row = bar_row();
    assert!(gap_lines(&row).is_empty(), "an `ls` row carries no annotation at all");

    row.gaps = Some(Vec::new());
    assert_eq!(gap_lines(&row), vec!["      no gaps".to_string()]);

    row.gaps = Some(vec![(86_400_000, 172_800_000)]);
    assert_eq!(gap_lines(&row), vec!["      gap 1970-01-02 .. 1970-01-03".to_string()]);

    row.gaps = None;
    row.gaps_error = Some("manifest unreadable".to_string());
    let lines = gap_lines(&row);
    assert!(lines[0].contains("gaps unavailable"), "{lines:?}");
    assert!(lines[0].contains("manifest unreadable"), "the server's own words: {lines:?}");
}

/// The `--gaps` degrade, seen through the renderer that carries it: the LISTING still renders
/// in full and the failure lands on the row it belongs to.
#[test]
fn a_failed_gap_probe_annotates_its_row_and_leaves_the_listing_whole() {
    let mut broken = bar_row();
    broken.gaps_error = Some("series_gaps: manifest unreadable".to_string());
    let mut clean = grouped_row();
    clean.gaps = Some(Vec::new());

    let lines = list_lines(&[broken, clean], 2, false, true, false);
    assert!(lines.iter().any(|l| l.contains("gaps unavailable")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("no gaps")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row itself survives: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("fam")), "{lines:?}");
}

// ---- `--class`: the reader `SymbolProperties::asset_class` did not have ----

/// Every one of the five probe outcomes renders as its own WORD, and none of them as a blank.
///
/// ⚠ **What would make this fail, and why it matters more than it looks:** folding
/// `unclassified` and `no-properties` into one cell. They are the two halves of "there is no
/// class here" and they send an operator to different places — the venue's producer, or the
/// recorder that never ran for this instrument. `vike_model::SymbolProperties::asset_class`
/// keeps them apart in the store by being an `Option` at all; a renderer that collapsed them
/// would undo that on the last hop.
#[test]
fn every_class_outcome_has_its_own_word_and_none_of_them_is_blank() {
    let cell = |probe: Option<ClassProbe>| {
        let mut row = bar_row();
        row.class = probe;
        class_cell(&row)
    };

    assert_eq!(cell(Some(ClassProbe::Classified("CryptoPerp"))), "CryptoPerp");
    assert_eq!(cell(Some(ClassProbe::Unclassified)), "unclassified");
    assert_eq!(cell(Some(ClassProbe::Unrecorded)), "no-properties");
    assert_eq!(cell(Some(ClassProbe::Grouped)), "(group)");
    assert_eq!(cell(Some(ClassProbe::Failed("boom".into()))), "(error)");
    assert_eq!(cell(None), "-", "the unreachable arm is still a cell, never a panic");

    // The two that mean "no class" are DIFFERENT words — the assertion the fold above would
    // break, stated on its own so the failure names the defect rather than a string.
    assert_ne!(
        cell(Some(ClassProbe::Unclassified)),
        cell(Some(ClassProbe::Unrecorded)),
        "a producer that named no class and a recorder that never ran are different findings"
    );
}

/// The class word is the MODEL's own spelling, taken from `vike_model::AssetClass::sql_word`
/// (which is also its serde word) — never a lowercase or prettified second form minted here.
/// A third spelling of one vocabulary is the exact defect `crates/vike-model/src/instrument/asset_class.rs`
/// opens its module doc with.
#[test]
fn the_rendered_class_word_is_the_models_own_spelling() {
    for class in vike_model::AssetClass::ALL {
        let mut row = bar_row();
        row.class = Some(ClassProbe::Classified(class.sql_word()));
        assert_eq!(class_cell(&row), class.sql_word());
    }
}

/// The CLASS column appears ONLY under `--class`, and without it every line is byte-identical
/// to the listing this verb rendered before the flag existed.
///
/// ⚠ That equality is the point of the test, not tidiness: `list_lines` grew a width that is
/// zero in the unasked case, and a regression there would silently add trailing whitespace to
/// every row of every `data hist list` anybody has ever piped into a diff.
#[test]
fn the_class_column_is_absent_unless_asked_and_changes_nothing_when_it_is() {
    let mut classified = bar_row();
    classified.class = Some(ClassProbe::Classified("CryptoPerp"));
    let mut grouped = grouped_row();
    grouped.class = Some(ClassProbe::Grouped);
    let rows = [classified, grouped];

    let without = list_lines(&rows, 2, false, false, false);
    assert!(!without[0].contains("CLASS"), "no header without the flag: {}", without[0]);
    assert!(!without[1].contains("CryptoPerp"), "…and no cell either: {}", without[1]);
    // The rows carry a probe, so this proves the RENDERER is gated and not merely the caller.
    assert_eq!(
        without,
        list_lines(&[bar_row(), grouped_row()], 2, false, false, false),
        "an unasked class may not change one byte of the listing"
    );

    let with = list_lines(&rows, 2, false, false, true);
    assert!(with[0].trim_end().ends_with("CLASS"), "the column is last: {}", with[0]);
    assert!(with[1].ends_with("CryptoPerp"), "{}", with[1]);
    assert!(with[2].ends_with("(group)"), "a grouped row was never asked: {}", with[2]);
    // The cells sit UNDER the header — the whole reason `last_w` is measured rather than
    // assumed, since a `LAST` column of varying width would otherwise ragged the one after it.
    let class_col = with[0].find("CLASS").expect("the header names the column");
    assert!(with[1][class_col..].starts_with("CryptoPerp"), "aligned: {}", with[1]);
    assert!(with[2][class_col..].starts_with("(group)"), "…both rows: {}", with[2]);
}

/// A failed probe degrades the ROW and leaves the listing whole — the same contract the gap
/// probe has, and the reason both are annotations rather than run failures.
#[test]
fn a_failed_class_probe_annotates_its_row_and_leaves_the_listing_whole() {
    let mut broken = bar_row();
    broken.class = Some(ClassProbe::Failed("properties_as_of: store unreadable".to_string()));
    let mut fine = grouped_row();
    fine.class = Some(ClassProbe::Unclassified);

    let lines = list_lines(&[broken, fine], 2, false, false, true);
    assert!(lines.iter().any(|l| l.contains("class unavailable")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("store unreadable")), "the server's words: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("BTCUSDT")), "the row survives: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("unclassified")), "…and so does its sibling");

    // Only the FAILURE gets a line — every other verdict is already a word in the row, which is
    // the asymmetry with `gap_lines` and the reason it is written down there.
    for probe in [ClassProbe::Classified("Fx"), ClassProbe::Unclassified, ClassProbe::Grouped] {
        let mut row = bar_row();
        row.class = Some(probe);
        assert!(class_error_line(&row).is_empty(), "{:?} needs no line", row.class);
    }
}

/// The `--json` wire: `asset_class_status` names WHICH answer each row got, and `asset_class`
/// is non-null for EXACTLY the `classified` verdict.
///
/// ⚠ This is the compatibility assertion the module doc argues for. Without the status field a
/// `null` class would mean four different things at once — not asked, no grid, a grid naming no
/// class, and a failed probe — and the last three are what an operator acts on differently.
#[test]
fn the_class_fields_are_a_status_and_a_word_that_cannot_disagree() {
    let args = parse_of(&["hist", "ls", "--class", "--json"]).unwrap();
    let mut rows = Vec::new();
    for probe in [
        ClassProbe::Classified("CryptoPerp"),
        ClassProbe::Unclassified,
        ClassProbe::Unrecorded,
        ClassProbe::Grouped,
        ClassProbe::Failed("store unreadable".to_string()),
    ] {
        let mut row = bar_row();
        row.class = Some(probe);
        rows.push(row);
    }
    let doc: serde_json::Value =
        serde_json::from_str(&list_json(&args, &rows, rows.len())).unwrap();

    assert_eq!(doc["class_requested"], true);
    let series = doc["series"].as_array().expect("an array of rows");
    let statuses: Vec<&str> =
        series.iter().map(|s| s["asset_class_status"].as_str().unwrap()).collect();
    assert_eq!(
        statuses,
        ["classified", "unclassified", "unrecorded", "grouped", "error"],
        "one status per outcome, and all five distinct"
    );
    for row in series {
        let classified = row["asset_class_status"] == "classified";
        assert_eq!(
            !row["asset_class"].is_null(),
            classified,
            "asset_class is non-null for exactly the classified verdict: {row}"
        );
        assert_eq!(
            !row["asset_class_error"].is_null(),
            row["asset_class_status"] == "error",
            "…and asset_class_error for exactly the error one: {row}"
        );
    }
    assert_eq!(series[0]["asset_class"], "CryptoPerp");
    assert_eq!(series[4]["asset_class_error"], "store unreadable");
}

/// WITHOUT `--class` the three keys are present and null on every row and `class_requested` is
/// false — the shape `gaps`/`gaps_requested` already has, so the addition is ADDITIVE for a
/// consumer that predates it and needs no new convention from one that does not.
#[test]
fn an_unasked_class_is_null_on_the_wire_and_says_it_was_not_asked() {
    let args = parse_of(&["hist", "ls", "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
    assert_eq!(doc["class_requested"], false);
    let row = &doc["series"][0];
    for key in ["asset_class", "asset_class_status", "asset_class_error"] {
        assert!(row.get(key).is_some(), "{key} is a KEY, so a consumer can read it uniformly");
        assert!(row[key].is_null(), "…and null, because nothing was asked: {row}");
    }
}

/// The document carries the RAW `symbol` and `group` beside the resolved `name`/`grouped`, so
/// a machine reader gets the identity rather than this side's reading of it — and the gap
/// ranges are objects rather than positional pairs.
#[test]
fn list_json_carries_the_raw_identity_beside_the_resolved_name() {
    let mut per_symbol = bar_row();
    per_symbol.gaps = Some(Vec::new());
    let mut grouped = grouped_row();
    grouped.gaps = Some(vec![(0, 86_400_000)]);
    // ⚠ `gaps`, not `ls --gaps`: the probe is DERIVED from the verb now, so this is also the
    // assertion that `gaps_requested` still follows it.
    let args = parse_of(&["hist", "gaps", "--venue", "poly", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&list_json(&args, &[per_symbol, grouped], 5)).unwrap();

    assert_eq!(doc["subcommand"], "gaps");
    assert_eq!(doc["addr"], DEFAULT_ADDR);
    assert_eq!(doc["filter"]["venue"], "poly");
    assert!(doc["filter"]["kind"].is_null(), "an unset filter dimension is null, not absent");
    assert_eq!(doc["gaps_requested"], true);
    assert_eq!(doc["series_reported"], 5);
    assert_eq!(doc["count"], 2);

    let per_symbol = &doc["series"][0];
    assert_eq!(per_symbol["kind"], "bar");
    assert_eq!(per_symbol["name"], "BTCUSDT");
    assert_eq!(per_symbol["symbol"], "BTCUSDT");
    assert!(per_symbol["group"].is_null());
    assert_eq!(per_symbol["grouped"], false);
    assert_eq!(per_symbol["interval"], "1h");
    assert_eq!(per_symbol["coverage"]["rows"], 48);
    assert!(per_symbol["gaps"].as_array().expect("the verb asked for them").is_empty());

    // ⚠ The grouped row is where a `VENUE:SYMBOL:INTERVAL` document would fall apart: its
    // symbol is EMPTY and its name comes from the group.
    let grouped = &doc["series"][1];
    assert_eq!(grouped["name"], "fam");
    assert_eq!(grouped["symbol"], "");
    assert_eq!(grouped["group"], "fam");
    assert_eq!(grouped["grouped"], true);
    assert!(grouped["interval"].is_null());
    assert_eq!(grouped["gaps"][0]["from_ts"], 0);
    assert_eq!(grouped["gaps"][0]["to_ts"], 86_400_000);
}

/// A series whose gaps were NOT asked for carries `null`, never an empty array — "nobody
/// asked" and "this series has no holes" are different facts and a fold over them differs.
#[test]
fn an_unasked_gap_field_is_null_rather_than_an_empty_array() {
    let args = parse_of(&["hist", "ls", "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&list_json(&args, &[bar_row()], 1)).unwrap();
    assert!(doc["series"][0]["gaps"].is_null());
    assert!(doc["series"][0]["gaps_error"].is_null());
    assert_eq!(doc["gaps_requested"], false);
}

/// The EMPTY answer's two causes are told apart, in both verbs' nouns.
#[test]
fn the_empty_answer_separates_an_empty_store_from_an_over_narrow_filter() {
    assert_eq!(
        list_lines(&[], 0, false, false, false),
        vec!["the datahub reported no series at all"]
    );
    assert_eq!(
        list_lines(&[], 12, true, false, false),
        vec!["no series match the filter (12 reported)"]
    );
    assert_eq!(coverage_lines(&[], 0, false), vec!["the datahub reported no instruments at all"]);
    assert_eq!(coverage_lines(&[], 4, true), vec!["no instruments match the filter (4 reported)"]);
}
