//! The funnel's named refusals are RepairRefused at every door, and nothing else is.

use super::*;

// ---------------------------------------------------------------------------------------------
// The funnel's NAMED refusals are `RepairRefused` at every door, and nothing else is
// ---------------------------------------------------------------------------------------------

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
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
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
