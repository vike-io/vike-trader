//! Pure Deribit combo-book wire model — `ComboSpec` legs → `private/create_combo` params, the
//! venue's returned combo definition → a canonical [`ComboMapping`], and the resolved combo →
//! `private/buy`/`private/sell` order params. NO I/O: every function here is a total function of
//! its arguments, which is where this feature's test coverage lives (`mod tests` below).
//!
//! # Why a combo is TWO venue calls, not one
//!
//! Deribit does NOT accept inline legs on an order. A combo is a first-class *instrument* with its
//! own order book ("combo book"): you first register the leg structure with `private/create_combo`,
//! which returns a combo `id` (e.g. `BTC-CS-29APR22-39300_39600`), and you then trade that id
//! through the ORDINARY `private/buy` / `private/sell` path. So `vike_model::build_combo`'s
//! contract — "`symbol` is left EMPTY; the venue adapter resolves the combo instrument name at
//! submit" — maps exactly onto this two-step.
//!
//! # Wire shapes (verbatim from the Deribit API reference, `api-reference/combo-books/`)
//!
//! `private/create_combo` params:
//! ```json
//! { "trades": [ { "instrument_name": "BTC-PERPETUAL", "amount": 1, "direction": "buy" } ] }
//! ```
//! * `instrument_name` (string, required) — "Unique instrument identifier"
//! * `amount` (number, required) — the leg size
//! * `direction` (string, required) — enum `"buy"` | `"sell"`
//!
//! result (also the shape of `public/get_combo_details`):
//! ```json
//! { "id": "BTC-FS-31DEC21-PERP", "instrument_id": 1, "state": "active",
//!   "legs": [ { "instrument_name": "BTC-PERPETUAL", "amount": -1 } ] }
//! ```
//! * `state` — enum `"active"` | `"inactive"`
//! * `legs[].amount` (integer) — "Size multiplier of a leg. A negative value indicates that the
//!   trades on given leg are in opposite direction to the combo trades they originate from"
//!
//! That `legs[].amount` IS our [`vike_model::ComboLeg::ratio`], which is why the round-trip below
//! is a ratio comparison and not a string compare.
//!
//! # The orientation/scale law (the reason this module exists)
//!
//! Deribit CANONICALIZES a combo. Asking for `+1 A / -1 B` may hand back an already-existing combo
//! defined as `-1 A / +1 B` (the inverse), or a ratio-reduced form (`+2/-2` → `+1/-1`). Submitting
//! our side/price against a differently-oriented venue combo would trade the WRONG DIRECTION at the
//! WRONG PRICE. So we never assume: we read the venue's returned legs back and solve for the one
//! signed rational `k` with `venue_ratio = k · spec_ratio` on EVERY leg (exact, in integer
//! cross-multiplication — no float compare). `k` is then CARRIED exactly, as the reduced integer
//! fraction `num/den` ([`ComboMapping`]) — never collapsed to an f64 — and:
//!
//! | quantity        | venue value             |
//! |-----------------|-------------------------|
//! | side            | `sign(k) · spec_side`   |
//! | amount          | `spec_qty / abs(k)`     |
//! | net limit price | `k · spec_net`          |
//!
//! Buying one venue unit acquires `k` spec units, so it costs `k · spec_net` — which is exactly why
//! **the price sign is load-bearing and is never clamped, `abs()`-ed, or assumed positive** here.
//! A credit combo has a genuinely negative net, and an inverted orientation flips a debit into a
//! credit. If no such `k` exists, [`map_combo`] returns an error and the caller must REJECT the
//! order rather than guess (see [`ComboMapError`]).
//!
//! # Why `k` stays a rational all the way to the wire (the one-ULP tick loss)
//!
//! A non-dyadic `k` — 1/3, from a `+3/-3` spec the venue reduces to `+1/-1` — has no exact f64.
//! Rescaling in floats can land a value whose EXACT venue image lies ON the tick/step grid one ULP
//! below it, and the wire's round-toward-zero then costs a FULL tick: at `k = ⅓`, net `0.03` @
//! tick `0.0005` quantized to `0.0095` instead of `0.0100` (~19% of on-grid nets lose a tick this
//! way), and qty `2.3` @ step `0.1` to `6.8` instead of `6.9` (~7%). Reordering the f64 ops does
//! NOT fix it (multiply-then-divide trades price losses for worse qty losses). So the rescale
//! happens INSIDE the Decimal quantization site — [`format_scaled_to_step_f`], the pinned wire
//! formatter's exact-rational sibling — and [`ComboOrder`] carries SPEC-side values plus the
//! mapping instead of pre-scaled f64s. Dyadic scales (1, 1/2, 1/4) were exact either way, which is
//! why only non-dyadic fixtures can catch a regression here.

use serde_json::{Value, json};

use vike_bridge_core::format::format_scaled_to_step_f;
use vike_bridge_core::json::json_num;
use vike_model::ComboLeg;

/// The venue's own combo definition, parsed from a `private/create_combo` /
/// `public/get_combo_details` result. `legs` reuses [`ComboLeg`] because Deribit's
/// `legs[].amount` has precisely `ComboLeg::ratio`'s meaning (signed size multiplier per combo
/// unit).
#[derive(Debug, Clone, PartialEq)]
pub struct VenueCombo {
    /// combo instrument name, e.g. `BTC-CS-29APR22-39300_39600` — this is what gets traded
    pub id: String,
    /// `"active"` | `"inactive"`; only an active combo book accepts orders
    pub state: String,
    pub legs: Vec<ComboLeg>,
}

impl VenueCombo {
    /// Deribit's `state` enum is `active` | `inactive`; an inactive combo book cannot be traded.
    pub fn is_active(&self) -> bool {
        self.state == "active"
    }
}

/// Why the venue's combo definition could not be reconciled with the requested spec. EVERY variant
/// is a hard submit-reject: an unreconcilable combo means we do not know which direction or at what
/// net price the venue would trade, and guessing risks the wrong side at the wrong price.
#[derive(Debug, Clone, PartialEq)]
pub enum ComboMapError {
    /// `create_combo` returned no parseable combo object (missing `id`/`legs`, or a leg `amount`
    /// that does not decode to an in-range integer multiplier)
    Unparseable,
    /// the combo book exists but is `inactive` — it accepts no orders
    Inactive(String),
    /// the venue combo has a different number of legs than the spec
    LegCountMismatch { spec: usize, venue: usize },
    /// a leg the venue named is not in the spec (or vice versa) — symbol sets differ; carries the
    /// symbol that failed to pair
    LegSymbolMismatch(String),
    /// the venue's ratios are not ANY signed multiple of the spec's — the combos differ in
    /// structure, not merely orientation/scale
    RatioMismatch,
    /// a zero ratio reached the mapper (the model forbids it, so this is a venue-side surprise)
    ZeroRatio,
    /// both leg lists are empty — nothing to reconcile (the model forbids an empty spec and
    /// [`parse_combo`] rejects empty venue legs, so this is defensive totality, not a live path)
    EmptyLegs,
}

impl core::fmt::Display for ComboMapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unparseable => write!(f, "create_combo returned no parseable combo"),
            Self::Inactive(id) => write!(f, "combo {id} is inactive (no order book)"),
            Self::LegCountMismatch { spec, venue } => {
                write!(f, "combo leg count mismatch: spec {spec}, venue {venue}")
            }
            Self::LegSymbolMismatch(s) => write!(f, "combo leg symbol mismatch: {s}"),
            Self::RatioMismatch => {
                write!(f, "venue combo ratios are not a signed multiple of the spec's")
            }
            Self::ZeroRatio => write!(f, "combo leg with ratio 0"),
            Self::EmptyLegs => write!(f, "combo with no legs"),
        }
    }
}

impl std::error::Error for ComboMapError {}

/// How the venue's canonical combo relates to the requested spec: `venue_ratio = k · spec_ratio`
/// on every leg, where `k` is the EXACT signed rational `num / den` — [`map_combo`] emits the
/// reduced form with `den > 0`, so the orientation sign lives in `num`. `k` is deliberately NOT an
/// f64: a non-dyadic scale (1/3) has no exact float and would cost a full tick at wire
/// quantization (see the module doc). The f64 methods below are PREVIEWS (logging, law tests);
/// the wire values are produced from the rational inside [`build_combo_order_params`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComboMapping {
    /// numerator of `k`; `num < 0` = the venue combo is our spec's INVERSE (every leg flipped)
    pub num: i64,
    /// denominator of `k`; positive as built by [`map_combo`]
    pub den: i64,
}

impl ComboMapping {
    /// The identity mapping — the venue returned exactly the spec we asked for.
    pub const IDENTITY: ComboMapping = ComboMapping { num: 1, den: 1 };

    /// Orientation: `+1` = the venue combo is our spec; `-1` = it is the INVERSE. (Robust to a
    /// hand-constructed negative `den`, though [`map_combo`] never emits one.)
    pub fn sign(&self) -> i32 {
        if (self.num < 0) != (self.den < 0) { -1 } else { 1 }
    }

    /// Venue order side for a spec side (`±1`). An inverted combo flips buy↔sell.
    pub fn venue_side(&self, spec_side: i32) -> i32 {
        self.sign() * spec_side
    }

    /// f64 PREVIEW of the venue order amount (combo units) for a spec qty — one venue unit is
    /// `abs(k)` spec units. NEVER quantize this: the wire amount is computed from the exact
    /// rational inside [`build_combo_order_params`].
    pub fn venue_qty(&self, spec_qty: f64) -> f64 {
        spec_qty * (self.den as f64 / self.num as f64).abs()
    }

    /// f64 PREVIEW of the venue NET limit price for a spec net limit. **SIGNED** — `k · net`. A
    /// credit combo stays negative; an inverted orientation flips debit↔credit. No clamp, no
    /// `abs()`. NEVER quantize this: the wire price is computed from the exact rational inside
    /// [`build_combo_order_params`].
    pub fn venue_price(&self, spec_net: f64) -> f64 {
        spec_net * self.num as f64 / self.den as f64
    }
}

/// Build `private/create_combo` params from the spec's legs.
///
/// The combo DEFINITION is per-unit and side-independent: `amount` is the leg's `|ratio|` and
/// `direction` carries the ratio's sign. The order's own side and qty are applied later, at
/// `private/buy`/`private/sell` on the resolved combo id — mixing them in here would register a
/// different combo per order size, which is not what a combo book is.
pub fn build_create_combo_params(legs: &[ComboLeg]) -> Value {
    let trades: Vec<Value> = legs
        .iter()
        .map(|leg| {
            json!({
                "instrument_name": leg.symbol,
                "amount": leg.ratio.unsigned_abs(),
                "direction": if leg.ratio > 0 { "buy" } else { "sell" },
            })
        })
        .collect();
    json!({ "trades": trades })
}

/// Parse a `private/create_combo` / `public/get_combo_details` result into a [`VenueCombo`].
/// `None` when the object has no `id`, no `legs` array, or ANY leg `amount` that does not decode
/// to an in-range integer multiplier — a bad amount fails the WHOLE parse loudly (the caller
/// rejects as [`ComboMapError::Unparseable`]) rather than coercing to a plausible wrong ratio and
/// leaning on a downstream zero-ratio guard.
pub fn parse_combo(result: &Value) -> Option<VenueCombo> {
    let id = result.get("id")?.as_str()?.to_string();
    if id.is_empty() {
        return None;
    }
    let legs = result
        .get("legs")?
        .as_array()?
        .iter()
        .map(|leg| {
            let symbol = leg.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("");
            let ratio = leg.get("amount").and_then(leg_ratio)?;
            Some(ComboLeg { symbol: symbol.to_string(), ratio })
        })
        .collect::<Option<Vec<_>>>()?;
    if legs.is_empty() {
        return None;
    }
    let state = result.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some(VenueCombo { id, state, legs })
}

/// Decode one `legs[].amount` — documented as an integer multiplier, but seen on the wire as an
/// integer, a float (`1.0` — a re-encoding venue/proxy), or a STRING (Deribit stringifies some
/// numerics). Anything else is `None`: a fractional float, an out-of-`i32`-range value (a
/// wrapping `as i32` would turn `2^32 + 1` into a plausible ratio 1), or a non-numeric string.
fn leg_ratio(v: &Value) -> Option<i32> {
    if let Some(i) = v.as_i64() {
        return i32::try_from(i).ok();
    }
    if let Some(f) = v.as_f64() {
        return f64_ratio(f);
    }
    v.as_str().and_then(str_ratio)
}

/// A string-encoded multiplier: integer form first, then the float form (`"1.0"`).
fn str_ratio(s: &str) -> Option<i32> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i64>() {
        return i32::try_from(i).ok();
    }
    s.parse::<f64>().ok().and_then(f64_ratio)
}

/// A float-encoded multiplier must be exactly integral and in `i32` range (NaN/±inf fail both
/// checks).
fn f64_ratio(f: f64) -> Option<i32> {
    (f.fract() == 0.0 && f >= f64::from(i32::MIN) && f <= f64::from(i32::MAX)).then_some(f as i32)
}

/// Solve for the [`ComboMapping`] between the requested spec legs and the venue's canonical combo
/// legs, or explain why none exists.
///
/// Exactness matters here, so the ratio law `venue_i · spec_0 == spec_i · venue_0` is checked in
/// INTEGER cross-multiplication (widened to `i64`) rather than by dividing floats — a
/// `2/3`-reduced combo must compare equal without a tolerance, and a near-miss must FAIL rather
/// than round into a false match. The solved `k` is returned gcd-reduced with `den > 0`, so
/// structurally identical mappings compare equal regardless of how the spec was scaled.
pub fn map_combo(spec: &[ComboLeg], venue: &[ComboLeg]) -> Result<ComboMapping, ComboMapError> {
    if spec.len() != venue.len() {
        return Err(ComboMapError::LegCountMismatch { spec: spec.len(), venue: venue.len() });
    }
    if spec.iter().chain(venue).any(|l| l.ratio == 0) {
        return Err(ComboMapError::ZeroRatio);
    }
    // Pair legs by instrument name (the venue is free to reorder them).
    let paired: Vec<(i64, i64)> = spec
        .iter()
        .map(|s| {
            venue
                .iter()
                .find(|v| v.symbol == s.symbol)
                .map(|v| (i64::from(s.ratio), i64::from(v.ratio)))
                .ok_or_else(|| ComboMapError::LegSymbolMismatch(s.symbol.clone()))
        })
        .collect::<Result<_, _>>()?;
    // A duplicate spec symbol would let two spec legs pair to the SAME venue leg and slip past the
    // length check; reject rather than mis-scale — naming the venue leg nothing paired to (the
    // actual offender), not venue[0] (usually a perfectly valid leg).
    if let Some(unmatched) = venue.iter().find(|v| !spec.iter().any(|s| s.symbol == v.symbol)) {
        return Err(ComboMapError::LegSymbolMismatch(unmatched.symbol.clone()));
    }
    // Two empty lists pass every check above; totality demands an error here, not `paired[0]`
    // panicking on the index.
    let Some(&(s0, v0)) = paired.first() else {
        return Err(ComboMapError::EmptyLegs);
    };
    if paired.iter().any(|&(si, vi)| vi * s0 != si * v0) {
        return Err(ComboMapError::RatioMismatch);
    }
    // Canonical reduced form of k = v0/s0: gcd-reduce and keep den positive (sign lives in num).
    let g = gcd(v0.unsigned_abs(), s0.unsigned_abs()) as i64; // >= 1: both ratios nonzero
    let (num, den) = if s0 < 0 { (-v0 / g, -s0 / g) } else { (v0 / g, s0 / g) };
    Ok(ComboMapping { num, den })
}

/// Plain Euclid on magnitudes; both inputs are nonzero at the one call site.
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Parse the combo instrument's OWN tick/step grid from a `public/get_instrument` result:
/// `(tick_size, min_trade_amount)`.
///
/// `None` unless BOTH axes are present and positive. A partial parse (one axis missing or zero)
/// must count as a FAILED fetch so the caller falls back to its documented leg-grid default —
/// accepting it would silently disable quantization on the zero axis (`format_to_step`'s
/// zero-step passthrough) while looking like a successfully fetched grid.
pub fn parse_combo_grid(result: &Value) -> Option<(f64, f64)> {
    let tick = result.get("tick_size").and_then(json_num).unwrap_or(0.0);
    let step = result.get("min_trade_amount").and_then(json_num).unwrap_or(0.0);
    (tick > 0.0 && step > 0.0).then_some((tick, step))
}

/// The resolved venue-side order: what `private/buy`|`private/sell` will be asked for.
///
/// Deliberately carries the SPEC-side qty/net plus the exact [`ComboMapping`] rather than
/// pre-scaled f64 venue values: the rational rescale is applied INSIDE the Decimal quantization
/// ([`build_combo_order_params`]), because an f64 venue value here would already have paid the
/// one-ULP tick tax the mapping exists to avoid (module doc, "Why `k` stays a rational").
#[derive(Debug, Clone, PartialEq)]
pub struct ComboOrder {
    /// the combo instrument name (`VenueCombo::id`) — substituted for the spec's EMPTY `symbol`
    pub instrument_name: String,
    /// ±1 after orientation; selects `private/buy` vs `private/sell`
    pub side: i32,
    /// SPEC qty (spec combo units); rescaled to venue units exactly, at quantization
    pub spec_qty: f64,
    /// SIGNED SPEC net limit per spec combo unit; `None` = a combo MARKET order. Rescaled to the
    /// venue net exactly, at quantization.
    pub spec_net: Option<f64>,
    /// the exact venue↔spec rational, applied at the wire site
    pub mapping: ComboMapping,
}

impl ComboOrder {
    /// f64 PREVIEW of the venue amount (logging/tests); the WIRE value is the exact rational,
    /// quantized in [`build_combo_order_params`].
    pub fn venue_qty(&self) -> f64 {
        self.mapping.venue_qty(self.spec_qty)
    }

    /// f64 PREVIEW of the venue net limit (logging/tests); SIGNED, like the wire value.
    pub fn venue_price(&self) -> Option<f64> {
        self.spec_net.map(|n| self.mapping.venue_price(n))
    }
}

/// Bind a [`ComboMapping`] to the spec's side/qty/net-limit, yielding the venue-side order. Only
/// the SIDE is resolved eagerly (it picks the RPC method); qty/net stay spec-side until the
/// Decimal wire site.
pub fn build_combo_order(
    combo_id: &str,
    mapping: ComboMapping,
    spec_side: i32,
    spec_qty: f64,
    spec_net: Option<f64>,
) -> ComboOrder {
    ComboOrder {
        instrument_name: combo_id.to_string(),
        side: mapping.venue_side(spec_side),
        spec_qty,
        spec_net,
        mapping,
    }
}

/// Render a [`ComboOrder`] as `private/buy`/`private/sell` params.
///
/// Quantization AND the venue rescale go through [`format_scaled_to_step_f`] — the exact-rational
/// sibling of the ONE pinned Decimal wire site (`format_to_step`, same module) — for BOTH amount
/// and price; no float formatting and no f64 rescale is hand-rolled here. The venue amount is
/// `spec_qty · |den/num|` and the venue net is `spec_net · num/den` (SIGNED), each computed in
/// Decimal so a non-dyadic scale (1/3) cannot land one ULP under a grid point and lose a full
/// tick to the round-down. `format_to_step` rounds toward zero and PRESERVES the sign (it even
/// keeps Python's negative zero), so a negative credit-combo net survives quantization as a
/// negative number.
///
/// `post_only` is forced FALSE for the same reason as the single-leg path (Deribit defaults it
/// true and would otherwise reprice/reject a marketable order), and `label` carries our
/// `client_order_id` so the fill stream's `label` still routes combo fills back to this order.
pub fn build_combo_order_params(
    order: &ComboOrder,
    client_order_id: &str,
    tick_size: f64,
    step_size: f64,
) -> Value {
    let m = order.mapping;
    // amount: magnitudes only — the SIDE carries direction.
    let amount = format_scaled_to_step_f(order.spec_qty, m.den.abs(), m.num.abs(), step_size)
        .parse::<f64>()
        .unwrap_or(0.0);
    let mut params = json!({
        "instrument_name": order.instrument_name,
        "amount": amount,
        "type": if order.spec_net.is_some() { "limit" } else { "market" },
        "label": client_order_id,
        "post_only": false,
    });
    if let Some(net) = order.spec_net {
        // SIGNED — `net · num/den`; a credit combo submits a negative limit verbatim.
        params["price"] = json!(
            format_scaled_to_step_f(net, m.num, m.den, tick_size)
                .parse::<f64>()
                .unwrap_or(f64::NAN)
        );
    }
    params
}

#[path = "combo_tests.rs"]
#[cfg(test)]
mod combo_tests;
