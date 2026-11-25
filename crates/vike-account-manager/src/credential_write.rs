//! The in-app credential editor's WRITE CONTEXT ([`AccountManagerCredentialHome`], [`AccountManagerCredentialWrite`]). The writes
//! themselves live in `vike-secrets`, and callers name them there:
//! `vike_secrets::save_owned_credentials` — an EXISTING account row's (or a venue's application)
//! credentials, by FIELD — and `vike_secrets::save_credentials_to_store` — a keyed upsert of named
//! DEPLOYMENT rows — into the settings database, the only credential store, which REFUSES a box
//! with no database rather than creating one. [`AccountManagerCredentialHome`] and [`AccountManagerCredentialWrite`] describe
//! WHERE a GUI write lands: the settings DIRECTORY that database sits in.
//!
//! The write lives in `vike-secrets` because it has a second caller on the far side of the layer
//! graph: a venue bridge persisting a REFRESHED OAuth grant (`vike_ctrader::transport::token_store`), and this
//! crate is layer 75 and links `egui` — a headless daemon cannot reach it. `vike-secrets` (layer
//! 15) is reachable from both sides.
//!
//! SECURITY (unchanged): never log a secret value. Callers must log only non-secret facts ("saved
//! credentials for binance/live"), never the key/secret/passphrase text itself.
//!
//! # The durable RECORD for a CREDENTIAL write is written by the write itself, in `vike-secrets`
//!
//! For the CREDENTIAL-plane writes (an account's or a venue's credentials, a deployment row, an
//! account row's lifecycle) the write and its `vike_change_journal` record are ONE call, so the two
//! cannot drift apart at a call site — `vike_secrets::save_owned_credentials`,
//! `vike_secrets::save_credentials_to_store` and `vike_secrets::edit_account_in`
//! (`docs/decisions/0086`). The GUI call sites (`crate::view`'s Save arm and `crate::view`'s
//! account strip) call those directly. (This module
//! once re-exported the write as `save_credentials` and wrapped it as `save_credentials_journalled`;
//! `docs/decisions/0036` cites both names here, so they stay named. Both are deleted.)
//!
//! What stays here: [`AccountManagerCredentialHome`]'s `journal()` accessor hands out a bare
//! `ChangeJournal` — the Data Manager's Venues (arming) sub-tab reuses this SAME boot-resolved
//! ledger for a `set_setting` record, which is not a credential write at all and has no `vike-secrets`
//! primitive to collapse into. [`AccountManagerCredentialWrite`] therefore carries `journal` for that caller,
//! ALONGSIDE the `process` cell the two journalled calls need — see that field's doc.
//!
//! The OTHER credential writers (the CLI, the ctrader token rotation, the tradehub control wire)
//! still append their OWN record directly; moving them onto the two `vike-secrets` primitives above
//! is follow-up work, tracked beside 0086.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use vike_change_journal::{ChangeJournal, ChangeProcess};

/// How many credential-plane writes this process's editor has made: a field's Save or Remove, an
/// account row's act. A REFUSED write counts too, since the rows it was refused about are the rows
/// to read again (the rule `crate::view`'s account-row cache already keeps).
///
/// A screen that holds the store's answer between frames compares [`credential_writes`] and reads
/// again when it moved, instead of opening the store every frame (`vike_app_core`'s
/// `DataManagerCredentialReads`, the Data window's Gateway, Backend and Instruments screens).
static CREDENTIAL_WRITES: AtomicU64 = AtomicU64::new(0);

/// The editor's credential writes so far (`CREDENTIAL_WRITES`).
#[must_use]
pub fn credential_writes() -> u64 {
    CREDENTIAL_WRITES.load(Ordering::Acquire)
}

/// Count one write, once it has returned (the editor writes on the paint thread, so the next frame's
/// read sees it).
pub(crate) fn count_credential_write() {
    CREDENTIAL_WRITES.fetch_add(1, Ordering::AcqRel);
}

/// WHERE a credential write lands, and WHERE it — or an unrelated `set_setting` write sharing this
/// boot's ledger — is RECORDED. Every field resolved by the composition root's ONE boot walk.
///
/// ⚠ **`settings_dir` is a parameter for the same reason `journal` is.** Both of these used to be
/// found rather than given, by `_from`-less resolvers — `$VIKE_SETTINGS_DIR`-BLIND, answering with
/// whatever the working directory happens to sit above. That is already the defect the root
/// `CLAUDE.md`'s one-walk-decides rule exists for, and adding a journal made it worse in a new way:
/// a ledger resolved from the boot's state directory while the STORE was resolved by a second walk
/// could record a write to a store in a different project than the one it landed in. One
/// resolution now produces both.
///
/// `now_ms` is here for `vike_change_journal`'s purity rule — that module reads no clock, so
/// the instant is a parameter all the way down, exactly as
/// `vike_tradehub::audit::SettingsWriteAudit`'s own `now_ms` field is.
#[derive(Debug, Clone, Copy)]
pub struct AccountManagerCredentialWrite<'a> {
    /// The settings DIRECTORY (`<project>/settings`), as the root resolved it — what
    /// `vike_secrets::save_owned_credentials`, `save_credentials_to_store` and `edit_account_in`
    /// all take. The store is the settings database inside it (`vike_secrets::db_path_in`).
    pub settings_dir: &'a Path,
    /// The durable ledger. `None` — a root whose boot walk found no project, and every caller that
    /// predates the journal — writes NOTHING rather than inventing a path: an append-only record in
    /// a guessed directory is worse than a counted absence. The store write still happens.
    ///
    /// ⚠ Read by exactly ONE caller now: the Venues (arming) sub-tab's `set_setting` write. The
    /// CREDENTIAL-plane writers (the Gateway screen editor's Save and Remove, the account-row strip)
    /// derive their OWN journal from the settings directory instead — see
    /// `vike_secrets::save_credentials_to_store`'s doc for why that is a strengthening.
    pub journal: Option<&'a ChangeJournal>,
    /// The writing process's identity — threaded to the `vike_secrets` writers' journal context
    /// (`ChangeStampCredential` / `ChangeStamp`). It is NOT derivable from `journal` above: `ChangeJournal` exposes no accessor for
    /// the `ChangeProcess` it was built with, on purpose (nothing outside `vike_change_journal` reads
    /// it back once it exists), so this is threaded down as its own cell rather than "found" a
    /// second time from the other field.
    pub process: &'a ChangeProcess,
    /// The instant to stamp the record with, supplied by the caller.
    pub now_ms: i64,
}

/// The OWNED pair a composition root resolves ONCE at startup and then hands out as a
/// [`AccountManagerCredentialWrite`] per write: the settings directory, and the durable ledger.
///
/// # Why this type exists at all, rather than four lines in `main.rs`
///
/// It is the resolution that carries the bug history, so it belongs where a test can reach it.
/// `vike-desktop` is EXCLUDED from the derived CI roster — `crates/vike-ops/tests/
/// ci_excluded_gui_shell_ratchet.rs` ratchets its size for exactly that reason, because a line added
/// to that crate is a line no test will run. This crate is in the roster, so [`AccountManagerCredentialHome::resolve`]
/// is covered by `credential_write_tests.rs` (declared below) and the shell keeps two call lines.
///
/// # The rule it encodes: ONE walk decides
///
/// `settings_dir` and `state_dir` are the composition root's own boot answers (`vike_boot::Booted`),
/// and every path here is DERIVED from them. Both used to be found instead, by the `_from`-less
/// resolvers, which are `$VIKE_SETTINGS_DIR`-BLIND — while the credential grid those surfaces render
/// RENDERS from a read that honours the override. On a deployment that names its settings directory
/// the editor therefore showed one project's keys and Save wrote another project's store, silently
/// and with no error on either side.
///
/// # The two `None`s mean different things, and neither is an invention
///
/// `state_dir: None` (no project above the working directory) yields NO journal: nothing is
/// recorded, rather than an append-only ledger in a guessed directory.
///
/// `settings_dir: None` has no such option — a write needs somewhere to go — so it falls back to
/// [`vike_secrets::workspace_settings_dir_from`] with no override, the same walk the boot itself
/// performed. ⚠ That is the one bare walk left in this path, and it is confined here on purpose: it
/// is reached ONLY when the root resolved no project, in which case the walk finds none either and
/// answers with the relative last-resort `settings/`. Whenever the root has an answer, the root's
/// answer wins.
#[derive(Debug, Clone)]
pub struct AccountManagerCredentialHome {
    settings_dir: std::path::PathBuf,
    journal: Option<ChangeJournal>,
    process: ChangeProcess,
}

impl AccountManagerCredentialHome {
    /// Derive both homes from the boot's own two answers. See the type doc for what each `None`
    /// means.
    pub fn resolve(
        settings_dir: Option<&Path>,
        state_dir: Option<&Path>,
        process: ChangeProcess,
    ) -> Self {
        Self {
            settings_dir: match settings_dir {
                Some(dir) => dir.to_path_buf(),
                None => vike_secrets::workspace_settings_dir_from(None),
            },
            journal: state_dir.map(|dir| ChangeJournal::in_state_dir(dir, process.clone())),
            process,
        }
    }

    /// The settings directory this project's credential writes land in — the settings database
    /// inside it is the store.
    pub fn settings_dir(&self) -> &Path {
        &self.settings_dir
    }

    /// The durable ledger, or `None` when the boot walk found no project.
    pub fn journal(&self) -> Option<&ChangeJournal> {
        self.journal.as_ref()
    }

    /// The process identity every write from this home stamps its journal record with.
    pub fn process(&self) -> &ChangeProcess {
        &self.process
    }

    /// One write's worth of context. `now_ms` is per-write because
    /// `vike_change_journal` reads no clock — the instant is a parameter all the way down.
    pub fn write_context(&self, now_ms: i64) -> AccountManagerCredentialWrite<'_> {
        AccountManagerCredentialWrite {
            settings_dir: &self.settings_dir,
            journal: self.journal.as_ref(),
            process: &self.process,
            now_ms,
        }
    }
}

#[path = "credential_write_tests.rs"]
#[cfg(test)]
mod credential_write_tests;
