//! The second release - the text column goes, and the aged shapes still answer.

use super::*;
use crate::support;

// ---------------------------------------------------------------------------------------------
// The second release: the text column goes from `account`, `credential` and `venue_setting`
// ---------------------------------------------------------------------------------------------

/// After the second release's first write, `account`, `credential` and `venue_setting` carry no
/// text venue at all, and every reader still answers.
///
/// ⚠ **On a store BORN contracted** — [`planted`] migrates through this binary, whose batch has no
/// text column — so this asks the shipped batch and the readers, and contracts nothing.
/// [`a_first_release_store_is_contracted_by_its_next_write_and_keeps_every_row`] is the test of the
/// contraction itself, on the shape both live boxes hold.
#[test]
fn the_contracting_write_removes_the_text_column_and_readers_still_answer() {
    let fx = planted();
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("the contracting write");
    let conn = fx.conn();
    for table in ["account", "credential", "venue_setting"] {
        assert!(!has_text_venue(&conn, table), "`{table}` must have lost its text venue column");
    }
    let mut venues: Vec<String> = fx.accounts().into_iter().map(|a| a.venue).collect();
    venues.sort();
    assert_eq!(venues, ["binance", "binance", "dukascopy"]);
}

/// `(id, account_id, venue_id)` of every `credential` row, by id — the identity and the links a
/// contraction's rebuild of that table may not move.
fn credential_links(conn: &rusqlite::Connection) -> Vec<(i64, Option<i64>, Option<i64>)> {
    let mut stmt =
        conn.prepare("SELECT id, account_id, venue_id FROM credential ORDER BY id").expect("prep");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect()
}

/// **The contraction of an EXISTING store — the production path on both live boxes.** The store is
/// planted on `vike_secrets::venue_links::RELEASE_1_DDL`, the shape the plan's first release carried
/// both boxes onto (a CARRIED store: links by `venue_id`, `NOT NULL`, with the text `venue` still
/// beside them), with rows in every linked table: `account` and `credential` each with their TOP row
/// deleted, so a rebuild that replayed the survivors without carrying the `AUTOINCREMENT` mark would
/// rewind it; an account-scoped, a venue-scoped and an infrastructure `credential` row; a
/// `venue_setting` and a `venue_arming` row. One ordinary write must then take the text column off
/// the three tables (and leave `venue_arming`'s), move no id, number or mark, leave no dangling
/// reference, keep every uniqueness, and leave every reader answering.
///
/// ⚠ **It is also the BEHAVIOURAL guard for `credential`'s scope check** (`CHECK (account_id IS NULL
/// OR venue_id IS NULL)`), asked of the table the contraction REBUILT. The DDL gate cannot hold it:
/// its drift key is `credential.check:account_id,venue_id` on both sides (§3's exactly-one check
/// names the same two columns), so deleting the DDL's check changes no key and the gate stays green.
/// So the rebuilt table is asked directly: it must REFUSE a row naming both an account and a venue,
/// and ADMIT each of the three kinds §5.1 allows.
#[test]
fn a_first_release_store_is_contracted_by_its_next_write_and_keeps_every_row() {
    let (fx, conn) = plant(vike_secrets::venue_links::RELEASE_1_DDL);
    conn.execute_batch(
        "INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (1, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'demo');
         INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (2, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'live');
         INSERT INTO account (id, venue, venue_id, tier, label) \
             VALUES (3, 'okx', (SELECT id FROM venue WHERE name = 'okx'), 'demo', 'GONE');
         DELETE FROM account WHERE id = 3;
         INSERT INTO credential (id, account_id, venue, venue_id, field, value, name) \
             VALUES (1, 1, NULL, NULL, 'API_KEY', 'an-account-value', 'binance-demo-key');
         INSERT INTO credential (id, account_id, venue, venue_id, field, value, name) \
             VALUES (2, NULL, 'ctrader', (SELECT id FROM venue WHERE name = 'ctrader'), \
                     'CLIENT_ID', 'a-venue-value', 'ctrader-client-id');
         INSERT INTO credential (id, account_id, venue, venue_id, field, value, name) \
             VALUES (3, NULL, NULL, NULL, 'TOKEN', 'an-infra-value', 'an-infrastructure-key');
         INSERT INTO credential (id, account_id, venue, venue_id, field, value, name) \
             VALUES (4, NULL, NULL, NULL, 'TOKEN', 'a-removed-value', 'a-removed-key');
         DELETE FROM credential WHERE id = 4;
         INSERT INTO venue_setting (venue, venue_id, tier, field, value) \
             VALUES ('polymarket', (SELECT id FROM venue WHERE name = 'polymarket'), 'any', \
                     'PROXY_ENABLED', 'false');
         INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'demo');",
    )
    .expect("rows in every linked table");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    let before = vike_secrets::venue_links::link_snapshot(fx.dir());
    assert_eq!(
        before.text_venue,
        ["account", "credential", "venue_setting", "venue_arming"],
        "premise: every linked table still carries its text column"
    );
    assert!(before.nullable.is_empty(), "premise: the first release carried it: {before:?}");
    let mark = |s: &vike_secrets::venue_links::LinkSnapshot, table: &str| {
        s.marks.iter().find(|(t, _)| t == table).map(|(_, seq)| *seq)
    };
    assert_eq!(
        (mark(&before, "account"), mark(&before, "credential")),
        (Some(3), Some(4)),
        "premise: both marks sit above every surviving id"
    );
    let credentials_before = credential_links(&fx.conn());

    // The write: a no-op value on an existing row, through the venue-setting door.
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("the contracting write");

    let after = vike_secrets::venue_links::link_snapshot(fx.dir());
    assert_eq!(after.text_venue, ["venue_arming"], "the three text columns are gone, one stays");
    assert_eq!(after.rows, before.rows, "every row kept its id and its number");
    assert_eq!(
        (mark(&after, "account"), mark(&after, "credential")),
        (Some(3), Some(4)),
        "no mark was rewound: the removed rows' ids are never handed out again"
    );
    for (table, seq) in &before.marks {
        assert!(mark(&after, table) >= Some(*seq), "`{table}`'s mark went DOWN: {after:?}");
    }

    let conn = fx.conn();
    assert_eq!(credential_links(&conn), credentials_before, "no credential moved or re-filed");
    assert_eq!(foreign_key_violations(&conn), 0, "no reference dangles after the rebuilds");
    assert!(object_sql(&fx.conn(), "account").contains("UNIQUE (venue_id, tier, label)"));
    assert!(object_sql(&fx.conn(), "venue_setting").contains("UNIQUE (venue_id, tier, field)"));
    for (index, keyed) in [
        ("account_one_account_per_book", "(venue_id, venue_account_id)"),
        ("credential_one_live_value", "(account_id, field)"),
        ("credential_one_live_name", "(name)"),
        ("venue_arming_one_per_venue", "(venue_id) WHERE label IS NULL"),
        ("venue_arming_one_per_account", "(venue_id, label) WHERE label IS NOT NULL"),
    ] {
        assert!(
            object_sql(&fx.conn(), index).contains(keyed),
            "`{index}` must still key on {keyed}"
        );
    }

    // Every reader answers.
    let read: Vec<(i64, String)> = fx.accounts().into_iter().map(|a| (a.id, a.venue)).collect();
    assert_eq!(read, [(1, "binance".to_string()), (2, "binance".to_string())]);
    let rows = settings_rows(&fx);
    assert_eq!(rows.venue.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["polymarket"]);
    assert_eq!(rows.arming.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["binance"]);
    let creds = vike_secrets::load_workspace_dotenv_from(fx.arg());
    for (name, value) in [
        ("binance-demo-key", "an-account-value"),
        ("ctrader-client-id", "a-venue-value"),
        ("an-infrastructure-key", "an-infra-value"),
    ] {
        assert_eq!(creds.get(name).map(String::as_str), Some(value), "`{name}` still answers");
    }
    // …and the venue-scoped credential names its venue by number, the only place it is written now.
    let named: Vec<(String, String)> = conn
        .prepare(
            "SELECT c.name, v.name FROM credential c JOIN venue v ON v.id = c.venue_id ORDER BY c.id",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    assert_eq!(named, [("ctrader-client-id".to_string(), "ctrader".to_string())]);

    // The scope check, asked of the REBUILT table: never both, and each of the three kinds admitted.
    let insert = |account_id: Option<i64>, venue: Option<&str>, name: &str| {
        conn.execute(
            "INSERT INTO credential (account_id, venue_id, field, value, name) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?2), 'F', 'v', ?3)",
            (account_id, venue, name),
        )
    };
    let both = insert(Some(1), Some("binance"), "check-both").expect_err("both set must refuse");
    assert!(
        both.to_string().contains("CHECK constraint failed"),
        "the scope check refuses: {both}"
    );
    insert(Some(2), None, "check-account").expect("an account-scoped row is admitted");
    insert(None, Some("okx"), "check-venue").expect("a venue-scoped row is admitted");
    insert(None, None, "check-infrastructure").expect("an infrastructure row is admitted");
    assert!(!has_column(&conn, "credential", "venue"), "…on the contracted table");
}

/// **A venue-scoped key filed under a venue the roster does not hold is REFUSED by name, not filed as
/// infrastructure.** On a contracted store a venue-scoped `credential` row names its venue by number
/// alone, looked up by name in the INSERT itself, so a venue the `venue` table lacks would get a NULL
/// number and no text — a row naming no venue, which every reader takes for an infrastructure key.
/// Driven through the credential door a rotating grant persists through, where a refusal is an
/// error naming the key.
#[test]
fn a_venue_scoped_key_for_a_venue_off_the_roster_is_refused_by_name() {
    let fx = Fixture::migrated();
    let key = "an-off-roster-venue-key";
    let classify = |name: &str| -> vike_secrets::Classification {
        if name == key {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Venue("no-such-venue".to_string()),
                field: "API_KEY".to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            };
        }
        support::classify(name)
    };
    let err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        vike_secrets::Table::Credential,
        &[(key.to_string(), "an-off-roster-value".to_string())],
        Some(&classify),
    )
    .expect_err("a key naming no roster venue must not land");
    // The door returns the store's refusal as text (`std::io::Error`), so the refusal is asked of
    // its rendering: `SchemaRefusal::VenueNotOnRoster`'s, which names the key and the venue.
    let text = err.to_string();
    assert!(
        text.contains(&format!(
            "{key}: the classifier filed this key under venue \"no-such-venue\", which this store's \
             venue table does not hold"
        )),
        "refused by name, as the venue it was filed under: {text}"
    );
    assert!(text.contains("NOTHING WAS WRITTEN"), "…saying nothing landed: {text}");
    assert!(!text.contains("an-off-roster-value"), "…never by value: {text}");
    let conn = fx.conn();
    let filed: i64 = conn
        .query_row("SELECT COUNT(*) FROM credential WHERE name = ?1", [key], |r| r.get(0))
        .expect("count");
    assert_eq!(filed, 0, "…and nothing was filed, as infrastructure or otherwise");
}

/// **A text venue with no number refuses the contraction BY NAME, and nothing is committed.** On a
/// store the first release carried, `account`, `venue_setting` and `venue_arming` hold a number on
/// every row (`venue_id` is `NOT NULL` there), but `credential.venue_id` stays nullable, so a
/// venue-scoped credential row filed for a venue the roster does not hold keeps its text and a NULL
/// number. Dropping the text would erase the only venue that row names, and trap 7 never asks a
/// carried table, so the drop pass (`crate::schema`'s `migrate_dropped_columns`) refuses with trap
/// 7's own message before it rebuilds anything.
///
/// ⚠ **This test stood here as
/// `a_carried_table_rebuilt_again_does_not_refuse_for_rows_it_does_not_copy` until the second
/// release, asserting the OPPOSITE of the same row**: that an off-roster venue-scoped credential
/// row on a carried store refused nothing, because no rebuild then copied `credential` into a shape
/// without its text. The drop pass is that rebuild now. Planted on
/// `vike_secrets::venue_links::RELEASE_1_DDL`, the first release's shipped batch, because a store
/// this binary writes can no longer hold a text venue in `credential`.
#[test]
fn a_text_venue_with_no_number_refuses_the_contraction_by_name() {
    let (fx, conn) = plant(vike_secrets::venue_links::RELEASE_1_DDL);
    conn.execute_batch(
        "INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (1, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'demo');
         INSERT INTO credential (id, account_id, venue, venue_id, field, value, name) \
             VALUES (4, NULL, 'no-such-venue', NULL, 'API_KEY', 'stray', 'NO_SUCH_VENUE_API_KEY');",
    )
    .expect("a carried account, and an off-roster venue-scoped credential row");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    let err =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("the drop pass must refuse to erase the row's only venue");
    let text = err.to_string();
    assert!(
        matches!(err.kind, DbErrorKind::RepairRefused { .. }),
        "the store's repair refusing, as at every door: {text}"
    );
    assert!(
        text.contains("`credential` id 4 (venue 'no-such-venue')"),
        "the refusal names the table, the row and the venue string together: {text}"
    );
    assert!(!text.contains("stray"), "…and never a value: {text}");
    let conn = fx.conn();
    assert!(
        has_text_venue(&conn, "account"),
        "nothing was committed: the refusal came before ANY table lost its text, `account` included"
    );
    let kept: String = conn
        .query_row("SELECT venue FROM credential WHERE id = 4", [], |r| r.get(0))
        .expect("the off-roster row and its text are where they were");
    assert_eq!(kept, "no-such-venue");
}

#[test]
fn the_first_write_rebuilds_every_linked_table_onto_venue_id() {
    let fx = planted_on_the_old_shape();
    assert!(
        !is_not_null(&fx.conn(), "account", "venue_id"),
        "precondition: the old shape is nullable"
    );
    // The re-keyed indexes are asserted against a premise too, or "keyed on `venue_id` after" would
    // hold for a store that never keyed them on the text: the planted arming indexes key on `venue`.
    assert!(
        object_sql(&fx.conn(), "venue_arming_one_per_venue")
            .contains("(venue) WHERE label IS NULL")
            && object_sql(&fx.conn(), "venue_arming_one_per_account")
                .contains("(venue, label) WHERE label IS NOT NULL"),
        "precondition: the old shape keys both arming indexes on the TEXT venue"
    );

    // Any write reaches the funnel. This one is a no-op value on an existing row.
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("a write through the funnel");

    for table in ["account", "venue_setting", "venue_arming"] {
        assert!(
            is_not_null(&fx.conn(), table, "venue_id"),
            "`{table}.venue_id` must be NOT NULL now"
        );
    }
    assert!(object_sql(&fx.conn(), "account").contains("UNIQUE (venue_id, tier, label)"));
    assert!(object_sql(&fx.conn(), "venue_setting").contains("UNIQUE (venue_id, tier, field)"));
    assert!(
        object_sql(&fx.conn(), "account_one_account_per_book")
            .contains("(venue_id, venue_account_id)"),
        "the book index must key on the venue's number"
    );
    // ⚠ BOTH arming indexes, with their `label` predicates: they are what stops a venue holding two
    // ceilings, and the funnel re-creates them by NAME, so an index left on the text `venue` would
    // keep every name this test could ask for while keying on the column nothing reads any more.
    assert!(
        object_sql(&fx.conn(), "venue_arming_one_per_venue")
            .contains("(venue_id) WHERE label IS NULL"),
        "the venue-level ceiling index must key on the venue's number, unlabelled rows only"
    );
    assert!(
        object_sql(&fx.conn(), "venue_arming_one_per_account")
            .contains("(venue_id, label) WHERE label IS NOT NULL"),
        "the per-account ceiling index must key on the venue's number, labelled rows only"
    );
    let ids: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();
    assert_eq!(ids, [1, 2], "a rebuild never changes an account's id");
}

#[test]
fn the_rebuild_does_not_rewind_the_account_mark() {
    let fx = planted_on_the_old_shape();
    {
        let conn = fx.conn();
        // The top account is gone; the mark still remembers it.
        conn.execute_batch(
            "DELETE FROM account WHERE id = 2; \
             UPDATE sqlite_sequence SET seq = 2 WHERE name = 'account';",
        )
        .expect("remove the top row");
    }
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("the rebuilding write");
    // ⚠ The premise, asserted: the write REBUILT `account`. Without it this test passes vacuously
    // the day the pass stops rebuilding that table — the mark is then never touched and the next
    // id is 3 anyway.
    assert!(
        is_not_null(&fx.conn(), "account", "venue_id"),
        "premise: the write carried `account` onto the number, which is the rebuild under test"
    );
    let made = fx
        .edit(AccountEdit::Create { venue: "binance", tier: "live", label: None })
        .expect("a new account");
    let id = made.after.expect("the new row").id;
    assert_eq!(id, 3, "the removed account's id 2 must never be handed out again");
}

/// One row's `venue_id`, `None` for SQL NULL.
fn venue_id_of(fx: &Fixture, table: &str, id: i64) -> Option<i64> {
    let conn = fx.conn();
    conn.query_row(&format!("SELECT venue_id FROM {table} WHERE id = ?1"), [id], |r| r.get(0))
        .unwrap_or_else(|e| panic!("`{table}` id {id}: {e}"))
}

/// ⚠ **The fragment is asserted WHOLE**, table, id and venue together: the message's fixed text
/// says `account` on its own and the error renders the store's temp path, so separate
/// `contains("account")` / `contains('7')` checks could pass on text that names no row at all.
/// And "nothing was committed" is asked of a row the BACKFILL fills — it runs before the refusal,
/// in the same transaction — because the refused table itself would read unrebuilt either way.
#[test]
fn a_row_naming_no_roster_venue_refuses_every_write_by_name() {
    let fx = planted_on_the_old_shape();
    {
        let conn = fx.conn();
        conn.execute_batch(
            "INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (7, 'no-such-venue', NULL, 'demo');
             INSERT INTO account (id, venue, venue_id, tier) VALUES (8, 'okx', NULL, 'demo');",
        )
        .expect("plant the unresolvable row, and one the backfill would number");
    }
    let err =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("the write must be refused");
    let text = err.to_string();
    assert!(
        text.contains("`account` id 7 (venue 'no-such-venue')"),
        "the refusal names the table, the row and the venue string together: {text}"
    );
    assert!(!is_not_null(&fx.conn(), "account", "venue_id"), "the table was not rebuilt");
    assert_eq!(
        venue_id_of(&fx, "account", 8),
        None,
        "…and nothing was committed: the backfill numbered row 8 before the refusal, so a \
         committed transaction would have left it numbered"
    );
}

/// **The refusal's own two claims, held: no vike-cli verb gets past it, and the repair it names
/// does.** `account remove` writes through the same funnel, so it is refused by the same check —
/// which is why the message sends the operator to a SQLite client instead. Correcting the row's
/// venue there is one of the two repairs it names, and the next write must then carry the store.
/// The correction is the SAFE case the message states: no other unlabelled `okx`/`demo` account
/// exists, so it cannot make that pair ambiguous.
#[test]
fn the_repair_the_refusal_names_lets_the_next_write_through() {
    let fx = planted_on_the_old_shape();
    {
        let conn = fx.conn();
        conn.execute_batch(
            "INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (7, 'no-such-venue', NULL, 'demo');",
        )
        .expect("plant the unresolvable row");
    }

    let removed = fx.edit(AccountEdit::Remove { id: 7 });
    let err = removed.expect_err("`account remove` writes through the same check");
    assert!(err.to_string().contains("no-such-venue"), "…and is refused by it: {err}");
    assert!(fx.row(7).is_some(), "the refused remove left the row where it was");

    {
        let conn = fx.conn();
        conn.execute("UPDATE account SET venue = 'okx' WHERE id = 7", []).expect("the repair");
    }
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
        .expect("the repaired store takes the write");
    assert!(is_not_null(&fx.conn(), "account", "venue_id"), "…and is carried onto the number");
    let repaired = fx.row(7).expect("the repaired row keeps its id");
    assert_eq!(repaired.venue, "okx");
}

/// A store restored from a backup older than stage 2: no `venue_id` column anywhere. A read-only
/// reader must still answer, and the first write must bring the store onto the shipped shape.
#[test]
fn a_store_with_no_venue_id_column_still_answers_and_the_first_write_carries_it() {
    let (fx, conn) = plant(&support::pre_stage_2_ddl());
    conn.execute_batch(
        "INSERT INTO account (id, venue, tier) VALUES (1, 'binance', 'demo');
         INSERT INTO account (id, venue, tier) VALUES (2, 'binance', 'live');
         INSERT INTO venue_setting (venue, tier, field, value) \
             VALUES ('polymarket', 'any', 'PROXY_ENABLED', 'false');
         INSERT INTO venue_arming (venue, label, mode) VALUES ('binance', NULL, 'demo');",
    )
    .expect("rows");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    let mut venues: Vec<String> = fx.accounts().into_iter().map(|a| a.venue).collect();
    venues.sort();
    assert_eq!(venues, ["binance", "binance"], "the text column is the only answer there is");
    let rows = settings_rows(&fx);
    assert_eq!(rows.arming.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["binance"]);
    assert_eq!(rows.venue.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["polymarket"]);

    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("the carrying write");
    for table in ["account", "venue_setting", "venue_arming"] {
        assert!(
            is_not_null(&fx.conn(), table, "venue_id"),
            "`{table}` was carried onto the number"
        );
    }
}

/// **On a store no writer has carried, a row with no number is read, and matched, by its text** —
/// the fallback `crate::schema::VenueLink` keeps for exactly the statements that can meet such a
/// store. Two of them, on the release-before shape with one account row never given its number:
/// the read-only account reader, and `set_venue_account_id`'s book-holder check, which runs no
/// funnel (the venue handshake reaches it at boot), so the store is still uncarried when it asks.
/// Matched on the number alone, the reader would meet a NULL name and the check would miss the
/// holder and leave the engine's bare `UNIQUE` to refuse the write.
#[test]
fn a_row_with_no_number_is_read_and_matched_by_its_text_until_a_writer_carries_the_store() {
    let (fx, conn) = plant(&vike_secrets::venue_links::pre_venue_link_ddl());
    conn.execute_batch(
        "INSERT INTO account (id, venue, venue_id, tier, venue_account_id) \
             VALUES (1, 'binance', NULL, 'demo', 'B1');
         INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (2, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'live');",
    )
    .expect("one row with its number, one without");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    let read: Vec<(i64, String)> = fx.accounts().into_iter().map(|a| (a.id, a.venue)).collect();
    assert_eq!(
        read,
        [(1, "binance".to_string()), (2, "binance".to_string())],
        "the reader answers for the row with no number from its text"
    );

    let err =
        vike_secrets::set_venue_account_id_in(fx.dir(), 2, Some("B1"), false, BookSource::Operator)
            .expect_err("account 1 holds that book");
    assert!(
        matches!(err.kind, DbErrorKind::BookHeldByAnother { holder: 1, .. }),
        "the check finds the unnumbered holder by its text and names it: {err}"
    );
    assert!(
        !is_not_null(&fx.conn(), "account", "venue_id"),
        "…and nothing carried the store meanwhile"
    );
}

/// **Decision 0095 at BOOT meets the store AS FOUND, before any funnel has carried it** — the boot
/// step and `vike-cli config migrate-store` both run its rewrite first, then the funnel, in one
/// transaction. Here the store is on the shape the release before this one wrote, and its binance
/// `live` row was never given its number (a row written by a binary that predates the column, after
/// the backfill last ran). Matched by number alone, the row would be skipped while the marker was
/// written, and every later read would take that `live` as MAINNET.
#[test]
fn decision_0095_at_boot_rewrites_a_live_row_whose_number_was_never_backfilled() {
    let (fx, conn) = plant(&vike_secrets::venue_links::pre_venue_link_ddl());
    conn.execute_batch(
        "INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', NULL, NULL, 'live');",
    )
    .expect("an un-backfilled live row");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);
    assert!(
        !is_not_null(&fx.conn(), "venue_arming", "venue_id"),
        "precondition: the pre-flip shape"
    );

    assert!(
        vike_secrets::live_means_mainnet::live_means_mainnet_pending(fx.dir()).expect("the probe"),
        "the read-only probe must see the row through its text"
    );
    let vike_secrets::live_means_mainnet::LiveMeansMainnet::Applied { rewrites, .. } =
        apply_0095(&fx)
    else {
        panic!("a pending store is migrated");
    };
    assert_eq!(rewrites.iter().map(|r| r.key()).collect::<Vec<_>>(), ["policy.venues.binance"]);
    let (venue, venue_id, mode) = the_arming_row(&fx);
    assert_eq!((venue.as_str(), mode.as_str()), ("binance", "demo"), "the row was rewritten");
    assert!(venue_id.is_some(), "…and the funnel that ran after it gave it its number");
    assert!(marked_0095(&fx), "…and the marker is written");
}

/// The same boot step on a store older than `venue_id` itself — a backup restored from before
/// stage 2. There is no number to match by; the text is the only answer, and asking the store for a
/// column it lacks would stop a daemon booting.
#[test]
fn decision_0095_at_boot_rewrites_a_live_row_on_a_store_with_no_venue_id_column() {
    let (fx, conn) = plant(&support::pre_stage_2_ddl());
    conn.execute_batch(
        "INSERT INTO venue_arming (venue, label, mode) VALUES ('binance', NULL, 'live');",
    )
    .expect("a live row");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    assert!(
        vike_secrets::live_means_mainnet::live_means_mainnet_pending(fx.dir()).expect("the probe"),
        "the read-only probe must answer for a store with no `venue_id` column"
    );
    let vike_secrets::live_means_mainnet::LiveMeansMainnet::Applied { rewrites, .. } =
        apply_0095(&fx)
    else {
        panic!("a pending store is migrated");
    };
    assert_eq!(rewrites.iter().map(|r| r.key()).collect::<Vec<_>>(), ["policy.venues.binance"]);
    let (_, venue_id, mode) = the_arming_row(&fx);
    assert_eq!(mode, "demo", "the row was rewritten");
    assert!(venue_id.is_some(), "…the funnel carried the store onto the number");
    assert!(
        is_not_null(&fx.conn(), "venue_arming", "venue_id"),
        "…all the way onto the shipped shape"
    );
    assert!(marked_0095(&fx), "…and the marker is written");
}

/// **The store's repair refusing on the BOOT path is told apart from decision 0095 failing.** The
/// boot-path entry runs 0095's rewrite and then the funnel in one transaction; here the rewrite
/// goes through and the funnel's venue-links refusal (a row naming no roster venue) stops it. That
/// refusal must come back as `RepairRefused`, carrying its own message: a booting root passes it
/// through rather than blaming 0095 and naming `vike-cli config migrate-store`, which runs this
/// same funnel. The root's half is `crates/vike-boot/src/lib_tests.rs`'s
/// `a_repair_refusal_at_the_ceiling_step_is_not_blamed_on_decision_0095`.
#[test]
fn a_venue_links_refusal_at_boot_is_told_apart_from_decision_0095() {
    let (fx, conn) = plant(&vike_secrets::venue_links::pre_venue_link_ddl());
    conn.execute_batch(
        "INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', NULL, NULL, 'live');
         INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (7, 'no-such-venue', NULL, 'demo');",
    )
    .expect("a row 0095 rewrites, and a row naming no roster venue");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    let err = try_apply_0095(&fx).expect_err("the funnel refuses the off-roster row");
    assert!(
        matches!(err.kind, DbErrorKind::RepairRefused { .. }),
        "the repair's refusal must arrive as `RepairRefused`, got {err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("`account` id 7 (venue 'no-such-venue')"),
        "it carries the refusal's own rows: {text}"
    );
    assert!(
        std::error::Error::source(&err).is_some(),
        "…and keeps the engine error it arrived in as its source, rather than a copy of its text"
    );
    let (_, venue_id, mode) = the_arming_row(&fx);
    assert_eq!(
        (venue_id, mode.as_str()),
        (None, "live"),
        "nothing was committed, 0095's rewrite included: the row is as it was planted"
    );
    assert!(!marked_0095(&fx), "…and no marker");
    assert!(
        vike_secrets::live_means_mainnet::live_means_mainnet_pending(fx.dir()).expect("the probe"),
        "…so the store still asks for the migration"
    );
}
