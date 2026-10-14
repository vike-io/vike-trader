//! **Which `account` row a Dukascopy start trades** — pure resolution over the settings database's
//! own rows, with no store access, no process state, and no knowledge of the process's ONE JForex
//! sidecar.
//!
//! # Split from `vike-tradehub-core` (decision 0088, Verdict 3)
//!
//! This logic used to live whole at `crates/vike-tradehub-core/src/dukascopy.rs`, placed there per
//! [0065](../../../../docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md)'s
//! own citations of it. Decision
//! [0088](../../../../docs/decisions/0088-mount-sheds-venue-facts-to-their-bridges.md) (Verdict 3)
//! moved the pure half here.
//!
//! What stayed in `vike-tradehub-core` then — `resolve_in` (the thin wrapper over the composition root's
//! `vike_secrets::account_directory::AccountDirectory`), the two one-sidecar refusal texts and
//! the confirmation park — moved down too, into `crate::adapter`, with the venue-adapter contract
//! (docs/decisions/0096). Only the generic one-per-process RULE is `vike-tradehub-core`'s: `holder` in
//! `crates/vike-tradehub-core/src/exclusive.rs` (which account holds the process's one sidecar, from the
//! arming projection over every labelled account) and `claim` (the process-wide backstop). That
//! rule must not move here: this crate's own `tests/dukascopy_exec.rs` spawns stub sidecars
//! repeatedly within one test process, and a process-wide guard living in this crate would collide
//! with that.
//!
//! # An account is its ROW (decision 0156 item 6)
//!
//! A credential belongs to its account, and the account's tier is written in ONE place,
//! `account.tier`. So the account a start trades is the ONE active row of
//! `(dukascopy, label, tier)` — [`resolve_account`] — and its credentials are that row's `LOGIN` /
//! `PASSWORD` fields (`AdapterInputs::credentials`, which `vike-tradehub-core` resolves from the same
//! address). The owner's two demo accounts are two rows at tier `demo` with two LABELS; the
//! default (unlabelled) account resolves like any other account, through its row.
//!
//! No key NAME is read for any of it: no credential name carries an account or a tier any more, so
//! the key-prefix mapping that used to pick a broker out of a row's key names is gone, and with it
//! the claim that this crate can tell which LEGAL ENTITY (Dukascopy Bank SA or Dukascopy Europe IBS
//! AS) an account is. The row's label, id and book are what an operator reads to tell two accounts
//! apart.
//!
//! # A refusal is a refusal, never a fallback
//!
//! An account the store cannot resolve — no `account` table, no such row, two rows answering one
//! address, a store that will not open — does NOT RUN and says so by name ([`DukascopyRefusal`]).
//! Nothing here picks one of two candidates or routes an account onto another's credentials.
//!
//! # How an operator addresses a second account
//!
//! Give it its own row and label, then its keys:
//!
//! ```text
//! vike-cli secrets accounts                       # which rows exist
//! vike-cli secrets account add --venue dukascopy --tier demo --label EU
//! vike-cli secrets set-book --id 8 --venue-account-id 3716974   # the number the venue gave it
//! ```
//!
//! Each row trades at its own `tier` while it is `active` (a row is born active):
//! `vike-cli secrets account deactivate --id 8` is the off switch. ⚠ The process runs ONE JForex
//! sidecar: `vike_tradehub_core::exclusive::pick_holder` decides which account gets it, from the account
//! table, before anything is started (a named labelled account takes it and the DEFAULT account
//! declines).

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_confirmation::ConfirmationRecord;
use vike_secrets::{Account, Accounts, NoAccountTable};

use crate::recon_client::VENUE;

/// **The account a dukascopy start resolved to** — its `account` row.
///
/// Formerly `DukascopyMount`, renamed in the shape-and-names change (decision 0166).
///
/// Every field is for the LOG, the confirmation park and an operator's eyes. None is a secret — a
/// row id is an opaque integer, a label is the operator's word and a `venue_account_id` is the
/// number the venue prints on its own page — while the login and password this never carries are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DukascopyAccount {
    /// `account.id` — the identity.
    pub row: i64,
    /// `account.label`; `None` for the unlabelled (default) account.
    pub label: Option<String>,
    /// `account.tier`, the tier the row was resolved at.
    pub tier: AccountTier,
    /// `account.venue_account_id`, when the row carries one. `None` is *not yet known*, never *this
    /// account has no book*.
    pub book: Option<String>,
}

/// **Why a dukascopy account could not be resolved to its row.** Every arm is a REFUSAL: the start
/// builds no exec client and stays paper.
///
/// ⚠ There is deliberately no arm that falls back to another row: an account nobody could identify
/// must not trade on credentials the operator did not file against it.
///
/// ⚠ **This enum used to also carry the process-wide sidecar-contention refusals
/// (`SidecarHeldByAnother`/`SidecarAlreadyClaimed`), and they are gone from here on purpose** —
/// decision 0088 moved them to a `vike-tradehub-core` enum, because they are a fact about the WHOLE PROCESS
/// rather than about an account row, and this crate's own test infrastructure
/// (`tests/dukascopy_exec.rs`) spawns multiple stub sidecars in one process, which a process-wide
/// guard living here would collide with. Since the venue-adapter contract (docs/decisions/0096) the
/// two TEXTS are this crate's again — `crate::adapter`'s `sidecar_held_by_another` and
/// `sidecar_already_claimed`, declared as the venue's `ProcessExclusive` — while the RULE that
/// decides when either is spoken stays in `crates/vike-tradehub-core/src/exclusive.rs`, for that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DukascopyRefusal {
    /// The store cannot be asked which accounts exist — a `Backend::Absent` box, or a process that
    /// read no store.
    NoAccountTable {
        /// Why there is no table to ask — rendered verbatim.
        why: String,
        /// The account that could not be resolved.
        label: String,
    },
    /// The store answered and no ACTIVE dukascopy row has this label at this tier.
    NoSuchAccount {
        label: String,
        tier: AccountTier,
        /// Every active dukascopy row, rendered for an operator: id, book, tier, label.
        known: Vec<String>,
    },
    /// **The store EXISTS and would not open** — a store FAILURE, reported as itself.
    ///
    /// ⚠ A variant of its own because the halves of the read fail INDEPENDENTLY (the `account` rows
    /// and the account's credentials), and folding a failed read into "no such account" or "no
    /// credentials" would send an operator to fix a row that was never at fault. `what` names WHICH
    /// read failed.
    StoreUnreadable { label: String, what: &'static str, error: String },
}

impl std::fmt::Display for DukascopyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DukascopyRefusal::NoAccountTable { why, label } => write!(
                f,
                "dukascopy account `{label}` cannot be resolved to an account row: {why}. An \
                 account trades through its own `account` row — its tier and its credentials are \
                 the row's — and the settings database's `account` table is the only thing that \
                 says which row an account is, so this account stays PAPER rather than trading on \
                 credentials nobody filed against it. `vike-cli secrets init` creates the store"
            ),
            DukascopyRefusal::NoSuchAccount { label, tier, known } => write!(
                f,
                "dukascopy account `{label}` names no active `{tier}` row in the settings \
                 database, so it stays PAPER — it is NOT routed to any other account. The \
                 dukascopy rows that exist are: {}. Give the account its own row \
                 (`vike-cli secrets account add --venue dukascopy --tier {tier} --label <LABEL>`) \
                 or address one of these by its label",
                render_list(known)
            ),
            DukascopyRefusal::StoreUnreadable { label, what, error } => write!(
                f,
                "dukascopy account `{label}` stays PAPER: the settings store EXISTS and would not \
                 open, so {what} could not be read ({error}). This is a STORE FAILURE, not a \
                 statement about the account — the row may be perfectly good, and nothing here \
                 says otherwise. Fix the store (`vike-cli secrets path` prints where it is, \
                 `vike-cli secrets accounts` reads the same table)"
            ),
        }
    }
}

/// `a`, `b` and `c` — or `(none)`, which has to read as an answer rather than as an empty string.
fn render_list(items: &[String]) -> String {
    if items.is_empty() { "(none)".to_string() } else { items.join(", ") }
}

/// One active row, rendered for an operator. Never a credential: `id` is an opaque integer, the
/// book is the number the venue prints, the tier and label are the row's own words.
fn render_row(row: &Account) -> String {
    let book = row.venue_account_id.as_deref().unwrap_or("book not yet known");
    let label = row.label.as_deref().unwrap_or("no label");
    format!("id {} [{book}] {} ({label})", row.id, row.tier)
}

/// **THE resolution — pure, over the store's own answer**: the ONE active dukascopy row whose
/// label is `label` (`NULL` for [`AccountLabel::Default`]) and whose tier is `tier`.
///
/// The same address `vike_secrets::account_directory::AccountDirectory::account_row` answers —
/// which is how `vike-tradehub-core` picks the credentials it hands the start — but with every miss NAMED:
/// a store with no table, no such row, two rows answering one address. Inactive rows are not
/// candidates (a retired account must not be startable by the address that used to reach it). The
/// default account is not special: it is the unlabelled row, resolved like any other.
pub fn resolve_account(
    label: &AccountLabel,
    tier: AccountTier,
    accounts: &Accounts,
) -> Result<DukascopyAccount, DukascopyRefusal> {
    // `Accounts::Unanswerable` is not "no accounts" — it is "this store cannot be asked" — and the
    // two may not be merged (`vike_secrets::Accounts`).
    if accounts.known().is_none() {
        let why = match accounts.unanswerable() {
            Some(NoAccountTable::NoStore { db }) => format!(
                "there is no settings database at {} and so no `account` table",
                db.display()
            ),
            None => "the store could not be asked".to_string(),
        };
        return Err(DukascopyRefusal::NoAccountTable { why, label: label.to_string() });
    }

    let active: Vec<&Account> = accounts.active_for_venue(VENUE).unwrap_or_default();
    let matched: Vec<&Account> = active
        .iter()
        .copied()
        .filter(|r| r.tier == tier && r.label.as_deref() == label.text())
        .collect();
    // SEVERAL rows may answer one address — the owner's two demo books are two ACTIVE rows at
    // tier `demo`, both unlabelled. The one with the LOWEST account id is the account (the same
    // rule `vike_secrets::account_directory::AccountDirectory::account_row` applies to the
    // credentials it hands the start, so the start and the login always agree on the row); the
    // others are reached by giving them a label of their own. The process runs ONE sidecar either
    // way (`vike_tradehub_core::exclusive`), so this is a deterministic choice among books, never a second
    // engine on one account.
    match matched.iter().copied().min_by_key(|r| r.id) {
        None => Err(DukascopyRefusal::NoSuchAccount {
            label: label.to_string(),
            tier,
            known: active.iter().copied().map(render_row).collect(),
        }),
        Some(row) => Ok(DukascopyAccount {
            row: row.id,
            label: row.label.clone(),
            tier: row.tier,
            book: row.venue_account_id.clone(),
        }),
    }
}

/// **The parked record a start would write for this resolution** — pure, so the ADDRESSING can be
/// tested without a declared project, a state directory or a clock. Called from `crate::adapter`'s
/// `record_confirmation`, which supplies `at_ms` and performs the park into the state directory
/// its caller hands it (this module reads no process-global state and writes no file).
///
/// ⚠ **The address is the row's LABEL + TIER, never `start.row`** — the labelled shape
/// `vike_tradehub_core::book_identity::confirmation_for_account` parks: `key_prefix` EMPTY, `label` and
/// `tier` set, the store's own `UNIQUE (venue, tier, label)` index. An `account.id` is a SQLite
/// rowid, stable only for the life of ONE database FILE, and a parked record outlives a database
/// that is recreated — so an id-keyed record could let a fold stamp a stranger's row. The row id
/// rides along as EVIDENCE only.
///
/// `None` for an UNLABELLED account: it has no label to be addressed by (and no credential key
/// prefix this crate reads), so there is nothing to park it under — the verdict is still logged by
/// `record_confirmation`.
pub fn confirmation_for(
    account_row: &DukascopyAccount,
    handshake_account: &str,
    at_ms: i64,
) -> Option<ConfirmationRecord> {
    let label = account_row.label.clone()?;
    Some(ConfirmationRecord {
        venue: VENUE.to_string(),
        key_prefix: String::new(),
        label: Some(label),
        tier: Some(account_row.tier),
        handshake_account_id: handshake_account.to_string(),
        observed_row: Some(account_row.row),
        observed_book: account_row.book.clone(),
        at_ms,
    })
}

#[path = "account_tests.rs"]
#[cfg(test)]
mod account_tests;
