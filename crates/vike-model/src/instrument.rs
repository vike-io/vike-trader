//! Venue-neutral instrument metadata — the per-symbol order-placement bounds every venue
//! REST parser fills from its own instrument endpoint (Binance /exchangeInfo filters,
//! Bybit lotSizeFilter, OKX tickSz/lotSz, Deribit min_trade_amount). Field shape is the
//! twin of `data/instrument_db.py::parse_symbol_properties` (floats, absent properties = 0.0).
//!
//! Lives in vike-model (not the per-venue bridge crates) because it is pure domain data consumed
//! on the venue-neutral side of the seam; the per-venue parse functions stay with their venues.
//! This is the seed of the fuller `data/instruments.py::InstrumentSpec` port (pip size,
//! contract size, ccys, price decimals) when the instrument catalog lands.

use crate::asset_class::AssetClass;
use crate::tick_scheme::{TickScheme, round_price_tiered};

/// Per-symbol order-placement bounds. Absent venue properties stay `0.0` (Python
/// `float(x or 0)`), so `0.0` means "venue did not constrain this".
///
/// ## ⚠ Construct this with functional-update syntax, not an exhaustive literal
///
/// ```ignore
/// SymbolProperties { tick_size, step_size, ..Default::default() }   // yes
/// SymbolProperties { tick_size, step_size, min_qty: 0.0, /* …every field… */ }  // no
/// ```
///
/// This struct GROWS — `contract_size`, `tick_scheme`, `taker_hold_ms` and `asset_class` each
/// arrived after the original five — and every field is `Default`-zero-or-`None` because the whole
/// convention is absent-is-`0`. An exhaustive literal therefore turns each addition into a
/// compile error at every construction site in the workspace: `taker_hold_ms` alone broke
/// **14 crates / ~32 sites**, each needing the same mechanical `taker_hold_ms: 0`.
/// Functional-update syntax makes a new field cost only the crates that actually populate or
/// persist it. (`asset_class` cost 17 sites in two crates, and all 17 were the two families below
/// — the convention held everywhere else.)
///
/// Two families of site deliberately KEEP the exhaustive literal, and must:
///
/// * the vike-data `kind=properties` Parquet codec (`datafusion_hist::codec`) — its `decode_row`
///   and its round-trip / old-part-decode pins. A `SymbolProperties` field with no codec column is
///   SILENTLY DROPPED on a store round-trip (see [`SymbolProperties::tick_scheme`], which sits in
///   exactly that trap), so the compile error there is the feature: it forces the column decision
///   at the one place that can make it;
/// * this module's own serde round-trip / legacy-shape tests, for the same reason on the JSON side.
///
/// `#[non_exhaustive]` was CONSIDERED as a way to enforce the convention and rejected: it forbids
/// cross-crate struct literals **including** functional update, so every bridge parser would have
/// to become `let mut p = SymbolProperties::default(); p.tick_size = …;` or a hand-written builder
/// — a bigger and uglier churn than the one it prevents, and it would ALSO disable the two
/// exhaustive families above, which is precisely where the compile error is wanted. A convention
/// plus this note is the right size for a workspace-internal (`publish = false`) type.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SymbolProperties {
    pub tick_size: f64,
    pub step_size: f64,
    pub min_qty: f64,
    pub max_qty: f64,
    pub min_notional: f64,
    /// Contract size / notional multiplier — how much underlying ONE unit of `qty` controls.
    /// Options and inverse perps quote a size that is NOT 1:1 with notional (a Deribit BTC option
    /// is 1 BTC per contract, an ETH option 1 ETH), so true notional is `|qty| * |price| *
    /// contract_size`. Spot and linear FX/crypto leave this unset.
    ///
    /// Follows the struct's absent-is-`0.0` convention rather than defaulting to `1.0`: that keeps
    /// `Default`/`..Default::default()` and every existing venue parse byte-identical, and keeps
    /// "the venue did not tell us" distinguishable from "the venue said 1.0". Consumers that need a
    /// multiplier call [`SymbolProperties::multiplier`], which folds absent to the inert `1.0`.
    ///
    /// `#[serde(default)]` is a compatibility pin, NOT decoration: this struct is persisted in the
    /// `kind=properties` PIT series, so rows recorded before this field existed must keep
    /// deserializing. Do not remove it.
    #[serde(default)]
    pub contract_size: f64,
    /// OPTIONAL tiered price grid — the venue's minimum price increment as a FUNCTION of the price
    /// (see [`crate::tick_scheme`]). `None` — every venue but a Deribit option today — means the
    /// scalar [`SymbolProperties::tick_size`] above IS the whole grid, exactly as before.
    ///
    /// Deribit's `public/get_instrument` returns `tick_size` PLUS `tick_size_steps`
    /// (`0.0005` above `0.005` for BTC options); a limit price above that boundary snapped onto the
    /// BASE grid is venue-rejected, which is why the live options smoke deliberately rests below
    /// the boundary. Carrying the steps here is what lets a rounding site ask for the tick AT a
    /// price ([`SymbolProperties::effective_tick`]) instead of assuming one.
    ///
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` is a compatibility pin, NOT
    /// decoration — the same contract `contract_size` carries: this struct is persisted in the
    /// `kind=properties` PIT series, so rows written before this field existed must keep
    /// deserializing, AND a tier-less value must re-serialize byte-identically to a world without
    /// the field (the key is omitted entirely). Both halves are pinned by tests below. Do not
    /// remove either attribute.
    ///
    /// ⚠ PERSISTENCE ASYMMETRY — deliberate, scoped, and a HARD PREREQUISITE for the follow-up:
    /// the serde (JSON) shape carries this field, but the columnar Parquet `kind=properties` codec
    /// in vike-data (`datafusion_hist::codec.rs`, the `series_codec!` for the properties series)
    /// has NO column for it. Its `decode_row` therefore rebuilds every stored row with
    /// `tick_scheme: None`, so a populated scheme is SILENTLY DROPPED on a store
    /// round-trip — no error, no warning, just a flat grid coming back out. That is inert TODAY
    /// (nothing in the tree constructs a `TickScheme`, so every written row genuinely is `None`),
    /// which is exactly why it is safe to land the type first.
    ///
    /// Consequence for the venue work: ADDING THAT CODEC COLUMN IS A PREREQUISITE OF, NOT A PEER
    /// TASK TO, the first venue parser that populates `tick_scheme` (Deribit's `tick_size_steps`).
    /// The column must land — additive + nullable, the same shape `contract_size` uses so old parts
    /// still decode — BEFORE any parser starts emitting schemes, or the PIT properties series
    /// quietly loses them and a replay/backtest reading `properties_as_of` rounds on a wrong grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_scheme: Option<TickScheme>,
    /// The venue's own **enforced hold** on an incoming order for this market, in milliseconds —
    /// how long the EXCHANGE deliberately sits on the order before matching or booking it. `0`
    /// (the struct's absent convention, and every venue but Polymarket today) means "no venue
    /// hold", which is byte-identical to a world without this field.
    ///
    /// This is NOT a network estimate, NOT a tunable, and NOT the same thing as latency: it is a
    /// property the venue DECLARES per market, so it is point-in-time data recorded on the
    /// `kind=properties` tape beside the price/lot grid, and a replay reads it back with
    /// `properties_as_of`. A backtest consumes it as extra ENTRY-leg delay
    /// (`vike_sim::LatencyModelKind`), which is why it is measured in the same units the venue states
    /// it in rather than nanoseconds.
    ///
    /// **Polymarket declares it through TWO SEPARATE, INDEPENDENT mechanisms** — do not conflate
    /// them (measured live 2026-07-23 against the production CLOB/Gamma APIs):
    ///
    /// 1. **`itode` → 250 ms**, on the CRYPTO/finance up/down markets. The flag lives ONLY on
    ///    `GET https://clob.polymarket.com/clob-markets/{condition_id}` (a terse-keyed payload,
    ///    `{r,t,c,mos,mts,mbf,tbf,ao,aot,itode,fd}`) — it is NOT on `/markets/{condition_id}` and
    ///    NOT on Gamma, so probing those makes crypto look delay-free. Verified `itode: true` on
    ///    4/4 live btc/eth/sol/xrp `updown-5m` markets; live since 2026-06-05. Semantics
    ///    (docs.polymarket.com/concepts/order-lifecycle): the order is held 250 ms, a CANCEL IS
    ///    REJECTED while it is pending, it survives a dropped connection, and it is then
    ///    re-validated and either matched or placed on the book.
    /// 2. **`seconds_delay` → 3 s (3000 ms)**, on SPORTS **game** markets. This field is on the
    ///    CLOB `/markets/{condition_id}` payload and on Gamma (`secondsDelay`). Measured over
    ///    every open sports market: **400 of 400 markets carrying a `game_start_time` report
    ///    `seconds_delay: 3`**, while 191 of 192 futures/props report `0` — the discriminator is
    ///    exactly "has a game start". NBA (370) plus EPL/NFL/CBB/IPL. Politics: 0 of 30. Crypto
    ///    reports `seconds_delay: 0` — its delay is `itode` instead.
    ///
    /// `#[serde(default)]` is a compatibility pin, NOT decoration — the same contract
    /// `contract_size` carries: this struct is persisted in the `kind=properties` PIT series, so
    /// rows recorded before this field existed must keep deserializing. Do not remove it. The
    /// columnar Parquet twin of that pin is the codec column in
    /// `vike-data`'s `datafusion_hist::codec` (see [`SymbolProperties::tick_scheme`]'s doc for
    /// what happens to a struct field with no column — it is silently dropped; this field HAS its
    /// column).
    #[serde(default)]
    pub taker_hold_ms: u32,
    /// What KIND of instrument this grid belongs to — spot, perp, option, FX, … — as the closed
    /// [`AssetClass`] vocabulary. `None` means **nobody has said**, and that is a different claim
    /// from any variant.
    ///
    /// This is the STORE's half of `docs/decisions/0061-an-instrument-names-its-kind.md`'s step 2
    /// — that record's step 2 is "fill the class in, per venue", and this is the carrier it fills
    /// INTO, so the two land together rather than a vocabulary arriving with nowhere to put a
    /// value. The defect being closed is the one 0061 measured: an instrument's
    /// kind was being re-derived, differently, at every site that needed it, and 0061's cure is to
    /// carry it as data from the one place it is known for free — the venue fetch that asked a
    /// PERP endpoint for a PERP grid. Because this struct is the PIT `kind=properties` tape, a
    /// class recorded here is a class a replay, a backtest and a store consumer all read back
    /// instead of guessing.
    ///
    /// ⚠ **`Option`, not a defaulted variant, and the absence is the point.** Every row already on
    /// disk was written before this field existed and names no class; a `Default` of (say)
    /// `CryptoSpot` would turn "we never asked" into a positive claim about every historical
    /// partition, and a consumer cannot tell a guess from a fetch once it is stored. `None` is the
    /// only honest value for those rows, so the type has to be able to hold it. It is also why
    /// there is no `unwrap_or` helper here in the shape of [`SymbolProperties::multiplier`]: an
    /// absent multiplier has an inert identity (`1.0`) and an absent CLASS does not.
    ///
    /// ⚠ **Do not fill this by reading the symbol string.** A `.P` suffix, a `/`, a `-PERPETUAL`
    /// tail — inferring the class from the ticker is exactly the implicit encoding 0061 exists to
    /// REMOVE, and a parser that does it has thrown away the venue's own answer to substitute its
    /// own. Fill it where the producer genuinely knows (the endpoint it called, the `instType` the
    /// venue returned) and leave it `None`, with a comment, where it does not.
    ///
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` is a compatibility pin, NOT
    /// decoration — the same contract [`SymbolProperties::tick_scheme`] carries, and for both its
    /// reasons: rows recorded before this field existed must keep deserializing, AND a class-less
    /// value must re-serialize byte-identically to a world without the field (the key is omitted
    /// entirely). Both halves are pinned by tests below. Do not remove either attribute.
    ///
    /// The columnar twin of that pin is the `asset_class` column in `vike-data`'s
    /// `datafusion_hist::codec` — present since this field was, so this field is NOT in the
    /// silently-dropped trap [`SymbolProperties::tick_scheme`]'s doc describes: it is stored as the
    /// variant's own [`AssetClass::sql_word`] (which is its serde word), in a nullable Utf8 column
    /// appended last, so an OLD part still decodes and a class-less row stays a SQL NULL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<AssetClass>,
}

impl SymbolProperties {
    /// The notional multiplier to fold into `|qty| * |price|`, with the absent/degenerate grid
    /// folded to the inert `1.0`. This is the ONE place absent-contract-size becomes `1.0`, so a
    /// venue that reports no contract size (spot crypto, FX) stays byte-identical to a world
    /// without the field at all.
    ///
    /// Non-finite and non-positive values are also treated as absent — a venue returning `0.0`,
    /// `NaN`, or a negative contract size must never silently zero out or invert a notional cap.
    pub fn multiplier(&self) -> f64 {
        if self.contract_size.is_finite() && self.contract_size > 0.0 {
            self.contract_size
        } else {
            1.0
        }
    }

    /// The minimum price increment IN FORCE AT `price`. With no [`SymbolProperties::tick_scheme`]
    /// this is the scalar [`SymbolProperties::tick_size`] verbatim — including its absent-is-`0.0`
    /// convention, which the rounding sites already read as UNCONSTRAINED
    /// ([`crate::scalar::nz_step`]). With a scheme it is the tier's tick (see
    /// [`crate::tick_scheme::TickScheme::tick_at`] for the boundary rule) and the scalar
    /// `tick_size` is NOT consulted at all — which is why [`SymbolProperties::with_tick_scheme`]
    /// requires the scheme's base tick to equal it.
    #[inline]
    pub fn effective_tick(&self, price: f64) -> f64 {
        match &self.tick_scheme {
            Some(s) => s.tick_at(price),
            None => self.tick_size,
        }
    }

    /// Snap `price` onto this instrument's grid — the TIER-AWARE rounder.
    ///
    /// With no scheme this is EXACTLY `round_to(price, nz_step(self.tick_size))`, the expression
    /// every price-rounding site runs today, so a tier-less instrument is byte-identical.
    /// [`crate::scalar::round_to`]/[`crate::scalar::round_to_step`] themselves are untouched.
    #[inline]
    pub fn round_price(&self, price: f64) -> f64 {
        round_price_tiered(price, self.tick_scheme.as_ref(), self.tick_size)
    }

    /// Attach a tiered grid, builder-style. `SymbolProperties` is `Copy`, so this consumes and
    /// returns a value — it exists so a venue parser can add a scheme without restating the whole
    /// struct literal.
    ///
    /// ⚠ CALLER INVARIANT: the scheme's [`TickScheme::base_tick`] MUST equal `self.tick_size`.
    /// Once a scheme is attached, [`SymbolProperties::effective_tick`] and
    /// [`SymbolProperties::round_price`] read the SCHEME ONLY — the scalar `tick_size` field stops
    /// participating in price rounding entirely. Nothing in the type ties the two together, so a
    /// venue parser that builds the scheme from `tick_size_steps` but forgets to seed its base tick
    /// from the same `tick_size` gets a silently wrong grid (wrong-tick prices on the wire, venue
    /// rejects), not a compile error. The `debug_assert` below catches that in tests/debug builds;
    /// a release build trusts the caller (this is on the order path).
    ///
    /// Consumers that still read the scalar for non-price purposes (the RiskGate's `tick_size`
    /// limit, the PIT properties tape) keep seeing the base tick, which is the other half of why
    /// the two must agree.
    #[inline]
    pub fn with_tick_scheme(mut self, scheme: TickScheme) -> Self {
        debug_assert!(
            scheme.base_tick() == self.tick_size,
            "tick_scheme base_tick must equal SymbolProperties::tick_size"
        );
        self.tick_scheme = Some(scheme);
        self
    }
}

#[path = "instrument_tests.rs"]
#[cfg(test)]
mod instrument_tests;
