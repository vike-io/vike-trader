//! The read half: presence probes and the readers that load every stored profile.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::OptionalExtension;

use super::*;
use crate::db::{DbError, database_present, open_for_read};

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// The tables the PRESENCE PROBE requires, which is a SMALLER set and deliberately so.
///
/// ⚠ **Widening the probe to every table in [`PROFILE_TABLES`] would have been a silent
/// regression, and it is the first thing the recorder tables nearly broke.** `read_profiles` uses
/// the probe to tell "this store predates the profile phase" from "this store has an empty profile
/// table", and answers [`Profiles::none`] for the first. A store written before 2026-09-16 holds
/// exactly these four — so a probe demanding six would report every already-migrated box as
/// un-migrated, and its daemon and run profiles would vanish from the read path with no error
/// anywhere. Two boxes are in that state today.
///
/// So the probe keeps the four that define the phase, and [`read_recorder`] asks `sqlite_master`
/// for its own table instead of assuming it. `store_profile` and `ensure_tables` execute the whole
/// DDL (`CREATE TABLE IF NOT EXISTS`), so the first write upgrades an old store to all six.
const CORE_PROFILE_TABLES: [&str; 4] = ["profile", "mount", "mount_param", "profile_setting"];

/// Do the profile tables exist in this store?
///
/// The probe that lets [`read_profiles`] answer [`Profiles::none`] for a store that predates
/// Phase 3 instead of erroring — which is the difference between "this box has not migrated" and
/// "this box is broken", and the second answer would be false.
fn profile_tables_present(conn: &rusqlite::Connection, path: &Path) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
             ('profile', 'mount', 'mount_param', 'profile_setting')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(path, e))?;
    Ok(usize::try_from(found).unwrap_or(0) == CORE_PROFILE_TABLES.len())
}

/// Does this store carry the RECORDER tables? Asked rather than assumed — see
/// [`CORE_PROFILE_TABLES`] for why the main presence probe may not require them.
fn recorder_tables_present(conn: &rusqlite::Connection, path: &Path) -> Result<bool, DbError> {
    let found: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN \
             ('recorder', 'subscription')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| DbError::sql(path, e))?;
    Ok(found == 2)
}

/// **Every profile the store holds.** The whole read half of Phase 3, and it needs no unit change:
/// 0057's EROFS section measures that `ProtectSystem=strict` leaves reads untouched and that the
/// composition root already opens this store read-only at mount.
///
/// Three states collapse to [`Profiles::none`] — no database, no profile tables, no rows — because
/// the daemon must behave identically in all three and today.
///
/// # Errors
///
/// [`ProfileError::Db`] when the store exists and cannot be read (never when it is absent, which is
/// the ordinary unconfigured state), and [`ProfileError::UnreadableKind`] for a `kind` word outside
/// the three [`ProfileKind`] parses.
///
/// ⚠ **That clause named `recorder` as the unreadable kind and had been wrong since 2026-09-16**,
/// when the owner overruled 0057's NO: `recorder` is read here like any other kind, and this entry
/// point's whole job is handing the recorder body to the two sides of
/// [`Profiles::resolve_active`]'s contract.
pub fn read_profiles(path: &Path) -> Result<Profiles, ProfileError> {
    if !database_present(path) {
        return Ok(Profiles::none());
    }
    let (conn, _version) = open_for_read(path)?;
    if !profile_tables_present(&conn, path)? {
        return Ok(Profiles::none());
    }
    let mut rows: Vec<ProfileRow> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT name, kind, active, note FROM profile ORDER BY name")
            .map_err(|e| DbError::sql(path, e))?;
        let mapped = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| DbError::sql(path, e))?;
        for row in mapped {
            let (name, kind, active, note) = row.map_err(|e| DbError::sql(path, e))?;
            rows.push(ProfileRow { name, kind: ProfileKind::parse(&kind)?, active, note });
        }
    }
    let mut out = Vec::new();
    for row in rows {
        let mounts = read_mounts(&conn, path, &row.name)?;
        let params = read_params(&conn, path, &row.name)?;
        let settings = read_settings(&conn, path, &row.name)?;
        let recorder = read_recorder(&conn, path, &row.name)?;
        out.push(StoredProfile { row, mounts, params, settings, recorder });
    }
    Ok(Profiles { profiles: out, tables_present: true })
}

/// The `recorder` row and its subscriptions, or `None` when this profile has no recorder body.
///
/// ⚠ A profile of any kind is asked, not just [`ProfileKind::Recorder`]: the absence is read off
/// the TABLE rather than inferred from the kind word, so a row written under the wrong kind is
/// visible to a caller instead of being silently unreadable.
fn read_recorder(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<Option<RecorderBody>, DbError> {
    if !recorder_tables_present(conn, path)? {
        return Ok(None);
    }
    let row: Option<RecorderRow> = conn
        .query_row(
            "SELECT store, interval_secs, min_parts, target_mb, max_merge_rows, retention_days, \
             alert_webhooks, alert_repeat_secs, alert_series_prefix, note FROM recorder \
             WHERE profile = ?1",
            [profile],
            |r| {
                Ok(RecorderRow {
                    store: r.get(0)?,
                    interval_secs: r.get(1)?,
                    min_parts: r.get(2)?,
                    target_mb: r.get(3)?,
                    max_merge_rows: r.get(4)?,
                    retention_days: r.get(5)?,
                    alert_webhooks: r.get(6)?,
                    alert_repeat_secs: r.get(7)?,
                    alert_series_prefix: r.get(8)?,
                    note: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|e| DbError::sql(path, e))?;
    let Some(row) = row else { return Ok(None) };
    let mut stmt = conn
        .prepare(
            "SELECT ord, venue, family, symbols, backfill, note FROM subscription \
             WHERE profile = ?1 ORDER BY ord",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| {
            Ok(SubscriptionRow {
                ord: r.get(0)?,
                venue: r.get(1)?,
                family: r.get(2)?,
                symbols: r.get(3)?,
                backfill: r.get(4)?,
                note: r.get(5)?,
            })
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut subscriptions = Vec::new();
    for s in mapped {
        subscriptions.push(s.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(Some(RecorderBody { row, subscriptions }))
}

fn read_mounts(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<Vec<MountRow>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT ord, is_primary, venue, asset_class, symbol, token_id, interval, \
             interval_ms, resolution_ts_ms, qty, half_spread, tick_size, seed_cash, data_only, \
             account, strategy_name, strategy_rhai FROM mount WHERE profile = ?1 ORDER BY ord",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| {
            Ok(MountRow {
                ord: r.get(0)?,
                is_primary: r.get::<_, i64>(1)? != 0,
                venue: r.get(2)?,
                asset_class: r.get(3)?,
                symbol: r.get(4)?,
                token_id: r.get(5)?,
                interval: r.get(6)?,
                interval_ms: r.get(7)?,
                resolution_ts_ms: r.get(8)?,
                qty: r.get(9)?,
                half_spread: r.get(10)?,
                tick_size: r.get(11)?,
                seed_cash: r.get(12)?,
                data_only: r.get::<_, Option<i64>>(13)?.map(|v| v != 0),
                account: r.get(14)?,
                strategy_name: r.get(15)?,
                strategy_rhai: r.get(16)?,
            })
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(out)
}

fn read_params(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<BTreeMap<(i64, String), String>, DbError> {
    let mut stmt = conn
        .prepare(
            "SELECT mount_ord, key, value FROM mount_param WHERE profile = ?1 \
             ORDER BY mount_ord, key",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (ord, key, value) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert((ord, key), value);
    }
    Ok(out)
}

fn read_settings(
    conn: &rusqlite::Connection,
    path: &Path,
    profile: &str,
) -> Result<BTreeMap<String, String>, DbError> {
    let mut stmt = conn
        .prepare("SELECT path, value FROM profile_setting WHERE profile = ?1 ORDER BY path")
        .map_err(|e| DbError::sql(path, e))?;
    let mapped = stmt
        .query_map([profile], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| DbError::sql(path, e))?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (p, v) = row.map_err(|e| DbError::sql(path, e))?;
        out.insert(p, v);
    }
    Ok(out)
}
