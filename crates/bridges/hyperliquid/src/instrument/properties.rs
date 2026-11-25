//! Hyperliquid instrument load + cache — keyless `meta` (perp) + `spotMeta` (spot) → a
//! [`Symbology`] resolver plus per-instrument [`SymbolProperties`] (the order-placement grid).
//!
//! Fetched once at startup ([`HyperliquidInstruments::load`]) via [`HyperliquidTransport::info`], then
//! cached by USER name (`BTCUSDC.P`, `HYPEUSDC`; decision 0290, [`Symbology`]'s module doc). HL has **no tick size** — the grid derives from `szDecimals` and the
//! per-product decimal ceiling (research §4): `tick_size = 10^-(MAX_DECIMALS − szDecimals)`,
//! `step_size = 10^-szDecimals`, where `MAX_DECIMALS` = 6 (perp) / 8 (spot). Min notional is a flat
//! `$10` ([`crate::transport::consts::MIN_NOTIONAL_USD`]); `min_qty` = one size step; `max_qty` is a large finite
//! sentinel (HL publishes no per-instrument max size — position caps are dynamic/open-interest-driven).
//! Perp `max_leverage` is NOT a [`SymbolProperties`] field — it stays on the [`InstrumentRef`],
//! reachable through [`HyperliquidInstruments::symbology`]. [`InstrumentRef::only_isolated`]
//! (`meta.universe[].onlyIsolated`) rides there for the same reason and is likewise absent here:
//! [`SymbolProperties`] is the venue-neutral ROUNDING/placement grid (tick/step/min-qty/min-notional),
//! and a per-asset MARGIN-MODE restriction is not a grid fact. It cannot live in
//! [`vike_venues::venue_margin_support::VenueMarginSupport`] either — that table is per-VENUE, and
//! hyperliquid is the live counterexample to venue uniformity on this axis (9 of 232 core perps are
//! isolated-only, measured 2026-08-05). Parsed and exposed, NOT enforced: see the field's own doc
//! for why `venue_caps`' cross-only `margin_modes` row is what actually gates the mode today.
//!
//! Optional [`PropertiesRecorder`] threading (no root passes one: `flags.record_properties` arms only
//! the datahub's tape), matching
//! the crypto-venue convention (Bybit/OKX record the REAL fetched grid at instrument-fetch time):
//! [`HyperliquidInstruments::load_with_recorder`] records the whole derived grid when a recorder is
//! passed. Feeds the catalog, exec (rounding grid), and `RiskLimits`.
//!
//! **HIP-3 builder-deployed perp dexs** are an OPT-IN universe expansion (`flags.hyperliquid_hip3`, which the
//! composition root resolves and hands the start as a bool, `AdapterInputs::flags`; OFF by default). Off ⇒ the load fetches ONLY `meta` +
//! `spotMeta`, byte-identical to the pre-HIP-3 universe. On ⇒ the load ALSO fetches `perpDexs`
//! and, per builder dex, its `meta` (with the `dex` param), folding those markets in with the HIP-3
//! asset-id offset schema (see [`Symbology::extend_with_perp_dex`]). The extra fetches are
//! best-effort — a `perpDexs` / per-dex-`meta` failure logs and is skipped, never failing the core
//! universe load. Because exec places orders off `InstrumentRef::asset_id` verbatim, a folded HIP-3
//! market is order-ready with no change to the exec/signing wire.

use std::sync::{Arc, PoisonError, RwLock};

use indexmap::IndexMap;

use vike_bridge_core::transport::VenueApiError;
use vike_data::PropertiesRecorder;
use vike_model::{AssetClass, SymbolProperties};

use crate::config::unlisted_class;
use crate::instrument::symbology::{InstrumentRef, Symbology, parse_perp_dexs};
use crate::transport::HyperliquidTransport;
use crate::transport::consts::{MIN_NOTIONAL_USD, PERP_MAX_DECIMALS, SPOT_MAX_DECIMALS, VENUE};

/// A large but finite `max_qty` sentinel. HL exposes no per-instrument maximum order size (position
/// caps are dynamic, open-interest-driven), so the instrument grid is effectively size-unbounded; a
/// finite value keeps any downstream notional arithmetic overflow-free while never rejecting a real
/// order. (`RiskLimits::from_properties` does not even read `max_qty`, so this is a chart/catalog
/// convenience, not a live cap.)
const MAX_QUANTITY: f64 = 1e12;

/// Loaded + cached Hyperliquid instrument universe: the [`Symbology`] index (by symbol / by coin /
/// symbol→asset-id) plus the derived [`SymbolProperties`] grid, keyed by unified symbol.
pub struct HyperliquidInstruments {
    symbology: Symbology,
    /// Insertion-ordered (perps then spot, mirroring [`Symbology`]) so iteration is deterministic.
    properties: IndexMap<String, SymbolProperties>,
    /// Whether the HIP-3 builder dexs were folded in: a RELOAD of this universe asks for the same one
    /// ([`Self::hip3`]), so a coin listed later arrives under the flag the start was made with.
    hip3: bool,
    /// The builder dexes folded in ON DEMAND although the flag is off (decision 0218): the dex of a coin an
    /// order named and the start's universe lacked. A reload folds the same ones again (a universe only GROWS), so
    /// a second coin's reload does not drop the first's. Empty for a universe that folded none, and irrelevant
    /// when [`Self::hip3`] folds every dex.
    dexes: Vec<String>,
}

/// The universe the exec thread and the user-data pump BOTH read, replaceable as a whole: the exec
/// thread re-reads `meta` when an order names a coin the universe lacks (a coin listed after the start),
/// and the pump then remaps that coin's fills through the SAME new universe. A snapshot
/// ([`Self::current`]) is an `Arc` of one immutable [`HyperliquidInstruments`], so a reader never sees
/// a half-built one and the swap is one pointer store under a lock nobody holds across I/O.
#[derive(Clone)]
pub struct SharedInstruments(Arc<RwLock<Arc<HyperliquidInstruments>>>);

impl SharedInstruments {
    /// Share `instruments` (the start's universe).
    pub fn new(instruments: Arc<HyperliquidInstruments>) -> Self {
        Self(Arc::new(RwLock::new(instruments)))
    }

    /// The universe as of now.
    pub fn current(&self) -> Arc<HyperliquidInstruments> {
        Arc::clone(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Publish a freshly loaded universe to every reader.
    pub fn replace(&self, fresh: HyperliquidInstruments) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(fresh);
    }
}

impl HyperliquidInstruments {
    /// Fetch `meta` + `spotMeta` and build the resolver + properties grid. Two keyless `/info`
    /// reads, plus the HIP-3 builder dexs when `hip3` — the caller's resolved
    /// `flags.hyperliquid_hip3`.
    pub fn load(transport: &HyperliquidTransport, hip3: bool) -> Result<Self, VenueApiError> {
        Self::load_with_recorder(transport, None, hip3)
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
    /// `CatalogProviderHyperliquid::list_via`, which threads the same seam.
    pub(crate) fn load_from<F>(
        fetch: F,
        recorder: Option<&PropertiesRecorder>,
        hip3: bool,
    ) -> Result<Self, VenueApiError>
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, VenueApiError>,
    {
        Self::load_with_dexes(fetch, recorder, hip3, &[])
    }

    /// [`Self::load_from`] plus the NAMED builder dexes (decision 0218): with `hip3` off, `perpDexs` and the `meta`
    /// of exactly these dexes are read and folded in, so a coin of one dex is placeable with the HIP-3 universe
    /// flag off and without reading the hundreds of dexes the testnet lists. With `hip3` on every dex is folded
    /// and `dexes` adds nothing. The same best-effort rule as the flag's: a `perpDexs` or per-dex failure logs
    /// and is skipped, never failing the core universe.
    pub(crate) fn load_with_dexes<F>(
        fetch: F,
        recorder: Option<&PropertiesRecorder>,
        hip3: bool,
        dexes: &[String],
    ) -> Result<Self, VenueApiError>
    where
        F: Fn(&serde_json::Value) -> Result<serde_json::Value, VenueApiError>,
    {
        let meta = fetch(&serde_json::json!({ "type": "meta" }))?;
        let spot_meta = fetch(&serde_json::json!({ "type": "spotMeta" }))?;
        let mut symbology = Symbology::from_meta(&meta, &spot_meta);
        if hip3 {
            fold_perp_dexs(&fetch, &mut symbology, &spot_meta, None);
        } else if !dexes.is_empty() {
            fold_perp_dexs(&fetch, &mut symbology, &spot_meta, Some(dexes));
        }
        let mut loaded = Self::from_symbology(symbology, recorder, hip3);
        loaded.dexes = dexes.to_vec();
        Ok(loaded)
    }

    /// Whether this universe folded in the HIP-3 builder dexs — what a reload of it must ask for too.
    pub fn hip3(&self) -> bool {
        self.hip3
    }

    /// The builder dexes folded in on demand with the flag off — what a reload of it must fold again too.
    pub fn dexes(&self) -> &[String] {
        &self.dexes
    }

    /// Derive the [`SymbolProperties`] grid from a resolved [`Symbology`] and (optionally) record it
    /// — the shared tail of the loader and the test `build`. Records ONE row per symbol, stamped
    /// with the fetch wall-clock; idempotent per day in the store.
    fn from_symbology(
        symbology: Symbology,
        recorder: Option<&PropertiesRecorder>,
        hip3: bool,
    ) -> Self {
        let mut properties = IndexMap::with_capacity(symbology.len());
        for instrument in symbology.iter() {
            match properties_for(instrument) {
                Ok(properties_value) => {
                    properties.insert(instrument.symbol.clone(), properties_value);
                }
                Err(why) => {
                    tracing::warn!(venue = VENUE, symbol = %instrument.symbol, %why, "no grid")
                }
            }
        }
        if let Some(rec) = recorder {
            // Filed under the STORE name (decision 0290: a builder-dex `:` is no store path's).
            rec.record_all(
                VENUE,
                properties.iter().map(|(symbol_name, properties_value)| {
                    (
                        vike_paths::store_path::store_name_of_user_name(symbol_name),
                        *properties_value,
                    )
                }),
                vike_model::now_ns(),
            );
        }
        HyperliquidInstruments { symbology, properties, hip3, dexes: Vec::new() }
    }

    /// Pure builder over already-fetched core `meta`/`spotMeta` bodies — the fixture seam (no
    /// network, no HIP-3). Records the derived grid iff a recorder is supplied. `pub(crate)` so
    /// `crate::adapter`'s tests read a start's facts off the same universe shape.
    #[cfg(test)]
    pub(crate) fn build(
        meta: &serde_json::Value,
        spot_meta: &serde_json::Value,
        recorder: Option<&PropertiesRecorder>,
    ) -> Self {
        Self::from_symbology(Symbology::from_meta(meta, spot_meta), recorder, false)
    }

    /// The symbology resolver (coin ⇄ symbol, symbol → asset-id, perp `max_leverage`).
    pub fn symbology(&self) -> &Symbology {
        &self.symbology
    }

    /// The order-placement grid for a USER name (perp `"BTCUSDC.P"` / spot `"HYPEUSDC"`).
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
        self.properties.iter().map(|(symbol, properties)| (symbol.as_str(), properties))
    }
}

/// Derive the order-placement grid for one resolved instrument (research §4). `max_leverage` is not
/// a [`SymbolProperties`] field and is intentionally not carried here (it lives on the [`InstrumentRef`]).
/// `pub(crate)` for its one second caller, `crates/bridges/hyperliquid/src/catalog.rs`'s
/// `instrument_from_ref`, so a catalog row and the exec-path grid are the SAME derivation.
/// A class this venue lists none of has no grid: refused in words (`unlisted_class`).
pub(crate) fn properties_for(instrument: &InstrumentRef) -> Result<SymbolProperties, String> {
    let max_decimals = match instrument.asset_class {
        AssetClass::CryptoPerp => PERP_MAX_DECIMALS,
        AssetClass::CryptoSpot => SPOT_MAX_DECIMALS,
        other => return Err(unlisted_class(other)),
    };
    // szDecimals never exceeds MAX_DECIMALS on HL, so the price-decimal count is ≥ 0; saturate to be
    // safe against malformed metadata (a floor of 0 decimals → tick_size 1.0).
    let price_decimals = max_decimals.saturating_sub(instrument.sz_decimals);
    let step_size = pow10_neg(instrument.sz_decimals);
    Ok(SymbolProperties {
        tick_size: pow10_neg(price_decimals),
        step_size,
        min_qty: step_size,
        max_qty: MAX_QUANTITY,
        min_notional: MIN_NOTIONAL_USD,
        // The class comes from the venue's OWN split of its metadata endpoint (`InstrumentRef::asset_class`,
        // resolved from `meta` vs `spotMeta`) — never from the symbol text, which is the implicit encoding
        // `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove. A unified HL perp
        // symbol is a bare base ("BTC") and would be indistinguishable from a spot base by string.
        asset_class: Some(instrument.asset_class),
        // Everything else absent: HL perps and spot both size in the base asset (no contract
        // multiplier, = 1.0), the tick is one szDecimals-derived power of ten (flat grid, no
        // tiers), and HL declares no venue taker hold. FRU rather than an exhaustive literal so a
        // new `SymbolProperties` field costs this parser nothing.
        ..Default::default()
    })
}

/// `10^-n` as an f64 — the tick and the step of every grid [`properties_for`] builds, where `n` is
/// the venue's `szDecimals`-derived decimal count (≤ 8): `vike_math::powers::power_of_ten(-n)`, the correctly
/// rounded `fl(10⁻ⁿ)` on every platform (that function's doc carries why). Past 22 no power of ten is exact, so
/// a hostile `szDecimals` gets `0.0`, no grid (a guarded `round_to_step` reads it as identity), where a `powi`
/// once answered a platform-dependent sliver.
///
/// ⚠ **There must be exactly one spelling of this f64.** `crates/bridges/hyperliquid/src/instrument/price.rs`'s
/// `round_size` asks the grid witness (`crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image`) "is
/// this size the f64 image of a whole number of lots", a question that is only answerable against
/// the SAME step the lot grid was built from — the one this function writes into
/// `SymbolProperties::step_size` and `crates/vike-execution/src/risk.rs`'s `RiskGate` then multiplies by
/// on its way out. The witness parses the lot step's decimal text, which is the same correctly rounded
/// `fl(10⁻ⁿ)` as this one division for `n <= 22`; a spelling of `10⁻ⁿ` that ever disagreed by a bit
/// would make that witness decline precisely where it is needed.
///
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` carries the measured
/// census of this site (the same bits on both platforms for every `n <= 22`).
pub(crate) fn pow10_neg(n: u32) -> f64 {
    i32::try_from(n).ok().and_then(|n| vike_math::powers::power_of_ten(-n)).unwrap_or(0.0)
}

/// Fetch `perpDexs`, then each builder dex's `meta` (with the `dex` param), folding every returned
/// market into `symbology` with the HIP-3 asset-id offset schema. BEST-EFFORT: a `perpDexs` failure
/// leaves the core universe intact; a single per-dex `meta` failure skips only that dex. `fetch` is
/// the same `/info` seam [`HyperliquidInstruments::load_from`] threads through. `only` narrows it to the NAMED dexes
/// (decision 0218: a dex an order named, with the flag off), matched ignoring ASCII case (a user name spells its
/// dex upper case, `XYZ:TSLAUSDC.P`, the venue lower, `xyz`); `None` folds every one. `spot_meta` names each dex's
/// collateral token ([`Symbology::extend_with_perp_dex`]).
fn fold_perp_dexs<F>(
    fetch: &F,
    symbology: &mut Symbology,
    spot_meta: &serde_json::Value,
    only: Option<&[String]>,
) where
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
    let mut dexs = parse_perp_dexs(&perp_dexs);
    if let Some(names) = only {
        dexs.retain(|d| names.iter().any(|name| name.eq_ignore_ascii_case(&d.name)));
    }
    tracing::info!(
        target: "vike_hyperliquid",
        count = dexs.len(),
        "HIP-3: folding builder-deployed perp dexs into the universe"
    );
    for dex in &dexs {
        match fetch(&serde_json::json!({ "type": "meta", "dex": dex.name.clone() })) {
            Ok(dex_meta) => {
                symbology.extend_with_perp_dex(&dex_meta, dex.index, &dex.name, spot_meta)
            }
            Err(e) => tracing::warn!(
                target: "vike_hyperliquid",
                dex = %dex.name, error = %e,
                "HIP-3 per-dex meta fetch failed; skipping this dex"
            ),
        }
    }
}

#[path = "properties_tests.rs"]
#[cfg(test)]
mod properties_tests;

#[path = "properties_property_tests.rs"]
#[cfg(test)]
mod properties_property_tests;
