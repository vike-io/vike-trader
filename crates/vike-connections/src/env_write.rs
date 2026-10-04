//! The in-app credential editor's WRITE CONTEXT ([`CredentialHome`], [`CredentialWrite`]). The safe
//! `.env` upsert itself lives in `vike_secrets::env_write`, and callers name it there:
//! `vike_secrets::save_credentials` / `vike_secrets::upsert_env`.
//!
//! ⚠ This module used to re-export those two functions under its historical path. The re-export was
//! deleted 2026-09-27 under the owner's 2026-09-18 ruling that a symbol has ONE name — a second
//! path for the same function is a name that can rot.
//!
//! It moved because it grew a second caller on the far side of the layer graph: a venue bridge
//! persisting a REFRESHED OAuth grant (`vike_ctrader::token_store`) needs the same byte-preserving
//! upsert, and this crate is layer 75 and links `egui` — a headless daemon cannot reach it. The
//! transform is the one place the store's byte-level preservation is enforced and tested, so a
//! verbatim second copy would be two things to keep in step about one file; `vike-secrets` (layer
//! 15) is reachable from both sides.
//!
//! SECURITY (unchanged): never log a secret value. Callers must log only non-secret facts ("saved
//! credentials for binance/live"), never the key/secret/passphrase text itself.
//!
//! # The durable RECORD for a CREDENTIAL write moved with the write — into `vike-secrets` itself
//!
//! ⚠ **This module used to carry `save_credentials_journalled` and `edit_account_journalled`: two
//! wrappers that performed a store write and THEN appended the durable `vike_model::change_journal`
//! record, so the two calls could not drift apart at a call site.**
//! `docs/decisions/0086` collapses that pattern the same way
//! its own settings writer collapses "write, then journal what committed" into one primitive: for
//! the two CREDENTIAL-plane writes (a venue's own keys, an account row's lifecycle) the write and
//! its ledger record are now ONE call, and it lives in `vike-secrets` —
//! `vike_secrets::save_credentials_to_store_journalled` and
//! `vike_secrets::edit_account_in_journalled` — rather than one layer up here. The two GUI call
//! sites this served (`crate::view`'s Save arm and `crate::view`'s account strip) now call those
//! directly.
//!
//! What did NOT move: [`CredentialHome`]'s `journal()` accessor still hands out a bare
//! `ChangeJournal` — the Data Manager's Venues (arming) sub-tab reuses this SAME boot-resolved
//! ledger for a `set_setting` record, which is not a credential write at all and has no `vike-secrets`
//! primitive to collapse into. [`CredentialWrite`] therefore still carries `journal` (unchanged) for
//! that caller, ALONGSIDE the new `proc` cell the two collapsed calls need — see that field's doc.
//!
//! Migrating every OTHER credential writer (the CLI, the ctrader token rotation, the tradehub
//! control wire) onto the two `vike-secrets` primitives above is follow-up work, tracked beside
//! 0086; nothing about this move changes their behaviour, each of which still appends its OWN
//! record directly for the layering reason this crate's own doc used to state.

use std::path::Path;

use vike_model::change_journal::{ChangeJournal, Proc};

/// WHERE a credential write lands, and WHERE it — or an unrelated `set_setting` write sharing this
/// boot's ledger — is RECORDED. Every field resolved by the composition root's ONE boot walk.
///
/// ⚠ **`store` is a parameter for the same reason `journal` is.** Both of these used to be found
/// rather than given: the GUI Save arm called `vike_bridge_core::credentials::workspace_dotenv_path`
/// and the proxy box called `vike_secrets::workspace_dotenv_path`, and BOTH of those are the
/// `_from`-less resolvers — `$VIKE_SETTINGS_DIR`-BLIND, answering with whatever the working
/// directory happens to sit above. That is already the defect the root `CLAUDE.md`'s
/// one-walk-decides rule exists for, and adding a journal made it worse in a new way: a ledger
/// resolved from the boot's state directory while the STORE was resolved by a second walk could
/// record a write to a file in a different project than the one it landed in. One resolution now
/// produces both.
///
/// `now_ms` is here for `vike_model::change_journal`'s purity rule — that module reads no clock, so
/// the instant is a parameter all the way down, exactly as
/// `vike_tradehub::audit::SettingsWriteAudit`'s own `now_ms` field is.
#[derive(Debug, Clone, Copy)]
pub struct CredentialWrite<'a> {
    /// The credential FILE's path (`<project>/settings/secrets.env`), as the root resolved it — the
    /// handle for the settings directory, whose backend decides where a write lands (the database
    /// on a migrated box, this file otherwise).
    /// `vike_secrets::settings_dir_of_store` is how a caller reaching for the settings DIRECTORY —
    /// what `vike_secrets::save_credentials_to_store_journalled` and `edit_account_in_journalled`
    /// both take — derives it from this.
    pub store: &'a Path,
    /// The durable ledger. `None` — a root whose boot walk found no project, and every caller that
    /// predates the journal — writes NOTHING rather than inventing a path: an append-only record in
    /// a guessed directory is worse than a counted absence. The store write still happens.
    ///
    /// ⚠ Read by exactly ONE caller now: the Venues (arming) sub-tab's `set_setting` write. The two
    /// CREDENTIAL-plane writers (the Connections editor's Save, the account-row strip) derive their
    /// OWN journal from the settings directory instead — see
    /// `vike_secrets::save_credentials_to_store_journalled`'s doc for why that is a strengthening.
    pub journal: Option<&'a ChangeJournal>,
    /// The writing process's identity — threaded to
    /// `vike_secrets::save_credentials_to_store_journalled` / `edit_account_in_journalled`'s journal
    /// context. It is NOT derivable from `journal` above: `ChangeJournal` exposes no accessor for
    /// the `Proc` it was built with, on purpose (nothing outside `vike_model::change_journal` reads
    /// it back once it exists), so this is threaded down as its own cell rather than "found" a
    /// second time from the other field.
    pub proc: &'a Proc,
    /// The instant to stamp the record with, supplied by the caller.
    pub now_ms: i64,
}

/// The OWNED pair a composition root resolves ONCE at startup and then hands out as a
/// [`CredentialWrite`] per write: the credential store's path, and the durable ledger.
///
/// # Why this type exists at all, rather than four lines in `main.rs`
///
/// It is the resolution that carries the bug history, so it belongs where a test can reach it.
/// `vike-desktop` is EXCLUDED from the derived CI roster — `crates/vike-ops/tests/
/// ci_excluded_gui_shell_ratchet.rs` ratchets its size for exactly that reason, because a line added
/// to that crate is a line no test will run. This crate is in the roster, so [`CredentialHome::resolve`]
/// is covered by the suite below and the shell keeps two call lines.
///
/// # The rule it encodes: ONE walk decides
///
/// `settings_dir` and `state_dir` are the composition root's own boot answers (`vike_boot::Booted`),
/// and every path here is DERIVED from them. Both used to be found instead: the GUI Save arm called
/// `vike_bridge_core::credentials::workspace_dotenv_path` and the Data Manager's proxy box called
/// `vike_secrets::workspace_dotenv_path` — the `_from`-less resolvers, which are
/// `$VIKE_SETTINGS_DIR`-BLIND — while the credential grid those surfaces render RENDERS from a read
/// that honours the override. On a deployment that names its settings directory the editor therefore
/// showed one project's keys and Save wrote another project's file, silently and with no error on
/// either side.
///
/// # The two `None`s mean different things, and neither is an invention
///
/// `state_dir: None` (no project above the working directory) yields NO journal: nothing is
/// recorded, rather than an append-only ledger in a guessed directory.
///
/// `settings_dir: None` has no such option — a write needs somewhere to go — so it falls back to
/// [`vike_secrets::workspace_dotenv_path`], which is byte-identical to what both call sites did
/// before. ⚠ That is the one bare walk left in this path, and it is confined here on purpose: it is
/// reached ONLY when the root resolved no project, in which case the walk finds none either and
/// answers with the same relative `settings/secrets.env` it always did. Whenever the root has an
/// answer, the root's answer wins.
#[derive(Debug, Clone)]
pub struct CredentialHome {
    store: std::path::PathBuf,
    journal: Option<ChangeJournal>,
    proc: Proc,
}

impl CredentialHome {
    /// Derive both homes from the boot's own two answers. See the type doc for what each `None`
    /// means.
    pub fn resolve(settings_dir: Option<&Path>, state_dir: Option<&Path>, process: Proc) -> Self {
        Self {
            // `join` on the `OsStr`, so a settings path that is not valid UTF-8 resolves like any
            // other rather than degrading to the walk.
            store: match settings_dir {
                Some(dir) => dir.join(vike_secrets::SECRETS_FILE),
                None => vike_secrets::workspace_dotenv_path(),
            },
            journal: state_dir.map(|dir| ChangeJournal::in_state_dir(dir, process.clone())),
            proc: process,
        }
    }

    /// The credential store this project writes.
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// The durable ledger, or `None` when the boot walk found no project.
    pub fn journal(&self) -> Option<&ChangeJournal> {
        self.journal.as_ref()
    }

    /// The process identity every write from this home stamps its journal record with.
    pub fn proc(&self) -> &Proc {
        &self.proc
    }

    /// One write's worth of context. `now_ms` is per-write because
    /// `vike_model::change_journal` reads no clock — the instant is a parameter all the way down.
    pub fn write_ctx(&self, now_ms: i64) -> CredentialWrite<'_> {
        CredentialWrite {
            store: &self.store,
            journal: self.journal.as_ref(),
            proc: &self.proc,
            now_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::credentials::parse_dotenv;
    use vike_model::change_journal::Proc;

    // NOTE: every value below is a dummy placeholder ("test-key-123" etc), never a real secret.
    //
    // The transform's OWN suite (byte-preservation, ordering, quoting, atomic write) lives with the
    // implementation in `vike_secrets::env_write`. What stays here is the property this crate is
    // responsible for: that the GUI writes the exact KEY NAMES the venue loaders read.

    /// Dukascopy's DEMO2 keys (the EU demo account, added alongside DEMO1's existing edit form)
    /// upsert under the exact names `crate::status::dukascopy_configured` and
    /// `vike_dukascopy::config` read, and leave DEMO1 + any unrelated key untouched.
    #[test]
    fn dukascopy_demo2_keys_write_correct_names_and_preserve_others() {
        let existing = "\
DUKASCOPY_DEMO1_LOGIN=demo1-login-old
DUKASCOPY_DEMO1_PASSWORD=demo1-pass-old
UNRELATED_KEY=keep-me
";
        let updates = vec![
            ("DUKASCOPY_DEMO2_LOGIN".to_string(), "demo2-login-new".to_string()),
            ("DUKASCOPY_DEMO2_PASSWORD".to_string(), "demo2-pass-new".to_string()),
        ];
        let out = vike_secrets::upsert_env(existing, &updates);

        assert!(out.contains("DUKASCOPY_DEMO2_LOGIN=demo2-login-new"));
        assert!(out.contains("DUKASCOPY_DEMO2_PASSWORD=demo2-pass-new"));
        // DEMO1 and the unrelated key are untouched.
        assert!(out.contains("DUKASCOPY_DEMO1_LOGIN=demo1-login-old"));
        assert!(out.contains("DUKASCOPY_DEMO1_PASSWORD=demo1-pass-old"));
        assert!(out.contains("UNRELATED_KEY=keep-me"));

        // Round-trips through the real dotenv parser with the right key names.
        let parsed = parse_dotenv(&out);
        assert_eq!(
            parsed.get("DUKASCOPY_DEMO2_LOGIN").map(String::as_str),
            Some("demo2-login-new")
        );
        assert_eq!(
            parsed.get("DUKASCOPY_DEMO2_PASSWORD").map(String::as_str),
            Some("demo2-pass-new")
        );
    }

    /// ONE walk decides: both homes come off the composition root's own answers, and the ledger
    /// sits under the SAME `<project>/settings` the store does.
    ///
    /// This is the property the GUI had backwards — the grid read an override-honouring path while
    /// Save wrote a blind walk's — and it is asserted here rather than in `vike-desktop` deliberately:
    /// that crate is outside the derived CI roster, so an assertion living there would be run by
    /// nothing.
    #[test]
    fn both_homes_are_derived_from_the_boots_own_two_answers() {
        let settings = Path::new("/srv/vike-<unit>/settings");
        let state = settings.join("state");
        let home =
            CredentialHome::resolve(Some(settings), Some(&state), Proc::new("vike-test", 1, "0"));

        assert_eq!(home.store(), settings.join(vike_secrets::SECRETS_FILE));
        assert_eq!(
            home.journal().expect("a state dir yields a ledger").dir(),
            state.join(vike_model::change_journal::CHANGES_SUBDIR)
        );
        // …and the two really are siblings under one project, which is the whole point: a ledger
        // describing a store in a DIFFERENT project is the failure this derivation removes.
        assert_eq!(
            home.journal().unwrap().dir().parent().and_then(Path::parent),
            home.store().parent()
        );
        assert_eq!(home.proc().bin, "vike-test");
    }

    /// No state directory ⇒ NO ledger. Nothing is recorded, rather than an append-only record in a
    /// guessed directory — the `None`-handle behaviour `vike_model::change_journal` already pins.
    #[test]
    fn a_project_less_boot_gets_no_ledger_rather_than_an_invented_one() {
        let home = CredentialHome::resolve(
            Some(Path::new("/srv/vike-<unit>/settings")),
            None,
            Proc::new("vike-test", 1, "0"),
        );
        assert!(home.journal().is_none(), "a guessed ledger location is worse than none");
        assert!(home.write_ctx(0).journal.is_none(), "…and the per-write context agrees");
        // The STORE still resolves, because a write has to go somewhere.
        assert!(home.store().ends_with(vike_secrets::SECRETS_FILE));
    }

    /// An unresolved settings directory falls back to the bare walk — byte-identical to what both
    /// call sites did before this type existed, so the no-project case is unchanged rather than
    /// newly refused.
    #[test]
    fn a_settings_less_boot_falls_back_to_the_historical_walk() {
        let home = CredentialHome::resolve(None, None, Proc::new("vike-test", 1, "0"));
        assert_eq!(home.store(), vike_secrets::workspace_dotenv_path());
        assert_eq!(
            home.store().file_name().and_then(|n| n.to_str()),
            Some(vike_secrets::SECRETS_FILE),
            "whatever the walk answered, it still names the store"
        );
    }

    /// `write_ctx` hands out exactly what it holds, and the INSTANT is per-write — the journal reads
    /// no clock, so two records from one home can carry two timestamps.
    #[test]
    fn the_write_context_carries_the_homes_paths_and_the_callers_instant() {
        let settings = Path::new("/srv/vike-<unit>/settings");
        let home = CredentialHome::resolve(
            Some(settings),
            Some(&settings.join("state")),
            Proc::new("vike-test", 1, "0"),
        );
        let a = home.write_ctx(111);
        let b = home.write_ctx(222);
        assert_eq!((a.now_ms, b.now_ms), (111, 222));
        assert_eq!(a.store, home.store());
        assert!(a.journal.is_some() && b.journal.is_some());
        assert_eq!((a.proc.bin.as_str(), b.proc.bin.as_str()), ("vike-test", "vike-test"));
    }
}
