//! Symbol name<->id map plus per-symbol digits/scale, built from a cTrader `SymbolsList`
//! (name<->id, from the light-symbol list) plus a `SymbolById` response (the full
//! `ProtoOASymbol` carries `digits`, the volume grid and the pip/swap terms, which the light list
//! does NOT; [`SymbolTerms`] keeps the last for the catalog's contract). `scale(id)` is `10^digits`
//! — NOTE this is NOT the wire price divisor: `ProtoOASpotEvent`/`ProtoOATrendbar` prices
//! (bid/ask/OHLC) use a fixed 1/100000 relative-price scale for every symbol regardless of
//! `digits` (live-verified; see `event_mapper::RELATIVE_PRICE_SCALE`). `scale()`/`digits` are kept for
//! display/tick-rounding purposes (Task 5). Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/).
//!
//! The map also keeps the broker's REFERENCE lists the handshake fetches beside the symbols
//! ([`SymbolLists`]): symbol category id -> name + asset class id, asset class id -> name, and
//! asset id -> name, plus each light symbol's `symbol_category_id`/`base_asset_id`/
//! `quote_asset_id`. They are what the catalog reads an instrument's class and base/quote FROM
//! (`crate::catalog::kind_of`); nothing here interprets a name.

use std::collections::HashMap;

use crate::proto::{
    ProtoOaAsset, ProtoOaAssetClass, ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaSymbolCategory,
};

/// Every list the connect handshake (`crate::conn::open_and_handshake`) reads about the symbol
/// universe, verbatim off the wire: the light symbols (`ProtoOASymbolsListRes`), the full symbols
/// (`ProtoOASymbolByIdRes`), and the three broker reference lists (`ProtoOASymbolCategoryListRes`,
/// `ProtoOAAssetClassListRes`, `ProtoOAAssetListRes`). [`SymbolMap::from_lists`] builds the map
/// from it. A reference list the venue refused is EMPTY here (the handshake logs it and goes on:
/// a classification read must not block order flow), so a symbol in it reads as uncategorised.
#[derive(Debug, Default, Clone)]
pub struct SymbolLists {
    pub light: Vec<ProtoOaLightSymbol>,
    pub full: Vec<ProtoOaSymbol>,
    pub categories: Vec<ProtoOaSymbolCategory>,
    pub asset_classes: Vec<ProtoOaAssetClass>,
    pub assets: Vec<ProtoOaAsset>,
}

/// A symbol's broker category, resolved through the reference lists: the category's own name and
/// the name of the asset class it links to (`None` when the asset-class list did not carry that
/// id or carried it without a name). Both names are the BROKER's words, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolCategory<'a> {
    pub id: i64,
    pub name: &'a str,
    pub asset_class: Option<&'a str>,
}

/// Per-symbol order-volume grid, in cTrader **centi-units** (1/100 of a base unit — `volume =
/// units × 100`). Every field is a raw cTrader `ProtoOASymbol` value (all `Option<i64>` on the
/// wire; absent → `0`, treated as "no constraint / inert", mirroring the SymbolProperties
/// absent-grid-is-inert rule). Used by `event_mapper::order_to_new_order` to round an order's volume to
/// `step_volume` and clamp it to `[min_volume, max_volume]`. Live-verified (demo 2026-07-14):
/// EURUSD `lot_size=10_000_000` (1 lot = 100k units × 100), `min_volume=step_volume=100_000`
/// (0.01 lot), `max_volume=10_000_000_000`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VolumeGrid {
    /// Volume of one lot, in centi-units.
    pub lot_size: i64,
    /// Minimum order volume, in centi-units. An order rounding below this is rejected.
    pub min_volume: i64,
    /// Order-volume increment, in centi-units. Order volume must be a multiple.
    pub step_volume: i64,
    /// Maximum order volume, in centi-units (`0` = no cap).
    pub max_volume: i64,
}

impl VolumeGrid {
    /// One lot in BASE UNITS (`lot_size / 100`, the centi-unit value descaled), or `None` when the
    /// venue sent no positive lot size. EURUSD's live-verified `10_000_000` centi-units is
    /// `100_000.0` units. The catalog's `vike_model::FxSpec::lot_size` reads this.
    pub fn lot_units(&self) -> Option<f64> {
        (self.lot_size > 0).then(|| self.lot_size as f64 / 100.0)
    }
}

/// The full-entity `ProtoOASymbol` fields the CATALOG reads to describe a contract (the light
/// symbol carries none of them), kept per id beside [`VolumeGrid`]. Raw wire values, nothing
/// derived: `pip_position` is the exponent ([`SymbolTerms::pip_size`] descales it), the two swaps
/// are in the unit `swap_calculation_type` names (`ProtoOaSwapCalculationType`: pips, annual
/// percent or points; absent on the wire = pips, the proto default).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct SymbolTerms {
    /// `ProtoOASymbol.pip_position`: the pip is `10^-pip_position` (EURUSD `4` -> `0.0001`).
    pub pip_position: i32,
    /// `ProtoOASymbol.swap_long`, in the `swap_calculation_type` unit.
    pub swap_long: Option<f64>,
    /// `ProtoOASymbol.swap_short`, in the `swap_calculation_type` unit.
    pub swap_short: Option<f64>,
    /// `ProtoOASymbol.swap_calculation_type` (`ProtoOaSwapCalculationType` as `i32`).
    pub swap_calculation_type: Option<i32>,
}

impl SymbolTerms {
    /// The pip as a price increment, `10^-pip_position`, or `None` when that is not a finite
    /// positive number (a nonsense exponent from a fuzzed or broken payload). Computed as the
    /// reciprocal of `10^pip_position` so `i32::MIN` cannot overflow a negation.
    pub fn pip_size(&self) -> Option<f64> {
        let pip = 10f64.powi(self.pip_position).recip();
        (pip.is_finite() && pip > 0.0).then_some(pip)
    }
}

/// Bidirectional symbol map + digits→scale lookup + per-symbol order-volume grid. Built once at
/// handshake time and shared (`Arc`) between the actor thread and the data/exec clients
/// (Tasks 4/5).
#[derive(Debug, Default, Clone)]
pub struct SymbolMap {
    id_by_name: HashMap<String, i64>,
    name_by_id: HashMap<i64, String>,
    digits_by_id: HashMap<i64, i32>,
    volume_by_id: HashMap<i64, VolumeGrid>,
    terms_by_id: HashMap<i64, SymbolTerms>,
    /// Light symbol id -> its `symbol_category_id`.
    category_by_id: HashMap<i64, i64>,
    /// Light symbol id -> its `base_asset_id`.
    base_by_id: HashMap<i64, i64>,
    /// Light symbol id -> its `quote_asset_id`.
    quote_by_id: HashMap<i64, i64>,
    /// Category id -> `(asset_class_id, category name)`.
    categories: HashMap<i64, (i64, String)>,
    /// Asset class id -> name (an id the venue sent without a name is absent).
    asset_class_names: HashMap<i64, String>,
    /// Asset id -> name.
    asset_names: HashMap<i64, String>,
}

impl SymbolMap {
    /// Build from the light-symbol list (name<->id) and the full-symbol batch (digits), with NO
    /// reference lists: every symbol is uncategorised and has no asset names. A light symbol with
    /// no `symbol_name` is skipped; a symbol id with no matching full entry has an unknown scale
    /// (`scale` returns `1.0`).
    pub fn from_symbols(light: &[ProtoOaLightSymbol], full: &[ProtoOaSymbol]) -> Self {
        Self::from_lists(&SymbolLists {
            light: light.to_vec(),
            full: full.to_vec(),
            ..Default::default()
        })
    }

    /// Build from everything the handshake read ([`SymbolLists`]): the symbols as in
    /// [`SymbolMap::from_symbols`], plus each light symbol's category/base/quote ids and the three
    /// reference lists that name them.
    pub fn from_lists(lists: &SymbolLists) -> Self {
        let mut map = SymbolMap::default();
        for s in &lists.light {
            if let Some(name) = &s.symbol_name {
                map.id_by_name.insert(name.clone(), s.symbol_id);
                map.name_by_id.insert(s.symbol_id, name.clone());
            }
            if let Some(c) = s.symbol_category_id {
                map.category_by_id.insert(s.symbol_id, c);
            }
            if let Some(b) = s.base_asset_id {
                map.base_by_id.insert(s.symbol_id, b);
            }
            if let Some(q) = s.quote_asset_id {
                map.quote_by_id.insert(s.symbol_id, q);
            }
        }
        for c in &lists.categories {
            map.categories.insert(c.id, (c.asset_class_id, c.name.clone()));
        }
        for a in &lists.asset_classes {
            if let (Some(id), Some(name)) = (a.id, &a.name) {
                map.asset_class_names.insert(id, name.clone());
            }
        }
        for a in &lists.assets {
            map.asset_names.insert(a.asset_id, a.name.clone());
        }
        for s in &lists.full {
            map.digits_by_id.insert(s.symbol_id, s.digits);
            map.volume_by_id.insert(
                s.symbol_id,
                VolumeGrid {
                    lot_size: s.lot_size.unwrap_or(0),
                    min_volume: s.min_volume.unwrap_or(0),
                    step_volume: s.step_volume.unwrap_or(0),
                    max_volume: s.max_volume.unwrap_or(0),
                },
            );
            map.terms_by_id.insert(
                s.symbol_id,
                SymbolTerms {
                    pip_position: s.pip_position,
                    swap_long: s.swap_long,
                    swap_short: s.swap_short,
                    swap_calculation_type: s.swap_calculation_type,
                },
            );
        }
        map
    }

    /// The full-entity pip/swap fields for an id, if the handshake's `SymbolById` answered for it.
    /// `None` = only the light symbol is known (the catalog then has a name and nothing else).
    pub fn terms(&self, id: i64) -> Option<SymbolTerms> {
        self.terms_by_id.get(&id).copied()
    }

    /// The light symbol's raw `symbol_category_id`, if it carried one — whether or not the
    /// category list names it ([`SymbolMap::category`] is the resolved view).
    pub fn category_id(&self, id: i64) -> Option<i64> {
        self.category_by_id.get(&id).copied()
    }

    /// The symbol's broker category, resolved: `None` when the light symbol carried no category id
    /// or the handshake's category list did not contain it.
    pub fn category(&self, id: i64) -> Option<SymbolCategory<'_>> {
        let cat = self.category_id(id)?;
        let (class_id, name) = self.categories.get(&cat)?;
        Some(SymbolCategory {
            id: cat,
            name,
            asset_class: self.asset_class_names.get(class_id).map(String::as_str),
        })
    }

    /// The name of the symbol's base asset (`base_asset_id` through the asset list), if both are
    /// known.
    pub fn base_asset(&self, id: i64) -> Option<&str> {
        self.base_by_id.get(&id).and_then(|a| self.asset_names.get(a)).map(String::as_str)
    }

    /// The name of the symbol's quote asset (`quote_asset_id` through the asset list), if both are
    /// known.
    pub fn quote_asset(&self, id: i64) -> Option<&str> {
        self.quote_by_id.get(&id).and_then(|a| self.asset_names.get(a)).map(String::as_str)
    }

    /// The per-symbol order-volume grid (centi-units), if the id had a full `SymbolById` entry.
    /// `None` for an id never seen in the handshake's `SymbolById` response (its volume is
    /// unconstrained — `event_mapper::order_to_new_order` passes the raw centi-volume through).
    pub fn volume_grid(&self, id: i64) -> Option<VolumeGrid> {
        self.volume_by_id.get(&id).copied()
    }

    /// Build a venue-neutral [`vike_model::SymbolProperties`] grid for `symbol` from this
    /// already-handshake-resolved map, for the live `RiskGate` at start time
    /// (`vike_model::RiskLimits::from_properties` for the engine symbol and
    /// `vike_model::SymbolGrid::from_properties` for each declared leg, both applied by
    /// `vike-tradehub-core`'s fold to what [`crate::adapter::CtraderVenueAdapter`] answers with this). Needs NO
    /// network — every input was resolved during the exec handshake. `None` when the symbol name is
    /// unknown (never seen in the handshake's `SymbolsList`), so a misspelled/unmounted symbol
    /// leaves the permissive default limits in place, exactly as an absent pre-fetch does for the
    /// crypto venues.
    ///
    /// Field mapping:
    /// - `tick_size` = `1 / scale` = `10^(-digits)` — the price increment the display scale implies
    ///   (an id with no `SymbolById` digits scales by `1.0`, so its tick is `1.0`).
    /// - `step_size`/`min_qty`/`max_qty` = the centi-unit volume grid divided by `100` (cTrader
    ///   volumes are `units × 100`); an absent grid value is `0` = unconstrained.
    /// - `min_notional` and `contract_size` stay `0.0` = unconstrained: cTrader's `ProtoOASymbol`
    ///   reports neither a notional floor nor a per-unit contract multiplier (FX/CFD size is 1:1
    ///   with units), matching the absent-is-`0.0` fallback-grid convention.
    pub fn risk_properties(&self, symbol: &str) -> Option<vike_model::SymbolProperties> {
        let id = self.id_of(symbol)?;
        let scale = self.scale(id);
        let tick_size = if scale > 0.0 { 1.0 / scale } else { 0.0 };
        let vg = self.volume_grid(id).unwrap_or_default();
        Some(vike_model::SymbolProperties {
            tick_size,
            step_size: vg.step_volume as f64 / 100.0,
            min_qty: vg.min_volume as f64 / 100.0,
            max_qty: vg.max_volume as f64 / 100.0,
            // `asset_class` is READ, never guessed: the light symbol's `symbol_category_id` ->
            // the handshake's category list -> its asset class NAME, matched against
            // `crates/bridges/ctrader/src/catalog.rs`'s `ASSET_CLASS_KEYWORDS` (`listed_kind`).
            // Neither symbol message carries a class itself, so a symbol with no resolvable
            // category, or an asset class no row names, leaves it ABSENT — the field's own doc on
            // `vike_model::SymbolProperties::asset_class` argues why a missing answer must not
            // become a guess, and this grid is persisted as `kind=properties`.
            asset_class: self
                .category(id)
                .and_then(|c| c.asset_class)
                .and_then(crate::catalog::listed_kind)
                .map(crate::catalog::SymbolKind::asset_class),
            // Everything else absent: no min-notional, no contract multiplier, a flat grid
            // (cTrader reports one `digits`-derived tick, no tiers), and no venue taker hold. FRU
            // rather than an exhaustive literal so a new `SymbolProperties` field costs this
            // parser nothing.
            ..Default::default()
        })
    }

    /// Symbol id for a name (e.g. `"EURUSD"` -> `1`), if known.
    pub fn id_of(&self, name: &str) -> Option<i64> {
        self.id_by_name.get(name).copied()
    }

    /// Symbol name for an id, if known.
    pub fn name_of(&self, id: i64) -> Option<&str> {
        self.name_by_id.get(&id).map(String::as_str)
    }

    /// Display/tick scale for a symbol id: `10^digits`. NOT the wire price divisor for
    /// `ProtoOASpotEvent`/`ProtoOATrendbar` (those use the fixed
    /// `event_mapper::RELATIVE_PRICE_SCALE` = 1e5 for every symbol; live-verified). Unknown ids scale by
    /// `1.0` (identity).
    pub fn scale(&self, id: i64) -> f64 {
        let digits = self.digits_by_id.get(&id).copied().unwrap_or(0);
        10f64.powi(digits)
    }

    /// All known symbol ids (order unspecified).
    pub fn ids(&self) -> Vec<i64> {
        self.name_by_id.keys().copied().collect()
    }

    /// Every known symbol NAME (order unspecified). The catalog provider
    /// ([`crate::catalog::CtraderCatalog`]) iterates this to enumerate the venue's universe for the
    /// Symbol picker — cTrader has no REST symbol-list endpoint, so the handshake's `SymbolsList`
    /// (which built this map) is the only place the full name set exists.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.name_by_id.values().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_name_id_and_scale() {
        let light = vec![ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }];
        let full = vec![ProtoOaSymbol { symbol_id: 1, digits: 5, ..Default::default() }];
        let map = SymbolMap::from_symbols(&light, &full);
        assert_eq!(map.id_of("EURUSD"), Some(1));
        assert_eq!(map.name_of(1), Some("EURUSD"));
        assert_eq!(map.scale(1), 100000.0);
        // Unknown id -> identity scale, no name/id.
        assert_eq!(map.scale(999), 1.0);
        assert_eq!(map.name_of(999), None);
        assert_eq!(map.id_of("GBPUSD"), None);
    }

    #[test]
    fn names_iterates_all_symbols() {
        let light = vec![
            ProtoOaLightSymbol {
                symbol_id: 1,
                symbol_name: Some("EURUSD".to_string()),
                ..Default::default()
            },
            ProtoOaLightSymbol {
                symbol_id: 2,
                symbol_name: Some("US500".to_string()),
                ..Default::default()
            },
            // No symbol_name -> skipped (not in the map, not in names()).
            ProtoOaLightSymbol { symbol_id: 3, symbol_name: None, ..Default::default() },
        ];
        let map = SymbolMap::from_symbols(&light, &[]);
        let mut names: Vec<&str> = map.names().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["EURUSD", "US500"]);
    }

    #[test]
    fn risk_properties_builds_grid_from_digits_and_volume() {
        let light = vec![ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }];
        // Live-verified demo values (see the `VolumeGrid` doc): 1 lot = 100k units, min/step =
        // 0.01 lot, max = 100k lots — all in centi-units.
        let full = vec![ProtoOaSymbol {
            symbol_id: 1,
            digits: 5,
            lot_size: Some(10_000_000),
            min_volume: Some(100_000),
            step_volume: Some(100_000),
            max_volume: Some(10_000_000_000),
            ..Default::default()
        }];
        let map = SymbolMap::from_symbols(&light, &full);

        let p = map.risk_properties("EURUSD").expect("known symbol → a grid");
        assert_eq!(p.tick_size, 1.0 / 100_000.0, "10^-digits");
        assert_eq!(p.step_size, 1000.0, "100_000 centi-units / 100");
        assert_eq!(p.min_qty, 1000.0, "100_000 centi-units / 100");
        assert_eq!(p.max_qty, 100_000_000.0, "10_000_000_000 centi-units / 100");
        assert_eq!(p.min_notional, 0.0, "cTrader reports no notional floor → unconstrained");
        assert_eq!(p.contract_size, 0.0, "FX/CFD size is 1:1 → no multiplier");

        // Unknown symbol name → None (start site keeps the permissive default limits).
        assert!(map.risk_properties("GBPUSD").is_none());
    }

    #[test]
    fn terms_keep_the_full_entity_pip_and_swap_fields() {
        let light = vec![ProtoOaLightSymbol {
            symbol_id: 1,
            symbol_name: Some("EURUSD".to_string()),
            ..Default::default()
        }];
        let full = vec![ProtoOaSymbol {
            symbol_id: 1,
            digits: 5,
            pip_position: 4,
            swap_long: Some(-7.31),
            swap_short: Some(2.12),
            swap_calculation_type: Some(1),
            lot_size: Some(10_000_000),
            ..Default::default()
        }];
        let map = SymbolMap::from_symbols(&light, &full);
        let t = map.terms(1).expect("full entity → terms");
        assert_eq!(
            t,
            SymbolTerms {
                pip_position: 4,
                swap_long: Some(-7.31),
                swap_short: Some(2.12),
                swap_calculation_type: Some(1),
            }
        );
        assert_eq!(t.pip_size(), Some(0.0001));
        assert_eq!(map.volume_grid(1).and_then(|g| g.lot_units()), Some(100_000.0));
        assert_eq!(map.terms(2), None, "no SymbolById entry → no terms");
    }

    #[test]
    fn pip_size_and_lot_units_refuse_nonsense() {
        let pip = |pip_position| SymbolTerms { pip_position, ..Default::default() }.pip_size();
        assert_eq!(pip(0), Some(1.0));
        assert_eq!(pip(-1), Some(10.0));
        assert_eq!(pip(i32::MAX), None, "10^MAX is inf → a zero pip is no pip");
        assert_eq!(pip(i32::MIN), None, "10^MIN is 0 → an infinite pip is no pip");
        assert_eq!(VolumeGrid::default().lot_units(), None, "absent lot size → None, not 0");
        assert_eq!(VolumeGrid { lot_size: -5, ..Default::default() }.lot_units(), None);
    }

    /// The broker's reference lists, as the demo broker publishes them (eleven categories all
    /// named `Default Category`, each linked to one named asset class —
    /// `crates/bridges/ctrader/tests/fixtures/ctrader_catalog_capture.json`), for EURUSD (Forex),
    /// US500 (Indices), BTCUSD (Cryptocurrencies), an XYZ in a category the list does not carry,
    /// and an ABC in a class no keyword row names.
    fn reference_lists() -> SymbolLists {
        let light = |symbol_id, name: &str, category, base, quote| ProtoOaLightSymbol {
            symbol_id,
            symbol_name: Some(name.to_string()),
            symbol_category_id: Some(category),
            base_asset_id: Some(base),
            quote_asset_id: Some(quote),
            ..Default::default()
        };
        let category = |id, asset_class_id| ProtoOaSymbolCategory {
            id,
            asset_class_id,
            name: "Default Category".to_string(),
            ..Default::default()
        };
        let class = |id, name: &str| ProtoOaAssetClass {
            id: Some(id),
            name: Some(name.to_string()),
            ..Default::default()
        };
        let asset = |asset_id, name: &str| ProtoOaAsset {
            asset_id,
            name: name.to_string(),
            ..Default::default()
        };
        SymbolLists {
            light: vec![
                light(1, "EURUSD", 1, 5, 16),
                light(2, "US500", 3, 30, 16),
                light(3, "BTCUSD", 8, 50, 16),
                light(4, "XYZ", 99, 5, 16),
                light(5, "ABC", 12, 5, 16),
            ],
            full: (1..=5)
                .map(|symbol_id| ProtoOaSymbol { symbol_id, digits: 2, ..Default::default() })
                .collect(),
            categories: vec![category(1, 1), category(3, 3), category(8, 8), category(12, 12)],
            asset_classes: vec![
                class(1, "Forex"),
                class(3, "Indices"),
                class(8, "Cryptocurrencies"),
                class(12, "Something New"),
            ],
            assets: vec![asset(5, "EUR"), asset(16, "USD"), asset(30, "US500"), asset(50, "BTC")],
        }
    }

    #[test]
    fn reference_lists_resolve_category_class_and_asset_names() {
        let map = SymbolMap::from_lists(&reference_lists());
        assert_eq!(
            map.category(1),
            Some(SymbolCategory { id: 1, name: "Default Category", asset_class: Some("Forex") })
        );
        assert_eq!((map.base_asset(1), map.quote_asset(1)), (Some("EUR"), Some("USD")));
        assert_eq!((map.base_asset(3), map.quote_asset(3)), (Some("BTC"), Some("USD")));
        assert_eq!(map.category_id(4), Some(99), "the raw id stays readable");
        assert_eq!(map.category(4), None, "a category the list does not carry resolves to nothing");
        // `from_symbols` carries no reference list: the ids are kept, nothing resolves.
        let bare = SymbolMap::from_symbols(&reference_lists().light, &[]);
        assert_eq!((bare.category(1), bare.base_asset(1)), (None, None));
    }

    /// `asset_class` is READ from the broker's asset class name and ABSENT whenever that name is
    /// not there to read — a category the list does not carry, an asset class no keyword row
    /// names, or a map built with no reference lists — never guessed from the symbol name.
    #[test]
    fn risk_properties_asset_class_is_read_from_the_asset_class_or_absent() {
        use vike_model::AssetClass;
        let map = SymbolMap::from_lists(&reference_lists());
        let class = |s: &str| map.risk_properties(s).expect("known symbol → a grid").asset_class;
        assert_eq!(class("EURUSD"), Some(AssetClass::Fx));
        assert_eq!(class("US500"), Some(AssetClass::Cfd));
        assert_eq!(class("BTCUSD"), Some(AssetClass::Cfd), "a crypto CFD, not an FX pair");
        assert_eq!(class("XYZ"), None, "uncarried category");
        assert_eq!(class("ABC"), None, "a class no keyword row names");

        let lists = reference_lists();
        let bare = SymbolMap::from_symbols(&lists.light, &lists.full);
        assert_eq!(bare.risk_properties("EURUSD").expect("grid").asset_class, None);
    }
}
