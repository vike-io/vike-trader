//! What changed: the `ChangeTarget` enum, its six `*ChangeTarget` shapes, and the `Change` carrying one.

use serde::Serialize;

use vike_model::AccountTier;

#[cfg(doc)]
use super::ChangeJournal;
use super::{
    ChangeActor, ChangeOutcome, KIND_ACCESS, KIND_ACCOUNT_BOOK, KIND_ACCOUNT_LIFECYCLE,
    KIND_BOOT_SETTINGS, KIND_CREDENTIAL_RETIRE, KIND_CREDENTIAL_WRITE, KIND_SET_SETTING,
    KIND_VENUE_MOUNTED, KIND_WIRE_COMMAND, MAX_BOOT_CELL_BYTES, MAX_BOOT_ENTRIES,
    MAX_CREDENTIAL_KEYS, MAX_IDENT_BYTES, cap_bytes, clean_boot_cell, clean_field, clean_ident,
    clean_key_name, strip_control,
};

/// WHAT changed, one variant per `kind`.
///
/// Serialized `untagged`, so the object appears bare under `"target"` and the record's own `kind`
/// field is the discriminator — which is what lets a reader match on `kind` and skip the line
/// without parsing the rest of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ChangeTarget {
    /// `kind = "set_setting"` — one key in one settings file.
    Setting(ChangeTargetSetting),
    /// `kind = "credential_write"` — key NAMES and a count, and structurally nothing else.
    Credential(ChangeTargetCredential),
    /// `kind = "credential_retire"` — a credential DELETED (its row retired, decision 0144): the
    /// same NAMES-and-a-count shape as a write, under its own kind.
    CredentialRetire(ChangeTargetCredential),
    /// `kind = "boot_settings"` — the effective ceilings at process start.
    BootSettings(ChangeTargetBootConfig),
    /// `kind = "venue_mounted"` — the tier a venue was asked for, and the tier it reached.
    VenueMounted(ChangeTargetVenueMount),
    /// `kind = "account_book"` — an account ROW learned which book it is, in the settings database.
    AccountBook(ChangeTargetBook),
    /// `kind = "account_lifecycle"` — an account ROW was created, renamed, (de)activated or removed.
    AccountLifecycle(ChangeTargetAccountLifecycle),
    /// `kind = "wire_command"` — a control command a wire peer sent was ACCEPTED (decision 0288).
    WireCommand(ChangeTargetWireCommand),
    /// `kind = "access"` — the access list changed: a grant, a role, a revoke, a registration, a swap.
    Access(ChangeTargetAccess),
}

impl ChangeTarget {
    /// The `kind` string for this target. Total by construction — a new variant that forgets a kind
    /// is a compile error, not a record that serializes with the wrong discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            ChangeTarget::Setting(_) => KIND_SET_SETTING,
            ChangeTarget::Credential(_) => KIND_CREDENTIAL_WRITE,
            ChangeTarget::CredentialRetire(_) => KIND_CREDENTIAL_RETIRE,
            ChangeTarget::BootSettings(_) => KIND_BOOT_SETTINGS,
            ChangeTarget::VenueMounted(_) => KIND_VENUE_MOUNTED,
            ChangeTarget::AccountBook(_) => KIND_ACCOUNT_BOOK,
            ChangeTarget::AccountLifecycle(_) => KIND_ACCOUNT_LIFECYCLE,
            ChangeTarget::WireCommand(_) => KIND_WIRE_COMMAND,
            ChangeTarget::Access(_) => KIND_ACCESS,
        }
    }
}

/// A settings write — taken verbatim from `vike_config::write::SettingsWrite`, whose own doc calls
/// itself "the audit record's raw material".
///
/// ⚠ **`old` ABSENT is not `old: null`.** On an APPLIED row an absent field means the key was not
/// in the file at all; a present empty string means the key was there and held nothing. The
/// distinction is `SettingsWrite`'s (`old_value: Option<String>`), it is the one `vike-tradehub`'s
/// `audit::sanitize_value` preserves, and collapsing it would make "the ceiling was unset" and "the
/// ceiling was blank" read identically.
///
/// ⚠ **READ THE [`ChangeOutcome`] FIRST: on a row that did not apply, these cells describe the ATTEMPT,
/// not the file.** A refusing writer has no old value to report — it may have refused before
/// reading one ([`ChangeOutcome::Refused`] from a bad key or a held lock), or had the whole previous file
/// moved aside underneath it ([`ChangeOutcome::Stranded`]) — so both surfaces that journal a failure pass
/// `old: None` and `new` = the value that was being attempted. Taking absence as "the key was
/// previously unset" is therefore only sound on an applied row, and on a `Stranded` one it is
/// actively wrong: that outcome says the file this cell would describe is no longer where it was.
/// This is stated here, on the type, rather than fixed by making the cells optional, because the
/// absence has the SAME cause on every non-applied row and a per-outcome shape would invite a
/// reader to trust the cells on the rows that still carry them.
///
/// ⚠ **A REMOVAL is a `set_setting` record too** (`vike-cli config unset`, decision 0161), and it
/// is told apart by its cells and its `reason`, not by a kind of its own: `old` is `<set>` and
/// `new` is `<unset>` — the redaction vocabulary, never a value, because the removed row's value is
/// deliberately not recorded — and the record's `reason` says it was a removal. A reader that takes
/// `new` for a value somebody wrote reads `<unset>` as a string; branch on the `reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetSetting {
    /// The settings SECTION the write AIMED AT (`"policy"`; in the file era this cell held the file
    /// name, `"policy.toml"`) — written on an applied row, merely named on a refused or stranded
    /// one.
    pub file: String,
    /// The full dotted key (`"policy.max_notional_per_order"`).
    pub key: String,
    /// The file's previous value, rendered as TOML. ABSENT when the key was not set in the file —
    /// ⚠ and also whenever the row's [`ChangeOutcome`] is not an applied one, where no previous value was
    /// established at all. See the type's own doc: branch on the outcome before reading this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// The value written, rendered as TOML — ⚠ or, on a row whose [`ChangeOutcome`] is not an applied
    /// one, the value that was being attempted and did NOT land.
    pub new: String,
}

/// A credential write — **key NAMES and a count, and there is no way to put a value in one.**
///
/// # Why the values are structurally impossible rather than merely omitted
///
/// This is the same discipline that makes `vike_config::Policy` unable to implement `EnvOverride`:
/// the property is enforced by there being no code path, not by every author remembering. Every
/// field below is PRIVATE and the only constructors are [`Change::credential_write`] and its
/// delete twin [`Change::credential_retire`], **neither of which accepts an old/new/value parameter
/// at all** (one private body builds the target for both). A future author who wants to log the
/// value has to change this type's shape and those constructors' signatures — a diff a reviewer
/// sees — rather than pass one more argument at a call site nobody is reading.
/// `crates/vike-change-journal/tests/change_journal_credential_values.rs` gates both halves.
///
/// # Why the NAMES are recordable, and why they are the useful part
///
/// A key name is not a secret. `vike-cli secrets ls` prints names by an explicit decision in the
/// root `CLAUDE.md` ("shows key NAMES only, never values"), and the names are exactly what makes
/// *"when did I change the okx passphrase"* answerable — which is the question this channel exists
/// for. A record saying only "3 credentials changed" answers nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetCredential {
    /// The store that was written, by FILE NAME: `"vike.db"`, the settings database.
    store: String,
    /// The venue the keys belong to, or `"multi"` when a save spanned several.
    venue: String,
    /// The tier of the ACCOUNT the keys belong to — its row's tier word
    /// (`vike_model::AccountTier::as_str`: `paper` / `demo` / `live`), or [`crate::TIER_UNTIERED`]
    /// for a write no account owns (a deployment row, a venue's application rows). No credential
    /// name carries a tier (decision 0156 item 6), so the cell is never read out of a key name.
    tier: String,
    /// The key NAMES written, capped at [`MAX_CREDENTIAL_KEYS`].
    keys: Vec<String>,
    /// How many keys were written — the TRUE total, which may exceed `keys.len()` when the cap bit.
    count: usize,
}

impl ChangeTargetCredential {
    /// The recorded key names (possibly fewer than [`ChangeTargetCredential::count`] — see that field).
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    /// How many keys the write actually touched.
    pub fn count(&self) -> usize {
        self.count
    }

    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The venue the keys belong to.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The account's tier word, or [`crate::TIER_UNTIERED`].
    pub fn tier(&self) -> &str {
        &self.tier
    }
}

/// The effective value of a small fixed set of ceiling keys at process start.
///
/// One record per process start, and it exists because the other two kinds record DELTAS: a journal
/// of deltas cannot answer "what was the ceiling on the 14th" without replaying every delta from
/// the beginning of time and hoping none is missing. A periodic absolute anchor turns that into a
/// lookup, and process start is the cadence that costs nothing and is guaranteed to bracket every
/// hand-edit of a settings file (a class no delta channel can ever see).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetBootConfig {
    /// Dotted key → effective value, in the order the caller supplied. `None` = not set (the
    /// compiled-in default applies), spelled as an absent map entry's twin: `null`, because unlike
    /// [`ChangeTargetSetting::old`] the KEY is always present here and only its value is missing.
    settings: Vec<(String, Option<String>)>,
}

impl ChangeTargetBootConfig {
    /// The recorded (key, value) pairs.
    pub fn settings(&self) -> &[(String, Option<String>)] {
        &self.settings
    }
}

/// What a venue was ASKED to be at mount, and what it actually became.
///
/// The other three kinds record what an operator SAID. This one records what the program DID with
/// it, and the two differ routinely and legitimately: an account at tier `live` does NOT RUN when no
/// credentials are present, when the venue's build feature is absent, or when the venue declined
/// the key (decision 0156 — never a paper engine in its place). Without this record the journal
/// can prove the operator armed binance on the 14th and cannot prove binance ever traded — which is
/// the half an incident review actually needs, and the exact question a per-venue switch invites
/// ("I set live, why does it not run?").
///
/// ⚠ **A record is written when the two AGREE, too.** An absence is not evidence: "no divergence
/// record for binance" and "binance never mounted at all" are the same silence, and somebody will
/// read the first out of it. Same argument as [`ChangeTargetBootConfig`] — an absolute anchor beats an
/// inference over deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetVenueMount {
    /// The venue id, from `vike_model::VENUES`.
    venue: String,
    /// **WHICH ACCOUNT of that venue** — the label from
    /// [`vike_model::accounts::account_keys::AccountLabel`], or `None` for the account an unlabelled credential key
    /// addresses.
    ///
    /// ⚠ `None` is SKIPPED on the wire, and that is the whole reason it is an `Option` rather than a
    /// `String` carrying [`vike_model::accounts::account_keys::RESERVED_DEFAULT_LABEL`]: a box with one account per
    /// venue — which is every box whose account table arms no labelled account — writes the record
    /// it has always written, byte for byte, so nothing reading the ledger has to learn a new field
    /// to keep reading the old ones.
    ///
    /// It is the LABEL and not the route key deliberately: the route key is `venue#LABEL`, so a
    /// record carrying it would state the venue twice and a reader filtering on `venue` would have
    /// to parse it back apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    /// The tier asked for — the account row's `tier`.
    requested: AccountTier,
    /// The tier the account actually runs at — `None` (the key ABSENT from the record) when it
    /// does not run at all (decision 0156), and then `block` says why.
    #[serde(skip_serializing_if = "Option::is_none")]
    effective: Option<AccountTier>,
    /// Why the two differ, when they do — `vike_model::ArmingBlock::as_str`'s rendering.
    ///
    /// Stored as a STRING rather than that enum, and not by preference: `vike-config` is layer 20
    /// and this crate is layer 15, so naming the type here would invert the direction
    /// `crates/vike-ops/tests/architecture/layer_gate.rs` enforces. [`ChangeTargetCredential::tier`] holds
    /// its tier as the WORD the writer rendered (`vike_model::AccountTier::as_str`, or
    /// [`crate::TIER_UNTIERED`]) for a kindred reason — the vocabulary lives with the code that
    /// DECIDES, and the journal records its rendering.
    block: Option<String>,
}

impl ChangeTargetVenueMount {
    /// The venue id.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// Which account of it — `None` for the default account. See the field's own doc for why the
    /// default account is an absence rather than a spelling.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    /// The tier the operator asked for.
    pub fn requested(&self) -> AccountTier {
        self.requested
    }

    /// The tier the account runs at; `None` when it does not run.
    pub fn effective(&self) -> Option<AccountTier> {
        self.effective
    }

    /// Why the mount fell short, when it did.
    pub fn block(&self) -> Option<&str> {
        self.block.as_deref()
    }

    /// Whether the account does not run at the tier it was asked for.
    pub fn diverged(&self) -> bool {
        Some(self.requested) != self.effective
    }
}

/// **An account ROW learned which book it is** — the settings database's `account.venue_account_id`.
///
/// # Why this is in the ledger at all
///
/// A `venue_account_id` is the venue's own name for the BOOK an account trades. Getting it wrong
/// does not leak anything and does not fail loudly: it routes orders to another broker. The two
/// accounts that motivated the column are dukascopy's two demo accounts, Dukascopy Bank SA and
/// Dukascopy Europe IBS AS — two legal entities under one venue id — which are two `account` rows
/// of one venue at one tier, told apart by their `id` and the label the operator gave each
/// (decision 0156 item 6). *When was account 7 told it was 1234567, and what did it say
/// before* is therefore a question somebody eventually has to answer, and a delta channel is the
/// only thing that can.
///
/// # ⚠ Every cell here is an identifier, and NONE of them is a secret
///
/// The ids, the venue, the tier and the book are all things the venue prints on its own pages and
/// echoes on its own wire; the root `CLAUDE.md`'s rule is about credential VALUES, and this record
/// reaches none. There is deliberately no `value`, no key name and no store content of any kind —
/// the same structural property [`ChangeTargetCredential`] holds, reached the same way: there is no
/// parameter for one on [`Change::account_book`].
///
/// [`ChangeTargetBook::old`] is recorded BECAUSE the wrong-broker failure is the one this row
/// exists for: without the previous value a mistaken write is visible and not reversible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetBook {
    /// The store that was written — `"vike.db"`. A file name rather than a path, the same cell
    /// shape (and the same reason) as [`ChangeTargetCredential::store`].
    store: String,
    /// The `account.id` — the identity, and on a multi-account venue the ONLY thing that separates
    /// two rows. An integer rather than a string because it is one in the schema, and rendering it
    /// as text would invite a reader to match it against a label.
    account_id: i64,
    /// The row's venue id, from `vike_model::VENUES`.
    venue: String,
    /// The row's tier, the tier it trades at while it is active.
    tier: AccountTier,
    /// What the row named BEFORE this write. ABSENT when the book was not yet known, which is the
    /// state every migrated row starts in — so an absent `old` is *this row learned its book* and a
    /// present one is *this row was re-pointed*, which are very different events.
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<String>,
    /// The book the row names now — ABSENT when this write CLEARED the column back to *not yet
    /// known*.
    ///
    /// ⚠ A clear is a real event on this column, not an omission: it takes a wrong book off a row
    /// without naming another, so a ledger that could not record one would go silent on a step that
    /// changes which broker a row routes to. `old` present with `new` absent is exactly *this row
    /// stopped naming a book*.
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<String>,
}

impl ChangeTargetBook {
    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The `account.id` that was written.
    pub fn account_id(&self) -> i64 {
        self.account_id
    }

    /// The row's venue.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The row's tier.
    pub fn tier(&self) -> AccountTier {
        self.tier
    }

    /// The book the row named before — `None` when it did not name one. See the field's own doc:
    /// this absence distinguishes *learned* from *re-pointed*.
    ///
    /// ⚠ The ACCESSOR is `previous_book` while the serialized field stays `old`, matching
    /// [`ChangeTargetSetting`]'s wire shape — the two are deliberately allowed to differ. A reader parses
    /// `old`/`new`; a caller in Rust reads a name that says what the value is. Its sibling below is
    /// the reason the pair moved at all: `fn new(&self) -> &str` is `clippy::new_ret_no_self`, a
    /// `-D warnings` failure, because `new` is the constructor's name in every other type.
    pub fn previous_book(&self) -> Option<&str> {
        self.old.as_deref()
    }

    /// The book the row names now — `None` when this write CLEARED it. Serialized as `new`; see
    /// [`ChangeTargetBook::previous_book`] for why the accessor is not called that.
    pub fn book(&self) -> Option<&str> {
        self.new.as_deref()
    }

    /// Whether this write RE-POINTED an account that already named a book, rather than teaching one
    /// that named none. The question a reviewer of this ledger asks first.
    pub fn repointed(&self) -> bool {
        self.old.is_some()
    }

    /// Whether this write took a book AWAY rather than assigning one — `old` present, `new` absent.
    ///
    /// The repair step: clearing one row is how a swapped pair is corrected, so a run of records
    /// reading *cleared, assigned, assigned* is a repair rather than three unrelated writes.
    pub fn cleared(&self) -> bool {
        self.new.is_none()
    }
}

/// **An account ROW was created, renamed, deactivated or removed** — the settings database's
/// `account` table, everything [`ChangeTargetBook`] is not about.
///
/// # Why this is a kind of its own rather than a second shape of `account_book`
///
/// The same argument [`KIND_ACCOUNT_BOOK`] makes against riding [`KIND_CREDENTIAL_WRITE`], one rung
/// along: what changes here is not a book, and a ledger line that said `account_book` for a REMOVE
/// would make *"when was account 7 re-pointed"* answer with a row in which it was not. The two
/// kinds also answer different questions — that one is *which broker*, this one is *which rows
/// exist* — and the second is the one that has to be answerable after somebody deletes something.
///
/// # ⚠ Every cell here is an identifier, and NONE of them is a secret
///
/// Ids, the venue, the tier, labels and credential key NAMES. The key names are recorded for
/// exactly the reason [`ChangeTargetCredential::keys`] records them — they are not secret
/// (`vike-cli secrets ls` prints them by an explicit decision), and *"what did that account own
/// when it was removed"* is unanswerable without them. There is deliberately no `value` parameter
/// on [`Change::account_lifecycle`], which is the enforcement rather than a convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetAccountLifecycle {
    /// The store that was written — `"vike.db"`. A file name rather than a path, the same cell
    /// shape (and the same reason) as [`ChangeTargetCredential::store`].
    store: String,
    /// The act: `create` / `rename` / `activate` / `deactivate` / `remove`. The caller's rendering
    /// of `vike_secrets::AccountEdit::verb`, which is total over that enum — a string, as
    /// [`ChangeTargetVenueMount::block`] is. ⚠ The reason given here was that `vike-secrets` sat beside
    /// this module's crate rather than under it; the journal lived IN `vike-secrets` for a while and
    /// now sits BELOW it, in its own crate, which cannot name that enum either. The cell stays a
    /// string because it is the record's wire format.
    verb: String,
    /// The `account.id` the act names. ⚠ For a `create` this is the id the INSERT was given, so a
    /// reader may not assume a record of this kind describes a row that still exists — a later
    /// `remove` record for the same id is exactly the pair that says it does not.
    account_id: i64,
    /// The row's venue id, from `vike_model::VENUES`.
    venue: String,
    /// The row's tier.
    tier: AccountTier,
    /// The label the row carried BEFORE — ABSENT when it carried none, which is the state every
    /// migrated row is in and the state a `create` starts from.
    #[serde(skip_serializing_if = "Option::is_none")]
    old_label: Option<String>,
    /// The label the row carries now — ABSENT when it carries none, and absent on a `remove`,
    /// which leaves no row to carry one.
    #[serde(skip_serializing_if = "Option::is_none")]
    new_label: Option<String>,
    /// Whether the row is ACTIVE after the act. `false` on a `deactivate`; absent-as-`false` is not
    /// available here because *the row is off* is the whole content of that record.
    active: bool,
    /// The live credential key NAMES the row owned at the moment of the act — **names, never
    /// values**, capped at [`MAX_CREDENTIAL_KEYS`] like [`ChangeTargetCredential::keys`].
    ///
    /// ⚠ It is what makes a `deactivate` record answerable later: an account is deactivated
    /// BECAUSE of what it owns, and a row recording only the id would leave the reader of a
    /// six-month-old ledger unable to say which keys stopped being used. A `remove` record always
    /// carries an EMPTY list, structurally — the store refuses to delete a row that owns any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keys: Vec<String>,
}

impl ChangeTargetAccountLifecycle {
    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The act, as the PRODUCER spelled it — `crates/vike-secrets/src/db/accounts/edit.rs`'s `AccountEdit`, whose
    /// match is the authority and renders `set-tier` among them.
    ///
    /// ⚠ **The vocabulary is deliberately not listed here.** It was, and the list was one act short
    /// within a day of a new one landing — this module then lived in `vike-model`, below
    /// `vike-secrets`, and could not name the enum in a doc link, so a copy here was a copy
    /// nothing held in step. That is the same reason
    /// the producer's own doc gives for not restating it either.
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// The `account.id` the act named. See the field's own doc for why this is not a claim that the
    /// row still exists.
    pub fn account_id(&self) -> i64 {
        self.account_id
    }

    /// The row's venue.
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The row's tier.
    pub fn tier(&self) -> AccountTier {
        self.tier
    }

    /// The label the row carried before — `None` when it carried none.
    pub fn previous_label(&self) -> Option<&str> {
        self.old_label.as_deref()
    }

    /// The label the row carries now — `None` when it carries none, or when there is no row.
    pub fn label(&self) -> Option<&str> {
        self.new_label.as_deref()
    }

    /// Whether the row is active after the act.
    pub fn active(&self) -> bool {
        self.active
    }

    /// The credential key NAMES the row owned — names, never values.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
}

/// **A control command a wire peer sent, accepted** (decision 0288 item 5): the verb and the order client id it
/// named, under the [`ChangeActor::Wire`] that sent it. One record per ACCEPTED command: strategies run inside the
/// node and never cross the wire, so this is human pace. A refused command leaves no record, the node's uniform rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetWireCommand {
    /// The command's verb (`submit`, `cancel`, `flatten`, …): the node's `command_kind` word.
    verb: String,
    /// The order client id it named; ABSENT for an account-wide verb, which names none.
    #[serde(skip_serializing_if = "String::is_empty")]
    order_client_id: String,
}

impl ChangeTargetWireCommand {
    /// The command's verb.
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// The order client id it named; empty for an account-wide verb.
    pub fn order_client_id(&self) -> &str {
        &self.order_client_id
    }
}

/// **The access list changed** (decision 0288 item 5): one `access_key` row granted, re-roled, revoked or registered
/// by an invitation, or an invitation minted or its ssh line swapped. Every cell is an identifier: the list holds
/// PUBLIC keys only, and this record carries none of their bytes, nor an invitation's secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeTargetAccess {
    /// `add`, `role`, `revoke`, `register`, `invite` or `swap`.
    verb: String,
    /// The person.
    person: String,
    /// The row's device; ABSENT for an invitation, which names no device yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    /// The role AFTER the act (`observe`, `control`, `admin`); ABSENT for a swap, which changes no role.
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    /// The row's device key id (`nk-…`); ABSENT for an invitation, which holds no key yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<String>,
}

impl ChangeTargetAccess {
    /// The act.
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// The person.
    pub fn person(&self) -> &str {
        &self.person
    }

    /// The row's device, when the act names one.
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// The role after the act, when the act sets or keeps one.
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    /// The row's device key id, when the act names one.
    pub fn key_id(&self) -> Option<&str> {
        self.key_id.as_deref()
    }
}

/// ONE change, ready to be journalled: the [`ChangeTarget`] plus who and how it went.
///
/// The timestamp and the sequence number are NOT here — [`ChangeJournal::append`] stamps them, so a
/// caller cannot accidentally record a stale time and the sequence is always the write order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub(super) outcome: ChangeOutcome,
    pub(super) actor: ChangeActor,
    pub(super) target: ChangeTarget,
    pub(super) reason: Option<String>,
}

impl Change {
    /// A settings write, from `vike_config::write::SettingsWrite`'s cells.
    ///
    /// ⚠ It also builds the rows for writes that did NOT apply, which is why `outcome` is the first
    /// parameter rather than an afterthought: on a [`ChangeOutcome::Refused`] or [`ChangeOutcome::Stranded`]
    /// row `old`/`new` describe the ATTEMPT and not the file. [`ChangeTargetSetting`]'s own doc is the
    /// authority for how a reader is meant to take them.
    pub fn set_setting(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        file: &str,
        key: &str,
        old: Option<&str>,
        new: &str,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::Setting(ChangeTargetSetting {
                file: clean_field(file),
                key: clean_field(key),
                old: old.map(clean_field),
                new: clean_field(new),
            }),
            reason: None,
        }
    }

    /// A credential write — **note what this signature does NOT take.**
    ///
    /// There is no `old`, no `new`, no `value` and no `values` parameter, and adding one is the
    /// change [`ChangeTargetCredential`]'s doc and
    /// `crates/vike-change-journal/tests/change_journal_credential_values.rs` exist to make loud.
    /// `keys` are NAMES; each is reduced to `[A-Za-z0-9_#.]` (the alphabet of a deployment name, of
    /// every legacy key name and of the store's own `{venue}#{id}.{FIELD}` / `{venue}.{FIELD}` row
    /// names, which survive intact) so that even a caller that passed the wrong thing cannot get
    /// whitespace, `=`, other punctuation or a line terminator into the record. `tier` is the
    /// ACCOUNT's tier word (`vike_model::AccountTier::as_str`) or [`crate::TIER_UNTIERED`].
    pub fn credential_write(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        store: &str,
        venue: &str,
        tier: &str,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::Credential(credential_target(store, venue, tier, keys)),
            reason: None,
        }
    }

    /// A credential DELETE — a key's row RETIRED (decision 0144). **The same signature discipline
    /// as [`Change::credential_write`]**: key NAMES only, no old/new/value parameter, the same
    /// `[A-Za-z0-9_#.]` reduction and the same cap — so a delete can no more journal a value than a
    /// write can. Its own kind, [`super::KIND_CREDENTIAL_RETIRE`], so a reader counting writes
    /// never counts a removal.
    pub fn credential_retire(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        store: &str,
        venue: &str,
        tier: &str,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::CredentialRetire(credential_target(store, venue, tier, keys)),
            reason: None,
        }
    }

    /// The effective ceilings at process start. The actor is [`ChangeActor::Boot`] by construction: a
    /// boot anchor is the process recording what it runs with, never a channel's change, and taking
    /// no actor is what keeps this kind's maximal record inside one page
    /// (`record_shapes_at_their_capabilities_fit_a_page`; [`super::MAX_BOOT_ENTRIES`] is measured at it).
    pub fn boot_settings(outcome: ChangeOutcome, settings: &[(&str, Option<&str>)]) -> Self {
        Self {
            outcome,
            actor: ChangeActor::Boot,
            target: ChangeTarget::BootSettings(ChangeTargetBootConfig {
                settings: settings
                    .iter()
                    .take(MAX_BOOT_ENTRIES)
                    .map(|(k, v)| {
                        (cap_bytes(&strip_control(k), MAX_BOOT_CELL_BYTES), v.map(clean_boot_cell))
                    })
                    .collect(),
            }),
            reason: None,
        }
    }

    /// A venue mount — the tier asked for, the tier it runs at (`None`: it does not run), and the
    /// block between them.
    ///
    /// `block` is the caller's rendering of `vike_model::ArmingBlock` (see
    /// [`ChangeTargetVenueMount::block`] for why this crate takes a string rather than that enum). It is
    /// DROPPED when the tiers agree: a block recorded against a mount that reached what it was
    /// asked for would read as a refusal that never happened, which is worse than recording
    /// nothing.
    /// `account` is the ACCOUNT LABEL, `None` for the default account — see
    /// [`ChangeTargetVenueMount::account`] for why the default is an absence, and why that keeps a
    /// single-account box's records byte-identical.
    pub fn venue_mounted(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        venue: &str,
        account: Option<&str>,
        requested: AccountTier,
        effective: Option<AccountTier>,
        block: Option<&str>,
    ) -> Self {
        let diverged = Some(requested) != effective;
        Self {
            outcome,
            actor,
            target: ChangeTarget::VenueMounted(ChangeTargetVenueMount {
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                account: account.map(|a| cap_bytes(&strip_control(a), MAX_IDENT_BYTES)),
                requested,
                effective,
                block: block
                    .filter(|_| diverged)
                    .map(|b| cap_bytes(&strip_control(b), MAX_IDENT_BYTES)),
            }),
            reason: None,
        }
    }
    /// An account row learning which BOOK it is — `account.venue_account_id`, in the settings
    /// database.
    ///
    /// `old` is the book the row named before, `None` when it named none (every migrated row starts
    /// there). `new` is the book it names now, `None` when this write CLEARED the column — the
    /// repair step a swapped pair needs. See [`ChangeTargetBook`] for why this kind exists at all
    /// rather than riding [`Change::credential_write`], and why the absence of either is
    /// load-bearing.
    ///
    /// ⚠ **There is no `value` parameter and there never may be**: nothing this constructor accepts
    /// is a credential, and the only way to put one in a record of this kind is to change this
    /// signature — a diff a reviewer sees, which is the same discipline
    /// [`Change::credential_write`] holds.
    pub fn account_book(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        store: &str,
        account_id: i64,
        venue: &str,
        tier: AccountTier,
        old: Option<&str>,
        new: Option<&str>,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::AccountBook(ChangeTargetBook {
                store: cap_bytes(&strip_control(store), MAX_IDENT_BYTES),
                account_id,
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                tier,
                old: old.map(|o| cap_bytes(&strip_control(o), MAX_IDENT_BYTES)),
                new: new.map(|n| cap_bytes(&strip_control(n), MAX_IDENT_BYTES)),
            }),
            reason: None,
        }
    }

    /// An account ROW created, renamed, (de)activated or removed — `vike_secrets::edit_account`'s
    /// side of the settings database, everything [`Change::account_book`] is not about.
    ///
    /// `keys` are credential key NAMES, reduced to `[A-Za-z0-9_#.]` by the same [`clean_key_name`]
    /// [`Change::credential_write`] applies, so a caller that passed the wrong thing still cannot
    /// get whitespace, `=`, other punctuation or a line terminator into the record.
    ///
    /// ⚠ **There is no `value` parameter and there never may be**: nothing this constructor accepts
    /// is a credential, and the only way to put one in a record of this kind is to change this
    /// signature — a diff a reviewer sees, which is the same discipline
    /// [`Change::credential_write`] and [`Change::account_book`] both hold.
    #[expect(clippy::too_many_arguments)]
    pub fn account_lifecycle(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        store: &str,
        verb: &str,
        account_id: i64,
        venue: &str,
        tier: AccountTier,
        old_label: Option<&str>,
        new_label: Option<&str>,
        active: bool,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::AccountLifecycle(ChangeTargetAccountLifecycle {
                store: cap_bytes(&strip_control(store), MAX_IDENT_BYTES),
                verb: cap_bytes(&strip_control(verb), MAX_IDENT_BYTES),
                account_id,
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                tier,
                old_label: old_label.map(|l| cap_bytes(&strip_control(l), MAX_IDENT_BYTES)),
                new_label: new_label.map(|l| cap_bytes(&strip_control(l), MAX_IDENT_BYTES)),
                active,
                keys: keys.iter().copied().take(MAX_CREDENTIAL_KEYS).map(clean_key_name).collect(),
            }),
            reason: None,
        }
    }

    /// A wire peer's ACCEPTED control command: its verb and the order client id it named (empty for an
    /// account-wide verb). See [`ChangeTargetWireCommand`].
    pub fn wire_command(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        verb: &str,
        order_client_id: &str,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::WireCommand(ChangeTargetWireCommand {
                verb: clean_ident(verb),
                order_client_id: clean_ident(order_client_id),
            }),
            reason: None,
        }
    }

    /// The access list changed: `verb` (`add`, `role`, `revoke`, `register`, `invite`, `swap`) for `person`, with
    /// the row's device, its role after the act and its key id where the act names them. See [`ChangeTargetAccess`].
    pub fn access(
        outcome: ChangeOutcome,
        actor: ChangeActor,
        verb: &str,
        person: &str,
        device: Option<&str>,
        role: Option<&str>,
        key_id: Option<&str>,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: ChangeTarget::Access(ChangeTargetAccess {
                verb: clean_ident(verb),
                person: clean_ident(person),
                device: device.map(clean_ident),
                role: role.map(clean_ident),
                key_id: key_id.map(clean_ident),
            }),
            reason: None,
        }
    }

    /// Attach the operator's rationale. `None`, or text that sanitizes to nothing, records no
    /// `reason` field at all — `vike-tradehub`'s `audit::sanitize_reason` idiom, so a change with
    /// no rationale does not carry a blank one that reads like a supplied-but-empty explanation.
    #[must_use]
    pub fn with_reason(mut self, reason: Option<&str>) -> Self {
        self.reason = reason.map(clean_field).filter(|s| !s.trim().is_empty());
        self
    }

    /// What kind of change this is.
    pub fn kind(&self) -> &'static str {
        self.target.kind()
    }

    /// The target, for a caller that wants to inspect what it built.
    pub fn target(&self) -> &ChangeTarget {
        &self.target
    }
}

/// The ONE body behind [`Change::credential_write`] and [`Change::credential_retire`]: key NAMES
/// reduced to `[A-Za-z0-9_#.]` and capped, and no parameter a value could travel through.
fn credential_target(
    store: &str,
    venue: &str,
    tier: &str,
    keys: &[&str],
) -> ChangeTargetCredential {
    ChangeTargetCredential {
        store: clean_field(store),
        venue: clean_ident(venue),
        tier: clean_ident(tier),
        keys: keys.iter().copied().take(MAX_CREDENTIAL_KEYS).map(clean_key_name).collect(),
        // The TRUE total, taken before the cap — so a capped record says how much it is not
        // showing instead of silently claiming the cap was the whole write.
        count: keys.len(),
    }
}
