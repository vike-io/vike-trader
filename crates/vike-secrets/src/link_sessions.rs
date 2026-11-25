//! **`link_session`: one row per venue session, every venue in ONE table.** How long a session lived, how it ended and
//! how long until the next one opened, as data: until now those numbers lived in journal lines, which rotate.
//! No per-venue table and no per-venue column.
//!
//! # It lives in the settings database, by the owner's ruling, and this is how that stays safe
//!
//! The file is the owner's only copy of his live keys, so what this module may do is narrow:
//!
//! * **It writes and deletes rows of `link_session` and of nothing else.** It reads `venue` for the number. It rewrites,
//!   moves and truncates nothing. A store that does not exist is never created ([`write_link_events`] is a no-op on it).
//! * **It is not settings.** The seal counts the `setting` table alone (`crates/vike-config/src/mirror.rs`'s
//!   `adoption_integrity`), so a row here cannot break the seal or read as tampering, and `config check` never sees it.
//! * **No version stamp, on purpose**: `CREATE TABLE IF NOT EXISTS`, and [`crate::SCHEMA_VERSION`] stays. The check is an
//!   exact match, so a bump would make an older binary, a rollback's above all, refuse the whole store, and a root
//!   that degrades that refusal reads no keys. An older binary selects from its own tables and never asks what else is in
//!   the file (`crate::profile_store`'s module doc is the same argument; a box's store already carries tables the code
//!   stopped reading). This table's DDL stays out of [`crate::DDL`] for the same reason `profile_store`'s does.
//! * **A writer killed inside a commit costs no read and no key.** The next read replays its journal
//!   (`crates/vike-secrets/src/db/open.rs`'s `replay_hot_journal`); only a unit with no `settings/db` grant is refused, by name,
//!   until one that has it reads the store.
//! * **A write waits [`LINK_WRITE_BUSY`], then the caller loses the rows.** A settings read or a `config set` never waits
//!   long behind this: its transaction is one short batch. The caller runs on its own thread, never a venue's.
//!
//! # A link by number, an address by words
//!
//! `venue_id` is a link, by number and with a foreign key, as every link in this store is. A `venue` row is never deleted, so
//! that key can refuse nothing. `route` is the account's route key as it was that day: a human address, not a link. A
//! foreign key to `account` would make `account rm` refuse an account for having a history, and a lane twin
//! (`binance#PERP`) has no row of its own at all.
//!
//! # What a row may hold
//!
//! Never a URL and never a key: [`plain_words`] is the one door to the `how` column, whatever a bridge handed it. The
//! `listenKey` scrub of `crates/bridges/binance/src/family/execution/user_data.rs` reaches here already; this is the second lock.
//! A row is read back as [`vike_model::LinkSessionRow`], the same type an Observe key reads over the datahub's
//! `GatewayLinks` verb: a column added to the read is a field there, and that type's test pins the fields allowed.

use std::path::Path;
use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use vike_model::LinkSessionRow;

use crate::db::{StoreError, open_for_read, open_for_write_within};
use crate::store_locator::db_path_in;

/// A venue's private stream: the session its fills and venue-side cancels come back on, or its order session.
pub const PLANE_TRADE: &str = "trade";
/// A venue's price stream, per symbol and lane.
pub const PLANE_PRICE: &str = "price";
const PLANES: [&str; 2] = [PLANE_TRADE, PLANE_PRICE];

/// How long a row is kept. Deleted by the writer, and only this table's rows.
pub const LINK_SESSION_RETENTION_DAYS: i64 = 90;
/// The most rows one plane keeps, whatever their age: a socket that flaps for a season must not grow the key store. At
/// about 200 bytes a row this bounds the table near 20 MB; the CI box's two churning venues make about 370 rows a day.
pub const LINK_SESSION_ROW_CAP: i64 = 50_000;
/// How long a write waits for the store's lock before its rows are lost. A statement of who may wait: a settings
/// write waits for this batch, this batch does not wait for a settings write.
pub const LINK_WRITE_BUSY: Duration = Duration::from_millis(250);
/// The most rows one read answers, newest first.
pub const LINK_SESSIONS_READ_LIMIT: usize = 5_000;
/// The longest `how` kept.
const HOW_LIMIT: usize = 240;
/// The shortest word read as a key whatever it is made of: a listenKey is 64, and no word of a close reason is this long.
const KEY_SHAPED_WORD: usize = 24;
/// The shortest word of letters and digits together read as a key.
const KEY_SHAPED_MIX: usize = 10;
/// What stands where a word was dropped.
const DROPPED_WORD: &str = "<dropped>";
/// How far ahead of the newest row a clock may stand and still be believed by the retention. A bad RTC before NTP is the
/// case: the table's own rows say the clock jumped. A quiet box only postpones a prune until its next row.
const PRUNE_CLOCK_LEAD_DAYS: i64 = 7;

/// A session a process left open when it died: its end is known only to lie between its open and this time, the last this
/// table was written. A stop of ours, no drop, and of no known length, so it stays out of every life figure.
pub const LEFT_OPEN_WORDS: &str =
    "the gatewayhub stopped with this session open; it ended at or before this time";
/// A session still open when the gatewayhub stopped cleanly.
pub const HUB_STOP_WORDS: &str = "the gatewayhub stopped";
/// A session whose close never reached this table (a full queue, a busy store) and whose socket opened again: a drop, of no
/// known length, and this time is the new session's open.
pub const LOST_CLOSE_WORDS: &str = "its end was not recorded; the next session opened at this time";

const LINK_SESSION_DDL: &str = "
CREATE TABLE IF NOT EXISTS link_session (
    id          INTEGER PRIMARY KEY,
    venue_id    INTEGER NOT NULL REFERENCES venue(id),
    route       TEXT NOT NULL,
    plane       TEXT NOT NULL,
    lane        TEXT NOT NULL,
    opened_ms   INTEGER NOT NULL,
    closed_ms   INTEGER,
    lived_s     REAL,
    how         TEXT,
    ours        INTEGER NOT NULL DEFAULT 0,
    next_open_s REAL
) STRICT;
CREATE INDEX IF NOT EXISTS link_session_lane ON link_session (venue_id, route, plane, lane, id);
CREATE INDEX IF NOT EXISTS link_session_opened ON link_session (opened_ms);
";

/// A [`LinkKey`], as `?1` to `?4` of a statement.
const OF_KEY: &str =
    "venue_id = (SELECT id FROM venue WHERE name = ?1) AND route = ?2 AND plane = ?3 AND lane = ?4";

/// Which socket a row is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkKey {
    /// The venue's roster word. A venue with no `venue` row yet writes no row.
    pub venue: String,
    /// The account's route key (`bybit#DEMO`); on the price plane, the symbol.
    pub route: String,
    /// [`PLANE_TRADE`] or [`PLANE_PRICE`].
    pub plane: &'static str,
    /// Which socket of that route: a pump's thread name, or a price stream's lane word.
    pub lane: String,
}

/// One transition of one socket: all a bridge ever says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// A session opened. Said again of a session still open, it changes nothing: a status can repeat.
    Opened {
        /// The socket.
        key: LinkKey,
        /// When, epoch-ms.
        at_ms: i64,
    },
    /// The open session ended.
    Closed {
        /// The socket.
        key: LinkKey,
        /// When, epoch-ms.
        at_ms: i64,
        /// How, in the bridge's words: [`plain_words`] judges them.
        how: String,
        /// We ended it (a stop, a restart), so it is no drop.
        ours: bool,
    },
}

/// **The one door to the `how` column: never a URL, never a key.** A whitelist: a word stays only if it reads as a word, so a
/// shape nobody thought of is dropped where it stands, and the rest of the reason still reads. Bounded.
#[must_use]
pub fn plain_words(how: &str) -> String {
    let words: Vec<&str> = how.split_whitespace().map(plain_word).collect();
    words.join(" ").chars().take(HOW_LIMIT).collect()
}

/// `/`, `?`, `&`, `=`, `@` and `%` are what a URL, a query and a credential are made of, and they stay out of the set: a word
/// holding one goes, whatever it says. The one exception is a named number (`code=1006`).
fn plain_word(word: &str) -> &str {
    let count = word.chars().count();
    let has = |test: fn(&char) -> bool| word.chars().any(|c| test(&c));
    let mixed = has(char::is_ascii_alphabetic) && has(char::is_ascii_digit);
    let reads_as_a_word =
        word.chars().all(|c| c.is_ascii_alphanumeric() || ".,:;!'\"()[]<>-_#".contains(c))
            && count < KEY_SHAPED_WORD
            && !(mixed && count >= KEY_SHAPED_MIX);
    let named_number = word.split_once('=').is_some_and(|(name, value)| {
        count < KEY_SHAPED_WORD
            && name.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
            && !value.is_empty()
            && value.chars().all(|c| c.is_ascii_digit())
    });
    if reads_as_a_word || named_number { word } else { DROPPED_WORD }
}

/// Write `events` in ONE short transaction. A store that is not there is not created, and is no error: nothing here may
/// make a box look configured.
///
/// # Errors
/// The store is busy past [`LINK_WRITE_BUSY`], at another schema, or will not open. No row of the batch landed.
pub fn write_link_events(settings_dir: &Path, events: &[LinkEvent]) -> Result<(), StoreError> {
    in_write(settings_dir, |tx| {
        for event in events {
            apply(tx, event)?;
        }
        Ok(())
    })
}

/// The writer's last act at a clean stop: `events`, and every row still open ended now, in ONE transaction, bounded by the
/// queues that held them.
///
/// # Errors
/// As [`write_link_events`].
pub fn flush_link_events(
    settings_dir: &Path,
    events: &[LinkEvent],
    now_ms: i64,
) -> Result<(), StoreError> {
    in_write(settings_dir, |tx| {
        for event in events {
            apply(tx, event)?;
        }
        tx.execute(
            "UPDATE link_session SET closed_ms = ?1, lived_s = max(0.0, (?1 - opened_ms) / 1000.0), how = ?2, ours = 1 \
             WHERE closed_ms IS NULL",
            params![now_ms, HUB_STOP_WORDS],
        )
        .map(drop)
    })
}

/// Close every row a process that never said goodbye left open, before any new session attaches to one: at the last time
/// this table was written, the latest it can vouch for, and of no known length ([`LEFT_OPEN_WORDS`]).
///
/// # Errors
/// As [`write_link_events`]. The caller tries again: a new session must not meet these rows.
pub fn close_left_open(settings_dir: &Path) -> Result<(), StoreError> {
    in_write(settings_dir, |tx| {
        tx.execute(
            "UPDATE link_session SET how = ?1, ours = 1, closed_ms = \
             (SELECT max(max(opened_ms, coalesce(closed_ms, opened_ms))) FROM link_session) \
             WHERE closed_ms IS NULL",
            [LEFT_OPEN_WORDS],
        )
        .map(drop)
    })
}

/// Delete what [`LINK_SESSION_RETENTION_DAYS`] and [`LINK_SESSION_ROW_CAP`] say is over. This table's rows, nothing else. The
/// age half stands down while `now_ms` is more than [`PRUNE_CLOCK_LEAD_DAYS`] ahead of the newest row; the cap reads no clock.
///
/// # Errors
/// As [`write_link_events`].
pub fn prune_link_sessions(settings_dir: &Path, now_ms: i64) -> Result<(), StoreError> {
    use vike_model::MS_PER_DAY;
    in_write(settings_dir, |tx| {
        let newest: Option<i64> =
            tx.query_row("SELECT max(opened_ms) FROM link_session", [], |r| r.get(0))?;
        if newest.is_some_and(|newest| now_ms - newest <= PRUNE_CLOCK_LEAD_DAYS * MS_PER_DAY) {
            let cutoff = now_ms - LINK_SESSION_RETENTION_DAYS * MS_PER_DAY;
            tx.execute("DELETE FROM link_session WHERE opened_ms < ?1", [cutoff])?;
        }
        for plane in PLANES {
            tx.execute(
                "DELETE FROM link_session WHERE plane = ?1 AND id <= \
                 (SELECT id FROM link_session WHERE plane = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2)",
                params![plane, LINK_SESSION_ROW_CAP],
            )?;
        }
        Ok(())
    })
}

/// `IMMEDIATE`, so a writer that cannot have the lock learns it now and within the budget; `open_for_write_within` is the
/// store's own open: its modes, its journal mode and its schema check.
fn in_write(
    settings_dir: &Path,
    body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<()>,
) -> Result<(), StoreError> {
    write_into(&db_path_in(settings_dir), body)
}

fn write_into(
    db: &Path,
    body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<()>,
) -> Result<(), StoreError> {
    // No store: nothing to write, and the open creates none.
    let Some(mut conn) = open_for_write_within(db, LINK_WRITE_BUSY)? else { return Ok(()) };
    let write = || -> rusqlite::Result<()> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(LINK_SESSION_DDL)?;
        body(&tx)?;
        tx.commit()
    };
    write().map_err(|e| StoreError::sql(db, e))
}

fn apply(tx: &Transaction<'_>, event: &LinkEvent) -> rusqlite::Result<()> {
    match event {
        LinkEvent::Opened { key, at_ms } => {
            let k = params![key.venue, key.route, key.plane, key.lane, at_ms];
            let open: Option<i64> = tx
                .query_row(
                    &format!("SELECT id FROM link_session WHERE {OF_KEY} AND closed_ms IS NULL LIMIT 1"),
                    params![key.venue, key.route, key.plane, key.lane],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = open {
                // A socket reports one open and one close, so this row's close was lost: a drop, and two sessions.
                tx.execute(
                    "UPDATE link_session SET closed_ms = ?2, how = ?3 WHERE id = ?1",
                    params![id, at_ms, LOST_CLOSE_WORDS],
                )?;
            } else {
                tx.execute(
                    &format!(
                        "UPDATE link_session SET next_open_s = max(0.0, (?5 - closed_ms) / 1000.0) WHERE id = \
                         (SELECT max(id) FROM link_session WHERE {OF_KEY})"
                    ),
                    k,
                )?;
            }
            tx.execute(
                "INSERT INTO link_session (venue_id, route, plane, lane, opened_ms) \
                 SELECT id, ?2, ?3, ?4, ?5 FROM venue WHERE name = ?1",
                k,
            )
            .map(drop)
        }
        LinkEvent::Closed { key, at_ms, how, ours } => tx
            .execute(
                &format!(
                    "UPDATE link_session SET closed_ms = ?5, lived_s = max(0.0, (?5 - opened_ms) / 1000.0), how = ?6, ours = ?7 \
                     WHERE id = (SELECT max(id) FROM link_session WHERE {OF_KEY}) AND closed_ms IS NULL"
                ),
                params![key.venue, key.route, key.plane, key.lane, at_ms, plain_words(how), ours],
            )
            .map(drop),
    }
}

/// The sessions open at any time since `since_ms` (one with no recorded end counts), newest first, at most
/// [`LINK_SESSIONS_READ_LIMIT`]; `route` narrows to one account and `venue` to one venue's accounts and feeds. A store with no
/// such table yet has no rows. Called by
/// [`crate::CredentialStore::link_sessions`].
pub(crate) fn read_link_sessions(
    db: &Path,
    since_ms: i64,
    route: Option<&str>,
    venue: Option<&str>,
) -> Result<Vec<LinkSessionRow>, StoreError> {
    let conn = open_for_read(db)?;
    let query = || -> rusqlite::Result<Vec<LinkSessionRow>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'link_session')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        let mut rows = conn.prepare(
            "SELECT v.name, s.route, s.plane, s.lane, s.opened_ms, s.closed_ms, s.lived_s, s.how, s.ours, s.next_open_s \
             FROM link_session s JOIN venue v ON v.id = s.venue_id \
             WHERE (s.closed_ms IS NULL OR s.closed_ms >= ?1) AND (?2 IS NULL OR s.route = ?2) \
             AND (?4 IS NULL OR v.name = ?4) ORDER BY s.id DESC LIMIT ?3",
        )?;
        let limit = i64::try_from(LINK_SESSIONS_READ_LIMIT).unwrap_or(i64::MAX);
        rows.query_map(params![since_ms, route, limit, venue], |r| {
            Ok(LinkSessionRow {
                venue: r.get(0)?,
                route: r.get(1)?,
                plane: r.get(2)?,
                lane: r.get(3)?,
                opened_ms: r.get(4)?,
                closed_ms: r.get(5)?,
                lived_s: r.get(6)?,
                how: r.get(7)?,
                ours: r.get(8)?,
                next_open_s: r.get(9)?,
            })
        })?
        .collect()
    };
    query().map_err(|e| StoreError::sql(db, e))
}

#[cfg(test)]
mod tests;
