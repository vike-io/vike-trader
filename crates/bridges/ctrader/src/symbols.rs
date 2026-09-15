//! Symbol name<->id map plus per-symbol digits/scale, built from a cTrader `SymbolsList`
//! (name<->id, from the light-symbol list) plus a `SymbolById` response (the full
//! `ProtoOASymbol` carries `digits`, which the light list does NOT). `scale(id)` is `10^digits`
//! — NOTE this is NOT the wire price divisor: `ProtoOASpotEvent`/`ProtoOATrendbar` prices
//! (bid/ask/OHLC) use a fixed 1/100000 relative-price scale for every symbol regardless of
//! `digits` (live-verified; see `event_mapper::RELATIVE_PRICE_SCALE`). `scale()`/`digits` are kept for
//! display/tick-rounding purposes (Task 5). Ports nothing — cTrader Open API
//! (https://help.ctrader.com/open-api/).

use std::collections::HashMap;

use crate::proto::{ProtoOaLightSymbol, ProtoOaSymbol};

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

/// Bidirectional symbol map + digits→scale lookup + per-symbol order-volume grid. Built once at
/// handshake time and shared (`Arc`) between the actor thread and the data/exec clients
/// (Tasks 4/5).
#[derive(Debug, Default, Clone)]
pub struct SymbolMap {
    id_by_name: HashMap<String, i64>,
    name_by_id: HashMap<i64, String>,
    digits_by_id: HashMap<i64, i32>,
    volume_by_id: HashMap<i64, VolumeGrid>,
}

impl SymbolMap {
    /// Build from the light-symbol list (name<->id) and the full-symbol batch (digits). A light
    /// symbol with no `symbol_name` is skipped; a symbol id with no matching full entry has an
    /// unknown scale (`scale` returns `1.0`).
    pub fn from_symbols(light: &[ProtoOaLightSymbol], full: &[ProtoOaSymbol]) -> Self {
        let mut map = SymbolMap::default();
        for s in light {
            if let Some(name) = &s.symbol_name {
                map.id_by_name.insert(name.clone(), s.symbol_id);
                map.name_by_id.insert(s.symbol_id, name.clone());
            }
        }
        for s in full {
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
        }
        map
    }

    /// The per-symbol order-volume grid (centi-units), if the id had a full `SymbolById` entry.
    /// `None` for an id never seen in the handshake's `SymbolById` response (its volume is
    /// unconstrained — `event_mapper::order_to_new_order` passes the raw centi-volume through).
    pub fn volume_grid(&self, id: i64) -> Option<VolumeGrid> {
        self.volume_by_id.get(&id).copied()
    }

    /// Build a venue-neutral [`vike_model::SymbolProperties`] grid for `symbol` from this
    /// already-handshake-resolved map, for the live `RiskGate` at mount time
    /// (`vike_exec::RiskLimits::from_properties`, called from `vike_mount::make_engine`). Needs NO
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

        // Unknown symbol name → None (mount site keeps the permissive default limits).
        assert!(map.risk_properties("GBPUSD").is_none());
    }
}
