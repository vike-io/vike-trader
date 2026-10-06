//! The account plane's store doors: `account`/`venue` readers and the routed account writers.

use super::*;

/// **Which accounts the store that ANSWERS for `settings_dir` holds** — the account-table twin of
/// [`resolve_store_in`], for a caller that already holds the settings DIRECTORY.
///
/// It asks the same [`backend_in`] the credential reader and the credential writer both ask, so the
/// three cannot disagree about which store is live. There is no `table` parameter: an account
/// belongs to the credential plane by definition, and `crate::db::Table::NodeKey`'s namespace holds
/// a pair of deployment keys that belong to no account at all.
///
/// # ⚠ The `Backend::Files` answer, and why it is not an empty list
///
/// A box with no settings database has no `account` table on it, and its credentials are perfectly
/// present under their legacy key names. Answering `Known(vec![])` there would tell every caller
/// *this store has no accounts* about a store that has sixteen of them — the shape of the failure
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §1 is about, and one that would
/// read downstream as *this venue has no credentials*. So the answer is
/// [`crate::db::NoAccountTable::FileStore`], carrying the credential file that IS answering:
/// `vike_model::accounts::account_keys::accounts_in_store` is the reader for that store, and it is the one
/// every caller uses today.
///
/// That arm is reached whether or not the credential file exists. An ABSENT file is the live gate
/// (no credentials ⇒ every venue stays paper) and is not this function's question; what it reports
/// is that the DATABASE is not what answers here, which is true either way.
///
/// # It is FALLIBLE, and has no infallible twin on purpose
///
/// `vike_bridge_core::credentials::load_workspace_secrets_at` may swallow its error into an empty
/// map because an empty credential map degrades SAFELY — it is the live gate. An empty account list
/// carries no such guarantee: it is an assertion about the store, not a refusal to arm. So a store
/// that exists and will not open comes back as [`SecretsError`] and the caller decides, exactly as
/// `vike_bridge_core::credentials::try_load_workspace_secrets_at` does for the map.
pub fn resolve_accounts_in(settings_dir: &Path) -> Result<crate::db::Accounts, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => Ok(crate::db::read_accounts(&db)?),
        Backend::Files => {
            Ok(crate::db::Accounts::Unanswerable(crate::db::NoAccountTable::FileStore {
                file: crate::dotenv::secrets_path_in(settings_dir),
            }))
        }
    }
}

/// [`resolve_accounts_in`] over the project walk — the account twin of [`resolve_project`], for a
/// caller that holds the `VIKE_SETTINGS_DIR` override as a `&str` and nothing else.
///
/// Same derivation as every other front door here: `settings_dir` is the override or `None`, and
/// the walk answers when it is `None`.
pub fn resolve_accounts(settings_dir: Option<&str>) -> Result<crate::db::Accounts, SecretsError> {
    resolve_accounts_in(&crate::dotenv::workspace_settings_dir_from(settings_dir))
}

/// **The credential key NAMES each account row owns, routed to the store that answers** — the
/// companion [`resolve_accounts_in`] needs before a human can ACT on its rows.
///
/// [`crate::db::AccountKeys`] carries the argument: two rows of `(dukascopy, demo, NULL)` render
/// identically except for an opaque `id`, and the thing that tells them apart —
/// `DUKASCOPY_DEMO1_*` versus `DUKASCOPY_DEMO2_*` — lives in the `credential` table rather than in
/// the `account` one. Values are never selected; see that type.
///
/// # `None` means EXACTLY what [`Accounts::Unanswerable`] means, and a caller may not merge them
///
/// `None` is *this store has no `account` table to key* — a [`Backend::Files`] box, where the
/// accounts live in the key names and `vike_model::accounts::account_keys::accounts_in_store` is the reader
/// that applies. `Some(map)` is *the table is there*, and a row absent from the map is an account
/// with no live credential row naming it, which is a real state and not a missing answer.
///
/// [`backend_in`] on the same `settings_dir`, so this and [`resolve_accounts_in`] cannot disagree
/// about which store they are describing.
///
/// [`Accounts::Unanswerable`]: crate::db::Accounts::Unanswerable
pub fn resolve_account_keys_in(
    settings_dir: &Path,
) -> Result<Option<std::collections::BTreeMap<i64, crate::db::AccountKeys>>, SecretsError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => Ok(Some(crate::db::read_account_keys(&db)?)),
        Backend::Files => Ok(None),
    }
}

/// [`resolve_account_keys_in`] over the project walk — the twin [`resolve_accounts`] is to
/// [`resolve_accounts_in`], for a caller that holds the `VIKE_SETTINGS_DIR` override and nothing
/// else.
///
/// It exists because the two readers are used TOGETHER and neither is useful alone for the case
/// they were built for: the dukascopy mount has to turn an `account` row into a broker, and
/// the row's own cells cannot do it (both dukascopy rows are `(dukascopy, demo, NULL)`) — the
/// discriminator is the owner PREFIX of its credential names, which only this reader carries. A
/// caller reaching one front door over the walk and the other over a hand-built path could open two
/// different stores; same derivation, same override, so these two cannot.
///
/// # Errors
/// [`SecretsError`] when a store that exists will not open.
pub fn resolve_account_keys(
    settings_dir: Option<&str>,
) -> Result<Option<std::collections::BTreeMap<i64, crate::db::AccountKeys>>, SecretsError> {
    resolve_account_keys_in(&crate::dotenv::workspace_settings_dir_from(settings_dir))
}

/// **Every row of the `venue` table in the database beside `settings_dir`** — [`crate::db::read_venues`]
/// over [`crate::dotenv::db_path_in`], the venue-table twin of [`resolve_accounts_in`] for a caller
/// that already holds the settings DIRECTORY. Read-only: an absent database, or one that predates
/// the table, answers an empty list, and the read creates nothing.
///
/// ⚠ **It lives HERE and not beside `read_venues`, and the reason is a gate.**
/// `crates/vike-secrets/src/db.rs` is the module that defines `migrate`, and
/// `crates/vike-ops/tests/credentials/credential_source_roster_gate/gate.rs`'s
/// `the_migration_reads_exactly_the_sources_the_roster_names` treats every path-resolver call in
/// that module as a path the migration reads or writes. A READER that resolved a path there would
/// read as a source of the migration. This file already resolves the path for every other `_in`
/// reader, so the one call belongs beside them.
///
/// # Errors
/// [`crate::DbError`] when a database that exists will not read.
pub fn read_venues_in(settings_dir: &Path) -> Result<Vec<crate::db::VenueRow>, crate::db::DbError> {
    crate::db::read_venues(&crate::dotenv::db_path_in(settings_dir))
}

/// **Write ONE account row's `venue_account_id`, routed to the store that actually answers** — the
/// account-row twin of [`save_credentials_to_store`], and the write twin of [`resolve_accounts_in`].
///
/// # The Backend decision is the SAME one, asked the same way
///
/// [`backend_in`], on the same `settings_dir` the reader is given — so the verb that LISTS the
/// account rows and the verb that writes one cannot disagree about which store they are talking
/// about. There is no second probe and no second path derivation.
///
/// # ⚠ A `Backend::Files` box is REFUSED, not silently no-op'd
///
/// A file store has no `account` table: on that box the accounts live in the credential key NAMES
/// and `vike_model::accounts::account_keys::accounts_in_store` is the reader that applies. There is nothing
/// here to write and nowhere to write it, so the answer is
/// [`crate::DbErrorKind::NoDatabase`] — loud, naming the file that IS answering and the migration
/// that would move it. **It is emphatically not a per-key fallback**: [`Backend`]'s whole rule is
/// that the choice is per RUN, and a writer that quietly stored the book somewhere else on an
/// unmigrated box would be inventing exactly the ladder
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids.
///
/// It creates no database either, on that path or any other: [`crate::migrate`] is still the only
/// function in this crate that may bring one into existence, and `set_venue_account_id`'s own
/// `created` arm is the belt behind this probe for the case where the file is removed in between.
///
/// # What this is NOT
///
/// It is not §11 step 3's FOLD of the ten stored book keys.
///
/// ⚠ It read *"and it is not the venue handshake's write path"* until 2026-09-15, and that half is
/// now false: the handshake fold reaches the store through this same router, passing
/// [`crate::BookSource::Handshake`] where the operator door passes [`crate::BookSource::Operator`].
/// One router, one writer, told which claim it is recording —
/// `crates/vike-ops/tests/credentials/credential_writer_gate.rs` is why a second function was not the answer.
/// `crate::db::set_venue_account_id`'s own doc separates the three sources; the short version is
/// that the OPERATOR door is for a book that is in NO store and derivable from nothing — which
/// today is dukascopy's two demo accounts and nothing else.
///
/// Nothing here writes, moves or deletes either credential FILE, and no `credential` row is read or
/// touched.
///
/// # `venue_account_id: None` is a CLEAR
///
/// It puts the column back to `NULL` — *not yet known* — and it is the repair a pair of rows
/// written the wrong way round needs, because correcting either one alone is refused by ruling 11's
/// index in both directions. `crate::db::set_venue_account_id`'s CLEAR section is the argument. It
/// reaches the same store through the same [`backend_in`] and refuses a [`Backend::Files`] box
/// identically: clearing a column that does not exist is not a thing to succeed quietly at.
pub fn set_venue_account_id_in(
    settings_dir: &Path,
    id: i64,
    venue_account_id: Option<&str>,
    replace: bool,
    source: crate::db::BookSource<'_>,
) -> Result<crate::db::BookWrite, crate::db::DbError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => {
            crate::db::set_venue_account_id(&db, id, venue_account_id, replace, source)
        }
        Backend::Files => Err(crate::db::DbError {
            path: crate::dotenv::db_path_in(settings_dir),
            kind: crate::db::DbErrorKind::NoDatabase {
                file: crate::dotenv::secrets_path_in(settings_dir),
            },
        }),
    }
}

/// **The account LIFECYCLE, routed to the store that actually answers** — create / rename /
/// (de)activate / remove, the sibling of [`set_venue_account_id_in`] and the write twin of
/// [`resolve_accounts_in`].
///
/// # The Backend decision is the SAME one, asked the same way
///
/// [`backend_in`], on the same `settings_dir` the reader is given — so the verb that LISTS the
/// account rows and the verb that edits one cannot disagree about which store they are talking
/// about. There is no second probe and no second path derivation.
///
/// # ⚠ A `Backend::Files` box is REFUSED, not silently no-op'd
///
/// A file store has no `account` table: on that box the accounts live in the credential key NAMES
/// and `vike_model::accounts::account_keys::accounts_in_store` is the reader that applies. There is nothing
/// here to write and nowhere to write it, so the answer is [`crate::DbErrorKind::NoDatabase`] —
/// loud, naming the file that IS answering and the migration that would move it. It is emphatically
/// not a per-key fallback: [`Backend`]'s whole rule is that the choice is per RUN.
///
/// # ⚠ It CREATES NO DATABASE, and that is reason 1 of `docs/decisions/0036` at its sharpest
///
/// The mere EXISTENCE of `<project>/settings/db/vike.db` is the whole of [`Backend`]'s per-run
/// choice, so an account verb that created one would make every credential in `secrets.env` unread
/// on that box in the same act — the LIVE GATE, silently, from a command about filing.
/// `vike-cli secrets migrate` stays the one creator; the probe above and
/// [`crate::db::edit_account`]'s own `created` rollback are the two layers that hold it.
///
/// Nothing here writes, moves or deletes either credential FILE, and no `credential` row is written
/// or its value read — [`crate::db::edit_account`]'s *what it never does* section is the contract.
///
/// # Errors
/// [`crate::DbError`] for every refusal [`crate::db::edit_account`] states, and for a
/// `Backend::Files` box.
pub fn edit_account_in(
    settings_dir: &Path,
    edit: crate::db::AccountEdit<'_>,
) -> Result<crate::db::AccountWrite, crate::db::DbError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => crate::db::edit_account(&db, edit),
        Backend::Files => Err(crate::db::DbError {
            path: crate::dotenv::db_path_in(settings_dir),
            kind: crate::db::DbErrorKind::NoDatabase {
                file: crate::dotenv::secrets_path_in(settings_dir),
            },
        }),
    }
}

/// **WHO is writing a store edit that carries no credential value — an account-lifecycle edit, or a
/// venue setting — and when.** The twin of [`CredentialJournal`], for the same reason:
/// [`edit_account_in_journalled`] and [`crate::settings::set_venue_setting_in_journalled`] cannot
/// derive either cell from the edit itself.
#[derive(Debug, Clone)]
pub struct AccountJournal {
    /// WHO is writing.
    pub actor: Actor,
    /// The writing process's identity. See [`CredentialJournal::proc`] for why this is a parameter.
    pub proc: Proc,
    /// The instant to stamp the record with.
    pub now_ms: i64,
}

/// **[`edit_account_in`], plus its durable [`Change::account_lifecycle`] record — together**, the
/// account-plane twin of [`save_credentials_to_store_journalled`] and the collapse of what used to
/// be `vike-connections`' `edit_account_journalled`.
///
/// # What it never carries
///
/// **No credential value, on any path** — [`edit_account_in`] never selects a `value` column, and
/// `Change::account_lifecycle` takes no value parameter, which is the enforcement rather than a
/// convention. The key NAMES that DO reach the record are recorded for the reason
/// [`Change::credential_write`]'s doc gives: they are not secret, and *"what did that account own
/// when it was deactivated"* is unanswerable without them.
///
/// # Ordering, and the two failure paths
///
/// The row write happens FIRST and its error returns unchanged — an edit that did not land must not
/// leave a record saying it did. Nothing to record when nothing CHANGED: a ledger line for a no-op
/// reads as an edit that did not happen. A JOURNAL failure, by contrast, cannot fail the call: the
/// row IS written, and it comes back as `Some(JournalAppendError)` for the caller to log.
///
/// # Errors
/// [`crate::DbError`] for every refusal [`edit_account_in`] states.
pub fn edit_account_in_journalled(
    settings_dir: &Path,
    edit: crate::db::AccountEdit<'_>,
    journal: AccountJournal,
) -> Result<(crate::db::AccountWrite, Option<JournalAppendError>), crate::db::DbError> {
    let done = edit_account_in(settings_dir, edit)?;
    if !done.changed {
        return Ok((done, None));
    }
    let journal_error = {
        let Some(row) = done.after.as_ref().or(done.before.as_ref()) else {
            return Ok((done, None));
        };
        let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
        let change = Change::account_lifecycle(
            Outcome::Applied,
            journal.actor,
            crate::dotenv::DB_FILE,
            done.verb,
            row.id,
            &row.venue,
            &row.tier,
            done.before.as_ref().and_then(|b| b.label.as_deref()),
            done.after.as_ref().and_then(|a| a.label.as_deref()),
            done.after.as_ref().is_some_and(|a| a.active),
            &keys,
        );
        let cj = journal_beside(settings_dir, journal.proc);
        cj.append(journal.now_ms, &change)
            .err()
            .map(|source| JournalAppendError { dir: cj.dir().to_path_buf(), source })
    };
    Ok((done, journal_error))
}
