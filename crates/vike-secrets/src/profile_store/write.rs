//! The write half: the operator capability and the verbs that store, select and clear profiles.

use std::path::Path;

use rusqlite::OptionalExtension;

use super::*;
use crate::db::{StoreError, open_for_write};

// ---------------------------------------------------------------------------------------------
// Writing — the operator's own act
// ---------------------------------------------------------------------------------------------

/// **The capability every write here requires.**
///
/// It is not a permission check and it does not pretend to be one: a type cannot tell which process
/// is holding it. What it is, is the thing that makes "only the operator's CLI writes profile rows"
/// a fact a GATE can hold rather than a convention a reviewer has to remember —
/// `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs` pins the files that may construct one, both
/// directions.
///
/// # ⚠ Why not a cargo feature, which is the obvious answer
///
/// MEASURED constraint, not a preference: a `profile-write` feature enabled only by `vike-cli` does
/// nothing under a workspace build. Resolver 2 unifies features across the whole selection, so
/// `cargo test --workspace` would enable it for every consumer at once and the daemon would compile
/// against the writers — the *light-consumers* failure class the root `CLAUDE.md` names, which has
/// already broken `main` once. A feature would make the seal LOOK structural in CI and be absent in
/// the one build that matters.
///
/// # What stops a daemon, and what stops the ONE daemon the kernel no longer stops
///
/// For every daemon but one: **the kernel.** Under `ProtectSystem=strict` a unit can write only
/// what its `ReadWritePaths` names, and `deploy/vike-datahub.service` names `settings/state/logs`
/// and its data root — so the recorder half of this module is unwritable from inside that
/// namespace whatever any Rust type says, and the GUI reaches the box only through a daemon.
///
/// ⚠ **This doc said that of `deploy/vike-tradehub.service` too — *"names `settings/state` alone"*
/// — and it stopped being true on 2026-09-18**, when the owner ruled the trading daemon must be
/// able to write the database and that unit's `ReadWritePaths` gained `settings/db`. The sentence
/// this paragraph replaced ended *"on the day that grant is paid"*; the grant is paid, and for that
/// one unit this type is now the whole seal rather than a second belt behind the kernel. Adding an
/// in-process writer there is no longer caught by anything but [`ProfileOperatorWrite`] and
/// `crates/vike-ops/tests/settings_secrets/profile_writer_gate.rs`.
#[derive(Debug, Clone)]
pub struct ProfileOperatorWrite {
    actor: String,
}

impl ProfileOperatorWrite {
    /// Claim the capability, naming the ACTOR the change journal will carry.
    ///
    /// `actor` is a human-meaningful label for who is writing (`"vike-cli profile activate"`), not
    /// a credential and not a user id — it lands in `updated_by` and is printed.
    #[must_use]
    pub fn claim(actor: &str) -> Self {
        ProfileOperatorWrite { actor: actor.to_string() }
    }

    /// The actor string this claim carries.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }
}

/// **Open the EXISTING store for a write, and refuse one that does not exist** — the open creates
/// nothing (`crate::db`'s `open_for_write`).
///
/// The first thing every write in this module does, and the single most important line here:
/// `crate::store::StoreBackend` decides which store answers credentials from ONE probe of this path,
/// so an empty database brought into existence by a profile write would silently become the credential
/// store, and every venue on the box would drop to paper.
///
/// It is also the reason nothing here stamps `PRAGMA user_version`: a store that already exists is
/// already stamped, so there is no half-finished state for this module to create or to repair.
fn open_existing_store(path: &Path) -> Result<rusqlite::Connection, ProfileStoreError> {
    open_for_write(path)?.ok_or_else(|| ProfileStoreError::NoStore { path: path.to_path_buf() })
}

/// The `mount` columns a store's table was born without — `strategy_file` (`profile_ddl`'s deviation
/// 6) and `max_leverage` (deviation 8), each with the declaration `profile_ddl` gives it, character for
/// character, in its order: `CREATE TABLE IF NOT EXISTS` never adds a column, so every profile writer
/// runs this right after the DDL. Adds the nullable columns and writes nothing: every existing row
/// reads `NULL`, a mount naming no file and stating no leverage cap.
fn ensure_added_mount_columns(
    conn: &rusqlite::Connection,
    path: &Path,
) -> Result<(), ProfileStoreError> {
    let has = |column: &str| {
        crate::settings::has_column(conn, "mount", column).map_err(|e| StoreError::sql(path, e))
    };
    if !has("strategy_file")? {
        conn.execute_batch("ALTER TABLE mount ADD COLUMN strategy_file TEXT;")
            .map_err(|e| StoreError::sql(path, e))?;
    }
    if !has("max_leverage")? {
        conn.execute_batch(
            "ALTER TABLE mount ADD COLUMN max_leverage REAL CHECK (max_leverage IS NULL OR max_leverage >= 1.0);",
        )
        .map_err(|e| StoreError::sql(path, e))?;
    }
    Ok(())
}

/// Create the profile tables if they are absent. Idempotent, and it does **not** touch
/// `PRAGMA user_version` — see this module's doc for why that is a safety property rather than an
/// omission.
///
/// # Errors
///
/// [`ProfileStoreError::Db`] when the store cannot be opened for writing — which on a deployed daemon's
/// box, run as the daemon, is exactly what should happen.
pub fn ensure_tables(
    path: &Path,
    _write: &ProfileOperatorWrite,
    asset_class_words: &[&str],
) -> Result<(), ProfileStoreError> {
    let conn = open_existing_store(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| StoreError::sql(path, e))?;
    ensure_added_mount_columns(&conn, path)?;
    Ok(())
}

/// Store ONE profile's body — the identity row, its mounts, its params and its settings — replacing
/// any body already stored under that name.
///
/// **It never sets `active`.** Storing a body is not selecting one, which is the separation 0057's
/// Phase 3 rests on and the reason a migration can run on a live box with nothing changing.
/// [`set_active`] is the separate, deliberate act.
///
/// ⚠ **…and "replacing any body already stored under that name" is fenced to the SAME KIND** —
/// [`ProfileStoreError::NameHeldByAnotherKind`] carries the argument and the message. The refusal lives
/// HERE, in the store, rather than in the CLI verb that reaches it: `profile.name` is one namespace
/// across all three kinds, the destruction is this function's own `DELETE` + `active`-preserving
/// re-`INSERT`, and putting the guard at the choke point covers the three callers that exist today
/// and every one that does not yet. The CLI's mirror ALSO refuses it a step earlier, so a
/// `--dry-run` cannot promise a write this would refuse; that pre-check builds this same error
/// value, so the words an operator reads have one spelling.
///
/// # Errors
///
/// [`ProfileStoreError::NameHeldByAnotherKind`] when the name is held by a profile of a different kind —
/// checked FIRST, inside the transaction, so nothing is deleted. [`ProfileStoreError::Db`] on any store
/// failure. The schema's own CHECKs refuse a malformed mount (two symbol spellings, two primaries)
/// before a row lands.
pub fn store_profile(
    path: &Path,
    profile: &ProfileRecord,
    write: &ProfileOperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileStoreError> {
    edit_profiles(path, write, now_utc, asset_class_words, |t| t.store(profile))
}

/// **ONE write transaction over the profile store**, handed to [`edit_profiles`]' closure: every
/// read and write made through it lands in ONE commit, or none does. It is how a verb that changes
/// a body and its selection (decision 0297: `profile create daemon` stores and activates) cannot
/// leave half of the change behind, and how it reads what it changes under the same lock it writes
/// with.
pub struct ProfileTxn<'a> {
    tx: rusqlite::Transaction<'a>,
    path: &'a Path,
    write: &'a ProfileOperatorWrite,
    now_utc: i64,
}

impl ProfileTxn<'_> {
    /// The body stored under `name`, as this transaction sees it; `None` when no profile holds it.
    ///
    /// # Errors
    ///
    /// [`ProfileStoreError::Db`] on a store failure, [`ProfileStoreError::UnreadableKind`] for a
    /// kind word this binary does not read.
    pub fn read(&self, name: &str) -> Result<Option<ProfileRecord>, ProfileStoreError> {
        super::read::read_one(&self.tx, self.path, name)
    }

    /// Store `profile`'s body, replacing the one under its name: [`store_profile`]'s rules (the
    /// `active` bit preserved and never set, a name held by another kind refused).
    ///
    /// # Errors
    ///
    /// [`store_profile`]'s.
    pub fn store(&self, profile: &ProfileRecord) -> Result<(), ProfileStoreError> {
        write_body(&self.tx, self.path, profile, self.write, self.now_utc)
    }

    /// Make `name` the ACTIVE profile of `kind`: [`set_active`]'s act, in this transaction.
    ///
    /// # Errors
    ///
    /// [`ProfileStoreError::Db`] on a store failure.
    pub fn set_active(&self, kind: ProfileKind, name: &str) -> Result<(), ProfileStoreError> {
        self.tx
            .execute(
                "UPDATE profile SET active = 0, updated_utc = ?2, updated_by = ?3 WHERE kind = ?1",
                rusqlite::params![kind.sql_word(), self.now_utc, self.write.actor()],
            )
            .map_err(|e| StoreError::sql(self.path, e))?;
        self.tx
            .execute(
                "UPDATE profile SET active = 1, updated_utc = ?2, updated_by = ?3 \
                 WHERE name = ?1 AND kind = ?4",
                rusqlite::params![name, self.now_utc, self.write.actor(), kind.sql_word()],
            )
            .map_err(|e| StoreError::sql(self.path, e))?;
        Ok(())
    }
}

/// **Run `edit` in ONE `IMMEDIATE` write transaction over the profile store**, and commit what it
/// did only when it answers `Ok`: an `Err` (its own refusal, [`ProfileStoreError::EditRefused`], or
/// a store failure inside it) rolls EVERY write it made back. The writers below are this with one
/// act inside; a verb that changes several bodies, or reads what it changes, does it all in one.
///
/// IMMEDIATE, not the default DEFERRED: these are read-then-write (the `active` bit is preserved
/// from the row already there), and `crate::db`'s `BUSY_TIMEOUT` doc records why a deferred
/// promotion is the one case SQLite refuses without consulting the busy handler at all.
///
/// # Errors
///
/// [`ProfileStoreError::NoStore`] for an absent store, `edit`'s own, and
/// [`ProfileStoreError::Db`] on a store failure.
pub fn edit_profiles<T>(
    path: &Path,
    write: &ProfileOperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
    edit: impl FnOnce(&ProfileTxn<'_>) -> Result<T, ProfileStoreError>,
) -> Result<T, ProfileStoreError> {
    let mut conn = open_existing_store(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| StoreError::sql(path, e))?;
    ensure_added_mount_columns(&conn, path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| StoreError::sql(path, e))?;
    let txn = ProfileTxn { tx, path, write, now_utc };
    let out = edit(&txn)?;
    txn.tx.commit().map_err(|e| StoreError::sql(path, e))?;
    Ok(out)
}

/// **Store ONE profile's body as an EDIT of the body stored under `name`, read under the write's
/// own lock.** `edit` gets the body the store holds now (or `None`), read inside the same
/// `IMMEDIATE` transaction the write commits, and answers the body to store, or `Ok(None)` for
/// nothing to write. Two edits of one body therefore cannot lose each other: the second waits on
/// the lock and edits what the first stored. A verb that reads with [`read_profiles`] and then calls
/// [`store_profile`] can lose a concurrent writer's edit, because `store_profile` replaces the whole
/// body (decision 0297: two writers share one daemon body, so that is now an ordinary case).
///
/// Every rule of [`store_profile`] holds: the `active` bit is preserved and never set, and a name
/// held by another kind is refused. Answers whether a body was written.
///
/// # Errors
///
/// [`ProfileStoreError::EditRefused`] with `edit`'s own words, nothing written (a body answered
/// under another name is refused the same way); otherwise [`store_profile`]'s.
pub fn update_profile(
    path: &Path,
    name: &str,
    write: &ProfileOperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
    edit: impl FnOnce(Option<ProfileRecord>) -> Result<Option<ProfileRecord>, String>,
) -> Result<bool, ProfileStoreError> {
    edit_profiles(path, write, now_utc, asset_class_words, |t| {
        let Some(body) = edit(t.read(name)?).map_err(ProfileStoreError::EditRefused)? else {
            return Ok(false);
        };
        if body.row.name != name {
            return Err(ProfileStoreError::EditRefused(format!(
                "an edit of profile `{name}` answered a body named `{}`. Nothing was written.",
                body.row.name
            )));
        }
        t.store(&body)?;
        Ok(true)
    })
}

/// The body replacement both writers run inside their transaction: the cross-kind refusal, the
/// preserved `active` bit, the `DELETE` and every `INSERT`.
fn write_body(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    profile: &ProfileRecord,
    write: &ProfileOperatorWrite,
    now_utc: i64,
) -> Result<(), ProfileStoreError> {
    // ⚠ PRESERVE the existing `active` bit across a body replacement. Re-storing a body is an edit
    // of WHAT a profile is, never of WHETHER it is the one running, and a re-store that silently
    // deactivated the live profile would be this module's own hazard wearing the other sign.
    //
    // ⚠ …and the KIND is read in the SAME statement, because preserving the bit across a CROSS-KIND
    // replacement is that hazard wearing the first sign: the bit would be inherited by a body the
    // operator never activated. The word is compared RAW rather than through `ProfileKind::parse` —
    // a row this binary cannot parse still holds the name, and a refusal that first had to
    // understand the squatter would let exactly the unreadable ones through.
    let held: Option<(String, i64)> = tx
        .query_row("SELECT kind, active FROM profile WHERE name = ?1", [&profile.row.name], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(|e| StoreError::sql(path, e))?;
    if let Some((held_kind, _)) = &held
        && held_kind != profile.row.kind.sql_word()
    {
        return Err(ProfileStoreError::NameHeldByAnotherKind {
            name: profile.row.name.clone(),
            held: held_kind.clone(),
            wanted: profile.row.kind,
        });
    }
    let active = held.map_or(i64::from(profile.row.active), |(_, a)| a);
    tx.execute("DELETE FROM profile WHERE name = ?1", [&profile.row.name])
        .map_err(|e| StoreError::sql(path, e))?;
    tx.execute(
        "INSERT INTO profile (name, kind, active, note, updated_utc, updated_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            profile.row.name,
            profile.row.kind.sql_word(),
            active,
            profile.row.note,
            now_utc,
            write.actor(),
        ],
    )
    .map_err(|e| StoreError::sql(path, e))?;
    // The retired `strategy_name`/`strategy_rhai` columns are not named, so a re-stored body leaves
    // them NULL: storing a profile again is what clears a row `vike-cli config check` refuses.
    for m in &profile.mounts {
        tx.execute(
            "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol, \
             interval, interval_ms, resolution_ts_ms, qty, half_spread, tick_size, seed_cash, \
             data_only, account, strategy_file, max_leverage) VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
             ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            rusqlite::params![
                profile.row.name,
                m.ord,
                i64::from(m.is_primary),
                m.venue,
                m.asset_class,
                m.symbol,
                m.interval,
                m.interval_ms,
                m.resolution_ts_ms,
                m.qty,
                m.half_spread,
                m.tick_size,
                m.seed_cash,
                m.data_only.map(i64::from),
                m.account,
                m.strategy_file,
                m.max_leverage,
            ],
        )
        .map_err(|e| StoreError::sql(path, e))?;
    }
    for ((ord, key), value) in &profile.params {
        tx.execute(
            "INSERT INTO mount_param (profile, mount_ord, key, value) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![profile.row.name, ord, key, value],
        )
        .map_err(|e| StoreError::sql(path, e))?;
    }
    for (p, value) in &profile.settings {
        tx.execute(
            "INSERT INTO profile_setting (profile, path, value, updated_utc, updated_by) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![profile.row.name, p, value, now_utc, write.actor()],
        )
        .map_err(|e| StoreError::sql(path, e))?;
    }
    // ⚠ The `DELETE FROM profile` above cascaded these away with everything else, so a body
    // replacement that drops a subscription genuinely drops it rather than leaving an orphan the
    // next read would feed to a venue.
    if let Some(body) = &profile.recorder {
        tx.execute(
            "INSERT INTO recorder (profile, store, interval_secs, min_parts, target_mb, \
             max_merge_rows, retention_days, alert_webhooks, alert_repeat_secs, \
             alert_series_prefix, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                profile.row.name,
                body.row.store,
                body.row.interval_secs,
                body.row.min_parts,
                body.row.target_mb,
                body.row.max_merge_rows,
                body.row.retention_days,
                body.row.alert_webhooks,
                body.row.alert_repeat_secs,
                body.row.alert_series_prefix,
                body.row.note,
            ],
        )
        .map_err(|e| StoreError::sql(path, e))?;
        for s in &body.subscriptions {
            tx.execute(
                "INSERT INTO subscription (profile, ord, venue, family, symbols, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![profile.row.name, s.ord, s.venue, s.family, s.symbols, s.note,],
            )
            .map_err(|e| StoreError::sql(path, e))?;
        }
    }
    Ok(())
}

/// **Select a profile: the deliberate act, and the only one that changes what this box trades.**
///
/// Clears the kind's previous active row in the same transaction, so the partial unique index can
/// never be the thing that reports a two-active state to an operator mid-edit.
///
/// # Errors
///
/// [`ProfileStoreError::Db`] on a store failure, which includes the read-only refusal a process inside
/// the daemon's namespace gets.
pub fn set_active(
    path: &Path,
    kind: ProfileKind,
    name: &str,
    write: &ProfileOperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileStoreError> {
    edit_profiles(path, write, now_utc, asset_class_words, |t| t.set_active(kind, name))
}

/// **Delete ONE stored profile**: its `profile` row and, by the schema's `ON DELETE CASCADE`, every
/// mount, parameter, setting, recorder row and subscription under it. No other profile, and no
/// table outside the profile family: `credential`, `account`, `hub_key` and `setting` are named by
/// no statement here.
///
/// [`delete_verdict`] is asked FIRST, inside the same `IMMEDIATE` transaction, so the row it judged
/// is the row deleted: the ACTIVE profile of a kind is never deleted, and neither is a name the
/// store does not hold as `kind`. It runs no DDL: a delete creates nothing.
///
/// # Errors
///
/// [`ProfileStoreError::NoStore`] for an absent store, [`delete_verdict`]'s two refusals, and
/// [`ProfileStoreError::Db`] on any store failure (a store with no profile tables among them).
pub fn delete_profile(
    path: &Path,
    kind: ProfileKind,
    name: &str,
    _write: &ProfileOperatorWrite,
) -> Result<(), ProfileStoreError> {
    let mut conn = open_existing_store(path)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| StoreError::sql(path, e))?;
    let held: Option<(String, i64)> = tx
        .query_row("SELECT kind, active FROM profile WHERE name = ?1", [name], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(|e| StoreError::sql(path, e))?;
    delete_verdict(kind, name, held.as_ref().map(|(word, active)| (word.as_str(), *active != 0)))?;
    tx.execute("DELETE FROM profile WHERE name = ?1", [name])
        .map_err(|e| StoreError::sql(path, e))?;
    tx.commit().map_err(|e| StoreError::sql(path, e))?;
    Ok(())
}

/// **The rule a profile delete answers to, written once**: [`delete_profile`] asks it inside its
/// transaction and `vike-cli config profile rm --dry-run` asks it of [`read_profiles`]' answer,
/// so a rehearsal cannot promise a delete the store would refuse. `held` is what the store holds
/// under `name` (its raw `kind` word, so an unparseable kind still holds the name, and its `active`
/// bit), or `None`.
///
/// # Errors
///
/// [`ProfileStoreError::NoSuchProfile`] when `name` is not stored as `kind`, and
/// [`ProfileStoreError::DeleteActive`] when it is the active profile of `kind`.
pub fn delete_verdict(
    kind: ProfileKind,
    name: &str,
    held: Option<(&str, bool)>,
) -> Result<(), ProfileStoreError> {
    match held {
        Some((word, false)) if word == kind.sql_word() => Ok(()),
        Some((word, true)) if word == kind.sql_word() => {
            Err(ProfileStoreError::DeleteActive { name: name.to_string(), kind })
        }
        other => Err(ProfileStoreError::NoSuchProfile {
            name: name.to_string(),
            kind,
            held: other.map(|(word, _)| word.to_string()),
        }),
    }
}

/// Deselect whatever is active for one kind, returning the box to the state every box is in today.
///
/// # Errors
///
/// [`ProfileStoreError::Db`] on a store failure.
pub fn clear_active(
    path: &Path,
    kind: ProfileKind,
    write: &ProfileOperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileStoreError> {
    let conn = open_existing_store(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| StoreError::sql(path, e))?;
    ensure_added_mount_columns(&conn, path)?;
    conn.execute(
        "UPDATE profile SET active = 0, updated_utc = ?2, updated_by = ?3 WHERE kind = ?1",
        rusqlite::params![kind.sql_word(), now_utc, write.actor()],
    )
    .map_err(|e| StoreError::sql(path, e))?;
    Ok(())
}
