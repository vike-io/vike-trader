//! Safe `.env` upsert (in-app credential editing) — **the implementation now lives in
//! `vike_secrets::env_write`** and this module is the re-export under its historical path.
//!
//! It moved because it grew a second caller on the far side of the layer graph: a venue bridge
//! persisting a REFRESHED OAuth grant (`vike_ctrader::token_store`) needs the same byte-preserving
//! upsert, and this crate is layer 75 and links `egui` — a headless daemon cannot reach it. The
//! transform is the one place the store's byte-level preservation is enforced and tested, so a
//! verbatim second copy would be two things to keep in step about one file; `vike-secrets` (layer
//! 15, zero dependencies, and the crate that already owns the store) is reachable from both sides.
//!
//! Every call site here is unchanged: [`upsert_env`] and [`save_credentials`] resolve exactly as
//! before.
//!
//! SECURITY (unchanged): never log a secret value. Callers must log only non-secret facts ("saved
//! credentials for binance/live"), never the key/secret/passphrase text itself.
//!
//! # …and the DURABLE record: [`save_credentials_journalled`]
//!
//! A credential rotation is a change to how this project trades, and until now the only trace of one
//! was a `tracing::info!` line — which `vike_model::change_journal`'s module doc measures as
//! *deleted within days* (daily rotation, a bounded file count) and, on the the CI box daemon,
//! *never written at all* (`VIKE_LOG_FILE_LEVEL=warn` beats the config, and the line is `info`).
//! [`save_credentials_journalled`] performs the store write and appends the append-only
//! `credential_write` record TOGETHER, so the two cannot drift apart at a call site.
//!
//! ⚠ **The wrapper lives HERE rather than in `vike-secrets`, and that is forced.**
//! `crates/vike-secrets/Cargo.toml` declares a literally EMPTY `[dependencies]` — the property that
//! lets `vike-bridge-core` (which owns the transport stack) and `vike-cli` (transport-free,
//! DataFusion-free) both link it — so that crate cannot name `vike-model` and cannot journal
//! anything. The CALLER writes the record. This crate is layer 75 and links `egui`, so the wrapper
//! serves the two GUI sites; `crates/bridges/ctrader/src/token_store.rs`'s `persist` is at layer 40
//! and cannot reach it, so it appends the same record itself.

use std::io;
use std::path::Path;

use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome};

pub use vike_secrets::{save_credentials, upsert_env};

/// WHERE a credential write lands, and WHERE it is recorded — every field resolved by the
/// composition root's ONE boot walk.
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
    /// The credential store file itself (`<project>/settings/secrets.env`), as the root resolved it.
    pub store: &'a Path,
    /// The durable ledger. `None` — a root whose boot walk found no project, and every caller that
    /// predates the journal — writes NOTHING rather than inventing a path: an append-only record in
    /// a guessed directory is worse than a counted absence. The store write still happens.
    pub journal: Option<&'a ChangeJournal>,
    /// The instant to stamp the record with, supplied by the caller.
    pub now_ms: i64,
}

/// The OWNED pair a composition root resolves ONCE at startup and then hands out as a
/// [`CredentialWrite`] per write: the credential store's path, and the durable ledger.
///
/// # Why this type exists at all, rather than four lines in `main.rs`
///
/// It is the resolution that carries the bug history, so it belongs where a test can reach it.
/// `vike-app` is EXCLUDED from the derived CI roster — `crates/vike-ops/tests/
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
}

impl CredentialHome {
    /// Derive both homes from the boot's own two answers. See the type doc for what each `None`
    /// means.
    pub fn resolve(
        settings_dir: Option<&Path>,
        state_dir: Option<&Path>,
        process: vike_model::change_journal::Proc,
    ) -> Self {
        Self {
            // `join` on the `OsStr`, so a settings path that is not valid UTF-8 resolves like any
            // other rather than degrading to the walk.
            store: match settings_dir {
                Some(dir) => dir.join(vike_secrets::SECRETS_FILE),
                None => vike_secrets::workspace_dotenv_path(),
            },
            journal: state_dir.map(|dir| ChangeJournal::in_state_dir(dir, process)),
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

    /// One write's worth of context. `now_ms` is per-write because
    /// `vike_model::change_journal` reads no clock — the instant is a parameter all the way down.
    pub fn write_ctx(&self, now_ms: i64) -> CredentialWrite<'_> {
        CredentialWrite { store: &self.store, journal: self.journal.as_ref(), now_ms }
    }
}

/// The ONE sanctioned credential write, plus its durable record — [`save_credentials`] and a
/// `credential_write` append, together.
///
/// # What reaches the ledger, and why a value cannot
///
/// `updates` carries `(KEY, VALUE)` pairs, and this function projects `.0` and nothing else. That is
/// belt: the braces are that `vike_model::change_journal::Change::credential_write` **takes no
/// old/new/value parameter at all** and `CredentialTarget`'s fields are private, so there is no
/// signature here that could accept one. Key NAMES are recorded deliberately — they are not secret
/// (`vike-cli secrets list` prints them by an explicit decision in the root `CLAUDE.md`) and they are
/// the whole reason the record is answerable: *"when did I last change the okx passphrase"* needs
/// the name, and "3 credentials changed" answers nothing.
///
/// # What it does NOT claim
///
/// [`save_credentials`] returns `io::Result<()>`. It does not report which keys it replaced in place
/// and which it appended as new lines, so neither does the record: `keys` is what the caller ASKED
/// to write. Inventing an updated-vs-inserted split here would mean re-reading and re-parsing the
/// store to guess at it, which is a second reader of the user's only copy of their live keys for a
/// cell nobody asked for.
///
/// # Ordering, and the two failure paths
///
/// The store write happens FIRST and its error is returned unchanged — a rotation that did not land
/// must not leave a record saying it did. A JOURNAL failure, by contrast, cannot fail the call: the
/// credential IS on disk, and reporting failure would send a caller down an error path for a write
/// that succeeded. It is logged at `error` deliberately, that being the one level
/// `deploy/vike-tradehub.service`'s `VIKE_LOG_FILE_LEVEL=warn` still lets through — the same
/// argument `vike_tradehub::audit::record_settings_write` makes for its own journal failure.
pub fn save_credentials_journalled(
    creds: CredentialWrite<'_>,
    actor: Actor,
    venue: &str,
    tier: &str,
    updates: &[(String, String)],
) -> io::Result<()> {
    save_credentials(creds.store, updates)?;

    let Some(journal) = creds.journal else { return Ok(()) };

    // ⚠ `.0` ONLY — the projection that makes a value unrepresentable downstream even before the
    // record type refuses to hold one.
    let keys: Vec<&str> = updates.iter().map(|(key, _value)| key.as_str()).collect();
    // The FILE NAME, not the path: `CredentialTarget::store` is documented as `"secrets.env"`, and a
    // record is bounded to one page — a deep project path would spend that budget saying what the
    // journal's own location already says (it sits under the same `<project>/settings`).
    let store =
        creds.store.file_name().and_then(|n| n.to_str()).unwrap_or(vike_secrets::SECRETS_FILE);
    let change = Change::credential_write(Outcome::Applied, actor, store, venue, tier, &keys);
    if let Err(e) = journal.append(creds.now_ms, &change) {
        tracing::error!(
            error = %e,
            dir = %journal.dir().display(),
            venue,
            tier,
            "credential write NOT recorded to the change journal (the keys ARE saved)"
        );
    }
    Ok(())
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
        let out = upsert_env(existing, &updates);

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
    /// Save wrote a blind walk's — and it is asserted here rather than in `vike-app` deliberately:
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
    }

    /// The re-export is a MOVE, not a fork: this crate's `save_credentials` is the one in
    /// `vike-secrets`. Pinned so a future "just copy it back" cannot pass review silently.
    #[test]
    fn the_writer_is_the_shared_one_not_a_local_copy() {
        /// The shared writer's signature, named so the two bindings below stay under
        /// `clippy::type_complexity` (a `-D warnings` gate) without an `allow`.
        type SaveFn = fn(&std::path::Path, &[(String, String)]) -> std::io::Result<()>;
        let shared: SaveFn = vike_secrets::save_credentials;
        let here: SaveFn = save_credentials;
        assert!(std::ptr::fn_addr_eq(shared, here), "vike-connections must not fork the upsert");
    }
}
