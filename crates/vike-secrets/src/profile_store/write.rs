//! The write half: the operator capability and the verbs that store, select and clear profiles.

use std::path::Path;

use rusqlite::OptionalExtension;

use super::*;
use crate::db::{DbError, database_present, open_for_write};

// ---------------------------------------------------------------------------------------------
// Writing — the operator's own act
// ---------------------------------------------------------------------------------------------

/// **The capability every write here requires.**
///
/// It is not a permission check and it does not pretend to be one: a type cannot tell which process
/// is holding it. What it is, is the thing that makes "only the operator's CLI writes profile rows"
/// a fact a GATE can hold rather than a convention a reviewer has to remember —
/// `crates/vike-ops/tests/settings/profile_writer_gate.rs` pins the files that may construct one, both
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
/// in-process writer there is no longer caught by anything but [`OperatorWrite`] and
/// `crates/vike-ops/tests/settings/profile_writer_gate.rs`.
#[derive(Debug, Clone)]
pub struct OperatorWrite {
    actor: String,
}

impl OperatorWrite {
    /// Claim the capability, naming the ACTOR the change journal will carry.
    ///
    /// `actor` is a human-meaningful label for who is writing (`"vike-cli profile activate"`), not
    /// a credential and not a user id — it lands in `updated_by` and is printed.
    #[must_use]
    pub fn claim(actor: &str) -> Self {
        OperatorWrite { actor: actor.to_string() }
    }

    /// The actor string this claim carries.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }
}

/// **Refuse a write against a store that does not exist**, rather than letting `open_for_write`
/// create one.
///
/// Called first by every write in this module, and it is the single most important line here.
/// `crate::db`'s `open_for_write` CREATES the database when the path is absent — which is correct
/// for the migration that owns creating it and catastrophic for anything else, because
/// `crate::store::Backend` decides which store answers credentials from ONE `is_file` on this path.
/// An empty database brought into existence by a profile write would silently become the credential
/// store, the file beside it would stop being read, and every venue on the box would drop to paper
/// with `secrets.env` looking perfectly correct.
///
/// It is also the reason nothing here stamps `PRAGMA user_version`: a store that already exists is
/// already stamped, so there is no half-finished state for this module to create or to repair.
fn refuse_absent_store(path: &Path) -> Result<(), ProfileError> {
    if database_present(path) {
        return Ok(());
    }
    Err(ProfileError::NoStore { path: path.to_path_buf() })
}

/// Create the profile tables if they are absent. Idempotent, and it does **not** touch
/// `PRAGMA user_version` — see this module's doc for why that is a safety property rather than an
/// omission.
///
/// # Errors
///
/// [`ProfileError::Db`] when the store cannot be opened for writing — which on a deployed daemon's
/// box, run as the daemon, is exactly what should happen.
pub fn ensure_tables(
    path: &Path,
    _write: &OperatorWrite,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
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
/// [`ProfileError::NameHeldByAnotherKind`] carries the argument and the message. The refusal lives
/// HERE, in the store, rather than in the CLI verb that reaches it: `profile.name` is one namespace
/// across all three kinds, the destruction is this function's own `DELETE` + `active`-preserving
/// re-`INSERT`, and putting the guard at the choke point covers the three callers that exist today
/// and every one that does not yet. The CLI's mirror ALSO refuses it a step earlier, so a
/// `--dry-run` cannot promise a write this would refuse; that pre-check builds this same error
/// value, so the words an operator reads have one spelling.
///
/// # Errors
///
/// [`ProfileError::NameHeldByAnotherKind`] when the name is held by a profile of a different kind —
/// checked FIRST, inside the transaction, so nothing is deleted. [`ProfileError::Db`] on any store
/// failure. The schema's own CHECKs refuse a malformed mount (two symbol spellings, two primaries)
/// before a row lands.
pub fn store_profile(
    path: &Path,
    profile: &StoredProfile,
    write: &OperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (mut conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    // IMMEDIATE, not the default DEFERRED: this is a read-then-write (the `active` bit is preserved
    // from the row already there), and `crate::db`'s `BUSY_TIMEOUT` doc records why a deferred
    // promotion is the one case SQLite refuses without consulting the busy handler at all.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;
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
        .map_err(|e| DbError::sql(path, e))?;
    if let Some((held_kind, _)) = &held
        && held_kind != profile.row.kind.sql_word()
    {
        return Err(ProfileError::NameHeldByAnotherKind {
            name: profile.row.name.clone(),
            held: held_kind.clone(),
            wanted: profile.row.kind,
        });
    }
    let active = held.map_or(i64::from(profile.row.active), |(_, a)| a);
    tx.execute("DELETE FROM profile WHERE name = ?1", [&profile.row.name])
        .map_err(|e| DbError::sql(path, e))?;
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
    .map_err(|e| DbError::sql(path, e))?;
    for m in &profile.mounts {
        tx.execute(
            "INSERT INTO mount (profile, ord, is_primary, venue, asset_class, symbol, token_id, \
             interval, interval_ms, resolution_ts_ms, qty, half_spread, tick_size, seed_cash, \
             data_only, account, strategy_name, strategy_rhai) VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
             ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                profile.row.name,
                m.ord,
                i64::from(m.is_primary),
                m.venue,
                m.asset_class,
                m.symbol,
                m.token_id,
                m.interval,
                m.interval_ms,
                m.resolution_ts_ms,
                m.qty,
                m.half_spread,
                m.tick_size,
                m.seed_cash,
                m.data_only.map(i64::from),
                m.account,
                m.strategy_name,
                m.strategy_rhai,
            ],
        )
        .map_err(|e| DbError::sql(path, e))?;
    }
    for ((ord, key), value) in &profile.params {
        tx.execute(
            "INSERT INTO mount_param (profile, mount_ord, key, value) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![profile.row.name, ord, key, value],
        )
        .map_err(|e| DbError::sql(path, e))?;
    }
    for (p, value) in &profile.settings {
        tx.execute(
            "INSERT INTO profile_setting (profile, path, value, updated_utc, updated_by) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![profile.row.name, p, value, now_utc, write.actor()],
        )
        .map_err(|e| DbError::sql(path, e))?;
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
        .map_err(|e| DbError::sql(path, e))?;
        for s in &body.subscriptions {
            tx.execute(
                "INSERT INTO subscription (profile, ord, venue, family, symbols, backfill, note) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    profile.row.name,
                    s.ord,
                    s.venue,
                    s.family,
                    s.symbols,
                    s.backfill,
                    s.note,
                ],
            )
            .map_err(|e| DbError::sql(path, e))?;
        }
    }
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// **Select a profile: the deliberate act, and the only one that changes what this box trades.**
///
/// Clears the kind's previous active row in the same transaction, so the partial unique index can
/// never be the thing that reports a two-active state to an operator mid-edit.
///
/// # Errors
///
/// [`ProfileError::Db`] on a store failure, which includes the read-only refusal a process inside
/// the daemon's namespace gets.
pub fn set_active(
    path: &Path,
    kind: ProfileKind,
    name: &str,
    write: &OperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (mut conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;
    tx.execute(
        "UPDATE profile SET active = 0, updated_utc = ?2, updated_by = ?3 WHERE kind = ?1",
        rusqlite::params![kind.sql_word(), now_utc, write.actor()],
    )
    .map_err(|e| DbError::sql(path, e))?;
    tx.execute(
        "UPDATE profile SET active = 1, updated_utc = ?2, updated_by = ?3 \
         WHERE name = ?1 AND kind = ?4",
        rusqlite::params![name, now_utc, write.actor(), kind.sql_word()],
    )
    .map_err(|e| DbError::sql(path, e))?;
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(())
}

/// Deselect whatever is active for one kind, returning the box to the state every box is in today.
///
/// # Errors
///
/// [`ProfileError::Db`] on a store failure.
pub fn clear_active(
    path: &Path,
    kind: ProfileKind,
    write: &OperatorWrite,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), ProfileError> {
    refuse_absent_store(path)?;
    let (conn, _created, _version) = open_for_write(path)?;
    conn.execute_batch(&profile_ddl(asset_class_words)?).map_err(|e| DbError::sql(path, e))?;
    conn.execute(
        "UPDATE profile SET active = 0, updated_utc = ?2, updated_by = ?3 WHERE kind = ?1",
        rusqlite::params![kind.sql_word(), now_utc, write.actor()],
    )
    .map_err(|e| DbError::sql(path, e))?;
    Ok(())
}
