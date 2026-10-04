//! **Every venue link in the settings store is read BY NUMBER** — ruling 3 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, ordered finished by the
//! owner on 2026-09-30.
//!
//! The text `venue` column was still WRITTEN for one release, so that a rollback to the release
//! before could read a store the first release migrated. The only way to prove a statement used the
//! number was therefore to make the text LIE and ask again: the `…_by_venue_id` tests plant real
//! rows through the public writers, rewrite every text `venue` cell to a string that names no
//! venue, and ask. ⚠ **The plan's second release DROPPED that column from `account`, `credential`
//! and `venue_setting`**, so on those three there is nothing left to lie with and no statement can
//! read a venue but by its number; the same tests now prove the property through `venue_arming`,
//! the one table that keeps a text column until Plan B deletes it, and through the absence of any
//! text column elsewhere (`the_contracting_write_removes_the_text_column_and_readers_still_answer`,
//! with `a_text_venue_with_no_number_refuses_the_contraction_by_name` for the one row the drop
//! would otherwise erase).
//! Covered that way: the three readers (`read_accounts`, both halves of `read_settings`), every
//! branch of the account label guard (the label, the unlabelled and the book collision), the two
//! book-holder checks (`set_venue_account_id`'s and a re-activation's), the venue setting writer's
//! lookup, the one-row arming writer's `UPDATE`, decision 0095's ceiling migration, the credential
//! filer's fallback account lookup (`AccountResolver::load`), the arming fold's two reads, and trap
//! 5's collision check.
//!
//! The other half of the file is the store the number is MISSING from — older shapes a read-only
//! reader and decision 0095's boot-path migration must still answer for, because neither can wait
//! for the write funnel to carry the store — and the pass that carries it.

use vike_secrets::{
    AccountEdit, ArmingRow, BookSource, DbErrorKind, SettingsSource, StoredSettings,
};

mod support;
use support::Fixture;

/// Rewrite every text `venue` cell of the four linked tables to `text-lies-<table>-<id>`.
/// `venue_id` is untouched, so a reader that still answers `binance` read the number.
///
/// ⚠ Each table must have at least one cell to rewrite, asserted rather than assumed: a table the
/// fixture left empty would make every "the reader ignored the lie" claim about it vacuous, which is
/// how this helper rewrote zero `credential` cells until [`planted`] filed a venue-scoped one.
///
/// ⚠ **The lie names its TABLE as well as its row, and it named only the row until review.** Two
/// tables' rows share an id often — the binance `demo` account and the binance `demo` arming row
/// are both id 1 in [`planted`] — so `text-lies-<id>` told the same lie twice, and a statement that
/// JOINED or MATCHED two tables by their text agreed with itself: the arming fold reverted to the
/// text on BOTH of its reads still armed account 1 under arming row 1's `text-lies-1`, and
/// `the_arming_fold_reads_both_tables_by_venue_id` stayed green over a fold that read no number at
/// all. A table-specific lie cannot agree across tables.
///
/// ⚠ **A table with no text `venue` is skipped, and since the plan's second release that is three
/// of the four.** `account`, `credential` and `venue_setting` lost the column, so there is nothing
/// in them to lie with: no statement can read a venue there but by the number. The tests built on
/// this keep running unchanged and prove the property through `venue_arming`, the one table that
/// keeps a text column until Plan B deletes it, and through the absence of any text column
/// elsewhere (`the_contracting_write_removes_the_text_column_and_readers_still_answer`). The probe
/// asks the engine, so a store whose text column came back would be lied to again.
fn falsify_the_text_column(fx: &Fixture) {
    let conn = rusqlite::Connection::open(fx.db()).expect("open the store");
    for table in ["account", "credential", "venue_setting", "venue_arming"] {
        let has_text: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'venue'"),
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("probe `{table}`: {e}"));
        if has_text == 0 {
            continue;
        }
        let rewritten = conn
            .execute(
                &format!(
                    "UPDATE {table} SET venue = 'text-lies-{table}-' || id WHERE venue IS NOT NULL"
                ),
                [],
            )
            .unwrap_or_else(|e| panic!("falsify `{table}`: {e}"));
        assert!(
            rewritten > 0,
            "`{table}` had no text venue to falsify; the lie would prove nothing"
        );
    }
}

/// A store with rows in every linked table: the fixture's accounts (binance demo and live,
/// dukascopy demo), one VENUE-scoped credential, one machine-scoped venue setting and one venue
/// arming row.
///
/// ⚠ The venue-scoped credential is filed the way `tests/venue_table.rs` files one: a key the shared
/// classifier has never seen, appended to the file store and migrated with a classifier layered
/// over `support::classify` that answers `Placement::Venue` for it alone. The shared classifier
/// answers only `Account` or `Infrastructure`, and `credential`'s `CHECK (account_id IS NULL OR
/// venue_id IS NULL)` (over the text `venue` until the plan's second release) means an
/// account-scoped row never carries a venue — so without this row the `credential` table holds no
/// venue link at all.
fn planted() -> Fixture {
    let fx = Fixture::migrated();
    let venue_key = "CTRADER_CLIENT_ID";
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(fx.store())
            .expect("open the file store for append");
        writeln!(file, "{venue_key}={}", support::fake_value(venue_key)).expect("append");
    }
    let classify = |name: &str| -> vike_secrets::Classification {
        if name == venue_key {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Venue("ctrader".to_string()),
                field: "CLIENT_ID".to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            };
        }
        support::classify(name)
    };
    vike_secrets::migrate(fx.arg(), support::is_node_key, &classify)
        .expect("migrate the venue-scoped credential");
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("a venue setting");
    vike_secrets::write_settings_in(
        fx.dir(),
        &StoredSettings {
            arming: vec![ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "demo".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("an arming row");
    fx
}

/// The store's settings rows, through the public read-only reader.
fn settings_rows(fx: &Fixture) -> StoredSettings {
    match vike_secrets::read_settings(&fx.db()).expect("the settings read") {
        SettingsSource::Rows { rows, .. } => rows,
        other => panic!("the planted store has its settings tables: {other}"),
    }
}

#[test]
fn the_account_reader_answers_from_venue_id_when_the_text_column_lies() {
    let fx = planted();
    let honest: Vec<(i64, String)> = fx.accounts().into_iter().map(|a| (a.id, a.venue)).collect();
    falsify_the_text_column(&fx);

    let read: Vec<(i64, String)> = fx.accounts().into_iter().map(|a| (a.id, a.venue)).collect();
    assert_eq!(read, honest, "`read_accounts` must name each account's venue from `venue_id`");
    // ⚠ …and the LITERAL venues, because the comparison above is against the reader's OWN pre-lie
    // read: a reader that answered wrongly both times would agree with itself.
    assert_eq!(
        read.iter().map(|(_, venue)| venue.as_str()).collect::<Vec<_>>(),
        ["binance", "binance", "dukascopy"],
        "the planted accounts are binance demo, binance live and dukascopy demo"
    );
}

#[test]
fn the_arming_reader_answers_from_venue_id_when_the_text_column_lies() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let rows = settings_rows(&fx);
    assert_eq!(
        rows.arming.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(),
        ["binance"],
        "the arming row's venue must come from `venue_id`"
    );
}

#[test]
fn the_venue_setting_reader_answers_from_venue_id_when_the_text_column_lies() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let rows = settings_rows(&fx);
    assert_eq!(
        rows.venue.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(),
        ["polymarket"],
        "the venue setting's venue must come from `venue_id`"
    );
}

#[test]
fn the_label_guard_finds_the_other_account_by_venue_id() {
    let fx = planted();
    fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") })
        .expect("the first HEDGE");
    falsify_the_text_column(&fx);

    let second =
        fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") });
    let err = second.expect_err("a second binance/demo/HEDGE must be refused");
    assert!(
        matches!(err.kind, vike_secrets::DbErrorKind::AccountLabelTaken { .. }),
        "the guard must find the first HEDGE through `venue_id`, got {err}"
    );
}

#[test]
fn a_venue_setting_rewrite_finds_its_row_by_venue_id() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let previous =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect("the rewrite");
    assert_eq!(
        previous.as_deref(),
        Some("false"),
        "the writer must find the existing row by `venue_id` and report its old value"
    );
}

/// The label guard's UNLABELLED branch: a second unlabelled `(binance, demo)` row is the ambiguity
/// no index can refuse (NULL labels are distinct), so only the guard's own query stands in front of
/// it, and that query must find the first row by its number.
#[test]
fn the_unlabelled_guard_finds_the_other_account_by_venue_id() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let second = fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: None });
    let err = second.expect_err("a second unlabelled binance/demo must be refused");
    assert!(
        matches!(err.kind, DbErrorKind::AmbiguousUnlabelledAccount { .. }),
        "the guard must find the unlabelled binance/demo row through `venue_id`, got {err}"
    );
}

/// The label guard's BOOK branch: a label equal to an active account's book at the same venue is
/// refused, because a mount resolves a policy address against the book first.
#[test]
fn the_book_guard_finds_the_label_held_as_a_book_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    vike_secrets::set_venue_account_id_in(
        fx.dir(),
        demo,
        Some("HEDGE"),
        false,
        BookSource::Operator,
    )
    .expect("the demo account's book");
    falsify_the_text_column(&fx);

    let labelled =
        fx.edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") });
    let err = labelled.expect_err("a label that is another binance account's book must be refused");
    assert!(
        matches!(err.kind, DbErrorKind::AccountLabelHeldAsBook { .. }),
        "the guard must find the book's holder through `venue_id`, got {err}"
    );
}

/// `set_venue_account_id`'s holder check: one book, two accounts of one venue, refused BY NAME.
///
/// ⚠ This writer runs NO repair funnel — it is one `UPDATE` of one row, and the venue handshake
/// reaches it at a daemon's boot — so its check also has to answer on a store no writer has carried
/// yet. On a carried store, which this is, it answers from the number like every other check.
#[test]
fn setting_a_book_finds_its_holder_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    let live = fx.id_of("binance", "live");
    vike_secrets::set_venue_account_id_in(fx.dir(), demo, Some("B1"), false, BookSource::Operator)
        .expect("the demo account's book");
    falsify_the_text_column(&fx);

    let err = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        live,
        Some("B1"),
        false,
        BookSource::Operator,
    )
    .expect_err("a book another active binance account names must be refused");
    assert!(
        matches!(err.kind, DbErrorKind::BookHeldByAnother { holder, .. } if holder == demo),
        "the check must find the demo account through `venue_id` and name it, got {err}"
    );
}

/// A RE-ACTIVATION's holder check: while the row was off, another row of its venue took its book.
#[test]
fn reactivating_finds_the_book_holder_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    let live = fx.id_of("binance", "live");
    vike_secrets::set_venue_account_id_in(fx.dir(), demo, Some("B1"), false, BookSource::Operator)
        .expect("the demo account's book");
    fx.edit(AccountEdit::SetActive { id: demo, active: false }).expect("deactivate it");
    vike_secrets::set_venue_account_id_in(fx.dir(), live, Some("B1"), false, BookSource::Operator)
        .expect("an inactive row frees its book");
    falsify_the_text_column(&fx);

    let err = fx
        .edit(AccountEdit::SetActive { id: demo, active: true })
        .expect_err("re-activating onto a book another active binance account holds is refused");
    assert!(
        matches!(err.kind, DbErrorKind::BookHeldByAnother { holder, .. } if holder == live),
        "the check must find the live account through `venue_id` and name it, got {err}"
    );
}

/// The one-row arming writer decides UPDATE-or-INSERT from the rows it reads, then `UPDATE`s by
/// venue and label. Both halves must name the venue by number, or the write either lands nowhere
/// or collides with the row it should have changed.
#[test]
fn an_arming_rewrite_finds_its_row_by_venue_id() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let written = vike_secrets::write_setting_row_in(
        fx.dir(),
        vike_secrets::RowChange::Arming {
            venue: "binance".to_string(),
            label: None,
            mode: "live".to_string(),
        },
        std::time::Duration::from_secs(3),
        |_, _, _| Ok(()),
    )
    .expect("the arming rewrite");
    assert_eq!(written.old_value.as_deref(), Some("demo"), "it found the row it replaced");

    let arming: Vec<(String, String)> =
        settings_rows(&fx).arming.into_iter().map(|r| (r.venue, r.mode)).collect();
    assert_eq!(
        arming,
        [("binance".to_string(), "live".to_string())],
        "…and the UPDATE landed on that row, not on nothing and not beside it"
    );
}

/// Decision 0095's migration reads the arming rows by number: a lying text column must not hide a
/// pre-0095 `live` ceiling of a switched venue, which a later read would take as MAINNET.
#[test]
fn a_lying_text_column_does_not_hide_a_live_ceiling_from_decision_0095() {
    let fx = planted();
    vike_secrets::write_settings_in(
        fx.dir(),
        &StoredSettings {
            arming: vec![ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "live".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("a binance `live` ceiling");
    vike_secrets::live_means_mainnet::unmark_live_means_mainnet(fx.dir());
    falsify_the_text_column(&fx);

    assert!(
        vike_secrets::live_means_mainnet::live_means_mainnet_pending(fx.dir()).expect("the probe"),
        "the read-only probe must see binance's `live` row through `venue_id`"
    );
    let vike_secrets::live_means_mainnet::LiveMeansMainnet::Applied { rewrites, .. } =
        apply_0095(&fx)
    else {
        panic!("a pending store is migrated");
    };
    assert_eq!(rewrites.iter().map(|r| r.key()).collect::<Vec<_>>(), ["policy.venues.binance"]);
    assert_eq!(
        settings_rows(&fx).arming.iter().map(|r| r.mode.as_str()).collect::<Vec<_>>(),
        ["demo"],
        "…and the row it reported is the row it rewrote"
    );
}

/// [`vike_secrets::live_means_mainnet::apply_live_means_mainnet`], the boot path's entry, as a test
/// root calls it.
fn apply_0095(fx: &Fixture) -> vike_secrets::live_means_mainnet::LiveMeansMainnet {
    try_apply_0095(fx).expect("the boot-path migration")
}

fn try_apply_0095(
    fx: &Fixture,
) -> Result<vike_secrets::live_means_mainnet::LiveMeansMainnet, vike_secrets::DbError> {
    vike_secrets::live_means_mainnet::apply_live_means_mainnet(
        fx.dir(),
        vike_model::change_journal::Actor::cli("test"),
        vike_model::change_journal::Proc::new("test", 1, "0.0.0"),
        1,
    )
}

/// Every venue-scoped credential row, alias rows included, carries the venue's number.
///
/// ⚠ `populated` keeps it from passing on an empty question: the shared fixture files no
/// venue-scoped credential, so [`planted`] adds one, and this asserts the row is there.
///
/// ⚠ **Since the plan's second release the number is ALL such a row carries**: `credential` has no
/// text venue, so "a text venue with no number" cannot be asked of it any more. What is left to ask
/// is that the row [`planted`] filed for `ctrader` names `ctrader` by its number — the only place
/// that venue is written down — and that no other row of the table names a venue at all (every
/// other credential the fixture files is account-scoped).
#[test]
fn every_venue_scoped_credential_row_names_its_venue_by_number() {
    let fx = planted();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT c.name, v.name FROM credential c JOIN venue v ON v.id = c.venue_id \
             ORDER BY c.name",
        )
        .expect("prepare");
    let named: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        named,
        [("CTRADER_CLIENT_ID".to_string(), "ctrader".to_string())],
        "the fixture's one venue-scoped credential names its venue by number, and nothing else \
         names one"
    );
}

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
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    for table in ["account", "credential", "venue_setting"] {
        let has: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'venue'"),
                [],
                |r| r.get(0),
            )
            .expect("probe");
        assert_eq!(has, 0, "`{table}` must have lost its text venue column");
    }
    let mut venues: Vec<String> = fx.accounts().into_iter().map(|a| a.venue).collect();
    venues.sort();
    assert_eq!(venues, ["binance", "binance", "dukascopy"]);
}

/// Whether the engine's `table` carries a column named `column`.
fn has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or_else(|e| panic!("probe `{table}.{column}`: {e}"))
        > 0
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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
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
    let credentials_before = credential_links(&rusqlite::Connection::open(fx.db()).expect("open"));

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

    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    assert_eq!(credential_links(&conn), credentials_before, "no credential moved or re-filed");
    let dangling: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))
        .expect("fk check");
    assert_eq!(dangling, 0, "no reference dangles after the rebuilds");
    assert!(table_sql(&fx, "account").contains("UNIQUE (venue_id, tier, label)"));
    assert!(table_sql(&fx, "venue_setting").contains("UNIQUE (venue_id, tier, field)"));
    for (index, keyed) in [
        ("account_one_account_per_book", "(venue_id, venue_account_id)"),
        ("credential_one_live_value", "(account_id, field)"),
        ("credential_one_live_name", "(name)"),
        ("venue_arming_one_per_venue", "(venue_id) WHERE label IS NULL"),
        ("venue_arming_one_per_account", "(venue_id, label) WHERE label IS NOT NULL"),
    ] {
        assert!(table_sql(&fx, index).contains(keyed), "`{index}` must still key on {keyed}");
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
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
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
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let text_columns: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('account') WHERE name = 'venue'",
            [],
            |r| r.get(0),
        )
        .expect("probe");
    assert_eq!(
        text_columns, 1,
        "nothing was committed: the refusal came before ANY table lost its text, `account` included"
    );
    let kept: String = conn
        .query_row("SELECT venue FROM credential WHERE id = 4", [], |r| r.get(0))
        .expect("the off-roster row and its text are where they were");
    assert_eq!(kept, "no-such-venue");
}

/// `(column, notnull)` of one table, from the engine itself.
fn notnull_of(fx: &Fixture, table: &str, column: &str) -> bool {
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.query_row(
        &format!("SELECT \"notnull\" FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n == 1)
    .unwrap_or_else(|e| panic!("`{table}.{column}`: {e}"))
}

fn table_sql(fx: &Fixture, name: &str) -> String {
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.query_row("SELECT sql FROM sqlite_master WHERE name = ?1", [name], |r| r.get(0))
        .unwrap_or_else(|e| panic!("`{name}`: {e}"))
}

/// Plant a store on `ddl` the way `tests/venue_setting_any_tier.rs`' `PreAnyTier::build` does
/// (the precedent every aged-store test here follows), with the whole roster in `venue`.
fn plant(ddl: &str) -> (Fixture, rusqlite::Connection) {
    let fx = Fixture::file_store();
    let db = fx.db();
    std::fs::create_dir_all(db.parent().expect("db dir")).expect("db dir");
    let conn = rusqlite::Connection::open(&db).expect("create");
    conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA foreign_keys = ON;").expect("pragmas");
    conn.execute_batch(ddl).expect("the aged shape");
    for (i, venue) in vike_model::venues::VENUES.iter().enumerate() {
        conn.execute("INSERT INTO venue (id, name) VALUES (?1, ?2)", (i as i64 + 1, venue))
            .expect("roster");
    }
    (fx, conn)
}

/// Plant the store BOTH live boxes hold today, the shipped `DDL` with this plan's first release
/// reverted, and give it rows in every linked table.
fn planted_on_the_old_shape() -> Fixture {
    let (fx, conn) = plant(&vike_secrets::venue_links::pre_venue_link_ddl());
    conn.execute_batch(
        "INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (1, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'demo');
         INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (2, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'live');
         INSERT INTO venue_setting (venue, venue_id, tier, field, value) \
             VALUES ('polymarket', (SELECT id FROM venue WHERE name = 'polymarket'), 'any', \
                     'PROXY_ENABLED', 'false');
         INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'demo');",
    )
    .expect("rows");
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
    drop(conn);
    fx
}

#[test]
fn the_first_write_rebuilds_every_linked_table_onto_venue_id() {
    let fx = planted_on_the_old_shape();
    assert!(!notnull_of(&fx, "account", "venue_id"), "precondition: the old shape is nullable");
    // The re-keyed indexes are asserted against a premise too, or "keyed on `venue_id` after" would
    // hold for a store that never keyed them on the text: the planted arming indexes key on `venue`.
    assert!(
        table_sql(&fx, "venue_arming_one_per_venue").contains("(venue) WHERE label IS NULL")
            && table_sql(&fx, "venue_arming_one_per_account")
                .contains("(venue, label) WHERE label IS NOT NULL"),
        "precondition: the old shape keys both arming indexes on the TEXT venue"
    );

    // Any write reaches the funnel. This one is a no-op value on an existing row.
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("a write through the funnel");

    for table in ["account", "venue_setting", "venue_arming"] {
        assert!(notnull_of(&fx, table, "venue_id"), "`{table}.venue_id` must be NOT NULL now");
    }
    assert!(table_sql(&fx, "account").contains("UNIQUE (venue_id, tier, label)"));
    assert!(table_sql(&fx, "venue_setting").contains("UNIQUE (venue_id, tier, field)"));
    assert!(
        table_sql(&fx, "account_one_account_per_book").contains("(venue_id, venue_account_id)"),
        "the book index must key on the venue's number"
    );
    // ⚠ BOTH arming indexes, with their `label` predicates: they are what stops a venue holding two
    // ceilings, and the funnel re-creates them by NAME, so an index left on the text `venue` would
    // keep every name this test could ask for while keying on the column nothing reads any more.
    assert!(
        table_sql(&fx, "venue_arming_one_per_venue").contains("(venue_id) WHERE label IS NULL"),
        "the venue-level ceiling index must key on the venue's number, unlabelled rows only"
    );
    assert!(
        table_sql(&fx, "venue_arming_one_per_account")
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
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
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
        notnull_of(&fx, "account", "venue_id"),
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
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
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
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
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
    assert!(!notnull_of(&fx, "account", "venue_id"), "the table was not rebuilt");
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
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
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
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute("UPDATE account SET venue = 'okx' WHERE id = 7", []).expect("the repair");
    }
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
        .expect("the repaired store takes the write");
    assert!(notnull_of(&fx, "account", "venue_id"), "…and is carried onto the number");
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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
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
        assert!(notnull_of(&fx, table, "venue_id"), "`{table}` was carried onto the number");
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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
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
    assert!(!notnull_of(&fx, "account", "venue_id"), "…and nothing carried the store meanwhile");
}

/// The one `venue_arming` row of `fx`, raw: `(venue text, venue_id, mode)`.
fn the_arming_row(fx: &Fixture) -> (String, Option<i64>, String) {
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.query_row("SELECT venue, venue_id, mode FROM venue_arming", [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .expect("exactly one arming row")
}

/// Whether decision 0095's marker row is on the store.
fn marked_0095(fx: &Fixture) -> bool {
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.query_row(
        "SELECT COUNT(*) FROM store_migration WHERE name = ?1",
        [vike_secrets::live_means_mainnet::LIVE_MEANS_MAINNET],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
    drop(conn);
    assert!(!notnull_of(&fx, "venue_arming", "venue_id"), "precondition: the pre-flip shape");

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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
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
    assert!(notnull_of(&fx, "venue_arming", "venue_id"), "…all the way onto the shipped shape");
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
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
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

// ---------------------------------------------------------------------------------------------
// The funnel's NAMED refusals are `RepairRefused` at every door, and nothing else is
// ---------------------------------------------------------------------------------------------

/// The release-before shape with one `account` row naming a venue the roster lacks — the store
/// trap 7 refuses every write on.
fn with_an_off_roster_account() -> Fixture {
    let fx = planted_on_the_old_shape();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    conn.execute(
        "INSERT INTO account (id, venue, venue_id, tier) VALUES (7, 'no-such-venue', NULL, 'demo')",
        [],
    )
    .expect("plant the unresolvable row");
    fx
}

/// What every door must answer for trap 7: the store's repair refusing, named as such, carrying
/// the rows — and never the engine-failure prefix that sends an operator to the file's permissions.
fn assert_a_repair_refusal(err: &vike_secrets::DbError) {
    let text = err.to_string();
    assert!(
        matches!(err.kind, DbErrorKind::RepairRefused { .. }),
        "trap 7 is the store's repair refusing, at every door: {text}"
    );
    assert!(
        !text.contains("could not be read"),
        "the store WAS read and a write was refused; this prefix points at permissions: {text}"
    );
    assert!(text.contains("`account` id 7 (venue 'no-such-venue')"), "…naming the row: {text}");
}

/// Final review M3: an ACCOUNT EDIT meets trap 7 like every writer, and reports it the way the boot
/// path does.
#[test]
fn trap_7_met_by_an_account_edit_is_a_repair_refusal() {
    let fx = with_an_off_roster_account();
    let err = fx
        .edit(AccountEdit::Create { venue: "okx", tier: "demo", label: None })
        .expect_err("the edit runs the funnel, which refuses the store");
    assert_a_repair_refusal(&err);
}

/// …and so does a VENUE-SETTING write, the door `vike-cli config set venue.*` reaches.
#[test]
fn trap_7_met_by_a_venue_setting_write_is_a_repair_refusal() {
    let fx = with_an_off_roster_account();
    let err =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("the write runs the funnel, which refuses the store");
    assert_a_repair_refusal(&err);
}

/// Make the database at `db` READ-ONLY by its own header: the file format's WRITE VERSION (byte 18)
/// above 2 makes SQLite treat the file as read-only — SQLite's file-format document: *"If the read
/// version is 1 or 2 but the write version is greater than 2, then the database file must be treated
/// as read-only"* — so every write meets the engine's own `SQLITE_READONLY`. Chosen over a `chmod`
/// because a mode refuses nothing to root, and this refusal is the engine's whoever runs the test.
fn make_read_only_by_header(db: &std::path::Path) {
    let mut bytes = std::fs::read(db).expect("read the database file");
    assert!(bytes.len() > 100 && bytes.starts_with(b"SQLite format 3\0"), "a database file");
    bytes[18] = 3;
    std::fs::write(db, bytes).expect("write it back");
}

/// …and a BARE engine failure inside the same funnel is NOT one. The store is read-only, so the
/// funnel's first statement that writes — `crate::db`'s `ensure_venue_rows` roster top-up, before
/// any pass — meets `SQLITE_READONLY`: an engine failure that names no row and that no SQLite-client
/// repair answers.
#[test]
fn an_engine_refusal_inside_the_funnel_is_not_a_repair_refusal() {
    let fx = Fixture::migrated();
    make_read_only_by_header(&fx.db());
    let err =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("a read-only store refuses the write");
    let text = err.to_string();
    assert!(
        matches!(err.kind, DbErrorKind::Sqlite(_)),
        "an engine failure stays an engine failure: {text}"
    );
    assert!(text.contains("readonly"), "premise: the engine's own SQLITE_READONLY: {text}");
}

/// …on the BOOT path too, where every funnel failure used to come back `RepairRefused`. A
/// read-only store cannot reach the funnel there (decision 0095's own `UPDATE` is that path's first
/// write), so the funnel is failed by the engine another way: two `account` rows whose TEXT differs
/// and whose NUMBER, tier and label agree — a lie only a hand edit makes — meet the number-keyed
/// `UNIQUE` in the carry's copy, an unnamed engine refusal.
#[test]
fn an_engine_refusal_inside_the_funnel_at_boot_is_not_a_repair_refusal() {
    let (fx, conn) = plant(&vike_secrets::venue_links::pre_venue_link_ddl());
    conn.execute_batch(
        "INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'live');
         INSERT INTO account (id, venue, venue_id, tier, label) \
             VALUES (1, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'demo', 'X');
         INSERT INTO account (id, venue, venue_id, tier, label) \
             VALUES (2, 'okx', (SELECT id FROM venue WHERE name = 'binance'), 'demo', 'X');",
    )
    .expect("a row 0095 rewrites, and two rows only their text tells apart");
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
    drop(conn);

    let err = try_apply_0095(&fx).expect_err("the carry's copy meets the number-keyed UNIQUE");
    let text = err.to_string();
    assert!(
        matches!(err.kind, DbErrorKind::Sqlite(_)),
        "an engine failure inside the funnel is not the funnel's NAMED refusal: {text}"
    );
    assert!(text.contains("UNIQUE constraint failed"), "premise: the engine's own words: {text}");
    let (_, _, mode) = the_arming_row(&fx);
    assert_eq!(mode, "live", "nothing was committed, 0095's rewrite included");
    assert!(!marked_0095(&fx), "…and no marker");
}

// ---------------------------------------------------------------------------------------------
// The repair the refusals send an operator to
// ---------------------------------------------------------------------------------------------

/// Final review M2: the SQLite-client repair trap 7 names must not be able to make things worse. It
/// offers the CORRECTION before the delete (a `DELETE` of a `credential` row removes a secret for
/// good), turns foreign keys ON first (the `sqlite3` shell starts with them OFF, so a premature
/// parent delete would go through and leave dangling references), and names the client version
/// below which the shell cannot open a store of `STRICT` tables at all.
#[test]
fn trap_7_offers_the_correction_first_and_opens_its_repair_with_foreign_keys_on() {
    let fx = with_an_off_roster_account();
    let text =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("refused")
            .to_string();
    assert!(text.contains("PRAGMA foreign_keys = ON;"), "the repair turns foreign keys on: {text}");
    assert!(text.contains("3.37"), "…and names the client version STRICT tables need: {text}");
    let correct = text.find("UPDATE <table> SET venue").expect("the correction is offered");
    let delete = text.find("DELETE FROM <table>").expect("the delete is offered");
    assert!(correct < delete, "the CORRECTION comes before the delete: {text}");
    assert!(
        text.contains("only after copying out every value"),
        "…and the delete only after the values are copied out: {text}"
    );
}

/// Final review M2's third step: a rebuild that finds a reference to a row that does not exist used
/// to refuse naming NO row — *"left 1 dangling reference(s); nothing was committed"* — while every
/// write stayed refused. It names each one now, from `pragma_foreign_key_check`.
#[test]
fn a_dangling_reference_is_refused_naming_its_rows() {
    let fx = planted_on_the_old_shape();
    {
        // Foreign keys OFF, as the `sqlite3` shell starts — the only way a credential row comes to
        // name an account that is not there. Turned off EXPLICITLY: the SQLite this workspace
        // bundles is built with them ON by default (MEASURED — the plant was refused
        // `FOREIGN KEY constraint failed` without this line), which is not the shell's default.
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute_batch("PRAGMA foreign_keys = OFF;").expect("foreign keys off");
        conn.execute(
            "INSERT INTO credential (id, account_id, field, value, name) \
             VALUES (5, 99, 'API_KEY', 'a-dangling-value', 'A_DANGLING_KEY')",
            [],
        )
        .expect("plant a credential row naming no account");
    }
    let err =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect_err("the carry's rebuild of `account` finds the dangling reference");
    let text = err.to_string();
    assert!(
        text.contains("`credential` row 5 (its `account` row is missing)"),
        "the refusal names the row, its table and the parent it names: {text}"
    );
    assert!(!text.contains("a-dangling-value"), "…and never a value: {text}");
    assert!(!notnull_of(&fx, "account", "venue_id"), "…and nothing was committed");
}

// ---------------------------------------------------------------------------------------------
// Decision 0095 on a row whose two spellings disagree
// ---------------------------------------------------------------------------------------------

/// `(the venue its NUMBER names, its text, its mode)` for every `venue_arming` row, by that name.
fn arming_by_number(fx: &Fixture) -> Vec<(String, String, String)> {
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT v.name, a.venue, a.mode FROM venue_arming a \
             JOIN venue v ON v.id = a.venue_id ORDER BY v.name",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect()
}

/// A row whose number and text disagree is rewritten from `live` to `demo` when EITHER names a
/// venue decision 0095 switches: this binary reads the number, a ROLLBACK reads the text, and `demo`
/// is the safe answer to both. Two rows, mirror images: one numbered aster (unswitched) and spelled
/// binance, one numbered binance and spelled aster.
#[test]
fn decision_0095_rewrites_a_live_row_when_either_its_number_or_its_text_names_a_switched_venue() {
    let fx = planted();
    let live = |venue: &str| ArmingRow {
        venue: venue.to_string(),
        label: None,
        mode: "live".to_string(),
        max_exposure: None,
    };
    vike_secrets::write_settings_in(
        fx.dir(),
        &StoredSettings { arming: vec![live("aster"), live("binance")], ..Default::default() },
    )
    .expect("two `live` ceilings");
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute(
            "UPDATE venue_arming SET venue = \
             CASE venue WHEN 'aster' THEN 'binance' WHEN 'binance' THEN 'aster' END",
            [],
        )
        .expect("swap the two rows' text cells");
    }
    vike_secrets::live_means_mainnet::unmark_live_means_mainnet(fx.dir());
    assert_eq!(
        arming_by_number(&fx),
        [
            ("aster".to_string(), "binance".to_string(), "live".to_string()),
            ("binance".to_string(), "aster".to_string(), "live".to_string()),
        ],
        "premise: both rows are `live`, and each one's text names the other venue"
    );

    let vike_secrets::live_means_mainnet::LiveMeansMainnet::Applied { rewrites, .. } =
        apply_0095(&fx)
    else {
        panic!("a pending store is migrated");
    };
    assert_eq!(rewrites.len(), 2, "both rows are rewritten: {rewrites:?}");
    // ⚠ …and each is REPORTED under the key this binary reads it by, its number's venue: the row
    // numbered aster was this binary's `policy.venues.aster`, and that is the ceiling that moved.
    // Until review the row was reported under its TEXT's switched venue, so both rewrites said
    // `policy.venues.binance` and the aster ceiling that changed was named nowhere.
    assert_eq!(
        rewrites.iter().map(|r| r.key()).collect::<Vec<_>>(),
        ["policy.venues.aster", "policy.venues.binance"],
        "each rewrite names the key this binary reads the row by"
    );
    assert_eq!(
        arming_by_number(&fx).into_iter().map(|(_, _, mode)| mode).collect::<Vec<_>>(),
        ["demo", "demo"],
        "…on the store, not only in the report"
    );
    assert!(marked_0095(&fx), "…and the marker is written");
}

// ---------------------------------------------------------------------------------------------
// The converted readers a revert to the text would otherwise survive
// ---------------------------------------------------------------------------------------------

/// `crate::schema`'s `AccountResolver::load` keys its fallback lookup — `(venue, tier, label)`, the
/// one a key with a NEW owner prefix reaches — on the venue's number. A legacy `MAINNET` spelling is
/// that key: its prefix is new, and the tier it normalizes onto names the binance `live` account
/// that already exists. Through a lying text column the lookup must still find it, or the store
/// mints a SECOND unlabelled binance `live` account, which no index refuses (NULL labels are
/// distinct) and which arms an ambiguity refusal for the next key of that tier.
#[test]
fn a_credential_filed_through_the_fallback_lookup_finds_its_account_by_venue_id() {
    let fx = planted();
    let live = fx.id_of("binance", "live");
    let before: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();
    falsify_the_text_column(&fx);

    // Composed, so the settings registry's literal sweep does not read a credential name here.
    let key = concat!("BINANCE", "_MAINNET_API_SECRET");
    {
        use std::io::Write as _;
        let mut file =
            std::fs::OpenOptions::new().append(true).open(fx.store()).expect("open for append");
        writeln!(file, "{key}={}", support::fake_value(key)).expect("append");
    }
    let classify = |name: &str| -> vike_secrets::Classification {
        if let Some(field) = name.strip_prefix(concat!("BINANCE", "_MAINNET_")) {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Account(vike_secrets::AccountKey {
                    venue: "binance".to_string(),
                    tier: "live".to_string(),
                    label: None,
                    discriminator: None,
                }),
                field: field.to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            };
        }
        support::classify(name)
    };
    vike_secrets::migrate(fx.arg(), support::is_node_key, &classify).expect("file the new key");

    let after: Vec<i64> = fx.accounts().iter().map(|a| a.id).collect();
    assert_eq!(after, before, "no account was minted: the key found its account by venue_id");
    let keys = vike_secrets::resolve_account_keys_in(fx.dir())
        .expect("the key names")
        .expect("a database answers");
    assert!(
        keys.get(&live).is_some_and(|k| k.names.iter().any(|n| n == key)),
        "…and the key is filed against binance's `live` account: {keys:?}"
    );
}

/// `fold_arming_into_accounts` reads BOTH tables by the venue's number — the arming rows it folds
/// FROM and the accounts it folds ONTO. Every bit is cleared first, so only a fold that runs and
/// matches the two tables by number can arm the binance `demo` account under binance's `demo` line.
#[test]
fn the_arming_fold_reads_both_tables_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    let live = fx.id_of("binance", "live");
    let duka = fx.id_of("dukascopy", "demo");
    falsify_the_text_column(&fx);
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute("UPDATE account SET armed = 0", []).expect("clear every bit");
    }

    // A write that folds and changes nothing else: the dukascopy account re-stated at its own
    // (absent) label — the account verbs fold on every commit, a no-op one included.
    fx.edit(AccountEdit::Rename { id: duka, label: None }).expect("a no-op rename");

    let armed = |id: i64| fx.row(id).expect("the row").armed;
    assert!(armed(demo), "binance `demo` is armed by binance's `demo` line, read by number");
    assert!(!armed(live), "…and binance `live`, above that line, is not");
}

/// Trap 5's collision check (`crate::schema`'s `carried_key_collisions`) groups by the venue's
/// number: two machine-scoped polymarket rows for one field collide under the shipped
/// `UNIQUE (venue_id, tier, field)` however their text cells read. With the text lying — each row
/// spelled differently — only the number can see it, and the refusal must still name both rows.
#[test]
fn trap_5_finds_a_collision_by_venue_id_when_the_text_column_lies() {
    let (fx, conn) = plant(&support::pre_any_tier_ddl());
    conn.execute_batch(
        "DROP INDEX venue_setting_one_per_machine;
         INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
             SELECT 1, 'polymarket', id, NULL, 'PROXY_HOST', 'one-value'
             FROM venue WHERE name = 'polymarket';
         INSERT INTO venue_setting (id, venue, venue_id, tier, field, value)
             SELECT 2, 'polymarket', id, NULL, 'PROXY_HOST', 'another-value'
             FROM venue WHERE name = 'polymarket';
         UPDATE venue_setting SET venue = 'text-lies-' || id;",
    )
    .expect("two rows one key would share by number, spelled apart");
    conn.pragma_update(None, "user_version", vike_secrets::SCHEMA_VERSION).expect("stamp");
    drop(conn);

    let err = vike_secrets::set_venue_setting_in(fx.dir(), "ibkr", None, "HOST", "<host>")
        .expect_err("step 7's rebuild meets the collision");
    let text = err.to_string();
    assert!(
        text.contains("POLY_PROXY_HOST") && text.contains("rows 1, 2"),
        "the refusal names the key and both rows, found by number: {text}"
    );
    assert!(
        !text.contains("one-value") && !text.contains("another-value"),
        "…and names keys, never values: {text}"
    );
}
