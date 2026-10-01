//! dukascopy's VENUE MOUNT — the `vike_bridge_core::venue_mount::VenueMount` contract over the
//! JForex Java sidecar (docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md).
//!
//! Moved, behaviour unchanged, from `vike-mount`: its `("dukascopy", _)` arm (now
//! [`DukascopyVenueMount`]'s `mount`), its arming-probe row (`resolve`), its clock, book-identity
//! and grid-source rows (`declaration`), and the rest of its `dukascopy` module: the account
//! resolution over the root's store read (`resolve_in`), the confirmation park
//! (`record_confirmation`) and the two one-sidecar refusal texts, which the declaration hands
//! `vike-mount` as its `ProcessExclusive` renderers.
//!
//! ⚠ **NOTHING IN PRODUCTION MOUNTS THIS VENUE, and that is the owner's call, not an omission.**
//! `crates/vike-tradehub/src/wired_markets.rs`'s `WIRED_MARKETS` has no dukascopy row, so `build_node` never asks
//! this row to MOUNT: [`DukascopyVenueMount`]'s `mount` runs in tests only. Its declaration and its
//! `resolve` do run in production — the roster-wide arming projection
//! (`crates/vike-mount/src/arming.rs`'s `venue_arming`, which the daemon runs at every live start and
//! for its `venues` report) reads this row's declaration, and asks its `resolve` once
//! `venues.dukascopy` is above `paper`. The venue mount contract ported the venue as it was (the
//! owner's 2026-09-28 answer to its spec's open question 1); wiring a venue that has never run in
//! production is its own change.
//!
//! # Which account
//!
//! `resolve_in` is `crate::account`'s `resolve_account` over the settings database's `account`
//! table AS THE COMPOSITION ROOT READ IT (`MountInputs::accounts`) — this crate opens no store.
//! The row's credential-key OWNER PREFIX picks the broker; `crate::account`'s module doc is the
//! authority on which broker a row names and why `AccountLabel::Default` is always the Swiss bank.
//! ⚠ A refusal is a refusal, never a fallback: DEMO1 is Dukascopy Bank SA and DEMO2 is Dukascopy
//! Europe IBS AS — two LEGAL ENTITIES — so an account the store cannot identify stays PAPER and
//! says so by name.
//!
//! # ⚠ ONE sidecar per process — the evidence, and the measurement that retires the limit
//!
//! The RULE is `vike-mount`'s and is generic: `crates/vike-mount/src/exclusive.rs`'s `holder`
//! picks which account holds the process's one resource from the POLICY (a named account takes it
//! and the DEFAULT account yields), and its `claim` is the backstop, kept only for a running
//! sidecar. The claim must not live in this crate: `crates/bridges/dukascopy/tests/dukascopy_exec.rs`
//! spawns stub sidecars repeatedly within one test process. What stays HERE is why this venue
//! declares the limit at all, and it is a refusal rather than a report because the risk falls on
//! the account that was already working:
//!
//! * `crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/Bridge.java` sets no
//!   platform cache directory (it builds its client with `ClientFactory.getDefaultInstance()` and
//!   never calls the SDK's cache-directory setter), so two JVMs of one user share the per-user
//!   default cache;
//! * `crates/bridges/dukascopy/CLAUDE.md`'s login-failure triage records what a corrupted local
//!   platform cache costs — an INSTANT `login failed` on **every** attempt until the directory is
//!   deleted by hand. That is a failure of both accounts, not of the new one, so admitting a second
//!   sidecar can take the FIRST account off the venue. `docs/decisions/0013-degrade-vs-refuse.md`
//!   licenses degrading the capability being ADDED; it does not license degrading one that
//!   already worked.
//! * `crates/bridges/dukascopy/src/exec.rs`'s `READY_TIMEOUT` is 300s and the fan-out mounts
//!   accounts SERIALLY, so two would also make a cold mount cost up to ten minutes before any
//!   other venue is reached.
//! * `crates/bridges/dukascopy/tests/dukascopy_live_smoke.rs`'s own header says to run its two
//!   logins separately. That note is about one ACCOUNT's session and is therefore not evidence
//!   about two accounts — it is recorded here as what the tree knows, not as the argument.
//!
//! **What retires the limit is a measurement, not a review**: give each sidecar its own platform
//! cache directory (the JForex `IClient` has a setter for it and `Bridge.java` calls none), then
//! run two real logins concurrently on a box with the SDK staged and show both reach a `ready`
//! envelope. The refusal is loud and names this paragraph, so an operator who hits it is not left
//! guessing.
//!
//! # …and the OTHER direction: what the VENUE says about the account we chose
//!
//! `record_confirmation` is the answer coming back: the sidecar's `ready` envelope carries
//! `IAccount.getAccountId()`, the credential-schema spec's §4.5 handshake writer for
//! `account.venue_account_id` and `account.last_verified_at`. ⚠ **It writes no store, and that is
//! a MEASURED constraint**: the shipped unit runs under `ProtectSystem=strict` with
//! `ReadWritePaths=<project>/settings/state`, and the settings database is outside it — so a
//! mount-time `UPDATE` is `EROFS` on every deployment. The mount PARKS
//! (`vike_model::account_confirmation`, writable by construction) into the state directory
//! `vike-mount` hands it in `MountInputs::process` — the one the boot declared, never a fresh walk
//! — and `vike-cli secrets confirm` folds. A DISAGREEMENT is reported at `error!`, writes nothing,
//! and never fails the mount.

use std::path::Path;

use vike_bridge_core::account_directory::AccountDirectory;
use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs,
    MountOutcome, MountRequest, PaperCause, ProcessExclusive, Resolution, Tier, VenueDeclaration,
    VenueMount, recon_if_enabled,
};
use vike_exec::EventSender;
use vike_model::account_keys::AccountLabel;

use crate::account::{
    DEFAULT_ACCOUNT, DukascopyMount, DukascopyRefusal, confirmation_for, resolve_account,
};
use crate::config::{
    DukascopyConfig, DukascopyTools, load_dukascopy_config_from, resolve_dukascopy_tools,
};
use crate::exec::{DukascopyError, DukascopyExecutionClient};
use crate::recon_client::VENUE;

/// The refusal a would-arm account gets when ANOTHER account of this process holds the one JForex
/// sidecar — this venue's `ProcessExclusive::held_by_another`. Text unchanged but for its closing
/// citation, which follows the module doc here.
fn sidecar_held_by_another(label: &str, holder: &str) -> String {
    format!(
        "dukascopy account `{label}` stays PAPER: this process runs ONE JForex sidecar and \
         `{holder}` holds it. Two sidecars share one JForex platform cache (the Java bridge sets \
         no cache directory) and a corrupted cache makes EVERY login fail until the directory is \
         deleted, so exactly one dukascopy account is armed per process. WHICH one is the \
         policy's to choose: an `[accounts.dukascopy]` line above `paper` takes it, and with no \
         such line the DEFAULT account keeps it. To trade both at once, give the second account \
         its own project folder (its own VIKE_SETTINGS_DIR and credential store) and run a second \
         process there; `crates/vike-mount/src/exclusive.rs`'s `pick_holder` carries the rule and \
         `crates/bridges/dukascopy/src/mount.rs`'s module doc names the measurement that retires \
         it"
    )
}

/// The refusal the process-level BACKSTOP gives — a second dukascopy account asking for a sidecar
/// in a process that already runs one — this venue's `ProcessExclusive::already_claimed`. Text
/// unchanged but for its closing citation.
fn sidecar_already_claimed(label: &str) -> String {
    format!(
        "dukascopy account `{label}` stays PAPER: this process already runs a JForex sidecar and \
         a SECOND one is refused. Two sidecars share one JForex platform cache (the Java bridge \
         sets no cache directory), and a corrupted cache makes EVERY login fail until the \
         directory is deleted — so admitting the second account could take the FIRST one off the \
         venue. Run the second account in its own project folder (its own VIKE_SETTINGS_DIR and \
         credential store) until per-sidecar cache directories are proven; \
         `crates/bridges/dukascopy/src/mount.rs`'s module doc names the measurement that retires \
         this"
    )
}

/// **`resolve_account` over the snapshot the composition ROOT read** — the only door the mount and
/// the arming probe reach the `account` table through.
///
/// The four answers it can be handed, and what each means here:
///
/// * **UNREAD** — no root read the store (a test, a tool, a root that predates the field). The
///   DEFAULT account resolves to `DEFAULT_ACCOUNT` and every labelled one is REFUSED, which is
///   exactly a `Backend::Files` box's answer.
/// * **`Accounts::Unanswerable`** — a real store with no `account` table. Same shape, and the
///   refusal renders WHICH store and why.
/// * **`Accounts::Known`** — the table answered; `resolve_account` does the rest.
/// * **an ERROR on either half** — the store exists and would not open. The DEFAULT account still
///   resolves (it never needed the table) and says so in the log; a labelled account is refused as
///   `DukascopyRefusal::StoreUnreadable`, a STORE failure reported as itself.
///
/// ⚠ **The KEY-NAME half is not optional for a labelled account**, and that is why its error is
/// reported rather than folded away: the row's credential-key OWNER PREFIX is the entire mapping
/// from a row to a broker, and folding "the store would not open" into "this row's key names name
/// no broker" would send an operator to fix a row that was never at fault.
fn resolve_in(
    label: &AccountLabel,
    directory: &AccountDirectory,
) -> Result<DukascopyMount, DukascopyRefusal> {
    let default_mount = || DukascopyMount { account: DEFAULT_ACCOUNT, row: None, book: None };
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
    // account — which cannot be identified without it — while the DEFAULT account still
    // resolves: a broken database must not take a single-account box off a venue it has always
    // traded.
    let accounts = match rows {
        Ok(a) => a,
        Err(error) => {
            let Some(text) = label.text() else {
                tracing::error!(
                    venue = VENUE,
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
        // NOT read at all — impossible beside a `Some` row answer, but it costs one arm to keep
        // the two halves independent rather than assume they move together.
        None => None,
        Some(Err(error)) => {
            let Some(text) = label.text() else {
                tracing::error!(
                    venue = VENUE,
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

/// **THE HANDSHAKE HALF: record what the venue answered, and say the verdict out loud.** Called
/// once per successful sidecar login with the account identifier
/// `crate::exec::DukascopyExecutionClient::handshake_account` carries, and the state directory the
/// mount was handed.
///
/// It PARKS a `vike_model::account_confirmation::ConfirmationRecord` and writes no database (the
/// module doc's measured constraint). The verdict is LOGGED here as well as parked, because the
/// fold may be days away and a wrong-broker disagreement must not wait for it. ⚠ A disagreement
/// does NOT take the venue down — the session authenticated, and refusing the mount over a
/// bookkeeping disagreement would degrade a capability that was working — and it writes nothing:
/// not the book, not the timestamp. ⚠ It cannot tell a wrong broker from a FORM mismatch (the
/// credential-schema spec §9 leaves the identifier's form unsettled), and the message says so.
/// Nothing here can log a credential: the record holds a venue account id, a key PREFIX and a row
/// id.
fn record_confirmation(mount: &DukascopyMount, handshake_account: &str, state_dir: Option<&Path>) {
    use vike_model::account_confirmation::{Verdict, verdict};

    let venue = VENUE;
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

    // The park. A mount handed no state directory is a test, a tool, or a root that declared no
    // project — it has no writable path and no row to address either. ⚠ The HANDED directory,
    // never a fresh walk: it is the one the boot resolved (and therefore the one
    // `$VIKE_SETTINGS_DIR` moved), exactly the path the shipped unit's `ReadWritePaths` grants.
    let Some(state) = state_dir else { return };

    let record = confirmation_for(mount, handshake_account, vike_model::clock::now_ms());
    if let Err(e) = vike_model::account_confirmation::park(Some(state), record) {
        // ⚠ warn, not error, and the session is NOT failed: a confirmation that could not be
        // parked costs the fold one mount's worth of evidence, which the next mount re-supplies.
        tracing::warn!(
            venue,
            error = %e,
            state = %state.display(),
            "dukascopy: the venue's account confirmation could not be parked for \
             `vike-cli secrets confirm` — the mount is unaffected and the next one will re-record it"
        );
    }
}

/// The one resolution `resolve` and `mount` share: WHICH account (from the store the root read)
/// and that account's credential set, or the refusal that names why there is no account.
fn resolved(
    inputs: &MountInputs<'_>,
) -> Result<(DukascopyMount, Option<DukascopyConfig>), DukascopyRefusal> {
    let mount = resolve_in(inputs.account, inputs.accounts)?;
    // The login from the credential map; the JForex server from the demo tier's
    // `venue.dukascopy.demo.server` row (`MountInputs::settings`, decision 0095).
    let config = load_dukascopy_config_from(mount.account, inputs.secrets, inputs.settings);
    Ok((mount, config))
}

/// [`DukascopyVenueMount`]'s `mount`, with its one side-effecting step — STARTING the sidecar —
/// taken as a parameter: `mount` passes `crate::exec`'s real `spawn`, and `mount_tests.rs` passes a
/// double that records whether the start was reached and with which login and tools. The real start
/// cannot witness either on a test box: with no jar there, its own check turns a refused mount and
/// a wrongly started one into the same PAPER answer.
fn mount_with(
    req: MountRequest<'_>,
    spawn: impl FnOnce(
        DukascopyConfig,
        &DukascopyTools,
        EventSender,
    ) -> Result<DukascopyExecutionClient, DukascopyError>,
) -> MountOutcome {
    let (mount, cfg) = match resolved(&req.inputs) {
        Err(refusal) => {
            // `error!`, the same class as a half-written credential set: the operator wrote a
            // configuration that describes a real account and it is not being traded.
            //
            // ⚠ THIS ARM IS THE WHOLE REFUSAL, not a second copy of one: `vike-mount` asks a row
            // whose probe answered paper to mount WITHOUT taking the process's one-sidecar claim,
            // so an account that got past here would reach a broker nobody chose AND sit outside
            // the one-sidecar backstop.
            tracing::error!(venue = VENUE, "{refusal}");
            return MountOutcome::paper();
        }
        Ok((_, None)) => return MountOutcome::paper(),
        Ok((mount, Some(cfg))) => (mount, cfg),
    };
    // `vike-mount` claimed the process's one sidecar before calling this (the declaration's
    // `ProcessExclusive`) and keeps the claim only if this returns `Live`: every other exit
    // releases it, so a failed start never refuses the next account with a reason that is not
    // true.
    //
    // ⚠ The STATE directory is handed in its own right, never re-derived from the bin one:
    // `$VIKE_SETTINGS_DIR` moves `settings/` without moving `bin/`. It resolves the JVM's
    // `user.home` — see `crate::exec`'s module doc for the `ProtectHome=yes` defect that makes
    // it load-bearing.
    let process = req.inputs.process;
    let tools = resolve_dukascopy_tools(
        req.inputs.secrets,
        process.bin_dir.as_deref(),
        process.state_dir.as_deref(),
    );
    match spawn(cfg, &tools, req.events.clone()) {
        Ok(client) => {
            // The kill switch: the file `vike-mount` resolved for the process (decision 0099).
            let client = client.with_halt_path(process.halt_path.clone());
            // ⚠ The line NAMES THE BROKER, the row and the book: the only moment an operator can
            // catch a wrong mapping before an order does. None of the three is a secret, and
            // the login and password appear nowhere.
            tracing::warn!(
                venue = VENUE,
                account = %req.inputs.account,
                broker = mount.account.broker(),
                keys = mount.account.key_prefix(),
                row = ?mount.row,
                book = ?mount.book,
                "dukascopy: DEMO credentials present and the JForex sidecar started → LIVE \
                 exec client (real demo-account orders)"
            );
            // THE HANDSHAKE HALF — only on this path, where a sidecar really authenticated, so
            // a parked record can never describe a login that did not happen.
            record_confirmation(&mount, client.handshake_account(), process.state_dir.as_deref());
            // Derived from the client, so built BEFORE the client moves into the box — the
            // sidecar owns the one connection both share. Positions only: `crate::recon_client`'s
            // module doc says what it defers and why.
            let rc = client.recon_client();
            MountOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client: Box::new(client),
                    bound_tier: Tier::Demo,
                    grid: None,
                    contract_size: None,
                    margin_mode: None,
                    leg_grids: Vec::new(),
                }),
                recon: recon_if_enabled(req.recon_enabled, || Some(Box::new(rc))),
                identity: None,
            }
        }
        Err(e) => {
            // Includes the absent-jar case, which `spawn` has already logged with the path it
            // looked for. Staying paper is the live gate doing its job.
            tracing::error!(
                venue = VENUE,
                account = %req.inputs.account,
                error = ?e,
                "dukascopy: the JForex sidecar would not start — staying PAPER"
            );
            MountOutcome::paper()
        }
    }
}

/// dukascopy's mount. `vike_tradehub::registry::REGISTRY` holds `&DukascopyVenueMount`; nothing in
/// production asks it to mount (see the module doc).
pub struct DukascopyVenueMount;

impl VenueMount for DukascopyVenueMount {
    fn venue(&self) -> &'static str {
        VENUE
    }

    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            // The account is addressed through the settings database's `account` ROW rather than
            // a key name — the one venue whose labelled accounts are not `…__LABEL` keys. A
            // labelled account can resolve to the same key family as the DEFAULT one (address the
            // book of the row owning `DUKASCOPY_DEMO1_*` and that IS the default account), so
            // "different account ⇒ different keys" does not hold here; what holds instead is the
            // one-sidecar rule below, which makes two engines over one credential set unreachable.
            addresses_accounts: true,
            process_exclusive: Some(ProcessExclusive {
                resource: "JForex sidecar",
                held_by_another: sidecar_held_by_another,
                already_claimed: sidecar_already_claimed,
            }),
            // Interval-only reconcile: the recon client is derived from the running sidecar.
            takes_recon_trigger: false,
            // No grid pre-fetch at all: the mounted symbol keeps `RiskLimits::new()`'s permissive
            // `None`s, so a declared leg inherits nothing.
            grid_source: DeclaredGridSource::NoGrid,
            // `effective_book` reads the credential STORE and nothing else, and this venue's store
            // holds a LOGIN rather than an account number. The venue tells us at connect instead
            // (`crate::proto`'s `Envelope::Ready`), which `record_confirmation` parks.
            book_identity: BookIdentity::Undeterminable {
                why: "the store holds a JForex LOGIN, not an account number, so nothing in it names \
                      the book; the venue answers at the sidecar's ready handshake instead, and \
                      `account.venue_account_id` is where that answer is kept",
            },
            // ⚠ The one deliberate text change of this port (the spec's "Findings" 3): the row
            // said `make_engine` had no arm for this venue, false since 2026-09-09.
            clock: ClockDecl::NotWired {
                reason: "execution is a JForex Java sidecar over stdio with no REST API, and the \
                         sidecar starts only at mount — after this step — so there is no server \
                         clock this preflight can read",
                unmeasured_risk: None,
            },
        }
    }

    /// The arming-probe row as it was: an account the store cannot identify is refused BY NAME
    /// (`AccountNotInStore` — the keys may well be present; what is missing is the row that says
    /// which broker they are); a resolved account with its LOGIN and PASSWORD arms a DEMO session
    /// (the sidecar authenticates against a demo server and no live tier is wired).
    ///
    /// ⚠ It can LOG — `resolve_in`'s store-failure line for the default account — exactly as the
    /// arming row it replaces did. It performs no I/O and starts nothing.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match resolved(inputs) {
            Err(_) => Resolution::Paper(PaperCause::AccountNotInStore),
            Ok((_, None)) => Resolution::Paper(PaperCause::NoCredentials),
            Ok((_, Some(_))) => Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
        }
    }

    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        mount_with(req, DukascopyExecutionClient::spawn)
    }
}

#[path = "mount_tests.rs"]
#[cfg(test)]
mod mount_tests;
