//! `fetch` cut one calendar year at a time, and `running`/`cancel` over a fetch being served.

use std::collections::{BTreeSet, HashSet};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_datahub::backfill::{BackfillFn, BackfillLane, BackfillTable};
use vike_datahub::serve_with_backfill;
use vike_model::Bar;

use super::support::{DAY_MS, bar, spawn_seeded_datahub, utc_day};
use super::*;

// ─── `fetch`: one request per calendar year at a day-grid lane, one request everywhere else ────

/// What a fake lane below saw: every request's window, every day it "fetched" from its fake venue,
/// and the day keys the store accepted.
#[derive(Default)]
struct LaneLog {
    requests: Vec<(i64, i64)>,
    fetched_days: Vec<i64>,
    stored_keys: HashSet<String>,
}

/// A DAY-GRID lane — the shape of `crates/vike-backfill/src/klines.rs`'s `ingest_klines_chunked`,
/// which OANDA's row stores through: the request is rounded OUTWARD to whole UTC days, every day is
/// keyed by the GRID (`{venue}:{symbol}:{interval}:{c0}-{c1}`) whatever request covered it, a day
/// whose key the store already accepted is skipped BEFORE any fetch, and a failed fetch returns at
/// once with the days before it left stored. One bar per day, at midnight.
///
/// The skip reads `stored_keys`, which holds exactly the keys the double's own commit-key check
/// (`MemHistStore::append_bars`) accepted — standing in for the manifest read the real ingest makes
/// before each day. `fail_once_at` names a day whose first fetch fails.
fn day_grid_lane(
    store: Arc<MemHistStore>,
    log: Arc<Mutex<LaneLog>>,
    fail_once_at: Arc<Mutex<Option<i64>>>,
) -> BackfillFn {
    Box::new(move |symbol: &str, interval: &str, start: i64, end: i64, _: &dyn Fn() -> bool| {
        let mut log = log.lock().unwrap();
        log.requests.push((start, end));
        let mut rows = 0;
        let mut c0 = start.div_euclid(DAY_MS) * DAY_MS;
        while c0 <= end {
            let c1 = c0 + DAY_MS - 1;
            let key = format!("oanda:{symbol}:{interval}:{c0}-{c1}");
            if !log.stored_keys.contains(&key) {
                let mut fail = fail_once_at.lock().unwrap();
                if *fail == Some(c0) {
                    *fail = None;
                    return Err(format!(
                        "chunk [{c0}, {c1}] failed after {rows} bars were written"
                    ));
                }
                log.fetched_days.push(c0);
                let written = store
                    .append_bars("oanda", symbol, interval, &[bar(c0)], Some(&key))
                    .map_err(|e| e.to_string())?;
                if written > 0 {
                    log.stored_keys.insert(key);
                }
                rows += written;
            }
            c0 += DAY_MS;
        }
        Ok(rows)
    })
}

/// A ONE-SHOT lane — the shape of `crates/vike-backfill/src/klines.rs`'s `ingest_klines`: ONE commit
/// key per request, over the request's own bounds. Records the request and stores one bar per day
/// inside it.
fn one_shot_lane(store: Arc<MemHistStore>, log: Arc<Mutex<LaneLog>>) -> BackfillFn {
    Box::new(move |symbol: &str, interval: &str, start: i64, end: i64, _: &dyn Fn() -> bool| {
        log.lock().unwrap().requests.push((start, end));
        let first = start.div_euclid(DAY_MS) * DAY_MS;
        let bars: Vec<Bar> =
            (0..).map(|n| first + n * DAY_MS).take_while(|&t| t <= end).map(bar).collect();
        let key = format!("bybit:{symbol}:{interval}:{start}-{end}");
        store.append_bars("bybit", symbol, interval, &bars, Some(&key)).map_err(|e| e.to_string())
    })
}

/// A key-less datahub whose `Backfill` dispatch is `table`, writing into one in-memory store the
/// server also serves — so each request's `BackfillDone` read-back is a real one.
fn spawn_backfill_datahub(table: impl FnOnce(Arc<MemHistStore>) -> BackfillTable) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store = Arc::new(MemHistStore::new());
    let table = table(Arc::clone(&store));
    let served: Arc<dyn HistStore + Send + Sync> = store;
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, served, Some(table));
    });
    addr
}

/// The OANDA-shaped fixture: a datahub whose `oanda` row is the day-grid lane, on the lane the real
/// table puts it on.
fn spawn_day_grid_datahub(log: &Arc<Mutex<LaneLog>>, fail_once_at: Option<i64>) -> SocketAddr {
    let (log, fail) = (Arc::clone(log), Arc::new(Mutex::new(fail_once_at)));
    spawn_backfill_datahub(move |store| {
        BackfillTable::new(Vec::new()).with(
            "oanda",
            BackfillLane::CredentialedKlines,
            day_grid_lane(store, log, fail),
        )
    })
}

/// Every day from `from` to `to`, inclusive, as midnights.
fn days_between(from: i64, to: i64) -> Vec<i64> {
    (0..).map(|n| from + n * DAY_MS).take_while(|&t| t <= to).collect()
}

/// The three-year window both day-grid cases fetch — a ragged first year, a whole one, a ragged
/// last — minus its `--to` value.
const SPLIT_ARGS: [&str; 7] =
    ["data", "hist", "fetch", "oanda:EUR_USD:1h", "--from", "2021-07-01", "--to"];

/// **A long window at a day-grid lane goes out ONE REQUEST PER CALENDAR YEAR, and each year is
/// reported as it finishes.** End to end through the shipped binary and a real datahub: the requests
/// are the three year pieces with the operator's own outer bounds, together they fetch every day of
/// the window exactly once, stderr carries the header and one line per year, and the `--json`
/// document carries every key a one-request fetch's does — merged — plus one entry per piece.
#[test]
fn a_day_grid_lanes_long_window_is_fetched_a_year_at_a_time() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let log = Arc::new(Mutex::new(LaneLog::default()));
    let addr = spawn_day_grid_datahub(&log, None).to_string();

    let mut args = SPLIT_ARGS.to_vec();
    args.extend(["2023-03-01", "--addr", addr.as_str(), "--json"]);
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));

    let (start, end) = (utc_day(2021, 7, 1), utc_day(2023, 3, 1));
    let log = log.lock().unwrap();
    assert_eq!(
        log.requests,
        vec![
            (start, utc_day(2022, 1, 1) - 1),
            (utc_day(2022, 1, 1), utc_day(2023, 1, 1) - 1),
            (utc_day(2023, 1, 1), end),
        ],
        "one request per calendar year, the outer two keeping the typed bounds"
    );
    assert_eq!(log.fetched_days, days_between(start, end), "every day once, in order");

    let err = stderr(&out);
    assert!(err.contains("as 3 requests, one per calendar year"), "{err}");
    for line in [
        "  1/3 [2021-07-01 .. 2021-12-31]: 184 rows written in ",
        "  2/3 [2022-01-01 .. 2022-12-31]: 365 rows written in ",
        "  3/3 [2023-01-01 .. 2023-03-01]: 60 rows written in ",
    ] {
        assert!(err.contains(line), "missing progress line {line:?}: {err}");
    }

    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));
    assert_eq!(doc["venue"], "oanda");
    assert_eq!(doc["symbol"], "EUR_USD");
    assert_eq!(doc["interval"], "1h");
    assert_eq!(doc["addr"], addr.as_str());
    assert_eq!(doc["rows_written"], 609);
    assert_eq!(doc["first_ts"], start);
    assert_eq!(doc["last_ts"], end);
    let pieces = doc["pieces"].as_array().expect("a pieces array");
    assert_eq!(pieces.len(), 3);
    for (piece, &(from, to)) in pieces.iter().zip(&log.requests) {
        assert_eq!(piece["from"], from, "{piece}");
        assert_eq!(piece["to"], to, "{piece}");
        assert!(piece["elapsed_ms"].is_u64(), "{piece}");
    }
    assert_eq!(pieces[1]["rows_written"], 365);
}

/// **A failed year STOPS the run, names what is stored, and a re-run RESUMES.** The fake venue fails
/// on 15 June of the second year, once:
///
/// 1. the first run exits non-zero, never sends the third year, and names the failed piece, the rows
///    the year before it wrote and how to resume;
/// 2. the SAME command run again fetches only what is missing — from the failed day on — and no day
///    the first run stored is fetched a second time;
/// 3. a third run does no work at all: zero days fetched, zero rows.
#[test]
fn a_failed_year_stops_the_run_and_the_same_command_resumes_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let log = Arc::new(Mutex::new(LaneLog::default()));
    let failing_day = utc_day(2022, 6, 15);
    let addr = spawn_day_grid_datahub(&log, Some(failing_day)).to_string();
    let mut args = SPLIT_ARGS.to_vec();
    args.extend(["2023-03-01", "--addr", addr.as_str()]);
    let (start, end) = (utc_day(2021, 7, 1), utc_day(2023, 3, 1));

    // 1. The failure.
    let out = run(scratch.path(), &args);
    assert_eq!(out.status.code(), Some(1), "a failed year is a failed run: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("piece 2 of 3 [2022-01-01 .. 2022-12-31] failed"), "{err}");
    assert!(err.contains("the piece before it wrote 184 rows, which stay stored"), "{err}");
    assert!(err.contains("Re-run the same command to resume"), "{err}");
    assert!(err.contains(&format!("chunk [{failing_day}, ")), "the datahub's own text: {err}");
    assert!(err.contains("  1/3 [2021-07-01 .. 2021-12-31]: 184 rows written in "), "{err}");
    assert!(!err.contains("  2/3 ["), "no progress line for the failed year: {err}");
    assert_eq!(stdout(&out), "", "a failed run prints no result line");
    let stored_by_the_first_run = {
        let log = log.lock().unwrap();
        assert_eq!(log.requests.len(), 2, "the third year was sent after the second failed");
        assert_eq!(log.fetched_days, days_between(start, failing_day - DAY_MS));
        log.fetched_days.len()
    };

    // 2. The resume: only the missing days are fetched, and none twice.
    args.push("--json");
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    {
        let log = log.lock().unwrap();
        assert_eq!(log.requests.len(), 5, "the re-run sends the three years again");
        assert_eq!(
            log.fetched_days,
            days_between(start, end),
            "the re-run fetched a day the first run had stored, or missed one"
        );
        let resumed = (log.fetched_days.len() - stored_by_the_first_run) as u64;
        assert_eq!(doc["rows_written"], resumed, "the re-run wrote exactly the missing days");
        assert_eq!(doc["pieces"][0]["rows_written"], 0, "the stored first year cost nothing");
    }

    // 3. Nothing left to do.
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    assert_eq!(doc["rows_written"], 0);
    let log = log.lock().unwrap();
    assert_eq!(log.requests.len(), 8);
    assert_eq!(log.fetched_days.len(), days_between(start, end).len(), "the third run fetched");
}

/// **A ONE-SHOT lane's window is ONE request, however long** — the hazard the split must never
/// reach. bybit's lane keys a commit by the REQUEST's bounds, so three per-year requests would
/// store the window's bars under three keys a later whole-window request does not share: the same
/// bars twice. Three years is far past the one-year floor, so only the lane rule keeps this one
/// request. And the answer is the one-request contract as it always was: the same line, nothing
/// about years on stderr, and a `--json` document with exactly the keys it always had.
///
/// ⚠ **bybit, and not binance, on purpose — measured by this file's kill proof.** binance's rows
/// claim a `Funding` lane beside its `Klines` one, and a venue is cut only when EVERY lane it claims
/// stores whole days; so a mutation that made the `Klines` lane split left binance whole and this
/// case green while three unit tests went red. bybit claims `Klines` alone, so the same mutation
/// cuts it and this case fails for the reason it states.
#[test]
fn a_one_shot_lanes_long_window_is_one_request() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let log = Arc::new(Mutex::new(LaneLog::default()));
    let lane_log = Arc::clone(&log);
    let addr = spawn_backfill_datahub(move |store| {
        BackfillTable::new(vec![("bybit".to_string(), one_shot_lane(store, lane_log))])
    })
    .to_string();

    let mut args = vec!["data", "hist", "fetch", "bybit:BTCUSDT:1h"];
    args.extend(["--from", "2021-07-01", "--to", "2024-03-01", "--addr", addr.as_str()]);
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let (start, end) = (utc_day(2021, 7, 1), utc_day(2024, 3, 1));
    assert_eq!(log.lock().unwrap().requests, vec![(start, end)], "a one-shot lane was cut");
    let rows = days_between(start, end).len();
    assert_eq!(
        stdout(&out).trim_end(),
        format!(
            "fetched bybit:BTCUSDT:1h -> {rows} rows written spanning 2021-07-01 .. 2024-03-01 \
             (datahub at {addr})"
        )
    );
    assert!(!stderr(&out).contains("calendar year"), "{}", stderr(&out));

    args.push("--json");
    let out = run(scratch.path(), &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    let keys: BTreeSet<&str> =
        doc.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        BTreeSet::from([
            "addr",
            "first_ts",
            "interval",
            "last_ts",
            "rows_written",
            "symbol",
            "venue"
        ]),
        "the one-request document grew or lost a key"
    );
    assert_eq!(log.lock().unwrap().requests.len(), 2, "the --json run was cut");
}

// ─── `running` and `cancel`: the operator's door onto a fetch the datahub is serving ───────────

/// How many chunks the gated lane below walks.
const GATED_CHUNKS: usize = 4;

/// What the gated lane did at one boundary.
#[derive(Debug, PartialEq)]
enum Boundary {
    /// The stop probe said go on, and chunk `n` was "stored".
    Stored(usize),
    /// The stop probe said stop at the top of chunk `n`.
    Stopped(usize),
}

/// A CHUNKED lane in miniature — the shape of `vike_backfill`'s day-chunked ingest, minus the
/// store: [`GATED_CHUNKS`] chunks, the request's stop probe asked at the top of each, and every
/// boundary WAITING for the test's go-ahead, so the test decides what the operator has done by the
/// time the probe is asked. Mounted as a `CredentialedKlines` row, the lane a cancel can stop.
fn gated_lane() -> (BackfillFn, mpsc::Sender<()>, mpsc::Receiver<Boundary>) {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (heard_tx, heard_rx) = mpsc::channel::<Boundary>();
    let go_rx = Mutex::new(go_rx);
    let lane: BackfillFn =
        Box::new(move |_: &str, _: &str, _: i64, _: i64, should_stop: &dyn Fn() -> bool| {
            let go = go_rx.lock().expect("one request at a time");
            for chunk in 0..GATED_CHUNKS {
                if go.recv_timeout(Duration::from_secs(30)).is_err() {
                    return Err("the test stopped driving the lane".to_string());
                }
                if should_stop() {
                    let _ = heard_tx.send(Boundary::Stopped(chunk));
                    return Err(format!(
                        "stopped: before chunk {} of {GATED_CHUNKS}; {chunk} chunk(s) stored; \
                         repeating the request resumes",
                        chunk + 1
                    ));
                }
                let _ = heard_tx.send(Boundary::Stored(chunk));
            }
            Ok(GATED_CHUNKS)
        });
    (lane, go_tx, heard_rx)
}

/// **The door, end to end through the shipped binary.** A `fetch` runs in one process; a second
/// invocation lists it with `running` — its spec ready to paste, a cancel able to stop it — and a
/// third `cancel`s it by that spec. The fetch then stops at its NEXT boundary, exits on the
/// run-failure rung with the cancel named and the collector's account of what stayed stored, and
/// `running` lists nothing.
#[test]
fn a_running_fetch_is_listed_and_a_cancel_by_its_spec_stops_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (lane, go, heard) = gated_lane();
    let addr = spawn_backfill_datahub(move |_| {
        BackfillTable::new(Vec::new()).with("oanda", BackfillLane::CredentialedKlines, lane)
    })
    .to_string();

    let fetch = {
        let (dir, addr) = (scratch.path().to_path_buf(), addr.clone());
        thread::spawn(move || {
            let mut args = vec!["data", "hist", "fetch", "oanda:EUR_USD:1h"];
            args.extend(["--from", "2024-01-01", "--to", "2024-01-05", "--addr", addr.as_str()]);
            run(&dir, &args)
        })
    };
    go.send(()).expect("the lane is waiting at its first boundary");
    assert_eq!(heard.recv_timeout(Duration::from_secs(30)).expect("it runs"), Boundary::Stored(0));

    // `running` — the human table, then the document.
    let out = run(scratch.path(), &["data", "hist", "running", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("1 backfill running on the datahub at"), "{text}");
    assert!(text.contains("oanda:EUR_USD:1h"), "the spec, ready to paste: {text}");
    assert!(text.contains("2024-01-01 .. 2024-01-05"), "{text}");
    assert!(text.contains("at its next chunk boundary"), "a cancel can stop it: {text}");
    let out = run(scratch.path(), &["data", "hist", "running", "--addr", &addr, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one JSON document");
    let row = &doc["running"][0];
    assert_eq!(row["spec"], "oanda:EUR_USD:1h");
    assert_eq!(row["lane"], "CredentialedKlines");
    assert_eq!(row["stoppable"], true);
    assert_eq!(row["cancelled"], false);
    assert_eq!(row["boundaries"], 1, "the top of chunk 0");

    // `cancel` by that spec.
    let out = run(scratch.path(), &["data", "hist", "cancel", "oanda:EUR_USD:1h", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("cancel raised on #1 oanda:EUR_USD:1h"), "{text}");
    assert!(text.contains("does not wait"), "{text}");

    // It stops at the NEXT boundary, and its own client is answered with the cancel.
    go.send(()).expect("the lane is waiting at its second boundary");
    assert_eq!(
        heard.recv_timeout(Duration::from_secs(30)).expect("it reaches the boundary"),
        Boundary::Stopped(1),
        "the cancelled fetch must stop at the first boundary after the cancel"
    );
    let fetched = fetch.join().expect("the fetching thread");
    assert_eq!(
        fetched.status.code(),
        Some(1),
        "a cancelled fetch is a failed run: {}",
        stderr(&fetched)
    );
    let err = stderr(&fetched);
    assert!(err.contains("CANCELLED by an operator"), "{err}");
    assert!(err.contains("1 chunk(s) stored") && err.contains("resumes"), "{err}");

    let out = run(scratch.path(), &["data", "hist", "running", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("no backfill is running"), "{}", stdout(&out));
    // ...and a second cancel of the same series is an empty SUCCESS, not an error.
    let out = run(scratch.path(), &["data", "hist", "cancel", "oanda:EUR_USD:1h", "--addr", &addr]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("nothing is running on oanda:EUR_USD:1h"), "{}", stdout(&out));
}

/// **A datahub that does not advertise the door is refused with NOTHING SENT**, on the run-failure
/// rung, naming the capability — here a datahub with no collector table, the shape of a server
/// that runs no backfill (an older one answers the same way, through the same client check).
#[test]
fn the_door_is_refused_by_name_against_a_datahub_that_does_not_serve_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    for args in [
        vec!["data", "hist", "running", "--addr", addr.as_str()],
        vec!["data", "hist", "cancel", "binance:BTCUSDT:1h", "--addr", addr.as_str()],
    ] {
        let out = run(scratch.path(), &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains("backfill_cancel"), "{args:?} names the capability: {err}");
        assert!(err.contains("nothing was sent"), "{args:?}: {err}");
        assert_eq!(stdout(&out), "", "{args:?} printed an answer it did not get");
    }
}
