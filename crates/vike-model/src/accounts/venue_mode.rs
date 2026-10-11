//! [`VenueMode`] — **THE tier vocabulary of an account; `paper < demo < live`.** One enum for the
//! three words wherever they mean a tier: the `account.tier` column, the tier a mount reached, the
//! tier a bridge is handed (`vike_bridge_core::venue_mount::MountInputs::tier`), the tier a venue
//! session authenticated at, the journal and the projection
//! (`docs/decisions/0129-one-tier-vocabulary-in-vike-model.md`).
//!
//! An account trades at its own tier: the `tier` of its ACTIVE row in the settings database's
//! `account` table (decision 0119), read by the mount (`crates/vike-mount/src/arming.rs`'s
//! `account_tier`). This module owns only the WORDS — the three tiers, their order, their spelling
//! and the one parser — and decides nothing: no row, an inactive row or a `paper` row all mean
//! `paper`, and that rule is the mount's.
//!
//! # ⚠ The ORDER is part of the type
//!
//! [`VenueMode`] derives `PartialOrd`/`Ord` over variants declared in ascending risk order, so
//! `Paper < Demo < Live` is a property of the declaration rather than of a comment. Every consumer
//! that asks "is the tier the mount reached BELOW the account's own" leans on it
//! (`vike_config::VenueArming::is_capped`, and the mount's interlock that refuses a bridge binding
//! past the account's tier).
//!
//! # ⚠ ONE parser
//!
//! [`VenueMode`]'s [`FromStr`] takes `paper`, `demo` or `live`, case-insensitive and trimmed
//! (`DEMO`, ` Live ` — operators type these into `--tier` flags and dotted setting keys, and every
//! spelling that worked before the type existed keeps working). The old `sim` spelling of `paper`
//! is NOT accepted (decision 0129 dropped it: there are no legacy fallbacks, decision 0117). Every
//! place a tier WORD becomes a tier goes through it, so a word one reader accepts and another
//! refuses cannot exist. Serde stays exact lowercase: it reads what this crate wrote.
//!
//! ⚠ **A credential KEY's tier token is a different alphabet** — `SIM`/`DEMO`/`LIVE`
//! (`crate::credential_keys::CREDENTIAL_TIERS`), the words an operator typed into a key NAME. That
//! token is not a tier word and never goes through this parser; `vike_secrets`'
//! `account_tier_of_key_token` is the one map from it onto a [`VenueMode`].
//!
//! What `live` MEANS depends on the venue (decision 0095, item 3 of 0119): for binance, bybit, okx
//! and hyperliquid it is MAINNET and `demo` is the demo network; elsewhere the venue's own mount
//! picks among the credentials it finds for the tier.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// One account's tier — `account.tier`'s vocabulary, the tier a mount reached, and the tier a
/// bridge binds.
///
/// ⚠ The variants are declared in ASCENDING risk order and the `Ord` derive follows declaration
/// order, so `Paper < Demo < Live` is a property of this list rather than of a comment. Reordering
/// them silently inverts every comparison; there is deliberately no explicit discriminant to make
/// the ordering look like an arbitrary numbering that could be edited.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum VenueMode {
    /// Simulated fills against the local paper exchange. No credential is used, no order leaves
    /// the process. **The default**, because the safest answer is what an account no active row
    /// names must get.
    #[default]
    Paper,
    /// The venue's own demo/testnet/sandbox/practice account — real wire, real rejections, no real
    /// funds.
    Demo,
    /// The venue's production account. Real money.
    Live,
}

impl VenueMode {
    /// Every mode, in ascending risk order — the order [`Ord`] agrees with, so a caller rendering a
    /// menu and a caller comparing two modes cannot disagree.
    pub const ALL: [VenueMode; 3] = [VenueMode::Paper, VenueMode::Demo, VenueMode::Live];

    /// The wire/row spelling, which is also what serde reads and writes (`rename_all` above), what
    /// [`FromStr`] accepts and what `account.tier`'s `CHECK` admits.
    ///
    /// Every message, every rendering, every SQL bind and every structured log field
    /// (`tier = VenueMode::Live.as_str()` on a bridge's live-mount line,
    /// `crates/vike-ops/tests/venues/live_mount_line_gate.rs`) goes through this rather than through
    /// a literal, so the legal set an error prints cannot drift from the set the parser accepts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VenueMode::Paper => "paper",
            VenueMode::Demo => "demo",
            VenueMode::Live => "live",
        }
    }

    /// The legal words, rendered for a refusal message — `paper / demo / live`.
    ///
    /// Derived from [`VenueMode::ALL`] so the message and the parser cannot disagree about what is
    /// legal; the same argument as `crates/vike-config/src/write.rs`'s `unknown_section_message`.
    #[must_use]
    pub fn legal_words() -> String {
        VenueMode::ALL.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(" / ")
    }
}

impl fmt::Display for VenueMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A word [`VenueMode`]'s [`FromStr`] refused — it carries the word, and its `Display` names the
/// three legal ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotATier {
    /// The word as it was given, unrepaired.
    pub word: String,
}

impl fmt::Display for NotATier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a tier (one of {})", self.word, VenueMode::legal_words())
    }
}

impl std::error::Error for NotATier {}

impl FromStr for VenueMode {
    type Err = NotATier;

    /// **THE tier parser.** `paper`, `demo` or `live`, case-insensitive and trimmed (`DEMO`,
    /// ` Live `): operators type these words into `--tier` flags and dotted setting keys, and every
    /// spelling that worked before the type existed keeps working. Anything else, `sim` included
    /// (decision 0129), is [`NotATier`].
    fn from_str(word: &str) -> Result<Self, Self::Err> {
        let trimmed = word.trim();
        VenueMode::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(trimmed))
            .ok_or_else(|| NotATier { word: word.to_string() })
    }
}

#[path = "venue_mode_tests.rs"]
#[cfg(test)]
mod venue_mode_tests;
