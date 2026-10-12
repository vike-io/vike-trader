//! The routed credential UPSERT and DELETE, and their change-journal records:
//! `save_credentials_to_store*` and `retire_credential_in_store*`.

use super::*;

/// **The credential UPSERT into the settings database** — the write twin of [`resolve_store_in`].
///
/// # One decision, shared with the reader
///
/// [`backend_in`], on the same `settings_dir` the reader is given. There is no second probe and no
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
/// * [`crate::UpsertIntent::Add`] — a new name may be created. Since the settings database's
///   schema 2 a `credential` row says which ACCOUNT it belongs to
///   (`docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4), and a new row is filed under
///   the account, venue and field `vike_secrets::credentials::classify_credential_name` derives from
///   its NAME. A new `node_key` name is simply inserted: 0051's pair belongs to no account and its
///   table is `(name, value)` in every schema.
/// * [`crate::UpsertIntent::Replace`] — only names the table holds live; any other is refused by
///   name (`crate::DbErrorKind::NotHeld`) and the batch writes nothing.
///
/// **A known key is replaced in place under either word**, and the classifier is not consulted
/// for it: the row already carries its account, which the replace leaves where it is.
///
/// ⚠ This was a `classify: Option<&dyn Fn(&str) -> Classification>` parameter, a closure because
/// the classifier lived above this crate until decision 0135 moved it down; `None` and
/// `Some(&classify_credential_name)` were the only two values any caller passed, and they are the
/// two words now.
pub fn save_credentials_to_store(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    intent: crate::db::UpsertIntent,
) -> std::io::Result<Backend> {
    upsert_in_store(settings_dir, table, updates, intent.new_names())
}

/// **[`save_credentials_to_store`] with the classification SUPPLIED** — the test-support door, and
/// the only way to hand the upsert anything but the real classifier.
///
/// It exists for this crate's own suites, which drive the schema-2 fill with SYNTHETIC
/// classifications no real name produces (the `Rule` tables of
/// `crates/vike-secrets/tests/support/classify.rs`: a discriminator on an unlabelled account, a
/// classifier that contradicts itself). Behaves as [`crate::UpsertIntent::Add`] with `classify`
/// in place of `classify_credential_name`; every other rule is the same one, through the same body.
///
/// # Errors
/// Exactly [`save_credentials_to_store`]'s.
#[cfg(any(test, feature = "test-support"))]
pub fn save_credentials_to_store_filed_by(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> std::io::Result<Backend> {
    upsert_in_store(settings_dir, table, updates, crate::db::NewNames::FiledBy(classify))
}

/// The one body behind [`save_credentials_to_store`] and its test-support door: the multiline
/// refusal, ONE [`backend_in`], the upsert or the absent-store refusal.
fn upsert_in_store(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    new_names: crate::db::NewNames<'_>,
) -> std::io::Result<Backend> {
    refuse_multiline(updates)?;
    let backend = backend_in(settings_dir);
    match &backend {
        Backend::Database(db) => crate::db::upsert_rows(db, table, updates, new_names)
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

/// **The credential DELETE** — the write twin of [`save_credentials_to_store`] for the one act it
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
pub fn retire_credential_in_store(
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

/// **[`retire_credential_in_store`], plus its durable [`Change::credential_retire`] record** — the
/// DELETE's twin of [`save_credentials_to_store_journalled`], with that function's ordering and
/// its two failure paths: the retire first (its error returns unchanged, and no record is written
/// for a delete that did not happen), then the record, whose failure comes back as
/// `Some(JournalAppendError)` and never fails the call — the row IS retired either way.
///
/// The record carries the NAME and nothing else: there is no value in this call at all.
pub fn retire_credential_in_store_journalled(
    settings_dir: &Path,
    table: crate::db::Table,
    name: &str,
    journal: CredentialJournal<'_>,
) -> std::io::Result<(Backend, Option<JournalAppendError>)> {
    let backend = retire_credential_in_store(settings_dir, table, name)?;
    let change = Change::credential_retire(
        Outcome::Applied,
        journal.actor,
        crate::store_locator::DB_FILE,
        journal.venue,
        journal.tier,
        &[name],
    );
    let cj = journal_beside(settings_dir, journal.proc);
    let journal_error = cj
        .append(journal.now_ms, &change)
        .err()
        .map(|source| JournalAppendError { dir: cj.dir().to_path_buf(), source });
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
/// [`JournalAppendError`] rather than logged, exactly as `crate::change_journal::
/// ChangeJournalError`'s own doc argues for itself. The CALLER — `vike-connections`, which already
/// links `tracing` for its own console lines — logs it.
#[derive(Debug, Clone)]
pub struct CredentialJournal<'a> {
    /// WHO is writing.
    pub actor: Actor,
    /// The venue the keys belong to, or `"multi"` for a save spanning several.
    pub venue: &'a str,
    /// The credential tier (`"SIM"`/`"DEMO"`/`"LIVE"`, or
    /// `crate::change_journal::TIER_UNTIERED`).
    pub tier: &'a str,
    /// The writing process's identity — see `crate::change_journal::Proc`. A PARAMETER, never
    /// derived with `Proc::current()` in here: this function runs inside every binary that links
    /// this crate, so `env!("CARGO_PKG_VERSION")` read HERE would name `vike-secrets`'s own version
    /// rather than the caller's, which is exactly the confusion `Proc`'s own doc — *"which binary
    /// wrote this"* — exists to answer honestly.
    pub proc: Proc,
    /// The instant to stamp the record with. A PARAMETER, because `crate::change_journal`
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
/// `vike_model::paths::state_path::project_state_dir(_from)` uses (`project_settings_dir(start)?.join(
/// STATE_SUBDIR)`), so a caller handing this function the SAME settings directory it read the store
/// from can never resolve a ledger describing a different project than the one the write landed in.
pub(crate) fn journal_beside(settings_dir: &Path, process: Proc) -> ChangeJournal {
    ChangeJournal::in_state_dir(
        &settings_dir.join(vike_model::paths::state_path::STATE_SUBDIR),
        process,
    )
}

/// **[`save_credentials_to_store`], plus its durable [`Change::credential_write`] record —
/// together, so the two cannot drift apart at a call site.**
///
/// One call, so a caller never resolves the store's path AND the ledger separately and then has to
/// remember to call both; the record is built from what this function ITSELF just wrote —
/// `updates`' key NAMES and nothing else, per [`Change::credential_write`]'s own "note what this
/// signature does NOT take".
///
/// # Ordering, and the two failure paths
///
/// The store write happens FIRST (via [`save_credentials_to_store`]) and its error returns
/// unchanged — a write that did not land must not leave a record saying it did. A JOURNAL failure,
/// by contrast, cannot fail the call: the credential IS on disk, and sending the caller down an
/// error path for a write that succeeded would be worse than a silently-missed ledger line. It
/// comes back as `Some(JournalAppendError)` in the `Ok` tuple for the caller to log — see
/// [`CredentialJournal`] for why this crate cannot log it itself.
pub fn save_credentials_to_store_journalled(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    intent: crate::db::UpsertIntent,
    journal: CredentialJournal<'_>,
) -> std::io::Result<(Backend, Option<JournalAppendError>)> {
    let backend = save_credentials_to_store(settings_dir, table, updates, intent)?;

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
    let cj = journal_beside(settings_dir, journal.proc);
    let journal_error = cj
        .append(journal.now_ms, &change)
        .err()
        .map(|source| JournalAppendError { dir: cj.dir().to_path_buf(), source });
    Ok((backend, journal_error))
}
