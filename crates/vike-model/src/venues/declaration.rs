//! **What a venue adapter DECLARES, as the words every daemon may name**: the clock risk a venue
//! states ([`ClockRisk`]), what a start drops from the credentials before the bridge sees them
//! ([`Withhold`]), the credential-free summary of one venue's declaration the arming table
//! carries ([`VenueAdapterDeclarationView`]) and its clock word ([`ClockDeclarationKind`]).
//!
//! The declaration itself is the bridge's (`vike_bridge_core::venue_adapter::VenueAdapterDeclaration`,
//! decision 0096), and a hub client may not name `vike-bridge-core` (decision 0164). These words came
//! down here so the bridge, the gatewayhub, its lease wire and the tradehub name ONE type each. Their
//! serde tags and field names ARE the lease wire's (`fixtures/gatewayhub_wire/`, held by
//! `crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`): a rename is a protocol break.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::GridSource;

/// What a drifted host clock actually COSTS at a venue — the fact each wired row must state, so the
/// report's remedy can never assert a mechanism the venue does not have, and so only the venues
/// that can lose an order are judged against the recv-window thresholds (the THRESHOLDS are the
/// tradehub's judgement over this fact, never the gatewayhub's: only a signed-timestamp venue may
/// FAIL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockRisk {
    /// Every private request is SIGNED with a timestamp and rejected outside a recv window (the
    /// signers hard-code `recv_window: 5000`). Drift far enough and EVERY order is rejected:
    /// binance `-1021`, bybit `10002`, okx `50102`.
    SignedTimestamp,
    /// The clock is bound into the order NONCE rather than a recv window. Hyperliquid accepts a
    /// nonce inside `(T - 2 days, T + 1 day)`
    /// (`crates/bridges/hyperliquid/src/signing/hash.rs`'s `NonceManager`), so the rejection cliff
    /// is a DAY away, not five seconds.
    NonceWindow,
    /// Auth carries no per-request timestamp at all (deribit's `client_credentials`, ig's session
    /// token pair), so no order can be rejected for clock drift. The leg is a HOST-HEALTH canary:
    /// it still proves the box's NTP is broken, and a wrong clock still misdates every local
    /// record of the session.
    NoTimestamp,
}

impl ClockRisk {
    /// The remediation line for a measured out-of-band skew at a venue with this risk. Every one
    /// starts with the same action — the host clock is what is wrong — and differs in what it
    /// costs, which is the part an operator prioritises on.
    #[must_use]
    pub fn remedy(self) -> &'static str {
        match self {
            ClockRisk::SignedTimestamp => {
                "sync the host clock (NTP / w32tm) — this venue stamps a timestamp on every signed \
                 request and rejects one outside a 5000 ms recv window, so drift rejects ORDERS"
            }
            ClockRisk::NonceWindow => {
                "sync the host clock (NTP / w32tm) — this venue binds the clock into the order \
                 nonce (valid within roughly a day), so orders survive this drift but the host's \
                 NTP is broken"
            }
            ClockRisk::NoTimestamp => {
                "sync the host clock (NTP / w32tm) — this venue's auth stamps no timestamp, so \
                 orders are NOT at risk here; the drift misdates every local record and is evidence \
                 the host's NTP is broken"
            }
        }
    }
}

/// What a start drops from the credentials before the bridge sees them: the tradehub's two reasons
/// to start an adapter DATA-ONLY, applied at the gatewayhub (`AccountDirectory::without_venue_credentials`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Withhold {
    /// The venue's startup preflight hard-FAILED this session: its credentials are withheld and its
    /// `demo`/`live` accounts stop with [`crate::ArmingBlock::PreflightFailed`].
    PreflightFailed,
    /// The venue is declared `data_only`: its exec credentials are withheld while a credentialed
    /// market feed authenticates from the same store.
    DataOnly,
}

/// **How a venue declares its server clock, as the arming table's facts carry it** (the preflight's
/// clock leg reads it): the bridge's `ClockDeclaration` summarised, [`Self::Absent`] for a venue this build
/// does not carry, and [`Self::Unknown`] for a word this build does not know (a peer of another
/// version).
///
/// ⚠ Its wire form is the WORD ([`Self::word`]: `wired`, `not-wired`, `absent`) under the key
/// `clock_declaration_word`, so the serde is written by hand rather than derived: an unknown word reads as
/// [`Self::Unknown`] holding it, never as an error, and writes back unchanged (the frozen rows of
/// `fixtures/gatewayhub_wire/` carry three such words). A row with no key reads as `Unknown("")`, the
/// empty word the field always defaulted to. No decision reads an `Unknown`: the preflight counts
/// [`Self::Wired`] alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockDeclarationKind {
    /// The venue's clock endpoint is wired: the preflight reads it.
    Wired,
    /// The venue declares no clock endpoint, with its reason.
    NotWired,
    /// This build does not carry the venue's bridge.
    Absent,
    /// A word this build does not know, as it was read.
    Unknown(String),
}

impl ClockDeclarationKind {
    /// The wire word.
    #[must_use]
    pub fn word(&self) -> &str {
        match self {
            ClockDeclarationKind::Wired => "wired",
            ClockDeclarationKind::NotWired => "not-wired",
            ClockDeclarationKind::Absent => "absent",
            ClockDeclarationKind::Unknown(word) => word,
        }
    }

    /// The kind a wire word names; a word this build does not know is [`Self::Unknown`].
    #[must_use]
    pub fn from_word(word: &str) -> Self {
        match word {
            "wired" => ClockDeclarationKind::Wired,
            "not-wired" => ClockDeclarationKind::NotWired,
            "absent" => ClockDeclarationKind::Absent,
            other => ClockDeclarationKind::Unknown(other.to_string()),
        }
    }
}

impl Default for ClockDeclarationKind {
    /// The empty word a row without the key always read as.
    fn default() -> Self {
        ClockDeclarationKind::Unknown(String::new())
    }
}

impl Serialize for ClockDeclarationKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.word())
    }
}

impl<'de> Deserialize<'de> for ClockDeclarationKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|word| ClockDeclarationKind::from_word(&word))
    }
}

/// **The static, credential-free facts of one venue's adapter**, read once with the arming table: a
/// SUMMARY of the bridge's declaration (five of its fields, two of them as text, no book rule) plus
/// whether this build carries the bridge at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueAdapterDeclarationView {
    /// `false` = the daemon build carries no adapter for the venue
    /// ([`crate::ArmingBlock::FeatureAbsent`]): a `paper` account of it runs the simulator, a
    /// `demo`/`live` one does not run.
    pub adapter_present: bool,
    /// Whether the start can address a NAMED (labelled) account. Unread when `adapter_present` is
    /// `false`.
    pub addresses_accounts: bool,
    /// Whether the bridge takes the reconnect-resync trigger.
    pub takes_reconcile_trigger: bool,
    /// Where a declared leg's grid comes from.
    pub grid_source: GridSource,
    /// The process-exclusive resource the venue's start claims (a JForex sidecar), if it declares one.
    #[serde(default)]
    pub process_exclusive: Option<String>,
    /// How the venue declares its server clock (the preflight's clock leg reads it).
    #[serde(default)]
    pub clock_declaration_word: ClockDeclarationKind,
}

impl VenueAdapterDeclarationView {
    /// **The facts of a venue whose bridge this build does not carry** (a `FeatureAbsent` row, or an
    /// id the registry holds no row for): no adapter, nothing addressable, no grid, no exclusive
    /// resource, clock word [`ClockDeclarationKind::Absent`].
    #[must_use]
    pub fn absent() -> Self {
        VenueAdapterDeclarationView {
            adapter_present: false,
            addresses_accounts: false,
            takes_reconcile_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_declaration_word: ClockDeclarationKind::Absent,
        }
    }
}

#[path = "declaration_tests.rs"]
#[cfg(test)]
mod declaration_tests;
