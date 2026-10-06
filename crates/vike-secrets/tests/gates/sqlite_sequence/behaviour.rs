//! The gate over behaviour: every rebuild this crate performs carries the mark, measured through the
//! verbs an operator runs.

use std::collections::{BTreeMap, BTreeSet};

use super::armed_tables;
use super::create_statement;
use super::engine_seam::{
    MarkCarry, create, index_exists, mark, max_id, nothing_references_venue,
    rebuild_preserving_ids, set_mark, table_exists,
};
use crate::support;
use crate::support::Fixture;
use crate::support::sql::{columns, foreign_key_violations, table_sql, user_version};
use rusqlite::Connection;
use vike_secrets::{AccountEdit, DDL};

// -------------------------------------------------------------------------------------------
// The gate — the behaviour
// -------------------------------------------------------------------------------------------

/// **The fact §4.1 says nobody here knows**, measured against a store built entirely by this
/// crate's own code: `sqlite_sequence` exists, and it carries a row for every table the shipped
/// schema arms.
///
/// It is also the precondition for everything below it. A store with no `sqlite_sequence` table at
/// all would make every mark comparison in this file compare `None` with `None` and pass.
#[test]
fn a_real_store_carries_a_sqlite_sequence_mark_for_every_armed_table() {
    let fx = Fixture::migrated();
    let conn = fx.conn();

    // ⚠ An armed table this fixture left EMPTY is SKIPPED rather than failed. When stage 4 arms
    // `account`, a store may legitimately hold no account row, and an empty table's mark says
    // nothing about the engine — failing there would redden stage 4 for a fixture's reason rather
    // than a real one. The counter below is what keeps the skip from emptying the test out.
    let armed = armed_tables(DDL);
    let mut checked = 0usize;
    for table in &armed {
        let Some(rows) = max_id(&conn, table) else { continue };
        assert_eq!(
            mark(&conn, table),
            Some(rows),
            "`{table}` is armed and holds rows, so SQLite must be keeping a `sqlite_sequence` \
             high-water mark for it at `max(id)` — if this is None the engine kept no mark and \
             every rebuild assertion in this file is measuring nothing"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no armed table in this store holds a row, so nothing above was compared — the fixture \
         stopped reaching the engine and every mark assertion in this file is now vacuous"
    );
}

/// **§7 item 4, literally: remove the top account, rebuild the table, and see what the next id
/// is.**
///
/// The expectation is DERIVED from the shipped `DDL` rather than written down, which is what lets
/// this land green today and bind for real the day stage 4 arms the column — see this module's
/// table. The rebuild here CARRIES the mark (the correct procedure), so a red means the schema arms
/// `account` and the store handed the freed id back anyway: either the rebuild lost the mark or a
/// writer is assigning ids itself.
#[test]
fn section_7_item_4_remove_the_top_account_rebuild_and_create_again() {
    let fx = Fixture::migrated();

    let first = create(&fx, "binance", "live", "ONE");
    let second = create(&fx, "binance", "live", "TWO");
    let top = create(&fx, "binance", "live", "THREE");
    assert!(first < second && second < top, "the fixture needs the third row to hold the top id");

    // "Remove the top account" — permitted because this row owns no credential. See `create`.
    fx.edit(AccountEdit::Remove { id: top }).expect("a keyless row removes");

    let conn = fx.conn();
    assert_eq!(max_id(&conn, "account"), Some(second), "the top id is now free");
    rebuild_preserving_ids(&conn, "account", MarkCarry::Carried);

    // Anti-vacuity: the rebuild must actually have happened, and must actually have PRESERVED the
    // ids. A rebuild that silently did nothing, or that renumbered the survivors, would make the
    // assertion below a statement about something else entirely.
    assert!(
        !table_exists(&conn, "account_rebuild_scratch"),
        "the scratch table survived — the rebuild did not finish"
    );
    let survivors: BTreeSet<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM account").expect("prepare");
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
        rows.map(|r| r.expect("row")).collect()
    };
    assert!(
        survivors.contains(&first) && survivors.contains(&second) && !survivors.contains(&top),
        "the rebuild must replay the surviving rows with their ids and resurrect none: {survivors:?}"
    );
    assert!(
        index_exists(&conn, "account_one_account_per_book"),
        "the rebuilt table lost the unique index the rename carried off with the scratch table — \
         see `rebuild_preserving_ids`' second `DDL` pass, and the trap it is there for"
    );
    drop(conn);

    // A DIFFERENT account, at a different venue, with a different label — nothing about it says it
    // should inherit the removed row's number.
    let reborn = create(&fx, "okx", "demo", "FOUR");

    if armed_tables(DDL).contains("account") {
        assert_ne!(
            reborn, top,
            "`account` declares `AUTOINCREMENT`, so §7 item 4's assertion is live: the id freed \
             by the removal must NOT come back. It did. Either the rebuild replayed the surviving \
             rows without carrying `sqlite_sequence` across (spec §4.1's hazard — `MarkCarry::\
             Dropped` in this file is that mistake, spelled out), or a writer is computing the id \
             itself instead of letting the engine assign it."
        );
        assert!(reborn > top, "an armed table hands out ids strictly above its high-water mark");
    } else {
        assert_eq!(
            reborn, top,
            "THE DEBT, measured: `account` carries no `AUTOINCREMENT`, so SQLite hands a new row \
             `max(rowid) + 1` and the id the removed account held comes straight back out — spec \
             §2.4. If this assertion is the one that failed, the schema changed WITHOUT \
             `AUTOINCREMENT` appearing in `account`'s body, and \
             `every_pinned_verdict_matches_the_schema` is not going to tell you about it."
        );
    }
}

/// **§4.1's hazard, run on the one table the shipped schema arms today.**
///
/// A rebuild that replays the surviving rows with their ids sets the mark to `max(id)` of what it
/// replayed. Remove the top row first and that is BELOW the old mark, so the next insert is handed
/// a number that has already been used. Carrying the mark across is the whole cure, and it is one
/// statement.
///
/// ⚠ This measures the ENGINE, and it is pinned rather than assumed because every claim in this
/// file rests on a dropped table taking its `sqlite_sequence` row with it, and nothing else in this
/// workspace states that anywhere.
#[test]
fn a_rebuild_that_does_not_carry_the_mark_rewinds_it() {
    assert!(
        armed_tables(DDL).contains("venue"),
        "this test runs on `venue` because it is the table the shipped schema arms; if it no \
         longer is, `SEQUENCE_PIN` is where that is decided and this test needs a new subject"
    );

    for (carry, expectation) in [(MarkCarry::Dropped, "rewinds"), (MarkCarry::Carried, "survives")]
    {
        let fx = Fixture::migrated();
        let conn = fx.conn();

        let top = max_id(&conn, "venue").expect("the migration seeded the roster");
        assert_eq!(mark(&conn, "venue"), Some(top), "the precondition: the mark is at the top row");
        // ⚠ The venue at the TOP of the roster must have nothing pointing at it: a `venue_id`
        // referencing the row would make this a foreign-key question rather than a sequence one,
        // and the delete below would be refused instead of measured. This holds only because
        // `vike_model::VENUES`' tail is `hyperliquid` while `support::FIXTURE_KEYS` names
        // binance/dukascopy/cloudflare — asserted rather than left as a comment, so a roster
        // reorder fails HERE, loudly and by name, instead of quietly testing the FK refusal path
        // under this test's name.
        assert!(
            nothing_references_venue(&conn, top),
            "the venue at the top of the roster (id {top}) is referenced by a `venue_id` column — \
             this test no longer measures the sequence hazard it claims to; the roster changed \
             under it and this fixture needs a genuinely-unreferenced venue instead"
        );
        conn.execute("DELETE FROM venue WHERE id = ?1", [top]).expect("remove the top row");
        assert_eq!(mark(&conn, "venue"), Some(top), "a DELETE alone does not move the mark");

        rebuild_preserving_ids(&conn, "venue", carry);

        let after = mark(&conn, "venue");
        match carry {
            MarkCarry::Dropped => assert_eq!(
                after,
                Some(top - 1),
                "spec §4.1: a rebuild that replays the survivors and does nothing else must be \
                 seen to REWIND the mark to `max(id)`. If it did not, this engine no longer \
                 deletes a dropped table's `sqlite_sequence` row and the whole hazard this gate \
                 exists for has changed shape"
            ),
            MarkCarry::Carried => assert_eq!(
                after,
                Some(top),
                "carrying the mark across is the cure, and it did not take"
            ),
        }

        // …and what that costs, at the only place it is visible: the next row's id.
        conn.execute("INSERT INTO venue (name) VALUES ('a-venue-this-roster-does-not-name')", [])
            .expect("insert");
        let next = max_id(&conn, "venue").expect("a row");
        match carry {
            MarkCarry::Dropped => assert_eq!(
                next, top,
                "the freed id came back out — every `venue_id` written down against the old venue \
                 {top} now names a different one ({expectation})"
            ),
            MarkCarry::Carried => assert_eq!(
                next,
                top + 1,
                "the mark {expectation}, so the freed id must not be handed out again"
            ),
        }
    }
}

/// **The REAL reshape, driven through the call a binary makes, over a store that already carries an
/// armed table with a mark.**
///
/// `vike_secrets::migrate` is `crate::schema::reshape_into`'s production entry point: `fill_into`
/// runs the reshape for any store below the current schema, which is what
/// `vike_secrets::plant_schema_1` puts on disk here. That is the whole of this file's location
/// ruling — the private function is unreachable from `tests/`, the REBUILD is not.
///
/// Today it is GREEN because the reshape renames, re-creates and drops `credential` alone and
/// leaves `venue` untouched. It binds the day that stops being true — a reshape whose `DDL` batch
/// or scratch drop reaches an armed table, which is exactly what stage 4's rebuild of `account`
/// will be.
#[test]
fn the_real_reshape_does_not_rewind_an_armed_tables_mark() {
    assert!(
        armed_tables(DDL).contains("venue"),
        "this test's subject is the table the shipped schema arms; `SEQUENCE_PIN` is where that is \
         decided"
    );
    let fx = Fixture::file_store();
    let creds: Vec<(String, String)> =
        support::FIXTURE_KEYS.iter().map(|k| ((*k).to_string(), support::fake_value(k))).collect();
    std::fs::create_dir_all(fx.db().parent().expect("a db dir")).expect("db dir");
    vike_secrets::plant_schema_1(&fx.db(), &creds, &[])
        .expect("plant the shape both boxes were in");

    // The armed table, planted from the shipped statement, with its top row FREED — the only state
    // a rewind is visible in. A schema-1 store has no `venue` table of its own; this is the state a
    // store that has already been carried once is in, which is the state every FUTURE reshape runs
    // over.
    let top = {
        let conn = fx.conn();
        conn.execute_batch(&create_statement(DDL, "venue")).expect("plant the armed table");
        for name in ["a-venue", "b-venue", "c-venue"] {
            conn.execute("INSERT INTO venue (name) VALUES (?1)", [name]).expect("seed");
        }
        let top = max_id(&conn, "venue").expect("the fixture seeded rows");
        conn.execute("DELETE FROM venue WHERE id = ?1", [top]).expect("free the top id");
        assert_eq!(
            mark(&conn, "venue"),
            Some(top),
            "the precondition this test is worthless without: an armed table whose mark is ABOVE \
             its top surviving row"
        );
        top
    };

    vike_secrets::migrate(fx.arg(), support::is_node_key, &support::classify)
        .expect("the real reshape, through the entry point a binary calls");

    let conn = fx.conn();
    // Anti-vacuity: the reshape must actually have RUN. A migration that refused, or that decided
    // there was nothing to do, would leave the mark untouched for the wrong reason entirely.
    let version = user_version(&conn);
    assert_eq!(version, vike_secrets::SCHEMA_VERSION, "the store did not reach the new schema");
    assert!(
        !table_exists(&conn, "credential_schema1"),
        "the reshape's own scratch table survived, so the rebuild did not finish"
    );

    let after = mark(&conn, "venue").expect("an armed table with rows keeps a mark");
    assert!(
        after >= top,
        "a `sqlite_sequence` mark may only ever GROW, and the reshape moved `venue`'s DOWN (was \
         {top}, now {after}). Spec §4.1: a rebuild that replays surviving rows after the top one \
         was removed rewinds the mark and silently un-does the guarantee. Whatever the reshape now \
         does to `venue`, it must carry the mark across — `rebuild_preserving_ids` in this file is \
         the procedure."
    );
    let reused: i64 = conn
        .query_row("SELECT count(*) FROM venue WHERE id = ?1", [top], |r| r.get(0))
        .expect("count");
    assert_eq!(
        reused, 0,
        "the id freed before the migration was handed to one of the roster venues the migration \
         seeded — which is §7 item 4's failure, on a real production path"
    );
}

/// **Re-create `table` from a MUTATED copy of its own shipped statement**, keeping every row, every
/// id and (where the mutation leaves the table armed) its `sqlite_sequence` mark.
///
/// This is how the fixtures below plant the shapes a REAL store is in before a repair has run. The
/// statement is the SHIPPED one put through `mutate`, so a plant is this store's own schema wearing
/// one difference rather than a second spelling of it — the same discipline
/// `crate::schema::create_statement_under` follows in production. `select_expr` rewrites one
/// column's value in the copy, for the plants whose mutated constraint would refuse the rows as
/// they stand.
///
/// The mark is restored only when the mutated statement still declares `AUTOINCREMENT`: an unarmed
/// table has no mark to hold, and writing one into `sqlite_sequence` for it would be a fact the
/// engine ignores and a reader would believe.
fn replant(
    conn: &Connection,
    table: &str,
    mutate: &dyn Fn(String) -> String,
    select_expr: &dyn Fn(&str) -> Option<String>,
) {
    let scratch = format!("{table}_replant_scratch");
    let head = format!("CREATE TABLE IF NOT EXISTS {table} ");
    let statement = mutate(create_statement(DDL, table).replacen(
        &head,
        &format!("CREATE TABLE {scratch} "),
        1,
    ));
    let stays_armed = statement.to_uppercase().contains("AUTOINCREMENT");

    let before = mark(conn, table);
    // ⚠ The SAME pair as [`rebuild_preserving_ids`], and its doc is the authority for which one
    // does what — but this function is the OTHER shape (it builds the scratch BESIDE and renames it
    // INTO place), so the two halves earn their place differently here. `foreign_keys = OFF` is
    // what lets `DROP TABLE {table}` below run at all: this is a bare `Connection`, which comes up
    // with foreign keys ON in this build, and the drop performs an implicit `DELETE FROM` that
    // fires every child row. `legacy_alter_table = ON` has nothing to suppress — the only rename
    // here is `{scratch} -> {table}` and no clause in this schema names the scratch — and it is
    // kept so that this file spells a rebuild's pragmas ONE way rather than two.
    conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")
        .expect("a rebuild suspends the constraints it is about to break");
    conn.execute_batch(&statement).expect("plant the mutated table beside the live one");
    let cols = columns(conn, table);
    let selected: Vec<String> =
        cols.iter().map(|c| select_expr(c).unwrap_or_else(|| c.clone())).collect();
    conn.execute_batch(&format!(
        "INSERT INTO {scratch} ({}) SELECT {} FROM {table};",
        cols.join(", "),
        selected.join(", ")
    ))
    .expect("replay the rows with their ids");
    conn.execute_batch(&format!("DROP TABLE {table};")).expect("drop the live table");
    conn.execute_batch(&format!("ALTER TABLE {scratch} RENAME TO {table};")).expect("rename it in");
    conn.execute_batch(DDL).expect("re-create the indexes the drop took");
    match (stays_armed, before) {
        (true, Some(seq)) => set_mark(conn, table, seq),
        _ => {
            conn.execute("DELETE FROM sqlite_sequence WHERE name = ?1", [table]).expect("no mark");
        }
    }
    conn.execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")
        .expect("restore the constraints");
}

/// **Put `table` back onto the PRE-§4.4 tier vocabulary**, keeping its rows, its ids and its
/// `sqlite_sequence` mark — the state a store that has not been written since the rename is in,
/// and the ONE state `crate::schema::migrate_sim_tier_to_paper` fires on.
///
/// It keeps `AUTOINCREMENT`, which is the whole point: a fixture that planted an UNARMED table
/// would make the assertions that follow measure nothing.
fn plant_pre_paper_tier(conn: &Connection, table: &str) {
    replant(conn, table, &|sql| sql.replace("'paper'", "'sim'"), &|c| {
        (c == "tier").then(|| "CASE tier WHEN 'paper' THEN 'sim' ELSE tier END".to_string())
    });
    let sql = table_sql(conn, table);
    assert!(sql.contains("'sim'"), "the plant must produce the OLD vocabulary: {sql}");
    assert!(
        sql.to_uppercase().contains("AUTOINCREMENT"),
        "…and must keep the table ARMED — a mark is the whole subject here: {sql}"
    );
}

/// **Put `table` back into the PRE-§4.1 shape: no `AUTOINCREMENT`**, keeping its rows and ids.
///
/// This is the state BOTH LIVE BOXES are in, and the one a fresh store can never be in — every
/// store born from `DDL` is armed at birth, so a test that only ever sees a fresh store proves
/// nothing about the migration.
fn plant_unarmed(conn: &Connection, table: &str) {
    replant(
        conn,
        table,
        &|sql| sql.replace("INTEGER PRIMARY KEY AUTOINCREMENT", "INTEGER PRIMARY KEY"),
        &|_| None,
    );
    let sql = table_sql(conn, table);
    assert!(
        !sql.to_uppercase().contains("AUTOINCREMENT"),
        "the plant must actually have DISARMED the table, or the repair under test has nothing to \
         do and its assertions are vacuous: {sql}"
    );
    assert_eq!(mark(conn, table), None, "…and an unarmed table holds no mark");
}

/// **The behaviour assertion [`TABLE_DROP_PIN`]'s `schema.rs` row used to declare as OWED** — and
/// it is owed no longer, because stage 4 armed the very tables §4.4's rebuild touches.
///
/// `crate::schema::migrate_sim_tier_to_paper` reads `account`'s `sqlite_sequence` mark before the
/// rebuild and puts it back afterwards. While no table declared `AUTOINCREMENT` that read answered
/// `None` and the carry was a NO-OP: no test could fail for its absence, which the pin row said in
/// its own words. Now it can, and this is the test that does — driven through
/// `AccountEdit::Create`, the verb an operator runs, so what is measured is the production repair
/// path rather than a model of it.
///
/// Delete the two `sqlite_sequence` statements at the end of
/// `crate::schema::rebuild_table_from_ddl` and this goes red naming a REUSED id.
#[test]
fn the_paper_tier_rebuild_does_not_rewind_the_account_marks() {
    assert!(
        armed_tables(DDL).contains("account"),
        "this test's whole subject is an ARMED `account`; `SEQUENCE_PIN` is where that is decided, \
         and while the table was `Owed` there was no mark for a rebuild to rewind"
    );

    let fx = Fixture::migrated();
    let first = create(&fx, "binance", "live", "ONE");
    let second = create(&fx, "binance", "live", "TWO");
    let top = create(&fx, "binance", "live", "THREE");
    assert!(first < second && second < top, "the fixture needs the third row to hold the top id");
    fx.edit(AccountEdit::Remove { id: top }).expect("a keyless row removes");

    {
        let conn = fx.conn();
        assert_eq!(max_id(&conn, "account"), Some(second), "the top id is now free");
        assert_eq!(
            mark(&conn, "account"),
            Some(top),
            "…and the mark is still ABOVE the top surviving row, which is the ONLY state a rewind \
             is visible in"
        );

        plant_pre_paper_tier(&conn, "account");

        // The preconditions, asserted AFTER the plant rather than assumed to have survived it.
        let sql = table_sql(&conn, "account");
        assert!(
            sql.contains("'sim'"),
            "the plant must leave a table `migrate_sim_tier_to_paper` will FIRE on: {sql}"
        );
        assert_eq!(
            mark(&conn, "account"),
            Some(top),
            "the plant must keep the mark above the top surviving row"
        );
        assert_eq!(
            max_id(&conn, "account"),
            Some(second),
            "…and must not have resurrected the removed row"
        );
    }

    // THE REAL REPAIR PATH: `edit_account` runs `ensure_venue_id_columns`, which is where
    // `migrate_sim_tier_to_paper` lives, before it inserts anything.
    let reborn = create(&fx, "okx", "demo", "FOUR");

    let conn = fx.conn();
    let sql = table_sql(&conn, "account");
    assert!(
        sql.contains("'paper'") && !sql.contains("'sim'"),
        "anti-vacuity: the rebuild must actually have RUN. If it did not, nothing below is a \
         statement about a rebuild at all — it is a statement about an untouched table: {sql}"
    );
    assert_eq!(
        reborn,
        top + 1,
        "§4.1: the rebuild replayed the SURVIVING rows with their ids, which sets the mark to \
         `max(id)` = {second} — a REWIND, because the row holding {top} had been removed first. \
         Carrying the old mark across is the whole cure and it is two statements at the end of \
         `crate::schema::rebuild_table_from_ddl`. Without them this account is handed {top}, the \
         number the removed one held, and every note, runbook and wire client that remembered it \
         now names a different account"
    );
    assert_ne!(
        reborn, top,
        "the id freed before the rebuild came back out — see the assertion above for the cure"
    );
    assert_eq!(
        mark(&conn, "account"),
        Some(top + 1),
        "…and the mark moved UP by exactly the row that was inserted, never down"
    );
}

/// **§4.1 reaches an EXISTING store, which is the only claim that is about the live boxes.**
///
/// ⚠ `crate::schema::DDL` is `CREATE TABLE IF NOT EXISTS` throughout, so the batch applies NOTHING
/// to a table that is already there: every store born after stage 4 is armed at birth, and a test
/// that creates a fresh store proves nothing whatever about the CI box or the dev box. Both of those
/// hold an `account` table written before §4.1, and the REBUILD is what arms it.
///
/// So this plants that shape — the shipped table with `AUTOINCREMENT` taken back out, rows and ids
/// intact — and then does the two things an operator does, in order: remove an account, create
/// another. It asserts all three properties the migration owes:
///
/// * the repair RAN (the stored statement declares `AUTOINCREMENT` afterwards);
/// * every surviving id is UNCHANGED, because `credential.account_id` and an operator's own notes
///   both name rows by that number;
/// * and the id freed by the removal is not handed out again — §2.4's defect, closed.
#[test]
fn an_existing_unarmed_account_table_is_armed_by_the_next_write_and_keeps_its_ids() {
    let fx = Fixture::migrated();
    let first = create(&fx, "binance", "live", "ONE");
    let second = create(&fx, "binance", "live", "TWO");
    let top = create(&fx, "binance", "live", "THREE");
    assert!(first < second && second < top, "the fixture needs the third row to hold the top id");

    {
        let conn = fx.conn();
        plant_unarmed(&conn, "account");
        assert_eq!(
            max_id(&conn, "account"),
            Some(top),
            "the plant must keep the rows it found — it is a disarming, not a truncation"
        );
    }

    // The first write an operator performs on such a store. The repair runs inside it, BEFORE the
    // removal, which is why the freed id is already protected by the time the row goes.
    fx.edit(AccountEdit::Remove { id: top }).expect("a keyless row removes");

    let conn = fx.conn();
    let sql = table_sql(&conn, "account");
    assert!(
        sql.to_uppercase().contains("AUTOINCREMENT"),
        "the repair must have run on the EXISTING table. `DDL` alone cannot do this — \
         `CREATE TABLE IF NOT EXISTS` changes nothing about a table that is already there, which \
         is exactly the state both live boxes are in: {sql}"
    );
    let survivors: BTreeSet<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM account").expect("prepare");
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
        rows.map(|r| r.expect("row")).collect()
    };
    assert!(
        survivors.contains(&first) && survivors.contains(&second) && !survivors.contains(&top),
        "the arming rebuild must replay every surviving row with its OWN id — a renumbering here \
         would re-point `credential.account_id` and every book an operator wrote down: {survivors:?}"
    );
    assert_eq!(
        mark(&conn, "account"),
        Some(top),
        "…and the mark it was given is `max(id)` of what it copied, which on THIS migration is \
         correct precisely because no row had been removed when it ran (spec §4.1)"
    );
    drop(conn);

    let reborn = create(&fx, "okx", "demo", "FOUR");
    assert_eq!(
        reborn,
        top + 1,
        "§2.4, closed: the id the removed account held must not come back. Before stage 4 this \
         was {top} — `remove account 16, add an account, and the new one IS account 16`"
    );
}

/// Every armed table's id set, for the before/after comparison
/// [`every_armed_table_planted_unarmed_at_once_is_repaired_by_one_write`] rests on.
fn ids_by_table(conn: &Connection, tables: &BTreeSet<String>) -> BTreeMap<String, BTreeSet<i64>> {
    tables
        .iter()
        .map(|table| {
            let mut stmt =
                conn.prepare(&format!("SELECT id FROM {table}")).expect("every armed table has id");
            let ids: BTreeSet<i64> = stmt
                .query_map([], |r| r.get::<_, i64>(0))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect();
            (table.clone(), ids)
        })
        .collect()
}

/// **EVERY armed table planted unarmed AT ONCE — the shape both live boxes are actually in.**
///
/// The tests above disarm ONE table. A store written before §4.1 has none of them armed, and
/// `crate::schema::migrate_tables_onto_autoincrement` rebuilds them in a LOOP, inside ONE
/// transaction, over tables that reference each other (`account.venue_id`, `credential.account_id`,
/// `credential.venue_id`, `venue_setting.venue_id`). The failures that needs are the ones a
/// single-table plant cannot produce: a rebuild dropping a parent another rebuild is part-way
/// through, a `REFERENCES` clause left naming a scratch table, an id renumbered because the loop
/// reached the child first. So this asks the whole-database question at the end —
/// `pragma_foreign_key_check` over everything, not over the statements just run.
///
/// The roster is DERIVED from the shipped `DDL` ([`armed_tables`]), never listed here, so a table
/// armed next month joins this test by existing rather than by somebody remembering.
///
/// ⚠ The rows in the tables the fixture leaves empty are planted DELIBERATELY. Without them
/// "every id survived" is a claim about the three tables `Fixture::migrated` happens to fill and a
/// vacuous truth about the rest — so the anti-vacuity loop below refuses an empty one BY NAME.
#[test]
fn every_armed_table_planted_unarmed_at_once_is_repaired_by_one_write() {
    let fx = Fixture::migrated();
    let first = create(&fx, "binance", "live", "PLANTONE");
    let second = create(&fx, "okx", "demo", "PLANTTWO");
    assert!(first < second, "the fixture needs two accounts with distinct ids");

    let armed = armed_tables(DDL);
    assert!(
        armed.len() > 1,
        "this test is about SEVERAL tables being repaired in one pass; the shipped schema arms \
         {} — if that is ever one, `SEQUENCE_PIN` is where it was decided",
        armed.len()
    );

    {
        let conn = fx.conn();
        conn.execute_batch(
            "INSERT INTO node_key (name, value) VALUES ('vike_node_unarmed_probe', 'v');
             INSERT INTO setting (section, key, value) VALUES ('flags', 'unarmed_probe', '1');
             INSERT INTO profile_risk (profile, key, value) VALUES ('probe', 'max_qty', '2');
             INSERT INTO venue_setting (venue_id, tier, field, value)
                 SELECT id, 'demo', 'unarmed_probe', 'v'
                 FROM venue WHERE name = 'binance';",
        )
        .expect("seed the armed tables the migration leaves empty");
    }

    let before = ids_by_table(&fx.conn(), &armed);
    for (table, ids) in &before {
        assert!(
            !ids.is_empty(),
            "`{table}` is armed and EMPTY, so the id-preservation assertion below says nothing \
             about it. Give this fixture a row in it rather than letting the table drop quietly \
             out of what this test covers"
        );
    }

    {
        let conn = fx.conn();
        for table in &armed {
            plant_unarmed(&conn, table);
        }
        // The premise, asserted rather than assumed: a plant that silently left a table armed
        // would make every assertion below a statement about a store that needed no repair.
        for table in &armed {
            let sql = table_sql(&conn, table);
            assert!(
                !sql.to_uppercase().contains("AUTOINCREMENT"),
                "`{table}` survived the plant still armed: {sql}"
            );
        }
    }

    // ONE production write, through the verb an operator runs. The repair rides inside it.
    let reborn = create(&fx, "bybit", "demo", "PLANTTHREE");

    let conn = fx.conn();
    for table in &armed {
        let sql = table_sql(&conn, table);
        assert!(
            sql.to_uppercase().contains("AUTOINCREMENT"),
            "`{table}` was left UNARMED by the repair. One write must arm every table the shipped \
             `DDL` arms, not the first one the loop reaches — a half-repaired store hands the next \
             operator a reused id in whichever table was missed: {sql}"
        );
    }

    let after = ids_by_table(&conn, &armed);
    for (table, ids) in &before {
        let now = after.get(table).expect("the same roster on both sides");
        let lost: Vec<i64> = ids.difference(now).copied().collect();
        assert!(
            lost.is_empty(),
            "the rebuild of `{table}` did not replay {lost:?} with their own ids. Every one of \
             these numbers is an ADDRESS — `credential.account_id`, `venue_setting.venue_id` and \
             an operator's own notes all name rows by it"
        );
    }
    assert!(after["account"].contains(&reborn), "the new account is in the rebuilt table");
    assert_eq!(
        after["account"].len(),
        before["account"].len() + 1,
        "…and it is the ONLY row the write added: a rebuild that resurrected a row would pass the \
         loop above, which only asks that nothing was lost"
    );

    let violations = foreign_key_violations(&conn);
    assert_eq!(
        violations, 0,
        "repairing every armed table in one transaction left dangling references. This is the \
         question a single-table plant cannot ask: each rebuild renames a scratch table into \
         place, and a clause rewritten to follow one of those renames survives the scratch's own \
         DROP pointing at nothing"
    );
}
