//! Reading the settings tables and the adoption seal: `read_settings*`, `write_adoption`, `clear_adoption`.

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
    if !table_exists(db, &conn, "setting")? || !table_exists(db, &conn, "venue_arming")? {
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

    let mut arming = Vec::new();
    {
        // The venue by its NUMBER (`crate::schema::VenueLink`); the text answers only for a row
        // whose number names no `venue` row.
        let link = crate::schema::VenueLink::of(conn, "venue_arming", "t")
            .map_err(|e| DbError::sql(db, e))?;
        // ⚠ The column is SELECTED only where it exists — see [`has_column`]. A store written
        // before it was added still answers, with `None` for every row, which is the truthful
        // reading: that box states no per-account figure because it could not have.
        let exposure = match has_column(conn, "venue_arming", "max_exposure")
            .map_err(|e| DbError::sql(db, e))?
        {
            true => "t.max_exposure",
            false => "NULL",
        };
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {}, t.label, t.mode, {exposure} FROM venue_arming t {} ORDER BY 1, 2",
                link.name, link.join
            ))
            .map_err(|e| DbError::sql(db, e))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ArmingRow {
                    venue: r.get(0)?,
                    label: r.get(1)?,
                    mode: r.get(2)?,
                    max_exposure: r.get(3)?,
                })
            })
            .map_err(|e| DbError::sql(db, e))?;
        for row in rows {
            arming.push(row.map_err(|e| DbError::sql(db, e))?);
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

    Ok(StoredSettings { settings, arming, venue }.sorted())
}

/// The ADOPTION row, over a connection the caller already holds. At most one — `CHECK (id = 1)`.
pub(super) fn read_adoption(
    db: &Path,
    conn: &rusqlite::Connection,
) -> Result<Option<Adoption>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT adopted_at, tool_version, files_present, venues_declared, setting_rows, \
             arming_rows FROM settings_adoption WHERE id = 1",
        )
        .map_err(|e| DbError::sql(db, e))?;
    let mut rows = stmt
        .query_map([], |r| {
            Ok(Adoption {
                adopted_at: r.get(0)?,
                tool_version: r.get(1)?,
                files_present: r.get(2)?,
                venues_declared: r.get::<_, i64>(3)? != 0,
                setting_rows: r.get::<_, i64>(4)?.max(0) as usize,
                arming_rows: r.get::<_, i64>(5)?.max(0) as usize,
            })
        })
        .map_err(|e| DbError::sql(db, e))?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| DbError::sql(db, e))?)),
        None => Ok(None),
    }
}

/// **Write the ADOPTION SEAL** — the act that makes the rows answer for every settings key on this
/// box, and the only thing in this crate that can.
///
/// Reached from `vike-cli config adopt` and from nothing else. That command re-compares the two
/// resolutions and re-reads every row through the boot's own reader BEFORE calling this, which is
/// what LICENSES the dispositions that change on the far side of the seal — above all `vike_config`
/// inverting an unreadable row from a degrade into a refusal. The seal is positive evidence that
/// every row was readable at a moment somebody chose, not a preference.
///
/// The counts are taken from the tables INSIDE this transaction rather than from the caller, so the
/// erase detector can never be sealed against a number nobody measured.
///
/// ⚠ Refuses a store with no `setting` table rather than creating one: an adoption over tables that
/// do not exist would seal a box into resolving from nothing, which is every ceiling absent and
/// every venue `paper`. `vike-cli config mirror` is what creates them.
pub fn write_adoption(
    db: &Path,
    adopted_at: &str,
    tool_version: &str,
    files_present: &str,
    venues_declared: bool,
) -> Result<Adoption, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    let (mut conn, created) = open_for_write(db)?;
    if created {
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    if !table_exists(db, &conn, "setting")? || !table_exists(db, &conn, "venue_arming")? {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsTables });
    }

    let tx = conn.transaction().map_err(|e| DbError::sql(db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(db, e))?;
    let setting_rows: i64 = tx
        .query_row("SELECT count(*) FROM setting", [], |r| r.get(0))
        .map_err(|e| DbError::sql(db, e))?;
    let arming_rows: i64 = tx
        .query_row("SELECT count(*) FROM venue_arming", [], |r| r.get(0))
        .map_err(|e| DbError::sql(db, e))?;
    tx.execute("DELETE FROM settings_adoption", []).map_err(|e| DbError::sql(db, e))?;
    tx.execute(
        "INSERT INTO settings_adoption \
         (id, adopted_at, tool_version, files_present, venues_declared, setting_rows, arming_rows) \
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            adopted_at,
            tool_version,
            files_present,
            i64::from(venues_declared),
            setting_rows,
            arming_rows
        ],
    )
    .map_err(|e| DbError::sql(db, e))?;
    tx.commit().map_err(|e| DbError::sql(db, e))?;

    Ok(Adoption {
        adopted_at: adopted_at.to_string(),
        tool_version: tool_version.to_string(),
        files_present: files_present.to_string(),
        venues_declared,
        setting_rows: setting_rows.max(0) as usize,
        arming_rows: arming_rows.max(0) as usize,
    })
}

/// **Delete the ADOPTION SEAL** — `vike-cli config adopt --undo`, the rollback.
///
/// The rows are left exactly where they are and the four files answer again. That is the whole of
/// the rollback: ONE row, and no redeploy of a trading daemon. It is also why the crossing is safe
/// to rehearse — nothing about the data is undone, because nothing about the data was changed.
///
/// `Ok(false)` when there was no seal (an already-unadopted box is not an error to un-adopt).
pub fn clear_adoption(db: &Path) -> Result<bool, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    let (conn, created) = open_for_write(db)?;
    if created {
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    if !table_exists(db, &conn, "settings_adoption")? {
        return Ok(false);
    }
    let removed =
        conn.execute("DELETE FROM settings_adoption", []).map_err(|e| DbError::sql(db, e))?;
    Ok(removed > 0)
}
