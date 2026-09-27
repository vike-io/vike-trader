//! **Which Dukascopy ACCOUNT a mount is for** — the resolution `crate::make_engine_for_account`'s
//! `("dukascopy", _)` arm used to answer with a hardcoded `DukascopyAccount::Demo1`.
//!
//! # The defect this module removes
//!
//! Dukascopy's two demo accounts are two LEGAL ENTITIES — Dukascopy Bank SA (Swiss/global) and
//! Dukascopy Europe IBS AS (EU) — with two credential sets, `DUKASCOPY_DEMO1_*` and
//! `DUKASCOPY_DEMO2_*`. The arm read the first pair whatever account it was mounting, and
//! `crate::arming::arm_addresses_accounts` refused the venue outright so that nothing could ask it
//! to do otherwise. `docs/superpowers/specs/2026-09-14-the-credential-schema.md` ruling 13 refused
//! every KEY-GRAMMAR fix for that and said to wait for the database: *"once the account is a
//! COLUMN, a name carries no account, no tier and no index."* The column landed (schema 2), its
//! reader landed (`vike_secrets::read_accounts`), and the key-name reader beside it
//! (`vike_secrets::read_account_keys`) landed after it. This is what they were for.
//!
//! # What keys the mapping, and what deliberately does not
//!
//! **The row's credential-key OWNER PREFIX** — `DUKASCOPY_DEMO1_` or `DUKASCOPY_DEMO2_` — is what
//! decides the broker, through `vike_dukascopy::DukascopyAccount::from_key_prefix`. Three reasons,
//! strongest first:
//!
//! 1. **It is the store's own account identity.** `crates/vike-secrets/src/schema.rs`'s
//!    `Classification::owner_prefix` says so in its own words — *an account's owner prefix is
//!    recoverable from any one of its own rows* — and that is exactly why the migration needs no
//!    stored discriminator for the one pair that shares `(venue, tier, label)`. Keying on it means
//!    the mount and the migration agree about which row is which by construction.
//! 2. **It is the fact the LOADER needs.** `DukascopyAccount::key_prefix` IS the prefix of the three
//!    key names `vike_dukascopy::load_dukascopy_config_from` reads, so the mapping is an inverse
//!    rather than a table of magic values that could drift from either side.
//! 3. **It is deployment-independent.** Every migrated box derives the same two prefixes from the
//!    same two key families; nothing about it is a property of one operator's accounts.
//!
//! **`venue_account_id` — the BOOK — is the ADDRESS rather than the mapping**, and the difference
//! matters. It is what an operator WRITES to say which account they mean
//! (`policy.accounts.dukascopy.<BOOK>`), and it is the one identity the venue itself supplies; but
//! it is `NULL` on every row a migration writes (`vike_secrets::Account::venue_account_id`'s own
//! doc: the ten-key fold of §11 step 3 is not performed), so a box that has not run
//! `vike-cli secrets set-book` has no book on either row and could map nothing at all if the book
//! were the mapping. It is also per-deployment DATA: the two numbers on the owner's boxes are his
//! accounts, and a constant table of them compiled into a shipped bridge would be the identity-in-
//! code defect this whole schema exists to stop carrying.
//!
//! **`label` is not used for either job, and cannot be.** Both dukascopy rows carry `NULL`: the
//! owner refused the migration's provisional `DEMO1`/`DEMO2` spellings at the spec's signature
//! (*labels are informative and optional, `id` is the identity*), and refused the same spelling once
//! before that as ruling 13. A row that DOES carry a label an operator wrote is matched too — see
//! [`resolve_account`] — but nothing here invents one and nothing depends on one existing.
//!
//! **`Account::id` is never an address.** It is a SQLite rowid — ⚠ *"with no `AUTOINCREMENT`"*
//! until stage 4 of the settings-store plane, which changes nothing here because the high-water
//! mark lives in the file — stable only
//! for the life of one database file, and this tree documents a recovery that deletes the database
//! and migrates again — after which the same accounts come back numbered differently. So an id is
//! logged (it is what `vike-cli secrets set-book` addresses) and never written into a settings file.
//!
//! # What `AccountLabel::Default` means here
//!
//! **`DukascopyAccount::Demo1`, always, on every box, whatever the database holds** — see
//! [`resolve_account`]'s own doc for the argument. That is the byte-identity guarantee for every
//! existing deployment, and it is also the only answer the store could honestly give: after the
//! migration a box has TWO unlabelled `(dukascopy, demo)` rows and neither is marked "the default
//! one", so asking the table which row the default account is has no answer that is not invented.
//!
//! # How an operator actually addresses the second account
//!
//! Three acts, none of them a new mechanism and none of them a label on a row:
//!
//! ```text
//! vike-cli secrets accounts                       # which rows exist, and each one's key names
//! vike-cli secrets set-book --id 8 --venue-account-id 3716974   # the number the venue gave it
//! ```
//!
//! ```toml
//! # <project>/settings/policy.toml
//! [venues]
//! dukascopy = "demo"        # the venue's own ceiling — without it NOTHING dukascopy arms
//!
//! [accounts.dukascopy]
//! 3716974 = "demo"          # the BOOK is the address; `AccountLabel::parse` accepts A-Z0-9
//! ```
//!
//! ⚠ The mount then builds a `dukascopy#3716974` engine against `DUKASCOPY_DEMO2_*` — the key
//! family row 8 owns — **and the DEFAULT account DECLINES**: it resolves paper, says so by name, and
//! its `DUKASCOPY_DEMO1_*` keys are not read. That is [`sidecar_holder`]'s rule, and the whole of
//! why this venue's second account is reachable at all — see that function for the argument. A box
//! that names NO dukascopy account under `[accounts]` is untouched: the default account mounts
//! `DUKASCOPY_DEMO1_*` exactly as it always has.
//!
//! And `crates/vike-run/src/node.rs`'s `refuse_unarmed_mount_accounts` still applies: a strategy
//! that NAMES an account the mount did not arm is refused rather than silently run on the default
//! one. That refusal reads the same projection this module feeds, which is why the projection has to
//! know about the sidecar holder too — before it did, it answered `Demo` for BOTH accounts of a
//! two-account box while the mount built a PAPER engine for one of them, and a strategy that
//! believed it was armed ran on paper.
//!
//! ⚠ **One residual this change does not close, and it predates it**: `vike_run::WIRED_MARKETS`
//! carries no dukascopy row, so the venue is reached only by a strategy mount that names it. The
//! arm addresses whatever account it is asked for; what asks is a separate question.
//!
//! # ⚠ ONE sidecar per process — WHOSE it is, and the measurement that retires the limit
//!
//! [`sidecar_holder`] decides which dukascopy account gets it, from the POLICY, before anything is
//! mounted; [`claim_sidecar`] is the process-level backstop under that decision. ⚠ Until
//! 2026-09-15 there was no decision — the claim alone decided, first-come, and the fan-out always
//! mounts the DEFAULT account first, so the default always won and **no policy could mount the
//! second account at all**. The feature was unreachable, which is the defect [`sidecar_holder`]
//! exists to remove.
//!
//! ⚠ **Those two paragraphs can be read as contradicting each other, so read them together.** The
//! refusal below protects the account that already worked from a hazard NOBODY ASKED FOR — a second
//! sidecar started beside it, corrupting the cache both depend on. [`sidecar_holder`] takes that
//! same account off the venue only when the operator NAMED another one in `policy.toml`, which is a
//! request rather than an accident, and it never starts a second sidecar to do it. One is a silent
//! degrade of a working capability; the other is an operator's own choice, said out loud at the
//! account that yields.
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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_bridge_core::credentials::{AccountKeys, Accounts, NoAccountTable};
use vike_dukascopy::DukascopyAccount;
use vike_model::account_keys::AccountLabel;

use crate::AccountDirectory;

/// The venue id this module is about, spelled once.
pub(crate) const VENUE: &str = "dukascopy";

/// **What `AccountLabel::Default` means on this venue** — Dukascopy Bank SA, the Swiss entity,
/// whatever any store says.
///
/// ⚠ Spelled ONCE, as a constant, because two sites answer with it ([`resolve_account`]'s default
/// branch and [`resolve_in`]'s unread-store branch) and a box whose two answers disagreed would
/// mount a different BROKER depending on whether its root had read the store.
/// The argument for the value is at [`resolve_account`].
const DEFAULT_ACCOUNT: DukascopyAccount = DukascopyAccount::Demo1;

/// **The account a dukascopy mount resolved to**, with the evidence that chose it.
///
/// `row` and `book` are for the LOG and for an operator's eyes. Neither is a secret — a
/// `venue_account_id` is the number the venue prints on its own page, and a row id is an opaque
/// integer — while the login and password this never carries are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DukascopyMount {
    /// Which credential set, and therefore which LEGAL ENTITY.
    pub account: DukascopyAccount,
    /// The `account.id` this was read off, when a store could answer. `None` for the default
    /// account on a store that cannot be asked — and for the default account generally, which does
    /// not need a row to resolve.
    pub row: Option<i64>,
    /// `account.venue_account_id`, when the row carries one. `None` is *not yet known*, never *this
    /// account has no book*.
    pub book: Option<String>,
}

/// **Why a dukascopy account could not be mapped to a broker.** Every arm is a REFUSAL: the mount
/// builds no exec client and stays paper.
///
/// ⚠ There is deliberately no arm that falls back to [`DukascopyAccount::Demo1`]. Demo1 is the Swiss
/// bank; coercing an account nobody could identify onto it would place orders with a counterparty
/// the operator did not choose, under a different regulator, and would do it silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DukascopyRefusal {
    /// The store cannot be asked which accounts exist — a `Backend::Files` box, or a settings
    /// database older than the `account` table. The DEFAULT account is unaffected (it resolves
    /// without the store); this is a labelled account on a box with nowhere to look it up.
    NoAccountTable {
        /// The store that IS answering, and why it carries no table — rendered verbatim.
        why: String,
        /// The label that could not be resolved.
        label: String,
    },
    /// The store answered and no ACTIVE dukascopy row matches the label.
    NoSuchAccount {
        label: String,
        /// Every active dukascopy row, rendered for an operator: id, book, key prefixes.
        known: Vec<String>,
    },
    /// More than one active row matches. Refused rather than picked — see the type's own doc.
    Ambiguous { label: String, matched: Vec<String> },
    /// A row was found and its credential key names name no dukascopy credential family, or name
    /// both. The row exists; nothing says which broker it is.
    UnmappableRow { label: String, row: i64, prefixes: Vec<String> },
    /// **The store EXISTS and would not open** — a store FAILURE, reported as itself.
    ///
    /// ⚠ It is a variant of its own because the two halves of the read fail INDEPENDENTLY and the
    /// second one used to be swallowed: the account rows came back fine, the key-NAME read failed,
    /// `.ok().flatten()` turned that into "no key names", and a labelled account was then refused as
    /// [`Self::UnmappableRow`] — *this row's key names do not say which broker it is* — about a row
    /// whose key names nobody could read. An operator went looking for a row that was correct.
    /// `what` names WHICH read failed so the two are never confused again.
    StoreUnreadable { label: String, what: &'static str, error: String },
    /// **Another dukascopy account holds this process's one sidecar** — the POLICY-derived decline
    /// ([`sidecar_holder`]), not a race. It is what the DEFAULT account gets on a box that arms a
    /// labelled dukascopy account, and what the extra accounts get on a box that arms several.
    ///
    /// ⚠ Raised only for an account whose CREDENTIALS resolved — `crate::make_engine_for_account`
    /// checks the holder below the credential read. An account that could not have armed anyway is
    /// told nothing, because it lost nothing.
    SidecarHeldByAnother { label: String, holder: String },
    /// A second dukascopy account asked for a sidecar in a process that already has one — the
    /// process-level BACKSTOP under [`sidecar_holder`], which should already have refused it. See
    /// the module doc's ONE-sidecar section.
    SidecarAlreadyClaimed { label: String },
}

impl std::fmt::Display for DukascopyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DukascopyRefusal::NoAccountTable { why, label } => write!(
                f,
                "dukascopy account `{label}` cannot be mapped to a broker: {why}. Dukascopy's two \
                 demo accounts are two LEGAL ENTITIES (Dukascopy Bank SA and Dukascopy Europe IBS \
                 AS) and the settings database's `account` table is the only thing that says which \
                 row is which, so this account stays PAPER rather than being routed to a broker \
                 nobody chose. `vike-cli secrets migrate` moves a file store; the DEFAULT account \
                 is unaffected and still mounts DUKASCOPY_DEMO1_*"
            ),
            DukascopyRefusal::NoSuchAccount { label, known } => write!(
                f,
                "dukascopy account `{label}` names no active row in the settings database, so it \
                 stays PAPER — it is NOT routed to the default account's broker. The dukascopy \
                 rows that exist are: {}. Address one of them by its `venue_account_id` \
                 (`vike-cli secrets accounts` prints it, `vike-cli secrets set-book` writes it) or \
                 by a label written on the row",
                render_list(known)
            ),
            DukascopyRefusal::Ambiguous { label, matched } => write!(
                f,
                "dukascopy account `{label}` matches more than one active row ({}), so it stays \
                 PAPER: two brokers answer to one address and picking either would be a guess. \
                 `vike-cli secrets accounts` prints the rows",
                render_list(matched)
            ),
            DukascopyRefusal::UnmappableRow { label, row, prefixes } => write!(
                f,
                "dukascopy account `{label}` resolves to account row {row}, whose credential key \
                 names do not say which broker it is ({}) — a dukascopy row must own exactly one \
                 of the DUKASCOPY_DEMO1_ / DUKASCOPY_DEMO2_ key families. It stays PAPER rather \
                 than being routed to either",
                render_list(prefixes)
            ),
            DukascopyRefusal::StoreUnreadable { label, what, error } => write!(
                f,
                "dukascopy account `{label}` stays PAPER: the settings store EXISTS and would not \
                 open, so {what} could not be read ({error}). This is a STORE FAILURE, not a \
                 statement about the account — the row may be perfectly good, and nothing here \
                 says otherwise. Fix the store (`vike-cli secrets path` prints where it is, \
                 `vike-cli secrets accounts` reads the same table); the DEFAULT account is \
                 unaffected and still mounts DUKASCOPY_DEMO1_*"
            ),
            DukascopyRefusal::SidecarHeldByAnother { label, holder } => write!(
                f,
                "dukascopy account `{label}` stays PAPER: this process runs ONE JForex sidecar and \
                 `{holder}` holds it. Two sidecars share one JForex platform cache (the Java \
                 bridge sets no cache directory) and a corrupted cache makes EVERY login fail \
                 until the directory is deleted, so exactly one dukascopy account is armed per \
                 process. WHICH one is the policy's to choose: an `[accounts.dukascopy]` line \
                 above `paper` takes it, and with no such line the DEFAULT account keeps it. To \
                 trade both at once, give the second account its own project folder (its own \
                 VIKE_SETTINGS_DIR and credential store) and run a second process there; \
                 `crates/vike-mount/src/dukascopy.rs`'s `sidecar_holder` carries the rule and the \
                 module doc names the measurement that retires it"
            ),
            DukascopyRefusal::SidecarAlreadyClaimed { label } => write!(
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

/// `a`, `b` and `c` — or `(none)`, which has to read as an answer rather than as an empty string.
fn render_list(items: &[String]) -> String {
    if items.is_empty() { "(none)".to_string() } else { items.join(", ") }
}

/// One active row, rendered for an operator. Never a credential: `id` is an opaque integer, the
/// book is the number the venue prints, and the prefixes are key NAMES.
fn render_row(row: &vike_bridge_core::credentials::Account, keys: Option<&AccountKeys>) -> String {
    let book = row.venue_account_id.as_deref().unwrap_or("book not yet known");
    let prefixes = keys.map(|k| k.prefixes.clone()).unwrap_or_default();
    let label = row.label.as_deref().unwrap_or("no label");
    format!("id {} [{book}] keys {} ({label})", row.id, render_list(&prefixes))
}

/// **THE resolution — pure, over the store's own answer.**
///
/// # `AccountLabel::Default` is `Demo1`, unconditionally, and that is a decision
///
/// A single-account box has mounted `DUKASCOPY_DEMO1_*` — Dukascopy Bank SA, the Swiss entity —
/// since the arm was written, and `crate::make_engine_with_legs` reaches this function at
/// `AccountLabel::Default` for every one of the ~19 call sites that mount the one account they have
/// ever had. Three things follow, and together they are the argument:
///
/// * **Re-pointing it would move a BROKER with no operator act.** A box upgrades, its store is
///   migrated by a command about credentials, and the venue an order reaches changes. That is the
///   failure class §1 of the credential-schema spec is about, pointing the other way.
/// * **The store has no better answer to give.** After the migration there are TWO
///   `(dukascopy, demo, NULL)` rows and nothing marks either "the default". `id` order is the order
///   the migration happened to meet key names in, which is not a fact about the operator's
///   intention; picking the lowest id would be inventing one.
/// * **`Default` is not "no account" — it is the account an UNLABELLED key addresses**
///   (`AccountLabel`'s own doc). `DUKASCOPY_DEMO1_*` is that key family only by history, and
///   `DUKASCOPY_DEMO2_*` carries an index token that says it is the second one; but history is
///   exactly what byte-identity is about, so this is the tie-break rather than a derivation.
///
/// The store is still consulted for the default account — but only to ATTACH the row and book to
/// the answer, never to choose it, so the log can say which row the default account is. A store
/// that cannot be asked yields `row: None` and changes nothing.
///
/// # A LABELLED account is looked up, and refused when it cannot be
///
/// `policy.accounts.dukascopy.<LABEL>` (and a `…__<LABEL>` key in the store) names an account. The
/// label is matched against the row's `venue_account_id` FIRST — the venue's own identity for the
/// book, which `vike-cli secrets set-book` writes and which an operator reads off Dukascopy's own
/// page — and then against `account.label`, so a row somebody names later works without this
/// function changing. Inactive rows are not candidates. Every failure is a [`DukascopyRefusal`].
pub(crate) fn resolve_account(
    label: &AccountLabel,
    accounts: &Accounts,
    keys: Option<&BTreeMap<i64, AccountKeys>>,
) -> Result<DukascopyMount, DukascopyRefusal> {
    let active: Vec<&vike_bridge_core::credentials::Account> =
        accounts.active_for_venue(VENUE).unwrap_or_default();

    let Some(text) = label.text() else {
        // THE DEFAULT ACCOUNT. Demo1 by decision (see this function's doc); the row is attached
        // only when the store can name exactly one row owning that key family, so the log can say
        // which row it is without the answer ever depending on the store.
        let account = DEFAULT_ACCOUNT;
        let row = active
            .iter()
            .find(|r| row_account(r.id, keys) == Some(account))
            .map(|r| (r.id, r.venue_account_id.clone()));
        return Ok(DukascopyMount {
            account,
            row: row.as_ref().map(|(id, _)| *id),
            book: row.and_then(|(_, book)| book),
        });
    };

    // A LABELLED account needs the table. `Accounts::Unanswerable` is not "no accounts" — it is
    // "this store cannot be asked" — and the two may not be merged (`vike_secrets::Accounts`).
    if accounts.known().is_none() {
        let why = match accounts.unanswerable() {
            Some(NoAccountTable::FileStore { file }) => format!(
                "the credential store {} is a FILE and carries no `account` table",
                file.display()
            ),
            Some(NoAccountTable::OlderSchema { path, found }) => format!(
                "the settings database {} is at schema {found}, which predates the `account` table",
                path.display()
            ),
            None => "the store could not be asked".to_string(),
        };
        return Err(DukascopyRefusal::NoAccountTable { why, label: text.to_string() });
    }

    let matched: Vec<&vike_bridge_core::credentials::Account> = active
        .iter()
        .copied()
        .filter(|r| r.venue_account_id.as_deref() == Some(text) || r.label.as_deref() == Some(text))
        .collect();
    match matched.as_slice() {
        [] => Err(DukascopyRefusal::NoSuchAccount {
            label: text.to_string(),
            known: active.iter().map(|r| render_row(r, keys.and_then(|k| k.get(&r.id)))).collect(),
        }),
        [row] => match row_account(row.id, keys) {
            Some(account) => Ok(DukascopyMount {
                account,
                row: Some(row.id),
                book: row.venue_account_id.clone(),
            }),
            None => Err(DukascopyRefusal::UnmappableRow {
                label: text.to_string(),
                row: row.id,
                prefixes: keys
                    .and_then(|k| k.get(&row.id))
                    .map(|k| k.prefixes.clone())
                    .unwrap_or_default(),
            }),
        },
        many => Err(DukascopyRefusal::Ambiguous {
            label: text.to_string(),
            matched: many.iter().map(|r| render_row(r, keys.and_then(|k| k.get(&r.id)))).collect(),
        }),
    }
}

/// **Which dukascopy credential family one `account` row owns** — `None` when its key names name
/// neither, or BOTH.
///
/// "Both" is a refusal rather than a preference: a row owning `DUKASCOPY_DEMO1_` and
/// `DUKASCOPY_DEMO2_` names two brokers, and there is no reading of it that picks one honestly. A
/// row with no key names at all is the same answer for the same reason — an account row can outlive
/// the last key that created it (`vike_secrets::AccountKeys::prefixes`), and an account with no
/// credentials is not an account that is Demo1.
fn row_account(id: i64, keys: Option<&BTreeMap<i64, AccountKeys>>) -> Option<DukascopyAccount> {
    let prefixes = &keys?.get(&id)?.prefixes;
    let mut found: Option<DukascopyAccount> = None;
    for prefix in prefixes {
        if let Some(account) = DukascopyAccount::from_key_prefix(prefix) {
            match found {
                None => found = Some(account),
                Some(already) if already == account => {}
                Some(_) => return None,
            }
        }
    }
    found
}

/// **WHICH dukascopy account this process's ONE sidecar belongs to** — decided from the POLICY,
/// before anything is mounted, so the arming projection and the mount answer the same way.
///
/// `armed` is every LABELLED dukascopy account whose arming row is above paper on its own merits —
/// the operator armed it, the store identifies it, and its credentials are present — in label order.
/// The rule:
///
/// * **none** → [`AccountLabel::Default`]. That is every deployment that exists today, and it is
///   byte-identical to the first-come claim that preceded this function: the default account mounts
///   `DUKASCOPY_DEMO1_*`.
/// * **exactly one** → that account, and **the DEFAULT account declines**.
/// * **more than one** → the FIRST in label order, and the rest decline.
///
/// # ⚠ Why the DEFAULT account declines, which is the whole point
///
/// The fan-out mounts the default account FIRST ([`crate::accounts_to_mount`] preserves row order
/// and [`crate::arming::known_accounts`] puts `Default` first), and `account_ceiling` is
/// `min(venue line, account line)` — so raising `venues.dukascopy` to `demo`, which is the only way
/// to get an `[accounts.dukascopy]` line above paper, NECESSARILY arms the default account too.
/// With a first-come claim the default therefore took the sidecar on every box, every time, and
/// **there was no policy that could mount the second account**. The feature shipped unreachable.
///
/// So naming an account under `[accounts.dukascopy]` is read as what it plainly is — *this is the
/// dukascopy account I want this process to trade* — and the account that was never named yields to
/// the one that was. The alternative (default wins, labelled accounts refused) is what shipped and is
/// the defect; the other alternative (refuse BOTH and strand the venue) punishes a configuration the
/// operator states clearly.
///
/// ⚠ **The cost, stated rather than discovered**: on a box that arms a labelled dukascopy account, a
/// strategy mounted on the venue's DEFAULT account now runs on a PAPER engine where it used to trade.
/// It is loud — the arm logs an `error!` naming the holder, and the arming row carries
/// `vike_config::ArmingBlock::SidecarHeldElsewhere` with its own sentence (`VenueArming::why`, the
/// Venues tab's hover text) — but `vike_run::refuse_unarmed_mount_accounts` does NOT refuse it,
/// because that refusal deliberately skips mounts that name no account. An operator who wants the
/// default account back deletes the `[accounts.dukascopy]` line.
///
/// # More than one is a PICK, and picking is legitimate here
///
/// Everywhere else in this module a guess would choose a LEGAL ENTITY for somebody
/// ([`DukascopyRefusal`]'s own doc), and every such arm refuses. This one does not choose an entity:
/// each armed account still resolves to its own row's own key family, and what is being rationed is
/// a PROCESS RESOURCE. Refusing every account instead would take a working box off the venue over a
/// line that names a real account. And the account that loses is not silently mistraded: it resolves
/// PAPER, the projection says so, and a strategy that names it is refused outright by
/// `vike_run::refuse_unarmed_mount_accounts`.
pub(crate) fn sidecar_holder(armed: &[AccountLabel]) -> AccountLabel {
    armed.first().cloned().unwrap_or(AccountLabel::Default)
}

/// The process's ONE sidecar flag. Private: [`claim_sidecar`] is the only way to take it, and
/// [`claim_in`] is the same mechanism over a caller-supplied flag so a test can exercise the
/// release without burning the process's own.
static SIDECAR: AtomicBool = AtomicBool::new(false);

/// **The ONE-sidecar claim, held** — taken immediately before a spawn and RELEASED on drop unless
/// [`SidecarClaim::keep`] commits it.
///
/// ⚠ **The release is the point, and its absence was a live defect.** The claim was a bare
/// `AtomicBool` that nothing ever cleared, so a FAILED spawn — an absent jar, a `Command::spawn`
/// error, a `Fatal` envelope from a bad login, or the 300s `READY_TIMEOUT` elapsing — burned it, and
/// every later dukascopy account in the process was then refused with *this process already runs a
/// JForex sidecar* when it runs none. `crates/bridges/dukascopy/CLAUDE.md` records a JNLP 404 firing
/// on roughly every second mount on a box without the retry jar, so that was the common path.
///
/// RAII rather than a `release()` call on the error arm: a `return` added between the claim and the
/// spawn would silently reintroduce the leak, and `Drop` cannot be forgotten.
#[derive(Debug)]
pub(crate) struct SidecarClaim<'a> {
    flag: &'a AtomicBool,
    keep: bool,
}

impl SidecarClaim<'_> {
    /// **Commit the claim for the life of the process** — called once the sidecar is actually
    /// running. Consumes the guard, so the only other thing that can happen to it is a release.
    pub(crate) fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for SidecarClaim<'_> {
    fn drop(&mut self) {
        if !self.keep {
            self.flag.store(false, Ordering::SeqCst);
        }
    }
}

/// **The process-level backstop under [`sidecar_holder`]**: `Some` for the first caller, `None`
/// while a committed claim is outstanding.
///
/// It should never fire on a correctly-resolved mount — [`sidecar_holder`] names exactly one account
/// per process and the arm refuses every other one before reaching here. What it still covers is a
/// process that mounts the venue TWICE (two cores in one process), which no policy describes and no
/// projection can predict.
#[must_use]
pub(crate) fn claim_sidecar() -> Option<SidecarClaim<'static>> {
    claim_in(&SIDECAR)
}

/// [`claim_sidecar`] over a caller-supplied flag — the seam a test uses, so proving that a failed
/// spawn RELEASES the claim does not depend on the order the process's own tests happen to run in.
#[must_use]
pub(crate) fn claim_in(flag: &AtomicBool) -> Option<SidecarClaim<'_>> {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .ok()
        .map(|_| SidecarClaim { flag, keep: false })
}

/// **[`resolve_account`] over the snapshot the composition ROOT read** — the only door the mount and
/// the arming projection reach the `account` table through.
///
/// ⚠ **This used to open the store itself**, at a settings directory taken from a process global
/// (`vike_bridge_core::halt::declared_project_state_dir`). That is the class
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` exists to ratchet down — a
/// library reading global configuration state its caller can neither see nor substitute — and the
/// gate could not see it, because neither account reader was one of `CREDENTIAL_STORE_READERS`'
/// names. Both halves are fixed: the readers are keyed now, and the read moved to the root
/// ([`crate::AccountDirectory`]), which is where the credential MAP has always been loaded.
///
/// The four answers it can be handed, and what each means here:
///
/// * **UNREAD** — no root read the store (a test, a tool, a root that predates the field). The
///   DEFAULT account resolves to [`DEFAULT_ACCOUNT`] and every labelled one is REFUSED, which is
///   exactly a `Backend::Files` box's answer and byte-identical to this arm before any of this
///   existed.
/// * **`Accounts::Unanswerable`** — a real store with no `account` table. Same shape, and the
///   refusal renders WHICH store and why.
/// * **`Accounts::Known`** — the table answered; [`resolve_account`] does the rest.
/// * **an ERROR on either half** — the store exists and would not open. The DEFAULT account still
///   resolves (it never needed the table) and says so in the log; a labelled account is refused as
///   [`DukascopyRefusal::StoreUnreadable`], a STORE failure reported as itself.
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
    // account — which cannot be identified without it — while the DEFAULT account still resolves.
    // That asymmetry is the byte-identity guarantee doing its job: a broken database must not take a
    // single-account box off a venue it has always traded, and the default account never needed the
    // table in the first place.
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
        // NOT read at all — impossible beside a `Some` row answer, since a directory is built from
        // both reads at once, but it costs one arm to keep the two halves independent rather than
        // assume they move together.
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

    let verdict = verdict(handshake_account, mount.book.as_deref());
    match &verdict {
        Verdict::Confirms => tracing::info!(
            venue = VENUE,
            broker = mount.account.broker(),
            row = ?mount.row,
            book = %handshake_account,
            "dukascopy: the venue CONFIRMED the book this account row names"
        ),
        Verdict::Learns => tracing::info!(
            venue = VENUE,
            broker = mount.account.broker(),
            row = ?mount.row,
            book = %handshake_account,
            "dukascopy: the venue NAMED this account's book, which the store did not know"
        ),
        Verdict::Disagrees { stored } => tracing::error!(
            venue = VENUE,
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
            venue = VENUE,
            error = %e,
            state = %state.display(),
            "dukascopy: the venue's account confirmation could not be parked for \
             `vike-cli secrets confirm` — the mount is unaffected and the next one will re-record it"
        );
    }
}

/// **The parked record this mount would write** — pure, so the ADDRESSING can be tested without a
/// declared project, a state directory or a clock.
///
/// ⚠ **The address is the KEY PREFIX, never `mount.row`.** An `account.id` is a SQLite rowid
/// (`AUTOINCREMENT` since stage 4 of the settings-store plane, which does not widen the scope —
/// the mark is a row of the same file), stable only for the life of ONE database FILE, and this
/// tree documents a recovery that deletes the database and migrates again — after which the same
/// accounts come back numbered differently. A parked record outlives that, so an id-keyed one
/// would let a fold stamp a stranger's row, which on this venue is the wrong legal entity. The
/// prefix is the store's own account identity (`crates/vike-secrets/src/schema.rs`'s
/// `Classification::owner_prefix`) and is what [`row_account`] already keys the broker mapping on,
/// so the mount and the fold cannot disagree about which row is which. The row id rides along as
/// EVIDENCE only.
fn confirmation_for(
    mount: &DukascopyMount,
    handshake_account: &str,
    at_ms: i64,
) -> vike_model::account_confirmation::ConfirmationRecord {
    vike_model::account_confirmation::ConfirmationRecord {
        venue: VENUE.to_string(),
        key_prefix: mount.account.key_prefix().to_string(),
        // ⚠ UNLABELLED, always, and by DECISION rather than by omission: the owner refused the
        // provisional `DEMO1`/`DEMO2` label spellings at the credential schema's signature, so both
        // dukascopy rows carry `label = NULL` and the key prefix is the only thing that tells them
        // apart. That is the exact case prefix addressing was built for, and filling the labelled
        // address here would re-point the record at an index that constrains nothing for this venue.
        label: None,
        tier: None,
        handshake_account_id: handshake_account.to_string(),
        observed_row: mount.row,
        observed_book: mount.book.clone(),
        at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::credentials::Account;

    fn row(id: i64, book: Option<&str>, label: Option<&str>, active: bool) -> Account {
        Account {
            id,
            venue: VENUE.to_string(),
            tier: "demo".to_string(),
            label: label.map(str::to_string),
            venue_account_id: book.map(str::to_string),
            parent_id: None,
            active,
            last_verified_at: None,
            // The arming column is DERIVED from the store's `venue_arming` rows
            // (`vike_secrets::Account::armed`), and nothing on this path reads it — these rows
            // are planted to exercise BOOK identity, which `armed` does not participate in.
            armed: false,
        }
    }

    /// ⚠ The two prefixes are taken from the BRIDGE rather than spelled here, for two reasons: a
    /// fixture that re-spelled them could drift from the loader it is meant to describe, and
    /// `crates/vike-ops/tests/settings_registry.rs` harvests env-shaped string literals out of this
    /// tree and would read a hand-written `"DUKASCOPY_DEMO1_"` as a variable this crate reads.
    const SWISS_KEYS: &str = DukascopyAccount::Demo1.key_prefix();
    const EU_KEYS: &str = DukascopyAccount::Demo2.key_prefix();

    fn keys(rows: &[(i64, &[&str])]) -> BTreeMap<i64, AccountKeys> {
        rows.iter()
            .map(|(id, prefixes)| {
                (
                    *id,
                    AccountKeys {
                        prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
                        names: Vec::new(),
                    },
                )
            })
            .collect()
    }

    /// The live shape: two unlabelled demo rows, distinguishable only by their key prefixes, with
    /// the books an operator wrote against them.
    fn live_store() -> (Accounts, BTreeMap<i64, AccountKeys>) {
        (
            Accounts::Known(vec![
                row(7, Some("3709890"), None, true),
                row(8, Some("3716974"), None, true),
            ]),
            keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]),
        )
    }

    fn label(text: &str) -> AccountLabel {
        AccountLabel::parse(text).expect("a legal label")
    }

    /// **The whole point of the change**: the row decides the broker, and the two rows decide
    /// differently.
    #[test]
    fn each_row_maps_to_its_own_broker() {
        let (accounts, keys) = live_store();
        let eu = resolve_account(&label("3716974"), &accounts, Some(&keys)).expect("row 8");
        assert_eq!(eu.account, DukascopyAccount::Demo2);
        assert_eq!(eu.row, Some(8));
        assert_eq!(eu.book.as_deref(), Some("3716974"));

        let swiss = resolve_account(&label("3709890"), &accounts, Some(&keys)).expect("row 7");
        assert_eq!(swiss.account, DukascopyAccount::Demo1);
        assert_eq!(swiss.row, Some(7));
    }

    /// …and it is keyed on the PREFIX, not on the book: swap which row carries which key family and
    /// the same address resolves to the other broker.
    #[test]
    fn the_key_prefix_decides_the_broker_not_the_book() {
        let (accounts, _) = live_store();
        let swapped = keys(&[(7, &[EU_KEYS]), (8, &[SWISS_KEYS])]);
        let mount = resolve_account(&label("3716974"), &accounts, Some(&swapped)).expect("row 8");
        assert_eq!(mount.account, DukascopyAccount::Demo1, "row 8 now owns the DEMO1 keys");
    }

    /// A row an operator DID name is addressable by that name too, without this function changing.
    #[test]
    fn a_written_label_addresses_its_row() {
        let accounts =
            Accounts::Known(vec![row(7, None, None, true), row(8, None, Some("EU"), true)]);
        let k = keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]);
        let mount = resolve_account(&label("EU"), &accounts, Some(&k)).expect("row 8");
        assert_eq!(mount.account, DukascopyAccount::Demo2);
        assert_eq!(mount.row, Some(8));
    }

    /// **The DEFAULT account is Demo1 on every store**, including one whose rows say nothing useful
    /// and one that cannot be asked at all.
    #[test]
    fn the_default_account_is_always_the_swiss_bank() {
        let (accounts, k) = live_store();
        assert_eq!(
            resolve_account(&AccountLabel::Default, &accounts, Some(&k)).expect("default").account,
            DukascopyAccount::Demo1
        );
        // …and it carries the row, so the mount can say which one it is.
        assert_eq!(
            resolve_account(&AccountLabel::Default, &accounts, Some(&k)).expect("default").row,
            Some(7)
        );

        // A store that cannot be asked — the Files box. Same broker, no row.
        let files = Accounts::Unanswerable(NoAccountTable::FileStore {
            file: std::path::PathBuf::from("/p/settings/secrets.env"),
        });
        let mount = resolve_account(&AccountLabel::Default, &files, None).expect("default");
        assert_eq!(mount.account, DukascopyAccount::Demo1);
        assert_eq!(mount.row, None);
        assert_eq!(mount.book, None);

        // …and a table that exists and holds NO dukascopy row at all.
        let empty = Accounts::Known(Vec::new());
        assert_eq!(
            resolve_account(&AccountLabel::Default, &empty, None).expect("default").account,
            DukascopyAccount::Demo1
        );

        // ⚠ Even a store whose ONLY dukascopy row owns the DEMO2 keys: the default account is not
        // re-pointed at another broker by a migration nobody asked to change routing.
        let only_eu = Accounts::Known(vec![row(8, Some("3716974"), None, true)]);
        let mount =
            resolve_account(&AccountLabel::Default, &only_eu, Some(&keys(&[(8, &[EU_KEYS])])))
                .expect("default");
        assert_eq!(mount.account, DukascopyAccount::Demo1);
        assert_eq!(mount.row, None, "no row owns the DEMO1 keys, so none is attached");
    }

    /// A labelled account on a store with no `account` table is REFUSED — never coerced onto the
    /// default account's broker.
    #[test]
    fn a_labelled_account_without_a_table_is_refused() {
        let files = Accounts::Unanswerable(NoAccountTable::FileStore {
            file: std::path::PathBuf::from("/p/settings/secrets.env"),
        });
        let err = resolve_account(&label("3716974"), &files, None).expect_err("refused");
        assert!(matches!(err, DukascopyRefusal::NoAccountTable { .. }));
        let text = err.to_string();
        assert!(text.contains("3716974"), "{text}");
        assert!(text.contains("PAPER"), "{text}");
        assert!(text.contains("LEGAL ENTITIES"), "{text}");

        let old = Accounts::Unanswerable(NoAccountTable::OlderSchema {
            path: std::path::PathBuf::from("/p/settings/db/vike.db"),
            found: 1,
        });
        let err = resolve_account(&label("EU"), &old, None).expect_err("refused");
        assert!(err.to_string().contains("schema 1"), "{err}");
    }

    /// An address that names no row is refused, and the refusal NAMES the rows that do exist so an
    /// operator can see what they could have written.
    #[test]
    fn an_unknown_address_is_refused_and_lists_the_rows() {
        let (accounts, k) = live_store();
        let err = resolve_account(&label("9999999"), &accounts, Some(&k)).expect_err("refused");
        let text = err.to_string();
        assert!(matches!(err, DukascopyRefusal::NoSuchAccount { .. }));
        for needle in ["9999999", "id 7", "id 8", "3709890", "3716974", SWISS_KEYS] {
            assert!(text.contains(needle), "the refusal must carry {needle:?}: {text}");
        }
        assert!(text.contains("NOT routed to the default"), "{text}");
    }

    /// An INACTIVE row is not a candidate — a retired account must not be mountable by the address
    /// that used to reach it.
    #[test]
    fn an_inactive_row_is_not_addressable() {
        let accounts = Accounts::Known(vec![
            row(7, Some("3709890"), None, true),
            row(8, Some("3716974"), None, false),
        ]);
        let k = keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]);
        let err = resolve_account(&label("3716974"), &accounts, Some(&k)).expect_err("refused");
        assert!(matches!(err, DukascopyRefusal::NoSuchAccount { .. }), "{err}");
    }

    /// A row whose key names name neither family — or BOTH — is refused rather than guessed at.
    #[test]
    fn a_row_that_names_no_broker_is_refused() {
        let accounts = Accounts::Known(vec![row(8, Some("3716974"), None, true)]);

        // …no key family at all; BOTH families; and ANOTHER venue's family (built rather than
        // spelled, for the reason `SWISS_KEYS` above carries).
        let other_venue = SWISS_KEYS.replace("DUKASCOPY", "SOMEVENUE");
        for prefixes in [&[][..], &[SWISS_KEYS, EU_KEYS][..], &[other_venue.as_str()][..]] {
            let k = keys(&[(8, prefixes)]);
            let err = resolve_account(&label("3716974"), &accounts, Some(&k)).expect_err("refused");
            assert!(matches!(err, DukascopyRefusal::UnmappableRow { .. }), "{err}");
            assert!(err.to_string().contains("row 8"), "{err}");
        }

        // …and a row the key reader knows nothing about at all.
        let err = resolve_account(&label("3716974"), &accounts, None).expect_err("refused");
        assert!(matches!(err, DukascopyRefusal::UnmappableRow { .. }), "{err}");
    }

    /// Two rows answering to one address is refused, not picked.
    #[test]
    fn two_rows_on_one_address_are_refused() {
        let accounts = Accounts::Known(vec![
            row(7, Some("3709890"), None, true),
            row(8, None, Some("3709890"), true),
        ]);
        let k = keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]);
        let err = resolve_account(&label("3709890"), &accounts, Some(&k)).expect_err("refused");
        assert!(matches!(err, DukascopyRefusal::Ambiguous { .. }), "{err}");
        assert!(err.to_string().contains("more than one"), "{err}");
    }

    /// The sidecar claim is one per process, and the refusal it produces says why.
    #[test]
    fn the_sidecar_is_claimed_once() {
        let flag = AtomicBool::new(false);
        let held = claim_in(&flag).expect("the first caller claims it");
        assert!(claim_in(&flag).is_none(), "and every later one is refused while it is held");
        held.keep();
        assert!(claim_in(&flag).is_none(), "a COMMITTED claim outlives the guard");
        let text =
            DukascopyRefusal::SidecarAlreadyClaimed { label: "3716974".to_string() }.to_string();
        assert!(text.contains("platform cache"), "{text}");
        assert!(text.contains("own project folder"), "{text}");
    }

    /// **A claim that is NOT committed is released** — the defect that made a failed spawn refuse
    /// every later account in the process with a reason that was not true.
    ///
    /// Driven over [`claim_in`] rather than the process flag so it cannot depend on which test ran
    /// first; the mount's own commit point is [`crate::make_engine_for_account`]'s dukascopy arm,
    /// which calls `keep()` only after `spawn` returned a running client.
    #[test]
    fn a_claim_that_is_not_committed_is_released() {
        let flag = AtomicBool::new(false);
        {
            let _failed = claim_in(&flag).expect("claimed");
            assert!(claim_in(&flag).is_none(), "held while the spawn is in flight");
        }
        let second = claim_in(&flag).expect("a FAILED spawn leaves the sidecar claimable");
        second.keep();
        assert!(claim_in(&flag).is_none(), "…and the one that succeeded keeps it");
    }

    /// **The holder rule** — the whole of why a second dukascopy account can be mounted at all.
    #[test]
    fn the_policy_names_the_sidecar_holder_and_the_default_yields() {
        // No labelled account armed: the default keeps it. Every deployment today.
        assert_eq!(sidecar_holder(&[]), AccountLabel::Default);
        // Exactly one: it takes it, and the default is no longer the answer.
        assert_eq!(sidecar_holder(&[label("3716974")]), label("3716974"));
        // More than one: the first in label order, deterministically — a PICK about a process
        // resource, never about which broker an order reaches (see `sidecar_holder`).
        assert_eq!(
            sidecar_holder(&[label("3709890"), label("3716974")]),
            label("3709890"),
            "the pick must be deterministic, or two starts of one box would trade two brokers"
        );
    }

    /// A store failure is reported as a STORE FAILURE — for BOTH halves of the read, and never as a
    /// row that names no broker.
    #[test]
    fn a_store_that_will_not_open_is_not_reported_as_a_bad_row() {
        let unreadable = AccountDirectory::read(
            Err::<Accounts, _>("disk I/O error"),
            Err::<Option<BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
        );
        let err = resolve_in(&label("3716974"), &unreadable).expect_err("refused");
        assert!(matches!(err, DukascopyRefusal::StoreUnreadable { .. }), "{err}");
        assert!(err.to_string().contains("disk I/O error"), "{err}");
        // …and the DEFAULT account still mounts, because it never needed the table.
        assert_eq!(
            resolve_in(&AccountLabel::Default, &unreadable).expect("default").account,
            DukascopyAccount::Demo1
        );

        // ⚠ THE HALF THAT WAS SWALLOWED: rows fine, key NAMES unreadable. This used to resolve to
        // `UnmappableRow` — *this row's key names do not say which broker it is* — about a row
        // nobody could read the key names of.
        let (accounts, _) = live_store();
        let keys_failed = AccountDirectory::read(
            Ok::<_, &str>(accounts),
            Err::<Option<BTreeMap<i64, AccountKeys>>, _>("database is locked"),
        );
        let err = resolve_in(&label("3716974"), &keys_failed).expect_err("refused");
        assert!(
            matches!(err, DukascopyRefusal::StoreUnreadable { .. }),
            "a key-read failure must be a store failure, not a bad row: {err}"
        );
        let text = err.to_string();
        assert!(text.contains("database is locked"), "{text}");
        assert!(text.contains("STORE FAILURE"), "{text}");
    }

    /// A process that read NO store behaves exactly like a `Backend::Files` box: the default
    /// account mounts the Swiss bank, every labelled one is refused.
    #[test]
    fn an_unread_store_is_a_files_box() {
        let unread = AccountDirectory::unread();
        let mount = resolve_in(&AccountLabel::Default, &unread).expect("default");
        assert_eq!(mount.account, DukascopyAccount::Demo1);
        assert_eq!(mount.row, None);
        let err = resolve_in(&label("3716974"), &unread).expect_err("refused");
        assert!(matches!(err, DukascopyRefusal::NoAccountTable { .. }), "{err}");
    }

    /// …and a directory a caller DID read resolves both accounts, through the same entry point the
    /// mount uses.
    #[test]
    fn a_read_directory_resolves_each_account_through_the_mount_entry_point() {
        let (accounts, keys) = live_store();
        let dir = AccountDirectory::from_rows(accounts, Some(keys));
        assert_eq!(
            resolve_in(&label("3716974"), &dir).expect("row 8").account,
            DukascopyAccount::Demo2
        );
        assert_eq!(
            resolve_in(&AccountLabel::Default, &dir).expect("default").account,
            DukascopyAccount::Demo1
        );
    }

    /// Every refusal renders a sentence that says PAPER, so no arm can be read as a startup failure
    /// or as a silent fallback.
    #[test]
    fn every_refusal_says_it_stays_paper() {
        for refusal in [
            DukascopyRefusal::NoAccountTable { why: "w".into(), label: "L".into() },
            DukascopyRefusal::NoSuchAccount { label: "L".into(), known: Vec::new() },
            DukascopyRefusal::Ambiguous { label: "L".into(), matched: Vec::new() },
            DukascopyRefusal::UnmappableRow { label: "L".into(), row: 1, prefixes: Vec::new() },
            DukascopyRefusal::StoreUnreadable {
                label: "L".into(),
                what: "the `account` table",
                error: "e".into(),
            },
            DukascopyRefusal::SidecarHeldByAnother {
                label: "L".into(),
                holder: "dukascopy".into(),
            },
            DukascopyRefusal::SidecarAlreadyClaimed { label: "L".into() },
        ] {
            let text = refusal.to_string();
            assert!(text.contains("PAPER"), "{refusal:?}: {text}");
            assert!(text.contains('L'), "{refusal:?} must name the account: {text}");
            assert!(text.len() > 60, "{refusal:?}: {text}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // The HANDSHAKE half — what gets parked, and what addresses it
    // -----------------------------------------------------------------------------------------

    fn mounted(account: DukascopyAccount, row: Option<i64>, book: Option<&str>) -> DukascopyMount {
        DukascopyMount { account, row, book: book.map(str::to_string) }
    }

    /// ⚠ **THE ADDRESS IS THE KEY PREFIX, and the row id is EVIDENCE.**
    ///
    /// `account.id` is a rowid stable only within one database FILE, and a parked record outlives a
    /// re-migration; keying on it would let a fold stamp a stranger's row, which on this venue is
    /// the wrong legal entity. This is the assertion that keeps that straight.
    #[test]
    fn a_parked_confirmation_is_addressed_by_key_prefix_not_by_row_id() {
        let rec = confirmation_for(
            &mounted(DukascopyAccount::Demo2, Some(8), Some("3716974")),
            "DEMO2cGyrc",
            1_787_356_800_000,
        );
        assert_eq!(rec.venue, VENUE);
        assert_eq!(rec.key_prefix, DukascopyAccount::Demo2.key_prefix(), "the ADDRESS");
        assert_eq!(rec.observed_row, Some(8), "the row id is carried as evidence");
        assert_eq!(rec.observed_book.as_deref(), Some("3716974"), "…and so is what the store said");
        assert_eq!(rec.handshake_account_id, "DEMO2cGyrc", "…and the venue's answer, verbatim");
        assert_eq!(rec.at_ms, 1_787_356_800_000, "the HANDSHAKE's instant, supplied by the caller");

        // The two accounts park under DIFFERENT addresses, which is the property the whole schema
        // needs: two credential sets, two books, two entries that cannot overwrite each other.
        let other = confirmation_for(&mounted(DukascopyAccount::Demo1, Some(7), None), "DEMO1x", 0);
        assert_ne!(rec.key_prefix, other.key_prefix);
    }

    /// A mount the store could not answer for still parks a record: the key prefix is known from
    /// the BROKER the mount resolved, and it is the address. `observed_row` is `None` — a
    /// `Backend::Files` box, an older schema, or a process that declared no project.
    #[test]
    fn a_row_less_mount_still_has_an_address() {
        let rec = confirmation_for(&mounted(DukascopyAccount::Demo1, None, None), "DEMO1x", 0);
        assert_eq!(rec.key_prefix, DukascopyAccount::Demo1.key_prefix());
        assert_eq!(rec.observed_row, None);
        assert_eq!(rec.observed_book, None);
    }

    /// The three verdicts as this arm will meet them, over the real key prefixes. The DISAGREEMENT
    /// row is the one that matters: the store says one book, the venue answers another, and nothing
    /// is written.
    #[test]
    fn the_verdict_is_computed_from_the_row_the_mount_actually_used() {
        use vike_model::account_confirmation::{Verdict, verdict};
        let learns = mounted(DukascopyAccount::Demo1, Some(7), None);
        assert_eq!(verdict("DEMO1x", learns.book.as_deref()), Verdict::Learns);

        let confirms = mounted(DukascopyAccount::Demo1, Some(7), Some("DEMO1x"));
        assert_eq!(verdict("DEMO1x", confirms.book.as_deref()), Verdict::Confirms);

        let disagrees = mounted(DukascopyAccount::Demo1, Some(7), Some("3709890"));
        assert_eq!(
            verdict("DEMO1x", disagrees.book.as_deref()),
            Verdict::Disagrees { stored: "3709890".to_string() },
            "a stored number against a login-shaped answer is REPORTED, not classified away"
        );
    }
}
