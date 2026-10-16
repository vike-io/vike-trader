//! Persisting a ROTATED cTrader OAuth grant back into the credential store — the write half of the
//! token loop, and the pure expiry arithmetic that decides when to rotate.
//!
//! # The loop this closes
//!
//! `crates/bridges/ctrader/CLAUDE.md` described a credential loop closed BY HAND: the authorize bin
//! wrote a token file nothing read back, `crate::config::CtraderConfig` was built from the
//! credential store only, and `crate::conn`'s `try_refresh_and_reauth` assigned a refreshed pair IN
//! MEMORY and returned. The refreshed credential therefore lived exactly as long as the process,
//! and — because cTrader ROTATES the refresh token on every refresh — the pair on disk went stale
//! the first time a daemon refreshed. An operator whose demo grant aged out got a daemon that would
//! not start while the credentials to start it sat in the same file. Observed live: the
//! alpaca+ctrader live rehearsal (PR #1407), Evidence 2.
//!
//! # ONE credential, ONE home: the ACCOUNT's fields in the credential store
//!
//! The grant is a credential of ONE account, so it lives where every other credential lives — the
//! settings database, on that account's `ACCESS_TOKEN` and `REFRESH_TOKEN` fields (decision 0156
//! item 6: a credential belongs to its account, addressed by `account.id` + field, never by a
//! composed name). It is written through [`vike_secrets::save_owned_credentials`], the UPSERT that
//! replaces exactly the account's two fields in one transaction and leaves every other row
//! untouched. That is the ONE sanctioned way to write the store; what the workspace rule forbids is
//! rewriting the user's only copy of their keys WHOLESALE, never amending one key it was asked to
//! amend.
//!
//! ⚠ The write is REFUSED rather than lost, in three shapes, and [`persist`] returns each as a
//! [`PersistError`] that `crate::conn` escalates:
//!
//! * **no database** — `save_owned_credentials` asks `vike_secrets::backend_in` whether there is a
//!   store, the same question every reader asks, and refuses when there is none: a rotation that
//!   "landed" nowhere a later process reads would leave the store carrying the SPENT grant — a
//!   token cTrader has already exchanged;
//! * **no account** — credentials handed with no `account.id` ([`TokenPersist::account_row`] is
//!   `None`: a test's or a tool's) name no account to write back to, and the rotation is refused
//!   loudly rather than filed under some other owner;
//! * **a field the account does not hold** — the write is `UpsertIntent::Replace`: a rotation
//!   replaces the two rows the grant was READ from and never invents one (the account row's own
//!   existence is the writer's check too: an id no row carries is `DbErrorKind::NoSuchAccount`).
//!
//! ⚠ The authorize bin's `<project>/settings/state/ctrader_token.json` is NOT a second source of
//! truth and is not read here. It stays that tool's own output record; the daemon reads the store
//! and writes the store.
//!
//! # Secrets never reach a log — or a ledger
//!
//! [`PersistError`] carries the store PATH, the account row id, the two FIELD names and an OS
//! reason — never a token value.
//!
//! ⚠ This module used to log NOTHING AT ALL, which meant a venue-driven credential rotation was
//! invisible on every surface in the workspace. [`persist`] now writes through the journalling
//! writer, which appends one `vike_change_journal` `credential_write` record per rotation,
//! and [`persist`] emits exactly one `tracing::warn!` — when that APPEND fails. Both carry NAMES
//! and neither can carry a value: `vike_change_journal::Change::credential_write` accepts
//! no old/new/value parameter, so a token has no way into the record even by mistake.

use std::path::{Path, PathBuf};

use crate::config::{ACCESS_TOKEN, REFRESH_TOKEN};

/// The exact command that re-issues a cTrader grant. Named in every refusal a fresh grant would
/// fix, because "Access token expired" alone tells an operator what broke and not what to do — the
/// gap the live rehearsal hit, where a daemon refused startup and said nothing about the remedy.
/// The grant belongs to ONE account, so the tool is told which (`vike-cli secrets accounts` prints
/// the ids).
pub const REAUTHORIZE_CMD: &str =
    "cargo run -p vike-ctrader --bin ctrader_authorize -- --account-id <ID>";

/// Refresh this long BEFORE the grant lapses, rather than after it has been refused.
///
/// ⚠ This is the knob that turns the loop from REACTIVE into PROACTIVE, and it is sized against
/// what actually goes wrong: a daemon that only refreshes on a rejection has already failed a
/// request. cTrader demo grants carry a multi-week `expiresIn`, so five minutes is negligible
/// against the token's life while being comfortably longer than the actor's 10 s heartbeat cadence
/// — the tick that evaluates it (`crate::conn`'s actor loop, never the fold).
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

/// Everything a running connection needs in order to persist a rotated grant: WHERE the store is,
/// WHICH account the grant belongs to, and WHERE the rotation is recorded.
///
/// `None` on a `ConnConfig` (every caller that does not supply one — tests, the catalog probe)
/// keeps the historical behaviour exactly: a refresh lives as long as the process and nothing is
/// written — no store write, and therefore no journal record either. That is the honest shape: a
/// start with no project resolved has no store to rotate INTO, so there is nothing to record and no
/// path to invent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPersist {
    /// The settings DIRECTORY (`<project>/settings`), as the COMPOSITION ROOT resolved it — a
    /// parameter, never a path this library walks for itself. The credential store is the database
    /// inside it (`vike_secrets::db_path_in`). See [`settings_dir_beside_state_dir`].
    pub settings_dir: PathBuf,
    /// **The `account.id` whose grant this is** — `AccountCredentials::account_id` of the
    /// credentials the grant was read from, so the rotation writes back to the account it came
    /// from and never over another account's grant. ⚠ Not cTrader's trading-account id (`ctid`,
    /// `CtraderConfig::account_id`): this is the settings database's row.
    ///
    /// `None` when the credentials named no account row (a test's, a tool's): [`persist`] then
    /// REFUSES, loudly, as it refuses a box with no store.
    pub account_row: Option<i64>,
    /// `<project>/settings/state`, the SAME resolved directory [`TokenPersist::settings_dir`] was
    /// derived from — the change journal's home: `vike_secrets::save_owned_credentials` appends
    /// its record at `<settings_dir>/state/changes`, which is this directory's `changes`, because
    /// [`TokenPersist::settings_dir`] is its parent.
    ///
    /// ⚠ Not an `Option`, and not a second walk. Both fields are produced together by one
    /// `CtraderConfig::from_credentials_with_store` call out of one composition-root parameter, so
    /// a rotation can never record itself into one project's ledger while writing another
    /// project's store. Where the root resolved no state directory there is no `TokenPersist` at
    /// all.
    pub state_dir: PathBuf,
}

impl TokenPersist {
    /// The credential store a rotation writes: the settings database inside
    /// [`TokenPersist::settings_dir`]. A PATH for a log line or an error, never opened here.
    #[must_use]
    pub fn store(&self) -> PathBuf {
        vike_secrets::db_path_in(&self.settings_dir)
    }
}

/// The settings directory a project's `settings/state` directory sits in:
/// `<project>/settings/state` → `<project>/settings`. Whether there is a store in it — the
/// database — is `vike_secrets::backend_in`'s call, made by the write in [`persist`].
///
/// ⚠ A DERIVATION, not a second resolution walk. The root `CLAUDE.md`'s rule is that ONE walk
/// decides and every project-relative path is derived from it — the `_from`-less resolvers are
/// `$VIKE_SETTINGS_DIR`-blind, and all three shipped units set that variable, so a library that
/// re-walked from its working directory would answer for the wrong project on every deployment.
/// The caller passes the state directory its boot already resolved.
#[must_use]
pub fn settings_dir_beside_state_dir(state_dir: &Path) -> Option<PathBuf> {
    state_dir.parent().map(Path::to_path_buf)
}

/// Wall-clock ms at which a grant obtained at `obtained_at_ms` with lifetime `expires_in` seconds
/// lapses. `None` when the venue reported no lifetime — an unknown expiry must read as "no
/// opinion", never as a timestamp in the past that would trigger a refresh storm.
///
/// Saturating throughout, so an absurd `expires_in` reads as very far away rather than wrapping.
#[must_use]
pub fn expires_at_ms(obtained_at_ms: i64, expires_in: u64) -> Option<i64> {
    if expires_in == 0 {
        return None;
    }
    let life_ms = i64::try_from(expires_in).unwrap_or(i64::MAX / 2).saturating_mul(1000);
    Some(obtained_at_ms.saturating_add(life_ms))
}

/// Is a grant expiring at `expires_at_ms` due for a PROACTIVE refresh at `now_ms`?
///
/// `None` (an unknown expiry — a process that has not refreshed yet, so it never saw an
/// `expiresIn`) is deliberately NOT due: it must leave the reactive path exactly as it was rather
/// than refreshing on every heartbeat forever.
#[must_use]
pub fn refresh_due(expires_at_ms: Option<i64>, now_ms: i64) -> bool {
    match expires_at_ms {
        Some(at) => now_ms >= at.saturating_sub(REFRESH_MARGIN_MS),
        None => false,
    }
}

/// Why a rotated grant could not be persisted. Carries the store path (the settings database), the
/// account row id and an OS reason — NEVER a token value.
#[derive(Debug)]
pub struct PersistError {
    pub store: PathBuf,
    /// The `account.id` the rotation was for — `None` when the credentials named no account row.
    pub account_row: Option<i64>,
    pub reason: String,
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whose = match self.account_row {
            Some(id) => format!("account row {id}"),
            None => "no account row".to_string(),
        };
        write!(
            f,
            "could not persist the refreshed cTrader grant ({whose}: {ACCESS_TOKEN} / \
             {REFRESH_TOKEN}) to {}: {} — the running session keeps the new token, but a RESTART \
             will fall back to the stale pair on disk",
            self.store.display(),
            self.reason
        )
    }
}

impl std::error::Error for PersistError {}

/// Upsert a freshly-refreshed grant onto its account's two fields, leaving every other row
/// untouched — and RECORD the rotation in the append-only change journal.
///
/// Both fields are written in ONE upsert: cTrader rotates the refresh token alongside the access
/// token, so persisting only the access token would leave the store carrying a refresh token that
/// has already been spent — the exact staleness this function exists to remove, in a subtler form.
/// **The record follows that shape rather than the call count: ONE rotation is ONE record carrying
/// both row names and `count == 2`.** Two records would read as two rotations, which is precisely
/// the half-write story an incident review must not be told.
///
/// # ⚠ This site was invisible EVERYWHERE before the journal
///
/// The other credential writers at least emit a `tracing` line. This one emitted nothing at all —
/// the module doc's "nothing in this module logs" is still true, and still deliberate — so a
/// venue-driven credential rotation left no trace on any surface. `crate::conn`'s
/// `adopt_refreshed_token` does log the outcome, but that is one function away and is a console line
/// with a rolling-file retention measured in days. The ledger is the durable answer to *"when did
/// this grant last rotate"*.
///
/// # `now_ms` is a parameter, and the actor is the VENUE
///
/// `vike_change_journal` reads no clock, so the instant comes from the caller — which already
/// takes one (`vike_model::now_ms`, read in `crate::conn` off the fold, at a heartbeat tick or after
/// an OAuth round trip). The record's actor is `Actor::venue("ctrader")` rather than any human channel,
/// because that is literally what happened: the venue rotated the grant and this process wrote down
/// what it was handed. That origin exists in the journal for this call site. The record's venue and
/// tier are the ACCOUNT ROW's own, read by the writer — never carried here, so a ledger line cannot
/// name a tier the account does not have.
///
/// # A journal failure does not fail the rotation
///
/// The store write happens FIRST and its error is returned unchanged. A journal append that fails
/// afterwards is `warn!`ed with the DIRECTORY and the account row — never a token — and the call
/// still succeeds: the fresh grant IS on disk, and refusing to acknowledge it would turn a
/// successful self-heal into an outage over a missing audit line. (`warn` rather than the `error`
/// its GUI twin uses, because `crate::conn`'s caller already escalates a persist FAILURE and this is
/// the weaker half of a rotation that otherwise worked.)
///
/// ⚠ Nothing here can put a token in the ledger: `vike_change_journal::Change::credential_write`
/// takes no old/new/value parameter, and the only cells fed to it are the row NAMES the writer
/// itself reports.
pub fn persist(
    persist: &TokenPersist,
    token: &crate::oauth::Token,
    now_ms: i64,
) -> Result<(), PersistError> {
    use vike_change_journal::{Actor, Proc};

    let refused = |reason: String| PersistError {
        store: persist.store(),
        account_row: persist.account_row,
        reason,
    };
    // ⚠ NO ACCOUNT, NO WRITE. The grant belongs to one account; with none named there is no owner
    // to file it under, and a rotation that landed anywhere a later process does not read would
    // make the next restart authenticate with a token already exchanged.
    let Some(account) = persist.account_row else {
        return Err(refused(
            "these credentials name no `account` row, so there is no account to write the rotated \
             grant back to — refused rather than filed under any other owner"
                .to_string(),
        ));
    };

    // `Proc::current` reads `current_exe`, so resolve it once per process rather than per rotation.
    static PROCESS: std::sync::OnceLock<Proc> = std::sync::OnceLock::new();
    let process = PROCESS.get_or_init(|| Proc::current(env!("CARGO_PKG_VERSION")));

    let fields = [
        (ACCESS_TOKEN.to_string(), token.access_token.clone()),
        (REFRESH_TOKEN.to_string(), token.refresh_token.clone()),
    ];
    // ⚠ `save_owned_credentials` asks `vike_secrets::backend_in` about the settings directory —
    // the same question every reader asks — and refuses when there is no database. cTrader rotates
    // the refresh token on every refresh, so a rotation that landed anywhere a later process does
    // not read would make the next restart authenticate with a token already exchanged, and that
    // session cannot recover on its own.
    let (_written, journal_error) = vike_secrets::save_owned_credentials(
        &persist.settings_dir,
        &vike_secrets::CredentialOwner::Account(account),
        &fields,
        // `Replace`: the venue rotates the grant in place, on the two fields the loader READ it
        // from, so both are held live; a field the account does not hold is refused, never created.
        vike_secrets::UpsertIntent::Replace,
        // ONE record, BOTH fields — the pair rotates together, so it is one change. The VENUE
        // rotated it, so the actor is the venue; the venue and tier are the account row's.
        vike_secrets::AccountJournal {
            actor: Actor::venue("ctrader"),
            proc: process.clone(),
            now_ms,
        },
    )
    .map_err(|e| refused(e.to_string()))?;

    if let Some(e) = journal_error {
        tracing::warn!(
            target: "ctrader",
            error = %e.source,
            dir = %e.dir.display(),
            account_row = account,
            "rotated cTrader grant NOT recorded to the change journal (the grant IS saved)"
        );
    }
    Ok(())
}

#[path = "token_store_tests.rs"]
#[cfg(test)]
mod token_store_tests;
