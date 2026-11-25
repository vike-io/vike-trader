//! **Who may reach the hubs: the server's access list, this device's own key and the invitations**
//! (`docs/decisions/0288-every-device-signs-with-its-own-key-one-access-list-decides-who-and-a-guest-joins-by-one-invitation.md`,
//! items 2 and 7). Three tables of the ONE `vike.db`, a class of their own under
//! `docs/decisions/0051-node-keys-live-in-their-own-store.md` (they grow with the number of people, so they are not
//! `hub_key` rows):
//!
//! * `access_key` — the SERVER's list: one row per device a person reaches the hubs from, with its role and its 32
//!   PUBLIC bytes. A revoke stamps `revoked_ms` and the row stays; a revoked row is never re-granted.
//! * `device_key` — THIS device's own pair, as its PKCS#8 document. ⚠ The one secret here: never printed, logged or
//!   sent; [`DeviceKeyRow`]'s `Debug` shows the key id alone.
//! * `access_invite` — the server's invitations: `auth_secret` is `invite_auth_secret(secret)` (SHA-256), never the
//!   secret, and it registers once, before `expires_ms`. Checked when it is used; nothing here runs on a timer.
//!
//! # What this module does NOT check, and who does
//!
//! It is crypto-free and names no hub crate: `vike-hub-protocol` sits on this crate's own layer, so the shapes
//! (`valid_name`, `valid_role`, `valid_ssh_key`), the key id (`key_fingerprint` of the public bytes) and the proof
//! (`invite_proof_verifies`, handed in as a closure) are the CALLER's, in `crates/vike-hub-protocol/src/access.rs`.
//! The role column's `CHECK` is the one shape the store refuses itself.
//!
//! # Where it lives, and why it is safe in the key store
//!
//! `crate::link_sessions`' argument: `CREATE TABLE IF NOT EXISTS` by every writer here, no version stamp, the DDL out
//! of [`crate::DDL`], a store that does not exist is never created (a write is refused with
//! [`StoreErrorKind::NoSettingsDatabase`], a read answers nothing), and this module writes rows of these three tables
//! and of nothing else. No change-journal record here: the `access` record kind is the caller's (decision 0288 item 5).

use std::fmt;
use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::{StoreError, StoreErrorKind, database_present, open_for_read, open_for_write};
use crate::store_locator::db_path_in;

/// The three tables, verbatim decision 0288 item 2.
const ACCESS_DDL: &str = "
CREATE TABLE IF NOT EXISTS access_key (
    key_id     TEXT PRIMARY KEY,
    person     TEXT NOT NULL,
    device     TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('observe', 'control', 'admin')),
    public_key BLOB NOT NULL,
    granted_ms INTEGER NOT NULL,
    revoked_ms INTEGER
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS access_key_live ON access_key (person, device) WHERE revoked_ms IS NULL;

CREATE TABLE IF NOT EXISTS device_key (
    key_id      TEXT PRIMARY KEY,
    person      TEXT NOT NULL,
    device      TEXT NOT NULL,
    private_key BLOB NOT NULL,
    created_ms  INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS access_invite (
    id            TEXT PRIMARY KEY,
    person        TEXT NOT NULL,
    role          TEXT NOT NULL CHECK (role IN ('observe', 'control', 'admin')),
    auth_secret   BLOB NOT NULL,
    expires_ms    INTEGER NOT NULL,
    registered_ms INTEGER,
    key_id        TEXT,
    ssh_key       TEXT,
    swapped_ms    INTEGER
) STRICT;
";

/// One `access_key` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessKeyRow {
    /// `nk-…`, `key_fingerprint(public_key)`.
    pub key_id: String,
    /// Who.
    pub person: String,
    /// Which of their devices.
    pub device: String,
    /// `observe`, `control` or `admin`.
    pub role: String,
    /// The device's 32 public bytes.
    pub public_key: Vec<u8>,
    /// When it was granted, epoch-ms.
    pub granted_ms: i64,
    /// When it was revoked, epoch-ms; `None` while live.
    pub revoked_ms: Option<i64>,
}

/// One `device_key` row: THIS device's own pair. `Debug` shows no private byte.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceKeyRow {
    /// `nk-…` of its public half.
    pub key_id: String,
    /// Who holds this device.
    pub person: String,
    /// This device's name.
    pub device: String,
    /// The PKCS#8 v2 document. Never printed, logged or sent.
    pub private_key: Vec<u8>,
    /// When it was made, epoch-ms.
    pub created_ms: i64,
}

impl fmt::Debug for DeviceKeyRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceKeyRow({} {}/{})", self.key_id, self.person, self.device)
    }
}

/// One `access_invite` row as `access ls` shows it: no `auth_secret`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessInviteRow {
    /// The invitation's public id (8 lower-case hex).
    pub id: String,
    /// Who it is for.
    pub person: String,
    /// The role it grants.
    pub role: String,
    /// When it stops registering, epoch-ms.
    pub expires_ms: i64,
    /// When it registered a device, epoch-ms.
    pub registered_ms: Option<i64>,
    /// The device key it registered.
    pub key_id: Option<String>,
    /// When its one-time ssh line was swapped for the guest's own, epoch-ms.
    pub swapped_ms: Option<i64>,
}

/// What [`add_access_key`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessAdded {
    /// The row is in.
    Added,
    /// This very key is live already under this (person, device): nothing to do, and that is a success (a client
    /// re-registers on every connect).
    Held,
    /// A live row already holds this (person, device); nothing was written.
    DeviceTaken,
    /// A row (live or revoked) already holds this key id; nothing was written.
    KeyTaken,
}

/// What [`add_device_key`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAdded {
    /// The pair is in (and, when asked, its live `access_key` row).
    Added,
    /// This device already holds a key under that (person, device); nothing was written.
    Held,
    /// The access list holds a REVOKED row for that (person, device); nothing was written, never re-granted.
    Revoked,
    /// The access list holds a live row for that (person, device) under another key; nothing was written.
    DeviceTaken,
}

/// What [`register_access`] came to (decision 0288 item 7.3). Only [`RegisterOutcome::Registered`] wrote anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// The `access_key` row is in and the invitation is registered.
    Registered {
        /// The invitation's person.
        person: String,
        /// The invitation's role.
        role: String,
    },
    /// No invitation has this id (or the box has no store).
    Unknown,
    /// Its `expires_ms` has passed.
    Expired,
    /// It registered a device already: single use.
    AlreadyRegistered,
    /// The proof did not verify under its `auth_secret`.
    WrongProof,
    /// A live row holds (person, device), or a row holds this key id.
    DeviceTaken,
}

/// What [`swap_access_invite`] came to (decision 0288 item 7.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOutcome {
    /// Swapped now: the line to append is this device's, for this ssh key.
    Swapped {
        /// The invitation's person (the `vike-access:<person>/<device>` tag).
        person: String,
        /// The device it registered.
        device: String,
        /// The guest's own `ssh-ed25519 <base64>`.
        ssh_key: String,
    },
    /// No invitation has this id.
    Unknown,
    /// It has registered nothing yet.
    NotRegistered,
    /// It was swapped before.
    AlreadySwapped,
    /// The device it registered is revoked (alone, or with its whole person): no line is owed, nothing was written.
    Revoked,
}

/// An invitation as a registration reads it: person, role, `auth_secret`, `expires_ms`, `registered_ms`.
type InviteHead = (String, String, Vec<u8>, i64, Option<i64>);

/// An invitation as a swap reads it: person, `key_id`, `ssh_key`, `swapped_ms`.
type SwapHead = (String, Option<String>, Option<String>, Option<i64>);

/// One IMMEDIATE transaction on an EXISTING store, the three tables created first.
fn write_one<T>(
    settings_dir: &Path,
    body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
) -> Result<T, StoreError> {
    let db = db_path_in(settings_dir);
    let refused = |kind| StoreError { path: db.clone(), kind };
    let Some(mut conn) = open_for_write(&db)? else {
        return Err(refused(StoreErrorKind::NoSettingsDatabase));
    };
    let write = || -> rusqlite::Result<T> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(ACCESS_DDL)?;
        let out = body(&tx)?;
        tx.commit()?;
        Ok(out)
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

/// A read of an existing store whose `table` exists; `None` for no store or no table yet.
fn read_one<T>(
    settings_dir: &Path,
    table: &str,
    body: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<Option<T>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(None);
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Option<T>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?;
        if there { body(&conn).map(Some) } else { Ok(None) }
    };
    query().map_err(|e| StoreError::sql(&db, e))
}

const ACCESS_KEY_COLUMNS: &str = "key_id, person, device, role, public_key, granted_ms, revoked_ms";

fn access_key_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AccessKeyRow> {
    Ok(AccessKeyRow {
        key_id: r.get(0)?,
        person: r.get(1)?,
        device: r.get(2)?,
        role: r.get(3)?,
        public_key: r.get(4)?,
        granted_ms: r.get(5)?,
        revoked_ms: r.get(6)?,
    })
}

/// **The row of `key_id`**, revoked or not: the one read a hub's access list makes, afresh at every call. `None` for
/// no row, no table yet and a box with no store.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn find_access_key(
    settings_dir: &Path,
    key_id: &str,
) -> Result<Option<AccessKeyRow>, StoreError> {
    Ok(read_one(settings_dir, "access_key", |c| {
        c.query_row(
            &format!("SELECT {ACCESS_KEY_COLUMNS} FROM access_key WHERE key_id = ?1"),
            [key_id],
            access_key_row,
        )
        .optional()
    })?
    .flatten())
}

/// **Every `access_key` row**, by person, device, then grant time.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn list_access_keys(settings_dir: &Path) -> Result<Vec<AccessKeyRow>, StoreError> {
    Ok(read_one(settings_dir, "access_key", |c| {
        let mut st = c.prepare(&format!(
            "SELECT {ACCESS_KEY_COLUMNS} FROM access_key ORDER BY person, device, granted_ms"
        ))?;
        st.query_map([], access_key_row)?.collect()
    })?
    .unwrap_or_default())
}

/// **Every invitation**, newest first, without its `auth_secret`.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn list_access_invites(settings_dir: &Path) -> Result<Vec<AccessInviteRow>, StoreError> {
    Ok(read_one(settings_dir, "access_invite", |c| {
        let mut st = c.prepare(
            "SELECT id, person, role, expires_ms, registered_ms, key_id, swapped_ms \
             FROM access_invite ORDER BY expires_ms DESC, id",
        )?;
        st.query_map([], |r| {
            Ok(AccessInviteRow {
                id: r.get(0)?,
                person: r.get(1)?,
                role: r.get(2)?,
                expires_ms: r.get(3)?,
                registered_ms: r.get(4)?,
                key_id: r.get(5)?,
                swapped_ms: r.get(6)?,
            })
        })?
        .collect()
    })?
    .unwrap_or_default())
}

/// Whether a live row holds (person, device), or any row holds `key_id`.
fn taken(
    tx: &Transaction<'_>,
    person: &str,
    device: &str,
    key_id: &str,
) -> rusqlite::Result<Option<AccessAdded>> {
    let device_live: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM access_key WHERE person = ?1 AND device = ?2 AND revoked_ms IS NULL)",
        params![person, device],
        |r| r.get(0),
    )?;
    if device_live {
        return Ok(Some(AccessAdded::DeviceTaken));
    }
    let key_held: bool =
        tx.query_row("SELECT EXISTS(SELECT 1 FROM access_key WHERE key_id = ?1)", [key_id], |r| {
            r.get(0)
        })?;
    Ok(key_held.then_some(AccessAdded::KeyTaken))
}

fn insert_access_key(tx: &Transaction<'_>, row: &AccessKeyRow) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO access_key (key_id, person, device, role, public_key, granted_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![row.key_id, row.person, row.device, row.role, row.public_key, row.granted_ms],
    )?;
    Ok(())
}

/// **Grant one device**: insert `row` live (its `revoked_ms` is ignored), unless a live row holds its (person, device)
/// or any row holds its key id. The same key live under the same (person, device) is [`AccessAdded::Held`], a
/// success: its role stays (`access role` changes a role).
///
/// # Errors
/// [`StoreErrorKind::NoSettingsDatabase`] (none is created), a busy store, one at another schema, or a role outside
/// the table's `CHECK`.
pub fn add_access_key(settings_dir: &Path, row: &AccessKeyRow) -> Result<AccessAdded, StoreError> {
    write_one(settings_dir, |tx| {
        let same: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM access_key \
             WHERE key_id = ?1 AND person = ?2 AND device = ?3 AND revoked_ms IS NULL)",
            params![row.key_id, row.person, row.device],
            |r| r.get(0),
        )?;
        if same {
            return Ok(AccessAdded::Held);
        }
        if let Some(refused) = taken(tx, &row.person, &row.device, &row.key_id)? {
            return Ok(refused);
        }
        insert_access_key(tx, row)?;
        Ok(AccessAdded::Added)
    })
}

/// **Set the role of `person`'s live rows** (`device` narrows it to one). Returns how many rows changed; a revoked row
/// is never touched.
///
/// # Errors
/// As [`add_access_key`].
pub fn set_access_role(
    settings_dir: &Path,
    person: &str,
    device: Option<&str>,
    role: &str,
) -> Result<usize, StoreError> {
    write_one(settings_dir, |tx| {
        tx.execute(
            "UPDATE access_key SET role = ?3 \
             WHERE person = ?1 AND (?2 IS NULL OR device = ?2) AND revoked_ms IS NULL",
            params![person, device, role],
        )
    })
}

/// **Revoke `person`'s live rows** (`device` narrows it to one): `revoked_ms` = `now_ms`, the row kept. Returns how many
/// rows were revoked now. A whole person's open invitations expire now too, in the same transaction.
///
/// # Errors
/// As [`add_access_key`].
pub fn revoke_access(
    settings_dir: &Path,
    person: &str,
    device: Option<&str>,
    now_ms: i64,
) -> Result<usize, StoreError> {
    write_one(settings_dir, |tx| {
        if device.is_none() {
            // A person revoked whole: an invitation of theirs still open must not register a device afterwards.
            tx.execute(
                "UPDATE access_invite SET expires_ms = ?2 \
                 WHERE person = ?1 AND registered_ms IS NULL AND expires_ms > ?2",
                params![person, now_ms],
            )?;
        }
        tx.execute(
            "UPDATE access_key SET revoked_ms = ?3 \
             WHERE person = ?1 AND (?2 IS NULL OR device = ?2) AND revoked_ms IS NULL",
            params![person, device, now_ms],
        )
    })
}

/// **This device's own key**: the FIRST pair it stored (a desktop holds one; a server box's first is `owner/server`,
/// which `backend mint-missing` stores before any other), or `None` (no row, no table, no store).
///
/// # Errors
/// A store that exists and cannot be read.
pub fn device_key_load(settings_dir: &Path) -> Result<Option<DeviceKeyRow>, StoreError> {
    Ok(read_one(settings_dir, "device_key", |c| {
        c.query_row(
            "SELECT key_id, person, device, private_key, created_ms FROM device_key \
             ORDER BY rowid LIMIT 1",
            [],
            |r| {
                Ok(DeviceKeyRow {
                    key_id: r.get(0)?,
                    person: r.get(1)?,
                    device: r.get(2)?,
                    private_key: r.get(3)?,
                    created_ms: r.get(4)?,
                })
            },
        )
        .optional()
    })?
    .flatten())
}

/// **Whether this box's access list holds any row**, revoked ones included. A hub ALWAYS authenticates (decision 0288
/// item 8); on a list with no row it refuses every device as "no device registered" rather than a bare "access
/// denied". `false` for no row, no table or no store.
///
/// # Errors
/// A store that exists and cannot be read (a caller fails closed on it).
pub fn access_list_held(settings_dir: &Path) -> Result<bool, StoreError> {
    Ok(read_one(settings_dir, "access_key", |c| {
        c.query_row("SELECT EXISTS(SELECT 1 FROM access_key)", [], |r| r.get(0))
    })?
    .unwrap_or(false))
}

/// **A named device's own key on this box** — the key a DAEMON of this box signs with (decision 0288 item 8:
/// `owner/telegram`, `server/backtesthub`), where [`device_key_load`] answers the box's first, the operator's.
/// `None` when this box holds no key under (person, device).
///
/// # Errors
/// A store that exists and cannot be read.
pub fn device_key_named(
    settings_dir: &Path,
    person: &str,
    device: &str,
) -> Result<Option<DeviceKeyRow>, StoreError> {
    Ok(read_one(settings_dir, "device_key", |c| {
        c.query_row(
            "SELECT key_id, person, device, private_key, created_ms FROM device_key \
             WHERE person = ?1 AND device = ?2",
            params![person, device],
            |r| {
                Ok(DeviceKeyRow {
                    key_id: r.get(0)?,
                    person: r.get(1)?,
                    device: r.get(2)?,
                    private_key: r.get(3)?,
                    created_ms: r.get(4)?,
                })
            },
        )
        .optional()
    })?
    .flatten())
}

/// The key id this device holds under (person, device), if any.
fn device_held(
    tx: &Transaction<'_>,
    person: &str,
    device: &str,
) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT key_id FROM device_key WHERE person = ?1 AND device = ?2",
        params![person, device],
        |r| r.get(0),
    )
    .optional()
}

fn insert_device_key(
    tx: &Transaction<'_>,
    key_id: &str,
    person: &str,
    device: &str,
    pkcs8: &[u8],
) -> rusqlite::Result<()> {
    // The clock is the store's: `created_ms` is a label, and this crate reads no clock of its own.
    tx.execute(
        "INSERT INTO device_key (key_id, person, device, private_key, created_ms) \
         VALUES (?1, ?2, ?3, ?4, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))",
        params![key_id, person, device, pkcs8],
    )?;
    Ok(())
}

/// **Store this device's own pair** (its PKCS#8 document): [`DeviceAdded::Held`] and nothing written when it already
/// holds a key under (person, device).
///
/// # Errors
/// As [`add_access_key`].
pub fn device_key_insert(
    settings_dir: &Path,
    key_id: &str,
    person: &str,
    device: &str,
    pkcs8: &[u8],
) -> Result<DeviceAdded, StoreError> {
    write_one(settings_dir, |tx| {
        if device_held(tx, person, device)?.is_some() {
            return Ok(DeviceAdded::Held);
        }
        insert_device_key(tx, key_id, person, device, pkcs8)?;
        Ok(DeviceAdded::Added)
    })
}

/// **Store this device's own pair AND grant it on this box's access list**, in ONE transaction: the box is its own
/// server (`backend mint-missing`). Nothing is written when this device already holds a key under (person, device)
/// ([`DeviceAdded::Held`], or [`DeviceAdded::Revoked`] when that key's row is revoked), nor when the list holds any row
/// for that (person, device): an operator's revoke is never undone by a start.
///
/// # Errors
/// As [`add_access_key`].
pub fn device_key_grant(
    settings_dir: &Path,
    pkcs8: &[u8],
    row: &AccessKeyRow,
) -> Result<DeviceAdded, StoreError> {
    write_one(settings_dir, |tx| {
        if let Some(held) = device_held(tx, &row.person, &row.device)? {
            let revoked: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM access_key WHERE key_id = ?1 AND revoked_ms IS NOT NULL)",
                [held],
                |r| r.get(0),
            )?;
            return Ok(if revoked { DeviceAdded::Revoked } else { DeviceAdded::Held });
        }
        let listed: Option<bool> = tx.query_row(
            "SELECT max(revoked_ms IS NULL) FROM access_key WHERE person = ?1 AND device = ?2",
            params![row.person, row.device],
            |r| r.get(0),
        )?;
        match listed {
            Some(true) => return Ok(DeviceAdded::DeviceTaken),
            Some(false) => return Ok(DeviceAdded::Revoked),
            None => {}
        }
        insert_access_key(tx, row)?;
        insert_device_key(tx, &row.key_id, &row.person, &row.device, pkcs8)?;
        Ok(DeviceAdded::Added)
    })
}

/// **Add one invitation** for `person` at `role`, registering until `expires_ms`. `false` when the id is taken (mint
/// another); nothing is written then.
///
/// # Errors
/// As [`add_access_key`].
pub fn add_access_invite(
    settings_dir: &Path,
    id: &str,
    person: &str,
    role: &str,
    auth_secret: &[u8],
    expires_ms: i64,
) -> Result<bool, StoreError> {
    write_one(settings_dir, |tx| {
        let added = tx.execute(
            "INSERT INTO access_invite (id, person, role, auth_secret, expires_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (id) DO NOTHING",
            params![id, person, role, auth_secret, expires_ms],
        )?;
        Ok(added == 1)
    })
}

/// **An invitation's registration, the tradehub door's one call** (decision 0288 item 7.3), in ONE IMMEDIATE
/// transaction: the invitation exists, has not registered, `expires_ms` is ahead of `now_ms`, `proof_verifies` accepts
/// its `auth_secret`, and no live row holds (its person, `device`) nor any row `key_id`. Then the `access_key` row
/// (the invitation's person and role) goes in and the invitation records `registered_ms`, `key_id` and `ssh_key`.
///
/// ⚠ The caller has checked the shapes (`valid_name(device)`, `valid_ssh_key(ssh_key)`, 32 public bytes) and that
/// `key_id` is `key_fingerprint(public_key)`: the module doc says why this crate cannot. A box with no store has no
/// invitation. ⚠ A refusal writes nothing: the 256-bit secret, the 15 minutes and the single use are the limits.
///
/// # Errors
/// A store that exists and cannot be written.
pub fn register_access(
    settings_dir: &Path,
    invite: &str,
    device: &str,
    public_key: &[u8],
    key_id: &str,
    ssh_key: &str,
    now_ms: i64,
    proof_verifies: impl FnOnce(&[u8]) -> bool,
) -> Result<RegisterOutcome, StoreError> {
    if !database_present(&db_path_in(settings_dir))? {
        return Ok(RegisterOutcome::Unknown);
    }
    write_one(settings_dir, |tx| {
        let row: Option<InviteHead> = tx
            .query_row(
                "SELECT person, role, auth_secret, expires_ms, registered_ms FROM access_invite WHERE id = ?1",
                [invite],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((person, role, auth_secret, expires_ms, registered_ms)) = row else {
            return Ok(RegisterOutcome::Unknown);
        };
        if registered_ms.is_some() {
            return Ok(RegisterOutcome::AlreadyRegistered);
        }
        if expires_ms <= now_ms {
            return Ok(RegisterOutcome::Expired);
        }
        if !proof_verifies(&auth_secret) {
            return Ok(RegisterOutcome::WrongProof);
        }
        if taken(tx, &person, device, key_id)?.is_some() {
            return Ok(RegisterOutcome::DeviceTaken);
        }
        insert_access_key(
            tx,
            &AccessKeyRow {
                key_id: key_id.to_string(),
                person: person.clone(),
                device: device.to_string(),
                role: role.clone(),
                public_key: public_key.to_vec(),
                granted_ms: now_ms,
                revoked_ms: None,
            },
        )?;
        tx.execute(
            "UPDATE access_invite SET registered_ms = ?2, key_id = ?3, ssh_key = ?4 \
             WHERE id = ?1 AND registered_ms IS NULL",
            params![invite, now_ms, key_id, ssh_key],
        )?;
        Ok(RegisterOutcome::Registered { person, role })
    })
}

/// **Mark a registered invitation swapped** and answer the line it owes (decision 0288 item 7.4): refused unless it
/// registered, was not swapped before, and the device it registered is still live. That last check sits in the same
/// IMMEDIATE transaction as the stamp, so a revoke (of the device, or of the whole person, which revokes every live
/// device) that lands between the registration and the swap is never followed by a permanent line.
/// `vikectl access swap` appends the guest's permanent line from the answer.
///
/// # Errors
/// As [`add_access_key`].
pub fn swap_access_invite(
    settings_dir: &Path,
    id: &str,
    now_ms: i64,
) -> Result<SwapOutcome, StoreError> {
    write_one(settings_dir, |tx| {
        let row: Option<SwapHead> = tx
            .query_row(
                "SELECT person, key_id, ssh_key, swapped_ms FROM access_invite WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((person, key_id, ssh_key, swapped_ms)) = row else {
            return Ok(SwapOutcome::Unknown);
        };
        if swapped_ms.is_some() {
            return Ok(SwapOutcome::AlreadySwapped);
        }
        let (Some(key_id), Some(ssh_key)) = (key_id, ssh_key) else {
            return Ok(SwapOutcome::NotRegistered);
        };
        let live: Option<String> = tx
            .query_row(
                "SELECT device FROM access_key WHERE key_id = ?1 AND revoked_ms IS NULL",
                [key_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(device) = live else {
            return Ok(SwapOutcome::Revoked);
        };
        tx.execute("UPDATE access_invite SET swapped_ms = ?2 WHERE id = ?1", params![id, now_ms])?;
        Ok(SwapOutcome::Swapped { person, device, ssh_key })
    })
}
