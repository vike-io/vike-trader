//! The mirror must leave the `venue_setting` table alone: a moved venue row has no file to return from.

use super::*;

/// The rows a mirror would carry: everything EXCEPT venue settings, which no file can spell.
fn from_files() -> StoredSettings {
    StoredSettings {
        settings: vec![SettingRow {
            section: "policy".into(),
            key: "max_leverage".into(),
            value: "3.0".into(),
        }],
        venue: Vec::new(),
    }
}

#[test]
fn a_mirror_does_not_delete_the_moved_venue_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = crate::store_locator::db_path_in(tmp.path());

    // A box that has run the move: one tier-scoped row and one machine-scoped one.
    {
        let (conn, _created) = crate::db::open_for_write(&db).expect("open");
        conn.execute_batch(crate::schema::DDL).expect("ddl");
        // ⚠ The `DDL` batch seeds no `venue` row, and `venue_setting.venue_id` is `NOT NULL` since
        // the venue-links flip, so the two venues must exist before a row can name one — as they
        // do on any box that has run the move, whose writes top the roster up first. A row names
        // its venue by that number alone: the shipped `venue_setting` has no text `venue` since the
        // plan's second release.
        conn.execute_batch("INSERT INTO venue (name) VALUES ('dukascopy'), ('polymarket');")
            .expect("the venues the rows name");
        conn.execute(
            "INSERT INTO venue_setting (venue_id, tier, field, value) \
                 VALUES ((SELECT id FROM venue WHERE name = ?1), ?2, ?3, ?4)",
            ("dukascopy", Some("demo"), "SERVER", "https://example.invalid/x.jnlp"),
        )
        .expect("tier-scoped row");
        // ⚠ `'any'`, not `NULL` — this planted SQL NULL until §5.2 step 7, and the shipped
        // `tier TEXT NOT NULL` now refuses it. `'any'` is how the table spells the machine
        // scope, and the reader maps it back to `None`, so `before` below is unchanged.
        conn.execute(
            "INSERT INTO venue_setting (venue_id, tier, field, value) \
                 VALUES ((SELECT id FROM venue WHERE name = ?1), 'any', ?2, ?3)",
            ("polymarket", "PROXY_HOST", "127.0.0.1"),
        )
        .expect("machine-scoped row");
        // The reader refuses an unstamped store, and rightly — a fixture that skips this is
        // testing the version check rather than the mirror.
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION).expect("stamp");
    }

    // The fixture has to actually be there, or the assertion below proves nothing.
    let before = match read_settings(&db).expect("read") {
        SettingsSource::Rows { rows, .. } => rows.venue,
        other => panic!("expected rows, got {other:?}"),
    };
    assert_eq!(before.len(), 2, "the fixture did not land: {before:?}");

    // THE MIRROR — rows derived from files, carrying no venue settings at all.
    write_settings_in(tmp.path(), &from_files()).expect("mirror");

    let after = match read_settings(&db).expect("read back") {
        SettingsSource::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    };
    assert_eq!(
        after.venue, before,
        "a mirror DELETED venue settings — a box just lost its JForex server, its IBKR \
             gateway and its polymarket egress, and `config mirror` reported success"
    );
    // …and the control: the mirror really did run, so the survival above is the guard working
    // rather than the writer having done nothing at all.
    assert_eq!(after.settings.len(), 1, "the mirror wrote no setting rows: {after:?}");
    assert_eq!(after.settings[0].key, "max_leverage");
}
