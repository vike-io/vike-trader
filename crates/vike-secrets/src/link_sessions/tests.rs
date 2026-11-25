//! `link_session` against a real store in a temp directory: what a row says, and that nothing else in the file moves.

use std::path::Path;
use std::time::{Duration, Instant};

use super::*;
use crate::store_locator::db_path_in;

const DAY_MS: i64 = 86_400_000;

fn store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp");
    crate::create_empty_store_for_test(&db_path_in(dir.path())).expect("a store");
    let conn = rusqlite::Connection::open(db_path_in(dir.path())).expect("open");
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('hyperliquid'), ('aster');
         INSERT INTO hub_key (name, value) VALUES ('A_KEY', 'a-fake-hub-key');",
    )
    .expect("seed");
    dir
}

fn key(route: &str) -> LinkKey {
    LinkKey {
        venue: "hyperliquid".to_string(),
        route: route.to_string(),
        plane: PLANE_TRADE,
        lane: "pump".to_string(),
    }
}

fn opened(route: &str, at_ms: i64) -> LinkEvent {
    LinkEvent::Opened { key: key(route), at_ms }
}

fn closed(route: &str, at_ms: i64, how: &str) -> LinkEvent {
    LinkEvent::Closed { key: key(route), at_ms, how: how.to_string(), ours: false }
}

fn rows(dir: &Path) -> Vec<LinkSessionRow> {
    read_link_sessions(&db_path_in(dir), 0, None, None).expect("read")
}

/// Every table but `link_session` as text, and the stamp: what a write here may never move.
fn everything_else(dir: &Path) -> String {
    let conn = rusqlite::Connection::open(db_path_in(dir)).expect("open");
    let mut tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name <> 'link_session' ORDER BY name")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("tables")
        .collect::<Result<_, _>>()
        .expect("names");
    tables.retain(|t| t != "sqlite_sequence");
    let mut out = String::new();
    for table in tables {
        let mut q =
            conn.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid")).expect("select");
        let width = q.column_count();
        let body: Vec<String> = q
            .query_map([], |r| {
                (0..width)
                    .map(|i| r.get_ref(i).map(|v| format!("{v:?}")))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|cells| cells.join("|"))
            })
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("cells");
        out.push_str(&format!("{table}: {body:?}\n"));
    }
    let stamp: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("stamp");
    out.push_str(&format!("user_version {stamp}"));
    out
}

/// The owner's two numbers are these two columns: how long it lived, and how long until the next one opened.
#[test]
fn a_session_is_one_row_and_the_next_open_says_how_long_it_took() {
    let dir = store();
    write_link_events(
        dir.path(),
        &[opened("hyperliquid", 1_000), closed("hyperliquid", 241_000, "server closed: 1000 bye")],
    )
    .expect("write");
    write_link_events(dir.path(), &[opened("hyperliquid", 242_500)]).expect("write");

    let got = rows(dir.path());
    assert_eq!(got.len(), 2, "{got:?}");
    let (newest, first) = (&got[0], &got[1]);
    assert_eq!((first.lived_s, first.next_open_s), (Some(240.0), Some(1.5)));
    assert_eq!(first.how.as_deref(), Some("server closed: 1000 bye"));
    assert_eq!(
        (first.venue.as_str(), first.route.as_str(), first.plane.as_str()),
        ("hyperliquid", "hyperliquid", "trade")
    );
    assert!(!first.ours);
    assert_eq!(
        (newest.closed_ms, newest.lived_s, newest.next_open_s),
        (None, None, None),
        "still open"
    );
}

/// A status can repeat (a joiner hears `Live` again), and a close of nothing open is no row.
#[test]
fn an_open_on_a_row_still_open_closes_it_as_lost_and_a_close_of_nothing_is_no_row() {
    let dir = store();
    write_link_events(dir.path(), &[closed("hyperliquid", 500, "nothing was open")])
        .expect("write");
    assert!(rows(dir.path()).is_empty(), "a close of nothing is no row");

    // Its close was lost (a full queue, a busy store): two sessions, one drop, and no life that spans the outage.
    write_link_events(dir.path(), &[opened("hyperliquid", 1_000), opened("hyperliquid", 9_000)])
        .expect("write");
    let got = rows(dir.path());
    assert_eq!(got.len(), 2, "{got:?}");
    let (new, lost) = (&got[0], &got[1]);
    assert_eq!((new.opened_ms, new.closed_ms), (9_000, None));
    assert_eq!((lost.opened_ms, lost.closed_ms), (1_000, Some(9_000)));
    assert_eq!(
        (lost.lived_s, lost.next_open_s, lost.ours),
        (None, None, false),
        "a drop, of no known length"
    );
    assert!(lost.how.as_deref().is_some_and(|how| how.contains("not recorded")), "{lost:?}");
}

/// ⚠ **The review's two shapes, and their kin: nothing shaped like a key reaches `how`, whatever it wears.** A whitelist:
/// a word stays only if it reads as a word, so a shape nobody thought of is dropped, and the rest of the reason still reads.
#[test]
fn nothing_shaped_like_a_key_reaches_how_whatever_it_wears() {
    let key = "pqia91ma19a5s61cv6a81va65sdf19v8a65a1a5s61cv6as5v1a6s5v1a6sdf1vaAA";
    for shape in [
        format!("fstream.example.com/ws/{key}?signature=x"),
        "a=1&sig=shortvalue1".to_string(),
        format!("wss://fstream.example.com/ws/{key}"),
        format!("listenKey={key}"),
        key.to_string(),
        "abc123def456ghi".to_string(),
        "operator:hunter2@host".to_string(),
    ] {
        let kept = plain_words(&format!("closed: {shape} after 5 s"));
        assert_eq!(kept, "closed: <dropped> after 5 s", "{shape}");
    }
}

/// A session counts in a window if it was open at any time in it: a 10-day session that dropped yesterday is a drop of this
/// week, and one still running is running.
#[test]
fn a_session_counts_in_a_window_if_it_overlapped_it() {
    let dir = store();
    write_link_events(
        dir.path(),
        &[
            opened("hyperliquid#OVER", 100),
            closed("hyperliquid#OVER", 200, "over before the window"),
            opened("hyperliquid#SPANS", 300),
            closed("hyperliquid#SPANS", 9_000, "ended inside it"),
            opened("hyperliquid#RUNS", 400),
            opened("hyperliquid#NEW", 8_000),
        ],
    )
    .expect("write");
    let db = db_path_in(dir.path());
    let mut routes: Vec<String> = read_link_sessions(&db, 5_000, None, None)
        .expect("read")
        .into_iter()
        .map(|r| r.route)
        .collect();
    routes.sort_unstable();
    assert_eq!(routes, ["hyperliquid#NEW", "hyperliquid#RUNS", "hyperliquid#SPANS"]);
}

/// ⚠ A clock that jumped far ahead of everything this table holds (a bad RTC before NTP) deletes nothing; a plain rule, and
/// the first row written on a clock the table can believe lets the retention run again.
#[test]
fn a_clock_far_ahead_of_the_newest_row_prunes_nothing() {
    let dir = store();
    let then = 1_000 * DAY_MS;
    write_link_events(dir.path(), &[opened("hyperliquid", then)]).expect("write");
    let jumped = then + (LINK_SESSION_RETENTION_DAYS + 200) * DAY_MS;
    prune_link_sessions(dir.path(), jumped).expect("prune");
    assert_eq!(rows(dir.path()).len(), 1, "history survives a jump");

    write_link_events(dir.path(), &[opened("hyperliquid#FRESH", jumped)]).expect("write");
    prune_link_sessions(dir.path(), jumped).expect("prune");
    let got = rows(dir.path());
    assert_eq!(got.len(), 1, "a newest row this close to the clock: the rule runs: {got:?}");
    assert_eq!(got[0].route, "hyperliquid#FRESH");
}

/// One account's two sockets are two histories, and two accounts never share a row.
#[test]
fn a_gap_is_measured_within_one_socket() {
    let dir = store();
    let other = LinkKey { lane: "other-pump".to_string(), ..key("hyperliquid") };
    write_link_events(
        dir.path(),
        &[
            opened("hyperliquid", 1_000),
            closed("hyperliquid", 2_000, "closed"),
            LinkEvent::Opened { key: other, at_ms: 2_500 },
            opened("hyperliquid#LIVE", 2_600),
        ],
    )
    .expect("write");
    let got = rows(dir.path());
    let first = got.iter().find(|r| r.opened_ms == 1_000).expect("the first");
    assert_eq!(
        first.next_open_s, None,
        "another socket's open is not this one's reconnect: {got:?}"
    );
}

/// ⚠ **A key-shaped URL never lands in a row**, whatever a bridge's error text carried. A listenKey in a URL is the
/// incident: a tungstenite error spells the URL it dialled.
#[test]
fn a_key_shaped_url_never_lands_in_a_row() {
    let listen_key = "pqia91ma19a5s61cv6a81va65sdf19v8a65a1a5s61cv6as5v1a6s5v1a6sdf1vaAA";
    let token = "eyJhbGciOiJIUzI1NiJ9abcdefghijklmnop";
    let how = format!(
        "URL error: wss://fstream.binance.com/ws/{listen_key} failed; https://x.example/a?k={token} then \
         listenKey={listen_key} token={token} and a bare {listen_key} and code=1006"
    );
    let dir = store();
    write_link_events(dir.path(), &[opened("hyperliquid", 1), closed("hyperliquid", 2, &how)])
        .expect("write");

    let raw: String = rusqlite::Connection::open(db_path_in(dir.path()))
        .expect("open")
        .query_row("SELECT how FROM link_session", [], |r| r.get(0))
        .expect("how");
    assert!(!raw.contains(listen_key) && !raw.contains(token), "{raw}");
    assert!(!raw.contains("://") && !raw.contains("binance.com"), "{raw}");
    assert!(raw.contains("URL error:") && raw.contains("code=1006"), "the rest still reads: {raw}");
}

/// A close reason stays whole: the words an operator reads are not key-shaped.
#[test]
fn the_words_of_a_reason_survive_and_a_long_one_is_bounded() {
    assert_eq!(plain_words("server closed: 1006 no pong"), "server closed: 1006 no pong");
    assert_eq!(plain_words("no frame from the venue for 60 s"), "no frame from the venue for 60 s");
    assert_eq!(
        plain_words("Connection reset by peer (os error 104)"),
        "Connection reset by peer (os error 104)"
    );
    assert_eq!(plain_words(&"word ".repeat(200)).chars().count(), HOW_LIMIT);
}

/// ⚠ **Only rows of this one table move.** Writes, the restart sweep and the prune leave every other table and the
/// version stamp exactly as they were, and the keys still read: the store a rollback's older binary opens.
#[test]
fn nothing_else_in_the_file_moves_and_the_stamp_stays() {
    let dir = store();
    let before = everything_else(dir.path());
    write_link_events(
        dir.path(),
        &[opened("hyperliquid", 1_000), closed("hyperliquid", 2_000, "closed")],
    )
    .expect("write");
    close_left_open(dir.path()).expect("sweep");
    flush_link_events(dir.path(), &[opened("hyperliquid#FLUSHED", 3_000)], 4_000).expect("flush");
    write_link_events(dir.path(), &[opened("hyperliquid#FRESH", 900 * DAY_MS)]).expect("write");
    prune_link_sessions(dir.path(), 900 * DAY_MS).expect("prune");
    assert_eq!(everything_else(dir.path()), before);
    assert_eq!(rows(dir.path()).len(), 1, "the old row went");

    let conn = crate::db::open_for_read(&db_path_in(dir.path()))
        .expect("every reader's open still takes this store");
    let stamp: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("stamp");
    assert_eq!(
        stamp,
        crate::SCHEMA_VERSION,
        "a bump would make an older binary refuse the whole store"
    );
    let (keys, _) = crate::db::read_table(
        &db_path_in(dir.path()),
        crate::db::StoreTable::HubKey,
        crate::CredentialScope::Full,
    )
    .expect("the keys read");
    assert_eq!(keys.into_map().get("A_KEY").map(String::as_str), Some("a-fake-hub-key"));
}

/// A process that died mid-session: the next start says so, as ours, and it counts as no drop.
#[test]
fn the_start_closes_what_a_dead_process_left_open_at_the_last_time_it_can_vouch_for() {
    let dir = store();
    write_link_events(
        dir.path(),
        &[
            opened("hyperliquid", 1_000),
            opened("hyperliquid#OTHER", 2_000),
            closed("hyperliquid#OTHER", 7_000, "closed"),
        ],
    )
    .expect("write");
    close_left_open(dir.path()).expect("sweep");
    let got = rows(dir.path());
    let left = got.iter().find(|r| r.route == "hyperliquid").expect("the row");
    assert_eq!(left.closed_ms, Some(7_000), "the last the table was written, not this start");
    assert_eq!(
        (left.lived_s, left.ours),
        (None, true),
        "no drop, and no length a LONGEST could take"
    );
    assert_eq!(left.how.as_deref(), Some(LEFT_OPEN_WORDS));
}

/// A clean stop says all it has in one transaction: every report still queued, and what is still open ends now, as ours.
#[test]
fn a_clean_stop_flushes_every_report_and_ends_what_is_still_open() {
    let dir = store();
    write_link_events(dir.path(), &[opened("hyperliquid#RUNS", 1_000)]).expect("write");
    flush_link_events(
        dir.path(),
        &[opened("hyperliquid", 2_000), closed("hyperliquid", 3_000, "server closed: 1000")],
        5_000,
    )
    .expect("flush");
    let got = rows(dir.path());
    let done = got.iter().find(|r| r.route == "hyperliquid").expect("the queued report");
    assert_eq!((done.lived_s, done.ours), (Some(1.0), false));
    let runs = got.iter().find(|r| r.route == "hyperliquid#RUNS").expect("the open row");
    assert_eq!((runs.closed_ms, runs.lived_s, runs.ours), (Some(5_000), Some(4.0), true));
    assert_eq!(runs.how.as_deref(), Some(HUB_STOP_WORDS));
}

/// ⚠ The file vanishing between the probe and the open (an operator's `rm` then `cp`) must not leave an EMPTY store: at
/// `user_version` 0 every reader would refuse it until somebody deleted it.
#[test]
fn a_store_that_vanishes_under_the_write_is_not_recreated() {
    let dir = tempfile::tempdir().expect("temp");
    let db = db_path_in(dir.path());
    write_into(&db, |_| panic!("a store this open made must not be written"))
        .expect("no store, no error");
    assert!(!db.exists(), "the writer left a new empty store");
}

/// ⚠ **Not a test: the figure `BATCH_MAX` was chosen by**, on a table at its cap, the real write through the real open, so
/// an upper bound on the store's lock hold. Run it on the box's own disk with `--run-ignored only --no-capture`.
#[test]
#[ignore = "a measurement, printed: how long one batch holds the store"]
fn measure_how_long_one_batch_holds_the_store() {
    let dir = store();
    let conn = rusqlite::Connection::open(db_path_in(dir.path())).expect("open");
    write_link_events(dir.path(), &[]).expect("the table");
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO link_session (venue_id, route, plane, lane, opened_ms, closed_ms, lived_s, how, ours)
         SELECT 1, 'r' || (i % 500), CASE i % 2 WHEN 0 THEN 'trade' ELSE 'price' END, 'lane', i, i + 5, 0.005,
                'server closed: 1000 a reason of the usual length', 0 FROM n",
        [LINK_SESSION_ROW_CAP * 2],
    )
    .expect("a table at its cap");
    drop(conn);
    for size in [16_usize, 64, 128, 256, 512, 1024, 5120] {
        let mut held: Vec<Duration> = (0..30_i64)
            .map(|run| {
                // Half opens, half closes of those opens: the two shapes of a real batch.
                let batch: Vec<LinkEvent> = (0..size / 2)
                    .flat_map(|i| {
                        let route = format!("m{run}-{i}");
                        [opened(&route, 1_000), closed(&route, 2_000, "server closed: 1000 bye")]
                    })
                    .collect();
                let started = Instant::now();
                write_link_events(dir.path(), &batch).expect("write");
                started.elapsed()
            })
            .collect();
        held.sort_unstable();
        println!(
            "batch {size:>5}: median {:>8.2?}  p90 {:>8.2?}  max {:>8.2?}",
            held[15], held[27], held[29]
        );
    }
}

/// By age, and by a plane's cap, and the newest stay.
#[test]
fn the_prune_goes_by_age_and_a_chatty_plane_stops_at_its_cap() {
    let dir = store();
    let now = 1_000 * DAY_MS;
    write_link_events(
        dir.path(),
        &[
            opened("hyperliquid#OLD", now - (LINK_SESSION_RETENTION_DAYS + 1) * DAY_MS),
            opened("hyperliquid#KEPT", now - (LINK_SESSION_RETENTION_DAYS - 1) * DAY_MS),
            opened("hyperliquid#FRESH", now),
        ],
    )
    .expect("write");
    prune_link_sessions(dir.path(), now).expect("prune");
    let mut got: Vec<String> = rows(dir.path()).into_iter().map(|r| r.route).collect();
    got.sort_unstable();
    assert_eq!(got, ["hyperliquid#FRESH", "hyperliquid#KEPT"]);

    let conn = rusqlite::Connection::open(db_path_in(dir.path())).expect("open");
    conn.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
         INSERT INTO link_session (venue_id, route, plane, lane, opened_ms)
         SELECT 1, 'hyperliquid', 'price', 'quotes', ?2 + i FROM n",
        params![LINK_SESSION_ROW_CAP + 7, now],
    )
    .expect("a season of flapping");
    prune_link_sessions(dir.path(), now).expect("prune");
    let price: i64 = conn
        .query_row("SELECT count(*) FROM link_session WHERE plane = 'price'", [], |r| r.get(0))
        .expect("count");
    assert_eq!(price, LINK_SESSION_ROW_CAP);
    let trade: i64 = conn
        .query_row("SELECT count(*) FROM link_session WHERE plane = 'trade'", [], |r| r.get(0))
        .expect("count");
    assert_eq!(trade, 2, "one plane's churn evicts nothing of the other's");
}

/// Nothing here may make a box look configured.
#[test]
fn an_absent_store_is_not_created() {
    let dir = tempfile::tempdir().expect("temp");
    write_link_events(dir.path(), &[opened("hyperliquid", 1)]).expect("no store, no error");
    close_left_open(dir.path()).expect("no store, no error");
    prune_link_sessions(dir.path(), 1).expect("no store, no error");
    assert!(!db_path_in(dir.path()).exists());
}

/// A store with no table yet (a box this build has not run on) is an empty answer.
#[test]
fn a_store_with_no_table_yet_answers_no_rows() {
    let dir = store();
    assert!(rows(dir.path()).is_empty());
}

/// ⚠ **The budget is a statement about who waits.** A settings writer holding the lock costs this batch its rows, soon,
/// and leaves nothing behind: the writer counts the loss and the venue never knew.
#[test]
fn a_busy_store_costs_the_rows_within_the_budget() {
    let dir = store();
    let holder = crate::hold_write_lock(dir.path());
    let started = Instant::now();
    let refused = write_link_events(dir.path(), &[opened("hyperliquid", 1)]);
    assert!(refused.is_err(), "the lock is held");
    assert!(
        started.elapsed() < LINK_WRITE_BUSY * 2 + Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    drop(holder);
    write_link_events(dir.path(), &[opened("hyperliquid", 2)]).expect("free again");
    assert_eq!(rows(dir.path()).len(), 1);
}

/// What a `SIGKILL` inside a commit leaves on disk, as a file pair copied mid-transaction: a stamped journal beside a
/// database file the transaction has already written into. Goes through `dst`, which the reader then finds.
fn killed_pair_into(live: &Path, dst: &Path) {
    let conn = rusqlite::Connection::open(db_path_in(live)).expect("open");
    conn.execute_batch(
        "PRAGMA cache_size = 1;
         BEGIN IMMEDIATE;
         UPDATE hub_key SET value = 'the killed writer never committed this';
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
         INSERT INTO link_session (venue_id, route, plane, lane, opened_ms)
         SELECT 1, 'uncommitted', 'trade', 'pump', i FROM n;",
    )
    .expect("a write big enough to spill to the file");
    let (source, db) = (db_path_in(live), db_path_in(dst));
    std::fs::create_dir_all(db.parent().expect("a parent")).expect("dir");
    std::fs::copy(&source, &db).expect("the database");
    let (mut journal, mut left) = (source.into_os_string(), db.into_os_string());
    journal.push("-journal");
    left.push("-journal");
    std::fs::copy(journal, left).expect("the journal: the transaction is open");
}

/// ⚠ **MEASURED, the rule this table made owed: a process killed hard inside a commit costs no read, no key and no row.** A
/// read-only open cannot replay the journal, and a refused read is the live gate, so the read replays it through one brief
/// read-write open and answers (`crate::db::open_for_read`, what every daemon's first read and `config check` go through).
#[test]
fn a_hard_kill_inside_a_commit_costs_no_read_and_every_key_and_row_stands() {
    let live = store();
    write_link_events(live.path(), &[opened("hyperliquid", 1)]).expect("a committed row");
    let before = everything_else(live.path());

    let killed = tempfile::tempdir().expect("temp");
    killed_pair_into(live.path(), killed.path());
    let db = db_path_in(killed.path());
    let mut journal = db.clone().into_os_string();
    journal.push("-journal");

    let (keys, _) =
        crate::db::read_table(&db, crate::db::StoreTable::HubKey, crate::CredentialScope::Full)
            .expect("the first read after the kill answers");
    assert_eq!(keys.into_map().get("A_KEY").map(String::as_str), Some("a-fake-hub-key"));
    assert!(!Path::new(&journal).exists(), "the replay removes the journal");
    assert_eq!(everything_else(killed.path()), before, "every other table and the stamp");
    let after = rows(killed.path());
    assert_eq!(
        after.len(),
        1,
        "the committed row stands, the uncommitted ones are gone: {after:?}"
    );
    assert_eq!(after[0].route, "hyperliquid");
}

/// ⚠ **The tradehub's account follower holds ONE read-only connection for its whole life**, so it meets the journal on a
/// connection that opened before the kill, and a refusal there stops it following every account. It replays too.
#[test]
fn a_watcher_held_across_a_hard_kill_answers_and_still_sees_a_later_commit() {
    let live = store();
    write_link_events(live.path(), &[opened("hyperliquid", 1)]).expect("a committed row");
    let watched = tempfile::tempdir().expect("temp");
    let db = db_path_in(watched.path());
    std::fs::create_dir_all(db.parent().expect("a parent")).expect("dir");
    std::fs::copy(db_path_in(live.path()), &db).expect("the store before the kill");
    let mut watch = crate::StoreWatch::begin(&db).expect("a watcher");

    killed_pair_into(live.path(), watched.path());
    watch.changed().expect("the watcher replays the journal and answers");
    write_link_events(watched.path(), &[opened("hyperliquid#AFTER", 2)]).expect("a later commit");
    assert!(watch.changed().expect("it answers"), "and it still follows the rows");
}

/// A store at another schema gets no row: this module writes into nothing it was not built for.
#[test]
fn a_store_at_another_schema_is_refused() {
    let dir = store();
    rusqlite::Connection::open(db_path_in(dir.path()))
        .expect("open")
        .pragma_update(None, "user_version", 99)
        .expect("stamp");
    assert!(write_link_events(dir.path(), &[opened("hyperliquid", 1)]).is_err());
}

/// One route of many, for `trade links BOOK`, and `since` bounds the read.
#[test]
fn a_read_narrows_to_one_account_or_to_every_account_of_a_venue() {
    let dir = store();
    let aster = LinkKey { venue: "aster".to_string(), ..key("aster#DEMO") };
    write_link_events(
        dir.path(),
        &[
            opened("hyperliquid#DEMO", 1_000),
            opened("hyperliquid#LIVE", 2_000),
            LinkEvent::Opened { key: aster, at_ms: 3_000 },
        ],
    )
    .expect("write");
    let db = db_path_in(dir.path());
    let one = read_link_sessions(&db, 0, Some("hyperliquid#LIVE"), None).expect("read");
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].route, "hyperliquid#LIVE");
    // A venue with no unlabelled account: its bare word still answers with its books.
    let venue = read_link_sessions(&db, 0, None, Some("hyperliquid")).expect("read");
    let mut routes: Vec<&str> = venue.iter().map(|r| r.route.as_str()).collect();
    routes.sort_unstable();
    assert_eq!(routes, ["hyperliquid#DEMO", "hyperliquid#LIVE"]);
    assert!(read_link_sessions(&db, 0, Some("hyperliquid"), None).expect("read").is_empty());
}
