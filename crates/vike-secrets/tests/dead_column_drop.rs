//! **§9 stage 4c's dropped column, proved against a store that ALREADY EXISTS.**
//!
//! §2.7 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` names one dead
//! column — `venue_arming.notes`, written by nothing and read by nothing — and stage 4c takes it
//! out of `crates/vike-secrets/src/schema.rs`'s `DDL`. Taking it out of the batch is the easy half
//! and it is **not the half that reaches the CI box or the dev box**:
//!
//! * `DDL` is `CREATE TABLE IF NOT EXISTS` throughout, so it changes NOTHING about a table that is
//!   already there. A store born after the edit simply never has the column, which is a statement
//!   about a temp directory and about no live box at all.
//! * `schema::migrate_tables_onto_autoincrement` — the rebuild stage 4a added — DOES drop an
//!   unknown column as a side effect of copying the column INTERSECTION, but only on the one
//!   occasion it rebuilds a table, and it cannot help here for two independent reasons: its
//!   trigger is *this table does not yet declare `AUTOINCREMENT`*, which goes FALSE FOREVER after
//!   the first rebuild, and `venue_arming` is deliberately not one of the tables it visits at all.
//!
//! So the drop needs a delivery path of its own — `schema::migrate_dropped_columns`, triggered on
//! the state that is still true (*the store's table still HAS the column*) — and that path is what
//! this file tests. `crates/vike-secrets/tests/sqlite_sequence_gate.rs`'s
//! `an_existing_unarmed_account_table_is_armed_by_the_next_write_and_keeps_its_ids` is the worked
//! example of the same distinction for stage 4a, and the reason it exists.
//!
//! # How each test avoids proving nothing
//!
//! * **The fixture's hostility is ASSERTED** ([`the_pre_drop_store_really_carries_the_column`]):
//!   the planted table has the column and a row with prose in it, and the SHIPPED batch does not
//!   declare it. Without that, every assertion below would pass on a store that never had it.
//! * **The fixture is the SHIPPED batch wearing ONE difference** ([`aged_ddl`]), so it is this
//!   store's own schema with the column put back rather than a second spelling of the schema — the
//!   discipline `crates/vike-secrets/tests/paper_tier.rs` states for its own fixture.
//! * ⚠ **…and therefore every OTHER table in the fixture already declares `AUTOINCREMENT`**, which
//!   is the post-stage-4a state both live boxes reach. [`the_autoincrement_pass_has_nothing_to_do_here`]
//!   asserts that positively, so the drop below cannot be attributed to the rebuild that runs
//!   beside it: that pass's trigger is false for every table it visits, and `venue_arming` is not
//!   one of them.
//! * **The writer touches neither the table nor the credential plane.** `set_venue_setting_in`
//!   writes a `venue_setting` row; it is shown reaching the repair funnel, not doing the work
//!   itself.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// The fixture — a store as it stood before the column was dropped
// ---------------------------------------------------------------------------------------------

/// The table the drop is about, and the column it lost.
const TABLE: &str = "venue_arming";
const DROPPED: &str = "notes";

/// The shipped `DDL` with `venue_arming.notes` put BACK — the ONE difference from the batch this
/// binary ships.
///
/// ⚠ **Derived rather than transcribed.** A fixture that spells its own `CREATE TABLE` proves the
/// migration against a table nobody ever shipped, and it silently stops aging anything the day the
/// real statement moves. The anchor is `max_exposure REAL,`, which occurs exactly once in the
/// batch — asserted, because a second occurrence would put the column on the wrong table and every
/// test below would then pass for the wrong reason.
fn aged_ddl() -> String {
    const ANCHOR: &str = "    max_exposure REAL,\n";
    let ddl = vike_secrets::DDL;
    assert_eq!(
        ddl.matches(ANCHOR).count(),
        1,
        "the fixture's anchor must name exactly one column of one table — if this fails the \
         re-insertion below lands somewhere unintended and nothing here is measuring the drop"
    );
    assert!(
        !venue_arming_block(ddl).contains(DROPPED),
        "the SHIPPED batch must NOT declare `{TABLE}.{DROPPED}` — this test file exists because it \
         was dropped, and if it is back, `migrate_dropped_columns` refuses every write instead"
    );
    let aged = ddl.replace(ANCHOR, &format!("    {DROPPED}    TEXT,\n{ANCHOR}"));
    assert!(
        venue_arming_block(&aged).contains(DROPPED),
        "…and the aged copy must carry it, or the fixture is the shipped schema and proves nothing"
    );
    aged
}

/// The `CREATE TABLE … venue_arming ( … ) STRICT` body of a batch, so the assertions above ask
/// about THAT table rather than about the word `notes` appearing anywhere in a batch that has six
/// other `notes` columns.
fn venue_arming_block(ddl: &str) -> String {
    let head = format!("CREATE TABLE IF NOT EXISTS {TABLE} (");
    let at = ddl.find(&head).expect("the batch must declare the table this file is about");
    let rest = &ddl[at + head.len()..];
    let end = rest.find(") STRICT").expect("…and it must be terminated");
    rest[..end].to_string()
}

/// One `venue_arming` row as the columns that SURVIVE the drop hold it — `notes` is deliberately
/// absent, because a row type naming it could not be read from the rebuilt table at all.
///
/// A named struct rather than the tuple this started as: clippy refuses a five-member tuple return
/// (`type_complexity`, which `-D warnings` makes an error), and the failure message an
/// `assert_eq!` prints names its fields instead of counting positions.
#[derive(Debug, PartialEq)]
struct ArmingRow {
    id: i64,
    venue: String,
    label: Option<String>,
    mode: String,
    max_exposure: Option<f64>,
}

/// A settings directory holding a store at the PRE-DROP shape: the aged batch, two `venue_arming`
/// rows (one carrying prose in the doomed column, one not), and an account with a credential
/// hanging off it so the rebuild's own `pragma_foreign_key_check` has something to be wrong about.
struct Aged {
    _dir: tempfile::TempDir,
    settings: PathBuf,
    db: PathBuf,
}

impl Aged {
    fn build() -> Aged {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let db = vike_secrets::db_path_in(&settings);
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("db dir");

        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA foreign_keys = ON;")
            .expect("pragmas");
        conn.execute_batch(&aged_ddl()).expect("aged ddl");
        conn.execute_batch(
            "INSERT INTO venue (name) VALUES ('binance');
             INSERT INTO account (id, venue, tier, label) VALUES (1, 'binance', 'live', NULL);
             INSERT INTO credential (id, account_id, field, value, name)
                 VALUES (10, 1, 'API_KEY', 'live-value', 'BINANCE_LIVE_API_KEY');
             INSERT INTO venue_arming (id, venue, label, mode, notes, max_exposure)
                 VALUES (5, 'binance', NULL, 'live', 'an operator wrote this', 250.0);
             INSERT INTO venue_arming (id, venue, label, mode, notes, max_exposure)
                 VALUES (6, 'okx', NULL, 'demo', NULL, NULL);",
        )
        .expect("aged rows");
        conn.pragma_update(None, "user_version", 2i64).expect("stamp");
        drop(conn);

        Aged { _dir: dir, settings, db }
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).expect("open")
    }

    /// The `CREATE TABLE` text the engine is holding for `table`.
    fn table_sql(&self, table: &str) -> String {
        self.conn()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |r| r.get::<_, String>(0),
            )
            .unwrap_or_else(|e| panic!("`{table}` must exist: {e}"))
    }

    /// `table`'s column names, as the ENGINE answers rather than as the batch reads — the two are
    /// the same question only when the rebuild actually ran.
    fn columns(&self, table: &str) -> Vec<String> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})")).expect("prepare");
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).expect("query");
        rows.map(Result::unwrap).collect()
    }

    /// Every `venue_arming` row as the surviving columns hold it, ordered by id.
    fn arming_rows(&self) -> Vec<ArmingRow> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, venue, label, mode, max_exposure FROM venue_arming ORDER BY id")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| {
                Ok(ArmingRow {
                    id: r.get(0)?,
                    venue: r.get(1)?,
                    label: r.get(2)?,
                    mode: r.get(3)?,
                    max_exposure: r.get(4)?,
                })
            })
            .expect("query");
        rows.map(Result::unwrap).collect()
    }

    /// The named indexes the engine holds for `table`.
    fn indexes(&self, table: &str) -> Vec<String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .expect("prepare");
        let rows = stmt.query_map([table], |r| r.get::<_, String>(0)).expect("query");
        rows.map(Result::unwrap).collect()
    }

    /// Drive a PUBLIC writer, which is what reaches `db::ensure_venue_id_columns` and therefore
    /// the migration. Deliberately a writer that touches NEITHER the dropped column's table nor
    /// the credential plane, so nothing here can be read as the writer having done the work.
    fn write_something_unrelated(&self) {
        vike_secrets::set_venue_setting_in(self.dir(), "polymarket", None, "PROXY_HOST", "1.2.3.4")
            .expect("the public writer must succeed");
    }

    /// The engine's own whole-database referential check.
    fn foreign_key_violations(&self) -> i64 {
        self.conn()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
            .expect("fk check")
    }
}

// ---------------------------------------------------------------------------------------------
// The premise — this fixture really is a store the drop has not reached
// ---------------------------------------------------------------------------------------------

/// **Without this test every assertion below is vacuous.** It shows the planted store carrying the
/// column AND a value in it, so a later absence is evidence the migration removed it rather than
/// evidence it was never there.
#[test]
fn the_pre_drop_store_really_carries_the_column() {
    let fx = Aged::build();

    assert!(
        fx.columns(TABLE).iter().any(|c| c == DROPPED),
        "the fixture must carry `{TABLE}.{DROPPED}`: {:?}",
        fx.columns(TABLE)
    );
    let note: Option<String> = fx
        .conn()
        .query_row("SELECT notes FROM venue_arming WHERE id = 5", [], |r| r.get(0))
        .expect("the planted row exists");
    assert_eq!(
        note.as_deref(),
        Some("an operator wrote this"),
        "…and a value in it, so this file is measuring a column with DATA rather than an empty \
         declaration — which is the whole reason `account.notes` and `credential.notes` were NOT \
         dropped beside it"
    );
}

/// ⚠ **The rebuild running beside this one cannot be what drops the column**, and that has to be
/// asserted rather than argued, because both passes run in the same transaction on the same store.
///
/// `migrate_tables_onto_autoincrement`'s trigger is *this table does not yet declare
/// `AUTOINCREMENT`*. The fixture is the shipped batch with one column put back, so every table
/// that batch arms is ALREADY armed here — the state both live boxes are in once stage 4a ships —
/// and that pass therefore has nothing to do. `venue_arming` is not in its list at all, which is
/// the second half and the reason this migration needs its own trigger.
#[test]
fn the_autoincrement_pass_has_nothing_to_do_here() {
    let fx = Aged::build();

    for table in ["node_key", "venue", "account", "credential", "venue_setting", "setting"] {
        assert!(
            fx.table_sql(table).to_uppercase().contains("AUTOINCREMENT"),
            "`{table}` must ALREADY be armed in the fixture, or a drop seen below could be the \
             autoincrement rebuild's intersection copy rather than this migration: {}",
            fx.table_sql(table)
        );
    }
    assert!(
        !fx.table_sql(TABLE).to_uppercase().contains("AUTOINCREMENT"),
        "…and `{TABLE}` is deliberately NOT armed, so it is not even a table that pass visits"
    );
}

// ---------------------------------------------------------------------------------------------
// The migration
// ---------------------------------------------------------------------------------------------

/// **One ordinary write takes the dead column off a store that already exists** — the half the
/// batch alone can never deliver, and the only claim in this file that is about the CI box and the dev
/// box rather than about a temp directory.
#[test]
fn a_public_write_drops_the_dead_column_from_an_existing_store() {
    let fx = Aged::build();
    let before = fx.arming_rows();
    fx.write_something_unrelated();

    // The NEW state, asserted of the ENGINE and of the stored statement — a rebuild that left the
    // old body in place would answer the first and fail the second.
    assert!(
        !fx.columns(TABLE).iter().any(|c| c == DROPPED),
        "`{TABLE}.{DROPPED}` must be gone from the table the engine holds: {:?}",
        fx.columns(TABLE)
    );
    assert!(
        !fx.table_sql(TABLE).contains(DROPPED),
        "…and out of its `CREATE TABLE` text: {}",
        fx.table_sql(TABLE)
    );

    // …and nothing else moved. Ids included: `venue_arming` is keyed by `(venue, label)` through
    // two partial indexes and re-numbering its rows would be invisible to a value comparison.
    assert_eq!(
        fx.arming_rows(),
        before,
        "every surviving column of every row must carry, with its id — the rebuild copies the \
         INTERSECTION and an id it failed to copy would be handed a fresh one by the engine"
    );
    assert_eq!(
        fx.indexes(TABLE),
        vec!["venue_arming_one_per_account".to_string(), "venue_arming_one_per_venue".to_string()],
        "both partial indexes must be back: they belonged to the table that was dropped, and the \
         closing `DDL` pass is the only thing that re-creates them"
    );
    assert_eq!(fx.foreign_key_violations(), 0, "no credential lost the account it hangs off");
}

/// **The credentials still read back through the PUBLIC reader.** A rebuild anywhere in this
/// transaction that re-filed a row against the wrong account would still `SELECT` fine and would
/// answer an operator with nothing.
#[test]
fn the_rebuild_does_not_lose_a_credential() {
    let fx = Aged::build();
    let arg = Some(fx.settings.to_str().expect("utf-8 temp path"));
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(arg)
            .get("BINANCE_LIVE_API_KEY")
            .map(String::as_str),
        Some("live-value"),
        "the premise: the planted credential reads back BEFORE the migration"
    );

    fx.write_something_unrelated();

    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(arg)
            .get("BINANCE_LIVE_API_KEY")
            .map(String::as_str),
        Some("live-value"),
        "…and after it"
    );
}

/// **IDEMPOTENT** — a second write must not rebuild again. The trigger is a column the first write
/// removed, so a repair that fired regardless would rewrite the table on every credential an
/// operator adds, forever.
#[test]
fn the_drop_is_a_no_op_the_second_time() {
    let fx = Aged::build();
    fx.write_something_unrelated();
    let once = fx.table_sql(TABLE);
    let rows = fx.arming_rows();

    fx.write_something_unrelated();
    assert_eq!(fx.table_sql(TABLE), once, "the table must not be rebuilt a second time");
    assert_eq!(fx.arming_rows(), rows, "…and no row moved");
    assert_eq!(fx.foreign_key_violations(), 0);
}

/// **A store BORN after the drop never had the column**, which is the other half of idempotence —
/// and, on its own, the test that would have proved NOTHING about a live box. It is here as the
/// control, not as the claim.
#[test]
fn a_store_born_after_the_drop_never_carries_the_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    std::fs::write(settings.join("secrets.env"), "BINANCE_LIVE_API_KEY=k\n").expect("store");
    let arg = Some(settings.to_str().expect("utf-8"));
    vike_secrets::migrate(arg, |_| false, &classify).expect("migration");

    let db = vike_secrets::db_path_in(&settings);
    let columns = |table: &str| {
        let conn = rusqlite::Connection::open(&db).expect("open");
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})")).expect("prepare");
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).expect("query");
        rows.map(Result::unwrap).collect::<Vec<String>>()
    };
    let sql_of = |table: &str| {
        rusqlite::Connection::open(&db)
            .expect("open")
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |r| r.get::<_, String>(0),
            )
            .expect("table")
    };

    assert!(
        !columns(TABLE).iter().any(|c| c == DROPPED),
        "a fresh store is born without it: {:?}",
        columns(TABLE)
    );
    let before = sql_of(TABLE);
    vike_secrets::set_venue_setting_in(&settings, "polymarket", None, "PROXY_HOST", "1.2.3.4")
        .expect("write");
    assert_eq!(
        sql_of(TABLE),
        before,
        "…and the table must not be rebuilt on a store that never carried the column"
    );
}

// ---------------------------------------------------------------------------------------------
// Support
// ---------------------------------------------------------------------------------------------

/// A deliberately SIMPLER classifier than the production one, for the reason
/// `crates/vike-secrets/tests/paper_tier.rs`' own copy gives: this crate cannot link the crate that
/// owns the real one.
fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    if let Some(field) = name.strip_prefix("BINANCE_LIVE_") {
        return Classification {
            placement: Placement::Account(AccountKey {
                venue: "binance".to_string(),
                tier: vike_secrets::account_tier_of_key_token("LIVE"),
                label: None,
                discriminator: None,
            }),
            secret: true,
            field: field.to_string(),
            recognised: true,
            pending_move: None,
        };
    }
    Classification::unrecognised(name)
}
