//! `InstanceOrigin` — the claim a running instance stamps into every client order id it mints, so
//! a SECOND instance trading the same venue account is RECOGNISABLE rather than anonymous.
//!
//! # The hole this closes
//!
//! `crates/vike-core/src/journal_lock.rs` refuses a second writer on one journal directory, and
//! its module doc lists what two writers break. Three of those four survive a move to two
//! MACHINES, because a file lock is a fact about one filesystem: two containers with separate
//! volumes, two VMs, two hosts — each takes its own lock, each believes it is alone, both start.
//! If they carry the same venue API key they trade ONE account while treating each other's orders
//! as foreign.
//!
//! A network lock is not the answer here (a new dependency, a new failure mode, and a lease that
//! is wrong exactly when the network is). The answer is IDENTITY: carry an origin claim on the
//! wire, and let reconcile read it. A divergence then says "another instance of mine" instead of
//! "an unknown order", and the fold decision can be made on that.
//!
//! # The format, and why it is a SESSION prefix rather than a coid suffix
//!
//! An origin-tagged coid is `<origin>V<8-hex-session><seq>`, produced by
//! [`crate::client_order_id::ClientOrderIdGenerator`] with NO change to its mint: the tag and the
//! [`ORIGIN_MARK`] are folded into the SESSION STRING at construction, so `generate` still
//! concatenates `session + seq` and every persisted `(session, seq)` pair already carries the
//! origin. That buys three properties a separate generator field would each have cost work:
//!
//! - **The journal needs no new field.** `Snap`'s `coid_session`
//!   (`crates/vike-core/src/journal/record.rs`) round-trips the origin for free, so a replay
//!   reproduces byte-identical ids and the determinism fence is untouched.
//!   [`crate::client_order_id::ClientOrderIdGenerator::resume`] stays the exact inverse of its
//!   `state`.
//! - **The hot path is unchanged.** No branch, no second allocation, no new work per order — the
//!   `p99 < 10µs` core-hop gate cannot see this feature at all.
//! - **The tag rides at the HEAD.** `crates/bridges/binance/src/family/order_map.rs`'s
//!   `binance_broker_coid` truncates the coid TAIL to fit Binance's 36-char ceiling; a tag at the
//!   tail would be the first thing silently eaten, and a silently-truncated origin is worse than
//!   no origin at all (it reads as a DIFFERENT instance). At the head it survives truncation, and
//!   what truncation costs instead is sequence range — budgeted per venue in
//!   [`crate::venue_coid_budget`], which asserts every venue keeps
//!   [`crate::venue_coid_budget::MIN_SEQ_DIGITS`] digits with the tag present.
//!
//! # Why `V`
//!
//! A live untagged session is `uuid4().hex[:8]` — eight LOWERCASE HEX characters — and the
//! sequence is decimal digits, so an untagged coid contains no letter above `f` and no uppercase
//! at all. `V` can therefore never occur in an untagged id, which makes [`origin_of_coid`] an
//! unambiguous parse rather than a heuristic: no length prefix, and no separator that
//! [`crate::client_order_id::is_valid_crypto_coid`] would reject (the charset is
//! `^[A-Za-z0-9]{1,32}$`, so uppercase is wire-legal on every venue that constrains the id at
//! all).
//!
//! The tag itself is lowercased on parse ([`InstanceOrigin::parse`]) for exactly that reason: a
//! `V` inside the tag would make the marker scan ambiguous. This is a NORMALISATION, not a
//! rejection — an operator writing `West` gets `west`, and two operators writing `West` and `west`
//! are correctly the same instance.
//!
//! # What identifies an instance
//!
//! The tag is DECLARED, never derived. `config.instance_origin`
//! (`crates/vike-config/src/config.rs`, env `VIKE_INSTANCE_ORIGIN`) is the whole of it, and unset
//! — the default — mints exactly the ids this workspace has always minted.
//!
//! Deriving it was considered and rejected on each candidate: the PROJECT ROOT changes when a
//! deployment is moved and is identical for two containers built from one image; a random id
//! persisted in the state directory is baked into an image by anyone who builds after a first run
//! and is lost by anyone who mounts a fresh volume — in both directions SILENTLY, which is the one
//! outcome a tag must not have; the SETTINGS DIRECTORY PATH is identical in two containers by
//! construction. A declared tag is the only candidate whose failure mode an operator can SEE: two
//! deployments that legitimately share one config file share one tag, read as one instance, and
//! get exactly today's behaviour — which is the honest answer, because by the only evidence the
//! system has they ARE configured as one instance. `docs/ops/double-live-instances.md` is the
//! operator page.

use std::fmt;

/// The marker byte between the origin tag and the session hex. See the module doc for why an
/// uppercase letter is the unambiguous choice.
pub const ORIGIN_MARK: char = 'V';

/// Longest accepted origin tag. Four base-36 characters = 1,679,616 distinct instances, several
/// orders of magnitude more than the problem needs; the ceiling exists because every character
/// here is charged to the venue id budget ([`crate::venue_coid_budget`]).
pub const MAX_ORIGIN_LEN: usize = 4;

/// Characters an origin costs a client order id at its widest: [`MAX_ORIGIN_LEN`] plus the
/// [`ORIGIN_MARK`]. THE number every venue budget reserves — see
/// `crates/vike-model/src/venue_coid_budget.rs`'s `coid_budget_for` and
/// `crates/vike-model/src/attribution.rs`'s `AttributionMechanic::validate_code`, which reserves
/// it UNCONDITIONALLY so that a Binance broker code can never eat the tag's room.
pub const ORIGIN_OVERHEAD: usize = MAX_ORIGIN_LEN + 1;

/// Width of the random session half — `uuid4().hex[:8]`, unchanged by this feature.
pub const SESSION_HEX_LEN: usize = 8;

/// A validated instance origin tag: 1..=[`MAX_ORIGIN_LEN`] lowercase ASCII alphanumerics, never
/// containing [`ORIGIN_MARK`] (lowercasing guarantees it).
///
/// Not `Copy`: it is a short owned string that lives in configuration and in a journaled
/// `ReconPolicy`, never on the fold hot path.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct InstanceOrigin(String);

/// Why an operator-supplied origin was refused. Carries the offending value so a startup message
/// can echo what was actually configured — an origin is an identity, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginError {
    /// Empty or all-whitespace. Absent is how you say "no origin"; blank is a typo.
    Blank,
    /// Longer than [`MAX_ORIGIN_LEN`] once trimmed.
    TooLong { value: String, len: usize },
    /// Contains something outside `[A-Za-z0-9]`.
    Charset { value: String },
}

impl fmt::Display for OriginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OriginError::Blank => write!(
                f,
                "instance origin is blank — leave the key unset to run without an origin tag"
            ),
            OriginError::TooLong { value, len } => write!(
                f,
                "instance origin {value:?} is {len} characters; at most {MAX_ORIGIN_LEN} are \
                 allowed (every character is charged to the venue client-order-id budget)"
            ),
            OriginError::Charset { value } => write!(
                f,
                "instance origin {value:?} must be ASCII letters and digits only (it goes on the \
                 wire inside a client order id)"
            ),
        }
    }
}

impl std::error::Error for OriginError {}

impl InstanceOrigin {
    /// Validate and NORMALISE an operator-supplied tag: surrounding whitespace trimmed, ASCII
    /// letters lowercased. See the module doc for why lowercasing is load-bearing rather than
    /// cosmetic.
    pub fn parse(raw: &str) -> Result<Self, OriginError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(OriginError::Blank);
        }
        if !trimmed.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(OriginError::Charset { value: trimmed.to_string() });
        }
        if trimmed.len() > MAX_ORIGIN_LEN {
            return Err(OriginError::TooLong { value: trimmed.to_string(), len: trimmed.len() });
        }
        Ok(InstanceOrigin(trimmed.to_ascii_lowercase()))
    }

    /// The normalised tag.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Build the coid SESSION string this origin mints under: `<origin>V<session_hex>`. The
    /// generator concatenates `session + seq` unchanged, so this is the ONE place the wire format
    /// is composed.
    pub fn session_prefix(&self, session_hex: &str) -> String {
        let mut s = String::with_capacity(self.0.len() + 1 + session_hex.len());
        s.push_str(&self.0);
        s.push(ORIGIN_MARK);
        s.push_str(session_hex);
        s
    }
}

impl fmt::Display for InstanceOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for InstanceOrigin {
    type Error = OriginError;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        InstanceOrigin::parse(&v)
    }
}

impl From<InstanceOrigin> for String {
    fn from(v: InstanceOrigin) -> String {
        v.0
    }
}

/// Read the origin tag out of a client order id (or out of a bare session string — the parse is
/// the same, since a coid is its session followed by decimal digits).
///
/// `None` means "this id carries no origin claim", which is the answer for every id this
/// workspace minted before the feature existed, for every id an unconfigured instance mints today,
/// and for anything a venue hands back that is not one of ours (Hyperliquid's reports echo a
/// 128-bit `cloid` HASH, not the id — see `crates/vike-model/src/venue_coid_budget.rs`).
///
/// STRICT on purpose, in the SAFE direction: a false `None` costs only today's behaviour, while a
/// false `Some` would let an instance's own fill read as a stranger's and be HELD instead of
/// folded. So the tag must be 1..=[`MAX_ORIGIN_LEN`] LOWERCASE alphanumerics, the marker must be
/// the first [`ORIGIN_MARK`] in the string, and a full [`SESSION_HEX_LEN`] characters must follow
/// it. A venue that case-folds the id it echoes therefore reads as untagged rather than as a
/// different instance.
///
/// Deliberately accepts a BARE SESSION (no sequence digits) as well as a whole coid: the session
/// is the id's own prefix, and this is what
/// [`crate::client_order_id::ClientOrderIdGenerator::origin`] reads its answer from — one parse,
/// so a generator can never disagree with reconcile about whose an id is.
pub fn origin_of_coid(coid: &str) -> Option<&str> {
    let mark = coid.find(ORIGIN_MARK)?;
    if mark == 0 || mark > MAX_ORIGIN_LEN {
        return None;
    }
    let (tag, rest) = coid.split_at(mark);
    if !tag.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
        return None;
    }
    // `rest` still carries the marker; a whole session must follow it.
    if rest.len() < 1 + SESSION_HEX_LEN {
        return None;
    }
    Some(tag)
}

/// Does `coid` carry an origin claim belonging to a DIFFERENT instance than `local`?
///
/// `false` whenever the answer is not positively yes — no local origin configured, no tag on the
/// id, or the same tag. That asymmetry is the safety property: this predicate is only ever
/// consulted to HOLD a divergence that would otherwise fold
/// (`crates/vike-exec/src/recon/resolve.rs`'s `mode_applies_divergence`), so a `false` can only
/// reproduce today's behaviour while a wrong `true` would suppress a real fold.
pub fn coid_is_foreign(coid: &str, local: Option<&InstanceOrigin>) -> bool {
    let Some(local) = local else { return false };
    match origin_of_coid(coid) {
        Some(tag) => tag != local.as_str(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_normalises_case_and_trims() {
        assert_eq!(InstanceOrigin::parse("  West ").unwrap().as_str(), "west");
        assert_eq!(InstanceOrigin::parse("A1").unwrap(), InstanceOrigin::parse("a1").unwrap());
    }

    #[test]
    fn parse_refuses_what_cannot_ride_a_client_order_id() {
        assert_eq!(InstanceOrigin::parse("   ").unwrap_err(), OriginError::Blank);
        assert!(matches!(
            InstanceOrigin::parse("abcde").unwrap_err(),
            OriginError::TooLong { len: 5, .. }
        ));
        // A hyphen is legal in a Binance broker code but NOT here: the id must stay
        // `^[A-Za-z0-9]{1,32}$` on the wire.
        assert!(matches!(InstanceOrigin::parse("a-b").unwrap_err(), OriginError::Charset { .. }));
    }

    #[test]
    fn a_parsed_tag_can_never_contain_the_marker() {
        // The whole marker scan rests on this. Uppercase input is the only way a `V` could get in.
        for raw in ["V", "vV", "AV", "VVVV"] {
            let o = InstanceOrigin::parse(raw).unwrap();
            assert!(!o.as_str().contains(ORIGIN_MARK), "{raw} -> {o}");
        }
    }

    #[test]
    fn session_prefix_composes_the_wire_shape() {
        let o = InstanceOrigin::parse("ap1").unwrap();
        assert_eq!(o.session_prefix("deadbeef"), "ap1Vdeadbeef");
    }

    #[test]
    fn an_untagged_coid_reads_as_having_no_origin() {
        // Every id this workspace minted before the feature: 8 lowercase hex + a decimal seq.
        for coid in ["deadbeef0", "a1b2c3d47654", "00000000123456", "ffffffff9"] {
            assert_eq!(origin_of_coid(coid), None, "{coid}");
        }
    }

    #[test]
    fn a_tagged_coid_yields_its_tag() {
        assert_eq!(origin_of_coid("ap1Vdeadbeef0"), Some("ap1"));
        assert_eq!(origin_of_coid("bVdeadbeef17"), Some("b"));
        assert_eq!(origin_of_coid("wxyzVa1b2c3d40"), Some("wxyz"));
    }

    #[test]
    fn the_parse_refuses_every_near_miss_rather_than_guessing() {
        // marker at position 0 — no tag at all
        assert_eq!(origin_of_coid("Vdeadbeef0"), None);
        // tag too long to be one
        assert_eq!(origin_of_coid("abcdeVdeadbeef0"), None);
        // uppercase tag: a venue that case-folded our id reads as untagged, never as a stranger
        assert_eq!(origin_of_coid("AP1Vdeadbeef0"), None);
        // nothing (or not enough) after the marker to be a whole session
        assert_eq!(origin_of_coid("ap1V"), None);
        assert_eq!(origin_of_coid("ap1Vdeadbee"), None);
        // ...but a BARE session (no sequence yet) is accepted — that is what the generator reads
        assert_eq!(origin_of_coid("ap1Vdeadbeef"), Some("ap1"));
        // strings a venue report can carry INSTEAD of one of our ids
        assert_eq!(origin_of_coid("0x9f3c1a2b4d5e6f708192a3b4c5d6e7f8"), None);
        assert_eq!(origin_of_coid("EXT-binance-771"), None);
    }

    #[test]
    fn foreignness_is_only_ever_a_positive_answer() {
        let local = InstanceOrigin::parse("ap1").unwrap();
        assert!(coid_is_foreign("bx2Vdeadbeef0", Some(&local)), "another instance's tag");
        assert!(!coid_is_foreign("ap1Vdeadbeef0", Some(&local)), "our own tag");
        assert!(!coid_is_foreign("deadbeef0", Some(&local)), "an untagged id is not a stranger");
        assert!(!coid_is_foreign("bx2Vdeadbeef0", None), "no local origin => nothing is foreign");
    }
}
