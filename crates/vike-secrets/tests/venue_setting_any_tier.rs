//! **§5.2 step 7 — `venue_setting`'s `tier IS NULL` becomes `'any'`, proved against a store that
//! PREDATES it.**
//!
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` §2.6 diagnoses the shape:
//! a NULL in `tier` meant "applies to any tier", two PARTIAL unique indexes encoded the split (one
//! `WHERE tier IS NULL`, one `WHERE tier IS NOT NULL` — SQL `UNIQUE` does not constrain NULLs), and
//! owner ruling 2 refuses a NULL that decides what KIND of row this is. Step 7 stores the literal
//! `'any'` instead, and the two partial indexes collapse into ONE total `UNIQUE`.
//!
//! # The trap that got this split off stage 4a, and what each test here holds against it
//!
//! `crates/vike-secrets/src/venue_setting.rs`'s `venue_setting_names` renders legacy credential key
//! NAMES from a `tier: Option<&str>`. A stored `'any'` that reached it UNMAPPED renders
//! `{HEAD}_ANY_{FIELD}` — a name no store holds — so every legacy row becomes UNREACHABLE. Not
//! wrong: invisible. Nothing errors, the reader answers "not configured", and a bridge falls back
//! to its built-in default — which is exactly how the polymarket egress proxy was read by nothing
//! once before (`crates/vike-secrets/tests/venue_setting_fold.rs` carries that incident).
//!
//! So the load-bearing assertions are made THROUGH THE PUBLIC READERS — the whole-map
//! `load_workspace_dotenv_from` and the scoped `resolve_project_scoped` that
//! `crates/bridges/polymarket/src/egress.rs` reaches — never through a `SELECT`: a migration that
//! wrote `'any'` perfectly and a reader that forgot to map it back would pass every raw check in
//! this file and fail every operator.
//!
//! # How each test avoids proving nothing
//!
//!   * **the fixture's hostility is ASSERTED** — [`the_pre_step_7_store_is_the_shape_step_7_repairs`]
//!     shows the aged table admitting a NULL tier, refusing `'any'`, and refusing a second NULL row
//!     for one `(venue, field)`. Every later "the row is now `'any'`" is therefore a statement about
//!     the migration, not about SQLite being permissive.
//!   * **the fixture is DERIVED from the shipped `DDL`** by `support::pre_any_tier_ddl`, which
//!     asserts each reverted literal matched exactly once.
//!   * **the NEW state is what is asserted**, never merely the absence of the old: `'any'` in the
//!     stored `CHECK`, `tier` declared `NOT NULL`, the legacy names ANSWERING through the reader.
//!   * **both writer doors are driven** — a venue-setting write AND a credential write — because
//!     `crates/vike-secrets/tests/paper_tier.rs` records a green that proved the right property on
//!     the wrong writer for a day.
//!
//! ⚠ Values are obviously fake. No real credential exists anywhere near this file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_secrets::{AccountKey, Classification, KeyScope, Lookup, Placement, Table};

mod support;

// ---------------------------------------------------------------------------------------------
// The fixture — a store as the CI box held it before step 7
// ---------------------------------------------------------------------------------------------

/// The legacy names the planted `venue_setting` rows answer for, with the value each must carry.
///
/// ⚠ Four RENDER PATHS, deliberately: a machine-scoped HAND-MAPPED family (`POLY_` — the head no
/// roster venue is spelled as), a machine-scoped CONFORMING venue (`IBKR_HOST`, which an unmapped
/// `'any'` would render `IBKR_ANY_HOST`), a tier-scoped conforming row, and a tier-scoped
/// hand-mapped one that renders TWO names. A mapping bug that only one branch of the renderer can
/// see is still caught.
const ROWS_ANSWER: [(&str, &str); 6] = [
    ("POLY_PROXY_HOST", "proxy.example.invalid"),
    ("POLY_PROXY_PORT", "1080"),
    ("IBKR_HOST", "<host>"),
    ("IBKR_DEMO_BACKEND", "cpapi"),
    ("DUKASCOPY_DEMO1_SERVER", "https://jnlp.example.invalid/demo.jnlp"),
    ("DUKASCOPY_DEMO2_SERVER", "https://jnlp.example.invalid/demo.jnlp"),
];

/// The ids of the three rows planted with a NULL tier — the rows step 7 exists to carry.
const NULL_TIER_IDS: [i64; 3] = [1, 2, 3];

/// The top id planted and then DELETED, so the table's `AUTOINCREMENT` mark sits ABOVE every
/// surviving row — the state in which a rebuild that replayed the survivors without carrying the
/// mark would REWIND it (§4.1's hazard).
const REMOVED_TOP_ID: i64 = 9;

/// A settings directory holding a store at the PRE-STEP-7 shape: `support::pre_any_tier_ddl`, one
/// account with a credential, and the `venue_setting` rows [`ROWS_ANSWER`] names.
struct PreAnyTier {
    _dir: tempfile::TempDir,
    settings: PathBuf,
    db: PathBuf,
}

impl PreAnyTier {
    fn build() -> PreAnyTier {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let db = vike_secrets::db_path_in(&settings);
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("db dir");

        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA foreign_keys = ON;")
            .expect("pragmas");
        conn.execute_batch(&support::pre_any_tier_ddl()).expect("aged ddl");
        conn.execute_batch(
            "INSERT INTO venue (name) VALUES ('binance'), ('ibkr'), ('polymarket'), ('dukascopy');
             INSERT INTO account (id, venue, venue_id, tier, label)
                 SELECT 1, 'binance', id, 'live', NULL FROM venue WHERE name = 'binance';
             INSERT INTO credential (id, account_id, field, value, name)
                 VALUES (10, 1, 'API_KEY', 'live-value', 'BINANCE_LIVE_API_KEY');
             INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
                 SELECT 1, 'polymarket', id, NULL, 'PROXY_HOST', 'proxy.example.invalid'
                 FROM venue WHERE name = 'polymarket';
             INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
                 SELECT 2, 'polymarket', id, NULL, 'PROXY_PORT', '1080'
                 FROM venue WHERE name = 'polymarket';
             INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
                 SELECT 3, 'ibkr', id, NULL, 'HOST', '<host>' FROM venue WHERE name = 'ibkr';
             INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
                 SELECT 4, 'ibkr', id, 'demo', 'BACKEND', 'cpapi' FROM venue WHERE name = 'ibkr';
             INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
                 SELECT 5, 'dukascopy', id, 'demo', 'SERVER',
                        'https://jnlp.example.invalid/demo.jnlp'
                 FROM venue WHERE name = 'dukascopy';
             INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
                 SELECT 9, 'ibkr', id, 'live', 'BACKEND', 'removed' FROM venue WHERE name = 'ibkr';
             DELETE FROM venue_setting WHERE id = 9;",
        )
        .expect("aged rows");
        conn.pragma_update(None, "user_version", 2i64).expect("stamp");
        drop(conn);

        PreAnyTier { _dir: dir, settings, db }
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    fn conn(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.db).expect("open")
    }

    /// The `CREATE TABLE venue_setting` text the engine is holding.
    fn table_sql(&self) -> String {
        self.conn()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'venue_setting'",
                [],
                |r| r.get(0),
            )
            .expect("`venue_setting` must exist")
    }

    /// Whether the ENGINE lets `venue_setting.tier` hold a NULL — asked of the table the engine
    /// holds, not of the statement text, so a respelling cannot fool it.
    fn tier_is_nullable(&self) -> bool {
        let notnull: i64 = self
            .conn()
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('venue_setting') WHERE name = 'tier'",
                [],
                |r| r.get(0),
            )
            .expect("`venue_setting.tier` must exist");
        notnull == 0
    }

    /// The b-tree page the engine keeps `venue_setting` on — the one fact a REBUILD cannot leave
    /// alone. The new table is built beside the old one before the old is dropped, so it gets a
    /// different page; the statement text, the rows and their ids all come out byte-identical, which
    /// is why none of those can tell a no-op from a rebuild that reproduced itself.
    fn rootpage(&self) -> i64 {
        self.conn()
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'venue_setting'",
                [],
                |r| r.get(0),
            )
            .expect("`venue_setting` must exist")
    }

    /// Every index the engine holds ON `venue_setting` that carries a name (the auto-index a
    /// table-level `UNIQUE` builds is named too, `sqlite_autoindex_…`), sorted.
    fn indexes(&self) -> Vec<String> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = \
                 'venue_setting' ORDER BY name",
            )
            .expect("prepare");
        stmt.query_map([], |r| r.get(0)).expect("query").map(Result::unwrap).collect()
    }

    /// `(id, venue, tier, field, value)` for every row, ordered by id.
    fn rows(&self) -> Vec<(i64, String, Option<String>, String, String)> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT id, venue, tier, field, value FROM venue_setting ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .expect("query")
            .map(Result::unwrap)
            .collect()
    }

    /// The table's `AUTOINCREMENT` high-water mark.
    fn mark(&self) -> i64 {
        self.conn()
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'venue_setting'", [], |r| {
                r.get(0)
            })
            .expect("an armed table holds a mark")
    }

    /// Try one raw `venue_setting` INSERT, returning the engine's verdict.
    fn try_insert(&self, venue: &str, tier: Option<&str>, field: &str) -> rusqlite::Result<usize> {
        self.conn().execute(
            "INSERT INTO venue_setting (venue, tier, field, value) VALUES (?1, ?2, ?3, 'x')",
            (venue, tier, field),
        )
    }

    /// The whole-map read every composition root performs.
    fn plain(&self) -> HashMap<String, String> {
        vike_secrets::load_workspace_dotenv_from(self.arg())
    }

    /// Drive the VENUE-SETTING door — `vike-cli config set venue.…`'s writer — with a row that is
    /// NOT one of the NULL-tier rows, so nothing here can be read as the writer having rewritten
    /// them itself.
    fn write_an_unrelated_venue_setting(&self) -> Result<Option<String>, vike_secrets::DbError> {
        vike_secrets::set_venue_setting_in(self.dir(), "ibkr", Some("demo"), "PORT", "4002")
    }

    /// The engine's own whole-database referential check.
    fn foreign_key_violations(&self) -> i64 {
        self.conn()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
            .expect("fk check")
    }
}

/// Every name in [`ROWS_ANSWER`] must come back from `vars` with its value.
fn assert_every_row_answers(vars: &HashMap<String, String>, when: &str) {
    for (name, value) in ROWS_ANSWER {
        assert_eq!(
            vars.get(name).map(String::as_str),
            Some(value),
            "{when}: `{name}` must answer through the PUBLIC reader. A row the reader cannot find \
             is a bridge silently on its built-in default. Names it did answer: {:?}",
            {
                let mut names: Vec<&String> = vars.keys().collect();
                names.sort();
                names
            }
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The premise — this fixture really is the shape step 7 repairs
// ---------------------------------------------------------------------------------------------

/// **Without this test every assertion below is vacuous.** It shows the aged table ADMITTING a NULL
/// tier and REFUSING the new word, so a later `'any'` row is evidence the constraint was replaced.
///
/// It also carries the premise of the NO-COLLISION PROOF step 7 rests on (the migration's own doc
/// states the proof): on this shape a second NULL-tier row for one `(venue, field)` is refused by
/// `venue_setting_one_per_machine`, a second tier-scoped row by `venue_setting_one_per_tier`, and no
/// row can already hold `'any'` — so mapping NULL onto `'any'` cannot make two surviving rows share
/// one key of the new total `UNIQUE (venue, tier, field)`.
#[test]
fn the_pre_step_7_store_is_the_shape_step_7_repairs() {
    let fx = PreAnyTier::build();

    assert!(fx.tier_is_nullable(), "the aged `tier` must admit NULL: {}", fx.table_sql());
    assert_eq!(
        fx.indexes(),
        vec!["venue_setting_one_per_machine".to_string(), "venue_setting_one_per_tier".to_string()],
        "the aged table carries exactly the two partial indexes step 7 retires"
    );
    assert_eq!(
        fx.rows().iter().filter(|r| r.2.is_none()).map(|r| r.0).collect::<Vec<_>>(),
        NULL_TIER_IDS.to_vec(),
        "the planted NULL-tier rows are there to be carried"
    );

    let any = fx.try_insert("fxcm", Some("any"), "URL").expect_err("`'any'` must be REFUSED here");
    assert!(
        any.to_string().to_lowercase().contains("constraint"),
        "the refusal must be the table's own CHECK: {any}"
    );
    let second_machine =
        fx.try_insert("polymarket", None, "PROXY_HOST").expect_err("a second NULL row must refuse");
    assert!(
        second_machine.to_string().contains("UNIQUE"),
        "…refused by `venue_setting_one_per_machine`, which is the proof's first premise: \
         {second_machine}"
    );
    let second_tiered =
        fx.try_insert("ibkr", Some("demo"), "BACKEND").expect_err("a second tier row must refuse");
    assert!(
        second_tiered.to_string().contains("UNIQUE"),
        "…and `venue_setting_one_per_tier`, the second: {second_tiered}"
    );
    assert_eq!(fx.mark(), REMOVED_TOP_ID, "the mark sits above every surviving row");

    // …and the reader answers for every row TODAY, so a later miss is the migration's doing.
    assert_every_row_answers(&fx.plain(), "before any write");
}

// ---------------------------------------------------------------------------------------------
// The migration, through both writer doors
// ---------------------------------------------------------------------------------------------

/// **THE TEST THIS STEP WAS SPLIT OFF FOR.** One ordinary venue-setting write carries the store
/// onto `'any'`, and every legacy NULL-tier row is STILL REACHABLE by its credential name through
/// the public reader afterwards.
///
/// The raw half proves the migration RAN (a reader test alone would pass on a store nothing
/// touched, since the old NULL rows read as "no tier" either way); the reader half proves it was
/// SAFE. Together they are the claim: the rows moved, and nobody can tell.
#[test]
fn a_null_tier_row_is_still_reachable_through_the_public_reader_after_the_migration() {
    let fx = PreAnyTier::build();
    let before = fx.rows();

    let previous = fx.write_an_unrelated_venue_setting().expect("the public writer must succeed");
    assert_eq!(previous, None, "the write created a row; it replaced nothing");

    // THE MIGRATION RAN — the new state asserted positively, of the engine's own table.
    assert!(!fx.tier_is_nullable(), "`tier` must now be NOT NULL: {}", fx.table_sql());
    assert!(
        fx.table_sql().contains("'any'"),
        "the rebuilt `CHECK` must admit the new word: {}",
        fx.table_sql()
    );
    assert!(
        !fx.indexes().iter().any(|i| i.starts_with("venue_setting_one_per_")),
        "the two partial indexes must be GONE — a total `UNIQUE` with one of them still beside it \
         is ruling 2's defect wearing one index instead of two: {:?}",
        fx.indexes()
    );

    // …every row carried, with its id, and every former NULL now spelled `'any'`.
    let after = fx.rows();
    for (id, venue, tier, field, value) in &before {
        let carried = after
            .iter()
            .find(|r| r.0 == *id)
            .unwrap_or_else(|| panic!("row {id} ({venue}/{field}) was LOST by the migration"));
        let want = tier.clone().or_else(|| Some("any".to_string()));
        assert_eq!(
            (&carried.1, &carried.2, &carried.3, &carried.4),
            (venue, &want, field, value),
            "row {id} must carry unchanged except that a NULL tier becomes `'any'`"
        );
    }
    assert_eq!(after.len(), before.len() + 1, "the write added exactly its own row: {after:?}");
    let written = after.iter().find(|r| r.3 == "PORT").expect("the write's own row");
    assert!(
        written.0 > REMOVED_TOP_ID,
        "the rebuild REWOUND the high-water mark: the new row was handed id {}, at or below the \
         removed top id {REMOVED_TOP_ID} — an id reused names a dead row",
        written.0
    );

    // THE READER — the half the trap is about.
    let vars = fx.plain();
    assert_every_row_answers(&vars, "after the migration");
    assert_eq!(vars.get("IBKR_DEMO_PORT").map(String::as_str), Some("4002"), "and the write's own");
    assert_eq!(vars.get("BINANCE_LIVE_API_KEY").map(String::as_str), Some("live-value"));
    let leaked: Vec<&String> = vars.keys().filter(|k| k.contains("_ANY_")).collect();
    assert!(
        leaked.is_empty(),
        "a stored `'any'` reached the renderer UNMAPPED and rendered names no store holds: \
         {leaked:?}"
    );

    // …and the SCOPED reader, which is the one `egress.rs` reaches the proxy family through.
    let scoped = vike_secrets::resolve_project_scoped(fx.arg(), &KeyScope::of(["POLY_PROXY_HOST"]))
        .expect("the store must open");
    assert_eq!(
        scoped.get("POLY_PROXY_HOST"),
        Lookup::Present("proxy.example.invalid"),
        "the machine-scoped proxy row must still answer through the SCOPED reader"
    );
    assert_eq!(fx.foreign_key_violations(), 0);
}

/// **The CREDENTIAL door carries the store too** — `vike-cli secrets set`'s writer, which is the
/// write a box performs most often and the one `crates/vike-secrets/tests/paper_tier.rs` found
/// unproven behind a green for a day.
#[test]
fn a_credential_write_carries_a_pre_step_7_store_onto_any() {
    let fx = PreAnyTier::build();

    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("OKX_DEMO_API_KEY".to_string(), "okx-value".to_string())],
        Some(&classify),
    )
    .expect("a credential write must carry the store, not refuse on it");

    assert!(!fx.tier_is_nullable(), "the credential door must reach the migration too");
    assert!(
        fx.rows().iter().all(|r| r.2.is_some()),
        "no NULL tier may survive a credential write: {:?}",
        fx.rows()
    );
    let vars = fx.plain();
    assert_every_row_answers(&vars, "after a credential write");
    assert_eq!(vars.get("OKX_DEMO_API_KEY").map(String::as_str), Some("okx-value"));
    assert_eq!(fx.foreign_key_violations(), 0);
}

/// **The WRITE boundary** — a machine-scoped write after the migration lands on the `'any'` row
/// the migration produced, reports the value it replaced, and duplicates nothing.
///
/// A writer that still bound SQL NULL for "no tier" would be refused by the new `NOT NULL`; one
/// that looked the previous value up with `tier IS NULL` would report `None` for a row that plainly
/// had a value. Both are the same boundary forgotten on the other side.
#[test]
fn a_machine_scoped_write_after_the_migration_replaces_the_any_row() {
    let fx = PreAnyTier::build();

    let was = vike_secrets::set_venue_setting_in(
        fx.dir(),
        "polymarket",
        None,
        "PROXY_HOST",
        "new.invalid",
    )
    .expect("a machine-scoped write must land after the migration");
    assert_eq!(
        was.as_deref(),
        Some("proxy.example.invalid"),
        "the write must report the value it REPLACED — the migrated `'any'` row is the same row"
    );

    let hosts: Vec<_> = fx.rows().into_iter().filter(|r| r.3 == "PROXY_HOST").collect();
    assert_eq!(hosts.len(), 1, "the upsert DUPLICATED the machine-scoped row: {hosts:?}");
    assert_eq!(
        (hosts[0].0, hosts[0].2.as_deref(), hosts[0].4.as_str()),
        (1, Some("any"), "new.invalid"),
        "same id, stored as `'any'`, new value"
    );
    assert_eq!(
        fx.plain().get("POLY_PROXY_HOST").map(String::as_str),
        Some("new.invalid"),
        "…and the reader answers the new value under the legacy name"
    );
}

/// **`Some("any")` is REFUSED by the write boundary — it is not a second spelling of `None`.**
///
/// Before step 7 the stored `CHECK` (`tier IS NULL OR tier IN ('paper', 'demo', 'live')`) refused
/// the word, so a caller passing it got an error, which is what `set_venue_setting_in`'s `# Errors`
/// section still promised. Step 7 added `'any'` to the `CHECK`, and a write boundary that passed a
/// `Some("any")` straight through then filed it as the MACHINE-SCOPED row, overwriting it and
/// returning its old value as `previous` with nothing erroring (a reviewer found the contract and
/// the behaviour disagreeing on the branch that made the change). The map between the word and
/// "no tier" is only one place per direction if it is a bijection: `'any'` is reached from `None`
/// and from nothing else. No production caller can produce `Some("any")` today
/// (`parse_venue_setting_key` classifies by `account_tier_named`, which does not know the word),
/// so this is the contract a future caller meets, held where it cannot drift.
#[test]
fn the_stored_word_is_refused_as_a_tier_by_the_venue_setting_writer() {
    let fx = PreAnyTier::build();
    fx.write_an_unrelated_venue_setting().expect("migrate");
    let rows = fx.rows();

    let err = vike_secrets::set_venue_setting_in(
        fx.dir(),
        "polymarket",
        Some("any"),
        "PROXY_HOST",
        "aliased.invalid",
    )
    .expect_err("`Some(\"any\")` names no tier and must be REFUSED, not filed as the machine row");
    let text = err.to_string();
    assert!(
        text.contains("pass `None`"),
        "the refusal must say what the caller meant to pass, not surface a bare engine error: \
         {text}"
    );
    assert_eq!(fx.rows(), rows, "a refused write must change no row");
    assert_eq!(
        fx.plain().get("POLY_PROXY_HOST").map(String::as_str),
        Some("proxy.example.invalid"),
        "…and the machine-scoped row still answers its OWN value"
    );
}

/// **ONE total `UNIQUE` still keeps the two scopes apart** — the guarantee the pair of partial
/// indexes gave, now given by one constraint with no NULL deciding which half applies.
#[test]
fn the_total_unique_keeps_one_row_per_key_and_the_scopes_apart() {
    let fx = PreAnyTier::build();
    fx.write_an_unrelated_venue_setting().expect("migrate");

    let dup = fx
        .try_insert("polymarket", Some("any"), "PROXY_HOST")
        .expect_err("a second `'any'` row for one key must be REFUSED");
    assert!(dup.to_string().contains("UNIQUE"), "…by the total UNIQUE: {dup}");
    assert_eq!(
        fx.try_insert("polymarket", Some("live"), "PROXY_HOST").expect("a tier-scoped neighbour"),
        1,
        "a tier-scoped row for the same field is a DIFFERENT row, exactly as before"
    );
    let bad =
        fx.try_insert("polymarket", None, "PROXY_PORT").expect_err("NULL must be refused now");
    assert!(
        bad.to_string().contains("NOT NULL"),
        "a NULL tier is refused by the column itself: {bad}"
    );
}

// ---------------------------------------------------------------------------------------------
// Idempotence, rollback, and the impossible collision
// ---------------------------------------------------------------------------------------------

/// **IDEMPOTENT** — a second write must not rebuild again. A repair that re-ran on every write
/// would rewrite this table on every credential an operator adds, forever.
#[test]
fn the_migration_is_a_no_op_the_second_time() {
    // ⚠ This compared only the statement text, the rows and the indexes, and it PASSED in the red
    // lane run with no migration at all — a store nothing touches is trivially unchanged by a second
    // write, and a rebuild that reproduces itself leaves all three byte-identical too. So it now
    // asserts the first write DID rebuild (the page moved and `tier` is `NOT NULL`) before it asks
    // whether the second one did, and it asks by the page.
    let fx = PreAnyTier::build();
    let aged_page = fx.rootpage();
    fx.write_an_unrelated_venue_setting().expect("first");
    assert!(
        !fx.tier_is_nullable(),
        "the first write must have migrated, or this test says nothing"
    );
    let page = fx.rootpage();
    assert_ne!(page, aged_page, "the first write REBUILT the table, so its page moved");
    let (sql, rows, indexes) = (fx.table_sql(), fx.rows(), fx.indexes());

    fx.write_an_unrelated_venue_setting().expect("second");
    assert_eq!(fx.rootpage(), page, "the table must not be rebuilt a second time");
    assert_eq!(fx.table_sql(), sql);
    assert_eq!(fx.rows(), rows, "…and no row moved");
    assert_eq!(fx.indexes(), indexes);
}

/// **ROLLBACK, then forward.** An OLDER binary's own batch carries the two partial indexes as
/// `CREATE … IF NOT EXISTS`, and the rebuild freed their names — so the first write an older binary
/// makes to a migrated store PUTS THEM BACK (MEASURED on a copy of the CI box's store with v0.1.34; see
/// the spec's step-7 as-built block). They are harmless on a `NOT NULL` column, and they are also
/// exactly the shape ruling 2 refuses, so the next write by THIS binary must retire them again.
#[test]
fn a_legacy_index_an_older_binary_puts_back_is_retired_on_the_next_write() {
    let fx = PreAnyTier::build();
    fx.write_an_unrelated_venue_setting().expect("migrate");
    let rows = fx.rows();

    fx.conn().execute_batch(support::LEGACY_TIER_INDEXES).expect("an older binary's batch");
    assert!(
        fx.indexes().iter().any(|i| i == "venue_setting_one_per_machine"),
        "the premise: the older batch must really have put the index back: {:?}",
        fx.indexes()
    );

    fx.write_an_unrelated_venue_setting().expect("the next write by this binary");
    assert!(
        !fx.indexes().iter().any(|i| i.starts_with("venue_setting_one_per_")),
        "a legacy partial index survived the next write: {:?}",
        fx.indexes()
    );
    assert_eq!(fx.rows(), rows, "retiring the indexes moved no row");
    assert_every_row_answers(&fx.plain(), "after rollback and forward");
}

/// **A store that somehow holds rows the new `UNIQUE` cannot take is REFUSED BY NAME, and nothing
/// is committed.**
///
/// Unreachable on any store this code wrote — [`the_pre_step_7_store_is_the_shape_step_7_repairs`]
/// carries the proof's premise — so this plants the one thing that could break it: somebody DROPS
/// `venue_setting_one_per_machine` by hand and files a second machine-scoped value for one field.
/// Picking either would hand a bridge one of two values chosen by row order, so neither is picked:
/// the write fails naming the key, and the store is exactly as it was.
#[test]
fn colliding_rows_refuse_the_write_by_name_and_nothing_is_committed() {
    let fx = PreAnyTier::build();
    fx.conn()
        .execute_batch(
            "DROP INDEX venue_setting_one_per_machine;
             INSERT INTO venue_setting (venue, tier, field, value)
                 VALUES ('polymarket', NULL, 'PROXY_HOST', 'a-second-value');",
        )
        .expect("plant the collision");
    let (sql, rows) = (fx.table_sql(), fx.rows());
    let colliding: Vec<String> = rows
        .iter()
        .filter(|r| r.1 == "polymarket" && r.2.is_none() && r.3 == "PROXY_HOST")
        .map(|r| r.0.to_string())
        .collect();
    assert_eq!(colliding.len(), 2, "the premise: two machine-scoped PROXY_HOST rows: {rows:?}");

    let err = fx
        .write_an_unrelated_venue_setting()
        .expect_err("rows that collide under the new UNIQUE must refuse the write");
    let text = err.to_string();
    assert!(
        text.contains("polymarket") && text.contains("PROXY_HOST"),
        "the refusal must NAME the colliding key, not surface a bare engine UNIQUE error: {text}"
    );
    assert!(!text.contains("a-second-value"), "…and it names KEYS, never values: {text}");
    // ⚠ The three below were added after review. The refusal said *"delete all but one row per key
    // named here and write again"* while naming the key only in its STORED form
    // (`polymarket/any/PROXY_HOST`) — a word the operator never typed — and while no shipped verb
    // can delete a `venue_setting` row at all. Because the pass rides the write funnel, every
    // write to the store is refused until the repair is made, so the message is the whole of what
    // the operator has to act on.
    assert!(
        text.contains("POLY_PROXY_HOST"),
        "the refusal must name the key the way an operator knows it — the credential name the \
         row answers to: {text}"
    );
    assert!(
        text.contains(&format!("rows {}", colliding.join(", "))),
        "…and the colliding ROWS by id ({colliding:?}), since the repair is a delete by id: {text}"
    );
    assert!(
        text.contains("DELETE FROM venue_setting WHERE id"),
        "…and the repair itself, which no vike-cli verb performs: {text}"
    );

    assert_eq!(fx.table_sql(), sql, "nothing may be committed — the table is on its old shape");
    assert_eq!(fx.rows(), rows, "…and every row is where it was");
    assert!(fx.tier_is_nullable());
}

/// **A store BORN after step 7 is never rebuilt**, and its machine-scoped rows are born `'any'`.
#[test]
fn a_store_born_after_step_7_is_never_rebuilt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    std::fs::write(settings.join("secrets.env"), "OKX_DEMO_API_KEY=k\n").expect("store");
    let arg = Some(settings.to_str().expect("utf-8"));
    vike_secrets::migrate(arg, |_| false, &classify).expect("migration");

    let db = vike_secrets::db_path_in(&settings);
    // `(statement text, b-tree page)` — the page is what a rebuild cannot leave alone; see
    // `PreAnyTier::rootpage`.
    let shape = || -> (String, i64) {
        rusqlite::Connection::open(&db)
            .expect("open")
            .query_row(
                "SELECT sql, rootpage FROM sqlite_master \
                 WHERE type = 'table' AND name = 'venue_setting'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("table")
    };
    let born = shape();
    assert!(born.0.contains("'any'"), "a fresh store is born on the new shape: {}", born.0);

    vike_secrets::set_venue_setting_in(&settings, "polymarket", None, "PROXY_HOST", "h.invalid")
        .expect("a machine-scoped write on a fresh store");
    assert_eq!(shape(), born, "a store born on the new shape must not be rebuilt");
    let tier: String = rusqlite::Connection::open(&db)
        .expect("open")
        .query_row("SELECT tier FROM venue_setting WHERE field = 'PROXY_HOST'", [], |r| r.get(0))
        .expect("the row");
    assert_eq!(tier, "any", "a machine-scoped row is stored as `'any'`, never as NULL");
    assert_eq!(
        vike_secrets::load_workspace_dotenv_from(arg).get("POLY_PROXY_HOST").map(String::as_str),
        Some("h.invalid")
    );
}

// ---------------------------------------------------------------------------------------------
// Support
// ---------------------------------------------------------------------------------------------

/// A deliberately SIMPLER classifier than the production one, for the reason
/// `crates/vike-secrets/tests/database_migration.rs`'s own gives: this crate cannot link the crate
/// that owns the real tables, and nothing here is about how a name is classified.
fn classify(name: &str) -> Classification {
    if let Some(field) = name.strip_prefix("OKX_DEMO_") {
        return Classification {
            placement: Placement::Account(AccountKey {
                venue: "okx".to_string(),
                tier: "demo".to_string(),
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
