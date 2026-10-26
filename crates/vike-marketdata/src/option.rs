//! The option feed vocabulary: [`OptionKind`] (call or put), the per-option live row
//! [`OptionTicker`], and the chain snapshot types [`OptionChain`], [`Expiry`], [`StrikeRow`],
//! [`OptionQuote`] and [`UnderlyingKind`], and the per-underlying bundle [`UnderlyingChains`].
//!
//! These are plain, venue-free value types: this crate holds the data a feed emits for options and
//! no pricing maths. The types are also exported at the crate root.
//
// Engineering notes (not rendered): the maths over them stays in `vike-options`, and so do the
// chain constructors that need a calendar (`make_expiry`, `expiry_ms`) and the strike window
// (`limit_strikes`): this crate has no `chrono` and takes none. `OptionKind` sits in this crate
// rather than in `vike-model` because the chain snapshot rows name it and this is the lower rank
// (`layer = 5`): `vike-model` re-exports it at its root, as it does `Bar`.
//
// `option` is a folder module: `option/chain.rs` holds the snapshot types and `option/ticker.rs`
// the live row. Both children are PRIVATE `mod`s and this root `pub use`s their items, so
// `vike_marketdata::option::OptionChain` is the one module path (the published-crate rule in this
// crate's page).

use serde::{Deserialize, Serialize};

mod chain;
mod ticker;

pub use chain::{Expiry, OptionChain, OptionQuote, StrikeRow, UnderlyingChains, UnderlyingKind};
pub use ticker::OptionTicker;

/// Call or put. On the wire a venue spells it `"C"` or `"P"` ([`OptionKind::from_cp`] /
/// [`OptionKind::as_cp`]); any other kind is unrepresentable. Serialized as `"call"` / `"put"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionKind {
    /// A call option (the right to buy).
    Call,
    /// A put option (the right to sell).
    Put,
}

impl OptionKind {
    /// Parse the venue wire letter: `"C"` gives [`OptionKind::Call`], `"P"` gives
    /// [`OptionKind::Put`], anything else `None`.
    pub fn from_cp(s: &str) -> Option<Self> {
        match s {
            "C" => Some(Self::Call),
            "P" => Some(Self::Put),
            _ => None,
        }
    }

    /// The venue wire letter (`"C"` / `"P"`).
    pub fn as_cp(self) -> &'static str {
        match self {
            Self::Call => "C",
            Self::Put => "P",
        }
    }
}
