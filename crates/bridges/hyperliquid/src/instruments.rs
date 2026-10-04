//! Hyperliquid instrument load + cache — keyless `meta` (perp) + `spotMeta` (spot) → a
//! [`Symbology`] resolver plus per-instrument [`SymbolProperties`] (the order-placement grid).
//!
//! Fetched once at startup ([`HyperliquidInstruments::load`]) via [`HyperliquidTransport::info`], then
//! cached by unified symbol. HL has **no tick size** — the grid derives from `szDecimals` and the
//! per-product decimal ceiling (research §4): `tick_size = 10^-(MAX_DECIMALS − szDecimals)`,
//! `step_size = 10^-szDecimals`, where `MAX_DECIMALS` = 6 (perp) / 8 (spot). Min notional is a flat
//! `$10` ([`crate::consts::MIN_NOTIONAL_USD`]); `min_qty` = one size step; `max_qty` is a large finite
//! sentinel (HL publishes no per-instrument max size — position caps are dynamic/open-interest-driven).
//! Perp `max_leverage` is NOT a [`SymbolProperties`] field — it stays on the [`InstrumentRef`],
//! reachable through [`HyperliquidInstruments::symbology`]. [`InstrumentRef::only_isolated`]
//! (`meta.universe[].onlyIsolated`) rides there for the same reason and is likewise absent here:
//! [`SymbolProperties`] is the venue-neutral ROUNDING/placement grid (tick/step/min-qty/min-notional),
//! and a per-asset MARGIN-MODE restriction is not a grid fact. It cannot live in
//! [`vike_model::venues::venue_margin_support::VenueMarginSupport`] either — that table is per-VENUE, and
//! hyperliquid is the live counterexample to venue uniformity on this axis (9 of 232 core perps are
//! isolated-only, measured 2026-08-05). Parsed and exposed, NOT enforced: see the field's own doc
//! for why `venue_caps`' cross-only `margin_modes` row is what actually gates the mode today.
//!
//! Optional [`PropertiesRecorder`] threading (`VIKE_RECORD_PROPERTIES=1`, OFF by default), matching
//! the crypto-venue convention (Bybit/OKX record the REAL fetched grid at instrument-fetch time):
//! [`HyperliquidInstruments::load_with_recorder`] records the whole derived grid when a recorder is
//! passed. Feeds the catalog, exec (rounding grid), and `RiskLimits`.
//!
//! **HIP-3 builder-deployed perp dexs** are an OPT-IN universe expansion (`flags.hyperliquid_hip3`,
//! folded under [`crate::consts::HIP3_ENV`]; OFF by default). Off ⇒ the load fetches ONLY `meta` +
//! `spotMeta`, byte-identical to the pre-HIP-3 universe. On ⇒ the load ALSO fetches `perpDexs`
//! and, per builder dex, its `meta` (with the `dex` param), folding those markets in with the HIP-3
//! asset-id offset schema (see [`Symbology::extend_with_perp_dex`]). The extra fetches are
//! best-effort — a `perpDexs` / per-dex-`meta` failure logs and is skipped, never failing the core
//! universe load. Because exec places orders off `InstrumentRef::asset_id` verbatim, a folded HIP-3
//! market is order-ready with no change to the exec/signing wire.

use indexmap::IndexMap;

use vike_bridge_core::transport::VenueApiError;
use vike_data::PropertiesRecorder;
use vike_model::{AssetClass, SymbolProperties};

use crate::config::Product;
use crate::consts::{HIP3_ENV, MIN_NOTIONAL_USD, PERP_MAX_DECIMALS, SPOT_MAX_DECIMALS, VENUE};
use crate::symbology::{InstrumentRef, Symbology, parse_perp_dexs};
use crate::transport::HyperliquidTransport;

/// A large but finite `max_qty` sentinel. HL exposes no per-instrument maximum order size (position
/// caps are dynamic, open-interest-driven), so the instrument grid is effectively size-unbounded; a
/// finite value keeps any downstream notional arithmetic overflow-free while never rejecting a real
/// order. (`RiskLimits::from_properties` does not even read `max_qty`, so this is a chart/catalog
/// convenience, not a live cap.)
const MAX_QTY: f64 = 1e12;

/// Loaded + cached Hyperliquid instrument universe: the [`Symbology`] index (by symbol / by coin /
/// symbol→asset-id) plus the derived [`SymbolProperties`] grid, keyed by unified symbol.
pub struct HyperliquidInstruments {
    symbology: Symbology,
    /// Insertion-ordered (perps then spot, mirroring [`Symbology`]) so iteration is deterministic.
    properties: IndexMap<String, SymbolProperties>,
}

impl HyperliquidInstruments {
    /// Fetch `meta` + `spotMeta` and build the resolver + properties grid. Two keyless `/info`
    /// reads, plus the HIP-3 builder dexs when `hip3` — the caller's resolved
    /// `flags.hyperliquid_hip3`.
    pub fn load(transport: &HyperliquidTransport, hip3: bool) -> Result<Self, VenueApiError> {
        Self::load_with_recorder(transport, None, hip3)
    }

    /// [`Self::load`] over a CALLER-SUPPLIED map — the shape a settings row can reach.
    ///
    /// Where HIP-3 comes from: `flags.hyperliquid_hip3`, folded into `vars` under
    /// [`crate::consts::HIP3_ENV`] by the composition root (`FoldTier::Resolved`). No environment
    /// is read (decision 0095). A caller whose map holds no such key gets the core universe — the
    /// same as [`Self::load`] with `hip3 = false`.
    pub fn load_from_vars(
        transport: &HyperliquidTransport,
        recorder: Option<&PropertiesRecorder>,
        vars: &std::collections::HashMap<String, String>,
    ) -> Result<Self, VenueApiError> {
        let hip3 = hip3_flag(vars.get(HIP3_ENV).map(String::as_str));
        Self::load_from(|body: &serde_json::Value| transport.info(body), recorder, hip3)
    }

    /// [`Self::load`] plus an opt-in [`PropertiesRecorder`]: when present, the whole freshly-derived
    /// grid is recorded into the PIT properties store (best-effort — the recorder no-ops when disabled
    /// and swallows store errors). Mirrors the Bybit/OKX `spawn_with_recorder` convention.
    ///
    /// HIP-3 builder-deployed perp dexs are folded in ONLY when `hip3` (see the module doc); OFF ⇒
    /// exactly the two `meta` + `spotMeta` reads as before, byte-identical.
    pub fn load_with_recorder(
        transport: &HyperliquidTransport,
        recorder: Option<&PropertiesRecorder>,
        hip3: bool,
    ) -> Result<Self, VenueApiError> {
        Self::load_from(|body: &serde_json::Value| transport.info(body), recorder, hip3)
    }

    /// The load orchestration over an `/info` fetch seam (so the OFF/ON request set is fixture-
    /// testable with NO network). ALWAYS fetches `meta` then `spotMeta`; when `hip3` is set,
    /// ADDITIONALLY fetches `perpDexs` and each builder dex's `meta` (with the `dex` param) and folds
    /// them in — best-effort (a HIP-3 fetch failure logs and is skipped, never failing the core
    /// universe). Only a core `meta`/`spotMeta` error propagates, so with `hip3 = false` this is
    /// byte-identical to the pre-HIP-3 loader. `pub(crate)` for `crate::catalog`'s
    /// `HyperliquidCatalog::list_via`, which threads the same seam.
    pub(crate) fn load_from<F>(
        fetch: F,
        recorder: Option<&PropertiesRecorder>,
        hip3: bool,
    ) -> Result<Self, VenueApiError>
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, VenueApiError>,
    {
        let meta = fetch(&serde_json::json!({ "type": "meta" }))?;
        let spot_meta = fetch(&serde_json::json!({ "type": "spotMeta" }))?;
        let mut symbology = Symbology::from_meta(&meta, &spot_meta);
        if hip3 {
            fold_perp_dexs(&fetch, &mut symbology);
        }
        Ok(Self::from_symbology(symbology, recorder))
    }

    /// Derive the [`SymbolProperties`] grid from a resolved [`Symbology`] and (optionally) record it
    /// — the shared tail of the loader and the test `build`. Records ONE row per symbol, stamped
    /// with the fetch wall-clock; idempotent per day in the store.
    fn from_symbology(symbology: Symbology, recorder: Option<&PropertiesRecorder>) -> Self {
        let mut properties = IndexMap::with_capacity(symbology.len());
        for inst in symbology.iter() {
            properties.insert(inst.symbol.clone(), properties_for(inst));
        }
        if let Some(rec) = recorder {
            rec.record_all(
                VENUE,
                properties.iter().map(|(sym, props)| (sym.clone(), *props)),
                vike_model::now_ns(),
            );
        }
        HyperliquidInstruments { symbology, properties }
    }

    /// Pure builder over already-fetched core `meta`/`spotMeta` bodies — the fixture seam (no
    /// network, no HIP-3). Records the derived grid iff a recorder is supplied. `pub(crate)` so
    /// `crate::mount`'s tests read a mount's facts off the same universe shape.
    #[cfg(test)]
    pub(crate) fn build(
        meta: &serde_json::Value,
        spot_meta: &serde_json::Value,
        recorder: Option<&PropertiesRecorder>,
    ) -> Self {
        Self::from_symbology(Symbology::from_meta(meta, spot_meta), recorder)
    }

    /// The symbology resolver (coin ⇄ symbol, symbol → asset-id, perp `max_leverage`).
    pub fn symbology(&self) -> &Symbology {
        &self.symbology
    }

    /// The order-placement grid for a unified symbol (perp `"BTC"` / spot `"BASE/QUOTE"`).
    pub fn properties(&self, symbol: &str) -> Option<&SymbolProperties> {
        self.properties.get(symbol)
    }

    /// Number of instruments with a derived grid.
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Iterate `(symbol, properties)` in insertion order (perps then spot).
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SymbolProperties)> {
        self.properties.iter().map(|(sym, props)| (sym.as_str(), props))
    }
}

/// Derive the order-placement grid for one resolved instrument (research §4). `max_leverage` is not
/// a [`SymbolProperties`] field and is intentionally not carried here (it lives on the [`InstrumentRef`]).
fn properties_for(inst: &InstrumentRef) -> SymbolProperties {
    let max_decimals = match inst.product {
        Product::Perp => PERP_MAX_DECIMALS,
        Product::Spot => SPOT_MAX_DECIMALS,
    };
    // szDecimals never exceeds MAX_DECIMALS on HL, so the price-decimal count is ≥ 0; saturate to be
    // safe against malformed metadata (a floor of 0 decimals → tick_size 1.0).
    let price_decimals = max_decimals.saturating_sub(inst.sz_decimals);
    let step_size = pow10_neg(inst.sz_decimals);
    SymbolProperties {
        tick_size: pow10_neg(price_decimals),
        step_size,
        min_qty: step_size,
        max_qty: MAX_QTY,
        min_notional: MIN_NOTIONAL_USD,
        // The class comes from the venue's OWN split of its metadata endpoint (`Product`, resolved
        // from `meta` vs `spotMeta`) — never from the symbol text, which is the implicit encoding
        // `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove. A unified HL perp
        // symbol is a bare base ("BTC") and would be indistinguishable from a spot base by string.
        asset_class: Some(match inst.product {
            Product::Perp => AssetClass::CryptoPerp,
            Product::Spot => AssetClass::CryptoSpot,
        }),
        // Everything else absent: HL perps and spot both size in the base asset (no contract
        // multiplier, = 1.0), the tick is one szDecimals-derived power of ten (flat grid, no
        // tiers), and HL declares no venue taker hold. FRU rather than an exhaustive literal so a
        // new `SymbolProperties` field costs this parser nothing.
        ..Default::default()
    }
}

/// `10^-n` as an f64 — the tick and the step of every grid [`properties_for`] builds, where `n` is
/// the venue's `szDecimals`-derived decimal count (≤ 8).
///
/// ⚠ **`pub(crate)` for ONE second caller, and the reason is that there must be exactly one
/// spelling of this f64.** `crates/bridges/hyperliquid/src/px.rs`'s `grid_multiple_image` asks "is
/// this size the f64 image of a whole number of lots", a question that is only answerable against
/// the SAME step the lot grid was built from — the one this function writes into
/// `SymbolProperties::step_size` and `crates/vike-exec/src/risk.rs`'s `RiskGate` then multiplies by
/// on its way out. A second, independently-derived spelling of `10⁻ⁿ` that ever disagreed by a bit
/// would make that witness decline precisely where it is needed, so it calls this rather than
/// re-deriving one.
///
/// ⚠ **MEASURED 2026-08-26: this call is BIT-IDENTICAL on Windows/MSVC `dev` and Linux/glibc `dev`,
/// and the spelling stands because it is portable — not, as this comment claimed until 2026-08-28,
/// because a divergence was suspected and left unmeasured.**
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` carries the
/// hashes (`pow10_neg` → `0dc16eac9344618c` on both boxes) and the sweep behind them: `10f64.powi(n)`
/// against the exact power of ten for `n in -22..=22`, `0` mismatches of 23 on each platform.
///
/// **Why it is portable, stated as the property rather than as the observation.** A power of ten is
/// EXACTLY representable in `f64` for `|n| <= 22`, so `10ⁿ` lands on the exact value however `powi`
/// is lowered, and `10⁻ⁿ` is then one correctly-rounded division of an exact numerator by an exact
/// denominator — a division IEEE 754 DOES require to be correctly rounded. Nothing here depends on
/// which lowering the compiler picked. The venue's `szDecimals`-derived `n` is `≤ 8`, an order of
/// magnitude inside that domain, so the bound is not close to being a constraint on this site.
///
/// ⚠ **The two claims this comment carried before, kept because each was wrong in an instructive
/// way.** First it ended "so `powi` is exact-enough and cheap" — an unexamined intuition, and the
/// exact assumption ADR 0032 was amended to refute, because on MSVC in a `dev` build LLVM does not
/// expand `llvm.powi` into multiplications at all: it emits the CRT's `pow()`, a transcendental
/// IEEE 754 does not require to be correctly rounded. That correction was right about the LOWERING
/// and it then overshot into a second wrong claim: that this call "can hand back a
/// last-bit-different f64 on the Windows dev box", with the site "left unconverted PENDING" a
/// measurement of whether the step-size rounding downstream absorbs the difference. The measurement
/// was taken, and there is no difference for anything downstream to absorb — the lowering being
/// non-portable does not make its RESULT non-portable when every input is exact. Classify a `powi`
/// by its BASE, which is the rule ADR 0032 ended up with: a literal `10` or `2` needs nothing, an
/// arbitrary runtime `f64` base needs `libm::pow`.
pub(crate) fn pow10_neg(n: u32) -> f64 {
    10f64.powi(-(n as i32))
}

/// The HIP-3 enumeration opt-in's pure gate: the EXACT string `"1"` (the reconcile-gate idiom — not
/// a fuzzy truthy parse); absent or anything else ⇒ OFF ⇒ core-only. Applied to the value the
/// caller's map folds under [`HIP3_ENV`]. (`hip3_enabled`, the process-environment read of the same
/// name that stood beside it, was deleted by decision 0095; the flag's row is the only source.)
fn hip3_flag(v: Option<&str>) -> bool {
    v == Some("1")
}

/// Fetch `perpDexs`, then each builder dex's `meta` (with the `dex` param), folding every returned
/// market into `symbology` with the HIP-3 asset-id offset schema. BEST-EFFORT: a `perpDexs` failure
/// leaves the core universe intact; a single per-dex `meta` failure skips only that dex. `fetch` is
/// the same `/info` seam [`HyperliquidInstruments::load_from`] threads through.
fn fold_perp_dexs<F>(fetch: &F, symbology: &mut Symbology)
where
    F: Fn(&serde_json::Value) -> Result<serde_json::Value, VenueApiError>,
{
    let perp_dexs = match fetch(&serde_json::json!({ "type": "perpDexs" })) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "vike_hyperliquid",
                error = %e,
                "HIP-3 perpDexs fetch failed; continuing with the core universe only"
            );
            return;
        }
    };
    let dexs = parse_perp_dexs(&perp_dexs);
    tracing::info!(
        target: "vike_hyperliquid",
        count = dexs.len(),
        "HIP-3 enabled: folding builder-deployed perp dexs into the universe"
    );
    for dex in &dexs {
        match fetch(&serde_json::json!({ "type": "meta", "dex": dex.name.clone() })) {
            Ok(dex_meta) => symbology.extend_with_perp_dex(&dex_meta, dex.index, &dex.name),
            Err(e) => tracing::warn!(
                target: "vike_hyperliquid",
                dex = %dex.name, error = %e,
                "HIP-3 per-dex meta fetch failed; skipping this dex"
            ),
        }
    }
}

#[path = "instruments_tests.rs"]
#[cfg(test)]
mod instruments_tests;
