//! The operator's writer for a moved venue setting, and the DDL constraint behind it.

use super::*;

fn empty_store() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = crate::store_locator::db_path_in(tmp.path());
    let (conn, _c) = crate::db::open_for_write(&db).expect("open");
    conn.execute_batch(crate::schema::DDL).expect("ddl");
    conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).expect("stamp");
    drop(conn);
    (tmp, db)
}

fn rows_of(db: &std::path::Path) -> Vec<VenueSettingRow> {
    match read_settings(db).expect("read") {
        SettingsSource::Rows { rows, .. } => rows.venue,
        other => panic!("expected rows, got {other:?}"),
    }
}

/// A new row reports `None`; writing it again reports what it replaced and does NOT duplicate.
#[test]
fn a_write_upserts_and_reports_what_it_replaced() {
    let (tmp, db) = empty_store();

    let first = set_venue_setting_in(tmp.path(), "dukascopy", Some("demo"), "SERVER", "first")
        .expect("write");
    assert_eq!(first, None, "a new row replaced nothing");

    let second = set_venue_setting_in(tmp.path(), "dukascopy", Some("demo"), "SERVER", "second")
        .expect("rewrite");
    assert_eq!(second.as_deref(), Some("first"), "the replaced value is reported");

    let rows = rows_of(&db);
    assert_eq!(rows.len(), 1, "the upsert DUPLICATED the row: {rows:?}");
    assert_eq!(rows[0].value, "second");
}

/// ⚠ **THE ARGUMENT FOR THE TABLE, AS BEHAVIOUR.** A mistyped tier is refused by the DATABASE.
///
/// This is what a `venue` map field on `vike_config::Config` could never have given: that shape
/// accepts any map key, so `venue.ibkr.demoo.backend` would have been stored, read by nothing,
/// and silent. `Config` carries `#[serde(deny_unknown_fields)]` precisely so an unknown key is
/// refused by name, and a map field would have made the venue subtree the one place in the
/// settings model where that stops being true — at sixteen roster venues, the largest
/// unguarded surface in it.
#[test]
fn a_mistyped_tier_is_refused_by_the_database() {
    let (tmp, db) = empty_store();

    let bad = set_venue_setting_in(tmp.path(), "ibkr", Some("demoo"), "BACKEND", "cpapi");
    assert!(bad.is_err(), "a tier outside the DDL's CHECK was accepted: {bad:?}");
    assert!(rows_of(&db).is_empty(), "a refused write left a row behind");

    // …and the control: the same call with a REAL tier lands, so the refusal above is the
    // CHECK biting rather than the writer being broken for every input.
    set_venue_setting_in(tmp.path(), "ibkr", Some("demo"), "BACKEND", "cpapi").expect("good");
    assert_eq!(rows_of(&db).len(), 1);
}

/// ⚠ A MACHINE-SCOPED row (`tier: None`, stored `'any'`) and a tier-scoped one are different
/// rows, which the total `UNIQUE (venue, tier, field)` encodes. The polymarket proxy is the
/// family that takes the first shape. (This read *"(`tier = NULL`) … which the two partial
/// unique indexes encode"* until §5.2 step 7 replaced both.)
#[test]
fn machine_scoped_and_tier_scoped_are_separate_rows() {
    let (tmp, db) = empty_store();

    set_venue_setting_in(tmp.path(), "polymarket", None, "PROXY_HOST", "127.0.0.1")
        .expect("machine-scoped");
    set_venue_setting_in(tmp.path(), "polymarket", Some("live"), "PROXY_HOST", "<host>")
        .expect("tier-scoped");

    let rows = rows_of(&db);
    assert_eq!(rows.len(), 2, "the two scopes collapsed onto one row: {rows:?}");

    // …and re-writing the machine-scoped one updates IT, not its tier-scoped neighbour.
    let was = set_venue_setting_in(tmp.path(), "polymarket", None, "PROXY_HOST", "127.0.0.2")
        .expect("rewrite");
    assert_eq!(was.as_deref(), Some("127.0.0.1"), "it replaced the machine-scoped value");
    let rows = rows_of(&db);
    assert_eq!(rows.len(), 2, "still two rows");
    let tiered = rows.iter().find(|r| r.tier.is_some()).expect("the tier-scoped row survives");
    assert_eq!(tiered.value, "<host>", "the neighbour was overwritten");
}
