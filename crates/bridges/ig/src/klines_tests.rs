//! Tests of the IG history source, over a scripted gateway: no network, no login, no credential.
//!
//! The fixtures are the 2026-10-23 probe's own rows (`crates/bridges/ig/tests/ig_history_probe.rs`,
//! its recorded output), verbatim where a figure matters: the EURUSD hours, the DAX and WTI hours, the
//! allowance metadata, the market snapshots, the local-clock window reading and the daily stamps. A
//! price in a row the probe did not print is invented and says so.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;

const EURUSD: &str = "CS.D.EURUSD.MINI.IP";
const DAX: &str = "IX.D.DAX.IFD.IP";
const WTI: &str = "CC.D.CL.BMU.IP";

/// 2026-10-23T17:22:23Z, the instant the probe's first answer arrived.
const NOW: i64 = 1_792_776_143_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

fn fixed_now() -> i64 {
    NOW
}

fn utc(text: &str) -> i64 {
    parse_ig_time_utc(text)
}

// ── the scripted gateway ───────────────────────────────────────────────────────────────────────

/// A session that answers from a queue and records every request it is asked.
struct Scripted {
    calls: Mutex<Vec<(String, String)>>,
    replies: Mutex<VecDeque<Result<Value, IgApiError>>>,
}

impl Scripted {
    fn new(replies: Vec<Result<Value, IgApiError>>) -> Arc<Self> {
        Arc::new(Self { calls: Mutex::new(Vec::new()), replies: Mutex::new(replies.into()) })
    }
    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }
}

impl PricesApi for Scripted {
    fn account_id(&self) -> String {
        "ABC12".to_string()
    }
    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, IgApiError> {
        assert!(version == "3", "every history request is version 3, got {version}");
        self.calls.lock().unwrap().push((path.to_string(), query.to_string()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("an unscripted request: {path}?{query}"))
    }
}

/// A gate that records what it is asked and refuses above a ceiling, when given one.
#[derive(Default)]
struct ScriptedGate {
    bound: Mutex<Vec<String>>,
    admitted: Mutex<Vec<u32>>,
    whats: Mutex<Vec<String>>,
    learned: Mutex<Vec<Allowance>>,
    exhausted_with: Mutex<Vec<String>>,
    refuse_above: Option<u32>,
    prechecked: Mutex<Vec<(u32, String)>>,
    precheck_refuses: bool,
    login_failures: Mutex<Vec<String>>,
}

impl AllowanceGate for ScriptedGate {
    fn bind(&self, account_id: &str) {
        self.bound.lock().unwrap().push(account_id.to_string());
    }
    fn admit(&self, points: u32, what: &str, _stop: &dyn Fn() -> bool) -> Result<(), SourceError> {
        self.admitted.lock().unwrap().push(points);
        self.whats.lock().unwrap().push(what.to_string());
        match self.refuse_above {
            Some(ceiling) if points > ceiling => {
                Err(SourceError::Fetch(format!("the ledger stops {points} points")))
            }
            _ => Ok(()),
        }
    }
    fn learn(&self, allowance: Allowance) {
        self.learned.lock().unwrap().push(allowance);
    }
    fn exhausted(&self, code: &str) -> String {
        self.exhausted_with.lock().unwrap().push(code.to_string());
        format!("EXHAUSTED {code}")
    }
    fn precheck(&self, points: u32, what: &str) -> Result<(), SourceError> {
        self.prechecked.lock().unwrap().push((points, what.to_string()));
        if self.precheck_refuses {
            Err(SourceError::Fetch("PRECHECK refused".to_string()))
        } else {
            Ok(())
        }
    }
    fn login_failed(&self, why: &str) {
        self.login_failures.lock().unwrap().push(why.to_string());
    }
}

struct Rig {
    source: IgKlines,
    api: Arc<Scripted>,
    gate: Arc<ScriptedGate>,
    logins: Arc<AtomicUsize>,
}

fn start(replies: Vec<Result<Value, IgApiError>>) -> Rig {
    start_with(replies, ScriptedGate::default(), fixed_now)
}

fn start_with(
    replies: Vec<Result<Value, IgApiError>>,
    gate: ScriptedGate,
    clock: fn() -> i64,
) -> Rig {
    let api = Scripted::new(replies);
    let gate = Arc::new(gate);
    let logins = Arc::new(AtomicUsize::new(0));
    let (login_api, counted) = (Arc::clone(&api), Arc::clone(&logins));
    let login: LoginFn = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&login_api) as Arc<dyn PricesApi>)
    });
    let source =
        IgKlines::with_login(login, Arc::clone(&gate) as Arc<dyn AllowanceGate>).with_clock(clock);
    Rig { source, api, gate, logins }
}

// ── the fixtures ───────────────────────────────────────────────────────────────────────────────

type Side = (f64, f64);

/// One row in the answer's own shape; every price is `(bid, ask)`.
fn row(utc: &str, local: &str, o: Side, h: Side, l: Side, c: Side, volume: u64) -> Value {
    let side = |(bid, ask): Side| json!({"bid": bid, "ask": ask, "lastTraded": null});
    json!({
        "snapshotTime": local, "snapshotTimeUTC": utc, "lastTradedVolume": volume,
        "openPrice": side(o), "highPrice": side(h), "lowPrice": side(l), "closePrice": side(c),
    })
}

fn body(rows: Vec<Value>, remaining: u32) -> Value {
    let n = rows.len();
    json!({
        "prices": rows,
        "instrumentType": "CURRENCIES",
        "metadata": {
            "allowance": {
                "allowanceExpiry": 604799, "remainingAllowance": remaining, "totalAllowance": 10000
            },
            "pageData": {"pageNumber": 1, "pageSize": n, "totalPages": 1},
            "size": n
        }
    })
}

/// The first read of EURUSD: one daily bar, the stamp pair the probe printed for its newest daily bar
/// (local midnight, +480 minutes), the close the market snapshot below prices.
fn eur_first_read() -> Value {
    body(
        vec![row(
            "2026-10-23T16:00:00",
            "2026/10/24 00:00:00",
            (1.1190, 1.1191),
            (1.1200, 1.1201),
            (1.1180, 1.1181),
            (1.11927, 1.11933),
            20_000,
        )],
        9_999,
    )
}

/// The probe's snapshot of EURUSD: `1.11927 / 1.11933`, `scalingFactor` 10000, a pip of 0.0001.
fn eur_snapshot() -> Value {
    json!({
        "instrument": {"onePipMeans": "0.0001 USD/EUR"},
        "snapshot": {"marketStatus": "TRADEABLE", "bid": 1.11927, "offer": 1.11933, "scalingFactor": 10000}
    })
}

/// The probe's window reading D: asked for 06:00 to 11:00 on the account's clock, the gateway answered
/// the six hours stamped 22:00Z to 03:00Z (prices invented).
fn probe_d_answer() -> Value {
    let hours = [
        ("2026-10-22T22:00:00", "2026/10/23 06:00:00"),
        ("2026-10-22T23:00:00", "2026/10/23 07:00:00"),
        ("2026-10-23T00:00:00", "2026/10/23 08:00:00"),
        ("2026-10-23T01:00:00", "2026/10/23 09:00:00"),
        ("2026-10-23T02:00:00", "2026/10/23 10:00:00"),
        ("2026-10-23T03:00:00", "2026/10/23 11:00:00"),
    ];
    let rows = hours
        .iter()
        .map(|(u, l)| row(u, l, (1.1, 1.1002), (1.1, 1.1002), (1.1, 1.1002), (1.1, 1.1002), 100))
        .collect();
    body(rows, 9_845)
}

fn probe_d_window() -> (i64, i64) {
    (utc("2026-10-22T22:00:00"), utc("2026-10-23T03:00:00") + 999)
}

/// The probe's EURUSD hours: its oldest and newest rows, verbatim.
fn probe_a_rows() -> Vec<Value> {
    vec![
        row(
            "2026-10-19T14:00:00",
            "2026/10/19 22:00:00",
            (1.12226, 1.12232),
            (1.12226, 1.12235),
            (1.12103, 1.12109),
            (1.12173, 1.12179),
            2507,
        ),
        row(
            "2026-10-23T17:00:00",
            "2026/10/24 01:00:00",
            (1.11929, 1.11935),
            (1.11942, 1.11948),
            (1.11904, 1.1191),
            (1.11927, 1.11933),
            512,
        ),
    ]
}

fn fetch_ok(rig: &Rig, symbol: &str, interval: &str, window: (i64, i64)) -> Vec<Bar> {
    rig.source.fetch(symbol, interval, window.0, window.1).expect("the scripted fetch")
}

// ── the numbers ────────────────────────────────────────────────────────────────────────────────

/// The probe's metadata, verbatim: remaining 9900 of 10000, resetting in 604799 s; and the line that
/// follows the 65-second wait. A body without the object (an error body) reads as `None`.
#[test]
fn the_probes_metadata_parses_into_an_allowance() {
    let first = json!({"metadata": {"allowance": {"allowanceExpiry": 604799, "remainingAllowance": 9900, "totalAllowance": 10000}, "pageData": {"pageNumber": 1, "pageSize": 100, "totalPages": 1}, "size": 100}});
    assert_eq!(
        parse_allowance(&first),
        Some(Allowance { remaining: 9900, total: 10000, resets_in_s: 604_799 })
    );
    let later = json!({"metadata": {"allowance": {"allowanceExpiry": 604730, "remainingAllowance": 9851, "totalAllowance": 10000}}});
    assert_eq!(parse_allowance(&later).map(|a| a.remaining), Some(9851));
    assert_eq!(
        parse_allowance(&json!({"errorCode": "error.public-api.exceeded-api-key-allowance"})),
        None
    );
    assert_eq!(
        parse_allowance(&json!({"metadata": {"allowance": {"remainingAllowance": 5}}})),
        None
    );
}

/// The reserve rounds UP (a fraction of a point is a point kept) and the per-request share rounds
/// DOWN (a fraction of a point is a point not spent).
#[test]
fn the_reserve_rounds_up_and_the_request_share_rounds_down() {
    assert_eq!((reserve_points(10_000), request_cap_points(10_000)), (2_000, 5_000));
    assert_eq!((reserve_points(9_999), request_cap_points(9_999)), (2_000, 4_999));
    assert_eq!((reserve_points(1), request_cap_points(1)), (1, 0));
    assert_eq!((reserve_points(0), request_cap_points(0)), (0, 0));
}

/// A chunk's cost is the calendar bound: one row per step. A day of hours is 24, a day of minutes
/// 1440, a 28-day daily chunk 28; an unserved interval and an inverted window have no estimate; a
/// window of centuries saturates instead of wrapping.
#[test]
fn the_estimate_is_the_calendar_bound_and_saturates() {
    assert_eq!(estimate_points("1h", 0, DAY_MS - 1), Some(24));
    assert_eq!(estimate_points("1m", 0, DAY_MS - 1), Some(1_440));
    assert_eq!(estimate_points("1d", 0, 28 * DAY_MS - 1), Some(28));
    assert_eq!(estimate_points("1h", 5, 5), Some(1), "one instant is one row at most");
    assert_eq!(estimate_points("1s", 0, DAY_MS), None);
    assert_eq!(estimate_points("1w", 0, DAY_MS), None);
    assert_eq!(estimate_points("1h", 10, 9), None);
    assert_eq!(estimate_points("1m", i64::MIN, i64::MAX), Some(u32::MAX));
}

/// The intervals served are the twelve of the catalog's pinned table, with the same step: the source
/// and the daemon that sizes and refuses a request cannot disagree about a width.
#[test]
fn the_served_intervals_are_the_catalogs_table() {
    for step in vike_catalog::IG_HISTORY_STEPS {
        let (resolution, ms) = served_interval(step.interval).expect(step.interval);
        assert_eq!(ms, step.step_ms, "{}", step.interval);
        assert!(!resolution.is_empty());
    }
    assert_eq!(served_interval("1h"), Some(("HOUR", HOUR_MS)));
    assert_eq!(served_interval("1d"), Some(("DAY", DAY_MS)));
    for not in ["1s", "1w", "1mo", "7m", "0m", "", "x"] {
        assert_eq!(served_interval(not), None, "{not:?}");
    }
    // The dearest chunk of the pinned table is a day of minutes: the most any one request can be
    // asked to spend at once.
    let dearest = vike_catalog::IG_HISTORY_STEPS
        .iter()
        .map(|s| estimate_points(s.interval, 0, i64::from(s.chunk_days) * DAY_MS - 1).unwrap())
        .max();
    assert_eq!(dearest, Some(1_440));
}

// ── the window, the offset, the forming bar ────────────────────────────────────────────────────

/// **`from` and `to` are read on the account's LOCAL clock.** The probe asked 06:00 to 11:00 and was
/// answered the hours stamped 22:00Z to 03:00Z: so a UTC window of 22:00Z to 03:00Z must go out as
/// 06:00 to 11:00, the offset taken from the account's clock rule (480 on that date) and proved by the
/// first read. The three requests are the first read, the market snapshot and the chunk.
#[test]
fn a_window_is_asked_in_the_accounts_local_clock() {
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(probe_d_answer())]);

    let bars = fetch_ok(&rig, EURUSD, "1h", probe_d_window());

    assert_eq!(
        rig.api.calls(),
        vec![
            (format!("/prices/{EURUSD}"), "resolution=DAY&max=1&pageSize=0".to_string()),
            (format!("/markets/{EURUSD}"), String::new()),
            (
                format!("/prices/{EURUSD}"),
                "resolution=HOUR&from=2026-10-23T06:00:00&to=2026-10-23T11:00:00&pageSize=0"
                    .to_string()
            ),
        ]
    );
    let stamps: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    let want: Vec<i64> = (0..6).map(|h| utc("2026-10-22T22:00:00") + h * HOUR_MS).collect();
    assert_eq!(stamps, want, "stored at the UTC open, not the local clock");
}

/// The first bar of the probe's EURUSD hours, pinned to the figures: the stamp is `snapshotTimeUTC`
/// (a literal epoch), each price is the MID of bid and ask, and the volume is the tick count.
#[test]
fn a_bar_is_the_mid_of_bid_and_ask_stamped_at_the_open_with_the_tick_count_as_volume() {
    let decoded = read_rows(&body(probe_a_rows(), 9_900)).expect("the probe's own rows");

    let first = &decoded.bars[0];
    assert_eq!(first.ts, 1_792_418_400_000, "2026-10-19T14:00:00Z, the UTC stamp");
    assert!((first.open - (1.12226 + 1.12232) / 2.0).abs() < 1e-12);
    assert!((first.high - (1.12226 + 1.12235) / 2.0).abs() < 1e-12);
    assert!((first.low - (1.12103 + 1.12109) / 2.0).abs() < 1e-12);
    assert!((first.close - (1.12173 + 1.12179) / 2.0).abs() < 1e-12);
    assert_eq!(first.volume, 2507.0, "lastTradedVolume, a tick count");
    assert_eq!(
        (first.bid, first.ask, first.funding, first.symbol.clone()),
        (None, None, None, None)
    );
}

/// **The forming bar is dropped.** The probe's newest row is the current hour (17:00Z at 17:22Z) and its
/// close sits on the live price: it is returned by IG and is NOT a closed bar. At the instant the hour
/// closes it is kept — a bar whose close time equals now is closed.
#[test]
fn the_forming_bar_is_dropped_and_a_closed_one_kept() {
    fn at_17_22() -> i64 {
        NOW
    }
    fn at_18_00() -> i64 {
        1_792_778_400_000
    }
    for (clock, kept) in [(at_17_22 as fn() -> i64, 1), (at_18_00 as fn() -> i64, 2)] {
        let rig = start_with(
            vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(probe_a_rows(), 9_800))],
            ScriptedGate::default(),
            clock,
        );

        let bars =
            fetch_ok(&rig, EURUSD, "1h", (utc("2026-10-19T00:00:00"), utc("2026-10-23T23:59:59")));

        assert_eq!(bars.len(), kept, "clock {}", clock());
        assert_eq!(bars[0].ts, utc("2026-10-19T14:00:00"));
        if kept == 2 {
            assert_eq!(bars[1].ts, utc("2026-10-23T17:00:00"));
        }
    }
}

/// **A daily bar is stamped at LOCAL midnight and stored at that instant**: the probe's three daily
/// stamps are 16:00Z of the day before (Wed, Thu, Fri 16:00Z for local Thu, Fri, Sat). They come back
/// under those stamps, and a 28-day chunk's window is asked from the chunk start's local clock.
#[test]
fn a_daily_bar_keeps_its_open_stamp_at_local_midnight() {
    let day =
        |u: &str, l: &str| row(u, l, (1.1, 1.1002), (1.1, 1.1002), (1.1, 1.1002), (1.1, 1.1002), 9);
    let answer = body(
        vec![
            day("2026-10-21T16:00:00", "2026/10/22 00:00:00"),
            day("2026-10-22T16:00:00", "2026/10/23 00:00:00"),
            day("2026-10-23T16:00:00", "2026/10/24 00:00:00"),
        ],
        9_000,
    );
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(answer)]);
    let chunk_start = 20_749 * DAY_MS - 28 * DAY_MS; // a grid-shaped start: 2026-09-25
    let window = (chunk_start, chunk_start + 28 * DAY_MS - 1);

    let bars = fetch_ok(&rig, EURUSD, "1d", window);

    let query = rig.api.calls()[2].1.clone();
    assert_eq!(query, "resolution=DAY&from=2026-09-25T08:00:00&to=2026-10-23T07:59:59&pageSize=0");
    // The clock stands at Friday 17:22Z: the Friday-16:00Z bar opened an hour ago and is forming.
    let stamps: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    assert_eq!(stamps, vec![utc("2026-10-21T16:00:00"), utc("2026-10-22T16:00:00")]);
    assert!(bars.iter().all(|b| b.ts.rem_euclid(DAY_MS) == 16 * HOUR_MS), "stored at 16:00Z");
}

/// Stamps must be STRICTLY increasing: an equal or a reversed pair is an answer this source cannot
/// store, and the failure names the row.
#[test]
fn stamps_must_be_strictly_increasing() {
    let same = |u: &str, l: &str| row(u, l, (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), 1);
    for rows in [
        vec![
            same("2026-10-22T22:00:00", "2026/10/23 06:00:00"),
            same("2026-10-22T22:00:00", "2026/10/23 06:00:00"),
        ],
        vec![
            same("2026-10-22T23:00:00", "2026/10/23 07:00:00"),
            same("2026-10-22T22:00:00", "2026/10/23 06:00:00"),
        ],
    ] {
        let why = read_rows(&body(rows, 9_000)).expect_err("not increasing");
        assert!(why.contains("row 2") && why.contains("strictly increasing"), "{why}");
    }
}

/// **Every row's offset is checked and fails loudly**: a difference that is not a whole number of
/// minutes, one that the account's clock rule does not give at that instant, one that no time zone is, a
/// row with no stamp.
#[test]
fn an_offset_that_is_not_whole_minutes_or_contradicts_the_rule_fails() {
    let at = |u: &str, l: &str| row(u, l, (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), 1);
    // 90 seconds apart.
    let why =
        read_rows(&body(vec![at("2026-10-22T22:00:00", "2026/10/23 06:01:30")], 1)).unwrap_err();
    assert!(why.contains("whole number of minutes"), "{why}");
    // +480 then +420 on a summer-time date: the second row contradicts the rule, and is named with
    // both numbers.
    let why = read_rows(&body(
        vec![
            at("2026-10-22T22:00:00", "2026/10/23 06:00:00"),
            at("2026-10-22T23:00:00", "2026/10/23 06:00:00"),
        ],
        1,
    ))
    .unwrap_err();
    assert!(
        why.contains("row 2") && why.contains("420 minutes") && why.contains("is 480"),
        "{why}"
    );
    // The same two rows a week after the change are +420 both, and fine: an answer may CROSS a change
    // and every row is judged at its own instant.
    let winter = read_rows(&body(
        vec![
            at("2026-10-26T22:00:00", "2026/10/27 05:00:00"),
            at("2026-10-26T23:00:00", "2026/10/27 06:00:00"),
        ],
        1,
    ))
    .expect("420 in the winter");
    assert_eq!(winter.bars.len(), 2);
    let across = read_rows(&body(
        vec![
            at("2026-10-25T00:00:00", "2026/10/25 08:00:00"),
            at("2026-10-25T01:00:00", "2026/10/25 08:00:00"),
        ],
        1,
    ))
    .expect("480 before the change instant and 420 from it");
    assert_eq!(across.bars.len(), 2);
    // A stamp before the rule's first year is refused, not guessed.
    let why =
        read_rows(&body(vec![at("1990-06-01T00:00:00", "1990/06/01 08:00:00")], 1)).unwrap_err();
    assert!(why.contains("1996"), "{why}");
    // +20 hours.
    let why =
        read_rows(&body(vec![at("2026-10-22T22:00:00", "2026/10/23 18:00:00")], 1)).unwrap_err();
    assert!(why.contains("no time zone"), "{why}");
    // a row without a local stamp
    let mut broken = at("2026-10-22T22:00:00", "2026/10/23 06:00:00");
    broken.as_object_mut().unwrap().remove("snapshotTime");
    let why = read_rows(&body(vec![broken], 1)).unwrap_err();
    assert!(why.contains("no `snapshotTime`"), "{why}");
}

/// A row with a side missing is an `Err` naming the row and the field, never a row skipped: a skipped
/// row is a hole in a chunk whose commit key then cannot be repaired.
#[test]
fn a_row_without_a_two_sided_price_fails_instead_of_being_skipped() {
    let mut one_sided = row(
        "2026-10-22T22:00:00",
        "2026/10/23 06:00:00",
        (1.0, 1.1),
        (1.0, 1.1),
        (1.0, 1.1),
        (1.0, 1.1),
        1,
    );
    one_sided["lowPrice"]["ask"] = Value::Null;
    let why = read_rows(&body(vec![one_sided], 1)).unwrap_err();
    assert!(
        why.contains("row 1") && why.contains("lowPrice") && why.contains("two-sided"),
        "{why}"
    );
}

/// `pageSize=0` answered one page in the probe. An answer that says otherwise, or whose body is not as
/// long as it says, is refused: a chunk stored from a first page would be short for good.
#[test]
fn a_paged_answer_or_a_truncated_body_fails() {
    let mut paged = body(probe_a_rows(), 9_000);
    paged["metadata"]["pageData"]["totalPages"] = json!(3);
    assert!(read_rows(&paged).unwrap_err().contains("paged"));
    let mut short = body(probe_a_rows(), 9_000);
    short["metadata"]["size"] = json!(5);
    assert!(read_rows(&short).unwrap_err().contains("truncated"));
    assert!(read_rows(&json!({"metadata": {}})).unwrap_err().contains("no `prices`"));
}

/// An answer whose offset contradicts the rule (here 420 on a summer-time date, the window asked in 480)
/// means the window was asked in the wrong clock and is shifted: the chunk is NOT returned, it is charged
/// and says so, and the offset is NOT learned from the answer: the next request is asked by the rule
/// again.
#[test]
fn an_answer_in_another_offset_fails_the_chunk_and_nothing_is_learned_from_it() {
    let shifted = {
        let at = |u: &str, l: &str| row(u, l, (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), 1);
        body(vec![at("2026-10-22T22:00:00", "2026/10/23 05:00:00")], 9_000)
    };
    let rig =
        start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(shifted), Ok(body(vec![], 9_000))]);

    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();

    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    assert!(text.contains("420 minutes") && text.contains("is 480"), "{text}");
    assert!(text.contains("shifted") && text.contains("not stored"), "{text}");
    // The answer was charged, and the same window asked again is charged again: the message must not
    // send the operator back to the same command.
    assert!(text.contains("charged") && text.contains("charged again"), "{text}");
    assert!(!text.contains("run the same command again"), "{text}");
    assert_eq!(rig.source.lock().spent, 1 + 6, "the first read and the chunk's bound are counted");
    rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).expect("empty is fine");
    let asked = rig.api.calls()[3].1.clone();
    assert!(asked.contains("from=2026-10-23T06:00:00"), "asked by the rule again: {asked}");
}

/// **A first read that contradicts the rule stops the request before any window is asked**: the account's
/// clock is not the one the lane asks by (the UK rule moved, or this is another account), so nothing past
/// the one-point first read is sent, and the sentence carries both numbers.
#[test]
fn a_first_read_that_contradicts_the_rule_stops_before_any_chunk_or_snapshot() {
    let mut first = eur_first_read();
    first["prices"][0]["snapshotTime"] = json!("2026/10/23 23:00:00"); // 420 minutes on 2026-10-23
    let rig = start(vec![Ok(first)]);

    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();

    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    for part in ["first read", "420 minutes", "is 480", "cannot fetch until the rule is corrected"]
    {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert_eq!(rig.api.calls().len(), 1, "the first read only: no snapshot, no chunk");
    assert_eq!(rig.source.lock().spent, 1);
}

// ── the account's clock moves: every chunk is asked in the offset valid at it ───────────────────

/// 2026-10-26T12:00:00Z, the day after the UK clock change that follows the acceptance run: the account
/// is at 420 minutes.
fn after_the_october_change() -> i64 {
    utc("2026-10-26T12:00:00")
}

/// 2027-01-15T12:00:00Z: winter, after all three changes the tests cut chunks at.
fn mid_january_2027() -> i64 {
    utc("2027-01-15T12:00:00")
}

/// A flat row at `ms`, stamped on both clocks `offset` minutes apart (`snapshotTime` written with slashes
/// as IG writes it).
fn at(ms: i64, offset: i64) -> Value {
    let local = stamp_text(ms + offset * MINUTE_MS).replace('-', "/").replace('T', " ");
    flat(&stamp_text(ms), &local)
}

/// `n` hourly rows from `first` (a UTC stamp text), on a clock `offset` minutes from UTC.
fn hours(first: &str, n: i64, offset: i64) -> Vec<Value> {
    (0..n).map(|h| at(utc(first) + h * HOUR_MS, offset)).collect()
}

/// The first read of EURUSD with the stamp pair given.
fn first_read_stamped(utc_text: &str, local_text: &str) -> Value {
    let mut first = eur_first_read();
    first["prices"][0]["snapshotTimeUTC"] = json!(utc_text);
    first["prices"][0]["snapshotTime"] = json!(local_text);
    first
}

/// The first read on the winter side, as this mock states it: a newest daily bar stamped 17:00Z, local
/// midnight at +420. Real IG stamps its winter daily bars 16:00Z (23:00 local), measured on 2026-10-24; the
/// winter-side mocks below keep the 17:00Z stamp they were written with.
fn eur_first_read_winter() -> Value {
    first_read_stamped("2026-10-25T17:00:00", "2026/10/26 00:00:00")
}

fn queries(rig: &Rig) -> Vec<String> {
    rig.api.calls().into_iter().skip(2).map(|(_, query)| query).collect()
}

/// **Run W of the acceptance run, repaired.** A daily chunk wholly on the winter side (2025-12-18 to
/// 2026-01-14), asked on a summer-time day: the lane used to ask it in 480, be answered in 420, fail and be
/// charged 24 points. It is now asked in 420 (the clock valid there), EXACTLY its own window (no hour wider
/// at either edge: the owner's ruling of 2026-10-25), and stored.
#[test]
fn a_chunk_wholly_on_the_other_side_of_a_change_from_now_is_asked_in_its_own_offset() {
    let window = (utc("2025-12-18T00:00:00"), utc("2026-01-14T23:59:59") + 999);
    let rows = vec![
        at(utc("2025-12-18T17:00:00"), 420),
        at(utc("2026-01-05T17:00:00"), 420),
        at(utc("2026-01-14T17:00:00"), 420),
    ];
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(rows, 9_000))]);

    let bars = fetch_ok(&rig, EURUSD, "1d", window);

    assert_eq!(
        queries(&rig),
        vec!["resolution=DAY&from=2025-12-18T07:00:00&to=2026-01-15T06:59:59&pageSize=0"]
    );
    assert_eq!(bars.len(), 3);
    assert!(bars.iter().all(|b| b.ts.rem_euclid(DAY_MS) == 17 * HOUR_MS), "stored at 17:00Z");
    // The cost the gate is told is the window's own: 28 days reach 28 daily rows at most.
    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 28]);
    assert_eq!(rig.source.lock().spent, 1 + 3, "charged for its rows, not for a failed chunk");
}

/// **The chunk the acceptance run died on (run B, chunk 3), repaired.** 2025-10-23 to 2025-11-19 holds the
/// UK clock change of 2025-10-26 01:00 UTC: it is cut AT it, asked in two pieces, each in the offset valid
/// at it (480, then 420), and returned whole, in order. The gate sees one precheck for the chunk and one
/// admission per piece, the chunk's key is the caller's and does not change, and the points charged are
/// the rows of the two pieces.
#[test]
fn the_chunk_that_holds_the_october_change_is_cut_at_it_and_asked_in_two_offsets() {
    let window = (utc("2025-10-23T00:00:00"), utc("2025-11-19T23:59:59") + 999);
    let before = vec![at(utc("2025-10-23T16:00:00"), 480), at(utc("2025-10-24T16:00:00"), 480)];
    let after = vec![at(utc("2025-10-26T17:00:00"), 420), at(utc("2025-11-19T17:00:00"), 420)];
    let rig = start(vec![
        Ok(eur_first_read()),
        Ok(eur_snapshot()),
        Ok(body(before, 9_000)),
        Ok(body(after, 8_996)),
    ]);

    let bars = fetch_ok(&rig, EURUSD, "1d", window);

    assert_eq!(
        queries(&rig),
        vec![
            // 00:00Z to 00:59:59.999Z on the 26th, at +480: the last second before the change.
            "resolution=DAY&from=2025-10-23T08:00:00&to=2025-10-26T08:59:59&pageSize=0",
            // 01:00Z on the 26th at +420 (the new offset's first instant) to the chunk's own end,
            // asked exactly: this side is the far one from a summer-time "now", and nothing is widened.
            "resolution=DAY&from=2025-10-26T08:00:00&to=2025-11-20T06:59:59&pageSize=0",
        ]
    );
    let stamps: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    assert_eq!(
        stamps,
        vec![
            utc("2025-10-23T16:00:00"),
            utc("2025-10-24T16:00:00"),
            utc("2025-10-26T17:00:00"),
            utc("2025-11-19T17:00:00"),
        ],
        "both pieces, in order, under the stamps of their own clocks"
    );
    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 4, 25]);
    assert_eq!(
        *rig.gate.whats.lock().unwrap(),
        vec![
            format!("the first daily bars of {EURUSD}"),
            format!("the market prices of {EURUSD}"),
            "1d bars from 2025-10-23 to 2025-10-26".to_string(),
            "1d bars from 2025-10-26 to 2025-11-19".to_string(),
        ]
    );
    assert_eq!(
        *rig.gate.prechecked.lock().unwrap(),
        vec![(28, "1d bars from 2025-10-23 to 2025-11-19".to_string())],
        "one precheck, for the chunk"
    );
    assert_eq!(rig.source.lock().spent, 1 + 2 + 2);
}

/// **Tomorrow, repaired.** The same chunk on 2026-10-26, when the account is at 420: the piece before the
/// change (480) is now the far side and is asked exactly like the piece after it, which the lane would ask
/// as a recent window: each piece in its own offset, neither widened.
#[test]
fn after_the_october_change_the_same_chunk_is_asked_the_other_way_round() {
    let window = (utc("2025-10-23T00:00:00"), utc("2025-11-19T23:59:59") + 999);
    let before = vec![at(utc("2025-10-23T16:00:00"), 480), at(utc("2025-10-24T16:00:00"), 480)];
    let after = vec![at(utc("2025-10-26T17:00:00"), 420), at(utc("2025-11-19T17:00:00"), 420)];
    let rig = start_with(
        vec![
            Ok(eur_first_read_winter()),
            Ok(eur_snapshot()),
            Ok(body(before, 9_000)),
            Ok(body(after, 8_996)),
        ],
        ScriptedGate::default(),
        after_the_october_change,
    );

    let bars = fetch_ok(&rig, EURUSD, "1d", window);

    assert_eq!(
        queries(&rig),
        vec![
            "resolution=DAY&from=2025-10-23T08:00:00&to=2025-10-26T08:59:59&pageSize=0",
            "resolution=DAY&from=2025-10-26T08:00:00&to=2025-11-20T06:59:59&pageSize=0",
        ]
    );
    assert_eq!(bars.len(), 4);
    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 4, 25]);

    // And a window wholly after the change, read from a winter-side clock, goes out at 420.
    let rows = hours("2026-11-02T00:00:00", 3, 420);
    let rig = start_with(
        vec![Ok(eur_first_read_winter()), Ok(eur_snapshot()), Ok(body(rows, 9_000))],
        ScriptedGate::default(),
        mid_january_2027,
    );
    let recent = (utc("2026-11-02T00:00:00"), utc("2026-11-02T05:59:59") + 999);
    assert_eq!(fetch_ok(&rig, EURUSD, "1h", recent).len(), 3);
    assert_eq!(
        queries(&rig),
        vec!["resolution=HOUR&from=2026-11-02T07:00:00&to=2026-11-02T12:59:59&pageSize=0"]
    );
}

/// **An hourly chunk across the March change, with the grid the lane really uses** (21 days from
/// 1970-01-01, here 2026-03-26 to 2026-04-15): 420 before the change (far from a summer-time "now"),
/// 480 after it. No wall-clock time the lane writes falls in the hour the March change skips (07:59:59
/// is the last second before it, 09:00:00 the first after). Neither piece is asked an hour wider, so every
/// row charged is a row stored (the 2026-10-24 measurement: 45 and 314, hour-exact against FXCM).
#[test]
fn an_hourly_chunk_across_the_march_change_is_asked_in_420_then_480() {
    let chunk_start = 20_538 * DAY_MS; // 2026-03-26, a multiple of 21 days
    assert_eq!(chunk_start, utc("2026-03-26T00:00:00"));
    let window = (chunk_start, chunk_start + 21 * DAY_MS - 1);
    let before = vec![at(utc("2026-03-26T00:00:00"), 420), at(utc("2026-03-27T10:00:00"), 420)];
    let after = vec![at(utc("2026-03-29T20:00:00"), 480), at(utc("2026-04-15T23:00:00"), 480)];
    let rig = start(vec![
        Ok(eur_first_read()),
        Ok(eur_snapshot()),
        Ok(body(before, 9_000)),
        Ok(body(after, 8_995)),
    ]);

    let bars = fetch_ok(&rig, EURUSD, "1h", window);

    assert_eq!(
        queries(&rig),
        vec![
            "resolution=HOUR&from=2026-03-26T07:00:00&to=2026-03-29T07:59:59&pageSize=0",
            "resolution=HOUR&from=2026-03-29T09:00:00&to=2026-04-16T07:59:59&pageSize=0",
        ]
    );
    let stamps: Vec<i64> = bars.iter().map(|b| b.ts).collect();
    assert_eq!(
        stamps,
        vec![
            utc("2026-03-26T00:00:00"),
            utc("2026-03-27T10:00:00"),
            utc("2026-03-29T20:00:00"),
            utc("2026-04-15T23:00:00"),
        ],
        "the chunk's own rows, in order"
    );
    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 73, 431]);
    assert_eq!(rig.source.lock().spent, 1 + 2 + 2, "every row charged is stored");
}

/// **All three recorded changes, one chunk each, both sides**: a daily chunk around the change is asked in
/// two pieces whose `from` and `to` are exactly the rule's offset at that piece, with no request sent in
/// an offset the rule does not give there, and no edge asked wider than the window.
#[test]
fn every_recorded_change_is_asked_in_the_offset_valid_on_each_side_of_it() {
    for (change_text, before, after) in [
        ("2025-10-26T01:00:00", 480, 420),
        ("2026-03-29T01:00:00", 420, 480),
        ("2026-10-25T01:00:00", 480, 420),
    ] {
        let change = utc(change_text);
        let window = (change - 3 * DAY_MS, change + 24 * DAY_MS);
        let stamp = |ms: i64, offset: i64| stamp_text(ms + offset * MINUTE_MS);
        let rig = start_with(
            vec![
                Ok(first_read_stamped("2027-01-14T17:00:00", "2027/01/15 00:00:00")),
                Ok(eur_snapshot()),
                Ok(body(vec![at(window.0 + 20 * HOUR_MS, before)], 9_000)),
                Ok(body(vec![at(change + 20 * HOUR_MS, after)], 8_999)),
            ],
            ScriptedGate::default(),
            mid_january_2027,
        );

        fetch_ok(&rig, EURUSD, "1h", window);

        let asked = queries(&rig);
        assert_eq!(asked.len(), 2, "{change_text}: two pieces");
        // Piece 1: [start, change - 1 ms] at the offset before, the wall clock's last second before the
        // change. Piece 2: [change, end] at the offset after.
        assert_eq!(
            asked[0],
            format!(
                "resolution=HOUR&from={}&to={}&pageSize=0",
                stamp(window.0, before),
                stamp(change - 1, before)
            ),
            "{change_text}: the piece before is asked in {before}"
        );
        assert_eq!(
            asked[1],
            format!(
                "resolution=HOUR&from={}&to={}&pageSize=0",
                stamp(change, after),
                stamp(window.1, after)
            ),
            "{change_text}: the piece after is asked in {after}"
        );
    }
}

/// **No hedge: a far-side hourly chunk is asked its own six hours and charged six points.** IG reads the
/// wall clock it is given at the offset of the INSTANT that wall clock names (measured 2026-10-24 on the
/// March change, hour-exact against FXCM), so a winter-side chunk asked on a summer-time day needs no extra
/// hour at either edge, and the lane does not ask for one. Never observed: the summer-to-winter direction
/// and an end edge; if the first chunk across 2026-10-25 shows a hole (after 2026-11-12), the hedge returns.
#[test]
fn a_far_side_chunk_is_asked_its_own_hours_and_no_more() {
    let window = (utc("2026-01-15T00:00:00"), utc("2026-01-15T05:59:59") + 999);
    let rig = start(vec![
        Ok(eur_first_read()),
        Ok(eur_snapshot()),
        Ok(body(hours("2026-01-15T00:00:00", 6, 420), 9_000)),
    ]);

    let bars = fetch_ok(&rig, EURUSD, "1h", window);

    assert_eq!(
        queries(&rig),
        vec!["resolution=HOUR&from=2026-01-15T07:00:00&to=2026-01-15T12:59:59&pageSize=0"]
    );
    assert_eq!(
        bars.iter().map(|b| b.ts).collect::<Vec<_>>(),
        (0..6).map(|h| window.0 + h * HOUR_MS).collect::<Vec<_>>()
    );
    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 6]);
    assert_eq!(rig.source.lock().spent, 1 + 6, "every row charged is stored");
}

/// A chunk whose SECOND piece fails says that the first was asked and charged and is not stored (a chunk
/// is stored whole or not at all), and nothing is asked after the failure.
#[test]
fn a_failure_on_the_second_piece_says_the_first_was_charged_and_is_not_stored() {
    let window = (utc("2025-10-23T00:00:00"), utc("2025-11-19T23:59:59") + 999);
    let before = vec![at(utc("2025-10-23T16:00:00"), 480)];
    let bad = IgApiError { status: 400, message: "error.pricing.invalid-date-range".into() };
    let rig =
        start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(before, 9_000)), Err(bad)]);

    let err = rig.source.fetch(EURUSD, "1d", window.0, window.1).unwrap_err();

    let SourceError::Fetch(text) = &err else { panic!("{err:?}") };
    for part in ["400", "invalid-date-range", "earlier piece", "WAS asked and charged", "whole"] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert_eq!(rig.api.calls().len(), 4, "nothing after the failure");

    // A failure on the FIRST piece has no earlier piece to speak of.
    let bad = IgApiError { status: 400, message: "error.pricing.invalid-date-range".into() };
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Err(bad)]);
    let SourceError::Fetch(text) = rig.source.fetch(EURUSD, "1d", window.0, window.1).unwrap_err()
    else {
        panic!("a Fetch")
    };
    assert!(!text.contains("earlier piece"), "{text}");
}

/// The empty-chunk rule judges the CHUNK, not a piece: a straddling chunk with bars in one piece only is
/// stored (an instrument listed after the change), one with none in either is refused as before.
#[test]
fn the_empty_chunk_rule_judges_the_whole_chunk_not_a_piece() {
    let window = (utc("2025-10-23T00:00:00"), utc("2025-11-19T23:59:59") + 999);
    let rows = vec![at(utc("2025-11-19T17:00:00"), 420)];
    let rig = start(vec![
        Ok(eur_first_read()),
        Ok(eur_snapshot()),
        Ok(body(vec![], 9_000)),
        Ok(body(rows, 8_999)),
    ]);
    assert_eq!(fetch_ok(&rig, EURUSD, "1d", window).len(), 1, "bars in the second piece only");

    let rig = start(vec![
        Ok(eur_first_read()),
        Ok(eur_snapshot()),
        Ok(body(vec![], 9_000)),
        Ok(body(vec![], 9_000)),
    ]);
    let SourceError::Fetch(text) = rig.source.fetch(EURUSD, "1d", window.0, window.1).unwrap_err()
    else {
        panic!("a Fetch")
    };
    assert!(text.contains("no bars") && text.contains("weekdays"), "{text}");
}

/// **A window the rule cannot name is refused before anything is asked**: no login, no request, no point.
#[test]
fn a_window_outside_the_years_of_the_rule_is_refused_before_a_login() {
    let rig = start(vec![]);
    for window in [
        (0, DAY_MS),
        (utc("1995-12-01T00:00:00"), utc("1996-02-01T00:00:00")),
        (utc("2026-01-01T00:00:00"), i64::MAX),
    ] {
        let err = rig.source.fetch(EURUSD, "1h", window.0, window.1).unwrap_err();
        assert!(matches!(err, SourceError::Refused(ref why) if why.contains("1996")), "{err:?}");
    }
    assert_eq!(rig.logins.load(Ordering::SeqCst), 0);
    assert!(rig.api.calls().is_empty());
    assert!(rig.gate.prechecked.lock().unwrap().is_empty());
}

// ── the scale ──────────────────────────────────────────────────────────────────────────────────

fn newest_close_mid(rows: Vec<Value>) -> f64 {
    read_rows(&body(rows, 9_000)).expect("rows").bars.last().expect("a row").close
}

/// **Price units, per class, from the probe's decoded rows**: the newest close of the EURUSD, DAX and
/// WTI hours is within the tolerance of the price the market snapshot gives (`level x scalingFactor x
/// pip`), so the history is in the snapshot's units and is not rescaled.
#[test]
fn the_history_is_in_price_units_for_the_three_classes_the_probe_read() {
    let dax_rows = vec![
        row(
            "2026-10-22T22:00:00",
            "2026/10/23 06:00:00",
            (25380.2, 25387.2),
            (25410.8, 25417.8),
            (25371.9, 25378.9),
            (25398.4, 25405.4),
            0,
        ),
        row(
            "2026-10-23T17:00:00",
            "2026/10/24 01:00:00",
            (25155.4, 25158.2),
            (25156.9, 25159.7),
            (25116.6, 25119.4),
            (25131.0, 25133.8),
            441,
        ),
    ];
    let wti_rows = vec![
        row(
            "2026-10-22T22:00:00",
            "2026/10/23 06:00:00",
            (9886.5, 9894.3),
            (9899.7, 9906.7),
            (9861.0, 9864.0),
            (9877.1, 9881.9),
            917,
        ),
        row(
            "2026-10-23T17:00:00",
            "2026/10/24 01:00:00",
            (10259.5, 10262.3),
            (10265.6, 10268.9),
            (10236.9, 10241.3),
            (10260.8, 10265.6),
            7519,
        ),
    ];
    let dax_snapshot = json!({"instrument": {"onePipMeans": "1 Index Point"}, "snapshot": {"marketStatus": "TRADEABLE", "bid": 25130.0, "offer": 25132.8, "scalingFactor": 1}});
    let wti_snapshot = json!({"instrument": {"onePipMeans": "1 cent per barrel"}, "snapshot": {"marketStatus": "TRADEABLE", "bid": 10263.1, "offer": 10267.9, "scalingFactor": 1}});

    let cases = [
        (EURUSD, newest_close_mid(probe_a_rows()), eur_snapshot(), 1.1193),
        (DAX, newest_close_mid(dax_rows), dax_snapshot, 25131.4),
        (WTI, newest_close_mid(wti_rows), wti_snapshot, 10265.5),
    ];
    for (epic, close, snapshot, lane_price) in cases {
        assert_eq!(compare_scale(epic, close, &snapshot), Scale::Agrees, "{epic}");
        let priced = snapshot_price(&snapshot).expect("a tradeable snapshot");
        assert!(
            (priced - lane_price).abs() < 1e-6 * lane_price.max(1.0),
            "{epic}: the probe's lane price {lane_price}, got {priced}"
        );
        assert!(
            (close / lane_price - 1.0).abs() < 0.001,
            "{epic}: the probe measured ratios within a tenth of a percent"
        );
    }
}

/// **A scale that disagrees is REFUSED, never rescaled.** The 2020 USDJPY capture's snapshot is
/// pip-scaled (a bid of 10806.1 beside `scalingFactor` 1 and a pip of 0.01, so 108.06): a history
/// close of 108 agrees with the price the snapshot implies, a history close of 10806 does not.
#[test]
fn a_history_off_by_the_pip_is_refused_and_a_shut_market_skips_the_check() {
    let jpy = json!({"instrument": {"onePipMeans": "0.01 JPY/USD"}, "snapshot": {"marketStatus": "TRADEABLE", "bid": 10806.1, "offer": 10806.3, "scalingFactor": 1}});
    assert_eq!(compare_scale("CS.D.USDJPY.MINI.IP", 108.07, &jpy), Scale::Agrees);
    let Scale::Disagrees(why) = compare_scale("CS.D.USDJPY.MINI.IP", 10_806.0, &jpy) else {
        panic!("a pip-scaled history must disagree")
    };
    assert!(why.contains("REFUSED") && why.contains("never rescaled"), "{why}");

    let shut = json!({"instrument": {"onePipMeans": "0.0001 USD/EUR"}, "snapshot": {"marketStatus": "CLOSED", "bid": null, "offer": null, "scalingFactor": 10000}});
    assert!(matches!(compare_scale(EURUSD, 1.1193, &shut), Scale::Unchecked(_)));
    let no_factor = json!({"instrument": {"onePipMeans": "0.0001 USD/EUR"}, "snapshot": {"marketStatus": "TRADEABLE", "bid": 1.1, "offer": 1.2}});
    assert!(matches!(compare_scale(EURUSD, 1.1193, &no_factor), Scale::Unchecked(_)));
}

/// Through the source: a disagreeing snapshot refuses the whole request at the first read (`Refused`,
/// nothing stored), a shut market lets it through.
#[test]
fn the_source_refuses_a_disagreeing_scale_and_passes_a_shut_market() {
    let pip_scaled = json!({"instrument": {"onePipMeans": "0.0001 USD/EUR"}, "snapshot": {"marketStatus": "TRADEABLE", "bid": 11192.7, "offer": 11193.3, "scalingFactor": 10000}});
    let rig = start(vec![Ok(eur_first_read()), Ok(pip_scaled)]);
    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();
    assert!(matches!(err, SourceError::Refused(ref why) if why.contains("REFUSED")), "{err:?}");
    assert_eq!(rig.api.calls().len(), 2, "no chunk was requested");

    let shut = json!({"instrument": {"onePipMeans": "0.0001 USD/EUR"}, "snapshot": {"marketStatus": "CLOSED", "bid": null, "offer": null, "scalingFactor": 10000}});
    let rig = start(vec![Ok(eur_first_read()), Ok(shut), Ok(probe_d_answer())]);
    assert_eq!(fetch_ok(&rig, EURUSD, "1h", probe_d_window()).len(), 6);
}

// ── the gate, the refusals, the politeness ─────────────────────────────────────────────────────

/// The gate is asked before EVERY request with its cost, in order: the one-point first read, the free
/// market snapshot, then the chunk's estimate; it is told the account once, and learns the allowance
/// of every answer that carried one.
#[test]
fn the_gate_is_asked_before_every_request_and_learns_every_answer() {
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(probe_d_answer())]);

    fetch_ok(&rig, EURUSD, "1h", probe_d_window());

    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 6]);
    assert_eq!(
        *rig.gate.whats.lock().unwrap(),
        vec![
            format!("the first daily bars of {EURUSD}"),
            format!("the market prices of {EURUSD}"),
            "1h bars from 2026-10-22 to 2026-10-23".to_string(),
        ],
        "the gate is told in words what each request is for"
    );
    assert_eq!(*rig.gate.bound.lock().unwrap(), vec!["ABC12".to_string()]);
    let learned: Vec<u32> = rig.gate.learned.lock().unwrap().iter().map(|a| a.remaining).collect();
    assert_eq!(learned, vec![9_999, 9_845]);
    assert_eq!(rig.logins.load(Ordering::SeqCst), 1);
}

/// One login per source, however many chunks: a second chunk asks the gate and the gateway but not the
/// login, the first read or the snapshot again.
#[test]
fn a_second_chunk_logs_in_and_reads_nothing_twice() {
    let rig = start(vec![
        Ok(eur_first_read()),
        Ok(eur_snapshot()),
        Ok(probe_d_answer()),
        Ok(body(vec![], 9_800)),
    ]);

    fetch_ok(&rig, EURUSD, "1h", probe_d_window());
    fetch_ok(&rig, EURUSD, "1h", (probe_d_window().0 + DAY_MS, probe_d_window().1 + DAY_MS));

    assert_eq!(rig.logins.load(Ordering::SeqCst), 1);
    assert_eq!(rig.api.calls().len(), 4);
    assert_eq!(*rig.gate.admitted.lock().unwrap(), vec![1, 0, 6, 6]);
}

/// **A `403` allowance refusal is ONE polite stop**: the chunk's request is the last one sent (the
/// scripted gateway panics if anything follows), the error is the gate's sentence for IG's own code,
/// and the gate was told the code so no later request walks into the same wall.
#[test]
fn a_403_allowance_refusal_is_one_polite_stop_and_nothing_is_repeated() {
    let refusal =
        IgApiError { status: 403, message: "error.public-api.exceeded-api-key-allowance".into() };
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Err(refusal)]);

    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();

    assert_eq!(
        err,
        SourceError::Fetch("EXHAUSTED error.public-api.exceeded-api-key-allowance".to_string())
    );
    assert_eq!(rig.api.calls().len(), 3, "no retry");
    assert_eq!(
        *rig.gate.exhausted_with.lock().unwrap(),
        vec!["error.public-api.exceeded-api-key-allowance".to_string()]
    );
}

/// Any other failure is a `Fetch` that names IG's own code and says nothing is repeated; a transport
/// failure adds that the request may have been counted.
#[test]
fn another_failure_is_named_and_a_transport_failure_may_have_been_counted() {
    let bad = IgApiError { status: 400, message: "error.pricing.invalid-date-range".into() };
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Err(bad)]);
    let SourceError::Fetch(text) =
        rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err()
    else {
        panic!("a Fetch")
    };
    assert!(text.contains("400") && text.contains("invalid-date-range"), "{text}");
    assert!(!text.contains("counted"), "a plain 400 was not charged: {text}");

    let dropped = IgApiError { status: 0, message: "network error: timed out".into() };
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Err(dropped)]);
    let SourceError::Fetch(text) =
        rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err()
    else {
        panic!("a Fetch")
    };
    assert!(text.contains("may have reached IG") && text.contains("not repeated"), "{text}");
}

/// **A ledger refusal stops the chunk before it is asked**: the gate refuses the chunk's estimate, the
/// error is the gate's own sentence, and the chunk's request is never sent.
#[test]
fn a_ledger_refusal_means_the_chunk_is_never_requested() {
    let gate = ScriptedGate { refuse_above: Some(5), ..ScriptedGate::default() };
    let rig = start_with(vec![Ok(eur_first_read()), Ok(eur_snapshot())], gate, fixed_now);

    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();

    assert_eq!(err, SourceError::Fetch("the ledger stops 6 points".to_string()));
    assert_eq!(rig.api.calls().len(), 2, "the first read and the snapshot only");
}

/// **One request spends at most half the week.** The first read states the week (10000); a chunk whose
/// estimate alone is above half of it (four days of minutes: 5760) is not requested, and the sentence
/// says the cap, the numbers and what to do.
#[test]
fn one_request_may_not_spend_more_than_its_share_of_the_week() {
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot())]);

    let err = rig
        .source
        .fetch(EURUSD, "1m", utc("2026-10-19T00:00:00"), utc("2026-10-23T00:00:00") - 1)
        .unwrap_err();

    let SourceError::Fetch(text) = err else { panic!("a Fetch") };
    for part in
        ["were NOT requested", "5760", "50 percent", "5000 of 10000", "Run the same command again"]
    {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert_eq!(rig.api.calls().len(), 2);
}

/// The count carries across chunks of one request: the first chunk's rows are spent, and a second chunk
/// that would take the request over the cap is the one that stops.
#[test]
fn the_share_counts_the_rows_already_spent_by_this_request() {
    let day = |d: i64| utc("2026-10-19T00:00:00") + d * DAY_MS;
    let mut replies = vec![Ok(eur_first_read()), Ok(eur_snapshot())];
    // Three whole days of minutes, 1440 rows each, the dearest chunk of the pinned table.
    replies.extend((0..3).map(|d| Ok(body(minute_rows(day(d), 1_440), 9_000))));
    let rig = start(replies);

    for d in 0..3 {
        rig.source
            .fetch(EURUSD, "1m", day(d), day(d) + DAY_MS - 1)
            .unwrap_or_else(|e| panic!("day {d} fits the share: {e}"));
    }
    assert_eq!(rig.source.lock().spent, 1 + 3 * 1_440, "the first read's row and three days");

    // 4321 spent; a fourth day's 1440 would make 5761, over half of the week.
    let err = rig.source.fetch(EURUSD, "1m", day(3), day(3) + DAY_MS - 1).unwrap_err();
    let SourceError::Fetch(text) = err else { panic!("a Fetch") };
    assert!(text.contains("already used 4321 points") && text.contains("1440 more"), "{text}");
    assert_eq!(rig.api.calls().len(), 2 + 3, "the fourth day was not requested");
}

/// A stamp as IG writes `snapshotTimeUTC`: `2026-10-22T22:00:00`.
fn stamp_text(ms: i64) -> String {
    let full = vike_model::time::utc_rfc3339(ms.div_euclid(1000));
    full.trim_end_matches('Z').to_string()
}

/// `n` minute rows from `start_ms`, each stamped on both clocks (+480 minutes, `snapshotTime` written
/// with slashes as IG writes it); the prices are invented.
fn minute_rows(start_ms: i64, n: i64) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let ms = start_ms + i * 60_000;
            let local = stamp_text(ms + 480 * 60_000).replace('-', "/").replace('T', " ");
            row(&stamp_text(ms), &local, (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), 1)
        })
        .collect()
}

/// Everything that needs no IG is refused BEFORE the login: a bare pair, another case, an unserved
/// interval, an inverted window. Nothing is logged in, nothing asked, and the answer is `Refused`.
#[test]
fn a_non_epic_an_unserved_interval_and_an_inverted_window_are_refused_before_a_login() {
    let rig = start(vec![]);
    for (symbol, interval, window) in [
        ("EURUSD", "1h", (0, DAY_MS)),
        ("cs.d.eurusd.mini.ip", "1h", (0, DAY_MS)),
        (EURUSD, "1s", (0, DAY_MS)),
        (EURUSD, "1w", (0, DAY_MS)),
        (EURUSD, "1h", (DAY_MS, 0)),
    ] {
        let err = rig.source.fetch(symbol, interval, window.0, window.1).unwrap_err();
        assert!(matches!(err, SourceError::Refused(_)), "{symbol} {interval}: {err:?}");
    }
    assert_eq!(rig.logins.load(Ordering::SeqCst), 0);
    assert!(rig.api.calls().is_empty());
    assert!(rig.gate.admitted.lock().unwrap().is_empty());
}

/// A stop probe that has fired asks IG nothing and logs nothing in.
#[test]
fn a_fired_stop_probe_asks_nothing() {
    let rig = start(vec![]);

    let err = rig.source.fetch_until(EURUSD, "1h", 0, DAY_MS, &|| true).unwrap_err();

    assert!(matches!(err, SourceError::Fetch(ref why) if why.contains("cancelled")), "{err:?}");
    assert_eq!(rig.logins.load(Ordering::SeqCst), 0);
}

// ── before the login, the window, the empty chunk, the scale (the review's follow-up) ──────────

fn flat(utc: &str, local: &str) -> Value {
    row(utc, local, (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), (1.0, 1.1), 1)
}

/// **The ledger is asked BEFORE the login.** A wall IG already answered with, a drained week or a login
/// it refused a moment ago costs no login and no request: the gate's `precheck` is the first thing a
/// fetch asks, with the chunk's cost and words, and its refusal is the fetch's error.
#[test]
fn the_ledger_is_prechecked_before_a_login_and_a_refusal_costs_no_login() {
    let gate = ScriptedGate { precheck_refuses: true, ..ScriptedGate::default() };
    let rig = start_with(vec![], gate, fixed_now);

    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();

    assert_eq!(err, SourceError::Fetch("PRECHECK refused".to_string()));
    assert_eq!(rig.logins.load(Ordering::SeqCst), 0, "no login");
    assert!(rig.api.calls().is_empty(), "no request");
    assert_eq!(
        *rig.gate.prechecked.lock().unwrap(),
        vec![(6, "1h bars from 2026-10-22 to 2026-10-23".to_string())]
    );
    assert!(rig.gate.admitted.lock().unwrap().is_empty(), "the pace was not taken either");
}

/// **A login IG refused is told to the gate** (so the ledger can hold the next one off), and a credential
/// that is simply not stored is not: nothing was asked of IG, so there is nothing to lock out.
#[test]
fn a_refused_login_is_told_to_the_gate_and_a_missing_credential_is_not() {
    let gate = Arc::new(ScriptedGate::default());
    let refused: LoginFn = Arc::new(|| {
        Err(SourceError::Fetch(
            "ig: the history lane could not log in (IG error 401: error.security.invalid-details)"
                .to_string(),
        ))
    });
    let source = IgKlines::with_login(refused, Arc::clone(&gate) as Arc<dyn AllowanceGate>)
        .with_clock(fixed_now);
    let err = source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();
    assert!(matches!(err, SourceError::Fetch(_)), "{err:?}");
    assert_eq!(gate.login_failures.lock().unwrap().len(), 1);
    assert!(gate.login_failures.lock().unwrap()[0].contains("invalid-details"));

    let missing: LoginFn =
        Arc::new(|| Err(SourceError::Refused("ig history needs API_KEY, IDENTIFIER".to_string())));
    let source = IgKlines::with_login(missing, Arc::clone(&gate) as Arc<dyn AllowanceGate>)
        .with_clock(fixed_now);
    assert!(source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).is_err());
    assert_eq!(
        gate.login_failures.lock().unwrap().len(),
        1,
        "a missing credential is not a lockout"
    );
}

/// **A shifted window is never stored.** A bar one step outside the window is tolerated (the edge of an
/// inclusive range read on another clock); a bar two steps outside, before the start or after the end,
/// fails the chunk with nothing returned, because the ingest would silently drop the strays and spend
/// the chunk's key on a window IG answered for somewhere else.
#[test]
fn a_bar_more_than_a_step_outside_the_window_fails_the_chunk() {
    let (from, to) = probe_d_window(); // 22:00Z to 03:00:00.999Z
    let one_before = body(
        vec![
            flat("2026-10-22T21:00:00", "2026/10/23 05:00:00"),
            flat("2026-10-22T22:00:00", "2026/10/23 06:00:00"),
        ],
        9_000,
    );
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(one_before)]);
    let kept = fetch_ok(&rig, EURUSD, "1h", (from, to));
    assert_eq!(kept.len(), 1, "one step out is tolerated, and cut: the chunk holds its own bars");
    assert_eq!(kept[0].ts, from);

    for (stray_utc, stray_local) in [
        ("2026-10-22T20:00:00", "2026/10/23 04:00:00"),
        ("2026-10-23T05:00:00", "2026/10/23 13:00:00"),
    ] {
        let mut rows =
            vec![flat("2026-10-22T22:00:00", "2026/10/23 06:00:00"), flat(stray_utc, stray_local)];
        rows.sort_by_key(|r| r["snapshotTimeUTC"].as_str().unwrap_or_default().to_string());
        let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(rows, 9_000))]);
        let err = rig.source.fetch(EURUSD, "1h", from, to).unwrap_err();
        let SourceError::Fetch(text) = err else { panic!("a Fetch") };
        assert!(
            text.contains("outside the window") && text.contains("not stored"),
            "{stray_utc}: {text}"
        );
    }
}

/// **An empty chunk is final once stored, so a wide one that should have bars is not stored.** Three
/// weeks of hours with weekdays in them and no bars is refused (IG failed, or the instrument was not
/// listed yet), naming the weekdays; a weekend-only window of two days and a day chunk of minutes (a
/// holiday) may be empty.
#[test]
fn an_empty_wide_chunk_that_holds_weekdays_is_refused_and_a_weekend_or_a_day_may_be_empty() {
    // Monday 2026-09-28 to Sunday 2026-10-18: 21 days, 15 weekdays.
    let monday = utc("2026-09-28T00:00:00");
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(vec![], 9_000))]);
    let err = rig.source.fetch(EURUSD, "1h", monday, monday + 21 * DAY_MS - 1).unwrap_err();
    let SourceError::Fetch(text) = err else { panic!("a Fetch") };
    for part in ["no bars", "15 weekdays", "not stored", "first trading day"] {
        assert!(text.contains(part), "{part:?} in {text}");
    }

    // Saturday and Sunday: nothing to find, empty is the right answer.
    let saturday = utc("2026-10-17T00:00:00");
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(vec![], 9_000))]);
    assert!(
        rig.source.fetch(EURUSD, "1h", saturday, saturday + 2 * DAY_MS - 1).unwrap().is_empty()
    );

    // One UTC day, a Wednesday: a holiday is empty.
    let wednesday = utc("2026-09-30T00:00:00");
    let rig = start(vec![Ok(eur_first_read()), Ok(eur_snapshot()), Ok(body(vec![], 9_000))]);
    assert!(rig.source.fetch(EURUSD, "1h", wednesday, wednesday + DAY_MS - 1).unwrap().is_empty());
}

/// **The scale check fails CLOSED.** The probe read prices in price units for CURRENCIES, INDICES and
/// COMMODITIES, so for those a shut market or an unreadable snapshot only warns. For any other class, or
/// none, the history is stored only if the snapshot witnesses the scale: a shut market and a failed
/// snapshot are refusals. An unmeasured class whose snapshot AGREES is allowed.
#[test]
fn the_scale_check_fails_closed_for_a_class_the_probe_did_not_read() {
    let first_of = |class: &str| {
        let mut first = eur_first_read();
        if class.is_empty() {
            first.as_object_mut().unwrap().remove("instrumentType");
        } else {
            first["instrumentType"] = json!(class);
        }
        first
    };
    let shut = json!({"instrument": {"onePipMeans": "0.0001 USD/EUR"}, "snapshot": {"marketStatus": "CLOSED", "bid": null, "offer": null, "scalingFactor": 10000}});
    let unreadable = || IgApiError { status: 500, message: "boom".to_string() };

    for class in ["CURRENCIES", "INDICES", "COMMODITIES"] {
        let rig = start(vec![Ok(first_of(class)), Ok(shut.clone()), Ok(probe_d_answer())]);
        assert_eq!(fetch_ok(&rig, EURUSD, "1h", probe_d_window()).len(), 6, "{class}: shut");
        let rig = start(vec![Ok(first_of(class)), Err(unreadable()), Ok(probe_d_answer())]);
        assert_eq!(fetch_ok(&rig, EURUSD, "1h", probe_d_window()).len(), 6, "{class}: unreadable");
    }
    for class in ["SHARES", "BONDS", "OPT_COMMODITIES", ""] {
        for second in [Ok(shut.clone()), Err(unreadable())] {
            let rig = start(vec![Ok(first_of(class)), second]);
            let err =
                rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();
            let SourceError::Refused(why) = err else { panic!("{class:?}: a Refused") };
            for part in ["REFUSED", "scale", "price units"] {
                assert!(why.contains(part), "{class:?}: {part:?} in {why}");
            }
            assert_eq!(rig.api.calls().len(), 2, "{class:?}: no chunk was requested");
        }
    }
    let rig = start(vec![Ok(first_of("SHARES")), Ok(eur_snapshot()), Ok(probe_d_answer())]);
    assert_eq!(fetch_ok(&rig, EURUSD, "1h", probe_d_window()).len(), 6, "a witnessed scale passes");
}

/// A `403` allowance refusal on the market snapshot is IG's wall like any other: it goes through the
/// ledger's `exhausted` path and stops the fetch with the ledger's sentence, nothing more is asked.
#[test]
fn a_403_allowance_refusal_on_the_market_snapshot_goes_through_the_ledger_too() {
    let code = "error.public-api.exceeded-api-key-allowance";
    let refusal = IgApiError { status: 403, message: code.to_string() };
    let rig = start(vec![Ok(eur_first_read()), Err(refusal)]);

    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();

    assert_eq!(err, SourceError::Fetch(format!("EXHAUSTED {code}")));
    assert_eq!(*rig.gate.exhausted_with.lock().unwrap(), vec![code.to_string()]);
    assert_eq!(rig.api.calls().len(), 2, "the snapshot was the last request");
}

// ── the one session ────────────────────────────────────────────────────────────────────────────

thread_local! {
    /// The session cache's clock in these tests: one per test thread.
    static CLOCK: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

fn clock_now() -> i64 {
    CLOCK.with(std::cell::Cell::get)
}

fn set_clock(ms: i64) {
    CLOCK.with(|c| c.set(ms));
}

const MINUTE_MS: i64 = 60_000;

/// A session that counts the times it was ended and answers every `get` with an empty body, or with the
/// `401` a session the bridge's own ladder could not mend ends up with while `rejects` is set.
struct FakeSession {
    id: usize,
    ended: Arc<AtomicUsize>,
    rejects: Arc<std::sync::atomic::AtomicBool>,
}

impl PricesApi for FakeSession {
    fn account_id(&self) -> String {
        format!("S{}", self.id)
    }
    fn get(&self, _path: &str, _version: &str, _query: &str) -> Result<Value, IgApiError> {
        if self.rejects.load(Ordering::SeqCst) {
            return Err(IgApiError {
                status: 401,
                message: "error.security.client-token-invalid".to_string(),
            });
        }
        Ok(json!({}))
    }
}

impl HistorySession for FakeSession {
    fn log_out(&self) {
        self.ended.fetch_add(1, Ordering::SeqCst);
    }
}

struct SessionRig {
    logins: Arc<AtomicUsize>,
    ended: Arc<AtomicUsize>,
    /// While set, every session made by this rig answers `get` with a `401`.
    rejects: Arc<std::sync::atomic::AtomicBool>,
    login: SessionLoginFn,
}

/// A login that makes a numbered fake session; the first `fail` logins are refused.
fn session_rig(fail: usize) -> SessionRig {
    let (logins, ended) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let rejects = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (counted, ends, rejecting) =
        (Arc::clone(&logins), Arc::clone(&ended), Arc::clone(&rejects));
    let login: SessionLoginFn = Arc::new(move || {
        let id = counted.fetch_add(1, Ordering::SeqCst) + 1;
        if id <= fail {
            return Err(SourceError::Fetch("ig: the history lane could not log in".to_string()));
        }
        Ok(Arc::new(FakeSession { id, ended: Arc::clone(&ends), rejects: Arc::clone(&rejecting) })
            as Arc<dyn HistorySession>)
    });
    SessionRig { logins, ended, rejects, login }
}

fn cache() -> Arc<SessionCache> {
    set_clock(0);
    SessionCache::with(Duration::from_secs(20 * 60), clock_now)
}

/// **One session serves every request while it is in use**: a 15-year daily fetch is fifteen
/// calendar-year requests, and they share one login instead of leaving fifteen sessions open.
#[test]
fn one_session_serves_every_request_while_it_is_used() {
    let rig = session_rig(0);
    let login = cache().through(Arc::clone(&rig.login));

    let first = login().expect("the first login");
    set_clock(19 * MINUTE_MS);
    let second = login().expect("a second request");
    set_clock(38 * MINUTE_MS);
    let third = login().expect("a third request, 19 minutes after the second");

    assert_eq!(rig.logins.load(Ordering::SeqCst), 1, "one login for three requests");
    assert_eq!(rig.ended.load(Ordering::SeqCst), 0);
    assert_eq!([first.account_id(), second.account_id(), third.account_id()], ["S1", "S1", "S1"]);
}

/// **A session idle past the limit is ended and replaced**: the stale one is logged out before the fresh
/// login, once, and the limit is exclusive (exactly twenty idle minutes is still the same session).
#[test]
fn an_idle_session_is_ended_and_replaced() {
    let rig = session_rig(0);
    let login = cache().through(Arc::clone(&rig.login));
    login().expect("the first login");

    set_clock(20 * MINUTE_MS);
    assert_eq!(login().expect("exactly at the limit").account_id(), "S1");
    assert_eq!(rig.ended.load(Ordering::SeqCst), 0);

    set_clock(20 * MINUTE_MS + 20 * MINUTE_MS + 1);
    let fresh = login().expect("past the limit");

    assert_eq!(fresh.account_id(), "S2");
    assert_eq!(rig.logins.load(Ordering::SeqCst), 2);
    assert_eq!(rig.ended.load(Ordering::SeqCst), 1, "the stale session was logged out, once");
}

/// **Using a session keeps it fresh**: a long request that kept asking for 30 minutes is not a stale
/// session to the next request, so it is not logged out from under the request still using it.
#[test]
fn using_a_session_keeps_it_fresh() {
    let rig = session_rig(0);
    let login = cache().through(Arc::clone(&rig.login));
    let held = login().expect("the first login");

    set_clock(15 * MINUTE_MS);
    held.get("/prices/x", "3", "").expect("a request on the held session");
    set_clock(30 * MINUTE_MS);
    let next = login().expect("the next request");

    assert_eq!(next.account_id(), "S1", "15 idle minutes since the last use, not 30");
    assert_eq!(rig.logins.load(Ordering::SeqCst), 1);
    assert_eq!(rig.ended.load(Ordering::SeqCst), 0);
}

/// A refused login caches nothing: the next request logs in again (the ledger, not the cache, holds the
/// lockout off).
#[test]
fn a_failed_login_caches_nothing() {
    let rig = session_rig(1);
    let login = cache().through(Arc::clone(&rig.login));

    assert!(login().is_err());
    assert_eq!(login().expect("the second try").account_id(), "S2");
    assert_eq!(rig.logins.load(Ordering::SeqCst), 2);
    assert_eq!(rig.ended.load(Ordering::SeqCst), 0, "there was nothing to end");
}

/// **A clean shutdown ends the session**: dropping the cache (the lane's last owner) logs out what it
/// holds, once; an empty cache ends nothing.
#[test]
fn dropping_the_cache_ends_the_session() {
    let rig = session_rig(0);
    let cache = cache();
    let login = cache.through(Arc::clone(&rig.login));
    login().expect("a session");

    drop(login);
    assert_eq!(rig.ended.load(Ordering::SeqCst), 0, "the cache still has an owner");
    drop(cache);
    assert_eq!(rig.ended.load(Ordering::SeqCst), 1);

    let rig = session_rig(0);
    drop(self::cache());
    assert_eq!(rig.ended.load(Ordering::SeqCst), 0);
}

/// **A session the bridge's own ladder could not mend is stale AT ONCE.** A `401` that reaches the lease
/// is one the ladder's single re-login did not cure (a rotated password), and the session must not look
/// fresh for twenty more minutes while every retry sends another failed login: the next request replaces
/// it, logging the dead one out.
#[test]
fn a_401_the_ladder_could_not_mend_makes_the_session_stale_at_once() {
    let rig = session_rig(0);
    let login = cache().through(Arc::clone(&rig.login));
    let held = login().expect("a session");

    rig.rejects.store(true, Ordering::SeqCst);
    let err = held.get("/prices/x", "3", "").unwrap_err();
    assert_eq!(err.status, 401);
    rig.rejects.store(false, Ordering::SeqCst);

    // No idle time has passed, and the next request still does not get the dead session.
    let next = login().expect("a fresh login");
    assert_eq!(next.account_id(), "S2");
    assert_eq!((rig.logins.load(Ordering::SeqCst), rig.ended.load(Ordering::SeqCst)), (2, 1));
}

/// **A lease on a session the cache has ended does not use it.** A source that sat idle past the bound
/// still holds its lease; asking through it must not call a logged-out session (whose own ladder would
/// log in again, outside the cache, and leave a session nobody ends). It is told, plainly, that the
/// session was ended.
#[test]
fn a_lease_on_a_session_the_cache_ended_does_not_use_it() {
    let rig = session_rig(0);
    let login = cache().through(Arc::clone(&rig.login));
    let held = login().expect("a session");

    set_clock(21 * MINUTE_MS);
    let fresh = login().expect("a request past the idle bound");
    assert_eq!((fresh.account_id(), rig.ended.load(Ordering::SeqCst)), ("S2".to_string(), 1));

    let err = held.get("/prices/x", "3", "").unwrap_err();
    assert!(err.message.contains("was ended after it sat idle"), "{err}");
    assert!(![0, 401, 403].contains(&err.status), "a status the callers do not act on: {err}");
    assert!(fresh.get("/prices/x", "3", "").is_ok(), "the fresh session is untouched");
}

/// **A `401` on a price request is a refused login as far as the ledger is concerned**, whether it
/// answers the prices call or the market snapshot: the bridge's ladder already tried one re-login, so
/// the ledger holds the next login off instead of letting every retry send another failed one.
#[test]
fn a_401_on_a_request_is_told_to_the_gate_as_a_refused_login() {
    let dead = IgApiError { status: 401, message: "error.security.client-token-invalid".into() };
    let rig = start(vec![Err(dead.clone())]);
    let err = rig.source.fetch(EURUSD, "1h", probe_d_window().0, probe_d_window().1).unwrap_err();
    assert!(matches!(err, SourceError::Fetch(_)), "{err:?}");
    let told = rig.gate.login_failures.lock().unwrap().clone();
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].contains("401") && told[0].contains("re-login"), "{}", told[0]);

    let rig = start(vec![Ok(eur_first_read()), Err(dead), Ok(probe_d_answer())]);
    assert_eq!(fetch_ok(&rig, EURUSD, "1h", probe_d_window()).len(), 6, "a measured class goes on");
    assert_eq!(rig.gate.login_failures.lock().unwrap().len(), 1, "the snapshot's 401 was told too");
}

// ── the words ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_words_name_the_numbers_and_the_reset() {
    assert_eq!(span_words(604_799), "6 days 23 hours");
    assert_eq!(span_words(86_400), "1 day 0 hours");
    assert_eq!(span_words(3_720), "1 hour 2 minutes");
    assert_eq!(span_words(120), "2 minutes");
    assert_eq!(span_words(59), "under a minute");

    // Seen at 2026-10-23T17:22:23Z, resetting in 604799 s: Fri 2026-10-30 17:22.
    assert_eq!(
        reset_sentence(Some((NOW, 604_799))),
        "The allowance resets in 6 days 23 hours (about 2026-10-30T17:22Z)."
    );
    assert!(reset_sentence(None).contains("has not said"));

    let allowance = Allowance { remaining: 1_950, total: 10_000, resets_in_s: 604_799 };
    let chunk = chunk_words("1d", utc("2026-09-01T00:00:00"), utc("2026-09-28T23:59:59"));
    assert_eq!(chunk, "1d bars from 2026-09-01 to 2026-09-28");
    let stop = reserve_stop_words(&chunk, 28, &allowance, Some((NOW, 604_799)));
    for part in [
        "were NOT requested",
        "up to 28 points",
        "1950 of this week's 10000 points are left",
        "2000 (20 percent) are kept in reserve",
        "resets in 6 days 23 hours",
        "Run the same command again after the reset",
    ] {
        assert!(stop.contains(part), "{part:?} in {stop}");
    }

    let history = exhausted_words(
        "error.public-api.exceeded-account-historical-data-allowance",
        Some((NOW, 100_000)),
    );
    assert!(
        history.contains("historical-data allowance is spent")
            && history.contains("resets in 1 day 3 hours"),
        "{history}"
    );
    let minute = exhausted_words("error.public-api.exceeded-api-key-allowance", None);
    assert!(
        minute.contains("per-minute ceiling")
            && minute.contains("after a minute")
            && minute.contains("Nothing is retried"),
        "{minute}"
    );
}

// ── the read-only property ─────────────────────────────────────────────────────────────────────

/// **This file reads and writes nothing else.** IG offers no read-only key, so the history source is
/// held to `GET` by its trait (`PricesApi` has one method) and by this scan of its own production
/// text for every write verb and every order builder of the bridge. The vike-ops gate
/// (`the_ig_history_lane_only_reads`) holds both halves, this file and the gatewayhub's, to the same.
#[test]
fn the_history_source_has_no_way_to_write() {
    let source = include_str!("klines.rs");
    let production = source.split("#[cfg(test)]").next().expect("the production part");
    for banned in [
        ".post(",
        ".put(",
        ".delete(",
        "post_method_delete",
        "build_request",
        "build_close_request",
        "IgExecutionClient",
        "/positions",
        "/workingorders",
        "/confirms",
    ] {
        assert!(
            !production.contains(banned),
            "klines.rs names `{banned}`: a history source only reads"
        );
    }
    assert!(production.contains("fn get(&self"), "the control: the scan sees the real file");
}
