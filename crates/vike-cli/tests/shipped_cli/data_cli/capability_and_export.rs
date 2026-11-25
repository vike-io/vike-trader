//! The keyed-datahub capability refusal, the withheld account series and the remote `export`.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::Output;
use std::sync::Arc;
use std::thread;

use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_datahub::{serve, serve_authed};
use vike_hub_protocol::auth::HubKeys;
use vike_marketdata::TickQuote;

use super::support::{
    DAY_MS, DEAD, fill, spawn_seeded_datahub, spawn_shared_account_store_datahub,
};
use super::*;

/// A datahub that REQUIRES authentication — it holds hub keys, so its `Welcome` advertises
/// `auth` and every connection must complete the mac handshake before a verb is answered.
///
/// A fixture of its own, for [`spawn_shared_account_store_datahub`]'s stated reason and one more:
/// this is the only server here that REFUSES a caller, and the caller under test is a `vike-cli`
/// run holding no keys at all. The store is empty on purpose — nothing in this fixture's cases
/// reaches a verb, which is the whole point of it.
///
/// ⚠ This opened "A FOURTH fixture", and the NUMBER was the defect rather than the sentence: a
/// hand-maintained count in prose that nothing holds in step, so a fixture landing above it makes
/// this doc silently wrong with no test able to notice — the same rot
/// `crates/vike-cli/tests/cli_verbs/message_literals.rs`'s `source_files` doc had to correct in its own
/// measurement. The distinguishing property is what carries the argument, and unlike an ordinal it
/// is a property rather than a position.
fn spawn_keyed_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let keys = HubKeys::new(b"an-observe-key".to_vec(), b"a-control-key".to_vec());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, Some(keys), None, None, None);
    });
    addr
}

/// A store holding one quote of a series and one of its `recorder` lane, told apart by the bid. A double,
/// because this crate links no engine; the real store's read is `vike-store`'s and the datahub's suites'.
struct LaneStore;

impl LaneStore {
    fn quote(bid: f64) -> Vec<TickQuote> {
        let symbol = Default::default();
        vec![TickQuote {
            timestamp: 1,
            local_timestamp: 0,
            bid,
            ask: 2.0,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol,
        }]
    }
}

impl HistoryStore for LaneStore {
    fn scan_quotes_capped(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TimestampRange,
        _budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        Ok(Self::quote(1.1))
    }
    fn scan_quotes_sourced_capped(
        &self,
        _venue: &str,
        source: &str,
        _symbol: &str,
        _range: TimestampRange,
        _budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        assert_eq!(source, "recorder");
        Ok(Self::quote(1.2))
    }
    vike_data::hist_store_stubs!(inert: all);
}

/// **`--source` reaches the wire: the lane's rows land in the file, and the line without it writes the
/// series'.** The whole verb, from the command line to the file, against a real server.
#[test]
fn a_remote_export_reads_a_lane_only_when_the_line_names_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    thread::spawn(move || {
        let _ = serve(listener, Arc::new(LaneStore));
    });

    let bids = |name: &str, lane: &[&str]| -> Vec<f64> {
        let file = scratch.path().join(name);
        let mut argv = vec![
            "data",
            "history",
            "export",
            "dukascopy:EURUSD",
            "--kind",
            "quote",
            "--addr",
            &addr,
            "--format",
            "jsonl",
            "--from",
            "1",
            "--to",
            "2",
            "--out",
            file.to_str().expect("a UTF-8 scratch path"),
        ];
        argv.extend(lane.iter().copied());
        let out = run(scratch.path(), &argv);
        assert!(out.status.success(), "{}", stderr(&out));
        std::fs::read_to_string(&file)
            .expect("the file")
            .lines()
            .map(|l| {
                let row: serde_json::Value = serde_json::from_str(l).expect("one object per line");
                row["bid"].as_f64().expect("bid")
            })
            .collect()
    };
    assert_eq!(bids("series.jsonl", &[]), [1.1]);
    assert_eq!(bids("tape.jsonl", &["--lane", "recorder"]), [1.2]);
}

/// A store whose depth lane holds two recorded snapshots, one minute apart, and whose lossless book lane
/// holds nothing.
struct DepthStore;

impl HistoryStore for DepthStore {
    fn scan_depth(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<vike_marketdata::L2BookUpdate>, HistoryStoreError> {
        use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind};
        let snapshot = |timestamp: i64, sequence: u64, bid: f64| L2BookUpdate {
            timestamp,
            local_timestamp: timestamp + 5,
            sequence,
            kind: L2BookUpdateKind::Snapshot,
            tick_size: 0.1,
            bids: vec![L2BookLevel::new(bid, 1.0), L2BookLevel::new(bid - 1.0, 3.0)],
            asks: vec![L2BookLevel::new(bid + 1.0, 2.0)],
            symbol: String::new(),
        };
        let inside =
            |t: i64| range.start.is_none_or(|s| t >= s) && range.end.is_none_or(|e| t <= e);
        Ok([snapshot(60_000, 7, 100.0), snapshot(120_000, 8, 100.5)]
            .into_iter()
            .filter(|u| inside(u.timestamp))
            .collect())
    }
    vike_data::hist_store_stubs!(inert: all);
}

/// **A recorded order book exports like bars**: `--kind depth` walks the depth lane window by window and
/// writes one row per recorded event, as recorded; `--levels 1` keeps the best level a side; `--kind
/// book` reads the OTHER lane (empty here).
#[test]
fn a_remote_export_writes_a_recorded_book_one_event_per_row() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    thread::spawn(move || {
        let _ = serve(listener, Arc::new(DepthStore));
    });
    let export = |kind: &str, name: &str, extra: &[&str]| -> String {
        let file = scratch.path().join(name);
        let mut argv = vec![
            "data",
            "history",
            "export",
            "binance:BTCUSDT.P",
            "--kind",
            kind,
            "--addr",
            &addr,
            "--format",
            "jsonl",
            "--from",
            "1",
            "--to",
            "150000",
            "--window",
            "1m",
            "--out",
            file.to_str().expect("a UTF-8 scratch path"),
        ];
        argv.extend_from_slice(extra);
        let out = run(scratch.path(), &argv);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(stdout(&out).contains("(3 windows)"), "{}", stdout(&out));
        std::fs::read_to_string(&file).expect("the file")
    };
    let rows = |text: &str| -> Vec<serde_json::Value> {
        text.lines().map(|l| serde_json::from_str(l).expect("one object per line")).collect()
    };
    let depth = export("depth", "depth.jsonl", &[]);
    let all = rows(&depth);
    assert_eq!(all.len(), 2, "{depth}");
    assert_eq!(all[0]["sequence"], 7);
    assert_eq!(all[0]["event"], "Snapshot");
    assert_eq!(all[0]["local_timestamp"], 60_005);
    assert_eq!(all[1]["bids"], serde_json::json!([[100.5, 1.0], [99.5, 3.0]]));
    assert_eq!(all[1]["asks"], serde_json::json!([[101.5, 2.0]]));
    // `--levels 1`: the same events, the best level a side, their time and sequence kept.
    let top = rows(&export("depth", "top.jsonl", &["--levels", "1"]));
    assert_eq!(top.len(), 2);
    assert_eq!(
        (top[1]["sequence"].clone(), top[1]["timestamp"].clone()),
        (8.into(), 120_000.into())
    );
    assert_eq!(top[1]["bids"], serde_json::json!([[100.5, 1.0]]));
    assert_eq!(top[1]["asks"], serde_json::json!([[101.5, 2.0]]));
    assert_eq!(export("book", "book.jsonl", &[]), "", "the lossless lane holds nothing here");
}

/// **A SERVED REFUSAL IS NOT AN ABSENT DATAHUB — through the CALL SITE, against a real server.**
///
/// ⚠ **This is the case that was missing, and the gap had a precise shape.** The refused path was
/// covered by two unit tests that between them never met each other: one over the CLASSIFIER
/// (`vike_hub_protocol::call::HubCallError::failure_outcome` over its variants) and one over the
/// RENDERER (a hand-built `ServerView::Refused`). Nothing ran `ask_the_server`, so inverting its
/// guard to `Err(e) if e.failure_outcome() != FailureOutcome::Refused` — or reverting it to fold
/// every failure into `connect` again — left both of them green while a PROTO_VERSION skew rendered
/// as `NOT REACHED`, the exact defect the third `ServerView` variant was added for.
///
/// The server here refuses for the commonest reason in production: it is KEYED and this run holds
/// no key at all (the scratch directory holds no store, so no device key), so `DatahubClient::connect`
/// completes the handshake and then answers `HubCallError::AccessDenied`. The CLI must still answer 0 with its
/// whole local matrix.
///
/// ⚠ [`run`] removes the retired datahub pair from the CHILD's environment, and its doc carries why:
/// an exported one refuses the start, so the case would never reach the path named above.
#[test]
fn the_capability_matrix_says_which_side_refused_when_a_server_answers_and_says_no() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_keyed_datahub().to_string();

    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr]);
    assert!(out.status.success(), "a refusal is REPORTED, never fatal: {}", stderr(&out));
    let body = stdout(&out);
    assert!(body.contains("REACHED, and it REFUSED"), "{body}");
    assert!(
        !body.contains("NOT REACHED"),
        "a server that answered the handshake may not be rendered as an absent one: {body}"
    );
    // The local half is untouched — the verb still answers whoever is on that port.
    for venue in vike_model::VENUES {
        assert!(body.contains(venue), "every roster venue must be rendered: {body}");
    }
    // ⚠ The needle is the string the ANSWERED arm's capability loop renders for a `None` — see the
    // unit twin `a_server_that_answered_and_refused_is_not_rendered_as_an_absent_one`. This
    // asserted `not served`, which `venues_lines` renders in no arm at all, so it could not fail:
    // the mutation it is here for (moving that loop out of the answered arm) prints
    // `was not asked for`, the very claim about the far side this branch deleted.
    assert!(
        !body.contains("was not asked for"),
        "nothing was read here, so no capability row may be rendered — least of all as unasked: \
         {body}"
    );
    assert_eq!(body.matches(&addr).count(), 1, "the address is named once on that line: {body}");

    // …and the DOCUMENT carries the three-state token rather than a boolean that would have to
    // call this server unreachable.
    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["server"]["state"], "refused");
    assert!(doc["server"]["features"].is_null(), "a discarded handshake advertised nothing: {doc}");

    // THE ANTI-VACUITY CONTROL: a KEYLESS server on the same code path is `answered`, so the
    // assertions above are about this server's refusal and not about every server.
    let open = spawn_seeded_datahub().to_string();
    let out = run(scratch.path(), &["data", "catalog", "venues", "--addr", &open, "--json"]);
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout is one JSON document");
    assert_eq!(doc["server"]["state"], "answered", "{doc}");
}

/// **A datahub that ANSWERED and refused this box is not the retry rung**: a `data` verb against a keyed
/// datahub, with no key here, exits `Exit::Failed` (1), never the `Exit::Connect` (3) a wrapper would
/// retry forever; a datahub that is not there stays 3.
#[test]
fn a_data_verb_refused_by_a_keyed_datahub_exits_failed_not_connect() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let keyed = spawn_keyed_datahub().to_string();
    let out = run(scratch.path(), &["data", "history", "ls", "--addr", &keyed]);
    assert_eq!(out.status.code(), Some(1), "a refusal is final: {}", stderr(&out));
    assert!(stderr(&out).contains("REQUIRES authentication"), "{}", stderr(&out));
    let out = run(scratch.path(), &["data", "history", "ls", "--addr", DEAD]);
    assert_eq!(out.status.code(), Some(3), "nothing answered: {}", stderr(&out));
}

/// A datahub over a store holding ONLY this account's own activity — a fill and no bars.
///
/// ⚠ Its own fixture rather than a flag on [`spawn_shared_account_store_datahub`], and the reason
/// is the case below: the combination under test is "the spec matched something, and everything it
/// matched was withheld", which needs a store with NO market series at all. It is the only store
/// here that has none — every other fixture in this file seeds bars. (This opened "A FIFTH
/// fixture"; see [`spawn_keyed_datahub`] for why the ordinal went and the property stayed.)
fn spawn_account_only_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = HistoryStoreMemory::new();
    store.append_exec_fills("binance", "BTCUSDT", &[fill(DAY_MS)], None).expect("seed a fill");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// **THE RUNG THE EXCLUSION MOVES.** A spec that matches account series and NOTHING ELSE is the
/// nothing-was-evaluated rung (`7`), and the refusal says what it withheld — because the operator
/// can see those series with their own eyes and "matches none of them" alone is a refusal they can
/// disprove in one command.
///
/// ⚠ **This combination was reached by no test at any level, and it is the one place the exclusion
/// changes an EXIT CODE.** Before the exclusion this store answered `6` (a breach: the series was
/// found and failed the day threshold); it answers `7` now. The withheld note is stitched into
/// `CliError::empty`'s message at exactly one site, and every other case in this file seeds bars
/// beside the fill — so dropping that stitching, or losing the `\n` that puts the note on its own
/// line, changed nothing any test could see.
///
/// The ANTI-VACUITY control is the last block: the SAME spec against a store with market data
/// answers a different rung entirely, so this case is about the withholding rather than about the
/// spec being unmatchable.
#[test]
fn a_spec_matching_only_withheld_account_series_is_the_empty_rung_and_names_what_it_withheld() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_account_only_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "history", "gate", "binance:BTCUSDT", "--require-days", "1", "--addr", &addr],
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "everything this spec matched was withheld, so NOTHING was evaluated — and that is not a \
         pass: {}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "", "no criteria were judged, so there is no verdict to print");

    let err = stderr(&out);
    assert!(err.contains("nothing to gate"), "{err}");
    // THE PROPERTY: the note is a LINE of its own, not a clause welded onto the end of the
    // sentence above it. That is what the `\n` in the stitching buys, and it is invisible to a
    // `contains` over the whole message.
    let note = err
        .lines()
        .find(|l| l.starts_with("note: "))
        .unwrap_or_else(|| panic!("the withholding must be DISCLOSED, on its own line: {err}"));
    assert!(note.contains("not part of this answer"), "{note}");
    assert!(note.contains("exec_fill"), "…naming the kind it withheld: {note}");
    assert!(note.contains("vike-cli account"), "…and the plane that will serve it: {note}");

    // The ANTI-VACUITY control: the same spec over a store that DOES hold market data reaches the
    // judging half instead, so the rung above is the withholding and not an unmatchable spec.
    let shared = spawn_shared_account_store_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "history", "gate", "binance:BTCUSDT", "--require-days", "1", "--addr", &shared],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("PASS"), "{}", stdout(&out));
}

/// **THE WALK, ON THE SHIPPED BINARY** — `export --addr --format jsonl` streams a remote store's
/// rows into `--out`, and the file it writes is the same whether the range took one request or
/// three.
///
/// ⚠ **That last clause is the whole test.** Every other property here (the file exists, it holds
/// rows, the rows are JSON) would pass a one-shot implementation that never walked at all. What
/// only a correct walk gives is INVARIANCE UNDER THE STEP: `Request::LoadBars`' bounds are
/// INCLUSIVE on both ends, so a window that began where the previous one ended would duplicate
/// every row landing on a boundary.
///
/// ⚠⚠ **`--from 1` RATHER THAN `--from 0`, AND THE REASON IS A MEASUREMENT — THIS CASE FAILED TO
/// FAIL ONCE ALREADY.** The first version walked `[0, 2d]` in `1d` steps and its comment claimed a
/// boundary row was "EVERY row". It is none of them: a window ends at `lo + step - 1`, so with
/// `--from 0` the ends fall on `1d-1` and `2d-2`, where this fixture has no bar — and the
/// kill-proof (mutating `windows`' `lo = hi.saturating_add(1)` to `lo = hi` in
/// `crates/vike-cli/src/cmd/data/hist/export.rs`) left this test GREEN while the unit case reddened.
/// Starting at `1` moves the first window's end to exactly `1d`, where a bar IS, so the mutation
/// re-fetches it and the two files differ. Re-verified: mutated → FAIL, reverted → PASS.
#[test]
fn a_remote_export_streams_rows_and_the_file_does_not_depend_on_the_step() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let walked = scratch.path().join("walked.jsonl");
    let one_shot = scratch.path().join("one-shot.jsonl");
    // ⚠ Fixed epoch bounds, so nothing depends on the wall clock — and `1` rather than `0` for the
    // boundary reason above, which costs the `ts=0` bar and buys the only assertion here that can
    // fail for its stated reason.
    let to = (2 * DAY_MS).to_string();

    let argv = |out: &Path, window: &str| -> Vec<String> {
        [
            "data",
            "history",
            "export",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--format",
            "jsonl",
            "--from",
            "1",
            "--to",
            &to,
            "--window",
            window,
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain(["--out".to_string(), out.display().to_string()])
        .collect()
    };
    // A `fn` rather than a closure: a closure's inferred signature ties the borrow of `v` to the
    // borrow of the returned slice, which does not typecheck here.
    fn as_args(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    // TWO windows: `1d` over the `[1, 2d]` inclusive range tiles as [1,1d] [1d+1,2d] — and the
    // first one's END is exactly where a bar sits, which is the property the whole case turns on.
    let v = argv(&walked, "1d");
    let out = run(scratch.path(), &as_args(&v));
    assert!(out.status.success(), "{}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains("2 windows"), "the walk reports how many steps it took: {said}");
    assert!(said.contains("2 bar rows"), "…and how many rows landed: {said}");

    // ONE window: the same range in a single request.
    let v = argv(&one_shot, "30d");
    let out = run(scratch.path(), &as_args(&v));
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("1 window)"), "{}", stdout(&out));

    let a = std::fs::read_to_string(&walked).expect("the walked file");
    let b = std::fs::read_to_string(&one_shot).expect("the one-shot file");
    assert_eq!(a, b, "the step may not change the ROWS — an inclusive-bound off-by-one duplicates");

    // ...and the rows are the two bars inside that range, ascending, self-describing. The `ts=0`
    // bar is OUTSIDE it by one millisecond, which is the cost of the boundary alignment above.
    let rows: Vec<serde_json::Value> =
        a.lines().map(|l| serde_json::from_str(l).expect("one object per line")).collect();
    assert_eq!(rows.len(), 2, "{a}");
    assert_eq!(
        rows.iter().map(|r| r["timestamp"].as_i64().expect("timestamp")).collect::<Vec<_>>(),
        vec![DAY_MS, 2 * DAY_MS]
    );
    for r in &rows {
        assert_eq!(r["venue"], "binance", "every row carries its series: {r}");
        assert_eq!(r["interval"], "1h");
        // ANTI-VACUITY for the equality above: the rows are not empty objects, and the OPTIONAL
        // columns are ABSENT rather than null — the shape `get --format jsonl` emits.
        assert_eq!(r["close"], 1.5);
        assert!(r.get("bid").is_none(), "an unrecorded bid is an ABSENT key, never null: {r}");
    }
}

/// The CSV half, and the three decisions it had to make — a header that always lands, minimal
/// quoting, an EMPTY field for a missing value — proven on the shipped binary rather than in a unit.
#[test]
fn a_remote_csv_export_writes_a_header_and_one_line_per_row() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out_path = scratch.path().join("btc.csv");
    let to = (2 * DAY_MS).to_string();
    let path = out_path.display().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "history",
            "export",
            "binance:BTCUSDT:1h",
            "--addr",
            &addr,
            "--format",
            "csv",
            "--from",
            "0",
            "--to",
            &to,
            "--out",
            &path,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let text = std::fs::read_to_string(&out_path).expect("the csv file");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "one header plus three rows: {text}");
    assert!(
        lines[0].starts_with("venue,symbol,interval,timestamp,timestamp_utc,open,"),
        "{}",
        lines[0]
    );
    // THE WIDTH INVARIANT: a row that lost a column would shift every field to its right, which is
    // the one CSV failure a reader cannot detect.
    let width = lines[0].split(',').count();
    for l in &lines[1..] {
        assert_eq!(l.split(',').count(), width, "row width must match the header: {l}");
    }
    // The NULL SPELLING: this fixture's bars carry no bid/ask/funding, so those three columns are
    // EMPTY rather than the word `null` — which would parse as text and poison the column.
    assert!(lines[1].ends_with(",,,"), "absent values are empty fields: {}", lines[1]);
    // ANTI-VACUITY: the same row's PRESENT values are not empty, so the check above is about
    // absence rather than about every field being blank.
    assert!(lines[1].starts_with("binance,BTCUSDT,1h,0,"), "{}", lines[1]);
}

/// The two refusals this route carries that a reader is likeliest to meet, on the shipped binary:
/// `--format parquet` over `--addr`, and a bulk range with only one bound.
///
/// ⚠ Each must name a COMMAND LINE that works rather than a phase or a diagnosis — the rule the
/// rest of this plane's refusals already follow.
#[test]
fn the_remote_export_refusals_name_a_command_that_works() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let path = scratch.path().join("x").display().to_string();
    let base = ["data", "history", "export", "binance:BTCUSDT:1h", "--addr", &addr, "--out", &path];

    let mut argv = base.to_vec();
    argv.extend(["--format", "parquet", "--from", "0", "--to", "1"]);
    let out = run(scratch.path(), &argv);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("drop --addr"), "the route that DOES write parquet: {err}");
    assert!(err.contains("backend-agnostic"), "…and the measurement, not a phase: {err}");
    assert!(
        err.contains("`--format parquet` is not served over --addr."),
        "…which is not the same as unbuilt: {err}"
    );

    let mut argv = base.to_vec();
    argv.extend(["--format", "jsonl", "--from", "0"]);
    let out = run(scratch.path(), &argv);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("BOTH --from and --to"), "{err}");
    assert!(
        err.contains("data history ls --venue binance --name BTCUSDT"),
        "…and where to read the \
                                                                         span: {err}"
    );

    // ANTI-VACUITY for both: the same line with the missing piece supplied SUCCEEDS, so each
    // refusal is about what it names rather than about the fixture being unreachable.
    let mut argv = base.to_vec();
    argv.extend(["--format", "jsonl", "--from", "0", "--to", "1"]);
    assert!(run(scratch.path(), &argv).status.success());
}

/// A store whose first quote window answers and whose every later one fails, so a walk fails AFTER
/// rows have landed. A double, for [`LaneStore`]'s reason.
struct DiesAfterTheFirstWindow;

impl HistoryStore for DiesAfterTheFirstWindow {
    fn scan_quotes_capped(
        &self,
        _venue: &str,
        _symbol: &str,
        range: TimestampRange,
        _budget: Option<usize>,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        if range.start == Some(1) {
            Ok(LaneStore::quote(1.1))
        } else {
            Err(HistoryStoreError::Query("the disk went away".to_string()))
        }
    }
    vike_data::hist_store_stubs!(inert: all);
}

/// The sorted file names in `dir`: what a failed export must not have added to.
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the export directory")
        .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// One remote quote export of two `1d` windows from `addr` into `out`.
fn export_quotes(settings: &Path, addr: &str, out: &Path) -> Output {
    let to = (2 * DAY_MS).to_string();
    let out = out.display().to_string();
    run(
        settings,
        &[
            "data",
            "history",
            "export",
            "dukascopy:EURUSD",
            "--kind",
            "quote",
            "--addr",
            addr,
            "--format",
            "jsonl",
            "--from",
            "1",
            "--to",
            &to,
            "--window",
            "1d",
            "--out",
            &out,
        ],
    )
}

/// **A FAILED REMOTE EXPORT LEAVES `--out` BYTE-IDENTICAL, WITH NOTHING BESIDE IT.** Measured on the
/// Windows client on 2026-11-07: a `File::create` before the dial took a 9-byte file to 0 on a
/// refused key and on a dead address, so the failure cost the operator the file they already had.
/// The three ways a walk fails: the handshake refused, nothing listening, and a read failing after
/// a window of rows has landed.
#[test]
fn a_failed_remote_export_leaves_out_byte_identical_and_nothing_beside_it() {
    const OLD: &[u8] = b"old bytes";
    let scratch = tempfile::tempdir().expect("tempdir");
    let dir = scratch.path().join("exports");
    std::fs::create_dir(&dir).expect("the export directory");
    let dest = dir.join("old.jsonl");
    std::fs::write(&dest, OLD).expect("plant the existing file");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let dies = listener.local_addr().expect("resolve assigned port").to_string();
    thread::spawn(move || {
        let _ = serve(listener, Arc::new(DiesAfterTheFirstWindow));
    });

    for addr in [spawn_keyed_datahub().to_string(), DEAD.to_string(), dies] {
        let out = export_quotes(scratch.path(), &addr, &dest);
        assert!(!out.status.success(), "{addr} must fail: {}", stdout(&out));
        assert_eq!(std::fs::read(&dest).expect("the file"), OLD, "{addr}: {}", stderr(&out));
        assert_eq!(names_in(&dir), ["old.jsonl"], "{addr}: no temporary file may be left");
    }
}

/// **A walk that finds nothing leaves an existing `--out` byte-identical, with nothing beside it**: a
/// typo'd symbol used to publish an empty file over it and exit 0. It stays exit 0, as a typo cannot
/// be told from a real hole. With no file there the empty one still lands.
#[test]
fn an_empty_walk_keeps_an_existing_out_and_says_so() {
    const OLD: &[u8] = b"old bytes";
    let scratch = tempfile::tempdir().expect("tempdir");
    let dir = scratch.path().join("exports");
    std::fs::create_dir(&dir).expect("the export directory");
    let addr = spawn_seeded_datahub().to_string();
    let to = (2 * DAY_MS).to_string();
    let export = |dest: &Path, extra: &[&str]| {
        let dest = dest.display().to_string();
        let mut line = vec!["data", "history", "export", "binance:NOSUCH:1h", "--addr", &addr];
        line.extend(["--format", "jsonl", "--from", "1", "--to", &to, "--out", &dest]);
        line.extend_from_slice(extra);
        let out = run(scratch.path(), &line);
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
    };

    let dest = dir.join("old.jsonl");
    std::fs::write(&dest, OLD).expect("plant the existing file");
    let said = export(&dest, &[]);
    assert!(said.contains("LEFT AS IT WAS"), "{said}");
    assert!(!said.contains("wrote"), "{said}");
    assert_eq!(std::fs::read(&dest).expect("the file"), OLD, "{said}");
    assert_eq!(names_in(&dir), ["old.jsonl"], "no temporary file may be left");
    let doc: serde_json::Value = serde_json::from_str(&export(&dest, &["--json"])).expect("json");
    assert_eq!((&doc["rows"], &doc["out_kept"]), (&serde_json::json!(0), &serde_json::json!(true)));
    assert_eq!(std::fs::read(&dest).expect("the file"), OLD);

    let fresh = dir.join("fresh.jsonl");
    let doc: serde_json::Value = serde_json::from_str(&export(&fresh, &["--json"])).expect("json");
    assert_eq!(doc["out_kept"], false, "{doc}");
    assert_eq!(std::fs::read(&fresh).expect("the empty file lands"), b"");
}

/// The other half: one that SUCCEEDS replaces `--out` whole, and one that fails with no file there
/// makes none. Without the first a fix that never publishes passes the case above.
#[test]
fn a_remote_export_replaces_out_whole_and_a_failed_one_makes_no_file() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let dir = scratch.path().join("exports");
    std::fs::create_dir(&dir).expect("the export directory");
    let dest = dir.join("old.jsonl");
    std::fs::write(&dest, "old bytes\n").expect("plant the existing file");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port").to_string();
    thread::spawn(move || {
        let _ = serve(listener, Arc::new(LaneStore));
    });
    let out = export_quotes(scratch.path(), &addr, &dest);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = std::fs::read_to_string(&dest).expect("the file");
    assert!(!text.contains("old bytes"), "the old bytes must be gone: {text}");
    assert_eq!(text.lines().count(), 2, "one quote per window: {text}");
    assert_eq!(names_in(&dir), ["old.jsonl"], "no temporary file may be left");

    let never = dir.join("never.jsonl");
    let out = export_quotes(scratch.path(), DEAD, &never);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert_eq!(names_in(&dir), ["old.jsonl"], "a failed export with no file there makes none");
}

/// A destination this side cannot write is still refused at the door, before a socket and with the
/// file intact: a directory, and a read-only file a rename alone would have replaced.
#[test]
fn a_remote_export_refuses_an_unwritable_out_before_it_dials() {
    let scratch = tempfile::tempdir().expect("tempdir");

    let out = export_quotes(scratch.path(), DEAD, scratch.path());
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("could not open --out"), "{}", stderr(&out));

    let read_only = scratch.path().join("read-only.jsonl");
    std::fs::write(&read_only, "old bytes\n").expect("plant");
    let mut permissions = std::fs::metadata(&read_only).expect("metadata").permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&read_only, permissions).expect("make it read-only");
    // Root writes through a read-only bit, so there the second half has nothing to prove.
    if std::fs::OpenOptions::new().write(true).open(&read_only).is_ok() {
        return;
    }
    let out = export_quotes(scratch.path(), DEAD, &read_only);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("could not open --out"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&read_only).expect("the file"), "old bytes\n");
}
