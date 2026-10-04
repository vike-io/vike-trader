//! The `venue` table — the store's projection of `vike_model::VENUES`.
//!
//! ⚠ It is a PROJECTION, never a second roster. The compiled `VENUES` decides what a venue is; this
//! table exists so a venue reference has a foreign-key target.

use vike_model::VENUES;

mod support;
use support::Fixture;

#[test]
fn a_migrated_store_holds_one_row_per_roster_venue() {
    let fx = Fixture::migrated();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let mut stmt = conn.prepare("SELECT name FROM venue ORDER BY name").expect("prepare");
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    let mut expected: Vec<String> = VENUES.iter().map(|v| (*v).to_string()).collect();
    expected.sort();
    assert_eq!(rows, expected, "the venue table must project the whole roster");
}

#[test]
fn every_venue_row_has_a_distinct_id() {
    let fx = Fixture::migrated();
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let count: i64 =
        conn.query_row("SELECT COUNT(DISTINCT id) FROM venue", [], |r| r.get(0)).expect("count");
    assert_eq!(count as usize, VENUES.len());
}

/// A no-op re-migrate over an unchanged file leaves the `venue` table exactly as it was — a true
/// property, but NOT a test of the top-up's idempotence: `migrate()` returns `AlreadyComplete`
/// without opening a write connection when nothing is pending, so `ensure_venue_rows` is never
/// re-entered here and this would pass identically with a plain `INSERT` in place of `INSERT OR
/// IGNORE`. **The idempotence property lives in `crates/vike-secrets/src/db_tests.rs`'s
/// `ensure_venue_rows_is_idempotent_within_one_transaction`**, a unit test mutation-proven
/// against exactly that defect — look there, not here, for the real proof.
#[test]
fn a_no_op_re_migrate_leaves_the_venue_table_alone() {
    let fx = Fixture::migrated();
    // A second migration over the SAME file must not double the table.
    vike_secrets::migrate(fx.arg(), support::is_node_key, &support::classify).expect("re-migrate");
    let conn = rusqlite::Connection::open(fx.db()).expect("open");
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM venue", [], |r| r.get(0)).expect("count");
    assert_eq!(count as usize, VENUES.len(), "a re-run must top up, never duplicate");
}

/// One table's half of [`every_writer_names_its_venue_by_number_on_every_row`] — the vacuousness
/// guard, the venues the rows name BY NUMBER, and (where the table still has a text column) the two
/// agreement checks, over a FRESH connection so a caller never risks reading a stale snapshot from
/// one opened before the write it is about to check.
///
/// ⚠ **Called immediately after the write it checks, and that is the whole point — see this
/// function's caller for why "immediately" is load-bearing rather than tidy.**
///
/// ⚠ **`named` is the property since the venue-links plan's second release.** `account`,
/// `credential` and `venue_setting` lost their text `venue` then, so on those three there is no
/// second spelling to agree with: what a writer can get wrong is the NUMBER, and the only witness
/// of the venue it was handed is the caller's own list. `venue_arming` keeps its text column until
/// Plan B, so the agreement checks still run there.
///
/// ⚠ **`named` is PER ROW**, `(the venue the row's number names, the row's own cell in column
/// `cell`)` for every row carrying a number, in id order — not the set of venues the table names.
/// A set would pass two rows whose numbers were SWAPPED (binance's row numbered dukascopy and the
/// other way round, say): both venues still appear. Pairing each venue with a cell that tells the
/// rows apart (`tier`, a credential's `name`, a ceiling's `mode`) fails that.
fn assert_venue_id_agrees(db: &std::path::Path, table: &str, cell: &str, named: &[(&str, &str)]) {
    let conn = rusqlite::Connection::open(db).expect("open");

    // The vacuousness guard fix round 1 asked for: without at least one row carrying a venue, every
    // assertion below is trivially true whatever the schema or the writers do — exactly how the
    // first version of this test passed while proving nothing for three of its four tables.
    let populated: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE venue_id IS NOT NULL"), [], |r| {
            r.get(0)
        })
        .expect("count");
    assert!(
        populated > 0,
        "`{table}` has no row with a venue set at all — this test would be vacuous for it"
    );
    assert_eq!(
        usize::try_from(populated).expect("a count"),
        named.len(),
        "`{table}`: every row with a number is in the expected list, and no other"
    );

    // Each row's venue by NUMBER, through the `venue` table: a writer that filed the wrong number
    // names the wrong venue here, and one that filed a number naming no `venue` row drops out of
    // the join and shortens the list against the count above.
    let mut stmt = conn
        .prepare(&format!(
            "SELECT v.name, t.{cell} FROM {table} t JOIN venue v ON v.id = t.venue_id ORDER BY t.id"
        ))
        .expect("prepare");
    let found: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    let expected: Vec<(String, String)> =
        named.iter().map(|(v, c)| ((*v).to_string(), (*c).to_string())).collect();
    assert_eq!(
        found, expected,
        "`{table}`'s rows must each name, by number, the venue they were written for"
    );

    let has_text: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'venue'"),
            [],
            |r| r.get(0),
        )
        .expect("probe");
    if has_text == 0 {
        return;
    }
    let sql = format!(
        "SELECT COUNT(*) FROM {table} t JOIN venue v ON v.id = t.venue_id \
         WHERE t.venue IS NOT NULL AND v.name <> t.venue"
    );
    let mismatched: i64 = conn.query_row(&sql, [], |r| r.get(0)).expect("query");
    assert_eq!(mismatched, 0, "`{table}` has rows where venue_id and venue disagree");

    let sql = format!("SELECT COUNT(*) FROM {table} WHERE venue IS NOT NULL AND venue_id IS NULL");
    let unfilled: i64 = conn.query_row(&sql, [], |r| r.get(0)).expect("query");
    assert_eq!(unfilled, 0, "`{table}` has rows with a venue but no venue_id");
}

/// Every writer files, on every row it writes, the NUMBER of the venue it was handed — and where a
/// table still carries the text `venue` beside it, the two AGREE on every row.
///
/// ⚠ **Renamed with the venue-links plan's second release**, from
/// `venue_id_agrees_with_the_text_column_on_every_row`. Agreement was the property that release's
/// reshape rested on: it dropped the text column from `account`, `credential` and `venue_setting`,
/// and a row where the two disagreed would have silently changed which venue it names. Once the
/// text was gone from those three, the number is the only spelling a writer can get wrong, so each
/// table's check now names the venues its rows must read as; `venue_arming` keeps the agreement
/// checks until Plan B deletes it.
///
/// ⚠ **`Fixture::migrated()` alone is not enough, and an earlier version of this test only used
/// that.** Its shared `classify()` — reused by `account_lifecycle.rs`, so it is not touched here —
/// only ever answers `Placement::Account` or `Placement::Infrastructure`, never `Placement::Venue`,
/// and the fixture never calls `write_settings_in`, `set_venue_setting_in`, `edit_account` or
/// `move_pending_rows`. So a bare migrated fixture leaves `venue_arming` and `venue_setting` EMPTY
/// and `credential` with zero rows carrying a non-NULL `venue`. This version writes at least one
/// venue-carrying row into all four tables itself, through the crate's own public writers, without
/// widening the SHARED fixture — a second `Placement::Venue` case in `support::classify` would
/// ripple into `account_lifecycle.rs`'s own assertions about which accounts a migration creates.
///
/// ⚠ **A SECOND, sharper defect this test has already had, and the reason every write below is
/// followed IMMEDIATELY by that table's own assertion rather than by one batched loop at the end.**
/// `ensure_venue_id_columns` (`crate::db`) runs at the START of every non-`fill_into` writer's own
/// transaction — `set_venue_setting_in` and `write_settings_in` both call it, and so does
/// `fill_into` itself, ahead of `write_rows`, on every `migrate()` call. That backfill sweeps ALL
/// FOUR tables, not just the one the caller is about to touch. So a batched-assertions version of
/// this test — write account, write credential, write venue_setting, write venue_arming, THEN check
/// all four — has THREE hidden healers: the credential-writing `migrate()` call heals a broken
/// `account` row from the fixture; `set_venue_setting_in` heals a broken `credential` row from that
/// `migrate()` call; `write_settings_in` heals a broken `venue_setting` row from `set_venue_setting_in`.
/// Only the LAST write of a batched version is ever genuinely proven — MEASURED in fix round 2's
/// review, which traced that fix round 1's own reordering (to prove `venue_arming` last) had
/// silently REOPENED `account`: a real mutation of `AccountResolver::resolve`'s INSERT would have
/// passed this test clean the moment the credential/venue_setting/venue_arming writes were added
/// after it, and nothing here would have said so. Asserting right after each write closes this by
/// construction — for every table, the only call that could ever heal a broken row is the write
/// this very check is validating, because nothing else runs between them — and it stays closed when
/// a future write is appended, because THAT write cannot rewrite the past.
#[test]
fn every_writer_names_its_venue_by_number_on_every_row() {
    let fx = Fixture::migrated();

    // `account`: `Fixture::migrated()` itself wrote these rows (binance/demo, binance/live,
    // dukascopy/demo1, all carrying a venue) — checked BEFORE anything else runs, so nothing has had
    // a chance to heal a broken one yet.
    assert_venue_id_agrees(
        &fx.db(),
        "account",
        "tier",
        &[("binance", "demo"), ("binance", "live"), ("dukascopy", "demo")],
    );

    // `credential`: append a key `support::classify` has never seen, and hand `migrate` a LOCAL
    // classifier — layered over `support::classify`, never replacing it — that answers
    // `Placement::Venue` for it alone. `write_rows` skips every name already live in the store, so
    // this closure is invoked for this one new name only; the six fixture keys keep going through
    // `support::classify`'s own answers, whether or not `write_rows` happens to re-consult it.
    let venue_credential_key = "CTRADER_CLIENT_ID";
    {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(fx.store())
            .expect("open the file store for append");
        writeln!(file, "{venue_credential_key}={}", support::fake_value(venue_credential_key))
            .expect("append a Placement::Venue credential line");
    }
    let classify_with_a_venue_credential = |name: &str| -> vike_secrets::Classification {
        if name == venue_credential_key {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Venue("binance".to_string()),
                field: "CLIENT_ID".to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            };
        }
        support::classify(name)
    };
    vike_secrets::migrate(fx.arg(), support::is_node_key, &classify_with_a_venue_credential)
        .expect("migrate the appended venue-scoped credential");
    assert_venue_id_agrees(&fx.db(), "credential", "name", &[("binance", "CTRADER_CLIENT_ID")]);

    // `venue_setting`: through the same public writer a venue-setting edit uses.
    vike_secrets::set_venue_setting_in(fx.dir(), "binance", None, "SERVER", "demo.example.test")
        .expect("write a venue_setting row");
    assert_venue_id_agrees(&fx.db(), "venue_setting", "tier", &[("binance", "any")]);

    // ...and a SECOND call for the SAME `(venue, tier, field)`, with a different value, so this
    // fires the `ON CONFLICT DO UPDATE` branch rather than the plain `INSERT` branch the call above
    // took. Without this, `set_venue_setting_in`'s conflict path had no coverage at all in this
    // crate: the first call above is always a fresh insert, since nothing wrote this key before it.
    // Asserted immediately after, same adjacency as every other write in this test, so masking stays
    // impossible here too.
    //
    // ⚠ **What this proves, and what it does NOT.** It proves the `ON CONFLICT` branch is REACHED
    // and that the row it updates still agrees on `venue_id` afterward. It does NOT — and cannot —
    // regression-guard `set_venue_setting_in`'s own `venue_id = excluded.venue_id` clause
    // specifically: the venue is inside `venue_setting`'s own conflict key (the text `venue` until
    // the venue-links plan's second release, `venue_id` since), so a conflicting row's `venue_id`
    // can never legitimately need to change, and removing that clause leaves this assertion green.
    // See that clause's own comment
    // (`crates/vike-secrets/src/settings.rs`) for the full argument — it is a DECLARED BLIND SPOT,
    // not a gap this test failed to close.
    vike_secrets::set_venue_setting_in(fx.dir(), "binance", None, "SERVER", "demo2.example.test")
        .expect("overwrite the venue_setting row via its ON CONFLICT branch");
    assert_venue_id_agrees(&fx.db(), "venue_setting", "tier", &[("binance", "any")]);

    // `venue_arming`: through the same public writer `vike-cli config mirror` uses.
    vike_secrets::write_settings_in(
        fx.dir(),
        &vike_secrets::StoredSettings {
            arming: vec![vike_secrets::ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "demo".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("write a venue_arming row");
    assert_venue_id_agrees(&fx.db(), "venue_arming", "mode", &[("binance", "demo")]);
}
