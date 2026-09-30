//! **Which Dukascopy ACCOUNT a mount is for, which one holds the process's ONE sidecar, and what
//! the venue answers back** — the mount-only third of a split decision 0088 made explicit.
//!
//! # Split with `vike-dukascopy` (decision 0088, Verdict 3)
//!
//! This file used to hold the whole resolution — including which BROKER a credential row names —
//! placed here per
//! [0065](../../../../docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md)'s own
//! citations of it. Decision
//! [0088](../../../../docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md) moved the
//! PURE half — `resolve_account`, `row_account`, `DukascopyMount`, `DukascopyRefusal`'s
//! resolution-outcome variants, `confirmation_for` — down into
//! `vike_dukascopy::account` (re-exported at that crate's root), because which broker a credential
//! row names is a fact about Dukascopy's own credential grammar, needing nothing `vike-mount`-
//! specific to answer. **What stays here, and why:**
//!
//! * [`resolve_in`] — a thin wrapper over [`AccountDirectory`] (the composition root's ONE store
//!   read, carried on `crate::MountPolicy::accounts`). It calls `vike_dukascopy::resolve_account`
//!   for everything it can answer and adds only the two arms `resolve_account` cannot reach on its
//!   own (nobody read the store at all; the store read but would not open).
//! * [`DukascopySidecarRefusal`]'s two variants — the refusals, in this venue's words. The
//!   one-sidecar RULE they report — which account holds the sidecar (`sidecar_holder` here until the
//!   venue mount contract made the rule generic) and the process-wide RAII claim under it — lives in
//!   `crates/vike-mount/src/exclusive.rs`, generic over any venue whose declaration is
//!   process-exclusive. It must NOT move into the bridge: `crates/bridges/dukascopy/tests/dukascopy_exec.rs`
//!   spawns stub sidecars repeatedly within one test process, and a process-wide guard living there
//!   would collide with that.
//! * [`record_confirmation`] — reads `vike_bridge_core::halt::declared_project_state_dir()`, a
//!   process-global fact no bridge reads today. It calls `vike_dukascopy::confirmation_for` (the
//!   pure builder that DID move) for the record and PARKS the result itself.
//!
//! Everything below documents the mount-only third: WHICH account this process mounts (over the
//! account-directory snapshot the root read), WHICH account gets the one sidecar, and what the
//! VENUE answers back once it has authenticated. `vike_dukascopy::account`'s own module doc is the
//! authority on which broker a row names and why `AccountLabel::Default` is always the Swiss bank.
//!
//! # ⚠ ONE sidecar per process — WHOSE it is, and the measurement that retires the limit
//!
//! [`crate::exclusive::holder`] decides which dukascopy account gets it, from the POLICY, before
//! anything is mounted; [`crate::exclusive::claim`] is the process-level backstop under that
//! decision. ⚠ Until 2026-09-15 there was no decision — the claim alone decided, first-come, and
//! the fan-out always mounts the DEFAULT account first, so the default always won and **no policy
//! could mount the second account at all**. The feature was unreachable, which is the defect
//! [`crate::exclusive::pick_holder`] exists to remove.
//!
//! ⚠ **Those two paragraphs can be read as contradicting each other, so read them together.** The
//! refusal below protects the account that already worked from a hazard NOBODY ASKED FOR — a second
//! sidecar started beside it, corrupting the cache both depend on. [`crate::exclusive::holder`]
//! takes that same account off the venue only when the operator NAMED another one in `policy.toml`,
//! which is a request rather than an accident, and it never starts a second sidecar to do it. One
//! is a silent degrade of a working capability; the other is an operator's own choice, said out
//! loud at the account that yields.
//!
//! Two concurrent sidecars are **unproven**, and the reason to refuse a second one
//! rather than report it is that the risk falls on the account that was already working:
//!
//! * `crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/Bridge.java` sets no platform
//!   cache directory (it builds its client with `ClientFactory.getDefaultInstance()` and never calls
//!   the SDK's cache-directory setter), so two JVMs of one user share the per-user default cache;
//! * `crates/bridges/dukascopy/CLAUDE.md`'s login-failure triage records what a corrupted local
//!   platform cache costs — an INSTANT `login failed` on **every** attempt until the directory is
//!   deleted by hand. That is a failure of both accounts, not of the new one, so admitting the
//!   second sidecar can take the FIRST account off the venue.
//!   `docs/decisions/0013-degrade-vs-refuse.md` licenses degrading the capability being ADDED; it
//!   does not license degrading one that already worked.
//! * `crates/bridges/dukascopy/src/exec.rs`'s `READY_TIMEOUT` is 300s and the fan-out mounts
//!   accounts SERIALLY, so two would also make a cold mount cost up to ten minutes before any other
//!   venue is reached.
//! * `crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s own header says to run its two
//!   logins separately. That note is about one ACCOUNT's session and is therefore not evidence about
//!   two accounts — it is recorded here as what the tree knows, not as the argument.
//!
//! **What retires it is a measurement, not a review**: give each sidecar its own platform cache
//! directory (the JForex `IClient` has a setter for it and `Bridge.java` calls none), then run two
//! real logins concurrently on a box with the SDK staged and show both reach a `ready` envelope. The
//! refusal is loud and names this paragraph, so an operator who hits it is not left guessing.
//!
//! # …and the OTHER direction: what the VENUE says about the account we chose
//!
//! Everything above is this module deciding which account to mount. [`record_confirmation`] is the
//! answer coming back: the sidecar's `ready` envelope carries `IAccount.getAccountId()`, and until
//! 2026-09-15 `crates/bridges/dukascopy/src/exec.rs`'s `spawn_with_program` discarded it at the one
//! moment it is knowable. That value is the credential-schema spec's §4.5 handshake writer for
//! `account.venue_account_id` and `account.last_verified_at` — two columns the tree had no writer
//! for at all, which is why *never verified* and *verified three weeks ago* looked identical to
//! *fine*.
//!
//! ⚠ **This module writes no store, and that is a MEASURED constraint.** The shipped unit runs under
//! `ProtectSystem=strict` with `ReadWritePaths=<project>/settings/state`, and the settings database
//! is outside it — so a mount-time `UPDATE` is `EROFS` on every deployment. The mount PARKS
//! (`vike_model::account_confirmation`, writable by construction) and `vike-cli secrets confirm`
//! folds. A DISAGREEMENT between the stored book and the venue's answer is reported at `error!`,
//! writes nothing, and never fails the mount — [`record_confirmation`] carries the whole argument,
//! including why a disagreement is not yet proof of a wrong broker.

use vike_bridge_core::account_directory::AccountDirectory;
use vike_dukascopy::{DukascopyMount, DukascopyRefusal, confirmation_for, resolve_account};
use vike_model::account_keys::AccountLabel;

/// **Why this process refuses to mount a dukascopy account over its ONE JForex sidecar.** Every arm
/// is a REFUSAL: the mount builds no exec client and stays paper.
///
/// ⚠ **These two variants used to live in `vike_dukascopy::DukascopyRefusal`, alongside the
/// pure-resolution ones, and decision 0088 split them out.** They are a fact about the WHOLE
/// PROCESS (only one JForex sidecar may run at a time) rather than about a credential row, so they
/// stay here even though the resolution they follow now lives in the bridge —
/// `vike_dukascopy::DukascopyRefusal`'s own doc argues the split from the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DukascopySidecarRefusal {
    /// **Another dukascopy account holds this process's one sidecar** — the POLICY-derived decline
    /// ([`crate::exclusive::holder`]), not a race. It is what the DEFAULT account gets on a box that arms a
    /// labelled dukascopy account, and what the extra accounts get on a box that arms several.
    ///
    /// ⚠ Raised only for an account whose CREDENTIALS resolved — `crate::make_engine_for_account`
    /// checks the holder below the credential read. An account that could not have armed anyway is
    /// told nothing, because it lost nothing.
    SidecarHeldByAnother { label: String, holder: String },
    /// A second dukascopy account asked for a sidecar in a process that already has one — the
    /// process-level BACKSTOP under [`crate::exclusive::holder`], which should already have refused it. See
    /// the module doc's ONE-sidecar section.
    SidecarAlreadyClaimed { label: String },
}

impl std::fmt::Display for DukascopySidecarRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DukascopySidecarRefusal::SidecarHeldByAnother { label, holder } => write!(
                f,
                "dukascopy account `{label}` stays PAPER: this process runs ONE JForex sidecar and \
                 `{holder}` holds it. Two sidecars share one JForex platform cache (the Java \
                 bridge sets no cache directory) and a corrupted cache makes EVERY login fail \
                 until the directory is deleted, so exactly one dukascopy account is armed per \
                 process. WHICH one is the policy's to choose: an `[accounts.dukascopy]` line \
                 above `paper` takes it, and with no such line the DEFAULT account keeps it. To \
                 trade both at once, give the second account its own project folder (its own \
                 VIKE_SETTINGS_DIR and credential store) and run a second process there; \
                 `crates/vike-mount/src/exclusive.rs`'s `pick_holder` carries the rule and \
                 `crates/vike-mount/src/dukascopy.rs`'s module doc names the measurement that \
                 retires it"
            ),
            DukascopySidecarRefusal::SidecarAlreadyClaimed { label } => write!(
                f,
                "dukascopy account `{label}` stays PAPER: this process already runs a JForex \
                 sidecar and a SECOND one is refused. Two sidecars share one JForex platform cache \
                 (the Java bridge sets no cache directory), and a corrupted cache makes EVERY \
                 login fail until the directory is deleted — so admitting the second account could \
                 take the FIRST one off the venue. Run the second account in its own project \
                 folder (its own VIKE_SETTINGS_DIR and credential store) until per-sidecar cache \
                 directories are proven; `crates/vike-mount/src/dukascopy.rs`'s module doc names \
                 the measurement that retires this"
            ),
        }
    }
}

/// **[`vike_dukascopy::resolve_account`] over the snapshot the composition ROOT read** — the only
/// door the mount and the arming projection reach the `account` table through.
///
/// ⚠ **This used to open the store itself**, at a settings directory taken from a process global
/// (`vike_bridge_core::halt::declared_project_state_dir`). That is the class
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` exists to ratchet down — a
/// library reading global configuration state its caller can neither see nor substitute — and the
/// gate could not see it, because neither account reader was one of `CREDENTIAL_STORE_READERS`'
/// names. Both halves are fixed: the readers are keyed now, and the read moved to the root
/// ([`AccountDirectory`]), which is where the credential MAP has always been loaded.
///
/// The four answers it can be handed, and what each means here:
///
/// * **UNREAD** — no root read the store (a test, a tool, a root that predates the field). The
///   DEFAULT account resolves to [`vike_dukascopy::DEFAULT_ACCOUNT`] and every labelled one is
///   REFUSED, which is exactly a `Backend::Files` box's answer and byte-identical to this arm
///   before any of this existed.
/// * **`Accounts::Unanswerable`** — a real store with no `account` table. Same shape, and the
///   refusal renders WHICH store and why.
/// * **`Accounts::Known`** — the table answered; [`vike_dukascopy::resolve_account`] does the rest.
/// * **an ERROR on either half** — the store exists and would not open. The DEFAULT account still
///   resolves (it never needed the table) and says so in the log; a labelled account is refused as
///   [`vike_dukascopy::DukascopyRefusal::StoreUnreadable`], a STORE failure reported as itself.
///
/// ⚠ **The KEY-NAME half is not optional for a labelled account**, and that is why its error is
/// reported rather than folded away. The row's credential-key OWNER PREFIX is the entire mapping
/// from a row to a broker; without the key names there is no answer, and the previous
/// `.ok().flatten()` turned "the store would not open" into "this row's key names name no broker" —
/// sending an operator to fix a row that was never at fault.
pub(crate) fn resolve_in(
    label: &AccountLabel,
    directory: &AccountDirectory,
) -> Result<DukascopyMount, DukascopyRefusal> {
    let default_mount =
        || DukascopyMount { account: vike_dukascopy::DEFAULT_ACCOUNT, row: None, book: None };
    let Some(rows) = directory.rows() else {
        // NOBODY READ THE STORE. Not "the store has no accounts": the DEFAULT account resolves
        // without the table and every labelled one is refused, rather than being coerced onto the
        // default account's broker.
        return match label.text() {
            None => Ok(default_mount()),
            Some(text) => Err(DukascopyRefusal::NoAccountTable {
                why: "this process read no settings store, so there is no `account` table to ask \
                      (a composition root fills `vike_mount::MountPolicy::accounts`)"
                    .to_string(),
                label: text.to_string(),
            }),
        };
    };
    // ⚠ A store that EXISTS and will not open is the loud case, and it is loud for a LABELLED
    // account — which cannot be identified without it — while the DEFAULT account still resolves.
    // That asymmetry is the byte-identity guarantee doing its job: a broken database must not take a
    // single-account box off a venue it has always traded, and the default account never needed the
    // table in the first place.
    let accounts = match rows {
        Ok(a) => a,
        Err(error) => {
            let Some(text) = label.text() else {
                tracing::error!(
                    venue = vike_dukascopy::recon_client::VENUE,
                    error,
                    "the settings store would not open; the DEFAULT dukascopy account still \
                     resolves to its own credential keys, and no labelled account can"
                );
                return Ok(default_mount());
            };
            return Err(DukascopyRefusal::StoreUnreadable {
                label: text.to_string(),
                what: "the `account` table",
                error: error.to_string(),
            });
        }
    };
    let keys = match directory.keys() {
        // The key names were read (`None` inside = the store carries no `account` table to key,
        // which `resolve_account` reads as "no row owns a known key family").
        Some(Ok(k)) => k,
        // NOT read at all — impossible beside a `Some` row answer, since a directory is built from
        // both reads at once, but it costs one arm to keep the two halves independent rather than
        // assume they move together.
        None => None,
        Some(Err(error)) => {
            let Some(text) = label.text() else {
                tracing::error!(
                    venue = vike_dukascopy::recon_client::VENUE,
                    error,
                    "the settings store's credential key NAMES would not read; the DEFAULT \
                     dukascopy account still resolves, without its row attached to the log line"
                );
                return Ok(default_mount());
            };
            return Err(DukascopyRefusal::StoreUnreadable {
                label: text.to_string(),
                what: "the account's credential key names — the fact that says which broker a row is",
                error: error.to_string(),
            });
        }
    };
    resolve_account(label, accounts, keys)
}

/// **THE HANDSHAKE HALF: record what the venue answered, and say the verdict out loud.**
///
/// Called once per successful sidecar login, with the account identifier
/// `vike_dukascopy::DukascopyExecutionClient::handshake_account` carries. It performs the
/// credential-schema spec's §4.5 handshake claim in the only shape a deployed daemon can:
///
/// # ⚠ It writes NO database, and that is a measured constraint rather than a preference
///
/// The shipped unit runs under `ProtectSystem=strict` with
/// `ReadWritePaths=<project>/settings/state`, and the settings database lives at
/// `<project>/settings/db/vike.db` — OUTSIDE it. A mount-time `UPDATE account …` fails `EROFS` on
/// every deployment this tree ships, the same wall `WireCommand::SetSetting` already hits. So this
/// PARKS a `vike_model::account_confirmation::ConfirmationRecord` in the state directory (writable
/// by construction — it is where the HALT sentinel and the rolling log already live) and
/// `vike-cli secrets confirm` folds it from a process that is not sandboxed. That module's doc
/// carries the whole argument; what belongs here is that this function is deliberately not a
/// writer, which is why `crates/vike-ops/tests/credential_writer_gate.rs` has no row for this file.
/// [`vike_dukascopy::confirmation_for`] is the pure builder this calls; it moved into the bridge
/// with the rest of the pure resolution (decision 0088) and this function is the only reason it is
/// still called from here rather than from the bridge itself.
///
/// # The verdict is LOGGED here as well as parked
///
/// The fold may be days away, or may never happen on a box nobody runs the CLI on. A wrong-broker
/// disagreement is the one finding that must not wait for it, so it is emitted at `error!` at the
/// moment it is discovered — the same class as the arm's own refusals, and for the same reason: the
/// operator has configured something that describes a real account and the store and the venue do
/// not agree about which one.
///
/// ⚠ **A disagreement does NOT take the venue down.** The session authenticated; refusing the mount
/// over a bookkeeping disagreement would degrade a capability that was working, which
/// `docs/decisions/0013-degrade-vs-refuse.md` does not license, and the credential-schema spec §8
/// rules exactly this case for the constraint's twin: *a violation discovered AT A HANDSHAKE is
/// REPORTED, not thrown*. What it does instead is write nothing: not the book (overwriting a stored
/// book with the handshake's would re-point an armed account at another broker in silence) and not
/// the timestamp (a row the venue has just contradicted is the single row that must not read
/// *verified today*). `vike_model::account_confirmation::verdict` carries both halves.
///
/// ⚠ **It cannot tell a wrong broker from a FORM mismatch, and it does not pretend to.** The
/// credential-schema spec §9 leaves the form of this identifier explicitly unsettled — the sidecar
/// sends `IAccount.getAccountId()`, the one frame this tree pins is login-shaped, and the books an
/// operator wrote by hand off Dukascopy's own page are numeric — so a box whose rows hold numbers
/// will see a disagreement on the first mount after this shipped. That is a REPORT and a one-time
/// reconciliation (`vike-cli secrets set-book --replace` with what the venue actually answered),
/// not a standing alarm; the message says so rather than asserting a wrong broker it cannot prove.
///
/// Nothing here can log a credential: the record holds a venue account id, a key PREFIX and a row
/// id, and the login and password never left the child's environment.
pub(crate) fn record_confirmation(mount: &DukascopyMount, handshake_account: &str) {
    use vike_model::account_confirmation::{Verdict, verdict};

    let venue = vike_dukascopy::recon_client::VENUE;
    let verdict = verdict(handshake_account, mount.book.as_deref());
    match &verdict {
        Verdict::Confirms => tracing::info!(
            venue,
            broker = mount.account.broker(),
            row = ?mount.row,
            book = %handshake_account,
            "dukascopy: the venue CONFIRMED the book this account row names"
        ),
        Verdict::Learns => tracing::info!(
            venue,
            broker = mount.account.broker(),
            row = ?mount.row,
            book = %handshake_account,
            "dukascopy: the venue NAMED this account's book, which the store did not know"
        ),
        Verdict::Disagrees { stored } => tracing::error!(
            venue,
            broker = mount.account.broker(),
            keys = mount.account.key_prefix(),
            row = ?mount.row,
            stored = %stored,
            venue_answered = %handshake_account,
            "dukascopy: ⚠ THE STORE AND THE VENUE DISAGREE about which account these credentials \
             are. The settings database says this row's venue account is `{stored}` and the \
             JForex handshake answered `{handshake_account}`. NOTHING WAS WRITTEN — not the book, \
             and not last_verified_at either, because a row the venue has just contradicted must \
             not read as verified. The session continues: it authenticated, and taking the venue \
             away over a bookkeeping disagreement would be worse than reporting it. ⚠ This is NOT \
             yet proof of a wrong broker: the FORM of this identifier is unsettled \
             (docs/superpowers/specs/2026-09-14-the-credential-schema.md §9 — the sidecar sends \
             IAccount.getAccountId(), which may be login-shaped, while a book read off Dukascopy's \
             own page is numeric), so a stored number against a login-shaped answer lands here too. \
             Resolve it once: check the account in the JForex platform, then either \
             `vike-cli secrets set-book --id <id> --venue-account-id {handshake_account} --replace` \
             if `{handshake_account}` is this account, or move the credentials if it is not"
        ),
    }

    // The park. A process with no declared project has no state directory to write into and no row
    // to address either — that is a test, a tool, or a root that declared nothing, and it is the
    // same population [`resolve_in`] already answers the UNREAD-store way for.
    //
    // ⚠ The DECLARED state directory, never a fresh walk: `declared_project_state_dir` is the one
    // the boot resolved (and therefore the one `$VIKE_SETTINGS_DIR` moved), and it is exactly the
    // path the shipped unit's `ReadWritePaths` grants. A second walk would answer for whatever the
    // working directory sits above, and could land the park outside the sandbox's one writable
    // path. That is the same rule [`resolve_in`]'s doc states for the account table: a mount reads
    // the project the ROOT resolved, never one it re-derives.
    let Some(state) = vike_bridge_core::halt::declared_project_state_dir() else { return };

    let record = confirmation_for(mount, handshake_account, vike_model::clock::now_ms());
    if let Err(e) = vike_model::account_confirmation::park(Some(&state), record) {
        // ⚠ warn, not error, and the session is NOT failed: a confirmation that could not be parked
        // costs the fold one mount's worth of evidence, which the next mount re-supplies. The
        // DISAGREEMENT above has already been logged at `error!` and does not depend on this write.
        tracing::warn!(
            venue,
            error = %e,
            state = %state.display(),
            "dukascopy: the venue's account confirmation could not be parked for \
             `vike-cli secrets confirm` — the mount is unaffected and the next one will re-record it"
        );
    }
}

#[path = "dukascopy_tests.rs"]
#[cfg(test)]
mod dukascopy_tests;
