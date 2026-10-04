use super::*;

/// A database with the schema on it and the version stamped — what `secrets migrate` leaves
/// behind, minus the credentials, so these tests never touch a credential path at all.
fn planted(dir: &Path) -> PathBuf {
    let db = crate::dotenv::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(crate::schema::DDL).unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    db
}

fn rows() -> StoredSettings {
    StoredSettings {
        venue: Vec::new(),
        settings: vec![
            SettingRow {
                section: "policy".into(),
                key: "max_leverage".into(),
                value: "3.0".into(),
            },
            SettingRow {
                section: "preferences".into(),
                key: "log_file_level".into(),
                value: "\"warn\"".into(),
            },
        ],
        arming: vec![
            ArmingRow {
                venue: "binance".into(),
                label: None,
                mode: "demo".into(),
                max_exposure: None,
            },
            ArmingRow {
                venue: "hyperliquid".into(),
                label: Some("ALT".into()),
                mode: "paper".into(),
                max_exposure: None,
            },
        ],
    }
}

// -----------------------------------------------------------------------------------------
// `write_setting_row_in` — the one-row writer (0086)
// -----------------------------------------------------------------------------------------

/// The validator every test below hands the writer that wants no opinion of its own: it accepts
/// whatever candidate it is offered. A real caller (`vike-config`'s planner) is what actually
/// checks `apply_rows`/`differing_keys`; these tests are about the PRIMITIVE's own
/// mechanics (the transaction, the seal, the fold, the refusals it owns), not the validator's.
fn accept_everything(
    _current: &StoredSettings,
    _adoption: Option<&Adoption>,
    _candidate: &StoredSettings,
) -> Result<(), String> {
    Ok(())
}

#[test]
fn a_new_setting_row_is_inserted_and_reported_as_having_no_old_value() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());
    let written = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_notional_per_order".into(),
            value: "250".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(written.old_value, None);
    assert_eq!(written.rows.settings.len(), 1);
    assert_eq!(written.rows.settings[0].value, "250");

    // Read back through the ordinary reader: the row really landed.
    let source = read_settings_in(tmp.path()).unwrap();
    let rows = source.rows().unwrap();
    assert_eq!(rows.settings.len(), 1);
    assert_eq!(rows.settings[0].section, "policy");
    assert_eq!(rows.settings[0].key, "max_notional_per_order");
    assert_eq!(rows.settings[0].value, "250");
}

#[test]
fn an_existing_setting_row_is_updated_in_place_and_reports_its_old_value() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();

    let written = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "5.0".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(written.old_value.as_deref(), Some("3.0"));

    // The OTHER settings row and every arming row are untouched — one row changed, and only
    // one: read the whole store back and compare against the fixture with one value patched.
    let mut expected = rows();
    expected.settings[0].value = "5.0".into();
    let source = read_settings_in(tmp.path()).unwrap();
    assert_eq!(source.rows().unwrap(), &expected.sorted());
}

#[test]
fn an_arming_row_with_no_label_and_one_with_a_label_both_upsert() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();

    // The unlabelled (venue-level) row — an UPDATE, since `rows()` already carries a `binance`
    // line.
    let written = write_setting_row_in(
        tmp.path(),
        RowChange::Arming { venue: "binance".into(), label: None, mode: "live".into() },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(written.old_value.as_deref(), Some("demo"));

    // A labelled row that does not exist yet — an INSERT, since the table is no longer empty.
    let written = write_setting_row_in(
        tmp.path(),
        RowChange::Arming {
            venue: "binance".into(),
            label: Some("ALT".into()),
            mode: "paper".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();
    assert_eq!(written.old_value, None);

    let source = read_settings_in(tmp.path()).unwrap();
    let arming = &source.rows().unwrap().arming;
    assert!(arming.iter().any(|r| r.venue == "binance" && r.label.is_none() && r.mode == "live"));
    assert!(
        arming.iter().any(|r| r.venue == "binance"
            && r.label.as_deref() == Some("ALT")
            && r.mode == "paper")
    );
    // The row this write did not touch is still exactly as it was.
    assert!(arming.iter().any(|r| r.venue == "hyperliquid"
        && r.label.as_deref() == Some("ALT")
        && r.mode == "paper"));
}

#[test]
fn the_first_arming_statement_on_an_empty_table_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path()); // no arming rows at all
    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Arming { venue: "binance".into(), label: None, mode: "live".into() },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap_err();
    assert!(matches!(err, RowWriteError::ArmingRosterEmpty), "{err}");
    // Nothing landed: the table this refusal is about is still empty.
    let source = read_settings_in(tmp.path()).unwrap();
    assert!(source.rows().unwrap().arming.is_empty());
}

#[test]
fn a_validator_refusal_leaves_the_database_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    let before = std::fs::read(&db).unwrap();

    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "50.0".into(),
        },
        std::time::Duration::from_millis(200),
        |_current, _adoption, _candidate| Err("this candidate does not boot clean".to_string()),
    )
    .unwrap_err();
    assert!(matches!(err, RowWriteError::Rejected(_)), "{err}");

    let after = std::fs::read(&db).unwrap();
    assert_eq!(before, after, "a refused write must not change one byte on disk");
}

#[test]
fn the_validator_sees_none_before_the_first_seal_and_the_real_seal_after() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "true".into(),
        },
        std::time::Duration::from_millis(200),
        |_current, adoption, _candidate| {
            assert!(adoption.is_none(), "no seal exists yet");
            Ok(())
        },
    )
    .unwrap();

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "false".into(),
        },
        std::time::Duration::from_millis(200),
        |_current, adoption, _candidate| {
            let seal = adoption.expect("the previous write must have sealed the store");
            assert_eq!(seal.setting_rows, 3);
            assert_eq!(seal.arming_rows, 2);
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn a_fresh_write_seals_a_store_that_had_no_seal_and_counts_both_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    // `write_settings` moves the seal's counts only when a seal already exists (see its own
    // doc) — a freshly planted store has none, so this is the state `write_setting_row_in`
    // seals for the first time.
    assert!(read_settings_in(tmp.path()).unwrap().adoption().is_none());

    write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "flags".into(),
            key: "tradehub_control".into(),
            value: "true".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    let seal = source.adoption().expect("the write must have sealed the store");
    assert_eq!(seal.setting_rows, 3); // the two fixture rows plus this one
    assert_eq!(seal.arming_rows, 2);
    assert!(seal.venues_declared);
}

#[test]
fn plant_settings_rows_creates_a_store_from_nothing_and_plants_exactly_the_given_rows() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(read_settings_in(tmp.path()).unwrap(), SettingsSource::NoDatabase { .. }));

    plant_settings_rows(tmp.path(), &rows()).unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    assert_eq!(source.rows().unwrap(), &rows().sorted());
}

#[test]
fn plant_settings_rows_on_an_existing_store_replaces_its_rows() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());
    plant_settings_rows(tmp.path(), &rows()).unwrap();
    plant_settings_rows(tmp.path(), &StoredSettings::default()).unwrap();

    let source = read_settings_in(tmp.path()).unwrap();
    assert!(source.rows().unwrap().is_empty());
}

#[test]
fn another_writer_holding_the_store_is_reported_as_busy_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    write_settings(&db, &rows()).unwrap();
    let before = std::fs::read(&db).unwrap();

    let _holder = hold_write_lock(tmp.path());
    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "9.0".into(),
        },
        std::time::Duration::from_millis(50),
        accept_everything,
    )
    .unwrap_err();
    assert!(matches!(err, RowWriteError::Busy), "{err}");
    drop(_holder);

    let after = std::fs::read(&db).unwrap();
    assert_eq!(before, after, "a busy refusal must not change one byte on disk");
}

#[test]
fn no_database_is_refused_without_creating_one() {
    let tmp = tempfile::tempdir().unwrap();
    let err = write_setting_row_in(
        tmp.path(),
        RowChange::Setting {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "3.0".into(),
        },
        std::time::Duration::from_millis(200),
        accept_everything,
    )
    .unwrap_err();
    assert!(matches!(err, RowWriteError::NoDatabase { .. }), "{err}");
    assert!(!crate::dotenv::db_path_in(tmp.path()).exists());
}

#[test]
fn a_project_with_no_database_reads_as_no_database_and_refuses_a_write() {
    let tmp = tempfile::tempdir().unwrap();
    let found = read_settings_in(tmp.path()).unwrap();
    assert!(matches!(found, SettingsSource::NoDatabase { .. }), "{found:?}");
    assert!(found.rows().is_none());

    let err = write_settings_in(tmp.path(), &rows()).unwrap_err();
    assert!(matches!(err.kind, DbErrorKind::NoSettingsDatabase), "{err}");
    // ...and it did not create one on the way past. This is the assertion that matters: a
    // settings write that created the store would take every venue to paper.
    assert!(
        !crate::dotenv::db_path_in(tmp.path()).exists(),
        "the refusal must leave NO database behind — its existence is the credential backend's \
             whole choice"
    );
}

// -----------------------------------------------------------------------------------------
// `account.armed` — the fold (spec §9 stage 3)
// -----------------------------------------------------------------------------------------

/// Plant `account` rows directly, as `(id, venue, tier, label)`.
///
/// ⚠ Rows rather than credentials, deliberately: the classifier that MINTS accounts lives in
/// `vike-bridge-core` (layer 25) and this crate cannot name it, so a test that went through
/// `migrate` would have to re-spell it. The fold reads the `account` table and nothing else,
/// so the table is what these tests plant.
///
/// ⚠ Each row names its venue by NUMBER: `account.venue_id` is `NOT NULL` since the venue-links
/// flip, the fold reads accounts through it, and the shipped `account` has had no text `venue` at
/// all since that plan's second release. The `DDL` batch [`planted`] runs seeds no `venue` row, so
/// the venue is filed first — the same `INSERT OR IGNORE` the roster top-up uses — and the row
/// takes its number from it in the same statement.
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

/// **The `account` table as a store OLDER than `armed` carries it** — the shipped shape minus
/// that one column, with the settings tables beside it.
///
/// Planted as real DDL rather than as an approximation, for the reason `plant_schema_1` gives:
/// a test that plants its own guess proves the reader against a table nobody ever shipped.
///
/// ⚠ **`tier_vocabulary` exists because there are now TWO such stores and they take DIFFERENT
/// paths.** A table carrying the pre-rename `'sim'` list is REBUILT wholesale by
/// `crate::schema::migrate_sim_tier_to_paper` — which gives it `armed` at the DDL's own
/// position, because the rebuild runs the DDL — while a table already on `'paper'` is left
/// alone and gains `armed` through this module's `ALTER TABLE`, which can only APPEND. The
/// first is the live-box shape; the second is the shape every future post-freeze column will
/// produce, and the reader has to answer from both.
///
/// ⚠ **`id_declaration` is the SECOND thing that decides the path, and stage 4 is what made it
/// one.** `crate::schema::migrate_tables_onto_autoincrement` rebuilds any table whose `id`
/// carries no `AUTOINCREMENT` — which is every table planted here by default — so an UNARMED
/// plant now reaches the DDL's own column order whatever its tier vocabulary says, and the
/// `ALTER` path becomes unreachable through it. An ARMED plant is skipped by that rebuild and
/// is the only remaining route to an APPENDED column, which is also the exact shape every
/// post-stage-4 store will be in when the NEXT post-freeze column lands. Call sites spell the
/// declaration rather than taking a flag, so the shape under test is readable at the test.
///
/// ⚠ **The venue-links flip is the THIRD repair, and it closed the `ALTER` path to everything this
/// function plants.** The table predates `venue_id`, so
/// `crate::schema::migrate_venue_links_onto_venue_id` REBUILDS it on the first write whatever the
/// two arguments say, and `armed` lands at the DDL's own position. The appended-column store is
/// planted by [`plant_flipped_account_table_without_armed`] instead; this function keeps the
/// live-box shape it was first written for. It also carries the book index and `venue_arming`'s
/// two, keyed on the TEXT `venue` as every store holding these tables holds them: the shipped batch
/// skips a name the store has, and without the names it would build the `venue_id`-keyed indexes
/// on tables that have no `venue_id` (`no such column: venue_id`, measured).
fn plant_account_table_without_armed(
    dir: &Path,
    tier_vocabulary: &str,
    id_declaration: &str,
) -> PathBuf {
    let db = crate::dotenv::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    // The shipped `account` table MINUS `armed`, and the settings tables beside it. Planted as
    // real DDL rather than as an approximation, for the reason `plant_schema_1` gives.
    conn.execute_batch(
        &"CREATE TABLE account (
                 id               @ID@,
                 venue            TEXT    NOT NULL,
                 tier             TEXT    NOT NULL,
                 label            TEXT,
                 venue_account_id TEXT,
                 parent_id        INTEGER REFERENCES account(id),
                 active           INTEGER NOT NULL DEFAULT 1,
                 last_verified_at TEXT,
                 notes            TEXT,
                 UNIQUE (venue, tier, label),
                 CHECK (tier IN (@TIERS@)),
                 CHECK (active IN (0, 1))
             ) STRICT;
             CREATE TABLE setting (
                 id      INTEGER PRIMARY KEY,
                 section TEXT NOT NULL,
                 key     TEXT NOT NULL,
                 value   TEXT NOT NULL,
                 notes   TEXT,
                 UNIQUE (section, key),
                 CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
             ) STRICT;
             CREATE TABLE venue_arming (
                 id    INTEGER PRIMARY KEY,
                 venue TEXT NOT NULL,
                 label TEXT,
                 mode  TEXT NOT NULL,
                 notes TEXT,
                 CHECK (mode IN ('paper', 'demo', 'live'))
             ) STRICT;
             CREATE UNIQUE INDEX account_one_account_per_book
                 ON account (venue, venue_account_id)
                 WHERE active = 1 AND venue_account_id IS NOT NULL;
             CREATE UNIQUE INDEX venue_arming_one_per_venue
                 ON venue_arming (venue) WHERE label IS NULL;
             CREATE UNIQUE INDEX venue_arming_one_per_account
                 ON venue_arming (venue, label) WHERE label IS NOT NULL;
             INSERT INTO account (id, venue, tier) VALUES (1, 'binance', 'live');"
            .replace("@TIERS@", tier_vocabulary)
            .replace("@ID@", id_declaration),
    )
    .unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    drop(conn);
    db
}

/// **The shipped batch with `armed` taken out of `account`** — the store every repair in the
/// funnel DECLINES (`'paper'`, `AUTOINCREMENT`, no dead column, step 7's tier, a `NOT NULL`
/// `venue_id`), so `armed` can only arrive by `ALTER TABLE`, APPENDED. It is the shape the NEXT
/// post-freeze column will find on every carried store.
///
/// ⚠ **Derived from the shipped `DDL`, where its sibling above is spelled out**, because what it
/// has to be is *every repair already done*, which the batch itself defines and which moves each
/// time a repair lands. Each removal is asserted to match exactly once.
fn plant_flipped_account_table_without_armed(dir: &Path) -> PathBuf {
    let mut ddl = crate::schema::DDL.to_string();
    for line in
        ["    armed            INTEGER NOT NULL DEFAULT 0,\n", "    CHECK (armed IN (0, 1)),\n"]
    {
        assert_eq!(ddl.matches(line).count(), 1, "the shipped DDL must spell {line:?} once");
        ddl = ddl.replacen(line, "", 1);
    }
    let db = crate::dotenv::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(&ddl).unwrap();
    // By number alone: the shipped `account` has no text `venue` since the venue-links plan's
    // second release, and this plant IS the shipped batch minus one column.
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
/// ⚠ This is the live-box shape: `crate::schema::DDL` is `CREATE TABLE IF NOT EXISTS`, so the
/// column reaches an already-migrated store through an `ALTER TABLE` and not before. A reader
/// that named `armed` unconditionally would answer `no such column` on both boxes for a store
/// that is simply older than it — the same hazard `ensure_arming_columns` exists for, one
/// table over.
///
/// ⚠ **WHICH path delivers the column changed on 2026-09-23 and this test does not care,
/// deliberately.** Its fixture is on the pre-rename `'sim'` vocabulary, so
/// `crate::schema::migrate_sim_tier_to_paper` REBUILDS the table from the DDL — `armed`
/// included — before the fold's own `ALTER` is ever consulted. The assertion is that the
/// column is THERE after a write and the bit was folded, which is the property an operator
/// depends on; naming the mechanism would make this test go red for a repair that worked. The
/// `ALTER` path keeps its own witness in
/// [`the_reader_answers_from_a_store_whose_armed_column_was_appended_last`], whose fixture is
/// on the paper vocabulary for exactly that reason.
#[test]
fn an_account_table_that_predates_the_armed_column_gains_it_on_the_next_write() {
    let tmp = tempfile::tempdir().unwrap();
    // The LIVE-BOX shape: no `armed`, the tier vocabulary §4.4 renamed, and no `AUTOINCREMENT`
    // — a store written before either repair, which is what both boxes hold.
    let db = plant_account_table_without_armed(
        tmp.path(),
        "'sim', 'demo', 'live'",
        "INTEGER PRIMARY KEY",
    );

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
/// ⚠ This doc said *"the physical shape BOTH LIVE BOXES get on their first write after this
/// ships"* and stage 4 made that false; see the comment at the fixture for what replaced it.
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
    // ⚠ BOTH arguments are load-bearing, and the second became so on 2026-09-23. A `'sim'`
    // table is REBUILT by `crate::schema::migrate_sim_tier_to_paper` on the very write below,
    // and an id with no `AUTOINCREMENT` is rebuilt by
    // `crate::schema::migrate_tables_onto_autoincrement` for stage 4's own reason — either
    // rebuild would give this table `armed` at the DDL's own index and make the premise
    // assertions below false. What is planted here is therefore a table BOTH repairs decline:
    // the shape that still reaches the `ALTER`, and the shape every post-stage-4 store will be
    // in when the NEXT post-freeze column lands, so the property is permanent even though this
    // fixture's provenance is not.
    //
    // ⚠ It is consequently no longer *"the physical shape BOTH LIVE BOXES get on their first
    // write"* — this test's own title said that, and stage 4 falsified it: a live box's
    // `account` is unarmed, so its first write REBUILDS rather than ALTERs and it lands on the
    // DDL's column order. The reader property is unchanged and is why the test stays.
    //
    // ⚠ The venue-links flip added a THIRD repair the table must decline, and the hand-spelled
    // plant this called with `'paper'` and `AUTOINCREMENT` could not: it predates `venue_id`, so
    // `crate::schema::migrate_venue_links_onto_venue_id` rebuilt it and `armed` stopped being
    // appended. The plant is now the shipped batch minus `armed`, which declines all of them.
    let db = plant_flipped_account_table_without_armed(tmp.path());
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
    let db = crate::dotenv::db_path_in(tmp.path());
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
    assert!(matches!(found, SettingsSource::TablesAbsent { .. }), "{found:?}");
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
/// posture `crate::store::resolve` already takes between an ABSENT credential store and an
/// unreadable one.
#[test]
fn a_store_that_exists_and_will_not_open_is_an_error_not_an_absent_one() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::dotenv::db_path_in(tmp.path());
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
/// `venue_arming.venue_id` has been `NOT NULL` since the venue-links flip, and the engine checks
/// `NOT NULL` before any `CHECK`. This row used to name no number, so it was refused for THAT and
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
    // ⚠ `venue_arming.venue_id` is `NOT NULL` since the venue-links flip, and the `DDL` batch seeds
    // no `venue` row: file the venue, then let each row take its number in its own statement.
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

// -----------------------------------------------------------------------------------------
// `profile_risk` — 0057 Phase 2
// -----------------------------------------------------------------------------------------

fn live_profile() -> StoredProfileRisk {
    StoredProfileRisk {
        profile: "run-live.toml".into(),
        rows: vec![
            ProfileRiskRow { key: "max_leverage".into(), value: "3.0".into() },
            ProfileRiskRow { key: "max_notional_per_order".into(), value: "250.0".into() },
            ProfileRiskRow { key: "max_total_exposure".into(), value: "1000.0".into() },
        ],
    }
}

#[test]
fn a_project_with_no_database_reads_as_no_database_and_refuses_a_profile_write() {
    let tmp = tempfile::tempdir().unwrap();
    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert!(matches!(found, ProfileRiskSource::NoDatabase { .. }), "{found:?}");
    assert!(found.profiles().is_none());

    let err = write_profile_risk_in(tmp.path(), &live_profile()).unwrap_err();
    assert!(matches!(err.kind, DbErrorKind::NoSettingsDatabase), "{err}");
    assert!(
        !crate::dotenv::db_path_in(tmp.path()).exists(),
        "the refusal must leave NO database behind — its existence is the credential backend's \
             whole choice"
    );
}

#[test]
fn a_profile_round_trips_and_a_re_mirror_replaces_only_its_own_rows() {
    let tmp = tempfile::tempdir().unwrap();
    planted(tmp.path());

    let live = live_profile();
    let paper = StoredProfileRisk {
        profile: "run-paper.toml".into(),
        rows: vec![ProfileRiskRow { key: "max_leverage".into(), value: "10.0".into() }],
    };
    let first = write_profile_risk_in(tmp.path(), &live).unwrap();
    assert_eq!((first.rows, first.replaced), (3, 0));
    write_profile_risk_in(tmp.path(), &paper).unwrap();

    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert_eq!(found.profiles(), Some(&[live.clone(), paper.clone()][..]));

    // Re-mirroring `run-live.toml` with FEWER keys drops its removed rows...
    let shrunk = StoredProfileRisk {
        profile: "run-live.toml".into(),
        rows: vec![ProfileRiskRow { key: "max_leverage".into(), value: "2.0".into() }],
    };
    let again = write_profile_risk_in(tmp.path(), &shrunk).unwrap();
    assert_eq!((again.rows, again.replaced), (1, 3));
    // ...and leaves the OTHER profile exactly where it was, which is the whole reason this
    // writer is scoped to one profile rather than replacing the table.
    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert_eq!(found.profiles(), Some(&[shrunk, paper][..]));
}

/// A store migrated before Phase 2 says so BY NAME rather than reading as "this profile sets
/// no ceilings", which is the distinction the enum exists for.
#[test]
fn a_store_without_the_table_reads_as_table_absent_rather_than_as_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db = crate::dotenv::db_path_in(tmp.path());
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(
        "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).unwrap();
    drop(conn);

    let found = read_profile_risk_in(tmp.path()).unwrap();
    assert!(matches!(found, ProfileRiskSource::TableAbsent { .. }), "{found:?}");
    assert!(found.profiles().is_none());
    assert!(found.to_string().contains("config mirror --profile"), "{found}");

    // ...and a write CREATES it, on the same store, without a schema-version bump.
    let written = write_profile_risk_in(tmp.path(), &live_profile()).unwrap();
    assert!(written.table_created);
    assert_eq!(read_profile_risk_in(tmp.path()).unwrap().profiles().unwrap().len(), 1);
}

/// `UNIQUE (profile, key)` is a gate, not a comment: two rows for one key of one profile are
/// refused by the engine, which is the half a Rust-side replace can never cover.
#[test]
fn the_schema_refuses_a_second_row_for_one_key_of_one_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let db = planted(tmp.path());
    let conn = rusqlite::Connection::open(&db).unwrap();
    let insert = |profile: &str, key: &str, value: &str| {
        conn.execute(
            "INSERT INTO profile_risk (profile, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![profile, key, value],
        )
    };
    insert("run-live.toml", "max_leverage", "3.0").unwrap();
    assert!(insert("run-live.toml", "max_leverage", "9.0").is_err(), "one row per key");
    // A DIFFERENT profile is a different file and is allowed to carry the same key.
    insert("run-paper.toml", "max_leverage", "9.0").unwrap();
}

// --------------------------------------------------------------------------------------------
// `venue_arming.max_exposure` — the column added to a table that already exists on two live
// boxes. Every test here is about that asymmetry, because the DDL cannot express it: every
// statement in it is `CREATE TABLE IF NOT EXISTS`, which does nothing at all to a table that is
// already there.
// --------------------------------------------------------------------------------------------

/// A store planted the way both migrated boxes actually look: the settings tables exist, built
/// by an earlier `config mirror`, and `venue_arming` has NO `max_exposure` column because the
/// code that created it predates one.
///
/// ⚠ The DDL here is the OLD shape, spelled out rather than derived, for the reason
/// `plant_schema_1` gives about approximations: a fixture that built the old table by editing
/// the new one would stop being the old one the day the new one changes again.
///
/// ⚠ **`venue_arming`'s two indexes are part of that old shape**, keyed on the TEXT `venue` as the
/// batch spelled them from the day the table was created — every store holding the table holds
/// them. They were left out until the venue-links flip re-keyed them on `venue_id`, and the leaving
/// out stopped being harmless that day: the shipped batch's `CREATE UNIQUE INDEX IF NOT EXISTS`
/// skips a name the store already has, but on a table missing the name it builds the index, and
/// this table has no `venue_id` to build it on (`no such column: venue_id`, measured).
fn planted_without_the_column(dir: &Path) -> PathBuf {
    let db = crate::dotenv::db_path_in(dir);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
    conn.execute_batch(
        "CREATE TABLE node_key (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
             CREATE TABLE venue_arming (
                 id    INTEGER PRIMARY KEY,
                 venue TEXT NOT NULL,
                 label TEXT,
                 mode  TEXT NOT NULL,
                 notes TEXT,
                 CHECK (mode IN ('paper', 'demo', 'live'))
             ) STRICT;
             CREATE UNIQUE INDEX venue_arming_one_per_venue
                 ON venue_arming (venue) WHERE label IS NULL;
             CREATE UNIQUE INDEX venue_arming_one_per_account
                 ON venue_arming (venue, label) WHERE label IS NOT NULL;
             CREATE TABLE setting (
                 id      INTEGER PRIMARY KEY,
                 section TEXT NOT NULL,
                 key     TEXT NOT NULL,
                 value   TEXT NOT NULL,
                 CHECK (section IN ('policy', 'config', 'preferences', 'flags'))
             ) STRICT;
             INSERT INTO venue_arming (venue, label, mode) VALUES ('binance', NULL, 'demo');",
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
    // ⚠ `venue_arming.venue_id` is `NOT NULL` since the venue-links flip, and the engine checks that
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
