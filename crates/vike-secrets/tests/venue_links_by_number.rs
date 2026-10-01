//! **Every venue link in the settings store is read BY NUMBER** — ruling 3 of
//! `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md`, ordered finished by the
//! owner on 2026-09-30.
//!
//! The text `venue` column is still WRITTEN for one release, so that a rollback to the release
//! before can read a store this one migrated. The only way to prove a statement uses the number is
//! therefore to make the text LIE and ask again: the `…_by_venue_id` tests plant real rows through
//! the public writers, rewrite every text `venue` cell to a string that names no venue, and ask.
//! Covered that way: the three readers (`read_accounts`, both halves of `read_settings`), every
//! branch of the account label guard (the label, the unlabelled and the book collision), the two
//! book-holder checks (`set_venue_account_id`'s and a re-activation's), the venue setting writer's
//! lookup, the one-row arming writer's `UPDATE`, and decision 0095's ceiling migration.
//!
//! The other half of the file is the store the number is MISSING from — older shapes a read-only
//! reader and decision 0095's boot-path migration must still answer for, because neither can wait
//! for the write funnel to carry the store — and the pass that carries it.

use vike_secrets::{
    AccountEdit, ArmingRow, BookSource, DbErrorKind, SettingsSource, StoredSettings,
};

mod support;
use support::Fixture;

/// Rewrite every text `venue` cell of the four linked tables to `text-lies-<id>`. `venue_id` is
/// untouched, so a reader that still answers `binance` read the number.
///
/// ⚠ Each table must have at least one cell to rewrite, asserted rather than assumed: a table the
/// fixture left empty would make every "the reader ignored the lie" claim about it vacuous, which is
/// how this helper rewrote zero `credential` cells until [`planted`] filed a venue-scoped one.
fn falsify_the_text_column(fx: &Fixture) {
    let conn = rusqlite::Connection::open(fx.db()).expect("open the store");
    for table in ["account", "credential", "venue_setting", "venue_arming"] {
        let rewritten = conn
            .execute(
                &format!("UPDATE {table} SET venue = 'text-lies-' || id WHERE venue IS NOT NULL"),
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
/// venue IS NULL)` means an account-scoped row never carries a venue — so without this row the
/// `credential` table holds no venue link at all.
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
#[test]
fn every_venue_scoped_credential_row_names_its_venue_by_number() {
    let fx = planted();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).expect("count") };
    assert!(
        count("SELECT COUNT(*) FROM credential WHERE venue IS NOT NULL") > 0,
        "the fixture must hold a venue-scoped credential, or this test asks nothing"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM credential WHERE venue IS NOT NULL AND venue_id IS NULL"),
        0,
        "a venue-scoped credential must carry venue_id"
    );
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
    let (fx, conn) = plant(&support::pre_venue_link_ddl());
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

/// **The refusal asks only while the table being rebuilt can still hold a NULL `venue_id`.** On a
/// CARRIED store an older pass can still rebuild a linked table — step 7's pass does, whenever an
/// older binary's batch has put a retired tier index back — and that copy cannot fail on
/// `venue_id`, whose column already refuses NULL. So an off-roster `credential` row (nullable by
/// design) must not refuse the write: no rebuild copies it into a `NOT NULL` column. Without the
/// table's own nullability guard, `crate::schema`'s trap 7 would refuse it here.
#[test]
fn a_carried_table_rebuilt_again_does_not_refuse_for_rows_it_does_not_copy() {
    let fx = Fixture::migrated();
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute_batch(support::LEGACY_TIER_INDEXES).expect("an older binary's batch");
        conn.execute(
            "INSERT INTO credential (account_id, venue, venue_id, field, value, name) \
             VALUES (NULL, 'no-such-venue', NULL, 'API_KEY', 'stray', 'NO_SUCH_VENUE_API_KEY')",
            [],
        )
        .expect("an off-roster venue-scoped credential row");
    }
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
        .expect("a carried store's write is not refused over a row no rebuild copies");

    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let retired: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' \
             AND name IN ('venue_setting_one_per_tier', 'venue_setting_one_per_machine')",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(retired, 0, "the premise: step 7's pass DID rebuild `venue_setting` on this write");
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
    let (fx, conn) = plant(&support::pre_venue_link_ddl());
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
    let (fx, conn) = plant(&support::pre_venue_link_ddl());
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
    let (fx, conn) = plant(&support::pre_venue_link_ddl());
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
    let DbErrorKind::RepairRefused { reason } = &err.kind else {
        panic!("the repair's refusal must arrive as `RepairRefused`, got {err}");
    };
    assert!(
        reason.contains("`account` id 7 (venue 'no-such-venue')"),
        "it carries the refusal's own rows: {reason}"
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
