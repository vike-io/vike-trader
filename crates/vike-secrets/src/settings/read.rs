//! Reading the settings tables and the adoption seal: `read_settings*`, `read_rows`, `read_adoption`.

use super::*;

/// **Read the settings rows from the database beside `settings_dir`.** Never creates anything.
///
/// The `_in` spelling of [`crate::backend_in`]'s convention: the caller holds the settings
/// DIRECTORY, the path is derived with [`crate::db_path_in`], and nothing here walks or reads an
/// environment.
pub fn read_settings_in(settings_dir: &Path) -> Result<SettingsSource, DbError> {
    read_settings(&crate::store_locator::db_path_in(settings_dir))
}

/// [`read_settings_in`] over the database's own path — the PURE root, so every arm is reachable
/// from a test without a walk.
pub fn read_settings(db: &Path) -> Result<SettingsSource, DbError> {
    if !database_present(db) {
        return Ok(SettingsSource::NoDatabase { path: db.to_path_buf() });
    }
    // Read-only, and the schema version is checked by the opener — a store at a version this binary
    // cannot read is LOUD here exactly as it is for a credential read, never a quiet empty layer.
    let conn = open_for_read(db)?;
    if !table_exists(db, &conn, "setting")? {
        return Ok(SettingsSource::TablesAbsent { path: db.to_path_buf() });
    }

    let rows = read_rows(&conn, db)?;

    // ...and the SEAL, from the same open. It is read LAST and it is read unconditionally: a store
    // whose `settings_adoption` table is absent is a store written before the seal existed, which
    // is `None` — the same answer as an adopted-then-undone box, and the right one for both.
    let adopted = if table_exists(db, &conn, "settings_adoption")? {
        read_adoption(db, &conn)?
    } else {
        None
    };

    Ok(SettingsSource::Rows { rows, adopted })
}

/// **The three settings tables' rows, over a connection the caller already holds** — the shared body
/// [`read_settings`] wraps (read-only, with the `NoDatabase`/`TablesAbsent` guards and the seal) and
/// [`write_setting_row_in`] calls directly (write-path, INSIDE its own transaction, where the tables
/// are guaranteed present by the DDL batch that runs ahead of it).
///
/// Takes `&rusqlite::Connection` rather than a generic so a `&rusqlite::Transaction<'_>` reaches it
/// by the ordinary `Deref` coercion every other reader/writer pair in this module already relies on.
pub(super) fn read_rows(conn: &rusqlite::Connection, db: &Path) -> Result<StoredSettings, DbError> {
    let mut settings = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT section, key, value FROM setting ORDER BY section, key")
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SettingRow { section: r.get(0)?, key: r.get(1)?, value: r.get(2)? })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            settings.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    // ⚠ The `venue_setting` rows, read the same way and guarded the same way. A store written
    // before this table existed answers with nothing to read, which is the truthful reading: that
    // box has moved no venue configuration out of `credential` because it could not have.
    let mut venue = Vec::new();
    if table_exists(db, conn, "venue_setting")? {
        let link = crate::schema::VenueLink::of(conn, "venue_setting", "t")
            .map_err(|e| DbError::sql(db, e))?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {}, t.tier, t.field, t.value FROM venue_setting t {} ORDER BY 1, 2, 3",
                link.name, link.join
            ))
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(VenueSettingRow {
                    venue: r.get(0)?,
                    // ⚠ §5.2 step 7's READ boundary, and the one place a stored tier becomes a
                    // Rust one. `'any'` AND a not-yet-migrated NULL both read as `None`; the
                    // function's own doc carries why an unmapped `'any'` is a silent outage.
                    tier: crate::schema::venue_setting_tier_of_stored(r.get(1)?),
                    field: r.get(2)?,
                    value: r.get(3)?,
                })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            venue.push(row.map_err(|e| DbError::sql(db, e))?);
        }
    }

    Ok(StoredSettings { settings, venue }.sorted())
}

/// The ADOPTION row, over a connection the caller already holds. At most one — `CHECK (id = 1)`.
pub(super) fn read_adoption(
    db: &Path,
    conn: &rusqlite::Connection,
) -> Result<Option<Adoption>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT adopted_at, tool_version, files_present, setting_rows \
             FROM settings_adoption WHERE id = 1",
        )
        .map_err(|e| DbError::sql(db, e))?;
    let mut rows = stmt
        .query_map([], |r| {
            Ok(Adoption {
                adopted_at: r.get(0)?,
                tool_version: r.get(1)?,
                files_present: r.get(2)?,
                setting_rows: r.get::<_, i64>(3)?.max(0) as usize,
            })
        })
        .map_err(|e| DbError::sql(db, e))?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| DbError::sql(db, e))?)),
        None => Ok(None),
    }
}
