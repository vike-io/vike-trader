//! What changed: the `Target` enum, its six `*Target` shapes, and the `Change` carrying one.

use serde::Serialize;

#[cfg(doc)]
use super::ChangeJournal;
use super::{
    Actor, KIND_ACCOUNT_BOOK, KIND_ACCOUNT_LIFECYCLE, KIND_BOOT_SETTINGS, KIND_CREDENTIAL_WRITE,
    KIND_SET_SETTING, KIND_VENUE_MOUNTED, MAX_BOOT_CELL_BYTES, MAX_BOOT_ENTRIES,
    MAX_CREDENTIAL_KEYS, MAX_IDENT_BYTES, Outcome, cap_bytes, clean_boot_cell, clean_field,
    clean_ident, clean_key_name, strip_control,
};

/// WHAT changed, one variant per `kind`.
///
/// Serialized `untagged`, so the object appears bare under `"target"` and the record's own `kind`
/// field is the discriminator — which is what lets a reader match on `kind` and skip the line
/// without parsing the rest of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Target {
    /// `kind = "set_setting"` — one key in one settings file.
    Setting(SettingTarget),
    /// `kind = "credential_write"` — key NAMES and a count, and structurally nothing else.
    Credential(CredentialTarget),
    /// `kind = "boot_settings"` — the effective ceilings at process start.
    BootSettings(BootSettingsTarget),
    /// `kind = "venue_mounted"` — the tier a venue was asked for, and the tier it reached.
    VenueMounted(VenueMountTarget),
    /// `kind = "account_book"` — an account ROW learned which book it is, in the settings database.
    AccountBook(AccountBookTarget),
    /// `kind = "account_lifecycle"` — an account ROW was created, renamed, (de)activated or removed.
    AccountLifecycle(AccountLifecycleTarget),
}

impl Target {
    /// The `kind` string for this target. Total by construction — a new variant that forgets a kind
    /// is a compile error, not a record that serializes with the wrong discriminator.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Setting(_) => KIND_SET_SETTING,
            Target::Credential(_) => KIND_CREDENTIAL_WRITE,
            Target::BootSettings(_) => KIND_BOOT_SETTINGS,
            Target::VenueMounted(_) => KIND_VENUE_MOUNTED,
            Target::AccountBook(_) => KIND_ACCOUNT_BOOK,
            Target::AccountLifecycle(_) => KIND_ACCOUNT_LIFECYCLE,
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
/// ⚠ **READ THE [`Outcome`] FIRST: on a row that did not apply, these cells describe the ATTEMPT,
/// not the file.** A refusing writer has no old value to report — it may have refused before
/// reading one ([`Outcome::Refused`] from a bad key or a held lock), or had the whole previous file
/// moved aside underneath it ([`Outcome::Stranded`]) — so both surfaces that journal a failure pass
/// `old: None` and `new` = the value that was being attempted. Taking absence as "the key was
/// previously unset" is therefore only sound on an applied row, and on a `Stranded` one it is
/// actively wrong: that outcome says the file this cell would describe is no longer where it was.
/// This is stated here, on the type, rather than fixed by making the cells optional, because the
/// absence has the SAME cause on every non-applied row and a per-outcome shape would invite a
/// reader to trust the cells on the rows that still carry them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingTarget {
    /// The settings SECTION the write AIMED AT (`"policy"`; in the file era this cell held the file
    /// name, `"policy.toml"`) — written on an applied row, merely named on a refused or stranded
    /// one.
    pub file: String,
    /// The full dotted key (`"policy.max_notional_per_order"`).
    pub key: String,
    /// The file's previous value, rendered as TOML. ABSENT when the key was not set in the file —
    /// ⚠ and also whenever the row's [`Outcome`] is not an applied one, where no previous value was
    /// established at all. See the type's own doc: branch on the outcome before reading this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// The value written, rendered as TOML — ⚠ or, on a row whose [`Outcome`] is not an applied
    /// one, the value that was being attempted and did NOT land.
    pub new: String,
}

/// A credential write — **key NAMES and a count, and there is no way to put a value in one.**
///
/// # Why the values are structurally impossible rather than merely omitted
///
/// This is the same discipline that makes `vike_config::Policy` unable to implement `EnvOverride`:
/// the property is enforced by there being no code path, not by every author remembering. Every
/// field below is PRIVATE and the only constructor is [`Change::credential_write`], **which accepts
/// no old/new/value parameter at all.** A future author who wants to log the value has to change
/// this type's shape and that constructor's signature — a diff a reviewer sees — rather than pass
/// one more argument at a call site nobody is reading.
/// `crates/vike-model/tests/change_journal_credential_values.rs` gates both halves.
///
/// # Why the NAMES are recordable, and why they are the useful part
///
/// A key name is not a secret. `vike-cli secrets list` prints names by an explicit decision in the
/// root `CLAUDE.md` ("shows key NAMES only, never values"), and the names are exactly what makes
/// *"when did I change the okx passphrase"* answerable — which is the question this channel exists
/// for. A record saying only "3 credentials changed" answers nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialTarget {
    /// The store file that was written, by NAME: `"vike.db"` on a box with a settings database,
    /// `"secrets.env"` on one without (`"node.env"` for the node key pair).
    store: String,
    /// The venue the keys belong to, or `"multi"` when a save spanned several.
    venue: String,
    /// The credential tier (`"SIM"` / `"DEMO"` / `"LIVE"`) —
    /// `vike_bridge_core::credentials::Environment`'s `as_str`.
    tier: String,
    /// The key NAMES written, capped at [`MAX_CREDENTIAL_KEYS`].
    keys: Vec<String>,
    /// How many keys were written — the TRUE total, which may exceed `keys.len()` when the cap bit.
    count: usize,
}

impl CredentialTarget {
    /// The recorded key names (possibly fewer than [`CredentialTarget::count`] — see that field).
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

    /// The credential tier.
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
pub struct BootSettingsTarget {
    /// Dotted key → effective value, in the order the caller supplied. `None` = not set (the
    /// compiled-in default applies), spelled as an absent map entry's twin: `null`, because unlike
    /// [`SettingTarget::old`] the KEY is always present here and only its value is missing.
    settings: Vec<(String, Option<String>)>,
}

impl BootSettingsTarget {
    /// The recorded (key, value) pairs.
    pub fn settings(&self) -> &[(String, Option<String>)] {
        &self.settings
    }
}

/// What a venue was ASKED to be at mount, and what it actually became.
///
/// The other three kinds record what an operator SAID. This one records what the program DID with
/// it, and the two differ routinely and legitimately: a ceiling of `live` still mounts paper when
/// no credentials are present, when the venue's build feature is absent, or when the venue declined
/// the key. Without this record the journal can prove the operator armed binance on the 14th and
/// cannot prove binance ever traded — which is the half an incident review actually needs, and the
/// exact question a per-venue switch invites ("I set live, why is it on paper?").
///
/// ⚠ **A record is written when the two AGREE, too.** An absence is not evidence: "no divergence
/// record for binance" and "binance never mounted at all" are the same silence, and somebody will
/// read the first out of it. Same argument as [`BootSettingsTarget`] — an absolute anchor beats an
/// inference over deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VenueMountTarget {
    /// The venue id, from `vike_model::VENUES`.
    venue: String,
    /// **WHICH ACCOUNT of that venue** — the label from
    /// [`crate::accounts::account_keys::AccountLabel`], or `None` for the account an unlabelled credential key
    /// addresses.
    ///
    /// ⚠ `None` is SKIPPED on the wire, and that is the whole reason it is an `Option` rather than a
    /// `String` carrying [`crate::accounts::account_keys::RESERVED_DEFAULT_LABEL`]: a box with one account per
    /// venue — which is every box that has not written an `[accounts]` table — writes the record it
    /// has always written, byte for byte, so nothing reading the ledger has to learn a new field to
    /// keep reading the old ones.
    ///
    /// It is the LABEL and not the route key deliberately: the route key is `venue#LABEL`, so a
    /// record carrying it would state the venue twice and a reader filtering on `venue` would have
    /// to parse it back apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    /// The tier asked for — the `policy.venues.<venue>` ceiling.
    requested: String,
    /// The tier the venue actually mounted at.
    effective: String,
    /// Why the two differ, when they do — `vike_config::ArmingBlock::as_str`'s rendering.
    ///
    /// Stored as a STRING rather than that enum, and not by preference: `vike-config` is layer 20
    /// and this crate is layer 10, so naming the type here would invert the direction
    /// `crates/vike-ops/tests/arch/layer_gate.rs` enforces. [`CredentialTarget::tier`] holds a
    /// `vike_bridge_core::credentials::Environment` the same way and for the same reason — the
    /// vocabulary lives with the code that DECIDES, and the journal records its rendering.
    block: Option<String>,
}

impl VenueMountTarget {
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
    pub fn requested(&self) -> &str {
        &self.requested
    }

    /// The tier actually reached.
    pub fn effective(&self) -> &str {
        &self.effective
    }

    /// Why the mount fell short, when it did.
    pub fn block(&self) -> Option<&str> {
        self.block.as_deref()
    }

    /// Whether the mount reached less than it was asked for.
    pub fn diverged(&self) -> bool {
        self.requested != self.effective
    }
}

/// **An account ROW learned which book it is** — the settings database's `account.venue_account_id`.
///
/// # Why this is in the ledger at all
///
/// A `venue_account_id` is the venue's own name for the BOOK an account trades. Getting it wrong
/// does not leak anything and does not fail loudly: it routes orders to another broker. The two
/// accounts that motivated the column are `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, which are
/// Dukascopy Bank SA and Dukascopy Europe IBS AS — two legal entities under one venue id — and
/// after `docs/superpowers/specs/2026-09-14-the-credential-schema.md`'s migration they are two rows
/// distinguishable only by `id`. *When was account 7 told it was 1234567, and what did it say
/// before* is therefore a question somebody eventually has to answer, and a delta channel is the
/// only thing that can.
///
/// # ⚠ Every cell here is an identifier, and NONE of them is a secret
///
/// The ids, the venue, the tier and the book are all things the venue prints on its own pages and
/// echoes on its own wire; the root `CLAUDE.md`'s rule is about credential VALUES, and this record
/// reaches none. There is deliberately no `value`, no key name and no store content of any kind —
/// the same structural property [`CredentialTarget`] holds, reached the same way: there is no
/// parameter for one on [`Change::account_book`].
///
/// [`AccountBookTarget::old`] is recorded BECAUSE the wrong-broker failure is the one this row
/// exists for: without the previous value a mistaken write is visible and not reversible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountBookTarget {
    /// The store that was written — `"vike.db"`. A file name rather than a path, the same cell
    /// shape (and the same reason) as [`CredentialTarget::store`].
    store: String,
    /// The `account.id` — the identity, and on a multi-account venue the ONLY thing that separates
    /// two rows. An integer rather than a string because it is one in the schema, and rendering it
    /// as text would invite a reader to match it against a label.
    account_id: i64,
    /// The row's venue id, from `vike_model::VENUES`.
    venue: String,
    /// The row's tier — `paper` / `demo` / `live`, the arming ceiling's vocabulary.
    tier: String,
    /// What the row named BEFORE this write. ABSENT when the book was not yet known, which is the
    /// state every migrated row starts in — so an absent `old` is *this row learned its book* and a
    /// present one is *this row was re-pointed*, which are very different events.
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<String>,
    /// The book the row names now — ABSENT when this write CLEARED the column back to *not yet
    /// known*.
    ///
    /// ⚠ A clear is a real and necessary event on this column, not an omission: a pair of rows
    /// written the wrong way round can only be repaired by clearing one of them first (ruling 11's
    /// one-account-per-book index refuses every direct correction), so a ledger that could not
    /// record one would go silent on the step that actually moves a book between two brokers.
    /// `old` present with `new` absent is exactly *this row stopped naming a book*.
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<String>,
}

impl AccountBookTarget {
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
    pub fn tier(&self) -> &str {
        &self.tier
    }

    /// The book the row named before — `None` when it did not name one. See the field's own doc:
    /// this absence distinguishes *learned* from *re-pointed*.
    ///
    /// ⚠ The ACCESSOR is `previous_book` while the serialized field stays `old`, matching
    /// [`SettingTarget`]'s wire shape — the two are deliberately allowed to differ. A reader parses
    /// `old`/`new`; a caller in Rust reads a name that says what the value is. Its sibling below is
    /// the reason the pair moved at all: `fn new(&self) -> &str` is `clippy::new_ret_no_self`, a
    /// `-D warnings` failure, because `new` is the constructor's name in every other type.
    pub fn previous_book(&self) -> Option<&str> {
        self.old.as_deref()
    }

    /// The book the row names now — `None` when this write CLEARED it. Serialized as `new`; see
    /// [`AccountBookTarget::previous_book`] for why the accessor is not called that.
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
/// `account` table, everything [`AccountBookTarget`] is not about.
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
/// exactly the reason [`CredentialTarget::keys`] records them — they are not secret
/// (`vike-cli secrets list` prints them by an explicit decision), and *"what did that account own
/// when it was removed"* is unanswerable without them. There is deliberately no `value` parameter
/// on [`Change::account_lifecycle`], which is the enforcement rather than a convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountLifecycleTarget {
    /// The store that was written — `"vike.db"`. A file name rather than a path, the same cell
    /// shape (and the same reason) as [`CredentialTarget::store`].
    store: String,
    /// The act: `create` / `rename` / `activate` / `deactivate` / `remove`. The caller's rendering
    /// of `vike_secrets::AccountEdit::verb`, which is total over that enum — this crate takes a
    /// string for the reason [`VenueMountTarget::block`] does, since `vike-secrets` sits beside
    /// this crate rather than under it.
    verb: String,
    /// The `account.id` the act names. ⚠ For a `create` this is the id the INSERT was given, so a
    /// reader may not assume a record of this kind describes a row that still exists — a later
    /// `remove` record for the same id is exactly the pair that says it does not.
    account_id: i64,
    /// The row's venue id, from `vike_model::VENUES`.
    venue: String,
    /// The row's tier — one of `vike_secrets::ACCOUNT_TIERS`.
    tier: String,
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
    /// values**, capped at [`MAX_CREDENTIAL_KEYS`] like [`CredentialTarget::keys`].
    ///
    /// ⚠ It is what makes a `deactivate` record answerable later: an account is deactivated
    /// BECAUSE of what it owns, and a row recording only the id would leave the reader of a
    /// six-month-old ledger unable to say which keys stopped being used. A `remove` record always
    /// carries an EMPTY list, structurally — the store refuses to delete a row that owns any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keys: Vec<String>,
}

impl AccountLifecycleTarget {
    /// The store file name.
    pub fn store(&self) -> &str {
        &self.store
    }

    /// The act, as the PRODUCER spelled it — `crates/vike-secrets/src/db/accounts/edit.rs`'s `AccountEdit`, whose
    /// match is the authority and renders `set-tier` among them.
    ///
    /// ⚠ **The vocabulary is deliberately not listed here.** It was, and the list was one act short
    /// within a day of a new one landing — this crate is below `vike-secrets` and cannot name the
    /// enum in a doc link, so a copy here is a copy nothing holds in step. That is the same reason
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
    pub fn tier(&self) -> &str {
        &self.tier
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

/// ONE change, ready to be journalled: the [`Target`] plus who and how it went.
///
/// The timestamp and the sequence number are NOT here — [`ChangeJournal::append`] stamps them, so a
/// caller cannot accidentally record a stale time and the sequence is always the write order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub(super) outcome: Outcome,
    pub(super) actor: Actor,
    pub(super) target: Target,
    pub(super) reason: Option<String>,
}

impl Change {
    /// A settings write, from `vike_config::write::SettingsWrite`'s cells.
    ///
    /// ⚠ It also builds the rows for writes that did NOT apply, which is why `outcome` is the first
    /// parameter rather than an afterthought: on a [`Outcome::Refused`] or [`Outcome::Stranded`]
    /// row `old`/`new` describe the ATTEMPT and not the file. [`SettingTarget`]'s own doc is the
    /// authority for how a reader is meant to take them.
    pub fn set_setting(
        outcome: Outcome,
        actor: Actor,
        file: &str,
        key: &str,
        old: Option<&str>,
        new: &str,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::Setting(SettingTarget {
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
    /// change [`CredentialTarget`]'s doc and
    /// `crates/vike-model/tests/change_journal_credential_values.rs` exist to make loud. `keys` are
    /// NAMES; each is reduced to `[A-Za-z0-9_]` (a credential key name is that alphabet by
    /// construction — `crate::credential_keys::credential_key` builds them) so that even a caller
    /// that passed the wrong thing cannot get punctuation, whitespace or a line terminator into the
    /// record.
    pub fn credential_write(
        outcome: Outcome,
        actor: Actor,
        store: &str,
        venue: &str,
        tier: &str,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::Credential(CredentialTarget {
                store: clean_field(store),
                venue: clean_ident(venue),
                tier: clean_ident(tier),
                keys: keys.iter().copied().take(MAX_CREDENTIAL_KEYS).map(clean_key_name).collect(),
                // The TRUE total, taken before the cap — so a capped record says how much it is not
                // showing instead of silently claiming the cap was the whole write.
                count: keys.len(),
            }),
            reason: None,
        }
    }

    /// The effective ceilings at process start.
    pub fn boot_settings(
        outcome: Outcome,
        actor: Actor,
        settings: &[(&str, Option<&str>)],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::BootSettings(BootSettingsTarget {
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

    /// A venue mount — the tier asked for, the tier reached, and the block between them.
    ///
    /// `block` is the caller's rendering of `vike_config::ArmingBlock` (see
    /// [`VenueMountTarget::block`] for why this crate takes a string rather than that enum). It is
    /// DROPPED when the tiers agree: a block recorded against a mount that reached what it was
    /// asked for would read as a refusal that never happened, which is worse than recording
    /// nothing.
    /// `account` is the ACCOUNT LABEL, `None` for the default account — see
    /// [`VenueMountTarget::account`] for why the default is an absence, and why that keeps a
    /// single-account box's records byte-identical.
    #[allow(clippy::too_many_arguments)]
    pub fn venue_mounted(
        outcome: Outcome,
        actor: Actor,
        venue: &str,
        account: Option<&str>,
        requested: &str,
        effective: &str,
        block: Option<&str>,
    ) -> Self {
        let requested = cap_bytes(&strip_control(requested), MAX_IDENT_BYTES);
        let effective = cap_bytes(&strip_control(effective), MAX_IDENT_BYTES);
        let diverged = requested != effective;
        Self {
            outcome,
            actor,
            target: Target::VenueMounted(VenueMountTarget {
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
    /// repair step a swapped pair needs. See [`AccountBookTarget`] for why this kind exists at all
    /// rather than riding [`Change::credential_write`], and why the absence of either is
    /// load-bearing.
    ///
    /// ⚠ **There is no `value` parameter and there never may be**: nothing this constructor accepts
    /// is a credential, and the only way to put one in a record of this kind is to change this
    /// signature — a diff a reviewer sees, which is the same discipline
    /// [`Change::credential_write`] holds.
    #[allow(clippy::too_many_arguments)]
    pub fn account_book(
        outcome: Outcome,
        actor: Actor,
        store: &str,
        account_id: i64,
        venue: &str,
        tier: &str,
        old: Option<&str>,
        new: Option<&str>,
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::AccountBook(AccountBookTarget {
                store: cap_bytes(&strip_control(store), MAX_IDENT_BYTES),
                account_id,
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                tier: cap_bytes(&strip_control(tier), MAX_IDENT_BYTES),
                old: old.map(|o| cap_bytes(&strip_control(o), MAX_IDENT_BYTES)),
                new: new.map(|n| cap_bytes(&strip_control(n), MAX_IDENT_BYTES)),
            }),
            reason: None,
        }
    }

    /// An account ROW created, renamed, (de)activated or removed — `vike_secrets::edit_account`'s
    /// side of the settings database, everything [`Change::account_book`] is not about.
    ///
    /// `keys` are credential key NAMES, reduced to `[A-Za-z0-9_]` by the same [`clean_key_name`]
    /// [`Change::credential_write`] applies, so a caller that passed the wrong thing still cannot
    /// get punctuation, whitespace or a line terminator into the record.
    ///
    /// ⚠ **There is no `value` parameter and there never may be**: nothing this constructor accepts
    /// is a credential, and the only way to put one in a record of this kind is to change this
    /// signature — a diff a reviewer sees, which is the same discipline
    /// [`Change::credential_write`] and [`Change::account_book`] both hold.
    #[allow(clippy::too_many_arguments)]
    pub fn account_lifecycle(
        outcome: Outcome,
        actor: Actor,
        store: &str,
        verb: &str,
        account_id: i64,
        venue: &str,
        tier: &str,
        old_label: Option<&str>,
        new_label: Option<&str>,
        active: bool,
        keys: &[&str],
    ) -> Self {
        Self {
            outcome,
            actor,
            target: Target::AccountLifecycle(AccountLifecycleTarget {
                store: cap_bytes(&strip_control(store), MAX_IDENT_BYTES),
                verb: cap_bytes(&strip_control(verb), MAX_IDENT_BYTES),
                account_id,
                venue: cap_bytes(&strip_control(venue), MAX_IDENT_BYTES),
                tier: cap_bytes(&strip_control(tier), MAX_IDENT_BYTES),
                old_label: old_label.map(|l| cap_bytes(&strip_control(l), MAX_IDENT_BYTES)),
                new_label: new_label.map(|l| cap_bytes(&strip_control(l), MAX_IDENT_BYTES)),
                active,
                keys: keys.iter().copied().take(MAX_CREDENTIAL_KEYS).map(clean_key_name).collect(),
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
    pub fn target(&self) -> &Target {
        &self.target
    }
}
