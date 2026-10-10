//! The `account.armed` fold and the `venue_arming.max_exposure` column on an older store.

use super::row_write_tests::{planted, rows};
use super::*;
use std::assert_matches;

// -----------------------------------------------------------------------------------------
// `account.armed` — the fold (spec §9 stage 3)
// -----------------------------------------------------------------------------------------

/// Plant `account` rows directly, as `(id, venue, tier, label)`.
///
/// ⚠ Rows rather than credentials, deliberately: the classifier that MINTS accounts lives in
/// `vike-bridge-core` (layer 25) and this crate cannot name it, so a test that went through
/// the credential writer would have to re-spell it. The fold reads the `account` table and nothing else,
/// so the table is what these tests plant.
///
/// ⚠ Each row names its venue by NUMBER: `account.venue_id` is `NOT NULL`, the fold reads accounts
/// through it, and the shipped `account` has no text `venue`. The `DDL` batch [`planted`] runs seeds
/// no `venue` row, so the venue is filed first — the same `INSERT OR IGNORE` the roster top-up uses
/// — and the row takes its number from it in the same statement.
fn plant_accounts(db: &Path, rows: &[(i64, &str, &str, Option<&str>)]) {
    let conn = rusqlite::Connection::open(db).unwrap();
    for (id, venue, tier, label) in rows {
        conn.execute("INSERT OR IGNORE INTO venue (name) VALUES (?1)", rusqlite::params![venue])
            .unwrap();
        conn.execute(
            "INSERT INTO account (id, venue_id, tier, label) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?2), ?3, ?4)",
            rusqlite::params![id, venue, tier, label],
        )
        .unwrap();
    }
}

/// `(id, armed)` for every row, in id order — the pair set
/// `crates/vike-config/tests/no_ceiling_widens_across_the_migration.rs` judges, read here
/// without that crate's vocabulary.
fn armed_bits(db: &Path) -> Vec<(i64, bool)> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn.prepare("SELECT id, armed FROM account ORDER BY id").unwrap();
    stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn arming(venue: &str, label: Option<&str>, mode: &str) -> ArmingRow {
    ArmingRow {
        venue: venue.into(),
        label: label.map(str::to_string),
        mode: mode.into(),
        max_exposure: None,
    }
}

fn with_arming(rows: Vec<ArmingRow>) -> StoredSettings {
    StoredSettings { settings: Vec::new(), arming: rows, venue: Vec::new() }
}

/// **The first arm, and the shape both live boxes have**: an UNLABELLED account takes its
/// venue's line, and is armed exactly where that line names the tier it already carries.
///
/// The hyperliquid pair is the one real instance of the *one arming row, N account rows* case
/// — mode `live` over a `demo` and a `live` account — and the two dukascopy rows are the case
/// that cannot be keyed on `(venue, tier, label)` at all.
#[test]
fn an_unlabelled_account_is_armed_where_its_venues_line_names_its_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    plant_accounts(
        &db,
        &[
            (1, "binance", "live", None),
            (2, "hyperliquid", "demo", None),
            (3, "hyperliquid", "live", None),
            (4, "dukascopy", "demo", None),
            (5, "dukascopy", "demo", None),
            (6, "okx", "demo", None),
        ],
    );
    write_settings_in(
        tmp.path(),
        &with_arming(vec![
            arming("binance", None, "live"),
            arming("hyperliquid", None, "live"),
            arming("dukascopy", None, "demo"),
            arming("okx", None, "live"),
        ]),
    )
    .unwrap();

    assert_eq!(
        armed_bits(&db),
        vec![(1, true), (2, false), (3, true), (4, true), (5, true), (6, false)],
        "binance/live and hyperliquid/live are named by their lines; hyperliquid/demo is not \
             (the box mounts that venue at live); BOTH dukascopy books are, since the line names \
             their tier and they are separate rows; okx/demo is not, because a `live` line does \
             not name a demo credential set"
    );
}

/// **The third arm, and the one that widens if it is forgotten.** A LABELLED account with no
/// `[accounts]` line of its own resolves to `paper` under
/// `vike_config::VenuePolicy::account` — *"the line was written when the venue had one
/// account; reading it as consent for an account that did not exist when it was written is
/// precisely the silent escalation the ceiling exists to prevent"*. A fold reading the VENUE
/// row for it would arm `BINANCE_LIVE_API_KEY__ALT` to live.
#[test]
fn a_labelled_account_with_no_line_of_its_own_is_not_armed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    plant_accounts(&db, &[(1, "binance", "live", None), (2, "binance", "live", Some("ALT"))]);
    write_settings_in(tmp.path(), &with_arming(vec![arming("binance", None, "live")])).unwrap();

    assert_eq!(
        armed_bits(&db),
        vec![(1, true), (2, false)],
        "the DEFAULT account inherits the venue line and a LABELLED one does not"
    );
}

/// **The second arm, and the cap the spec's prose omitted once.** A labelled account's own
/// line is a ceiling the VENUE's line still caps —
/// `VenuePolicy::account`'s `(Some(mode), _) => venue_ceiling.cap(mode)` — so
/// `[venues] binance = "demo"` with `[accounts] binance.ALT = "live"` went in at
/// `min(demo, live)` = demo, and a fold reading the labelled row alone arms it to live.
#[test]
fn a_labelled_line_above_its_venues_own_is_capped_by_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    plant_accounts(
        &db,
        &[
            (1, "binance", "demo", None),
            (2, "binance", "live", Some("ALT")),
            (3, "binance", "demo", Some("ALT")),
        ],
    );
    write_settings_in(
        tmp.path(),
        &with_arming(vec![arming("binance", None, "demo"), arming("binance", Some("ALT"), "live")]),
    )
    .unwrap();

    assert_eq!(
        armed_bits(&db),
        vec![(1, true), (2, false), (3, true)],
        "`ALT`'s own `live` line is capped to `demo` by the venue's, so the LIVE ALT row is \
             disarmed and the DEMO one is armed — the uncapped reading arms the live one"
    );
}

/// **A venue with no arming row at all is `paper`**, which is `VenuePolicy::get`'s own answer
/// for a venue it does not carry, and the answer a box with no `[venues]` table already gets.
///
/// ⚠ The `paper` row is the one that ARMS here, and it stopped being a curiosity on
/// 2026-09-23. It USED TO read: *`account.tier` spells paper `sim` while `venue_arming.mode`
/// spells it `paper`, so the fold's map is what makes the ceiling and the tier comparable —
/// without it this row reads as `sim != paper` and comes out disarmed, a different BIT for
/// identical behaviour.* §4.4's rename DELETED that map (`mode_word_of_tier`) by making the
/// two columns one word, so what this test now pins is that the direct comparison the fold
/// does is right — and the kept sentence is what a reader needs in order to see that the
/// deletion was a simplification rather than a lost case.
#[test]
fn a_venue_with_no_arming_row_is_paper_and_only_a_paper_account_matches_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    plant_accounts(
        &db,
        &[(1, "bybit", "live", None), (2, "bybit", "paper", None), (3, "okx", "paper", None)],
    );
    write_settings_in(tmp.path(), &with_arming(vec![arming("okx", None, "paper")])).unwrap();

    assert_eq!(
        armed_bits(&db),
        vec![(1, false), (2, true), (3, true)],
        "an absent line and a stated `paper` line answer identically, and the tier is spelled \
             with the SAME word"
    );
}

/// **The shipped batch with `armed` taken out of `account`** — the store born before the column,
/// so `armed` can only arrive by the fold's `ALTER TABLE`, APPENDED. It is also the shape the NEXT
/// post-freeze column will find on every store.
///
/// ⚠ **Derived from the shipped `DDL`** rather than spelled out, so every other column is the
/// current shape's. Each removal is asserted to match exactly once.
fn plant_account_table_without_armed(dir: &Path) -> PathBuf {
    let mut ddl = crate::schema::DDL.to_string();
    for line in
        ["    armed            INTEGER NOT NULL DEFAULT 0,\n", "    CHECK (armed IN (0, 1)),\n"]
    {
        assert_eq!(ddl.matches(line).count(), 1, "the shipped DDL must spell {line:?} once");
        ddl = ddl.replacen(line, "", 1);
    }
    let db = crate::store_locator::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(&ddl).unwrap();
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('binance');
         INSERT INTO account (id, venue_id, tier)
             SELECT 1, id, 'live' FROM venue WHERE name = 'binance';",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    drop(conn);
    db
}

/// Every column of `table`, in the order `PRAGMA table_info` reports them — i.e. PHYSICAL
/// order, which an `ALTER TABLE … ADD COLUMN` appends to.
fn column_order(db: &Path, table: &str) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})")).unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1)).unwrap().map(Result::unwrap).collect()
}

/// **An `account` table that predates the column gains it, and reads DISARMED until it does.**
///
/// ⚠ `crate::schema::DDL` is `CREATE TABLE IF NOT EXISTS`, so the column reaches a store born
/// before it through the fold's `ALTER TABLE` and not before. A reader that named `armed`
/// unconditionally would answer `no such column` for a store that is simply older than it — the
/// same hazard `ensure_arming_columns` exists for, one table over.
#[test]
fn an_account_table_that_predates_the_armed_column_gains_it_on_the_next_write() {
    let tmp = tempfile::tempdir().unwrap();
    let db = plant_account_table_without_armed(tmp.path());

    // The READ comes first: a store at this shape must answer, not error.
    let before = crate::db::read_accounts(&db).unwrap();
    let rows = match before {
        crate::db::Accounts::Known(rows) => rows,
        other => panic!("the store must answer for its accounts: {other:?}"),
    };
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].armed, "no column means no arming stated, never an error");

    write_settings_in(tmp.path(), &with_arming(vec![arming("binance", None, "live")])).unwrap();

    assert!(
        has_column(&rusqlite::Connection::open(&db).unwrap(), "account", "armed").unwrap(),
        "the write added the column"
    );
    assert_eq!(armed_bits(&db), vec![(1, true)], "…and folded the venue's line onto the row");
}

/// **…and the READER answers from that store once the column has been APPENDED — the one shape
/// the test above leaves to a raw `SELECT`.**
///
/// The distinction is not pedantry. A store BORN from `crate::schema::DDL` carries `armed` as
/// the fourth column, because that is where the DDL declares it; a store that gained it by
/// `ALTER TABLE … ADD COLUMN` carries it LAST, because that is the only place SQLite can put
/// it. `crate::db::account_select` names its columns, so the projection is positional in the
/// SELECT and not in the table — but nothing said so out loud, and `account_from_row` reads by
/// INDEX. If a later author ever replaced that projection with `SELECT *`, or appended a
/// column to the DDL, this is the store where `vike-cli secrets accounts` would start
/// answering from the wrong cell: the migrated ones, on the boxes that hold real credentials.
#[test]
fn the_reader_answers_from_a_store_whose_armed_column_was_appended_last() {
    let tmp = tempfile::tempdir().unwrap();
    // The shipped batch minus `armed`: the shape that reaches the fold's `ALTER`, and the shape
    // every store will be in when the NEXT post-freeze column lands.
    let db = plant_account_table_without_armed(tmp.path());
    write_settings_in(tmp.path(), &with_arming(vec![arming("binance", None, "live")])).unwrap();

    // ⚠ The premise, asserted rather than assumed: this store's `armed` is the LAST column,
    // and a store born from the DDL would have it fourth. Without this line the test could
    // pass against a table shaped like the DDL's and prove nothing about the ALTER path.
    let order = column_order(&db, "account");
    assert_eq!(
        order.last().map(String::as_str),
        Some("armed"),
        "the ALTER appends, so `armed` must be LAST here: {order:?}"
    );
    assert_ne!(
        order.iter().position(|c| c == "armed"),
        Some(3),
        "…and NOT where `crate::schema::DDL` declares it, or this store is not the migrated \
             shape: {order:?}"
    );

    // …and now the PUBLIC reader, through `account_select`'s `armed` branch.
    let rows = match crate::db::read_accounts(&db).unwrap() {
        crate::db::Accounts::Known(rows) => rows,
        other => panic!("the store must answer for its accounts: {other:?}"),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].venue, "binance", "the row is read from the right cells…");
    assert_eq!(rows[0].tier, "live");
    assert_eq!(rows[0].label, None);
    assert!(rows[0].active, "…including the OTHER flag, which shares `armed`'s `!= 0` shape");
    assert!(
        rows[0].armed,
        "…and `armed` reads TRUE through the public reader, not merely through a raw SELECT"
    );
}

/// **The fold is IDEMPOTENT and re-derives rather than accumulates** — it is a pure function of
/// the two tables, so a second mirror with a NARROWER policy takes the bit back down. A fold
/// that only ever set bits would leave an account armed after the operator revoked its line,
/// which is the one direction that matters.
#[test]
fn a_narrower_mirror_disarms_what_the_previous_one_armed() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    plant_accounts(&db, &[(1, "binance", "live", None)]);

    write_settings_in(tmp.path(), &with_arming(vec![arming("binance", None, "live")])).unwrap();
    assert_eq!(armed_bits(&db), vec![(1, true)]);

    write_settings_in(tmp.path(), &with_arming(vec![arming("binance", None, "paper")])).unwrap();
    assert_eq!(
        armed_bits(&db),
        vec![(1, false)],
        "a `paper` line no longer names this row's tier, so the bit comes back DOWN"
    );

    write_settings_in(tmp.path(), &with_arming(Vec::new())).unwrap();
    assert_eq!(
        armed_bits(&db),
        vec![(1, false)],
        "…and a policy with no `[venues]` table at all leaves it down"
    );
}

#[test]
fn a_round_trip_returns_exactly_what_was_written() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());
    let written = write_settings_in(tmp.path(), &rows()).unwrap();
    assert_eq!(written.settings, 2);
    assert_eq!(written.arming, 2);
    assert!(!written.tables_created, "a store planted from the full DDL already has them");

    let found = read_settings_in(tmp.path()).unwrap();
    assert_eq!(found.rows(), Some(&rows().sorted()));
}

/// A store migrated BEFORE these tables existed reads as [`SettingsSource::TablesAbsent`] and is
/// carried by the first write — never as an empty settings layer, which is the answer a caller
/// could not tell from "this box mirrored nothing on purpose".
#[test]
fn a_store_without_the_tables_says_so_and_the_first_write_creates_them() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::store_locator::db_path_in(tmp.path());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    // The credential half of the schema only — the shape a box migrated before Phase 1 carries.
    conn.execute_batch(
        "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    drop(conn);

    let found = read_settings(&db).unwrap();
    assert_matches!(found, SettingsSource::TablesAbsent { .. }, "{found:?}");
    assert!(found.rows().is_none(), "an unmirrored box contributes NO layer, not an empty one");

    let written = write_settings(&db, &rows()).unwrap();
    assert!(written.tables_created, "the first write is what creates them");
    assert_eq!(read_settings(&db).unwrap().rows(), Some(&rows().sorted()));
}

/// **A store that EXISTS and will not open is an ERROR, never `NoDatabase` and never empty
/// rows** — the three answers must stay distinguishable, because each one asks a caller for
/// something different.
///
/// This is the INPUT to the degrade decision `vike_boot::boot` makes (an unopenable store is a
/// warning; the files still answer), and that decision is only correct while this arm is an
/// `Err`: collapsing it into `NoDatabase` would make a permissions bug on a mirrored box read
/// exactly like a box that was never mirrored, with nothing anywhere saying so. The same
/// posture `crate::store` already takes between an ABSENT credential store and an
/// unreadable one.
#[test]
fn a_store_that_exists_and_will_not_open_is_an_error_not_an_absent_one() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::store_locator::db_path_in(tmp.path());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"this is not a database").unwrap();

    let err = read_settings(&db).unwrap_err();
    assert_eq!(err.path, db);
    assert!(
        !matches!(err.kind, DbErrorKind::NoSettingsDatabase),
        "the file IS there — the refusal must not read as absence: {err}"
    );
}

/// A mirror run makes the tables EQUAL to its input — a key that stops being set stops being a
/// row. An upsert would leave it behind, and a row nothing wrote reads as authoritative.
#[test]
fn a_second_write_replaces_rather_than_accumulates() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());
    write_settings_in(tmp.path(), &rows()).unwrap();

    let fewer = StoredSettings {
        venue: Vec::new(),
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "1.0".into(),
        }],
        arming: Vec::new(),
    };
    write_settings_in(tmp.path(), &fewer).unwrap();
    assert_eq!(read_settings_in(tmp.path()).unwrap().rows(), Some(&fewer.sorted()));
}

/// The `CHECK` lists are gates, not comments — a hand `INSERT` of an unknown section or an
/// unknown mode is refused by the engine, which is the half a Rust-side check can never cover.
///
/// ⚠ **The `venue_arming` row names its venue by NUMBER, and its refusal is asserted by its TEXT.**
/// `venue_arming.venue_id` is `NOT NULL`, and the engine checks `NOT NULL` before any `CHECK`.
/// This row used to name no number, so it was refused for THAT and
/// `.is_err()` held without the `mode` CHECK ever being consulted: widening the CHECK to admit
/// `'fat'` left the test green. Now the venue is filed, the row resolves its number, and the
/// refusal must be the `mode` CHECK's. (The `setting` row needs neither: it supplies every
/// `NOT NULL` column, so the CHECK is the only refusal it can meet.)
#[test]
fn the_schema_refuses_an_unknown_section_and_an_unknown_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    assert!(
        conn.execute("INSERT INTO setting (section, key, value) VALUES ('secrets', 'k', '1')", [])
            .is_err(),
        "`section` carries a CHECK over the four settings sections"
    );
    conn.execute("INSERT INTO venue (name) VALUES ('binance')", []).unwrap();
    let err = conn
        .execute(
            "INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'fat')",
            [],
        )
        .expect_err("`mode` carries a CHECK over paper/demo/live");
    // The engine's text names the constraint it refused (`CHECK constraint failed: mode IN (…)`,
    // read from a lane run), so this fragment pins WHICH check fired: the one over `mode`.
    let text = err.to_string();
    assert!(
        text.contains("CHECK constraint failed: mode IN"),
        "refused by the `mode` CHECK rather than by a missing number: {text}"
    );
}

/// The two partial indexes: one venue-level row per venue, one per (venue, label) — and the
/// NULL label does not silently permit duplicates, which is what a single `UNIQUE (venue,
/// label)` would have done.
#[test]
fn a_venue_gets_one_ceiling_row_and_each_label_gets_one_of_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    // ⚠ `venue_arming.venue_id` is `NOT NULL`, and the `DDL` batch seeds no `venue` row: file the
    // venue, then let each row take its number in its own statement.
    conn.execute("INSERT INTO venue (name) VALUES ('binance')", []).unwrap();
    let insert = |venue: &str, label: Option<&str>, mode: &str| {
        conn.execute(
            "INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?1), ?2, ?3)",
            rusqlite::params![venue, label, mode],
        )
    };
    insert("binance", None, "demo").unwrap();
    assert!(insert("binance", None, "live").is_err(), "one venue-level row per venue");
    insert("binance", Some("ALT"), "paper").unwrap();
    assert!(insert("binance", Some("ALT"), "live").is_err(), "one row per (venue, label)");
    // A DIFFERENT label is a different account and is allowed.
    insert("binance", Some("SUB"), "paper").unwrap();
}

// --------------------------------------------------------------------------------------------
// `venue_arming.max_exposure` — the column added to a table that already exists on two live
// boxes. Every test here is about that asymmetry, because the DDL cannot express it: every
// statement in it is `CREATE TABLE IF NOT EXISTS`, which does nothing at all to a table that is
// already there.
// --------------------------------------------------------------------------------------------

/// A store whose `venue_arming` has NO `max_exposure` column because the code that created it
/// predates one: the shipped batch with that one column (and its `CHECK`) taken out, and one
/// venue-level row.
///
/// ⚠ **Derived from the shipped `DDL`**, so every other column is the current shape's and the
/// column under test is the only difference. Each removal is asserted to match exactly once.
fn planted_without_the_column(dir: &Path) -> PathBuf {
    let mut ddl = crate::schema::DDL.to_string();
    for (line, kept) in [
        ("    max_exposure REAL,\n", ""),
        (
            "    CHECK (mode IN ('paper', 'demo', 'live')),\n    \
             CHECK (max_exposure IS NULL OR max_exposure > 0.0)\n",
            "    CHECK (mode IN ('paper', 'demo', 'live'))\n",
        ),
    ] {
        assert_eq!(ddl.matches(line).count(), 1, "the shipped DDL must spell {line:?} once");
        ddl = ddl.replacen(line, kept, 1);
    }
    let db = crate::store_locator::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(&ddl).unwrap();
    conn.execute_batch(
        "INSERT INTO venue (name) VALUES ('binance');
         INSERT INTO venue_arming (venue, venue_id, label, mode)
             SELECT 'binance', id, NULL, 'demo' FROM venue WHERE name = 'binance';",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    db
}

/// ⚠ **THE ONE THAT PROTECTS THE TWO MIGRATED BOXES.** A reader that selected an assumed column
/// would fail with a SQL error on every store written before it existed — which is both boxes —
/// and a settings read that ERRORS is not a degraded answer, it is a daemon that will not boot.
/// The honest answer is `None`: that box states no per-account figure because it could not have.
#[test]
fn a_store_without_the_column_reads_back_with_no_figure() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted_without_the_column(tmp.path());

    let source = read_settings(&db).expect("a store predating the column must still READ");
    let SettingsSource::Rows { rows, .. } = source else {
        panic!("a planted store answers with rows");
    };
    assert_eq!(rows.arming.len(), 1);
    assert_eq!(rows.arming[0].venue, "binance");
    assert_eq!(rows.arming[0].max_exposure, None, "no column means no figure, never an error");
}

/// …and WRITING to that same store adds the column rather than failing — idempotently, so the
/// second mirror run is a no-op. Without this the figure would be unwritable on exactly the
/// boxes that already exist.
#[test]
fn writing_to_a_store_without_the_column_adds_it_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted_without_the_column(tmp.path());

    let mut with_figure = rows();
    with_figure.arming[0].max_exposure = Some(5000.0);

    write_settings(&db, &with_figure).expect("the first write adds the column");
    write_settings(&db, &with_figure).expect("the second write must be a no-op, not a failure");

    let SettingsSource::Rows { rows, .. } = read_settings(&db).expect("read back") else {
        panic!("rows");
    };
    let binance = rows.arming.iter().find(|r| r.venue == "binance").expect("the binance row");
    assert_eq!(binance.max_exposure, Some(5000.0));
}

/// The round trip on a store born WITH the column — the ordinary case, and the one that says the
/// figure is carried rather than merely accepted.
#[test]
fn a_figure_survives_the_write_and_the_read() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());

    let mut written = rows();
    written.arming[0].max_exposure = Some(50000.0);
    written.arming[1].max_exposure = Some(5000.0);
    write_settings(&db, &written).expect("write");

    let SettingsSource::Rows { rows, .. } = read_settings(&db).expect("read") else {
        panic!("rows");
    };
    assert_eq!(rows.sorted(), written.sorted(), "the rows must come back exactly as written");
}

/// ⚠ **The store refuses a non-positive figure too**, and that second gate is not redundant with
/// the loader's: this one is what a hand `INSERT` meets. A ceiling of zero would become the
/// BINDING one under the mount's `min` fold and refuse every order on that account.
#[test]
fn the_store_refuses_a_non_positive_figure() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    // ⚠ `venue_arming.venue_id` is `NOT NULL`, and the engine checks that
    // BEFORE the `CHECK` this test is about: a row naming no number is refused for the wrong reason.
    // So the venue is filed and the row names its number.
    conn.execute("INSERT INTO venue (name) VALUES ('okx')", []).unwrap();
    for bad in ["0.0", "-1.0"] {
        let err = conn
            .execute_batch(&format!(
                "INSERT INTO venue_arming (venue, venue_id, label, mode, max_exposure) \
                     VALUES ('okx', (SELECT id FROM venue WHERE name = 'okx'), NULL, 'demo', {bad});"
            ))
            .expect_err("the DDL's CHECK must refuse it");
        assert!(
            err.to_string().to_lowercase().contains("check"),
            "refused by the CHECK rather than by accident ({bad}): {err}"
        );
    }
}
