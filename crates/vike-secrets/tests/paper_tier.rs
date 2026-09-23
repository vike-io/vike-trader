//! **§4.4's `sim` -> `paper` rename, proved against a store that PREDATES it.**
//!
//! Ruling 7 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`: *"One word
//! for one idea — no real broker connection."* The rename itself is a constant and a `CHECK`. What
//! needs a test is the store already on disk, and the reason is measured rather than feared:
//!
//! `crates/vike-secrets/src/schema.rs`'s `DDL` is `CREATE TABLE IF NOT EXISTS`, so it changes
//! NOTHING about a table that is already there. An already-migrated store — which is what both
//! live boxes hold — keeps `CHECK (tier IN ('sim', 'demo', 'live'))` after the constant is
//! renamed, and SQLite has no `ALTER TABLE … DROP CONSTRAINT`. So a rename that stopped at the
//! constant would leave a table that **refuses the very word the classifier now produces**: the
//! next `{VENUE}_SIM_*` credential write fails with a bare `CHECK constraint failed` on a store
//! that worked the day before. `schema::migrate_sim_tier_to_paper` is the cure, and it rewrites
//! the ROWS and the CONSTRAINT together because they are the same migration.
//!
//! # How each test avoids proving nothing
//!
//! ⚠ The trap this whole programme keeps hitting is a check that can only ever pass. So:
//!
//!   * **the fixture's hostility is ASSERTED, not assumed.** [`the_pre_rename_store_refuses_paper`]
//!     shows the aged store REFUSING a `paper` row before anything migrates it. Every later
//!     assertion that a `paper` row now lands is therefore a statement about the migration and not
//!     about SQLite being permissive.
//!   * **the fixture is DERIVED from the shipped `DDL`**, by reverting exactly the one literal the
//!     rename changed — and [`aged_ddl`] asserts it reverted TWO of them. A fixture that quietly
//!     stopped being aged (because the DDL's spacing moved, say) fails there rather than passing
//!     everything downstream.
//!   * **the NEW state is what is asserted**, never the absence of the old. `'paper'` must be in
//!     the rebuilt constraint; a `paper` row must INSERT; the account row must READ `paper`.
//!
//! ⚠ **THE WRITER THIS FILE ORIGINALLY PROVED WAS NEVER THE ONE AT RISK, and that is worth
//! carrying because the green looked complete.** The paragraph above names the failure exactly —
//! *"the next `{VENUE}_SIM_*` credential write fails with a bare `CHECK constraint failed`"* — and
//! every test below then drove [`Aged::write_something_unrelated`], i.e.
//! `vike_secrets::set_venue_setting_in`, which had reached `crate::db::ensure_venue_id_columns`
//! since the day it was written. The CREDENTIAL writer —
//! `crates/vike-secrets/src/db.rs`'s `upsert_rows`, the one `vike-cli secrets set` goes through —
//! was the only writer in the crate that did NOT call the repair funnel, so the sentence predicting
//! the failure and the tests "proving" the cure were about two different code paths.
//! [`a_credential_write_carries_a_pre_rename_store_onto_paper`] is the one driven through the
//! writer that was actually in danger; found by the branch review of 2026-09-23, after this file
//! had been green for a day.
//!
//! ⚠ MEASURED on the CI box and the dev box, 2026-09-23: `_SIM_` credential keys = 0 and `tier = 'sim'`
//! rows = 0 on both. So nothing on either live box is migrated by this, and these tests are the
//! only place the path runs at all. That is the reason it has to be tested rather than a reason it
//! does not.
//!
//! # ⚠ Where the rename's OTHER two silent paths are tested, and why not here
//!
//! The rename has four measured silent paths and this file covers one and a half of them. The
//! `venue_setting` NAMING GRAMMAR halves — a `paper` row still rendering its legacy `_SIM_` key
//! name, and a dotted key an operator typed before the rename still addressing its row — live in
//! `crates/vike-secrets/src/venue_setting.rs`'s own test module. That is not filing: they were
//! written here first and `crates/vike-ops/tests/smoke_store_parity_gate.rs`'s
//! `the_renderer_has_no_fourth_caller` refused the file, because the renderer is pinned to exactly
//! three callers and *"the definition itself, and its own tests"* is one of them. The gate was
//! right and the tests moved rather than the pin growing a row. The fourth path — a parked
//! confirmation record's tier — is normalized at the JOIN, in `vike-cli`.

use std::path::{Path, PathBuf};

use vike_secrets::{AccountKey, Classification, Placement};

// ---------------------------------------------------------------------------------------------
// The fixture — a store as it stood before the rename
// ---------------------------------------------------------------------------------------------

/// The shipped `DDL` with the ONE literal §4.4 changed reverted — `tier IN ('paper', …)` back to
/// `tier IN ('sim', …)`, on `account` and on `venue_setting`.
///
/// ⚠ **Derived rather than transcribed**, for the reason `db::plant_schema_1` gives in its own
/// words: a test that plants its own guess proves the reader against a table nobody ever shipped.
/// And ⚠ **`venue_arming.mode` is deliberately untouched** — that column has spelled it `paper`
/// since it was written, which is the whole asymmetry ruling 7 removes, so a blanket replace over
/// the word would age a table that was never young.
fn aged_ddl() -> String {
    const NEW: &str = "tier IN ('paper', 'demo', 'live')";
    const OLD: &str = "tier IN ('sim', 'demo', 'live')";
    let ddl = vike_secrets::DDL;
    assert_eq!(
        ddl.matches(NEW).count(),
        2,
        "the shipped DDL must constrain `tier` in exactly two tables (`account`, `venue_setting`) \
         with the spelling this fixture reverts — if this fails the fixture is no longer aging \
         anything and every test below would pass vacuously"
    );
    assert!(
        ddl.contains("mode IN ('paper', 'demo', 'live')"),
        "`venue_arming.mode` must still be there and must NOT be reverted by the replace below"
    );
    let aged = ddl.replace(NEW, OLD);
    assert!(!aged.contains(NEW), "the revert must be total across both tables");
    assert!(
        aged.contains("mode IN ('paper', 'demo', 'live')"),
        "…and must not have touched `venue_arming.mode`"
    );
    aged
}

/// A settings directory holding a store at the PRE-RENAME shape: the aged DDL, one `sim` account
/// row with a credential hanging off it, and one `sim`-tier `venue_setting` row.
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
             INSERT INTO account (id, venue, tier, label) VALUES (1, 'binance', 'sim', NULL);
             INSERT INTO account (id, venue, tier, label) VALUES (2, 'binance', 'demo', NULL);
             INSERT INTO credential (id, account_id, field, value, name)
                 VALUES (10, 1, 'API_KEY', 'sim-value', 'BINANCE_SIM_API_KEY');
             INSERT INTO credential (id, account_id, field, value, name)
                 VALUES (11, 2, 'API_KEY', 'demo-value', 'BINANCE_DEMO_API_KEY');
             INSERT INTO venue_setting (id, venue, tier, field, value)
                 VALUES (20, 'ibkr', 'sim', 'BACKEND', 'cpapi');",
        )
        .expect("aged rows");
        conn.pragma_update(None, "user_version", 2i64).expect("stamp");
        drop(conn);

        Aged { _dir: dir, settings, db }
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    /// The `CREATE TABLE` text the engine is holding for `table`.
    fn table_sql(&self, table: &str) -> String {
        let conn = rusqlite::Connection::open(&self.db).expect("open");
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_else(|e| panic!("`{table}` must exist: {e}"))
    }

    /// `(id, tier)` for every `account` row, ordered.
    fn account_tiers(&self) -> Vec<(i64, String)> {
        let conn = rusqlite::Connection::open(&self.db).expect("open");
        let mut stmt = conn.prepare("SELECT id, tier FROM account ORDER BY id").expect("prepare");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .expect("query");
        rows.map(Result::unwrap).collect()
    }

    /// `(id, tier)` for every `venue_setting` row, ordered.
    fn venue_setting_tiers(&self) -> Vec<(i64, Option<String>)> {
        let conn = rusqlite::Connection::open(&self.db).expect("open");
        let mut stmt =
            conn.prepare("SELECT id, tier FROM venue_setting ORDER BY id").expect("prepare");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
            .expect("query");
        rows.map(Result::unwrap).collect()
    }

    /// Try to write one `account` row at the given tier, returning the engine's verdict.
    fn try_insert_account(&self, id: i64, tier: &str) -> rusqlite::Result<usize> {
        let conn = rusqlite::Connection::open(&self.db)?;
        conn.execute("INSERT INTO account (id, venue, tier) VALUES (?1, 'binance', ?2)", (id, tier))
    }

    /// Drive a PUBLIC writer, which is what reaches `db::ensure_venue_id_columns` and therefore the
    /// migration. Deliberately a writer that touches NEITHER renamed column, so nothing here can
    /// be read as the writer having done the work itself.
    fn write_something_unrelated(&self) {
        vike_secrets::set_venue_setting_in(
            self.dir(),
            "polymarket",
            None,
            "PROXY_HOST",
            "127.0.0.1",
        )
        .expect("the public writer must succeed");
    }

    /// `(id, tier, whether `venue_id` resolves to the `venue` row of the same name)` for the one
    /// `account` row naming `venue`, or `None` when there is none.
    ///
    /// ⚠ The third element is not padding: `crate::db::ensure_venue_id_columns` is ONE funnel doing
    /// the rename AND the `venue_id` backfill, so a test that asserted only the tier could not tell
    /// a funnel that ran from a rename somebody performed on its own.
    fn account_row(&self, venue: &str) -> Option<(i64, String, bool)> {
        let conn = rusqlite::Connection::open(&self.db).expect("open");
        conn.query_row(
            "SELECT a.id, a.tier, a.venue_id IS NOT NULL AND a.venue_id = \
             (SELECT v.id FROM venue v WHERE v.name = a.venue) \
             FROM account a WHERE a.venue = ?1",
            [venue],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?)),
        )
        .ok()
    }

    /// The engine's own whole-database referential check.
    fn foreign_key_violations(&self) -> i64 {
        let conn = rusqlite::Connection::open(&self.db).expect("open");
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
            .expect("fk check")
    }
}

// ---------------------------------------------------------------------------------------------
// The premise — this fixture really is hostile to the new word
// ---------------------------------------------------------------------------------------------

/// **Without this test every assertion below is vacuous**, which is the failure this plan's own
/// method rule was written after. It shows the aged store REFUSING `paper` — so a later `paper`
/// row landing is evidence the constraint was REPLACED, not evidence SQLite never cared.
#[test]
fn the_pre_rename_store_refuses_paper() {
    let fx = Aged::build();

    let sql = fx.table_sql("account");
    assert!(sql.contains("'sim'"), "the fixture must carry the OLD vocabulary: {sql}");
    assert!(!sql.contains("'paper'"), "…and must not already carry the new one: {sql}");

    let err = fx.try_insert_account(99, "paper").expect_err("a `paper` row must be REFUSED here");
    let text = err.to_string();
    assert!(
        text.to_lowercase().contains("constraint"),
        "the refusal must be the table's own CHECK, not some other failure: {text}"
    );

    assert_eq!(
        fx.account_tiers(),
        vec![(1, "sim".to_string()), (2, "demo".to_string())],
        "and the planted rows carry the old word"
    );
}

// ---------------------------------------------------------------------------------------------
// The migration
// ---------------------------------------------------------------------------------------------

/// **One ordinary write carries the whole store onto the new vocabulary** — the rows AND the
/// constraint, which is the half an `UPDATE` could never have delivered.
///
/// The writer used touches neither `account` nor `account.tier`, so the migration is shown running
/// as a REPAIR on the path every writer funnels through rather than as something a tier-aware verb
/// did on its way past.
#[test]
fn a_public_write_carries_a_pre_rename_store_onto_paper() {
    let fx = Aged::build();
    fx.write_something_unrelated();

    // The NEW state, asserted positively.
    let sql = fx.table_sql("account");
    assert!(
        sql.contains("'paper'"),
        "the rebuilt `account` must constrain `tier` against the new word: {sql}"
    );
    assert_eq!(
        fx.try_insert_account(99, "paper").expect("a `paper` row must now be ACCEPTED"),
        1,
        "…and the row the pre-rename store refused now lands"
    );

    // …and the rows carried, with their identities.
    assert_eq!(
        fx.account_tiers(),
        vec![(1, "paper".to_string()), (2, "demo".to_string()), (99, "paper".to_string())],
        "`sim` became `paper`, `demo` was left alone, and no id moved — `credential.account_id` \
         resolves by id and a renumbering would silently re-file every credential"
    );
    assert_eq!(
        fx.venue_setting_tiers(),
        vec![(20, Some("paper".to_string())), (21, None)],
        "the second table carries the same rename, and the writer's own machine-scoped row landed \
         beside it with a NULL tier — which §5.2 step 7 is what turns into `'any'`, not this task"
    );
    assert_eq!(fx.foreign_key_violations(), 0, "no credential lost the account it hangs off");
}

/// **THE WRITER THAT WAS ACTUALLY AT RISK — `vike-cli secrets set`, on a store that worked
/// yesterday.**
///
/// ⚠ **Every other test in this file drives [`Aged::write_something_unrelated`], and that writer was
/// never in danger.** `vike_secrets::set_venue_setting_in` has called
/// `crates/vike-secrets/src/db.rs`'s `ensure_venue_id_columns` since it was written;
/// `upsert_rows` — the one the CLI's credential verb reaches, through
/// `vike_secrets::save_credentials_to_store` — did not, and was the single writer in the crate that
/// opened its transaction and went straight to the INSERT. So this file's own module doc predicted
/// the failure and then proved the cure on a path that could not have met it. This test is the
/// pairing: the SAME pre-rename store, driven through the credential door.
///
/// MEASURED by the branch review before the cure: `OKX_SIM_API_KEY` reaches
/// `crate::schema::write_rows`, whose `AccountResolver` mints `(okx, paper)` — the word the
/// production classifier now produces — and the aged table's `CHECK (tier IN ('sim', …))` refuses
/// it. What the operator sees is a bare `CHECK constraint failed` naming nothing.
///
/// ⚠ **`OKX_`, not `BINANCE_`, and that is load-bearing.** `AccountResolver::resolve` answers from
/// its `by_prefix` map first, and the fixture's own `BINANCE_SIM_API_KEY` row puts `BINANCE_SIM_`
/// in it — so a binance key would be filed against the existing account with no INSERT at all, and
/// this test would pass against the unrepaired writer. The venue has to be one the store has never
/// held for the mint to happen.
///
/// The OTHER shape that reaches the same one-line cure — a store predating `venue_id`, where the
/// mint dies `table account has no column named venue_id` before the CHECK is ever consulted — is
/// not planted here: this file's fixture is the stage-2 shape both live boxes hold, and a
/// `venue_id`-less fixture would be a different store's test in a file about §4.4's rename.
#[test]
fn a_credential_write_carries_a_pre_rename_store_onto_paper() {
    let fx = Aged::build();

    // PREMISE, asserted rather than assumed — the same guard `the_pre_rename_store_refuses_paper`
    // states for the file, restated here so THIS test cannot pass vacuously if the fixture ever
    // stops being hostile.
    let refused = fx.try_insert_account(98, "paper").expect_err("the fixture must refuse `paper`");
    assert!(
        refused.to_string().to_lowercase().contains("constraint"),
        "the premise must be the table's own CHECK: {refused}"
    );
    assert!(fx.account_row("okx").is_none(), "…and the venue this write mints must be absent");

    // The credential door, with the production-shaped classifier: the exact call
    // `vike-cli secrets set` makes.
    let landed = vike_secrets::save_credentials_to_store(
        fx.dir(),
        vike_secrets::Table::Credential,
        &[("OKX_SIM_API_KEY".to_string(), "okx-value".to_string())],
        Some(&classify),
    )
    .expect(
        "a credential write must carry the store onto the new vocabulary rather than meeting the \
         OLD CHECK — this is the live-box defect, and an `Err` here is that defect",
    );
    assert!(
        matches!(landed, vike_secrets::Backend::Database(_)),
        "the write must have gone to the DATABASE — a file answer would mean this test never \
         reached `upsert_rows` at all"
    );

    // The row it minted, in the shape the funnel owes: the new word AND the id half.
    let (_, tier, venue_id_agrees) =
        fx.account_row("okx").expect("the credential must have minted its account row");
    assert_eq!(tier, "paper", "the minted row carries the word the classifier produces");
    assert!(venue_id_agrees, "…and `venue_id` resolves to the `venue` row of the same name");

    // The rest of the store came with it, and nothing was lost carrying it.
    assert_eq!(
        fx.account_tiers(),
        vec![(1, "paper".to_string()), (2, "demo".to_string()), (3, "paper".to_string())],
        "the pre-existing rows were renamed and kept their ids, and the mint is the only addition"
    );
    let after = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert_eq!(
        after.get("OKX_SIM_API_KEY").map(String::as_str),
        Some("okx-value"),
        "the key an operator just set must read back through the public reader"
    );
    assert_eq!(after.get("BINANCE_SIM_API_KEY").map(String::as_str), Some("sim-value"));
    assert_eq!(fx.foreign_key_violations(), 0);
}

/// **The credentials still read back through the PUBLIC reader** — the property that matters to an
/// operator, and the one a table rebuild is most likely to break.
///
/// ⚠ It reads through `load_workspace_dotenv_from` rather than a `SELECT`, because a rebuild that
/// dropped `credential`'s `superseded_at` predicate or re-filed a row against the wrong account
/// would still SELECT fine and would answer the reader with nothing.
#[test]
fn the_rebuild_does_not_lose_a_credential() {
    let fx = Aged::build();
    let before = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert_eq!(before.get("BINANCE_SIM_API_KEY").map(String::as_str), Some("sim-value"));

    fx.write_something_unrelated();

    let after = vike_secrets::load_workspace_dotenv_from(fx.arg());
    assert_eq!(
        after.get("BINANCE_SIM_API_KEY").map(String::as_str),
        Some("sim-value"),
        "the key NAME is the operator's and does NOT move with the tier — ruling 7 renames the \
         ACCOUNT TIER, never the credential key token"
    );
    assert_eq!(after.get("BINANCE_DEMO_API_KEY").map(String::as_str), Some("demo-value"));
}

/// **IDEMPOTENT** — a second write must not rebuild again, and must not disturb what the first one
/// produced. A repair that ran on every write would rewrite the `account` table on every
/// credential an operator adds, forever.
#[test]
fn the_migration_is_a_no_op_the_second_time() {
    let fx = Aged::build();
    fx.write_something_unrelated();
    let once = fx.table_sql("account");
    let tiers = fx.account_tiers();

    fx.write_something_unrelated();
    assert_eq!(fx.table_sql("account"), once, "the table must not be rebuilt a second time");
    assert_eq!(fx.account_tiers(), tiers, "…and no row moved");
    assert_eq!(fx.foreign_key_violations(), 0);
}

/// **A store BORN after the rename is untouched**, which is the other half of idempotence: the
/// migration must not fire on a table it has nothing to do.
#[test]
fn a_store_born_on_the_new_vocabulary_is_never_rebuilt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    std::fs::write(settings.join("secrets.env"), "BINANCE_SIM_API_KEY=k\n").expect("store");
    let arg = Some(settings.to_str().expect("utf-8"));
    vike_secrets::migrate(arg, |_| false, &classify).expect("migration");

    let db = vike_secrets::db_path_in(&settings);
    let sql_of = |table: &str| {
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get::<_, String>(0),
        )
        .expect("table")
    };
    let before = (sql_of("account"), sql_of("venue_setting"));
    assert!(before.0.contains("'paper'"), "a fresh store is born on the new vocabulary");

    vike_secrets::set_venue_setting_in(&settings, "polymarket", None, "PROXY_HOST", "127.0.0.1")
        .expect("write");
    assert_eq!(
        (sql_of("account"), sql_of("venue_setting")),
        before,
        "neither table may be rebuilt on a store that never held the old word"
    );

    // …and the key-name parser did what §4.4 says: the `SIM` TOKEN mints a `paper` TIER.
    let conn = rusqlite::Connection::open(&db).expect("open");
    let tier: String = conn
        .query_row("SELECT tier FROM account WHERE venue = 'binance'", [], |r| r.get(0))
        .expect("the credential minted an account row");
    assert_eq!(tier, "paper", "`BINANCE_SIM_API_KEY` names the `paper` tier");
}

// ---------------------------------------------------------------------------------------------
// Support
// ---------------------------------------------------------------------------------------------

/// A deliberately SIMPLER classifier than the production one, for the reason
/// `crates/vike-secrets/tests/database_migration.rs`'s own gives: this crate cannot link the crate
/// that owns the real tables. ⚠ It reaches the tier through `vike_secrets::
/// account_tier_of_key_token`, which IS the production map — so the one fact this file is about is
/// not re-spelled here.
///
/// ⚠ **One `{VENUE}_SIM_{FIELD}` arm rather than one arm per venue.** It read `BINANCE_SIM_` alone
/// until [`a_credential_write_carries_a_pre_rename_store_onto_paper`] needed a venue the fixture
/// does NOT already hold — a second hard-coded prefix would have been the same rule written twice,
/// and the split answers identically for the key the other tests use.
fn classify(name: &str) -> Classification {
    if let Some((head, field)) = name.split_once("_SIM_") {
        return Classification {
            placement: Placement::Account(AccountKey {
                venue: head.to_ascii_lowercase(),
                tier: vike_secrets::account_tier_of_key_token("SIM"),
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
