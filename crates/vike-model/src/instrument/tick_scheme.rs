//! Tiered (price-dependent) tick schemes — the venue grid where the minimum price increment is a
//! FUNCTION OF THE PRICE, not a single per-symbol scalar.
//!
//! [`crate::instrument::SymbolProperties::tick_size`] models the flat case every crypto perp/spot
//! venue reports: ONE increment for the whole price range. Options desks do not work that way.
//! Deribit's `public/get_instrument` returns a `tick_size` PLUS a `tick_size_steps` array —
//! `[{"above_price": 0.005, "tick_size": 0.0005}]` for BTC options — meaning "0.0001 up to 0.005,
//! 0.0005 above it". A limit price above the boundary that was snapped onto the BASE grid is
//! REJECTED by the venue, which is why the live options smoke deliberately rests
//! its order below the boundary (`crates/bridges/deribit/tests/deribit_smoke.rs`, the
//! "option ticks are TIERED" note). This module is the venue-neutral model of that grid.
//!
//! Contract:
//! * [`TickScheme`] is a VALIDATED value — `TickScheme::new` is the only constructor, and it
//!   rejects a non-positive/non-finite base tick, a non-positive/non-finite tier tick, a negative
//!   or non-finite boundary, out-of-order boundaries, and more than [`MAX_TICK_TIERS`] tiers. So a
//!   `TickScheme` in hand always resolves to a strictly-positive tick.
//! * It is `Copy` and INLINE (a fixed `[TickTier; MAX_TICK_TIERS]` + a length), deliberately:
//!   `SymbolProperties` is `Copy` and is passed by value through the risk path, and a `Vec` would
//!   both break that and put an allocation on the order path.
//! * The BOUNDARY belongs to the LOWER tier: `TickTier::above_price` is compared STRICTLY
//!   (`price > above_price`), matching the field's name. See [`TickScheme::tick_at`].
//! * Nothing here changes [`crate::scalar::round_to`]/[`crate::scalar::round_to_step`] — those are
//!   parity-sacred and feed the pinned Decimal wire-format site. The tier-aware rounder
//!   [`round_price_tiered`] RESOLVES a tick and then delegates to the very same primitive, and its
//!   `None` arm is literally today's expression, so an instrument with no scheme is byte-identical.
//!
//! No I/O; pure `f64` math. Venue parsing of `tick_size_steps` stays with the venue (the bridge
//! crates), exactly as `SymbolProperties`' own per-venue parse functions do.

use crate::scalar::{nz_step, round_to, round_to_step};

/// How many tiers a [`TickScheme`] can carry. Deribit — the only venue known to report tiers today
/// — uses ONE step per option instrument; four is headroom, chosen to keep the struct `Copy` and
/// small. A venue reporting more must be reported as a blocker (widen this const in ONE place)
/// rather than silently truncated: [`TickScheme::new`] returns [`TickSchemeError::TooManyTiers`].
pub const MAX_TICK_TIERS: usize = 4;

/// ONE tier of a [`TickScheme`]: strictly above `above_price`, the effective increment becomes
/// `tick_size`. The twin of one element of Deribit's `tick_size_steps` array.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TickTier {
    /// The price the tier starts ABOVE (exclusive — see [`TickScheme::tick_at`]).
    pub above_price: f64,
    /// The increment in force for prices above [`TickTier::above_price`].
    pub tick_size: f64,
}

/// Why a [`TickScheme`] could not be built. Kept as a small `Copy` enum (not a string) so callers
/// can branch; `Display` gives the operator-facing text and is what serde reports on a bad row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickSchemeError {
    /// The base tick was non-finite or `<= 0.0` — a scheme must always resolve to a usable grid.
    BadBaseTick,
    /// A tier's `tick_size` was non-finite or `<= 0.0`.
    BadTierTick,
    /// A tier's `above_price` was non-finite or negative.
    BadBoundary,
    /// Tier boundaries were not STRICTLY increasing (duplicates included) — resolution walks them
    /// in order, so an unsorted array would silently mean something other than it reads.
    UnsortedTiers,
    /// More than [`MAX_TICK_TIERS`] tiers were supplied.
    TooManyTiers,
}

impl std::fmt::Display for TickSchemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            TickSchemeError::BadBaseTick => "base tick must be finite and > 0",
            TickSchemeError::BadTierTick => "tier tick_size must be finite and > 0",
            TickSchemeError::BadBoundary => "tier above_price must be finite and >= 0",
            TickSchemeError::UnsortedTiers => "tier above_price values must strictly increase",
            TickSchemeError::TooManyTiers => "too many tick tiers",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for TickSchemeError {}

/// A base tick plus N ascending `(above_price -> tick)` tiers — modeled on Deribit's
/// `tick_size` + `tick_size_steps`.
///
/// Construct with [`TickScheme::new`]; the fields are private so an INVALID scheme is
/// unrepresentable (including after a deserialize — the `Deserialize` impl runs the same
/// validation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickScheme {
    base_tick: f64,
    /// Ascending by `above_price` for the first `len` entries; the tail is inert padding
    /// (`PAD`), so two schemes built from the same tiers compare equal.
    tiers: [TickTier; MAX_TICK_TIERS],
    len: u8,
}

/// The inert padding slot for [`TickScheme::tiers`] beyond `len` — a constant so `PartialEq`
/// (which compares the whole array) only ever sees identical padding.
const PAD: TickTier = TickTier { above_price: 0.0, tick_size: 0.0 };

impl TickScheme {
    /// THE constructor. Validates the whole grid up front so every later resolution is
    /// total and allocation-free. `tiers` must be strictly ascending by `above_price`.
    ///
    /// An empty `tiers` slice is legal and yields a FLAT scheme, behaviorally identical to the
    /// plain scalar `tick_size` it was built from — that arm exists so a caller can build a scheme
    /// unconditionally without branching.
    pub fn new(base_tick: f64, tiers: &[TickTier]) -> Result<Self, TickSchemeError> {
        if !base_tick.is_finite() || base_tick <= 0.0 {
            return Err(TickSchemeError::BadBaseTick);
        }
        if tiers.len() > MAX_TICK_TIERS {
            return Err(TickSchemeError::TooManyTiers);
        }
        let mut prev = f64::NEG_INFINITY;
        for t in tiers {
            if !t.above_price.is_finite() || t.above_price < 0.0 {
                return Err(TickSchemeError::BadBoundary);
            }
            if !t.tick_size.is_finite() || t.tick_size <= 0.0 {
                return Err(TickSchemeError::BadTierTick);
            }
            if t.above_price <= prev {
                return Err(TickSchemeError::UnsortedTiers);
            }
            prev = t.above_price;
        }
        let mut slots = [PAD; MAX_TICK_TIERS];
        slots[..tiers.len()].copy_from_slice(tiers);
        // `tiers.len() <= MAX_TICK_TIERS` (checked above), so the cast cannot truncate.
        Ok(TickScheme { base_tick, tiers: slots, len: tiers.len() as u8 })
    }

    /// The increment in force at or below the FIRST tier boundary.
    #[inline]
    pub fn base_tick(&self) -> f64 {
        self.base_tick
    }

    /// The live tiers, ascending by `above_price` (empty for a flat scheme) — the padding is
    /// never exposed.
    #[inline]
    pub fn tiers(&self) -> &[TickTier] {
        &self.tiers[..self.len as usize]
    }

    /// Whether this scheme actually varies with price. A `false` here means every resolution
    /// returns [`TickScheme::base_tick`].
    #[inline]
    pub fn is_tiered(&self) -> bool {
        self.len > 0
    }

    /// THE resolution function: the effective tick at `price`.
    ///
    /// BOUNDARY RULE — the boundary belongs to the LOWER tier. A tier applies only when
    /// `|price| > above_price` (STRICT), matching the field's name: at EXACTLY `above_price` the
    /// tick in force below it still applies. In the shape venues actually publish this choice is
    /// behaviorally inert — the boundary itself sits on both grids (Deribit: `0.005` is a multiple
    /// of both the `0.0001` base and the `0.0005` tier tick), so rounding a price that equals the
    /// boundary returns the boundary either way — but it is PINNED by a test so that flipping it
    /// is a deliberate act, not a silent drift.
    ///
    /// The MAGNITUDE (`|price|`) is what is compared, mirroring [`crate::scalar::order_notional`]:
    /// a signed net limit (a credit combo) must resolve on the same grid as its debit mirror.
    /// A `NaN` price resolves to the base tick (every `>` comparison against `NaN` is false); an
    /// INFINITE price resolves to the TOP tier (`inf > every finite boundary`, and the magnitude is
    /// what is compared, so `-inf` lands there too). Either way the function is total and never
    /// yields a zero/NaN tick.
    #[inline]
    pub fn tick_at(&self, price: f64) -> f64 {
        let p = price.abs();
        let mut tick = self.base_tick;
        for t in self.tiers() {
            if p > t.above_price {
                tick = t.tick_size;
            } else {
                break; // tiers ascend — no later tier can apply
            }
        }
        tick
    }

    /// Snap `price` onto the grid in force AT THAT PRICE: `round_to_step(price, tick_at(price))`.
    ///
    /// The tier is resolved from the INPUT price and the rounding then happens ONCE, with no
    /// re-resolution: re-rounding the input on a second, finer tick (what a naive "the result
    /// landed in another tier, try again" loop would do) can walk the price straight OFF the
    /// coarse grid the venue requires. Landing on the coarse grid is safe on the shape venues
    /// publish, where each tier tick is an integer multiple of the base tick and each boundary
    /// sits on the base grid: a coarse-grid value is then always also a base-grid value, and a
    /// base-tick rounding from below can never jump strictly past a boundary.
    ///
    /// Half-to-EVEN, delegated verbatim to [`crate::scalar::round_to_step`] — the tick is
    /// strictly positive by construction, so the guard [`crate::scalar::round_to`] adds is moot.
    #[inline]
    pub fn round_price(&self, price: f64) -> f64 {
        round_to_step(price, self.tick_at(price))
    }
}

/// The TIER-AWARE variant of [`crate::scalar::round_to`] — the drop-in every
/// `SymbolProperties`-driven price-rounding site wants.
///
/// With `scheme == None` this is LITERALLY today's expression (`round_to(value, nz_step(tick))`),
/// so an instrument with no tiered grid — every venue but Deribit options — is byte-identical to
/// a world without this module. With a scheme, the tick is resolved from the price first.
#[inline]
pub fn round_price_tiered(value: f64, scheme: Option<&TickScheme>, tick_size: f64) -> f64 {
    match scheme {
        Some(s) => s.round_price(value),
        None => round_to(value, nz_step(tick_size)),
    }
}

// ---- serde -------------------------------------------------------------------------------------
// Hand-written so the WIRE shape is the venue-shaped `{base_tick, tiers: [...]}` (a flat scheme
// omits `tiers` entirely) rather than the padded inline array, and so EVERY decode re-runs
// `TickScheme::new`'s validation — a persisted row can no more produce an invalid scheme than a
// caller can. The helper structs live inside the fn bodies so no private type appears in a
// public signature.

/// `skip_serializing_if` predicate for the borrowed tier slice — module-level (not nested in the
/// `serialize` body) so the derive-generated code resolves the path unambiguously. The argument is
/// `&FieldType`, i.e. a reference to the `&[TickTier]` field.
fn tiers_empty(t: &&[TickTier]) -> bool {
    t.is_empty()
}

impl serde::Serialize for TickScheme {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(serde::Serialize)]
        struct Wire<'a> {
            base_tick: f64,
            #[serde(skip_serializing_if = "tiers_empty")]
            tiers: &'a [TickTier],
        }
        let wire = Wire { base_tick: self.base_tick, tiers: self.tiers() };
        serde::Serialize::serialize(&wire, serializer)
    }
}

impl<'de> serde::Deserialize<'de> for TickScheme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Wire {
            base_tick: f64,
            #[serde(default)]
            tiers: Vec<TickTier>,
        }
        let wire = <Wire as serde::Deserialize>::deserialize(deserializer)?;
        TickScheme::new(wire.base_tick, &wire.tiers).map_err(serde::de::Error::custom)
    }
}

#[path = "tick_scheme_tests.rs"]
#[cfg(test)]
mod tick_scheme_tests;
