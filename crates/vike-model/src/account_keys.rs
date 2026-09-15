//! `account_keys` — **the credential-key grammar for MORE THAN ONE account per venue**, declared
//! as data. STEP 1: nothing in this workspace reads a labelled key yet.
//!
//! # The requirement
//!
//! An operator may hold twenty hyperliquid accounts — demo and live — and wants every one of them
//! visible and independently armable. Today a venue has exactly one credential set per tier,
//! because the key names say so: [`crate::credential_keys`]' grammar is `{VENUE}_{TIER}{SUFFIX}`
//! and there is no place in it for a second account.
//!
//! # What the field does — and why the addressing seam is NOT reinvented here
//!
//! Three competitors, three shapes, surveyed before this module was written:
//!
//! * **NautilusTrader** keys each execution/data client on an ARBITRARY operator-chosen string
//!   rather than on the venue name (`"BINANCE_SPOT"` / `"BINANCE_FUTURES"` in their own
//!   multi-venue example), so two clients for one venue with different credentials coexist by
//!   simply taking different keys. That is exactly the shape
//!   `crates/vike-exec/src/execution_engine/mod.rs`'s `route_key` already has, down to its warning
//!   against decorating the `venue` field instead (the `"binance#2"` trap). So this module adds NO
//!   second addressing scheme: [`AccountRef::route_key`] RENDERS the existing one.
//! * **Hummingbot** makes an ACCOUNT a first-class named container (a `master_account` exists by
//!   default; the operator adds named ones and submits credentials against an (account, connector)
//!   pair). That is the presentation shape the requirement describes, and it is why the label here
//!   is a NAME the operator chooses rather than an index.
//! * **Freqtrade** refuses the problem in-process — one bot, one config, one systemd unit per
//!   account — and answers the shared-book hazard with a documentation warning. See
//!   `crates/vike-config/src/venue_accounts.rs`'s `shared_books` for what we do instead.
//!
//! Nautilus prescribes no naming convention at all, which is a warning rather than a licence: their
//! key is an opaque string with nothing to round-trip through. Ours has to round-trip through an
//! ENVIRONMENT-VARIABLE grammar that already exists and already has irregular members, so the
//! ambiguity is ours to resolve rather than the operator's to trip over.
//!
//! # ⚠ THE AMBIGUITY, and why the label goes at the END
//!
//! The obvious spelling is `{VENUE}_{TIER}_{LABEL}{SUFFIX}` — and it is **unparseable against the
//! keys that already exist**. Several venues' bespoke suffixes are themselves multi-word:
//! `OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, `ASTER_{TIER}_PRIVATE_KEY`,
//! `FXCM_{TIER}_USER` (see [`crate::credential_keys`]' own "what the grid deliberately does NOT
//! cover" section). Under that grammar `OANDA_DEMO_ACCOUNT_ID` reads equally well as
//! *tier `DEMO`, label `ACCOUNT`, suffix `_ID`* — a REAL, in-tree, live-credential key silently
//! reclassified as somebody's account. No amount of longest-match repairs it in general: it would
//! require the parser to enumerate every bespoke suffix in the workspace, which is precisely the
//! hand-kept list [`crate::credential_keys`] exists to avoid, and a venue adding a suffix later
//! would re-open the ambiguity with no test going red.
//!
//! So the label is **appended after the whole of today's key**, behind a separator that the
//! existing grammar cannot produce:
//!
//! ```text
//! {VENUE}_{TIER}{SUFFIX}                       the DEFAULT account — unchanged, byte for byte
//! {VENUE}_{TIER}{SUFFIX}__{LABEL}              a LABELLED account
//! ```
//!
//! [`ACCOUNT_SEPARATOR`] is a DOUBLE underscore, and that choice is what makes the parse total:
//! every existing key is single-underscore-separated words, so `__` occurs in none of them —
//! checked rather than assumed, by `crates/vike-model/tests/account_keys.rs`'s
//! `no_existing_credential_key_contains_the_separator`, which folds the enumerable grid AND sweeps
//! the bespoke literals out of every bridge crate's own source. The parse is therefore: **split at
//! the FIRST `__`; the left half is today's key, verbatim, whatever its shape; the right half is
//! the label.** `OANDA_DEMO_ACCOUNT_ID` contains no `__`, so it is the default account of
//! oanda/DEMO and nothing else — the ambiguity is gone STRUCTURALLY rather than by a table of
//! exceptions, and it stays gone for a bespoke suffix invented tomorrow.
//! `OANDA_DEMO_ACCOUNT_ID__HEDGE` is the `HEDGE` account's, with the suffix half untouched.
//!
//! A label is `[A-Z0-9]+` ([`AccountLabel::parse`]): no underscore, so a second `__` cannot appear
//! and the split can never be ambiguous about WHICH `__` it took; and it reserves `_` for a future
//! field after the label, should one ever be needed.
//!
//! # Byte-identity is the step-1 contract
//!
//! [`account_key`] with [`AccountLabel::Default`] returns its input unchanged, and
//! [`AccountRef::route_key`] for a default account returns the bare venue id — which is what
//! `vike_exec::ExecutionEngine::new` already seeds `route_key` with. A box whose store holds
//! `BYBIT_DEMO_API_KEY` and nothing else therefore parses to exactly one account, addressed
//! exactly as it is addressed today. Nothing here is read by a mount, a loader or a bridge yet.

use crate::credential_keys::{CREDENTIAL_TIERS, LEGACY_CREDENTIAL_TIERS};
use crate::venues::VENUES;

/// The token separating today's key from an account label — a DOUBLE underscore.
///
/// ⚠ The whole parse rests on this sequence being unreachable in the existing grammar. It is
/// checked, not assumed: `crates/vike-model/tests/account_keys.rs`'s
/// `no_existing_credential_key_contains_the_separator` folds the enumerable grid AND sweeps the
/// bespoke literals out of every bridge's own source.
pub const ACCOUNT_SEPARATOR: &str = "__";

/// The longest label accepted. A label becomes a component of
/// `vike_ops::live_lock::LiveLock`'s `LIVE-<route_key>.lock` filename, so it is bounded for the
/// same reason it is charset-restricted: a name that cannot be written to disk is a mount that
/// fails at the last possible moment.
pub const MAX_LABEL_LEN: usize = 24;

/// The label spelling RESERVED for the unlabelled account, refused as a label of its own.
///
/// An operator writing `DEFAULT` means the account they already have, whose ceiling is the venue's
/// own line — so accepting it would create a second, silent name for one account and a second,
/// silent ceiling for it. Refused by name instead.
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
    /// The account addressed by an unlabelled key — `BYBIT_DEMO_API_KEY`.
    #[default]
    Default,
    /// An operator-named account — the `ALT` of `BYBIT_DEMO_API_KEY__ALT`. Always
    /// [`AccountLabel::parse`]-validated, so the inner string is `[A-Z0-9]` of at most
    /// [`MAX_LABEL_LEN`] characters and is never [`RESERVED_DEFAULT_LABEL`].
    Named(String),
}

/// Why a would-be account key or label was refused. One variant per distinct operator mistake, for
/// the reason `crates/vike-config/src/venue_arming.rs`'s `ArmingBlock` has one per distinct cause:
/// each has a different fix, and a single `Invalid(String)` would make the caller re-derive which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKeyError {
    /// `BYBIT_DEMO_API_KEY__` — the separator with nothing after it.
    EmptyLabel,
    /// `__ALT` — a label with no key in front of it.
    EmptyBase,
    /// A byte outside `[A-Z0-9]`. Carries the offender so the message can name it; an underscore
    /// lands here, which is what makes a second separator impossible.
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
                "`{ACCOUNT_SEPARATOR}` with no account label after it — the label is the part that \
                 names WHICH account, so a key ending in the separator names none"
            ),
            AccountKeyError::EmptyBase => write!(
                f,
                "an account label with no credential key in front of it — the shape is \
                 `VENUE_TIER_SUFFIX{ACCOUNT_SEPARATOR}LABEL`"
            ),
            AccountKeyError::LabelChar(c) => write!(
                f,
                "an account label is A-Z and 0-9 only, and {c:?} is neither (an underscore \
                 especially: it is what keeps the separator unambiguous)"
            ),
            AccountKeyError::LabelTooLong(n) => write!(
                f,
                "an account label is at most {MAX_LABEL_LEN} characters and this one is {n} — the \
                 label becomes part of a lock FILENAME"
            ),
            AccountKeyError::ReservedLabel => write!(
                f,
                "`{RESERVED_DEFAULT_LABEL}` is reserved for the account an unlabelled key already \
                 addresses; its ceiling is that venue's own line, so naming it here would be a \
                 second name for one account"
            ),
        }
    }
}

impl AccountLabel {
    /// Validate an operator-supplied label. See [`AccountKeyError`] for every refusal.
    ///
    /// ⚠ Case-SENSITIVE, and deliberately not repaired: `alt` is refused rather than uppercased,
    /// for the reason `crates/vike-config/src/venue_mode.rs`'s `roster_id` refuses `Bybit` — a
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

impl std::fmt::Display for AccountLabel {
    /// [`RESERVED_DEFAULT_LABEL`] for the default account, the label otherwise — a rendering for a
    /// SCREEN, never for a key. [`account_key`] is what builds a key, and it renders the default
    /// account as no suffix at all.
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

/// **The one place a labelled key is spelled.** `base` is today's key, whatever its shape —
/// `BYBIT_DEMO_API_KEY`, `OANDA_DEMO_ACCOUNT_ID`, `ASTER_LIVE_PRIVATE_KEY`.
///
/// [`AccountLabel::Default`] returns `base` UNCHANGED, which is the step-1 contract: a
/// single-account box's key names are not merely compatible, they are the same `String`.
///
/// ```
/// use vike_model::account_keys::{account_key, AccountLabel};
/// let base = format!("BYBIT_{}_API_KEY", "DEMO");
/// assert_eq!(account_key(&base, &AccountLabel::Default), base);
/// let alt = AccountLabel::parse("ALT").expect("a legal label");
/// assert_eq!(account_key(&base, &alt), format!("{base}__ALT"));
/// ```
#[must_use]
pub fn account_key(base: &str, label: &AccountLabel) -> String {
    match label.text() {
        None => base.to_string(),
        Some(l) => format!("{base}{ACCOUNT_SEPARATOR}{l}"),
    }
}

/// One credential key split into today's key and the account it belongs to — the output of
/// [`split_account_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitKey<'a> {
    /// Today's key, verbatim: everything left of the first [`ACCOUNT_SEPARATOR`], or the whole
    /// name when there is none. **Never re-parsed here** — that is what keeps every bespoke suffix
    /// working without being enumerated.
    pub base: &'a str,
    /// Which account the key belongs to.
    pub label: AccountLabel,
}

/// **THE parse.** Split at the FIRST [`ACCOUNT_SEPARATOR`]; no separator means the default account
/// and a `base` equal to the whole input.
///
/// # Errors
/// [`AccountKeyError`] when a separator is present but what follows it is not a legal label — an
/// empty one, a second separator (an underscore is refused by [`AccountLabel::parse`]), an
/// over-long one, the reserved spelling — or when nothing precedes it.
pub fn split_account_key(name: &str) -> Result<SplitKey<'_>, AccountKeyError> {
    let Some(at) = name.find(ACCOUNT_SEPARATOR) else {
        return Ok(SplitKey { base: name, label: AccountLabel::Default });
    };
    let (base, rest) = name.split_at(at);
    if base.is_empty() {
        return Err(AccountKeyError::EmptyBase);
    }
    let label = AccountLabel::parse(&rest[ACCOUNT_SEPARATOR.len()..])?;
    Ok(SplitKey { base, label })
}

/// **One account of one venue at one tier** — what a Data Manager row is about, and what a
/// `route_key` addresses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountRef {
    /// A [`VENUES`] id, so a ref can never name a venue with no bridge — the same construction
    /// `crates/vike-config/src/venue_mode.rs`'s `VenuePolicy` uses for its keys.
    pub venue: &'static str,
    /// A [`CREDENTIAL_TIERS`] spelling. ⚠ NORMALIZED: the legacy `MAINNET` tier
    /// ([`LEGACY_CREDENTIAL_TIERS`]) resolves to `LIVE`, because a pre-rename store holding a
    /// `MAINNET` key set has one live account and not a second one called MAINNET.
    pub tier: &'static str,
    /// Which account.
    pub label: AccountLabel,
}

impl AccountRef {
    /// **The routing key for this account** — `vike_exec::ExecutionEngine`'s `route_key` field, and
    /// the `LIVE-<route_key>.lock` sentinel `vike_ops::live_lock::LiveLock::acquire` takes.
    ///
    /// ⚠ For [`AccountLabel::Default`] this is the BARE VENUE ID, which is exactly what
    /// `ExecutionEngine::new` already seeds the field with — so a single-account process's routing
    /// is bit-identical, by construction rather than by care. A labelled account suffixes with
    /// `#`, the spelling that field's own doc names as satisfying its path-safety requirement.
    ///
    /// It deliberately does NOT carry the tier: one process mounts a venue at one tier (the mount
    /// resolves a single `Environment` per venue), so a tier in the routing key would be a
    /// distinction with no second value, and `LiveLock`'s sentinel would change name for every
    /// existing deployment.
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
/// stamps it, `vike_ops::live_lock` names a sentinel file after it, the journal records it, and
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

/// The roster id whose `{VENUE}_` prefix `name` starts with — the LONGEST, so a future roster
/// holding both `ig` and `igx` cannot resolve the wrong one.
///
/// No roster pair is in a prefix relation today (`crates/vike-model/tests/account_keys.rs`'s
/// `the_roster_has_no_prefix_pairs` pins it); the longest match is what keeps this function correct
/// when one is added rather than requiring the roster to be checked by hand.
fn venue_prefix_of(name: &str) -> Option<&'static str> {
    VENUES
        .iter()
        .copied()
        .filter(|v| {
            let upper = v.to_uppercase();
            name.len() > upper.len()
                && name.starts_with(&upper)
                && name.as_bytes()[upper.len()] == b'_'
        })
        .max_by_key(|v| v.len())
}

/// The canonical tier `rest` starts with, plus the remainder after it — `LIVE` for the legacy
/// `MAINNET` spelling. `None` when the token after the venue is not a tier at all.
fn tier_prefix_of(rest: &str) -> Option<(&'static str, &str)> {
    CREDENTIAL_TIERS
        .iter()
        .map(|t| (*t, *t))
        // The legacy tier normalizes onto LIVE — see `AccountRef::tier`.
        .chain(LEGACY_CREDENTIAL_TIERS.iter().map(|t| (*t, "LIVE")))
        .find_map(|(spelling, canonical)| {
            let head = format!("{spelling}_");
            rest.strip_prefix(&head).map(|tail| (canonical, tail))
        })
}

/// **Which account, if any, a credential key name belongs to.** `None` for a name that is not a
/// `{VENUE}_{TIER}` credential key at all.
///
/// ⚠ It answers `None` for three real families, and that is a classification rather than a gap:
///
/// * attribution keys — the token after the venue is no tier;
/// * the polymarket L2 trio and the JForex sidecar's variables — no roster venue is spelled `POLY`
///   or `JFOREX`;
/// * the two genuinely NON-CONFORMING credential families: dukascopy's numbered sub-account, which
///   bakes an account index into the tier token (the one venue that already ships a multi-account
///   shape, and one this grammar deliberately does not retro-fit), and aster's `TESTNET` tier
///   spelling, which is outside [`CREDENTIAL_TIERS`]. Both are pinned by name in
///   `crates/vike-model/tests/account_keys.rs` so that a change to either is a red test rather
///   than a silent reclassification.
#[must_use]
pub fn account_ref_from_key(name: &str) -> Option<AccountRef> {
    let split = split_account_key(name).ok()?;
    let venue = venue_prefix_of(split.base)?;
    let rest = &split.base[venue.len() + 1..];
    let (tier, suffix) = tier_prefix_of(rest)?;
    // A bare `{VENUE}_{TIER}_` names no credential; only a key with an actual suffix does.
    if suffix.is_empty() {
        return None;
    }
    Some(AccountRef { venue, tier, label: split.label })
}

/// **Every account the credential store holds** — the enumeration the Data Manager renders, DERIVED
/// from the key names rather than from a list anybody maintains.
///
/// Sorted and deduplicated, so a screen built from it does not reorder between runs and a venue's
/// three keys yield one account. Names that are not credential keys are skipped silently: the store
/// legitimately holds attribution codes, sidecar paths and the bespoke families named in
/// [`account_ref_from_key`], and a store is not malformed for containing them.
#[must_use]
pub fn accounts_in_store<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<AccountRef> {
    let mut out: Vec<AccountRef> = keys.filter_map(account_ref_from_key).collect();
    out.sort();
    out.dedup();
    out
}
