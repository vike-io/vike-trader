//! **Which BROKER a Dukascopy credential-holding `account` row names** — pure resolution over the
//! settings database's own rows, with no store access, no process state, and no knowledge of the
//! process's ONE JForex sidecar.
//!
//! # Split from `vike-mount` (decision 0088, Verdict 3)
//!
//! This logic used to live whole at `crates/vike-mount/src/dukascopy.rs`, placed there per
//! [0065](../../../../docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md)'s
//! own citations of it. Decision
//! [0088](../../../../docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md) (Verdict 3)
//! moved the pure half here: **which broker a credential row names is a fact about Dukascopy's own
//! credential grammar**, needing nothing `vike-mount`-specific to answer.
//!
//! What stayed in `vike-mount` then — `resolve_in` (the thin wrapper over the composition root's
//! `vike_bridge_core::account_directory::AccountDirectory`), the two one-sidecar refusal texts and
//! the confirmation park — moved down too, into `crate::mount`, with the venue mount contract
//! (docs/decisions/0096). Only the generic one-per-process RULE is `vike-mount`'s: `holder` in
//! `crates/vike-mount/src/exclusive.rs` (which account holds the process's one sidecar, from the
//! arming projection over every labelled account) and `claim` (the process-wide backstop). That
//! rule must not move here: this crate's own `tests/dukascopy_exec.rs` spawns stub sidecars
//! repeatedly within one test process, and a process-wide guard living in this crate would collide
//! with that.
//!
//! # The defect this module removes
//!
//! Dukascopy's two demo accounts are two LEGAL ENTITIES — Dukascopy Bank SA (Swiss/global) and
//! Dukascopy Europe IBS AS (EU) — with two credential sets, `DUKASCOPY_DEMO1_*` and
//! `DUKASCOPY_DEMO2_*`. The mount's arm used to read the first pair whatever account it was
//! mounting, and `vike_mount::arming::arm_addresses_accounts` refused the venue outright so that
//! nothing could ask it to do otherwise. `docs/superpowers/specs/2026-09-14-the-credential-schema.md`
//! ruling 13 refused every KEY-GRAMMAR fix for that and said to wait for the database: *"once the
//! account is a COLUMN, a name carries no account, no tier and no index."* The column landed
//! (schema 2), its reader landed (`vike_secrets::read_accounts`), and the key-name reader beside it
//! (`vike_secrets::read_account_keys`) landed after it. This is what they were for.
//!
//! # What keys the mapping, and what deliberately does not
//!
//! **The row's credential-key OWNER PREFIX** — `DUKASCOPY_DEMO1_` or `DUKASCOPY_DEMO2_` — is what
//! decides the broker, through [`DukascopyAccount::from_key_prefix`]. Three reasons, strongest
//! first:
//!
//! 1. **It is the store's own account identity.** `crates/vike-secrets/src/schema.rs`'s
//!    `Classification::owner_prefix` says so in its own words — *an account's owner prefix is
//!    recoverable from any one of its own rows* — and that is exactly why the migration needs no
//!    stored discriminator for the one pair that shares `(venue, tier, label)`. Keying on it means
//!    the mount and the migration agree about which row is which by construction.
//! 2. **It is the fact the LOADER needs.** [`DukascopyAccount::key_prefix`] IS the prefix of the
//!    two credential names [`crate::load_dukascopy_config_from`] reads, so the mapping is an inverse
//!    rather than a table of magic values that could drift from either side.
//! 3. **It is deployment-independent.** Every migrated box derives the same two prefixes from the
//!    same two key families; nothing about it is a property of one operator's accounts.
//!
//! **`venue_account_id` — the BOOK — is the ADDRESS rather than the mapping**, and the difference
//! matters. It is what an operator WRITES to say which account they mean
//! (`policy.accounts.dukascopy.<BOOK>`), and it is the one identity the venue itself supplies; but
//! it is `NULL` on every row a migration writes, so a box that has not run `vike-cli secrets
//! set-book` has no book on either row and could map nothing at all if the book were the mapping.
//! It is also per-deployment DATA: the two numbers on the owner's boxes are his accounts, and a
//! constant table of them compiled into a shipped bridge would be the identity-in-code defect this
//! whole schema exists to stop carrying.
//!
//! **`label` is not used for either job, and cannot be.** Both dukascopy rows carry `NULL`: the
//! owner refused the migration's provisional `DEMO1`/`DEMO2` spellings (*labels are informative and
//! optional, `id` is the identity*). A row that DOES carry a label an operator wrote is matched too
//! — see [`resolve_account`] — but nothing here invents one and nothing depends on one existing.
//!
//! # What [`AccountLabel::Default`] means here
//!
//! **[`DEFAULT_ACCOUNT`], always, on every box, whatever the database holds** — see
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
//! family row 8 owns — **and the DEFAULT account DECLINES**: `vike_mount::exclusive::pick_holder`
//! decides which account gets the process's one JForex sidecar, from the policy, before anything is
//! mounted. A box that names NO dukascopy account under `[accounts]` is untouched: the default
//! account mounts `DUKASCOPY_DEMO1_*` exactly as it always has.

use std::collections::BTreeMap;

use vike_bridge_core::credentials::{Account, AccountKeys, Accounts, NoAccountTable};
use vike_model::account_confirmation::ConfirmationRecord;
use vike_model::account_keys::AccountLabel;

use crate::DukascopyAccount;
use crate::recon_client::VENUE;

/// **What [`AccountLabel::Default`] means on this venue** — Dukascopy Bank SA, the Swiss entity,
/// whatever any store says.
///
/// ⚠ Spelled ONCE, as a constant, because two sites answer with it ([`resolve_account`]'s default
/// branch and `crate::mount`'s `resolve_in` unread-store branch) and a box whose two
/// answers disagreed would mount a different BROKER depending on whether its root had read the
/// store. The argument for the value is at [`resolve_account`].
pub const DEFAULT_ACCOUNT: DukascopyAccount = DukascopyAccount::Demo1;

/// **The account a dukascopy mount resolved to**, with the evidence that chose it.
///
/// `row` and `book` are for the LOG and for an operator's eyes. Neither is a secret — a
/// `venue_account_id` is the number the venue prints on its own page, and a row id is an opaque
/// integer — while the login and password this never carries are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DukascopyMount {
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
/// ⚠ There is deliberately no arm that falls back to [`DukascopyAccount::Demo1`]. Demo1 is the
/// Swiss bank; coercing an account nobody could identify onto it would place orders with a
/// counterparty the operator did not choose, under a different regulator, and would do it silently.
///
/// ⚠ **This enum used to also carry the process-wide sidecar-contention refusals
/// (`SidecarHeldByAnother`/`SidecarAlreadyClaimed`), and they are gone from here on purpose** —
/// decision 0088 moved them to a `vike-mount` enum, because they are a fact about the WHOLE PROCESS
/// rather than about a credential row, and this crate's own test infrastructure
/// (`tests/dukascopy_exec.rs`) spawns multiple stub sidecars in one process, which a process-wide
/// guard living here would collide with. Since the venue mount contract (docs/decisions/0096) the
/// two TEXTS are this crate's again — `crate::mount`'s `sidecar_held_by_another` and
/// `sidecar_already_claimed`, declared as the venue's `ProcessExclusive` — while the RULE that
/// decides when either is spoken stays in `crates/vike-mount/src/exclusive.rs`, for that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DukascopyRefusal {
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
        }
    }
}

/// `a`, `b` and `c` — or `(none)`, which has to read as an answer rather than as an empty string.
fn render_list(items: &[String]) -> String {
    if items.is_empty() { "(none)".to_string() } else { items.join(", ") }
}

/// One active row, rendered for an operator. Never a credential: `id` is an opaque integer, the
/// book is the number the venue prints, and the prefixes are key NAMES.
fn render_row(row: &Account, keys: Option<&AccountKeys>) -> String {
    let book = row.venue_account_id.as_deref().unwrap_or("book not yet known");
    let prefixes = keys.map(|k| k.prefixes.clone()).unwrap_or_default();
    let label = row.label.as_deref().unwrap_or("no label");
    format!("id {} [{book}] keys {} ({label})", row.id, render_list(&prefixes))
}

/// **THE resolution — pure, over the store's own answer.**
///
/// # [`AccountLabel::Default`] is [`DEFAULT_ACCOUNT`], unconditionally, and that is a decision
///
/// A single-account box has mounted `DUKASCOPY_DEMO1_*` — Dukascopy Bank SA, the Swiss entity —
/// since the arm was written, and `crate::mount` reaches this function at
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
pub fn resolve_account(
    label: &AccountLabel,
    accounts: &Accounts,
    keys: Option<&BTreeMap<i64, AccountKeys>>,
) -> Result<DukascopyMount, DukascopyRefusal> {
    let active: Vec<&Account> = accounts.active_for_venue(VENUE).unwrap_or_default();

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

    let matched: Vec<&Account> = active
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

/// **The parked record a mount would write for this resolution** — pure, so the ADDRESSING can be
/// tested without a declared project, a state directory or a clock. Called from `crate::mount`'s
/// `record_confirmation`, which supplies `at_ms` and performs the park into the state directory
/// its caller hands it (this module reads no process-global state and writes no file).
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
pub fn confirmation_for(
    mount: &DukascopyMount,
    handshake_account: &str,
    at_ms: i64,
) -> ConfirmationRecord {
    ConfirmationRecord {
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

#[path = "account_tests.rs"]
#[cfg(test)]
mod account_tests;
