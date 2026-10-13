//! The routed credential UPSERT and DELETE, and their change-journal records:
//! `save_credentials_to_store*` (by NAME: a deployment row or a node key),
//! `save_owned_credentials*` (an ACCOUNT's or a VENUE's credential, by FIELD) and
//! `retire_credential_in_store*`.

use super::*;

/// **The credential UPSERT into the settings database, with no record** — the write twin of
/// [`CredentialStore::read_all`], and the body [`save_credentials_to_store`] runs first.
///
/// # One decision, shared with the reader
///
/// [`backend_in`], on the same settings directory the reader's [`CredentialStore::backend`] probes.
/// There is no second probe and no
/// second path derivation, so a writer and a reader in one process — or in two — cannot disagree
/// about whether there is a store.
///
/// # What it does
///
/// * **the database exists** — `INSERT … ON CONFLICT(name) DO UPDATE SET value` over the named rows
///   in ONE transaction: every other row untouched, and a pair impossible to half-write. Nothing
///   rewrites the store WHOLESALE, and there is no flag that does.
/// * **no database** — REFUSED, naming [`CREATE_STORE_REMEDY`]. It creates no database (only
///   `vike-cli secrets init` may), so there is nowhere else a key could land. ⚠ That
///   refusal is what keeps a key from vanishing in silence. The sharpest case is
///   `crates/bridges/ctrader/src/token_store.rs`'s `persist`, which stores a grant THE VENUE
///   rotated — a write that landed nowhere would lock that session out at the next restart, so it
///   must fail out loud instead.
///
/// # The multiline refusal happens FIRST
///
/// A credential is ONE line. SQLite would take a multi-line value happily, which is exactly why it
/// is refused here: such a value cannot be typed on the one line `vike-cli secrets set` reads, so
/// the store must not grow a shape only this door can make.
///
/// Returns the [`Backend`] that was written — always [`Backend::Database`] on success.
///
/// # `intent` — what may happen to a NAME the table does not hold
///
/// [`crate::UpsertIntent`] carries the one decision a caller makes
/// (`docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`):
///
/// * [`crate::UpsertIntent::Add`] — a new name may be created: a `credential` name as a
///   DEPLOYMENT row (no account, no venue — the pager token, a data provider's key), a `node_key`
///   name as 0051's pair. ⚠ **An ACCOUNT's credential is never written by name**: it is
///   [`save_owned_credentials`]'s, addressed by its account and field, and a name-addressed write
///   creates no account and chooses no tier (decision 0156 item 6).
/// * [`crate::UpsertIntent::Replace`] — only names the table holds live; any other is refused by
///   name (`crate::DbErrorKind::NotHeld`) and the batch writes nothing.
///
/// **A known name is replaced in place under either word**, its row's account, venue and field
/// left exactly where they are.
///
/// ⚠ **It writes NO change-journal record**, which is why it is not the public writer: production
/// reaches it through [`save_credentials_to_store`] alone, which runs it first and then records the
/// write. It is public to a TEST through the `test-support` feature, for a suite that seeds a store
/// with no journal beside it.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn save_credentials_to_store_unjournalled(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    intent: crate::db::UpsertIntent,
) -> std::io::Result<Backend> {
    upsert_in_store(settings_dir, table, updates, intent)
}

/// The one body behind [`save_credentials_to_store_unjournalled`]: the multiline refusal, ONE
/// [`backend_in`], the upsert or the absent-store refusal.
fn upsert_in_store(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    intent: crate::db::UpsertIntent,
) -> std::io::Result<Backend> {
    refuse_multiline(updates)?;
    let backend = backend_in(settings_dir);
    match &backend {
        Backend::Database(db) => crate::db::upsert_rows(db, table, updates, intent)
            // `DbError`'s `Display` carries the path and the reason and never a row value — the
            // same bridge `From<DbError> for SecretsError` argues for the read path.
            .map_err(|e| std::io::Error::other(e.to_string()))?,
        Backend::Absent => return Err(no_store_to_write(settings_dir, table)),
    }
    Ok(backend)
}

/// The refusal [`save_credentials_to_store`] answers a box with no settings database with: the
/// database path, what was not written, and [`CREATE_STORE_REMEDY`]. `NotFound`, because the store
/// is what is missing.
fn no_store_to_write(settings_dir: &Path, table: crate::db::Table) -> std::io::Error {
    let what = match table {
        crate::db::Table::Credential => "a credential",
        crate::db::Table::NodeKey => "a node key",
    };
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "there is no settings database at {} — it is the only credential store, so there is \
             nowhere to write {what}. NOTHING WAS WRITTEN and no store was created; \
             {CREATE_STORE_REMEDY}",
            crate::store_locator::db_path_in(settings_dir).display()
        ),
    )
}

/// **The credential DELETE, with no record** — the write twin of
/// [`save_credentials_to_store_unjournalled`] for the one act it
/// cannot express: take ONE name out of the store, so it reads as ABSENT from then on.
///
/// A SOFT delete (`docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`):
/// the row the readers answer for `name` is RETIRED — `superseded_at` marked — and stays in the
/// table, value and all, as the evidence of the act and the one way back. No row is ever removed,
/// no other row changes, and the same name can be added again later
/// ([`crate::UpsertIntent::Add`]), which is the row every reader then answers with. The database
/// half's doc (`crate::db`'s `retire_row`) lists the refusals: a node key (minted, never deleted by
/// name), a name that is not live, and a name another spelling still answers for at the same
/// account field.
///
/// The SAME routing as the upsert: ONE [`backend_in`]; no database is REFUSED naming
/// [`CREATE_STORE_REMEDY`] and creates none; a database that exists and cannot be opened is an
/// error. There is no value anywhere in this call, so none can reach an error, a log or the
/// journal.
///
/// # Errors
/// `NotFound` with no store; otherwise the refusal or engine error, by `DbError`'s `Display` (a
/// path, a key NAME, a reason — never a value). The `io::Error` carries the `DbError` as its source.
///
/// ⚠ **It writes NO change-journal record**: production reaches it through
/// [`retire_credential_in_store`] alone. Public to a TEST through the `test-support` feature.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn retire_credential_in_store_unjournalled(
    settings_dir: &Path,
    table: crate::db::Table,
    name: &str,
) -> std::io::Result<Backend> {
    let backend = backend_in(settings_dir);
    match &backend {
        Backend::Database(db) => crate::db::retire_row(db, table, name)
            // The `DbError` itself rides as the `io::Error`'s source (its `Display` is unchanged), so a
            // caller that words refusals for an operator downcasts to `DbErrorKind` instead of
            // matching on text: the tradehub wire does, for the three refusals a delete owes names to.
            .map_err(std::io::Error::other)?,
        Backend::Absent => return Err(no_store_to_retire(settings_dir)),
    }
    Ok(backend)
}

/// [`no_store_to_write`]'s twin for the DELETE: nothing to delete from, and nothing created.
fn no_store_to_retire(settings_dir: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "there is no settings database at {} — it is the only credential store, so there is \
             nothing to delete a credential from. NOTHING WAS CHANGED and no store was created; \
             {CREATE_STORE_REMEDY}",
            crate::store_locator::db_path_in(settings_dir).display()
        ),
    )
}

/// **The credential DELETE — [`retire_credential_in_store_unjournalled`] — plus its durable
/// [`Change::credential_retire`] record**: the DELETE's twin of [`save_credentials_to_store`], with
/// that function's ordering and
/// its two failure paths: the retire first (its error returns unchanged, and no record is written
/// for a delete that did not happen), then the record, whose failure comes back as
/// `Some(JournalAppendError)` and never fails the call — the row IS retired either way.
///
/// The record carries the NAME and nothing else: there is no value in this call at all.
pub fn retire_credential_in_store(
    settings_dir: &Path,
    table: crate::db::Table,
    name: &str,
    journal: CredentialJournal<'_>,
) -> std::io::Result<(Backend, Option<JournalAppendError>)> {
    let backend = retire_credential_in_store_unjournalled(settings_dir, table, name)?;
    let change = Change::credential_retire(
        Outcome::Applied,
        journal.actor,
        crate::store_locator::DB_FILE,
        journal.venue,
        journal.tier,
        &[name],
    );
    let journal_error = append_beside(settings_dir, journal.proc, journal.now_ms, &change);
    Ok((backend, journal_error))
}

/// **A credential is ONE line** — refused before any store is touched; see
/// [`save_credentials_to_store`] for why.
pub(crate) fn refuse_multiline(updates: &[(String, String)]) -> std::io::Result<()> {
    for (key, value) in updates {
        if key.contains(['\n', '\r']) || value.contains(['\n', '\r']) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{key}: a credential is ONE line, and this value spans more than one — it \
                     could not be typed, so the store refuses it"
                ),
            ));
        }
    }
    Ok(())
}

/// **What [`Change::credential_write`] and [`Change::account_lifecycle`] need to describe WHO is
/// writing, and how — the two things that were never derivable from the write itself.**
///
/// A finding, in the same sense [`PermissionWarning`] is: this crate's writers
/// log nothing (the crate doc says which loaders do), so a journal-append failure is returned as
/// [`JournalAppendError`] rather than logged, exactly as `vike_change_journal::
/// ChangeJournalError`'s own doc argues for itself. The CALLER — `vike-connections`, which already
/// links `tracing` for its own console lines — logs it.
#[derive(Debug, Clone)]
pub struct CredentialJournal<'a> {
    /// WHO is writing.
    pub actor: Actor,
    /// The venue the keys belong to, or `"multi"` for a save spanning several.
    pub venue: &'a str,
    /// The account's tier word (`vike_model::AccountTier::as_str`: `paper`/`demo`/`live`), or
    /// `vike_change_journal::TIER_UNTIERED` for a write no account owns.
    pub tier: &'a str,
    /// The writing process's identity — see `vike_change_journal::Proc`. A PARAMETER, never
    /// derived with `Proc::current()` in here: this function runs inside every binary that links
    /// this crate, so `env!("CARGO_PKG_VERSION")` read HERE would name `vike-secrets`'s own version
    /// rather than the caller's, which is exactly the confusion `Proc`'s own doc — *"which binary
    /// wrote this"* — exists to answer honestly.
    pub proc: Proc,
    /// The instant to stamp the record with. A PARAMETER, because `vike_change_journal`
    /// reads no clock — the instant travels from the composition root all the way down.
    pub now_ms: i64,
}

/// Why a journal append did not happen. See [`CredentialJournal`] for why this is returned rather
/// than logged.
#[derive(Debug)]
pub struct JournalAppendError {
    /// The journal directory the append was attempted against.
    pub dir: PathBuf,
    /// The underlying failure.
    pub source: ChangeJournalError,
}

impl std::fmt::Display for JournalAppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "change NOT recorded to the change journal at {} ({}) — the store write ITSELF already \
             committed",
            self.dir.display(),
            self.source
        )
    }
}

impl std::error::Error for JournalAppendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// The journal beside `settings_dir` — `<settings_dir>/state/changes`, the same derivation
/// `vike_paths::state_path::project_state_dir(_from)` uses (`project_settings_dir(start)?.join(
/// STATE_SUBDIR)`), so a caller handing this function the SAME settings directory it read the store
/// from can never resolve a ledger describing a different project than the one the write landed in.
pub(crate) fn journal_beside(settings_dir: &Path, process: Proc) -> ChangeJournal {
    ChangeJournal::in_state_dir(&settings_dir.join(vike_paths::state_path::STATE_SUBDIR), process)
}

/// Append `change` to the journal beside `settings_dir` ([`journal_beside`]) — the ONE spelling of
/// "record the write that just landed" every writer here shares: a failure is RETURNED as
/// [`JournalAppendError`], never logged and never a failure of the write (see [`CredentialJournal`]).
pub(crate) fn append_beside(
    settings_dir: &Path,
    process: Proc,
    now_ms: i64,
    change: &Change,
) -> Option<JournalAppendError> {
    let cj = journal_beside(settings_dir, process);
    cj.append(now_ms, change)
        .err()
        .map(|source| JournalAppendError { dir: cj.dir().to_path_buf(), source })
}

/// **THE credential write: the UPSERT ([`save_credentials_to_store_unjournalled`]) plus its durable
/// [`Change::credential_write`] record — together, so the two cannot drift apart at a call site.**
///
/// One call, so a caller never resolves the store's path AND the ledger separately and then has to
/// remember to call both; the record is built from what this function ITSELF just wrote —
/// `updates`' key NAMES and nothing else, per [`Change::credential_write`]'s own "note what this
/// signature does NOT take".
///
/// # Ordering, and the two failure paths
///
/// The store write happens FIRST (via [`save_credentials_to_store_unjournalled`]) and its error returns
/// unchanged — a write that did not land must not leave a record saying it did. A JOURNAL failure,
/// by contrast, cannot fail the call: the credential IS on disk, and sending the caller down an
/// error path for a write that succeeded would be worse than a silently-missed ledger line. It
/// comes back as `Some(JournalAppendError)` in the `Ok` tuple for the caller to log — see
/// [`CredentialJournal`] for why this crate cannot log it itself.
pub fn save_credentials_to_store(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    intent: crate::db::UpsertIntent,
    journal: CredentialJournal<'_>,
) -> std::io::Result<(Backend, Option<JournalAppendError>)> {
    let backend = save_credentials_to_store_unjournalled(settings_dir, table, updates, intent)?;

    // ⚠ `.0` ONLY — the projection that makes a value unrepresentable downstream even before
    // `Change::credential_write`'s own signature refuses to hold one.
    let keys: Vec<&str> = updates.iter().map(|(key, _value)| key.as_str()).collect();
    // The FILE NAME of the store the write LANDED in — `vike.db`, the only store there is — the
    // same cell `vike-cli secrets set` and `secrets init` stamp (`Change::credential_write`'s
    // `store`).
    let change = Change::credential_write(
        Outcome::Applied,
        journal.actor,
        crate::store_locator::DB_FILE,
        journal.venue,
        journal.tier,
        &keys,
    );
    let journal_error = append_beside(settings_dir, journal.proc, journal.now_ms, &change);
    Ok((backend, journal_error))
}

/// **An ACCOUNT's or a VENUE's credential write, with no record** — the body
/// [`save_owned_credentials`] runs first. Public to a TEST through the `test-support` feature.
///
/// The SAME routing as the name-addressed upsert: the multiline refusal first, ONE [`backend_in`];
/// no database is REFUSED naming [`CREATE_STORE_REMEDY`] and creates none.
///
/// # Errors
/// `NotFound` with no store; otherwise the refusal or engine error, by `DbError`'s `Display` (a
/// path, a field or account id, a reason — never a value). The `io::Error` carries the `DbError` as
/// its source, so a wire can word [`crate::DbErrorKind::NoSuchAccount`] for its operator.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn save_owned_credentials_unjournalled(
    settings_dir: &Path,
    owner: &crate::db::CredentialOwner,
    fields: &[(String, String)],
    intent: crate::db::UpsertIntent,
) -> std::io::Result<crate::db::OwnedWrite> {
    refuse_multiline(fields)?;
    match backend_in(settings_dir) {
        Backend::Database(db) => {
            crate::db::upsert_owned(&db, owner, fields, intent).map_err(std::io::Error::other)
        }
        Backend::Absent => Err(no_store_to_write(settings_dir, crate::db::Table::Credential)),
    }
}

/// **THE account credential write** — an EXISTING account's credential (or a venue's application
/// credential), addressed by its owner and FIELD, plus its durable [`Change::credential_write`]
/// record (decision 0156 item 6).
///
/// A credential belongs to its ACCOUNT: the account is the operator's to create, with its tier,
/// before a key is attached (`vike-cli secrets account add --tier …`), and this write REFUSES an
/// account id no row carries ([`crate::DbErrorKind::NoSuchAccount`]) rather than creating one. A
/// field the owner already holds is replaced in place; a new one is inserted under
/// [`crate::account_credential_name`] (`{venue}#{id}.{FIELD}`) or [`crate::venue_credential_name`]
/// (`{venue}.{FIELD}`), names that carry no tier.
///
/// The record carries the NAMES written, the account's venue and its tier WORD
/// (`vike_model::AccountTier::as_str`; `vike_change_journal::TIER_UNTIERED` for a venue's
/// application rows) — never a value. Ordering and the two failure paths are
/// [`save_credentials_to_store`]'s: the write first (its error returns unchanged), then the record,
/// whose failure comes back as `Some(JournalAppendError)`.
pub fn save_owned_credentials(
    settings_dir: &Path,
    owner: &crate::db::CredentialOwner,
    fields: &[(String, String)],
    intent: crate::db::UpsertIntent,
    journal: AccountJournal,
) -> std::io::Result<(crate::db::OwnedWrite, Option<JournalAppendError>)> {
    let done = save_owned_credentials_unjournalled(settings_dir, owner, fields, intent)?;
    let keys: Vec<&str> = done.names.iter().map(String::as_str).collect();
    let change = Change::credential_write(
        Outcome::Applied,
        journal.actor,
        crate::store_locator::DB_FILE,
        &done.venue,
        done.tier.map_or(vike_change_journal::TIER_UNTIERED, vike_model::AccountTier::as_str),
        &keys,
    );
    let journal_error = append_beside(settings_dir, journal.proc, journal.now_ms, &change);
    Ok((done, journal_error))
}
