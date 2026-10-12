//! The `profile_risk` rows: a DISCLOSURE copy of one run profile's `[risk]` table.

use super::*;

// ---------------------------------------------------------------------------------------------
// `profile_risk` — 0057 Phase 2. A DISCLOSURE copy; see this module's doc for what reads it.
// ---------------------------------------------------------------------------------------------

/// One row of the `profile_risk` table: a `[risk]` key of ONE run profile, and **the TOML RENDERING
/// of one scalar**.
///
/// The value is a rendering rather than a typed column for the same reason [`SettingRow::value`] is
/// — it round-trips through the same parse an operator's file goes through, so no second validator
/// is written. The key vocabulary is `vike_model::ProfileRisk::keys()`, and each row is judged by
/// that type's own parser (`vike_config::check_risk_key`).
///
/// ⚠ **This column stayed TOML when [`SettingRow::value`] became JSON on 2026-09-18, and the
/// difference is a decision rather than an oversight.** `vike_config::profile_risk`'s module doc
/// states the rule this table is built on — *"the mirror must never be
/// STRICTER than the boot"* — and a JSON column would break it on exactly one shape: JSON has no
/// literal for `inf`, while `max_leverage = inf` is a run profile `vike_model::ProfileRisk` parses
/// and `vike_core::RunProfile::validate` does not refuse. The settings column has no such shape,
/// because `vike_config::load` runs FIRST there and every `f64` field is finiteness-checked or
/// bounded. Every OTHER value this column can hold is valid JSON carrying the same value — the
/// roster admits floats, integers and bools and nothing else — so the two encodings differ on the
/// one case that forced them apart, and on nothing an operator's profile can otherwise produce.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileRiskRow {
    /// The `[risk]` key — `max_notional_per_order`, `max_leverage`, …. No `risk.` prefix: the
    /// table IS the `[risk]` table.
    pub key: String,
    /// The TOML rendering of one scalar — `250000.0`, `10`, `true`.
    pub value: String,
}

/// One run profile's whole `[risk]` table, as rows.
///
/// [`Self::profile`] is the profile's NAME, not a path. A path is a fact about one box's disk and
/// would put a box-specific string in a database that `vike-cli secrets`/`config` prints; the file
/// NAME is what an operator recognised (the table was written from a FILE by the retired
/// `config mirror --profile`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct StoredProfileRisk {
    /// The profile's file name — `run-live.toml`.
    pub profile: String,
    /// Its `[risk]` rows, sorted by key.
    pub rows: Vec<ProfileRiskRow>,
}

/// What [`read_profile_risk`] found, and **why it found nothing when it found nothing** — the same
/// three-armed shape [`SettingsSource`] carries, and for the same reason: a box that has not
/// mirrored is the ordinary state, and an empty `Vec` would let a caller report an authoritative
/// nothing about a store it cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRiskSource {
    /// The table is there and these are its profiles (possibly none), sorted by name.
    Rows(Vec<StoredProfileRisk>),
    /// [`crate::Backend::Absent`] — no settings database on this box at all.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// A database this binary can read that predates the `profile_risk` table — a box migrated
    /// before 0057 Phase 2 and not mirrored since.
    TableAbsent {
        /// The database that was opened.
        path: PathBuf,
    },
}

impl ProfileRiskSource {
    /// The profiles, or `None` for every arm that has none.
    #[must_use]
    pub fn profiles(&self) -> Option<&[StoredProfileRisk]> {
        match self {
            ProfileRiskSource::Rows(p) => Some(p),
            ProfileRiskSource::NoDatabase { .. } | ProfileRiskSource::TableAbsent { .. } => None,
        }
    }
}

impl std::fmt::Display for ProfileRiskSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileRiskSource::Rows(p) => write!(
                f,
                "{} mirrored run profile(s) carrying {} `[risk]` row(s)",
                p.len(),
                p.iter().map(|x| x.rows.len()).sum::<usize>()
            ),
            ProfileRiskSource::NoDatabase { path } => {
                write!(
                    f,
                    "no settings database at {} — the profile file answers alone",
                    path.display()
                )
            }
            // ⚠ It named the RETIRED `profile_risk` table until the run profile's body moved onto
            // the profile plane. Its one constructor is now `vike-cli config show`'s projection of
            // `profile_store`, so the absence this reports is the PROFILE TABLES' — a store
            // written before they existed, which is a different fact from "this profile sets no
            // ceiling" and must not read as one.
            ProfileRiskSource::TableAbsent { path } => write!(
                f,
                "the settings database {} carries no profile tables yet — `vike-cli config \
                 bootstrap-run` is what creates them",
                path.display()
            ),
        }
    }
}

/// What [`write_profile_risk`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProfileRiskWritten {
    /// Rows this profile has after the write.
    pub rows: usize,
    /// Rows this profile had BEFORE it — so a mirror that REMOVED keys can say so.
    pub replaced: usize,
    /// `true` when this call created the table (a store migrated before it existed).
    pub table_created: bool,
}

/// **Read every mirrored run profile's `[risk]` rows from the database beside `settings_dir`.**
pub fn read_profile_risk_in(settings_dir: &Path) -> Result<ProfileRiskSource, DbError> {
    read_profile_risk(&crate::store_locator::db_path_in(settings_dir))
}

/// [`read_profile_risk_in`] over the database's own path — the PURE root, so every arm is reachable
/// from a test without a walk.
///
/// ⚠ **This is a DISCLOSURE read and its callers are PINNED** —
/// `crates/vike-ops/tests/settings_secrets/profile_risk_readers_gate.rs`. See this module's doc: the rows judge no
/// order, and a new caller that wanted them to would have to say so in that file.
pub fn read_profile_risk(db: &Path) -> Result<ProfileRiskSource, DbError> {
    if !database_present(db) {
        return Ok(ProfileRiskSource::NoDatabase { path: db.to_path_buf() });
    }
    let conn = open_for_read(db)?;
    if !table_exists(db, &conn, "profile_risk")? {
        return Ok(ProfileRiskSource::TableAbsent { path: db.to_path_buf() });
    }

    let mut out: Vec<StoredProfileRisk> = Vec::new();
    let mut stmt = conn
        .prepare("SELECT profile, key, value FROM profile_risk ORDER BY profile, key")
        .map_err(|e| DbError::sql(db, e))?;
    let rows = stmt
        .query_map([], |r| {
            let profile: String = r.get(0)?;
            Ok((profile, ProfileRiskRow { key: r.get(1)?, value: r.get(2)? }))
        })
        .map_err(|e| DbError::sql(db, e))?;
    for row in rows {
        let (profile, row) = row.map_err(|e| DbError::sql(db, e))?;
        match out.last_mut() {
            Some(last) if last.profile == profile => last.rows.push(row),
            _ => out.push(StoredProfileRisk { profile, rows: vec![row] }),
        }
    }
    Ok(ProfileRiskSource::Rows(out))
}

/// **Replace ONE profile's `[risk]` rows** in the database beside `settings_dir`.
pub fn write_profile_risk_in(
    settings_dir: &Path,
    profile: &StoredProfileRisk,
) -> Result<ProfileRiskWritten, DbError> {
    write_profile_risk(&crate::store_locator::db_path_in(settings_dir), profile)
}

/// **Replace ONE profile's `[risk]` rows**, in ONE transaction.
///
/// # Scoped to the named profile, and deliberately so
///
/// [`write_settings`] replaces the WHOLE of its two tables because its source is the whole of the
/// four files. This writer's source is ONE file, so it deletes and re-inserts one profile's rows
/// and leaves every other profile alone — a mirror of `run-live.toml` must not silently drop the
/// rows of `run-paper.toml`. Within that profile the replace rule is the same and for the same
/// reason: a key the file stopped setting is a key that stops being set, and a row nothing wrote
/// reads as more authoritative than the file line it outlived.
///
/// # ⚠ It refuses a project with no database
///
/// [`crate::DbErrorKind::NoSettingsDatabase`], exactly as [`write_settings`], and the reason is the
/// live gate rather than tidiness: see that variant.
pub fn write_profile_risk(
    db: &Path,
    profile: &StoredProfileRisk,
) -> Result<ProfileRiskWritten, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    let (mut conn, created) = open_for_write(db)?;
    if created {
        // Same hazard and same repair as `write_settings`: the file vanished between the probe and
        // the open, and an empty database makes `Backend` answer `Database` for every process on
        // the box forever.
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }

    let table_created = !table_exists(db, &conn, "profile_risk")?;
    let tx = conn.transaction().map_err(|e| DbError::sql(db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(db, e))?;
    let replaced = tx
        .execute("DELETE FROM profile_risk WHERE profile = ?1", rusqlite::params![profile.profile])
        .map_err(|e| DbError::sql(db, e))?;
    {
        let mut stmt = tx
            .prepare("INSERT INTO profile_risk (profile, key, value) VALUES (?1, ?2, ?3)")
            .map_err(|e| DbError::sql(db, e))?;
        for row in &profile.rows {
            stmt.execute(rusqlite::params![profile.profile, row.key, row.value])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(db, e))?;

    Ok(ProfileRiskWritten { rows: profile.rows.len(), replaced, table_created })
}
