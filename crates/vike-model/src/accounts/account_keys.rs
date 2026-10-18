//! `account_keys` — **which account of a venue**: the operator's LABEL for an `account` row, and
//! the ROUTING key that addresses one account's engine.
//!
//! # A label names an account ROW, never a credential name
//!
//! An operator may hold several accounts of one venue — demo and live, or two books at one tier —
//! and each is an `account` row of the settings database with its own `tier`, `active` flag and
//! optional `label`. Its credentials belong to the ROW (`credential.account_id`, decision 0156 item
//! 6): no credential name carries the label, the tier or the venue's own word for either, and no
//! reader parses one out of a name. This module is the label's grammar ([`AccountLabel`]) and the
//! routing key's ([`route_key_of`]), nothing more.
//!
//! # What the routing key does — and why the addressing seam is NOT reinvented here
//!
//! One event-driven engine keys each execution/data client on an ARBITRARY operator-chosen string
//! rather than on the venue name, so two clients for one venue with different credentials coexist
//! by taking different keys. That is exactly the shape
//! `crates/vike-exec/src/execution_engine/mod.rs`'s `route_key` already has, down to its warning
//! against decorating the `venue` field instead (the `"binance#2"` trap). So this module adds NO
//! second addressing scheme: [`AccountRef::route_key`] RENDERS the existing one. For
//! [`AccountLabel::Default`] it is the bare venue id — what `vike_exec::ExecutionEngine::new` already
//! seeds `route_key` with — so a single-account box is addressed exactly as before.
//!
//! A label is `[A-Z0-9]+` ([`AccountLabel::parse`]): it becomes a component of a lock FILENAME and
//! of a routing key, so it is charset-restricted and bounded.

#[cfg(doc)]
use crate::venues::VENUES;

/// The longest label accepted. A label becomes a component of
/// `vike_boot::live_lock::LiveLock`'s `LIVE-<route_key>.lock` filename, so it is bounded for the
/// same reason it is charset-restricted: a name that cannot be written to disk is a mount that
/// fails at the last possible moment.
pub const MAX_LABEL_LEN: usize = 24;

/// The label spelling RESERVED for the unlabelled account, refused as a label of its own.
///
/// An operator writing `DEFAULT` means the account they already have — the venue's account row with
/// no label — so accepting it would create a second, silent name for one account. Refused by name
/// instead.
pub const RESERVED_DEFAULT_LABEL: &str = "DEFAULT";

/// **Which account of a venue** — the unlabelled one, or an operator-named one.
///
/// `Default` is not "no account": it is THE account a single-account box has, and every path here
/// renders it exactly as today's spellings render it.
///
/// # Serde goes THROUGH [`AccountLabel::parse`], and that is the point
///
/// A mount now names its account (`vike_exec::MountSpec::account`, which crosses the tradehub wire
/// and is persisted in `vike_core::mount_topology`'s `runtime_mounts.json`), so a label reaches
/// this type from a FILE and from a SOCKET as well as from a `parse` call. Deriving the enum's
/// serde would have let `"DEFAULT"`, `"alt"`, an underscore and a 40-character name across both,
/// each of which is a refusal [`AccountLabel::parse`] already makes — and a check somebody has to
/// remember at every deserialization site is a check that will be missed at one of them.
///
/// So the impls below are hand-written over a `String`: DESERIALIZING is `parse`, verbatim, and
/// SERIALIZING is the label text. `AccountLabel::Default` has no text by design (see
/// [`AccountLabel::text`]), so it is not representable on the wire at all — which is correct rather
/// than a gap: every carrier of this type spells the default account as an ABSENT field
/// (`Option<AccountLabel>` with `skip_serializing_if = "Option::is_none"`), so an account-less
/// mount emits no key and a pre-change file still parses. A `Serialize` call on `Default` is
/// therefore an error naming [`RESERVED_DEFAULT_LABEL`], not a silently-emitted string that would
/// fail to round-trip.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccountLabel {
    /// The venue's account row with no label — the account a single-account box has.
    #[default]
    Default,
    /// An operator-named account — an `account` row whose `label` is `ALT`. Always
    /// [`AccountLabel::parse`]-validated, so the inner string is `[A-Z0-9]` of at most
    /// [`MAX_LABEL_LEN`] characters and is never [`RESERVED_DEFAULT_LABEL`].
    Named(String),
}

/// Why a would-be account label was refused. One variant per distinct operator mistake, for
/// the reason `crates/vike-config/src/venue_arming.rs`'s `ArmingBlock` has one per distinct cause:
/// each has a different fix, and a single `Invalid(String)` would make the caller re-derive which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKeyError {
    /// An empty label.
    EmptyLabel,
    /// A byte outside `[A-Z0-9]`. Carries the offender so the message can name it.
    LabelChar(char),
    /// Longer than [`MAX_LABEL_LEN`]; carries the length that was offered.
    LabelTooLong(usize),
    /// Exactly [`RESERVED_DEFAULT_LABEL`] — see that constant.
    ReservedLabel,
}

impl std::fmt::Display for AccountKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountKeyError::EmptyLabel => write!(
                f,
                "an empty account label — the label is the part that names WHICH account, so an \
                 empty one names none"
            ),
            AccountKeyError::LabelChar(c) => {
                write!(f, "an account label is A-Z and 0-9 only, and {c:?} is neither")
            }
            AccountKeyError::LabelTooLong(n) => write!(
                f,
                "an account label is at most {MAX_LABEL_LEN} characters and this one is {n} — the \
                 label becomes part of a lock FILENAME"
            ),
            AccountKeyError::ReservedLabel => write!(
                f,
                "`{RESERVED_DEFAULT_LABEL}` is reserved for the venue's account row with no \
                 label, so naming it here would be a second name for one account"
            ),
        }
    }
}

impl AccountLabel {
    /// Validate an operator-supplied label. See [`AccountKeyError`] for every refusal.
    ///
    /// ⚠ Case-SENSITIVE, and deliberately not repaired: `alt` is refused rather than uppercased,
    /// for the reason `crates/vike-model/src/venues/mod.rs`'s `roster_id` refuses `Bybit` — a
    /// spelling the loader fixes on the operator's behalf is a spelling nobody learns, and the
    /// labelled key in their credential store would then not match the label in their policy file.
    ///
    /// # Errors
    /// [`AccountKeyError`] naming the first thing wrong with `text`.
    pub fn parse(text: &str) -> Result<AccountLabel, AccountKeyError> {
        if text.is_empty() {
            return Err(AccountKeyError::EmptyLabel);
        }
        if text.len() > MAX_LABEL_LEN {
            return Err(AccountKeyError::LabelTooLong(text.len()));
        }
        if let Some(c) = text.chars().find(|c| !c.is_ascii_uppercase() && !c.is_ascii_digit()) {
            return Err(AccountKeyError::LabelChar(c));
        }
        if text == RESERVED_DEFAULT_LABEL {
            return Err(AccountKeyError::ReservedLabel);
        }
        Ok(AccountLabel::Named(text.to_string()))
    }

    /// Is this the account an unlabelled key addresses?
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, AccountLabel::Default)
    }

    /// The label text, or `None` for [`AccountLabel::Default`] — which has no text BY DESIGN: the
    /// default account's key carries no label, so rendering one would be inventing a spelling that
    /// appears in no file.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            AccountLabel::Default => None,
            AccountLabel::Named(l) => Some(l),
        }
    }
}

/// **The WIRE's spelling of an account, which admits one word [`AccountLabel::parse`] refuses.**
///
/// Accepts [`RESERVED_DEFAULT_LABEL`] and answers [`AccountLabel::Default`]; delegates
/// everything else to [`AccountLabel::parse`] verbatim, so every other rule — the charset, the
/// length bound, the case-sensitivity — has exactly one implementation and cannot drift.
///
/// # ⚠ The asymmetry is the point, and it is two different questions
///
/// `AccountLabel::parse` refuses the reserved word because as an account LABEL it would be *"a
/// second name for one account"* — the default account already has a home, the venue's account row
/// with no label. **On the wire there is no second home**: a routing DESTINATION for the default
/// account has exactly one spelling, and this is it.
///
/// Without this function the original account becomes UNADDRESSABLE over the wire the moment an
/// operator adds a second one to that venue — a hole the absent-means-default convention cannot
/// close, because ABSENCE is exactly what cannot be trusted across a version boundary: an old
/// node drops a field it does not know, and the frame that arrives is byte-identical to one
/// that named no account at all.
///
/// So the wire has three states, and `Option<AccountLabel>` is how a caller reads them:
///
/// | on the wire | this function | means |
/// |---|---|---|
/// | field absent | not called | the sender named NO account |
/// | `"DEFAULT"` | `Ok(AccountLabel::Default)` | the sender named the unlabelled account |
/// | `"ALT"` | `Ok(AccountLabel::Named("ALT"))` | the sender named account `ALT` |
///
/// ⚠ **Nothing about [`AccountLabel`]'s own serde changes**, so an account label keeps refusing
/// the reserved spelling exactly as it does today. This is a wire PARSER, not a second
/// `Deserialize`.
///
/// # Errors
/// [`AccountKeyError`] naming the first thing wrong with `text` — the same set
/// [`AccountLabel::parse`] answers with, minus [`AccountKeyError::ReservedLabel`], which this
/// spelling cannot produce.
pub fn parse_wire_account(text: &str) -> Result<AccountLabel, AccountKeyError> {
    if text == RESERVED_DEFAULT_LABEL {
        return Ok(AccountLabel::Default);
    }
    AccountLabel::parse(text)
}
impl std::fmt::Display for AccountLabel {
    /// [`RESERVED_DEFAULT_LABEL`] for the default account, the label otherwise — a rendering for a
    /// SCREEN. [`route_key_of`] renders the default account as the bare venue id.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text().unwrap_or(RESERVED_DEFAULT_LABEL))
    }
}

impl serde::Serialize for AccountLabel {
    /// The label TEXT. [`AccountLabel::Default`] has none and is an ERROR rather than
    /// [`RESERVED_DEFAULT_LABEL`] — see the type's own doc: `DEFAULT` is exactly the spelling
    /// [`AccountLabel::parse`] refuses, so emitting it would produce a document this type cannot
    /// read back.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.text() {
            Some(l) => s.serialize_str(l),
            None => Err(serde::ser::Error::custom(format!(
                "the DEFAULT account has no label to serialize — carry it as an ABSENT field \
                 (`Option<AccountLabel>` + `skip_serializing_if`), which is what keeps an \
                 account-less mount's bytes unchanged; `{RESERVED_DEFAULT_LABEL}` is reserved and \
                 would not parse back"
            ))),
        }
    }
}

impl<'de> serde::Deserialize<'de> for AccountLabel {
    /// [`AccountLabel::parse`], verbatim — so the wire and the sidecar make exactly the refusals
    /// the constructor makes, at the one place they can be made for every carrier at once.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        AccountLabel::parse(&text)
            .map_err(|e| serde::de::Error::custom(format!("invalid account label {text:?}: {e}")))
    }
}

/// **The serde pair for a carrier that must round-trip [`AccountLabel::Default`]** —
/// `#[serde(with = "vike_model::accounts::account_keys::wire_account_option")]` on an
/// `Option<AccountLabel>` field.
///
/// # Why this exists rather than a fix to the impls above
///
/// Those impls are RIGHT for the carriers they serve, and they refuse `Default` on purpose: the
/// default account's key carries no label, so emitting one would invent a spelling that appears in
/// no file, and [`AccountLabel::parse`] would not read it back. Every carrier that can render the
/// default account as an ABSENT field does exactly that — `vike_exec::VenueBlock::
/// account` says so in those words.
///
/// ⚠ **`vike_model::OrderRequest::account` is the carrier that CANNOT**, and the two reasons are
/// independent. `Some(Default)` and `None` ROUTE DIFFERENTLY — on a two-account venue the first
/// reaches the unlabelled book and the second is refused as ambiguous
/// (`vike_core`'s `CoreThread::ambiguous_accounts`) — so collapsing them at the serializer
/// reintroduces the misroute the whole account-routing seam exists to delete. And the journal is
/// REQUIRED to carry which account an order was for, because a journal that omits it cannot be
/// reconciled against a venue later. So the field needed a REPRESENTATION that round-trips rather
/// than a value the writer could omit.
///
/// ⚠ Before this existed, a journaling daemon that accepted `{"account":"DEFAULT"}` — which the
/// daemon edge PASSES by design, the bare venue id being that account's own route key — reached
/// `serde_json::to_vec(…).expect("JournalRecordRef serializes")` in `vike_core`'s journal writer
/// and PANICKED THE FOLD THREAD. The core died and every later command answered `Gone`.
/// `crates/vike-core/tests/journal/journal_wiring.rs`'s
/// `a_submit_naming_the_default_account_journals_and_reads_back` is the gate.
///
/// # What it emits, and why nothing already on disk moves
///
/// [`AccountLabel`]'s `Display` for the value ([`RESERVED_DEFAULT_LABEL`] for the default account, the
/// label text otherwise) and [`parse_wire_account`] to read it back — the ONE parser that admits
/// that spelling, so this introduces no second reading of `DEFAULT` anywhere. A `Named` label
/// therefore emits the SAME bytes the impls above emit and reads back identically; the only value
/// whose representation changes is the one that could not be written at all, so no existing
/// document can contain the old spelling of it. `None` is unreachable through a field that also
/// declares `skip_serializing_if = "Option::is_none"`, and is handled anyway.
///
/// Zero allocation: the emitted `&str` is borrowed from the label or is the `'static` constant.
pub mod wire_account_option {
    use super::{AccountLabel, RESERVED_DEFAULT_LABEL, parse_wire_account};
    use serde::Deserialize;

    /// See the module doc. `Some` renders through [`AccountLabel`]'s `Display`.
    ///
    /// # Errors
    /// Only what the underlying serializer raises; this function itself never refuses a value —
    /// which is the whole difference from `impl Serialize for AccountLabel`.
    pub fn serialize<S: serde::Serializer>(
        account: &Option<AccountLabel>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match account {
            None => s.serialize_none(),
            Some(l) => s.serialize_some(l.text().unwrap_or(RESERVED_DEFAULT_LABEL)),
        }
    }

    /// See the module doc. Reads through [`parse_wire_account`], so every rule — the charset, the
    /// length bound, the case-sensitivity — still has exactly one implementation.
    ///
    /// # Errors
    /// A label [`parse_wire_account`] refuses, named in the message.
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<AccountLabel>, D::Error> {
        let Some(text) = Option::<String>::deserialize(d)? else {
            return Ok(None);
        };
        parse_wire_account(&text)
            .map(Some)
            .map_err(|e| serde::de::Error::custom(format!("invalid account {text:?}: {e}")))
    }
}

/// **One account of one venue** — what a `route_key` addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountRef {
    /// A [`VENUES`] id, so a ref can never name a venue with no bridge.
    pub venue: &'static str,
    /// Which account.
    pub label: AccountLabel,
}

impl AccountRef {
    /// **The routing key for this account** — `vike_exec::ExecutionEngine`'s `route_key` field, and
    /// the `LIVE-<route_key>.lock` sentinel `vike_boot::live_lock::LiveLock::acquire` takes.
    ///
    /// ⚠ For [`AccountLabel::Default`] this is the BARE VENUE ID, which is exactly what
    /// `ExecutionEngine::new` already seeds the field with — so a single-account process's routing
    /// is bit-identical, by construction rather than by care. A labelled account suffixes with
    /// `#`, the spelling that field's own doc names as satisfying its path-safety requirement.
    ///
    /// It deliberately does NOT carry the tier: an account runs at its row's one tier (decision
    /// 0156), so a tier in the routing key would be a distinction with no second value, and
    /// `LiveLock`'s sentinel would change name for every existing deployment.
    #[must_use]
    pub fn route_key(&self) -> String {
        route_key_of(self.venue, &self.label)
    }

    /// Is this the account a single-account box has?
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.label.is_default()
    }
}

/// **THE spelling of an account's routing key**, for a caller that holds a venue STRING rather than
/// a roster `&'static str` — `vike_core`'s `StrategyMount`, which names the account a strategy
/// trades on and must resolve it to `vike_exec::ExecutionEngine::route_key`.
///
/// [`AccountRef::route_key`] is this function, reached through a ref. There is ONE spelling because
/// a second one would be an engine mounted under a key nothing else looks for: the mount fan-out
/// stamps it, `vike_boot::live_lock` names a sentinel file after it, the journal records it, and
/// `vike_core`'s mount resolution matches on it. Four readers, one renderer.
///
/// ⚠ It takes a plain `&str` on purpose and so cannot check the venue against `crate::VENUES` —
/// which is fine here and NOT a weakening of [`AccountRef`]: a non-roster venue (a test's `"sim"`) can
/// only ever be its own default account, and the default arm renders the bare id unchanged.
#[must_use]
pub fn route_key_of(venue: &str, label: &AccountLabel) -> String {
    match label.text() {
        None => venue.to_string(),
        Some(l) => format!("{venue}#{l}"),
    }
}

/// **[`route_key_of`]'s INVERSE** — WHICH account of `venue` the routing key `route_key` names, or
/// `None` when that key does not belong to this venue at all.
///
/// `route_key_of(venue, &label_of_route_key(venue, k).unwrap()) == k` for every key this workspace
/// mints, which is the property `crates/vike-model/tests/account_keys.rs` pins.
///
/// # Why this exists rather than a `split_once('#')` at the call site
///
/// An engine carries `venue` and `route_key` and no label (`vike_exec::ExecutionEngine`), so a
/// reader that wants to SAY which account it is — `vike_exec::VenueBlock::account`, and
/// the operator surfaces after it — has to run this inverse. Doing it inline would be a second
/// spelling of the `#` convention beside [`route_key_of`]'s, and the two would be free to disagree
/// about the empty label, a venue that itself contains a `#`, and a label that is not a legal one.
/// They cannot disagree here.
///
/// ⚠ **The suffix goes through [`AccountLabel::parse`]**, so a malformed one answers `None` rather
/// than minting an `AccountLabel::Named` no file could ever have produced — the same reason that
/// type's `Deserialize` is hand-written. A key equal to the bare venue is [`AccountLabel::Default`],
/// which is what `vike_exec::ExecutionEngine::new` seeds every single-account engine with.
#[must_use]
pub fn label_of_route_key(venue: &str, route_key: &str) -> Option<AccountLabel> {
    if route_key == venue {
        return Some(AccountLabel::Default);
    }
    let suffix = route_key.strip_prefix(venue)?.strip_prefix('#')?;
    AccountLabel::parse(suffix).ok()
}
