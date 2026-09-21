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

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(above_price: f64, tick_size: f64) -> TickTier {
        TickTier { above_price, tick_size }
    }

    /// The real Deribit BTC-option grid: base `0.0001`, one step to `0.0005` above `0.005`.
    fn deribit_option_scheme() -> TickScheme {
        TickScheme::new(0.0001, &[tier(0.005, 0.0005)]).expect("valid deribit grid")
    }

    #[test]
    fn resolves_below_at_and_above_the_boundary() {
        let s = deribit_option_scheme();
        assert!(s.is_tiered());
        assert_eq!(s.base_tick(), 0.0001);
        // BELOW the boundary -> the base tick
        assert_eq!(s.tick_at(0.0001), 0.0001);
        assert_eq!(s.tick_at(0.0049), 0.0001);
        // ABOVE the boundary -> the tier tick (this is the case the live smoke dodges)
        assert_eq!(s.tick_at(0.0051), 0.0005);
        assert_eq!(s.tick_at(0.05), 0.0005);
        assert_eq!(s.tick_at(1.0), 0.0005);
    }

    /// THE boundary pin: `above_price` is EXCLUSIVE, so the boundary itself belongs to the LOWER
    /// tier. Flipping this must break a test, not slip through.
    #[test]
    fn the_exact_boundary_belongs_to_the_lower_tier() {
        let s = deribit_option_scheme();
        assert_eq!(s.tick_at(0.005), 0.0001, "exactly AT above_price -> the tick BELOW it");
        // ...and on the shape venues publish the choice is inert: the boundary is on both grids,
        // so the snapped price is the same under either reading.
        assert_eq!(s.round_price(0.005), round_to_step(0.005, 0.0001));
        assert_eq!(round_to_step(0.005, 0.0001), round_to_step(0.005, 0.0005));
    }

    /// Several tiers resolve by walking ascending boundaries, each with its own exclusive edge.
    #[test]
    fn multi_tier_resolution_walks_ascending_boundaries() {
        let tiers = [tier(10.0, 0.05), tier(100.0, 0.5), tier(1000.0, 5.0)];
        let s = TickScheme::new(0.01, &tiers).unwrap();
        assert_eq!(s.tiers().len(), 3);
        assert_eq!(s.tick_at(0.0), 0.01);
        assert_eq!(s.tick_at(9.99), 0.01);
        assert_eq!(s.tick_at(10.0), 0.01); // exact boundary -> lower tier
        assert_eq!(s.tick_at(10.5), 0.05);
        assert_eq!(s.tick_at(100.0), 0.05); // exact boundary -> lower tier
        assert_eq!(s.tick_at(100.5), 0.5);
        assert_eq!(s.tick_at(1000.0), 0.5); // exact boundary -> lower tier
        assert_eq!(s.tick_at(5000.0), 5.0);
    }

    /// A FLAT scheme (no tiers) is the scalar world: every price resolves to the base tick, and
    /// `round_price` equals the pinned primitive on that one tick.
    #[test]
    fn flat_scheme_is_the_scalar_grid() {
        let s = TickScheme::new(0.5, &[]).unwrap();
        assert!(!s.is_tiered());
        assert!(s.tiers().is_empty());
        for p in [0.0, 0.4, 2.5, 1e9] {
            assert_eq!(s.tick_at(p), 0.5);
            assert_eq!(s.round_price(p), round_to_step(p, 0.5));
        }
    }

    /// Magnitude, not sign: a signed (credit-combo) net limit resolves on the same grid as its
    /// debit mirror. A non-finite price stays total: `NaN` yields the BASE tick (no `>` holds),
    /// `±inf` the TOP tier (every finite boundary is exceeded).
    #[test]
    fn resolution_uses_the_magnitude_and_stays_total() {
        let s = deribit_option_scheme();
        assert_eq!(s.tick_at(-0.05), 0.0005);
        assert_eq!(s.tick_at(-0.001), 0.0001);
        assert_eq!(s.tick_at(f64::NAN), 0.0001);
        assert_eq!(s.tick_at(f64::INFINITY), 0.0005); // inf > every finite boundary
        assert_eq!(s.tick_at(f64::NEG_INFINITY), 0.0005); // ...magnitude, so likewise
    }

    /// Rounding snaps onto the grid RESOLVED FROM THE INPUT price — delegating, unchanged, to the
    /// parity-sacred primitive. This is the hole the lane exists to close: a price above the tier
    /// boundary snapped on the BASE grid is what the venue rejects.
    #[test]
    fn rounding_uses_the_tier_in_force_at_the_input_price() {
        let s = deribit_option_scheme();
        // above the boundary: the COARSE grid (0.0005), not the base one
        assert_eq!(s.round_price(0.01234), round_to_step(0.01234, 0.0005));
        assert_ne!(s.round_price(0.01234), round_to_step(0.01234, 0.0001));
        // below it: the base grid
        assert_eq!(s.round_price(0.00123), round_to_step(0.00123, 0.0001));
        // and the tiered result really is a multiple of the coarse tick
        let snapped = s.round_price(0.01234);
        assert_eq!(snapped, round_to_step(snapped, 0.0005), "snapping is idempotent on the tier");
    }

    /// A base-tick rounding from just BELOW a boundary can at worst land ON it, never strictly
    /// past it (the boundary is on the base grid) — the property `round_price`'s single pass
    /// relies on.
    #[test]
    fn base_grid_rounding_never_jumps_past_the_boundary() {
        let s = deribit_option_scheme();
        for p in [0.00494, 0.004951, 0.004999] {
            let r = s.round_price(p);
            assert!(r <= 0.005, "{p} rounded to {r}, past the boundary");
        }
    }

    #[test]
    fn invalid_schemes_are_rejected() {
        assert_eq!(TickScheme::new(0.0, &[]), Err(TickSchemeError::BadBaseTick));
        assert_eq!(TickScheme::new(-0.1, &[]), Err(TickSchemeError::BadBaseTick));
        assert_eq!(TickScheme::new(f64::NAN, &[]), Err(TickSchemeError::BadBaseTick));
        assert_eq!(TickScheme::new(f64::INFINITY, &[]), Err(TickSchemeError::BadBaseTick));
        let bad_tick = [tier(1.0, 0.0)];
        assert_eq!(TickScheme::new(0.1, &bad_tick), Err(TickSchemeError::BadTierTick));
        let inf_tick = [tier(1.0, f64::INFINITY)];
        assert_eq!(TickScheme::new(0.1, &inf_tick), Err(TickSchemeError::BadTierTick));
        let neg_edge = [tier(-1.0, 0.5)];
        assert_eq!(TickScheme::new(0.1, &neg_edge), Err(TickSchemeError::BadBoundary));
        let nan_edge = [tier(f64::NAN, 0.5)];
        assert_eq!(TickScheme::new(0.1, &nan_edge), Err(TickSchemeError::BadBoundary));
        // duplicate / descending boundaries are BOTH unsorted
        let dup = [tier(5.0, 0.5), tier(5.0, 1.0)];
        assert_eq!(TickScheme::new(0.1, &dup), Err(TickSchemeError::UnsortedTiers));
        let descending = [tier(9.0, 0.5), tier(5.0, 1.0)];
        assert_eq!(TickScheme::new(0.1, &descending), Err(TickSchemeError::UnsortedTiers));
    }

    /// Over-long tier arrays are REPORTED, never silently truncated — a truncated grid would
    /// put a wrong-tick price on the wire.
    #[test]
    fn more_tiers_than_capacity_is_an_error_not_a_truncation() {
        let many: Vec<TickTier> = (1..=MAX_TICK_TIERS + 1).map(|i| tier(i as f64, 0.5)).collect();
        assert_eq!(many.len(), MAX_TICK_TIERS + 1);
        assert_eq!(TickScheme::new(0.1, &many), Err(TickSchemeError::TooManyTiers));
        // exactly at capacity is fine
        assert!(TickScheme::new(0.1, &many[..MAX_TICK_TIERS]).is_ok());
    }

    /// THE off-path pin: with no scheme, the tier-aware rounder is the EXPRESSION IN USE TODAY.
    #[test]
    fn tierless_rounding_is_byte_identical_to_round_to() {
        for tick in [0.0, 0.0001, 0.01, 0.5, -0.5] {
            for v in [0.0, 1.23456, -1.23456, 2.5, 0.00499, 1e9] {
                assert_eq!(
                    round_price_tiered(v, None, tick),
                    round_to(v, nz_step(tick)),
                    "v={v} tick={tick}"
                );
            }
        }
    }

    /// ...and WITH a scheme it is the scheme's own rounding (the tick_size argument is ignored).
    #[test]
    fn tiered_rounding_defers_to_the_scheme() {
        let s = deribit_option_scheme();
        assert_eq!(round_price_tiered(0.01234, Some(&s), 0.0001), s.round_price(0.01234));
        assert_eq!(round_price_tiered(0.01234, Some(&s), 999.0), s.round_price(0.01234));
    }

    #[test]
    fn serde_round_trips_flat_and_tiered_schemes() {
        let flat = TickScheme::new(0.5, &[]).unwrap();
        let s = serde_json::to_string(&flat).unwrap();
        assert_eq!(s, r#"{"base_tick":0.5}"#, "a flat scheme omits `tiers` entirely");
        assert_eq!(serde_json::from_str::<TickScheme>(&s).unwrap(), flat);

        let tiered = deribit_option_scheme();
        let s = serde_json::to_string(&tiered).unwrap();
        assert!(s.contains("\"tiers\""), "a tiered scheme carries its tiers: {s}");
        let back: TickScheme = serde_json::from_str(&s).unwrap();
        assert_eq!(back, tiered);
        assert_eq!(back.tiers(), tiered.tiers());
        assert_eq!(back.tick_at(0.05), 0.0005);
    }

    /// The wire shape is the VENUE shape, so a hand-written (or venue-mirroring) payload decodes.
    #[test]
    fn deserializes_the_venue_shaped_payload() {
        let raw = r#"{"base_tick":0.0001,"tiers":[{"above_price":0.005,"tick_size":0.0005}]}"#;
        let s: TickScheme = serde_json::from_str(raw).unwrap();
        assert_eq!(s, deribit_option_scheme());
    }

    /// A persisted row can no more hold an invalid scheme than a caller can: deserialization
    /// re-runs the constructor's validation and FAILS rather than yielding a zero-tick grid.
    #[test]
    fn deserializing_an_invalid_scheme_fails() {
        assert!(serde_json::from_str::<TickScheme>(r#"{"base_tick":0.0}"#).is_err());
        let descending = r#"{"base_tick":0.1,"tiers":[{"above_price":9,"tick_size":1},
                             {"above_price":5,"tick_size":2}]}"#;
        assert!(serde_json::from_str::<TickScheme>(descending).is_err());
    }
}
