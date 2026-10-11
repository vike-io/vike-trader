//! The ACCOUNT ADMIN plane (decision 0065): the account verbs and the replies they get.

use serde::{Deserialize, Serialize};

/// **The account-administration verbs** — the settings database's `account` table, plus the one
/// verb on this wire that carries a credential VALUE.
///
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` is the design, in
/// three parts:
///
/// 1. **STRUCTURAL.** The server holds the capability as an `Option` its BINARY constructs. `None`
///    means no path from a frame to the store and no [`crate::proto::FEATURE_ACCOUNT_VERBS`]: the
///    verb is refused because there is nothing to refuse WITH, not because a check was remembered.
/// 2. **AUTHORIZATION.** Every verb requires `Scope::Account`, a THIRD key. The scope byte is in
///    the signed preimage, so the Control key every desktop carries cannot write key material.
/// 3. **CONFIDENTIALITY — DECLARED, because the process cannot know it.** The wire is PLAINTEXT and
///    authenticates the CONNECTION (`crates/vike-tradehub/src/server.rs`'s module doc), so
///    confidentiality is REACHABILITY: loopback behind an SSH tunnel, or an operator-declared
///    barrier. 0065 §3b: "refuse unless on loopback" is the WRONG predicate; §3c replaces it.
///
/// # ⚠ NOT a [`WireCommand`](super::WireCommand), and the separation is structural
///
/// Every `WireCommand` is lowered into the core, vetted as an order and routed by
/// [`WireCommand::addressed_venue`](super::WireCommand::addressed_venue); an account verb enters no
/// core and names no book. The load-bearing half is `Debug`: `WireCommand` DERIVES it, and 0065
/// §4.1 forbids a credential-carrying variant from riding that derive. This type has its own
/// redacting impl instead, shaped like `vike_bridge_core::credentials::Debug for Credentials`
/// (minus its four-character key tail).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountRequest {
    /// Which act.
    pub verb: AccountVerb,
    /// **The typed confirm** for the verbs that require one ([`AccountVerb::required_confirm`]);
    /// ignored by the rest.
    ///
    /// ⚠ **The client may NEVER pre-fill this from a field it already holds**: *"the friction IS
    /// the protection"*. Decision 0065's, untouched by 0086 point 7 (which deleted the SETTINGS
    /// retype). The server enforces it (`crates/vike-tradehub/src/server/accounts.rs`'s
    /// `AccountAdminSource`), with distinct messages for missing and mismatched confirms.
    #[serde(default)]
    pub confirm: Option<String>,
}

/// The five lifecycle acts, plus the credential write beside them.
///
/// ⚠ **`Debug` is HAND-WRITTEN on [`AccountRequest`] and redacts [`AccountVerb::SetCredential`]'s
/// value.** Never derive it here: a credential would reach every `{:?}`, including a test panic
/// printed to a CI log.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum AccountVerb {
    /// LIST the account rows — ids, venue, tier, label, book, active, and each row's credential
    /// key NAMES.
    ///
    /// ⚠ `Scope::Account` only: it enumerates which venues hold live credentials (0065 §5). The
    /// one exception (owner ruling 2026-09-30) is [`WireDirectory`], the key-name-free subset an
    /// Observe peer may read. **No value ever crosses**: the reader
    /// (`vike_secrets::read_account_keys`) selects `name, field, account_id` and has no `value`
    /// column — structural, not a rule.
    List,
    /// ADD a row. ⚠ The row is born ACTIVE, and since decision 0119 an active account trades at its
    /// own tier: a non-`paper` row ARMS that account at the next restart, once its credential keys
    /// are in the store ([`AccountVerb::SetActive`] `false` is the off switch).
    Add {
        /// A `vike_model::VENUES` id, validated at the server.
        venue: String,
        /// One of `vike_secrets::ACCOUNT_TIERS`. ⚠ `paper` is the account a `{VENUE}_SIM_*`
        /// credential mints, and an account at tier `paper` never trades.
        tier: String,
        /// The operator's name for the ROLE, or `None` for the unlabelled account.
        label: Option<String>,
    },
    /// CHANGE one row's label, and nothing else.
    Rename {
        /// The row, by `account.id`.
        id: i64,
        /// The new label, or `None` to clear it.
        label: Option<String>,
    },
    /// DEACTIVATE or re-activate a row — the reversible act a UI leads with.
    SetActive {
        /// The row, by `account.id`.
        id: i64,
        /// `false` = the operator no longer uses this account.
        active: bool,
    },
    /// DELETE a row. Refused while it still owns credentials (named by KEY NAME); requires
    /// [`AccountRequest::confirm`] to equal the id.
    Remove {
        /// The row, by `account.id`.
        id: i64,
    },
    /// **The verb that carries a credential VALUE** — one named key, upserted into the store that
    /// answers on the node's box. Requires [`AccountRequest::confirm`] to equal `key`. The server
    /// validates `key` against `vike_model::credential_keys` and refuses a multi-line value; the
    /// write is a CALL SITE of `vike_secrets::save_credentials_to_store`, never a second writer.
    ///
    /// ⚠ **Nothing ever reads it back.** There is no `GetCredential` and must not be: a verb
    /// returning a credential VALUE is a named 0065 reopener.
    SetCredential {
        /// The credential key NAME, validated against the grid at the server.
        key: String,
        /// The value. **One line.** ⚠ It reaches no `Debug` ([`AccountRequest`]'s impl enforces
        /// that), no log, no error message and no reply.
        value: String,
    },
    /// **WRITE one row's `venue_account_id` — the BOOK, as the venue names it**; the wire twin of
    /// `vike-cli secrets set-book`.
    ///
    /// ⚠ **This decides which BROKER an order is attributed to** (Dukascopy's two demo accounts
    /// are two legal entities), hence the gated overwrite. No secret: the venue prints the id on
    /// its own page (`vike_secrets::Account::venue_account_id`), so it rides the ordinary `Debug`.
    SetBook {
        /// The row, by `account.id`.
        id: i64,
        /// The book, or `None` for *not yet known*. Normalised through
        /// `vike_secrets::normalized_venue_account_id`, the same function the CLI calls.
        venue_account_id: Option<String>,
        /// Permission to REPOINT a row that already names a different book (the CLI's
        /// `--replace`). Without it `vike_secrets::set_venue_account_id` refuses
        /// (`DbErrorKind::BookAlreadyKnown`): a disagreeing book is a FINDING, not a stale value.
        /// ⚠ Writing onto an EMPTY column needs none of this, deliberately;
        /// [`AccountVerb::required_confirm`] keys on this flag.
        replace: bool,
    },
}

impl AccountVerb {
    /// A short, stable word for the act — for a log line and a refusal. Total by construction.
    ///
    /// ⚠ **This is what a server may log, and `{self:?}` is not**: a redacting `Debug` is a
    /// promise, this method cannot carry a value at all.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            AccountVerb::List => "list",
            AccountVerb::Add { .. } => "add",
            AccountVerb::Rename { .. } => "rename",
            AccountVerb::SetActive { active: true, .. } => "activate",
            AccountVerb::SetActive { active: false, .. } => "deactivate",
            AccountVerb::Remove { .. } => "remove",
            AccountVerb::SetCredential { .. } => "set-credential",
            // A CLEAR and a WRITE are different acts (the CLI's `--clear` is its own flag).
            AccountVerb::SetBook { venue_account_id: Some(_), .. } => "set-book",
            AccountVerb::SetBook { venue_account_id: None, .. } => "clear-book",
        }
    }

    /// **What [`AccountRequest::confirm`] must equal**, or `None` for no ceremony. ONE derivation,
    /// used by the client that prompts and by the server that refuses.
    ///
    /// ⚠ A client may call this to know WHETHER to prompt, never to FILL the box.
    #[must_use]
    pub fn required_confirm(&self) -> Option<String> {
        match self {
            AccountVerb::Remove { id } => Some(id.to_string()),
            AccountVerb::SetCredential { key, .. } => Some(key.clone()),
            // Only when REPOINTING an already-known book (the act that moves order attribution);
            // filling an empty column is the ordinary act. Same split as the CLI's `--replace`.
            AccountVerb::SetBook { id, replace: true, .. } => Some(id.to_string()),
            AccountVerb::SetBook { replace: false, .. }
            | AccountVerb::List
            | AccountVerb::Add { .. }
            | AccountVerb::Rename { .. }
            | AccountVerb::SetActive { .. } => None,
        }
    }
}

/// ⚠ **HAND-WRITTEN: it must never print [`AccountVerb::SetCredential`]'s `value`** (0065 §4.1:
/// *"The variant prints the key NAME and the word `set`."*). On the REQUEST, which derives no
/// `Debug`, so no wrapper derive can reach the value.
impl std::fmt::Debug for AccountRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never a secret, but an operator-typed field on a credential frame: a presence mark only.
        let confirm = if self.confirm.is_some() { "typed" } else { "absent" };
        match &self.verb {
            AccountVerb::SetCredential { key, .. } => {
                write!(f, "AccountRequest(set-credential key={key} value=<set> confirm={confirm})")
            }
            AccountVerb::Add { venue, tier, label } => write!(
                f,
                "AccountRequest(add venue={venue} tier={tier} label={} confirm={confirm})",
                label.as_deref().unwrap_or("(none)")
            ),
            AccountVerb::Rename { id, label } => write!(
                f,
                "AccountRequest(rename id={id} label={} confirm={confirm})",
                label.as_deref().unwrap_or("(none)")
            ),
            AccountVerb::SetActive { id, active } => {
                write!(f, "AccountRequest(set-active id={id} active={active} confirm={confirm})")
            }
            AccountVerb::Remove { id } => {
                write!(f, "AccountRequest(remove id={id} confirm={confirm})")
            }
            // The book IS printed: the venue prints it on its own page; a row never holds anything
            // derived from a credential (`vike_mount::book_identity`'s module doc).
            AccountVerb::SetBook { id, venue_account_id, replace } => write!(
                f,
                "AccountRequest(set-book id={id} book={} replace={replace} confirm={confirm})",
                venue_account_id.as_deref().unwrap_or("(clear)")
            ),
            AccountVerb::List => write!(f, "AccountRequest(list confirm={confirm})"),
        }
    }
}

/// One `account` row as [`AccountVerb::List`] renders it — a mirror of `vike_secrets::Account`
/// plus the row's credential key NAMES. ⚠ **No value field**, like the reader it comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAccountRow {
    /// `account.id`, stable for the life of ONE database file. ⚠ Never write it down.
    pub id: i64,
    /// The row's venue id.
    pub venue: String,
    /// `paper` / `demo` / `live` (`vike_secrets::ACCOUNT_TIERS`). ⚠ CARRIED RAW, no serde rename:
    /// an older peer's `sim` is REFUSED BY NAME at the server, never silently mapped.
    pub tier: String,
    /// The operator's name for the ROLE. `None` is ordinary; a reader may not synthesise one.
    pub label: Option<String>,
    /// The BOOK as the venue names it. `None` = *not yet known*, never *no book*.
    pub venue_account_id: Option<String>,
    /// `false` = deactivated: the account never trades. `true` on a `demo`/`live` row ARMS that account at
    /// its own tier at the next restart (decision 0119).
    pub active: bool,
    /// When a venue's handshake last confirmed this row, RFC 3339; `None` = never verified.
    pub last_verified_at: Option<String>,
    /// The live credential key NAMES this row owns. Names, never values.
    pub keys: Vec<String>,
}

/// The [`crate::proto::Response::AccountList`] payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAccountList {
    /// The store the node read, as a display path (`vike-cli secrets account`'s `store:` line): a
    /// listing from the wrong box reads exactly like one from the right box.
    pub store: String,
    /// Every row, `id`-ordered, INACTIVE included (hiding them would look like removal).
    pub rows: Vec<WireAccountRow>,
}

/// The [`crate::proto::Response::Directory`] payload (`Request::Directory`, `Scope::Read`): the
/// venues this node can mount that its settings database names, and the ACTIVE accounts — the
/// Trade window's venue · account list (owner ruling 2026-09-30: the GUI reads them from the
/// database, through the node).
///
/// ⚠ **This IS the enumeration 0065 §5 keeps `Scope::Account`-only**, served to the observe key by
/// that ruling (venue, label, mode, no API keys), not because key names are left out: venue + tier
/// are enough to construct an unlabelled account's key names. What it never carries is a
/// credential value, a key name string, anything from the `credential` table (built from
/// `vike_secrets::read_venues_in` and `vike_secrets::resolve_accounts_in`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireDirectory {
    /// Every `venue` row on the node's roster (`vike_model::VENUES`), id-ordered; a row the roster
    /// dropped is not listed.
    ///
    /// ⚠ **An account's venue can be missing here**: the list may be EMPTY while accounts are
    /// listed (no `venue` table), and accounts are not roster-filtered. Fall back to the account's
    /// OWN venue key (and to the key when a row has no title).
    pub venues: Vec<WireDirectoryVenue>,
    /// Every ACTIVE `account` row, id-ordered (a deactivated row reads as deleted).
    pub accounts: Vec<WireDirectoryAccount>,
}

/// One venue: its key and its own spelling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirectoryVenue {
    /// The roster key: `binance`, `ctrader`.
    pub name: String,
    /// The venue's own spelling: `Binance`, `cTrader`. `None` on a store not yet carried; show
    /// `name` then.
    pub title: Option<String>,
}

/// One account: enough to list it and to say why it is greyed.
///
/// ⚠ **Whether the node RUNS an account is the snapshot's question (its venue blocks), never this
/// reply's.** An active row states the tier the account trades at (decision 0119), but missing keys,
/// a missing feature or two active tiers for one account still mount it paper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireDirectoryAccount {
    /// `account.id`. ⚠ Stable for the life of ONE database file; never write it down.
    pub id: i64,
    /// Its venue's key.
    pub venue: String,
    /// The operator's label. `None` is the venue's default account.
    pub label: Option<String>,
    /// `paper` / `demo` / `live`, carried raw like `WireAccountRow::tier`.
    pub tier: String,
    /// The broker's own number for the book (`U1234567`), shown for an unlabelled account when its
    /// venue has several. `None` until known.
    pub venue_account_id: Option<String>,
}

/// The [`crate::proto::Response::AccountWritten`] payload — what an accepted write DID.
/// ⚠ No credential value on any verb, [`AccountVerb::SetCredential`] included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAccountWritten {
    /// [`AccountVerb::word`] for the act performed.
    pub verb: String,
    /// The row the act names; `None` for [`AccountVerb::SetCredential`] (it names a key) and for a
    /// [`AccountVerb::Remove`] that left none.
    pub row: Option<WireAccountRow>,
    /// `false` when the store already held this state and nothing was written (idempotent).
    pub changed: bool,
    /// One sentence for the operator (*this arms NOTHING* on `add`, *a running daemon does not
    /// notice until it restarts* on a deactivate). **Never a value**: composed from ids, venues,
    /// tiers and key NAMES.
    pub note: Option<String>,
}
