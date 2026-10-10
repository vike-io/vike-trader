//! The gate over behaviour: every rebuild this crate performs carries the mark, measured through the
//! verbs an operator runs.

use std::collections::BTreeSet;

use super::armed_tables;
use super::engine_seam::{
    MarkCarry, create, index_exists, mark, max_id, nothing_references_venue,
    rebuild_preserving_ids, table_exists,
};
use crate::support::Fixture;
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
    // ⚠ A store as its creation left it, and nothing after: every later write re-runs the roster's
    // `INSERT OR IGNORE`, and each ignored row still advances `venue`'s mark (MEASURED 2026-10-09:
    // 28 against a `max(id)` of 14 after one credential write) — so `mark == max(id)` holds only on
    // a store no write has touched since `secrets init` made it.
    let fx = Fixture::initialised();
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
    let fx = Fixture::seeded();

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
        // A store as its creation left it — see the first test in this file for why a written one
        // has its `venue` mark above `max(id)` before anything here runs.
        let fx = Fixture::initialised();
        let conn = fx.conn();

        let top = max_id(&conn, "venue").expect("the creation seeded the roster");
        assert_eq!(mark(&conn, "venue"), Some(top), "the precondition: the mark is at the top row");
        // ⚠ The venue at the TOP of the roster must have nothing pointing at it: a `venue_id`
        // referencing the row would make this a foreign-key question rather than a sequence one,
        // and the delete below would be refused instead of measured. A freshly created store holds
        // no row that names a venue — asserted rather than left as a comment, so a creation that
        // starts filing rows fails HERE, loudly and by name, instead of quietly testing the FK
        // refusal path under this test's name.
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
