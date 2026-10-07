//! The routed credential UPSERT and its change-journal record: `save_credentials_to_store*`.

use super::*;

/// **The UPSERT, routed to the store that actually answers** — the write twin of
/// [`resolve_store_in`], and the reason a migrated box can still change a key.
///
/// # The defect this exists to close
///
/// Every credential WRITE in this workspace used to name a FILE. After
/// `docs/decisions/0054`'s credential half that file is shadowed: the write succeeds, the file
/// genuinely changes, the change journal records it, the caller reports success — and no reader ever
/// opens that file again. The sharpest instance is `crates/bridges/ctrader/src/token_store.rs`'s
/// `persist`, which stores a grant THE VENUE rotated: a shadowed write means the token is lost at
/// restart and that session cannot re-authenticate.
///
/// # One decision, shared with the reader
///
/// [`backend_in`], on the same `settings_dir` the reader is given. There is no second probe and no
/// second path derivation, so a writer and a reader in one process — or in two — cannot disagree
/// about which store is live.
///
/// # The UPSERT rule carries over unchanged
///
/// **Replace exactly the named keys; leave everything else alone.**
///
/// * the FILE branch is [`crate::save_credentials`] VERBATIM — not reimplemented, not wrapped, not
///   "improved" — so every byte-preservation property that transform is tested for (comments, blank
///   lines, order, duplicate lines, the destination mode, a symlinked store, the atomic rename) is
///   the same property here;
/// * the DATABASE branch is `INSERT … ON CONFLICT(name) DO UPDATE SET value` over the named rows in
///   ONE transaction — every other row untouched, and a pair impossible to half-write. It creates no
///   database: it is reached only when one already exists.
///
/// Nothing in either branch rewrites a store WHOLESALE, and there is no flag that does.
///
/// # The multiline refusal happens FIRST, for both branches
///
/// A value spanning more than one line cannot be represented in the file's grammar. SQLite would
/// take it happily, which is exactly why it is refused here rather than in the file branch alone: a
/// key that round-trips on a migrated box and is rejected on an unmigrated one is a divergence
/// between two stores that must answer identically.
///
/// Returns the [`Backend`] that was written, so a caller can say WHERE the key landed.
/// # `classify` — required for a NEW credential name, meaningless for a node key
///
/// Since the settings database's schema 2, a `credential` row says which ACCOUNT it belongs to
/// (`docs/superpowers/specs/2026-09-14-the-credential-schema.md` §4), and that classification is
/// derived from the key NAME by machinery this crate cannot see — `vike-bridge-core` declares
/// `layer = 25` where tier 15's rule is *nothing above rank 10*, and it declares `vike-secrets`
/// itself, so the edge is a cycle as well as a band violation — and it arrives as a closure for
/// that reason. `vike_bridge_core::credentials::classify_credential_name` is the production
/// implementation. ⚠ This sentence read *"the same seam and for the same layering reason
/// [`crate::migrate`] takes `is_node_key` through"*, and the second half is false:
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
/// 2026-09-20) took THAT seam's layer bound away — `vike_model::credential_keys::is_platform_key`
/// is reachable from here — and it was ruled on 2026-09-26 to stay on different grounds, written
/// out in [`crate::migrate`]'s own doc. The shape is shared; the argument is not.
///
/// **It is only consulted for a name this store has never held.** Replacing a known key's value
/// needs no classification, because the row already carries one — which is what keeps the venue's
/// own rotation writer (`crates/bridges/ctrader/src/token_store.rs`'s `persist`) working with
/// nothing to supply. `None` is therefore correct for every [`crate::Table::NodeKey`] caller:
/// 0051's pair belongs to no account and its table is `(name, value)` in every schema. A NEW
/// credential name with `None` is refused by name rather than filed as a deployment-level
/// credential belonging to nothing — see `crate::DbErrorKind::Unclassified`.
///
/// The FILE branch ignores it entirely: `secrets.env` has no schema to classify into.
pub fn save_credentials_to_store(
    settings_dir: &Path,
    table: crate::db::Table,
    updates: &[(String, String)],
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
) -> std::io::Result<Backend> {
    crate::env_write::refuse_multiline(updates)?;
    let backend = backend_in(settings_dir);
    match &backend {
        Backend::Database(db) => crate::db::upsert_rows(db, table, updates, classify)
            // `DbError`'s `Display` carries the path and the reason and never a row value — the
            // same bridge `From<DbError> for SecretsError` argues for the read path.
            .map_err(|e| std::io::Error::other(e.to_string()))?,
        Backend::Files => {
            let file = match table {
                crate::db::Table::Credential => crate::dotenv::secrets_path_in(settings_dir),
                crate::db::Table::NodeKey => crate::dotenv::node_path_in(settings_dir),
            };
            crate::env_write::save_credentials(&file, updates)?;
        }
    }
    Ok(backend)
}

/// **What [`Change::credential_write`] and [`Change::account_lifecycle`] need to describe WHO is
/// writing, and how — the two things that were never derivable from the write itself.**
///
/// A finding, in the same sense [`PermissionWarning`] and [`LegacyStoreWarning`] are: this crate
/// carries no logging dependency (see the crate doc), so a journal-append failure is returned as
/// [`JournalAppendError`] rather than logged, exactly as `vike_model::change_journal::
/// ChangeJournalError`'s own doc argues for itself. The CALLER — `vike-connections`, which already
/// links `tracing` for its own console lines — logs it.
#[derive(Debug, Clone)]
pub struct CredentialJournal<'a> {
    /// WHO is writing.
    pub actor: Actor,
    /// The venue the keys belong to, or `"multi"` for a save spanning several.
    pub venue: &'a str,
    /// The credential tier (`"SIM"`/`"DEMO"`/`"LIVE"`, or
    /// `vike_model::change_journal::TIER_UNTIERED`).
    pub tier: &'a str,
    /// The writing process's identity — see `vike_model::change_journal::Proc`. A PARAMETER, never
    /// derived with `Proc::current()` in here: this function runs inside every binary that links
    /// this crate, so `env!("CARGO_PKG_VERSION")` read HERE would name `vike-secrets`'s own version
    /// rather than the caller's, which is exactly the confusion `Proc`'s own doc — *"which binary
    /// wrote this"* — exists to answer honestly.
    pub proc: Proc,
    /// The instant to stamp the record with. A PARAMETER, because `vike_model::change_journal`
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
/// This is the collapse of what used to be a two-call pattern at `vike-connections`'
/// (`save_credentials_journalled`, now deleted): a caller resolved the store's path AND the ledger
/// separately, then had to remember to call both. Here there is one call, and the record is built
/// from what this function ITSELF just wrote — `updates`' key NAMES and nothing else, per
/// [`Change::credential_write`]'s own "note what this signature does NOT take".
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
    classify: Option<&dyn Fn(&str) -> crate::schema::Classification>,
    journal: CredentialJournal<'_>,
) -> std::io::Result<(Backend, Option<JournalAppendError>)> {
    let backend = save_credentials_to_store(settings_dir, table, updates, classify)?;

    // ⚠ `.0` ONLY — the projection that makes a value unrepresentable downstream even before
    // `Change::credential_write`'s own signature refuses to hold one.
    let keys: Vec<&str> = updates.iter().map(|(key, _value)| key.as_str()).collect();
    // The FILE NAME this record has always carried, `secrets.env`, whichever backend actually
    // answered — see `Change::credential_write`'s `store` cell: it names the historical credential
    // surface, not the live one, and changing that is a decision for a later record, not this move.
    let change = Change::credential_write(
        Outcome::Applied,
        journal.actor,
        crate::dotenv::SECRETS_FILE,
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
