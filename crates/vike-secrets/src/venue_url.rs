//! **`venue_url`: every venue address as a row, in the one settings database.**
//!
//! The owner ruled on 2026-11-09 that every address a venue is dialled at lives here, typed `live`
//! or `demo`, so a moved address is a row change plus a restart and never a code change. The
//! vocabulary, the release's seed and the ONE lookup are `vike_venues::venue_urls`'s; this module is
//! the store half: the table, its reader, the boot's seeding, the operator's two writers and the
//! daily probe's record.
//!
//! # How the rows reach a bridge
//!
//! `crate::venue_setting::load_venue_settings` reads them with the `venue_setting` rows and hands
//! each venue's addresses down inside its `VenueSettings` (`VenueSettings::urls`), so they reach a
//! bridge as `AdapterInputs::settings` and the gatewayhub's data side through the same map. A
//! library never opens the store for them.
//!
//! # What this module may write
//!
//! * **Rows of `venue_url` and nothing else.** It creates no database ([`seed_venue_urls_in`] and
//!   [`record_venue_url_probes`] are no-ops on a box with none; [`set_venue_url_in`] and
//!   [`reset_venue_url_in`] refuse one) and rewrites no other table.
//! * **The seed never touches an `operator` row.** A missing seed row is inserted; a `default` row
//!   whose url or note the release moved is updated; nothing is deleted. Seeding is an idempotent
//!   upsert of defaults, not a migration (`docs/decisions/0117-there-are-no-migrations.md`).
//! * **No version stamp**: `CREATE TABLE IF NOT EXISTS` and `crate::SCHEMA_VERSION` stays, for the
//!   reason `crate::link_sessions`' module doc gives. The DDL stays out of `crate::DDL` for the same
//!   reason `crate::profile_store`'s does.
//!
//! # `venue` is a word, not a link
//!
//! Every link inside this store is by number, but a `venue_url` row may name a history vendor
//! (`tardis`, `databento`, `yahoo`) that the `venue` table, a projection of `vike_model::VENUES`,
//! does not hold. So `venue` is the roster word or the vendor's name, like `link_session.route`: an
//! address a human reads, which no foreign key could serve.

use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};
use vike_venues::venue_urls::{
    UrlChannel, UrlProtocol, UrlRefusal, UrlSeed, UrlType, VENUE_URL_SEED, VenueUrls, check_url,
};

use crate::db::{StoreError, StoreErrorKind, database_present, open_for_read, open_for_write};
use crate::store_locator::db_path_in;

const VENUE_URL_DDL: &str = "
CREATE TABLE IF NOT EXISTS venue_url (
    venue     TEXT NOT NULL,
    type      TEXT NOT NULL,
    channel   TEXT NOT NULL,
    protocol  TEXT NOT NULL,
    product   TEXT NOT NULL DEFAULT '',
    url       TEXT NOT NULL,
    source    TEXT NOT NULL DEFAULT 'default',
    note      TEXT NOT NULL DEFAULT '',
    probed_ms INTEGER,
    probe_ok  INTEGER,
    probe     TEXT,
    PRIMARY KEY (venue, type, channel, protocol, product),
    CHECK (type IN ('live', 'demo')),
    CHECK (channel IN ('all', 'trade', 'user', 'data', 'history', 'login')),
    CHECK (protocol IN ('rest', 'ws', 'socket', 'stream')),
    CHECK (source IN ('default', 'operator')),
    CHECK (probe_ok IS NULL OR probe_ok IN (0, 1))
) STRICT;
";

/// The selected columns, in [`row_of`]'s order.
const COLUMNS: &str =
    "venue, type, channel, protocol, product, url, source, note, probed_ms, probe_ok, probe";

/// Who wrote a row's url: the release (`default`, moved by the next release's seed) or the operator
/// (`operator`, never touched by a seed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlSource {
    /// The release's seed value; the next release may move it.
    Default,
    /// `vike-cli config url set` wrote it; no seed touches it.
    Operator,
}

impl UrlSource {
    /// The stored word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UrlSource::Default => "default",
            UrlSource::Operator => "operator",
        }
    }
}

/// One row's key: `(venue, type, channel, protocol, product)`, the table's primary key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UrlKey {
    /// The roster word or a history vendor's name.
    pub venue: String,
    /// `live` or `demo`.
    pub ty: UrlType,
    /// What the address is for.
    pub channel: UrlChannel,
    /// How it is dialled.
    pub protocol: UrlProtocol,
    /// The venue's book or plane word; `""` for none.
    pub product: String,
}

impl UrlKey {
    /// The dotted spelling the change journal records and a refusal names:
    /// `venue_url.<venue>.<type>.<channel>.<protocol>[.<product>]`.
    #[must_use]
    pub fn dotted(&self) -> String {
        let UrlKey { venue, ty, channel, protocol, product } = self;
        let tail = if product.is_empty() { String::new() } else { format!(".{product}") };
        format!("venue_url.{venue}.{ty}.{channel}.{protocol}{tail}")
    }
}

/// What the daily probe last found at a row's url.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlProbe {
    /// When, epoch-ms.
    pub at_ms: i64,
    /// `Some(false)` when the row is DOWN by the gatewayhub probe's rule (a resource not 2xx/3xx;
    /// any other row with no DNS, TCP connect or TLS handshake): `vike-cli trade health` warns on it.
    /// `None` when no ACTIVE account dials the row: it is NOT IN USE, probed but never down (the
    /// column's NULL beside a `probed_ms`).
    pub ok: Option<bool>,
    /// What it found, in words (`200`, `TLS ok`, `connect failed: …`, `not in use (…); …`).
    pub words: String,
}

/// One `venue_url` row as a reader gets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueUrlRow {
    /// The key.
    pub key: UrlKey,
    /// The base address.
    pub url: String,
    /// Who wrote it.
    pub source: UrlSource,
    /// Why it is the way it is, for a human.
    pub note: String,
    /// What the daily probe last found; `None` before the first probe of this url.
    pub probe: Option<UrlProbe>,
}

/// The release's seed as rows: what a fresh box's table holds, and what a box with no table reads.
/// In the table's read order (venue, type, channel, protocol, product, as stored words), so the
/// seed and a seeded table read alike.
#[must_use]
pub fn seed_rows() -> Vec<VenueUrlRow> {
    let mut rows: Vec<VenueUrlRow> = VENUE_URL_SEED
        .iter()
        .map(|s| VenueUrlRow {
            key: UrlKey {
                venue: s.venue.to_string(),
                ty: s.ty,
                channel: s.channel,
                protocol: s.protocol,
                product: s.product.to_string(),
            },
            url: s.url.to_string(),
            source: UrlSource::Default,
            note: s.note.to_string(),
            probe: None,
        })
        .collect();
    // The stored words compared as bytes, which is SQLite's `BINARY` order.
    rows.sort_by(|a, b| key_params(&a.key).cmp(&key_params(&b.key)));
    rows
}

/// The release's seed row of `key`, or `None` for a key the seed does not carry.
#[must_use]
pub fn seed_of(key: &UrlKey) -> Option<&'static UrlSeed> {
    VENUE_URL_SEED.iter().find(|s| {
        s.venue == key.venue
            && s.ty == key.ty
            && s.channel == key.channel
            && s.protocol == key.protocol
            && s.product == key.product
    })
}

/// `venue`'s addresses among `rows`, as the lookup takes them.
#[must_use]
pub fn venue_urls_of(rows: &[VenueUrlRow], venue: &str) -> VenueUrls {
    VenueUrls::from_rows(
        venue,
        rows.iter().map(|r| {
            let k = &r.key;
            (k.venue.as_str(), k.ty, k.channel, k.protocol, k.product.as_str(), r.url.as_str())
        }),
    )
}

/// **Every `venue_url` row.** A box with no settings database, or a store no gatewayhub has seeded
/// yet (no table, or an empty one), reads the release's seed ([`seed_rows`]): exactly what the next
/// seeding would write.
///
/// # Errors
/// A store that exists and cannot be read, or a row a `CHECK` should have refused.
pub fn read_venue_urls(settings_dir: &Path) -> Result<Vec<VenueUrlRow>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db) {
        return Ok(seed_rows());
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Option<Vec<VenueUrlRow>>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'venue_url')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(None);
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM venue_url ORDER BY venue, type, channel, protocol, product"
        ))?;
        let rows = stmt.query_map([], row_of)?.collect::<rusqlite::Result<Vec<_>>>()?;
        // A table with no row was created by a write that ran before any seeding landed (a probe
        // record on a store that refused the seed): it holds nothing the seed would not, so it reads
        // as the seed. No writer deletes a seed row, so a seeded table is never empty.
        Ok((!rows.is_empty()).then_some(rows))
    };
    Ok(query().map_err(|e| StoreError::sql(&db, e))?.unwrap_or_else(seed_rows))
}

/// One selected row ([`COLUMNS`]); a word outside the vocabulary is a conversion failure.
fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<VenueUrlRow> {
    fn word<T: std::str::FromStr<Err = String>>(
        r: &rusqlite::Row<'_>,
        i: usize,
    ) -> rusqlite::Result<T> {
        let text: String = r.get(i)?;
        text.parse::<T>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into())
        })
    }
    let source = match r.get::<_, String>(6)?.as_str() {
        "operator" => UrlSource::Operator,
        _ => UrlSource::Default,
    };
    let probed_ms: Option<i64> = r.get(8)?;
    let probe_ok: Option<i64> = r.get(9)?;
    let probe_words: Option<String> = r.get(10)?;
    Ok(VenueUrlRow {
        key: UrlKey {
            venue: r.get(0)?,
            ty: word(r, 1)?,
            channel: word(r, 2)?,
            protocol: word(r, 3)?,
            product: r.get(4)?,
        },
        url: r.get(5)?,
        source,
        note: r.get(7)?,
        probe: probed_ms.map(|at_ms| UrlProbe {
            at_ms,
            ok: probe_ok.map(|v| v == 1),
            words: probe_words.unwrap_or_default(),
        }),
    })
}

/// How many rows a seeding wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Seeded {
    /// Seed rows the table did not hold.
    pub inserted: usize,
    /// `default` rows whose url or note the release moved.
    pub updated: usize,
}

/// **The boot's seeding: insert every seed row the table lacks, update every `default` row whose
/// url or note differs from the seed, and never touch an `operator` row.** One transaction;
/// idempotent (a second run writes nothing). A box with no settings database is left alone.
///
/// # Errors
/// The store will not open for writing, or the engine refused a statement. No row of the batch
/// landed.
pub fn seed_venue_urls_in(settings_dir: &Path) -> Result<Seeded, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db) {
        return Ok(Seeded::default());
    }
    in_write(&db, |tx| {
        let mut seeded = Seeded::default();
        for row in seed_rows() {
            let found: Option<(String, String, String)> = tx
                .query_row(
                    &format!("SELECT url, source, note FROM venue_url WHERE {KEY_IS}"),
                    key_params(&row.key),
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let moved = |(url, source, note): &(String, String, String)| {
                source == UrlSource::Default.as_str() && (*url != row.url || *note != row.note)
            };
            match &found {
                None => seeded.inserted += 1,
                Some(old) if moved(old) => seeded.updated += 1,
                Some(_) => continue,
            }
            write_url(tx, &row.key, &row.url, UrlSource::Default, &row.note)?;
        }
        Ok(seeded)
    })
}

/// Write one row's url, source and note; a url that moved forgets its last probe.
fn write_url(
    tx: &Transaction<'_>,
    key: &UrlKey,
    url: &str,
    source: UrlSource,
    note: &str,
) -> rusqlite::Result<()> {
    let (venue, ty, channel, protocol, product) = key_params(key);
    tx.execute(
        "INSERT INTO venue_url (venue, type, channel, protocol, product, url, source, note) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT (venue, type, channel, protocol, product) DO UPDATE SET \
         probed_ms = CASE WHEN venue_url.url = excluded.url THEN venue_url.probed_ms END, \
         probe_ok = CASE WHEN venue_url.url = excluded.url THEN venue_url.probe_ok END, \
         probe = CASE WHEN venue_url.url = excluded.url THEN venue_url.probe END, \
         url = excluded.url, source = excluded.source, note = excluded.note",
        params![venue, ty, channel, protocol, product, url, source.as_str(), note],
    )
    .map(drop)
}

/// **What refuses `url` as the new address of `key`**, judged against the venue's rows `rows` with
/// the candidate in place: the shape ([`check_url`]) and the guard (a demo `trade`/`user` address
/// that equals the venue's live one, for this key or any other demo key of the venue the candidate
/// would make equal). `None` when it may be written. The words never hold a credential: a url is
/// not one (a keyed RPC url belongs in the credential store).
#[must_use]
pub fn refusal_of(rows: &[VenueUrlRow], key: &UrlKey, url: &str) -> Option<String> {
    if let Err(why) = check_url(key.protocol, url) {
        return Some(why);
    }
    let mut candidate: Vec<VenueUrlRow> =
        rows.iter().filter(|r| r.key.venue == key.venue && r.key != *key).cloned().collect();
    candidate.push(VenueUrlRow {
        key: key.clone(),
        url: url.to_string(),
        source: UrlSource::Operator,
        note: String::new(),
        probe: None,
    });
    let urls = venue_urls_of(&candidate, &key.venue);
    candidate.iter().filter(|r| r.key.ty == UrlType::Demo).find_map(|r| {
        match urls.url(UrlType::Demo, r.key.channel, r.key.protocol, &r.key.product) {
            Err(refusal @ UrlRefusal::DemoIsLive { .. }) => Some(refusal.to_string()),
            _ => None,
        }
    })
}

/// [`set_venue_url_in`]'s and [`reset_venue_url_in`]'s `Err`: the write's refusal, paired with the
/// outcome of recording it in the change journal (`crate::VenueSettingRefusal`'s shape).
pub type VenueUrlRefusal = crate::VenueSettingRefusal;

/// **The operator's address write: `key` dials `url` from the next restart, `source = operator`**,
/// plus its record in the change journal (`file` = `venue_url`, the key's dotted spelling). `note`
/// replaces the row's note when given and keeps it otherwise. Returns the url it replaced.
///
/// ⚠ It judges nothing: the caller asks [`refusal_of`] first (`vike-cli config url set` does), as
/// `crate::set_venue_setting_in`'s caller asks the catalog. It creates no database.
///
/// # Errors
/// No database (`StoreErrorKind::NoDatabase`, with no journal outcome), or the engine; each paired with
/// the outcome of recording the refusal.
pub fn set_venue_url_in(
    settings_dir: &Path,
    key: &UrlKey,
    url: &str,
    note: Option<&str>,
    journal: crate::ChangeStamp,
) -> Result<(Option<String>, Option<crate::ChangeAppendError>), VenueUrlRefusal> {
    journalled(settings_dir, key, url, journal, |tx| {
        let previous = previous_of(tx, key)?;
        let note = match note {
            Some(n) => n.to_string(),
            None => tx
                .query_row(
                    &format!("SELECT note FROM venue_url WHERE {KEY_IS}"),
                    key_params(key),
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or_default(),
        };
        write_url(tx, key, url, UrlSource::Operator, &note)?;
        Ok(previous)
    })
}

/// **Back to the release's seed: `key` dials its seed url again, `source = default`**, plus its
/// record. A key the seed does not carry (an address the operator ADDED) is deleted: the release's
/// default for it is no row. Returns the url it replaced.
///
/// # Errors
/// As [`set_venue_url_in`].
pub fn reset_venue_url_in(
    settings_dir: &Path,
    key: &UrlKey,
    journal: crate::ChangeStamp,
) -> Result<(Option<String>, Option<crate::ChangeAppendError>), VenueUrlRefusal> {
    let seed = seed_of(key);
    let shown = seed.map_or("<no row>", |s| s.url);
    journalled(settings_dir, key, shown, journal, |tx| {
        let previous = previous_of(tx, key)?;
        match seed {
            Some(s) => write_url(tx, key, s.url, UrlSource::Default, s.note)?,
            None => {
                tx.execute(&format!("DELETE FROM venue_url WHERE {KEY_IS}"), key_params(key))?;
            }
        }
        Ok(previous)
    })
}

/// The WHERE clause of one key, over [`key_params`].
const KEY_IS: &str = "venue = ?1 AND type = ?2 AND channel = ?3 AND protocol = ?4 AND product = ?5";

fn key_params(key: &UrlKey) -> (&str, &str, &str, &str, &str) {
    (&key.venue, key.ty.as_str(), key.channel.as_str(), key.protocol.as_str(), &key.product)
}

fn previous_of(tx: &Transaction<'_>, key: &UrlKey) -> rusqlite::Result<Option<String>> {
    tx.query_row(&format!("SELECT url FROM venue_url WHERE {KEY_IS}"), key_params(key), |r| {
        r.get(0)
    })
    .optional()
}

/// One operator write and its journal record, the shape of `crate::set_venue_setting_in`. The
/// table is seeded first, so `config url ls` after the write shows every row.
fn journalled(
    settings_dir: &Path,
    key: &UrlKey,
    shown: &str,
    journal: crate::ChangeStamp,
    body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<Option<String>>,
) -> Result<(Option<String>, Option<crate::ChangeAppendError>), VenueUrlRefusal> {
    use vike_change_journal::{Change, ChangeOutcome};
    let db = db_path_in(settings_dir);
    if !database_present(&db) {
        return Err(Box::new((StoreError { path: db, kind: StoreErrorKind::NoDatabase }, None)));
    }
    let dotted = key.dotted();
    let result = seed_venue_urls_in(settings_dir).and_then(|_| in_write(&db, body));
    let change = match &result {
        Ok(previous) => Change::set_setting(
            ChangeOutcome::AppliedPendingRestart,
            journal.actor,
            "venue_url",
            &dotted,
            previous.as_deref(),
            shown,
        ),
        Err(e) => Change::set_setting(
            ChangeOutcome::Refused,
            journal.actor,
            "venue_url",
            &dotted,
            None,
            shown,
        )
        .with_reason(Some(&e.to_string())),
    };
    let journal_error =
        crate::store::append_beside(settings_dir, journal.process, journal.now_ms, &change);
    match result {
        Ok(previous) => Ok((previous, journal_error)),
        Err(e) => Err(Box::new((e, journal_error))),
    }
}

/// **The daily probe's record**: each `(key, the url it probed, what it found)`, written only where
/// the row still holds that url (a row moved since is not described by an old probe). A box with no
/// settings database is left alone.
///
/// # Errors
/// As [`seed_venue_urls_in`].
pub fn record_venue_url_probes(
    settings_dir: &Path,
    probes: &[(UrlKey, String, UrlProbe)],
) -> Result<(), StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db) {
        return Ok(());
    }
    in_write(&db, |tx| {
        for (key, url, probe) in probes {
            tx.execute(
                &format!(
                    "UPDATE venue_url SET probed_ms = ?6, probe_ok = ?7, probe = ?8 WHERE {KEY_IS} AND url = ?9"
                ),
                params![
                    key.venue,
                    key.ty.as_str(),
                    key.channel.as_str(),
                    key.protocol.as_str(),
                    key.product,
                    probe.at_ms,
                    probe.ok.map(i64::from),
                    probe.words,
                    url
                ],
            )?;
        }
        Ok(())
    })
}

/// One transaction over the table, created first; the store's own open (its modes, its journal mode,
/// its schema check). The caller has seen the database exists.
fn in_write<T>(
    db: &Path,
    body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
) -> Result<T, StoreError> {
    let (mut conn, created) = open_for_write(db)?;
    if created {
        // The file vanished between the probe and the open, and this open made it: never leave an
        // EMPTY database for `crate::store::StoreBackend` to mistake for a real store
        // (`crate::link_sessions`' writer has the same repair).
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(StoreError { path: db.to_path_buf(), kind: StoreErrorKind::NoDatabase });
    }
    let write = || -> rusqlite::Result<T> {
        let tx = conn.transaction()?;
        tx.execute_batch(VENUE_URL_DDL)?;
        let out = body(&tx)?;
        tx.commit()?;
        Ok(out)
    };
    write().map_err(|e| StoreError::sql(db, e))
}

#[path = "venue_url_tests.rs"]
#[cfg(test)]
mod tests;
