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
//! [`vike_model::venue_margin_support::VenueMarginSupport`] either — that table is per-VENUE, and
//! hyperliquid is the live counterexample to venue uniformity on this axis (9 of 232 core perps are
//! isolated-only, measured 2026-08-05). Parsed and exposed, NOT enforced: see the field's own doc
//! for why `venue_caps`' cross-only `margin_modes` row is what actually gates the mode today.
//!
//! Optional [`PropertiesRecorder`] threading (`VIKE_RECORD_PROPERTIES=1`, OFF by default), matching
//! the crypto-venue convention (Bybit/OKX record the REAL fetched grid at instrument-fetch time):
//! [`HyperliquidInstruments::load_with_recorder`] records the whole derived grid when a recorder is
//! passed. Feeds the catalog, exec (rounding grid), and `RiskLimits`.
//!
//! **HIP-3 builder-deployed perp dexs** are an OPT-IN universe expansion (`HYPERLIQUID_HIP3=1`,
//! [`crate::consts::HIP3_ENV`]; OFF by default). Unset ⇒ the load fetches ONLY `meta` + `spotMeta`,
//! byte-identical to the pre-HIP-3 universe. Set ⇒ the load ALSO fetches `perpDexs` and, per
//! builder dex, its `meta` (with the `dex` param), folding those markets in with the HIP-3
//! asset-id offset schema (see [`Symbology::extend_with_perp_dex`]). The extra fetches are
//! best-effort — a `perpDexs` / per-dex-`meta` failure logs and is skipped, never failing the core
//! universe load. Because exec places orders off `InstrumentRef::asset_id` verbatim, a folded HIP-3
//! market is order-ready with no change to the exec/signing wire.

use indexmap::IndexMap;

use vike_bridge_core::transport::VenueApiError;
use vike_data::PropertiesRecorder;
use vike_model::SymbolProperties;

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
    /// Fetch `meta` + `spotMeta` and build the resolver + properties grid. Two keyless `/info` reads.
    pub fn load(transport: &HyperliquidTransport) -> Result<Self, VenueApiError> {
        Self::load_with_recorder(transport, None)
    }

    /// [`Self::load`] plus an opt-in [`PropertiesRecorder`]: when present, the whole freshly-derived
    /// grid is recorded into the PIT properties store (best-effort — the recorder no-ops when disabled
    /// and swallows store errors). Mirrors the Bybit/OKX `spawn_with_recorder` convention.
    ///
    /// HIP-3 builder-deployed perp dexs are folded in ONLY when `HYPERLIQUID_HIP3=1` (see the module
    /// doc); OFF (default) ⇒ exactly the two `meta` + `spotMeta` reads as before, byte-identical.
    pub fn load_with_recorder(
        transport: &HyperliquidTransport,
        recorder: Option<&PropertiesRecorder>,
    ) -> Result<Self, VenueApiError> {
        Self::load_from(|body: &serde_json::Value| transport.info(body), recorder, hip3_enabled())
    }

    /// The load orchestration over an `/info` fetch seam (so the OFF/ON request set is fixture-
    /// testable with NO network). ALWAYS fetches `meta` then `spotMeta`; when `hip3` is set,
    /// ADDITIONALLY fetches `perpDexs` and each builder dex's `meta` (with the `dex` param) and folds
    /// them in — best-effort (a HIP-3 fetch failure logs and is skipped, never failing the core
    /// universe). Only a core `meta`/`spotMeta` error propagates, so with `hip3 = false` this is
    /// byte-identical to the pre-HIP-3 loader.
    fn load_from<F>(
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
    /// network, no HIP-3). Records the derived grid iff a recorder is supplied.
    #[cfg(test)]
    fn build(
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

/// The HIP-3 enumeration opt-in: process env [`HIP3_ENV`] equal to the EXACT string `"1"` (the
/// `VIKE_RECONCILE` idiom — not a fuzzy truthy parse). Absent / anything-else ⇒ OFF ⇒ core-only.
fn hip3_enabled() -> bool {
    hip3_flag(std::env::var(HIP3_ENV).ok().as_deref())
}

/// The pure gate decision (EXACT `"1"`), split from the env read so it is testable without touching
/// the process environment.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Synthetic `meta`/`spotMeta` mirroring the [`crate::symbology`] fixture (BTC/ETH perps; PURR &
    /// HYPE spot). Drives the properties derivation with NO network.
    fn instruments() -> HyperliquidInstruments {
        let meta = json!({
            "universe": [
                {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
                {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
            ]
        });
        let spot = json!({
            "tokens": [
                {"name": "USDC", "szDecimals": 8, "index": 0},
                {"name": "PURR", "szDecimals": 0, "index": 1},
                {"name": "HYPE", "szDecimals": 2, "index": 150}
            ],
            "universe": [
                {"name": "PURR/USDC", "tokens": [1, 0], "index": 0, "isCanonical": true},
                {"name": "@107", "tokens": [150, 0], "index": 107, "isCanonical": true}
            ]
        });
        HyperliquidInstruments::build(&meta, &spot, None)
    }

    #[test]
    fn perp_grid_uses_max_decimals_6() {
        let i = instruments();
        // BTC szDecimals 5 → tick 10^-(6-5)=0.1, step 10^-5, min_qty=step.
        let btc = i.properties("BTC").expect("BTC");
        assert_eq!(btc.tick_size, 0.1);
        assert_eq!(btc.step_size, 1e-5);
        assert_eq!(btc.min_qty, 1e-5);
        assert_eq!(btc.min_notional, MIN_NOTIONAL_USD);
        assert_eq!(btc.max_qty, MAX_QTY);
        // ETH szDecimals 4 → tick 10^-(6-4)=0.01, step 10^-4.
        let eth = i.properties("ETH").expect("ETH");
        assert_eq!(eth.tick_size, 0.01);
        assert_eq!(eth.step_size, 1e-4);
    }

    #[test]
    fn spot_grid_uses_max_decimals_8() {
        let i = instruments();
        // HYPE/USDC base szDecimals 2 → tick 10^-(8-2)=1e-6, step 10^-2=0.01.
        let hype = i.properties("HYPE/USDC").expect("HYPE/USDC");
        assert_eq!(hype.tick_size, 1e-6);
        assert_eq!(hype.step_size, 0.01);
        assert_eq!(hype.min_qty, 0.01);
        // PURR/USDC base szDecimals 0 → tick 10^-8, step 10^0=1.0.
        let purr = i.properties("PURR/USDC").expect("PURR/USDC");
        assert_eq!(purr.tick_size, 1e-8);
        assert_eq!(purr.step_size, 1.0);
        assert_eq!(purr.min_qty, 1.0);
    }

    #[test]
    fn caches_every_symbology_instrument_and_leverage_stays_on_the_ref() {
        let i = instruments();
        assert_eq!(i.len(), 4); // BTC, ETH, PURR/USDC, HYPE/USDC
        assert!(!i.is_empty());
        // symbology still reachable for the leverage / asset-id facts SymbolProperties can't hold.
        assert_eq!(i.symbology().by_symbol("BTC").unwrap().max_leverage, Some(40));
        assert_eq!(i.symbology().asset_id_for("HYPE/USDC"), Some(10107));
        // unknown symbol → no grid.
        assert!(i.properties("DOGE").is_none());
        // iteration yields all four in insertion order (perps first).
        let syms: Vec<&str> = i.iter().map(|(s, _)| s).collect();
        assert_eq!(syms, vec!["BTC", "ETH", "PURR/USDC", "HYPE/USDC"]);
    }

    /// `only_isolated` survives the whole load path (meta → `Symbology` → cached instruments) and
    /// stays OFF the [`SymbolProperties`] grid. The grid of an isolated-only asset must be
    /// byte-identical to a cross asset with the same `szDecimals`: this fact constrains the margin
    /// mode, never the rounding — so wiring it can not have moved any order's price or size.
    #[test]
    fn only_isolated_reaches_the_loaded_universe_without_touching_the_grid() {
        let meta = json!({
            "universe": [
                {"name": "BTC", "szDecimals": 2, "maxLeverage": 40},
                {"name": "HPOS", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true}
            ]
        });
        let i = HyperliquidInstruments::build(&meta, &json!({}), None);
        // Reachable through the symbology, exactly like `max_leverage`.
        assert!(i.symbology().by_symbol("HPOS").unwrap().only_isolated);
        assert!(!i.symbology().by_symbol("BTC").unwrap().only_isolated);
        // Same szDecimals ⇒ byte-identical placement grid, isolated-only or not.
        assert_eq!(i.properties("HPOS").expect("HPOS"), i.properties("BTC").expect("BTC"));
    }

    #[test]
    fn empty_universe_yields_no_properties() {
        let empty = HyperliquidInstruments::build(&json!({"universe": []}), &json!({}), None);
        assert!(empty.is_empty());
        assert!(empty.properties("BTC").is_none());
    }

    // ---- HIP-3 opt-in enumeration (the `load_from` fetch seam — no network) ----

    use std::cell::RefCell;

    /// The core `meta` fixture (BTC/ETH perps) reused across the HIP-3 seam tests.
    fn core_meta() -> serde_json::Value {
        json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]})
    }
    /// The core `spotMeta` fixture (PURR & HYPE spot).
    fn core_spot() -> serde_json::Value {
        json!({
            "tokens": [
                {"name": "USDC", "szDecimals": 8, "index": 0},
                {"name": "PURR", "szDecimals": 0, "index": 1},
                {"name": "HYPE", "szDecimals": 2, "index": 150}
            ],
            "universe": [
                {"name": "PURR/USDC", "tokens": [1, 0], "index": 0, "isCanonical": true},
                {"name": "@107", "tokens": [150, 0], "index": 107, "isCanonical": true}
            ]
        })
    }

    #[test]
    fn hip3_flag_is_exact_one() {
        assert!(hip3_flag(Some("1")));
        assert!(!hip3_flag(Some("0")));
        assert!(!hip3_flag(Some("true")), "not a fuzzy truthy parse");
        assert!(!hip3_flag(Some("")));
        assert!(!hip3_flag(None), "absent env ⇒ OFF (default)");
    }

    #[test]
    fn hip3_off_fetches_only_core_meta_and_is_byte_identical() {
        let calls = RefCell::new(Vec::<String>::new());
        let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            let t = body.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            calls.borrow_mut().push(t.clone());
            Ok(match t.as_str() {
                "meta" => core_meta(),
                "spotMeta" => core_spot(),
                other => panic!("HIP-3 OFF must not fetch {other:?}"),
            })
        };
        let off = HyperliquidInstruments::load_from(fetch, None, false).expect("core load");
        // EXACTLY the two core reads, in order — no `perpDexs`, no per-dex `meta`.
        assert_eq!(*calls.borrow(), vec!["meta".to_string(), "spotMeta".to_string()]);
        // …and the resolved universe is byte-identical to the direct core builder.
        let baseline = HyperliquidInstruments::build(&core_meta(), &core_spot(), None);
        let off_refs: Vec<&InstrumentRef> = off.symbology().iter().collect();
        let base_refs: Vec<&InstrumentRef> = baseline.symbology().iter().collect();
        assert_eq!(off_refs, base_refs, "OFF load is byte-identical to the pre-HIP-3 universe");
        assert_eq!(off.len(), baseline.len());
    }

    #[test]
    fn hip3_on_fetches_perp_dexs_and_folds_the_builder_markets() {
        let calls = RefCell::new(Vec::<String>::new());
        let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            let t = body.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let dex = body.get("dex").and_then(|d| d.as_str()).map(|s| s.to_string());
            calls.borrow_mut().push(match &dex {
                Some(d) => format!("meta:{d}"),
                None => t.clone(),
            });
            Ok(match (t.as_str(), dex.as_deref()) {
                ("meta", None) => core_meta(),
                ("spotMeta", _) => core_spot(),
                ("perpDexs", _) => {
                    json!([null, {"name": "test", "fullName": "test dex", "deployer": "0xabc"}])
                }
                ("meta", Some("test")) => {
                    json!({"universe": [{"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10}]})
                }
                _ => json!({}),
            })
        };
        let insts = HyperliquidInstruments::load_from(fetch, None, true).expect("hip3 load");
        // The request set: core reads, THEN perpDexs, THEN the per-dex meta (with the dex param).
        assert_eq!(
            *calls.borrow(),
            ["meta", "spotMeta", "perpDexs", "meta:test"].map(String::from).to_vec()
        );
        // Core markets unchanged at their core asset ids.
        assert_eq!(insts.symbology().asset_id_for("BTC"), Some(0));
        // The HIP-3 market appears with the offset asset id, a derived grid, and the dex tag.
        assert_eq!(insts.symbology().asset_id_for("test:ABC"), Some(110_000));
        assert_eq!(insts.symbology().by_symbol("test:ABC").unwrap().dex.as_deref(), Some("test"));
        assert!(insts.properties("test:ABC").is_some(), "HIP-3 market has a derived grid");
        // BTC, ETH, PURR/USDC, HYPE/USDC, test:ABC.
        assert_eq!(insts.len(), 5);
    }

    #[test]
    fn hip3_perp_dexs_fetch_failure_falls_back_to_core_only() {
        let fetch = |body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            let t = body.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match t {
                "meta" if body.get("dex").is_none() => Ok(core_meta()),
                "spotMeta" => Ok(core_spot()),
                "perpDexs" => Err(VenueApiError { code: 500, msg: "boom".into() }),
                other => panic!("no request expected after perpDexs failed, got {other:?}"),
            }
        };
        let insts = HyperliquidInstruments::load_from(fetch, None, true).expect("core survives");
        // Core universe intact; nothing HIP-3 folded (best-effort).
        assert_eq!(insts.len(), 4);
        assert!(insts.properties("test:ABC").is_none());
        assert_eq!(insts.symbology().asset_id_for("BTC"), Some(0));
    }

    #[test]
    fn hip3_core_meta_error_still_propagates() {
        // A core `meta` failure must fail the whole load (byte-identical error behavior), even with
        // HIP-3 on — the extra reads never mask a core failure.
        let fetch = |_body: &serde_json::Value| -> Result<serde_json::Value, VenueApiError> {
            Err(VenueApiError { code: 503, msg: "core down".into() })
        };
        assert!(HyperliquidInstruments::load_from(fetch, None, true).is_err());
    }
}
