//! The READ half against a real loopback datahub: `ls`, `gaps`, `get` and `coverage`.

use super::support::{DAY_MS, DEAD, jsonl_rows, spawn_seeded_datahub};
use super::*;

/// A datahub that is not there is the CONNECT rung — the one a wrapper backs off and retries on,
/// and the same disposition (and the same sentence) `vike-cli backtest` gives for the same socket.
///
/// ⚠ The address is [`DEAD`], never a port this test bound and released: a released port does not
/// STAY free, and that is how this case reddened a green `main` (run 34941165673: `left: Some(0)`,
/// the CLI reached another test's listener on the port it had just let go of). A connect probe
/// around each run could only re-roll a stolen port, never keep this case's dial off the other
/// test's server. Port 1 cannot be handed to any test, and a box that SERVED it would redden this
/// case on every run, never pass it: a refusal cannot be "proved" against a listener.
#[test]
fn an_unreachable_datahub_is_the_connect_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    // ⚠ Whole ARGUMENT LISTS rather than verb names, because `gate` carries its own required
    // flags — and it is in this set for a reason stronger than symmetry. Its rung is its
    // PRODUCT, so a gate that answered `breach` for a socket that never opened would tell a CI
    // step its DATA is bad when its TUNNEL is down, which is the retry-vs-escalate inversion
    // this whole ladder exists to remove.
    for args in [
        vec!["data", "history", "ls"],
        vec!["data", "history", "coverage"],
        vec!["data", "history", "gate", "binance:BTCUSDT:1h", "--require-days", "1"],
    ] {
        let sub = args[2];
        let mut argv = args.clone();
        argv.extend_from_slice(&["--addr", DEAD]);
        let out = run(scratch.path(), &argv);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(3), "{sub} must be connect-class: {err}");
        assert!(err.contains("cannot connect to datahub"), "{sub}: {err}");
        assert!(err.contains(DEAD), "{sub} must name the address: {err}");
        assert_eq!(stdout(&out), "", "{sub} wrote a document for a run that never happened");
    }
}

/// THE plumbing for `list`: the verb reaches a real datahub's `inventory()` and carries every
/// dimension of what came back.
///
/// ⚠ The document carries the RAW `symbol` and `group` beside the derived `name`, which is the
/// property a `VENUE:SYMBOL:INTERVAL` rendering cannot have. Both of this fixture's series are
/// per-symbol (the in-memory double holds no grouped series), so the GROUPED half of that contract
/// is pinned by the unit tests beside the module; what this case proves is that the fields survive
/// the wire at all, and that a tick series' `interval` arrives as `null` rather than as an empty
/// string somebody invented on the way.
#[test]
fn list_reaches_a_real_datahub_and_carries_every_dimension() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "history", "ls", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["subcommand"], "ls");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["series_reported"], 2);
    assert_eq!(doc["count"], 2);
    assert_eq!(doc["gaps_requested"], false);

    // The store's enumeration is sorted by id, so `bar` precedes `properties`.
    let bars = &doc["series"][0];
    assert_eq!(bars["kind"], "bar");
    assert_eq!(bars["venue"], "binance");
    assert_eq!(bars["name"], "BTCUSDT");
    assert_eq!(bars["symbol"], "BTCUSDT");
    assert!(bars["group"].is_null());
    assert_eq!(bars["grouped"], false);
    assert_eq!(bars["interval"], "1h", "a bar series sub-partitions by its step");
    assert_eq!(bars["coverage"]["rows"], 3);
    assert_eq!(bars["coverage"]["first_timestamp"], 0);
    assert_eq!(bars["coverage"]["last_timestamp"], 2 * DAY_MS);
    assert!(bars["gaps"].is_null(), "`ls` asks no gap probe, so the field is null not empty");

    let ticks = &doc["series"][1];
    assert_eq!(ticks["kind"], "properties");
    assert_eq!(ticks["venue"], "okx");
    assert_eq!(ticks["name"], "BTC-USDT");
    assert!(ticks["interval"].is_null(), "a tick-shaped series genuinely has no interval");
    assert_eq!(ticks["coverage"]["rows"], 1);
}

/// The filter is applied CLIENT-SIDE to what the server sent, and the document carries BOTH counts
/// — which is what lets a caller tell an empty store from an over-narrow filter in one call.
#[test]
fn the_list_filter_narrows_the_rows_and_both_counts_are_reported() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(
        scratch.path(),
        &["data", "history", "ls", "--addr", &addr, "--venue", "OKX", "--json"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["count"], 1, "case-insensitive substring on the venue");
    assert_eq!(doc["series_reported"], 2, "…beside what the server actually reported");
    assert_eq!(doc["series"][0]["venue"], "okx");
    assert_eq!(doc["filter"]["venue"], "OKX", "the filter is echoed as typed");

    // A filter that matches nothing is a successful run with an honest empty, never a failure.
    let out = run(scratch.path(), &["data", "history", "ls", "--addr", &addr, "--name", "NOSUCH"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no series match the filter (2 reported)"), "{}", stdout(&out));
}

/// `data history gaps` reaches `series_gaps` per matched series, and an EMPTY answer is rendered as
/// an answered "no gaps" rather than as silence — the distinction the verb exists to draw.
///
/// ⚠ This is also the end-to-end proof of the PROMOTION: the probe now follows the VERB, so the
/// document's `gaps_requested` is `true` with no flag on the line at all, and the same filters
/// still narrow it.
#[test]
fn gaps_are_fetched_per_series_and_an_empty_answer_is_said_out_loud() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "history", "gaps", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["subcommand"], "gaps");
    assert_eq!(doc["gaps_requested"], true, "the VERB armed the probe, with no flag on the line");
    for i in 0..2 {
        let gaps = doc["series"][i]["gaps"].as_array().expect("the verb asked for them");
        assert!(gaps.is_empty(), "series {i} reported holes it does not have: {gaps:?}");
        assert!(doc["series"][i]["gaps_error"].is_null(), "series {i}");
    }

    let out = run(scratch.path(), &["data", "history", "gaps", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).matches("no gaps").count(),
        2,
        "each matched series gets an explicit verdict: {}",
        stdout(&out)
    );

    // ...and the filters `ls` takes narrow it identically — the property that let this become a
    // verb without building a series identity out of flags.
    let out = run(scratch.path(), &["data", "history", "gaps", "--addr", &addr, "--venue", "OKX"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out).matches("no gaps").count(),
        1,
        "the venue filter reached the probe: {}",
        stdout(&out)
    );
}

/// The human table's identity columns, against a real answer: `kind` and `SCOPE` are their own
/// cells and nothing is joined into a colon-string.
#[test]
fn the_human_listing_renders_columns_and_never_a_colon_string() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "history", "ls", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for token in ["KIND", "VENUE", "SCOPE", "NAME", "INTERVAL", "ROWS", "DAYS", "FIRST", "LAST"] {
        assert!(text.contains(token), "the header must carry {token}: {text}");
    }
    assert!(text.contains("bar") && text.contains("properties"), "both kinds are rows: {text}");
    assert!(text.contains("symbol"), "the scope cell says which alternative the name is: {text}");
    assert!(!text.contains("binance:BTCUSDT"), "no colon-string identity: {text}");
    assert!(text.trim_end().ends_with("2 series · 4 rows"), "the summary line: {text}");
}

/// **THE ROW VERB, END TO END** — `data history get` reaches a real datahub's `LoadBars` through
/// `DatahubClient::load_bars` and prints the rows themselves.
///
/// ⚠ **What this proves that no unit test can**: the `TimestampRange` read genuinely talks to a
/// server built from the shipping protocol, with no wire change — the claim the surface design's
/// §8.2 makes and the whole reason this verb needed no protocol arm. The bounds are asserted by
/// NARROWING as well as by matching, because a verb that dropped them on the floor would answer
/// with all three bars and satisfy every "the rows arrived" assertion.
#[test]
fn get_reaches_the_rows_over_the_epoch_ms_sibling_and_the_window_bounds_the_read() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let last = (2 * DAY_MS).to_string();

    let out = run(
        scratch.path(),
        &[
            "data",
            "history",
            "get",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--from",
            "0",
            "--to",
            &last,
            "--json",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["subcommand"], "get");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["spec"]["text"], "binance:BTCUSDT:1h");
    assert_eq!(doc["returned"], 3, "the whole seeded series is inside this window");
    assert_eq!(doc["shown"], 3);
    assert_eq!(doc["truncated"], false);
    let bars = doc["bars"].as_array().expect("an array of rows");
    assert_eq!(bars.len(), 3);
    assert_eq!(bars[0]["timestamp"], 0);
    assert_eq!(bars[0]["close"], 1.5, "the PRICE itself, which no sibling verb renders");
    assert_eq!(bars[0]["venue"], "binance", "every row is self-describing");
    assert!(bars[0].get("bid").is_none(), "an unrecorded tick-derived field is OMITTED");

    // THE ANTI-VACUITY CONTROL: the same series over a ONE-DAY window answers with one row, so the
    // bounds above genuinely crossed the wire rather than being parsed and dropped.
    let one = DAY_MS.to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "history",
            "get",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--from",
            &one,
            "--to",
            &one,
            "--json",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["returned"], 1, "the window narrowed the READ: {}", stdout(&out));
    assert_eq!(doc["bars"][0]["timestamp"], DAY_MS);
}

/// **§8.2 RULE 1, ON THE SHIPPED BINARY**: hitting the row ceiling is REPORTED with the exact count
/// withheld, and a `--limit` above the ceiling is refused rather than clamped.
#[test]
fn the_row_ceiling_is_disclosed_and_a_limit_above_it_is_the_usage_rung() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    // ⚠ `--from 0` rather than `--days N`: a half-open window from the epoch reaches this
    // fixture's bars whatever the wall clock says, so the case pins the CEILING rather than
    // accidentally pinning how far back a day count happens to reach in the year it is run.
    let spec_and_window =
        ["data", "history", "get", "binance:BTCUSDT:1h", "--addr", &addr, "--from", "0"];

    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", "2"]);
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "a cut answer is a successful run: {}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(
        text.lines().filter(|l| l.contains("1970-01-0")).count(),
        2,
        "cut to --limit: {text}"
    );
    assert!(text.contains("1 more rows"), "the EXACT count withheld is disclosed: {text}");
    assert!(text.contains("--limit 2"), "…and WHICH ceiling cut it: {text}");
    assert!(text.contains("export"), "…and the verb bulk extraction belongs to: {text}");

    // The machine form carries the same fact as FIELDS rather than as a note.
    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", "2", "--json"]);
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["returned"], 3);
    assert_eq!(doc["shown"], 2);
    assert_eq!(doc["truncated"], true);

    // ...and a limit ABOVE the ceiling is a command-line refusal, never a silent clamp — which is
    // the half of the rule an operator would otherwise never learn they had hit.
    //
    // ⚠ **NEITHER the argv NOR a bare `1000` can be what this asserts on, and it shipped asserting
    // on both.** The case read `--limit 100000` + `contains("1000")`, and `100000` CONTAINS
    // `1000` — so the argv echoed back into the refusal satisfied it on its own. So did the USAGE
    // page, which `crate::cmd::args`'s `finish` prints on the SAME stream for every
    // parse error and which states the ceiling twice (`at most 1000`, `(default 1000)`). Three
    // sources, one of them the thing under test. The value below carries none of the ceiling's
    // digits and the needle is the refusal's own phrase; the two controls pin both hazards.
    const ABOVE_THE_CEILING: &str = "4096";
    const NAMED: &str = "ceiling of 1000 rows";
    assert!(
        !ABOVE_THE_CEILING.contains("1000"),
        "the argv must not itself supply the literal this case asserts on"
    );
    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", ABOVE_THE_CEILING]);
    let out = run(scratch.path(), &argv);
    let refused = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "a refused command line is the usage rung: {refused}");
    assert!(refused.contains(NAMED), "the ceiling is NAMED by the refusal: {refused}");
    assert!(
        refused.contains(ABOVE_THE_CEILING),
        "…and so is what WAS asked for, which is the half that makes a clamp unbelievable: \
         {refused}"
    );
    // THE CONTROL: a DIFFERENT `--limit` refusal, which prints the very same USAGE page on the
    // very same stream, does NOT carry that phrase. Without it the assertion above would still
    // pass off the usage text if the refusal ever stopped naming the number.
    let mut argv = spec_and_window.to_vec();
    argv.extend(["--limit", "seven"]);
    let out = run(scratch.path(), &argv);
    let other = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "…still a usage rung: {other}");
    assert!(other.contains("whole number"), "a different refusal, same page: {other}");
    assert!(!other.contains(NAMED), "the phrase is the REFUSAL's, not the usage page's: {other}");
}

/// Under `--format jsonl`, STDOUT CARRIES ROWS AND NOTHING ELSE — the property a `| jq` and a
/// `> rows.jsonl` both depend on.
///
/// ⚠ The disclosures still happen; they go to STDERR. A run that simply dropped them would pass a
/// "stdout is only rows" assertion just as well, so both streams are asserted.
#[test]
fn a_jsonl_get_puts_rows_on_stdout_and_its_notes_on_stderr() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(
        scratch.path(),
        &[
            "data",
            "history",
            "get",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--from",
            "0",
            "--limit",
            "2",
            "--format",
            "jsonl",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let rows = jsonl_rows(&stdout(&out));
    assert_eq!(rows.len(), 2, "one object per row and nothing else on stdout");
    assert_eq!(rows[0]["interval"], "1h");
    assert!(stderr(&out).contains("1 more rows"), "the note went to stderr: {}", stderr(&out));
}

/// An EMPTY answer never claims the series is absent, because the wire cannot tell the two apart —
/// and it is a SUCCESS, like every other honest empty in this plane.
///
/// ⚠ **All THREE renderings, because the one a script reads was the one that said nothing.** The
/// `json` arm shipped printing `returned: 0, bars: []` and exiting 0 with nothing on either
/// stream, while `jsonl` — equally machine-facing — put the note on stderr. A consumer reads that
/// document as "the store has no bars for that week" when the truth is "that series is not in this
/// store", which is the exact misreading the note exists to prevent.
#[test]
fn a_get_that_matches_nothing_states_both_readings_and_still_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let base = ["data", "history", "get", "binance:NOSUCH:1h", "--addr", &addr, "--from", "0"];

    let out = run(scratch.path(), &base);
    assert!(out.status.success(), "an empty window is a fact, not a failure: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("ONE of two facts"), "{text}");
    assert!(text.contains("data history ls"), "…and the verb that answers the other: {text}");

    // The DOCUMENT form carries it as a field — stdout stays exactly one JSON document.
    let mut argv = base.to_vec();
    argv.push("--json");
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["returned"], 0);
    let note = doc["note"].as_str().unwrap_or_else(|| panic!("a `note` field: {doc}"));
    assert!(note.contains("ONE of two facts"), "the ambiguity is a FIELD: {note}");
    assert!(note.contains("data history ls"), "…naming the verb that answers the other: {note}");

    // The SEQUENCE form carries it on stderr, because stdout is rows and only rows.
    let mut argv = base.to_vec();
    argv.extend(["--format", "jsonl"]);
    let out = run(scratch.path(), &argv);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "", "no rows means no stdout at all under jsonl");
    assert!(stderr(&out).contains("ONE of two facts"), "{}", stderr(&out));

    // THE CONTROL: a series the fixture DOES hold carries no note in any of the three, so every
    // assertion above is about the emptiness rather than about a sentence printed unconditionally.
    let held = ["data", "history", "get", "binance:BTCUSDT:1h", "--addr", &addr, "--from", "0"];
    let out = run(scratch.path(), &held);
    assert!(!stdout(&out).contains("ONE of two facts"), "{}", stdout(&out));
    let mut argv = held.to_vec();
    argv.push("--json");
    let out = run(scratch.path(), &argv);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert!(doc["returned"].as_u64().is_some_and(|n| n > 0), "the fixture holds this one: {doc}");
    assert!(doc.get("note").is_none(), "a note on every run stops being read: {doc}");
}

/// `coverage` reaches the cross-kind verb and renders an EMPTY report honestly.
///
/// ⚠ What is pinned here is the RENDERING, not the report. The in-memory double inherits
/// `HistoryStore::coverage_report`'s default — an empty `Ok` — so this proves the request reaches the
/// server and comes back, and that a zero-row answer reads as a sentence rather than as a silently
/// blank table. A report with rows IN it is folded by the unit tests beside the module, which is
/// where it can be done without a `HistoryStoreDataFusion` this crate may not link.
#[test]
fn coverage_reaches_the_cross_kind_verb_and_renders_an_empty_report_honestly() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();

    let out = run(scratch.path(), &["data", "history", "coverage", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["subcommand"], "coverage");
    assert_eq!(doc["addr"], addr);
    assert_eq!(doc["instruments_reported"], 0);
    assert_eq!(doc["count"], 0);
    assert!(doc["instruments"].as_array().expect("an array, even when empty").is_empty());

    let out = run(scratch.path(), &["data", "history", "coverage", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("the datahub reported no instruments at all"),
        "an empty report is a sentence, not a blank table: {}",
        stdout(&out)
    );
}
