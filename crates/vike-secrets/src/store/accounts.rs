//! The account plane's store doors: the handle's `account` reads, the `venue` table reader, and
//! the account writers — each of which records its change in the change journal.

use super::*;

impl CredentialStore {
    /// **Which accounts the store holds** — the `account` table, for every caller that needs the
    /// rows (the mount's account directory, `vike-cli secrets accounts`, the GUI).
    ///
    /// It asks the same [`CredentialStore::backend`] the credential reads ask and the writers'
    /// [`backend_in`] answers, so the three cannot disagree about which store is live. There is no
    /// `table` parameter: an account belongs to the credential plane by definition, and
    /// `crate::db::Table::NodeKey`'s namespace holds a pair of deployment keys that belong to no
    /// account at all. The `account` table holds no credential, so the handle's scope has nothing to
    /// withhold here; [`Self::account_keys`] is where it applies.
    ///
    /// # ⚠ Three answers, and none may be collapsed into another
    ///
    /// * `Ok(Accounts::Known(rows))` — the store carries the table. An EMPTY `rows` is a real answer.
    /// * `Ok(Accounts::Unanswerable(_))` — no `account` table: no location, no settings database
    ///   ([`crate::db::NoAccountTable::NoStore`], naming the database that is not there), or one
    ///   older than the table. Answering `Known(vec![])` there would read as *this store has no
    ///   accounts* about a box that has NO store, and would put every venue silently on paper.
    /// * `Err(_)` — the store EXISTS and would not open. Loud, never an absence.
    ///
    /// There is deliberately **no infallible twin**: an empty credential map degrades safely because
    /// it IS the live gate, and an empty account list carries no such guarantee — it is an assertion
    /// about the store, not a refusal to arm.
    ///
    /// # ⚠ It ARMS A VENUE
    ///
    /// Dukascopy's mount resolves WHICH ACCOUNT — and so which LEGAL ENTITY an order reaches — from
    /// these rows (`crates/bridges/dukascopy/src/mount.rs`). A composition root reads it once, beside
    /// its credential read, and carries it to the mount as
    /// `crate::account_directory::AccountDirectory::read`'s input, with its key-name twin
    /// [`Self::account_keys`].
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn accounts(&self) -> Result<crate::db::Accounts, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(crate::db::read_accounts(&db)?),
            Backend::Absent => {
                Ok(crate::db::Accounts::Unanswerable(crate::db::NoAccountTable::NoStore {
                    db: self.db_path(),
                }))
            }
        }
    }

    /// **The credential key NAMES each account row owns** — the companion [`Self::accounts`] needs
    /// before a human, or the dukascopy mount, can ACT on its rows.
    ///
    /// [`crate::db::AccountKeys`] carries the argument: two rows of `(dukascopy, demo, NULL)` render
    /// identically except for an opaque `id`, and the thing that tells them apart —
    /// `DUKASCOPY_DEMO1_*` versus `DUKASCOPY_DEMO2_*` — lives in the `credential` table rather than
    /// in the `account` one. Values are never selected; see that type. Under
    /// [`CredentialScope::NoLiveKeys`] a withheld NAME is not selected either.
    ///
    /// # `None` means EXACTLY what [`Accounts::Unanswerable`] means, and a caller may not merge them
    ///
    /// `None` is *there is no `account` table to key* — a [`Backend::Absent`] box, which has no store
    /// at all. `Some(map)` is *the table is there*, and a row absent from the map is an account with
    /// no live credential row naming it, which is a real state and not a missing answer.
    ///
    /// [`Accounts::Unanswerable`]: crate::db::Accounts::Unanswerable
    ///
    /// # Errors
    /// [`SecretsError`] when a store that exists will not open.
    pub fn account_keys(
        &self,
    ) -> Result<Option<std::collections::BTreeMap<i64, crate::db::AccountKeys>>, SecretsError> {
        match self.backend() {
            Backend::Database(db) => Ok(Some(crate::db::read_account_keys(&db, self.scope())?)),
            Backend::Absent => Ok(None),
        }
    }

    /// The database this handle would read: `db/vike.db` in its location, or — for a handle with no
    /// location — the relative `settings/db/vike.db` the walk's last resort names, so the "no
    /// store" answer still names a path.
    fn db_path(&self) -> PathBuf {
        match self.location() {
            Some(location) => crate::store_locator::db_path_in(location.settings_dir()),
            None => crate::store_locator::db_path_in(Path::new(
                vike_paths::state_path::PROJECT_SETTINGS_DIR,
            )),
        }
    }
}

/// **Every row of the `venue` table in the database beside `settings_dir`** — [`crate::db::read_venues`]
/// over [`crate::store_locator::db_path_in`], for a caller that already holds the settings
/// DIRECTORY. Read-only: an absent database, or one that predates the table, answers an empty list,
/// and the read creates nothing. The `venue` table is a projection of the compiled roster and holds
/// no credential.
///
/// ⚠ **It lives HERE and not beside `read_venues`, and the reason is a gate.**
/// `crates/vike-secrets/src/db/create.rs` is the module that defines `create_store`, and
/// `crates/vike-ops/tests/settings_secrets/credential_source_roster_gate/gate.rs`'s
/// `the_creator_reads_exactly_the_sources_the_roster_names` treats every path-resolver call in
/// that module as a path the creator reads or writes. A READER that resolved a path there would
/// read as a source of the creator. This file already resolves the path for the account reads, so
/// the one call belongs beside them.
///
/// # Errors
/// [`crate::DbError`] when a database that exists will not read.
pub fn read_venues_in(settings_dir: &Path) -> Result<Vec<crate::db::VenueRow>, crate::db::DbError> {
    crate::db::read_venues(&crate::store_locator::db_path_in(settings_dir))
}

/// **WHO is writing a store edit that carries no credential value — an account-lifecycle edit, a
/// book write, or a venue setting — and when.** The twin of [`CredentialJournal`], for the same
/// reason: [`edit_account_in`], [`set_venue_account_id_in`] and
/// [`crate::set_venue_setting_in`] cannot derive either cell from the edit itself.
#[derive(Debug, Clone)]
pub struct AccountJournal {
    /// WHO is writing.
    pub actor: Actor,
    /// The writing process's identity. See [`CredentialJournal::proc`] for why this is a parameter.
    pub proc: Proc,
    /// The instant to stamp the record with.
    pub now_ms: i64,
}

/// **Write ONE account row's `venue_account_id`, and record it** — the BOOK write, with its
/// [`Change::account_book`] record.
///
/// # The write: routed to the store that actually answers
///
/// [`backend_in`], on the same `settings_dir` the reader is given — so the verb that LISTS the
/// account rows and the verb that writes one cannot disagree about which store they are talking
/// about. There is no second probe and no second path derivation.
///
/// ⚠ **A `Backend::Absent` box is REFUSED, not silently no-op'd**: with no settings database there
/// is no `account` table and no store at all, so the answer is [`crate::DbErrorKind::NoDatabase`] —
/// loud, naming the way to create the store. It is emphatically not a fallback: [`Backend`]'s whole
/// rule is that the choice is per RUN, and a writer that quietly stored the book somewhere else
/// would be inventing exactly the ladder `docs/decisions/0051-node-keys-live-in-their-own-store.md`
/// forbids. It creates no database either: [`crate::create_store`] is the only function in this
/// crate that may bring one into existence, and `set_venue_account_id`'s own `created` arm is the
/// belt behind this probe for the case where the file is removed in between.
///
/// It is not §11 step 3's FOLD of the ten stored book keys. The operator door and the venue
/// handshake's fold both reach the store through it, told which claim they record by
/// [`crate::BookSource`] (`Operator` / `Handshake`) — one router, one writer;
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` is why a second function was
/// not the answer. No `credential` row is read or touched.
///
/// `venue_account_id: None` is a CLEAR: it puts the column back to `NULL` — *not yet known* — and
/// it is the repair a pair of rows written the wrong way round needs (`crate::db::set_venue_account_id`'s
/// CLEAR section is the argument).
///
/// # The record: the same row both surfaces wrote by hand
///
/// [`Change::account_book`] with the row's id, venue and tier, the book it named BEFORE and the one
/// it names now (`None` = a clear), `store` = [`crate::DB_FILE`] — the cells `vike-cli secrets`'s
/// `record_book_write` and the tradehub wire's `journal_book_write` composed by hand.
///
/// ⚠ **Nothing is recorded when nothing CHANGED** ([`crate::BookWrite::changed`] is a claim about the
/// BOOK): a ledger line for a no-op reads as a re-pointing that did not happen, and a confirming
/// handshake fold that moves only `last_verified_at` changes no routing.
///
/// # Ordering, and the two failure paths
///
/// The row write happens FIRST and its error returns unchanged — an edit that did not land must not
/// leave a record saying it did. A JOURNAL failure cannot fail the call: the row IS written, and it
/// comes back as `Some(JournalAppendError)` for the caller to log.
///
/// # Errors
/// [`crate::DbError`] for every refusal `crate::db::set_venue_account_id` states, and for a
/// `Backend::Absent` box.
pub fn set_venue_account_id_in(
    settings_dir: &Path,
    id: i64,
    venue_account_id: Option<&str>,
    replace: bool,
    source: crate::db::BookSource<'_>,
    journal: AccountJournal,
) -> Result<(crate::db::BookWrite, Option<JournalAppendError>), crate::db::DbError> {
    let done =
        set_venue_account_id_in_unjournalled(settings_dir, id, venue_account_id, replace, source)?;
    if !done.changed {
        return Ok((done, None));
    }
    let change = Change::account_book(
        Outcome::Applied,
        journal.actor,
        crate::store_locator::DB_FILE,
        done.before.id,
        &done.before.venue,
        done.before.tier,
        done.before.venue_account_id.as_deref(),
        // `None` is a CLEAR — old present, new absent — which `AccountBookTarget::cleared` reads.
        done.venue_account_id.as_deref(),
    );
    let journal_error = append_beside(settings_dir, journal.proc, journal.now_ms, &change);
    Ok((done, journal_error))
}

/// [`set_venue_account_id_in`]'s store write WITHOUT its record — the body that function runs first,
/// and a `test-support` door for a suite that seeds a book with no journal beside it. Production
/// reaches it through [`set_venue_account_id_in`] alone.
///
/// # Errors
/// Exactly [`set_venue_account_id_in`]'s.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn set_venue_account_id_in_unjournalled(
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
        Backend::Absent => Err(crate::db::DbError {
            path: crate::store_locator::db_path_in(settings_dir),
            kind: crate::db::DbErrorKind::NoDatabase,
        }),
    }
}

/// **The account LIFECYCLE — create / rename / (de)activate / remove — and its durable
/// [`Change::account_lifecycle`] record, together.**
///
/// # The write: routed to the store that actually answers
///
/// [`backend_in`], on the same `settings_dir` the reader is given — so the verb that LISTS the
/// account rows and the verb that edits one cannot disagree about which store they are talking
/// about. A `Backend::Absent` box is REFUSED ([`crate::DbErrorKind::NoDatabase`]), not silently
/// no-op'd, and it CREATES NO DATABASE — reason 1 of `docs/decisions/0036` at its sharpest: the mere
/// EXISTENCE of `<project>/settings/db/vike.db` is the whole of [`Backend`]'s per-run choice, so an
/// account verb that created one would hand the box an EMPTY store as a side effect of an edit.
/// No `credential` row is written or its value read — `crate::db::edit_account`'s *what it never
/// does* section is the contract.
///
/// # What the record never carries
///
/// **No credential value, on any path** — the edit never selects a `value` column, and
/// `Change::account_lifecycle` takes no value parameter, which is the enforcement rather than a
/// convention. The key NAMES that DO reach the record are recorded for the reason
/// [`Change::credential_write`]'s doc gives: they are not secret, and *"what did that account own
/// when it was deactivated"* is unanswerable without them. The record is the one
/// `vike-cli secrets account`'s and the tradehub wire's lifecycle verbs composed by hand: the row
/// that exists after the act (or the removed one), the label before and after, `active`, the key
/// names, `store` = [`crate::DB_FILE`].
///
/// # Ordering, and the two failure paths
///
/// The row write happens FIRST and its error returns unchanged — an edit that did not land must not
/// leave a record saying it did. Nothing to record when nothing CHANGED: a ledger line for a no-op
/// reads as an edit that did not happen. A JOURNAL failure, by contrast, cannot fail the call: the
/// row IS written, and it comes back as `Some(JournalAppendError)` for the caller to log.
///
/// # Errors
/// [`crate::DbError`] for every refusal `crate::db::edit_account` states, and for a
/// `Backend::Absent` box.
pub fn edit_account_in(
    settings_dir: &Path,
    edit: crate::db::AccountEdit<'_>,
    journal: AccountJournal,
) -> Result<(crate::db::AccountWrite, Option<JournalAppendError>), crate::db::DbError> {
    let done = edit_account_in_unjournalled(settings_dir, edit)?;
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
            crate::store_locator::DB_FILE,
            done.verb,
            row.id,
            &row.venue,
            row.tier,
            done.before.as_ref().and_then(|b| b.label.as_deref()),
            done.after.as_ref().and_then(|a| a.label.as_deref()),
            done.after.as_ref().is_some_and(|a| a.active),
            &keys,
        );
        append_beside(settings_dir, journal.proc, journal.now_ms, &change)
    };
    Ok((done, journal_error))
}

/// [`edit_account_in`]'s store write WITHOUT its record — the body that function runs first, and a
/// `test-support` door for a suite that seeds accounts with no journal beside them. Production
/// reaches it through [`edit_account_in`] alone.
///
/// # Errors
/// Exactly [`edit_account_in`]'s.
#[cfg_attr(not(feature = "test-support"), expect(unreachable_pub))]
pub fn edit_account_in_unjournalled(
    settings_dir: &Path,
    edit: crate::db::AccountEdit<'_>,
) -> Result<crate::db::AccountWrite, crate::db::DbError> {
    match backend_in(settings_dir) {
        Backend::Database(db) => crate::db::edit_account(&db, edit),
        Backend::Absent => Err(crate::db::DbError {
            path: crate::store_locator::db_path_in(settings_dir),
            kind: crate::db::DbErrorKind::NoDatabase,
        }),
    }
}
