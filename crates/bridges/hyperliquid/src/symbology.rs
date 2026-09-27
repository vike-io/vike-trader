//! Hyperliquid symbology — the three index spaces (token index / spot-pair index / order-asset-id).
//! Pure, fixture-tested. Ports the wire rules in `docs/research/2026-07-16-hyperliquid-adapters`
//! §5.
//!
//! THREE distinct index spaces — conflating them is the classic HL adapter bug:
//! - **token index**: `spotMeta.tokens[].index` — a currency id (NOT used for orders).
//! - **spot-pair index**: `spotMeta.universe[].index` — appears in the `coin` string as `"@<index>"`.
//! - **order asset id**: PERP = the `meta.universe` ARRAY position; SPOT = `10000 + spotPairIndex`.
//!
//! The `coin` string (used in WS subscriptions, `/info` reads, candles) is the perp `name` (e.g.
//! `"BTC"`) or, for spot, the universe entry `name` — `"@<pairIndex>"` for every pair EXCEPT the
//! one literal `"PURR/USDC"`. Spot base/quote are resolved POSITIONALLY through `universe[].tokens =
//! [baseTokenIdx, quoteTokenIdx]` into `spotMeta.tokens[]`. Because we read the actual `name`, the
//! PURR/USDC special case needs no branch here — it falls out of the data.

use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::Value;
use vike_model::MarginMode;

use crate::config::Product;
use crate::consts::{PERP_DEX_ASSET_BASE, PERP_DEX_ASSET_STRIDE, SPOT_ASSET_OFFSET, VENUE};

/// One resolved instrument: the unified `symbol` (perp `"BTC"` / spot `"BASE/QUOTE"`), the venue
/// `coin` string, the order `asset_id`, and the grid facts orders/rounding need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRef {
    /// Unified symbol: perp = the coin (`"BTC"`); spot = `"BASE/QUOTE"` (e.g. `"PURR/USDC"`).
    pub symbol: String,
    /// The venue `coin` string for WS/info/candle payloads: perp `"BTC"`; spot `"@<pairIndex>"`
    /// (except the literal `"PURR/USDC"`).
    pub coin: String,
    /// The order asset id: perp = `meta.universe` array index; spot = `10000 + spotPairIndex`.
    pub asset_id: u32,
    pub product: Product,
    /// `szDecimals` — size rounding grid AND (via `MAX_DECIMALS - szDecimals`) the price grid.
    pub sz_decimals: u32,
    /// Perp only: max leverage from metadata (`None` for spot).
    pub max_leverage: Option<u32>,
    /// `true` when this asset is **isolated-margin ONLY** — cross margin is not available on it
    /// (`meta.universe[].onlyIsolated`). A per-ASSET margin constraint, which is exactly what
    /// [`vike_model::venue_margin_support::VenueMarginSupport`] is structurally unable to express:
    /// that table is per-VENUE, and hyperliquid is not uniform on this axis. Measured live
    /// 2026-08-05: **9 of 232** core perps — HPOS, RLB, UNIBOT, OX, FRIEND, SHIA, NFTI, PANDORA,
    /// CASHCAT.
    ///
    /// ## Why `bool` and not `Option<bool>` (unlike its `max_leverage` neighbour)
    /// HL encodes this as a **present-only-when-true** flag: in the live response the key appears on
    /// exactly the 9 isolated-only rows and on NO others — there are ZERO explicit `false` rows
    /// (verified against the real body, pinned by `only_isolated_matches_the_live_wire_encoding`).
    /// So "absent" is not a third state, it is the venue spelling `false`, and collapsing it is
    /// faithful rather than lossy. `max_leverage` stays `Option` for the opposite reason: spot has
    /// no leverage number at all, so inventing one would be a lie, whereas "is cross unavailable for
    /// this asset?" has a correct answer for spot — **no** — so spot rows are a truthful `false`.
    ///
    /// A non-bool or absent value therefore parses `false` (cross available), never `true`: this
    /// field only ever ADDS a restriction, so an unreadable value must not manufacture one.
    ///
    /// ⚠ **Nothing enforces this today, by design.** `vike_model::venue_caps`' `HYPERLIQUID` row
    /// declares `margin_modes: &[MarginMode::Cross]`, so `preflight_order` already refuses an
    /// explicit `Isolated` on this venue outright and no order can express the mode this field
    /// constrains. The enforcement point arrives when that row gains `Isolated` — at which moment a
    /// per-asset check belongs wherever the mode is chosen, reading this field. Until then it is a
    /// parsed, tested, legible venue fact and NOT a live gate.
    ///
    /// [`InstrumentRef::effective_margin_mode`] is the one place that combines this flag with
    /// `VenueCaps::default_margin_mode` — read that when the question is *"which mode rules for
    /// this asset?"* rather than *"is cross unavailable?"*.
    pub only_isolated: bool,
    /// The HIP-3 builder-deployed perp dex this market belongs to (`Some("test")` for a
    /// builder-deployed perp; `None` for a core perp or a spot pair). Carries the per-dex tag
    /// downstream (catalog/risk/caps can branch per-dex) and marks the asset id as living in the
    /// HIP-3 `100000+` range rather than the core `0..N` / spot `10000+` ranges.
    pub dex: Option<String>,
}

impl InstrumentRef {
    /// The margin mode that ACTUALLY rules for **this asset** when an `OrderRequest` names none —
    /// [`vike_model::VenueCaps::default_margin_mode`] narrowed by this row's
    /// [`Self::only_isolated`].
    ///
    /// ## The divergence this exists to resolve
    /// `default_margin_mode` is a per-VENUE field, and for every venue except okx it records "the
    /// account-side default that consequently rules, because the adapter sends no margin field".
    /// On hyperliquid that sentence has an exception the venue publishes itself: for an
    /// isolated-only asset there is no cross option to fall back to, so the mode that rules is
    /// [`MarginMode::Isolated`] while `caps_for("hyperliquid").default_margin_mode` says
    /// [`MarginMode::Cross`]. The venue agrees with THIS function, not with that field:
    /// [`crate::recon_client`]'s `parse_positions` maps `leverage.type == "isolated"` →
    /// `MarginMode::Isolated`, so a reconcile pass over a position on one of those assets reports
    /// Isolated.
    ///
    /// The fix is deliberately a DERIVATION, not a second table: the only new fact is the
    /// combination rule, and both inputs stay single-sourced — the per-asset half is #1081's
    /// already-parsed `only_isolated` (off `meta.universe`), the per-venue half is the caps
    /// registry. A per-asset column in `venue_caps` would be the second source of truth; this is
    /// not one.
    ///
    /// ## What it does NOT claim
    /// It answers exactly one question — *"has this asset removed the cross option?"* — and nothing
    /// else. Spot rows are `only_isolated: false`, so they return the venue default verbatim
    /// (`Cross`), which is neither more nor less accurate than the caps field alone: whether a
    /// fully-funded spot balance is better modelled as [`MarginMode::Cash`] is a separate question
    /// on a separate axis, and inventing an answer here would smuggle in a claim nothing measured.
    ///
    /// ## ⚠ Legible, not yet a gate
    /// Nothing calls this on the order path, and it must not change one:
    /// [`vike_model::venue_caps::preflight_order`] refuses an explicit `Isolated` on hyperliquid
    /// outright (`margin_modes: &[MarginMode::Cross]`), so no order can express the mode this
    /// returns. The live consumers of margin mode are all per-POSITION and read venue truth
    /// instead — `vike_exec`'s gate margin fold and `margin_call`'s pool partition, and
    /// `vike_core`'s liquidation-price badge, all keying on `PositionEntry.margin_mode`, which the
    /// reconcile snapshot overwrites with what the venue reported. This function is the call site
    /// waiting for the moment that caps row gains `Isolated`; until then it is the one place where
    /// the per-venue declaration and the per-asset flag are combined, and it is tested rather than
    /// asserted in prose.
    pub fn effective_margin_mode(&self) -> MarginMode {
        if self.only_isolated {
            MarginMode::Isolated
        } else {
            vike_model::caps_for(VENUE).default_margin_mode
        }
    }
}

/// The resolver built once from a venue `meta` + `spotMeta` fetch. Insertion-ordered (`IndexMap`)
/// so iteration is stable/deterministic.
#[derive(Debug, Default)]
pub struct Symbology {
    by_symbol: IndexMap<String, InstrumentRef>,
    coin_to_symbol: IndexMap<String, String>,
}

/// Build `token index (the `index` FIELD) -> (name, szDecimals)` from `spotMeta.tokens`. Spot
/// pairs reference their base/quote by TOKEN INDEX (e.g. HYPE = 150), NOT by array position — the
/// real array is dense (position == index) but resolving by the `index` field is correct either way.
fn build_token_index(tokens: Option<&Vec<Value>>) -> HashMap<u64, (String, u32)> {
    let mut map = HashMap::new();
    let Some(toks) = tokens else {
        return map;
    };
    for t in toks {
        let Some(idx) = t.get("index").and_then(|i| i.as_u64()) else {
            continue;
        };
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
        let szd = t.get("szDecimals").and_then(|d| d.as_u64()).unwrap_or(0) as u32;
        map.insert(idx, (name, szd));
    }
    map
}

/// Read one `meta.universe` row's [`InstrumentRef::only_isolated`] flag.
///
/// HL omits `onlyIsolated` entirely on cross-capable assets and sets it `true` on the isolated-only
/// ones — measured on the live body 2026-08-05: present on 9 rows, all `true`, ZERO explicit
/// `false`. Absent, null, or a non-bool therefore reads `false` (cross available). That direction is
/// the safe one and is deliberate: the flag only ever REMOVES cross, so an unreadable value must
/// never manufacture a restriction that the venue did not state. Shared by the core-perp loader and
/// the HIP-3 per-dex loader so the two can never drift apart.
fn parse_only_isolated(row: &Value) -> bool {
    row.get("onlyIsolated").and_then(|v| v.as_bool()).unwrap_or(false)
}

impl Symbology {
    /// Build from the raw `meta` (perp) + `spotMeta` (spot) JSON bodies. Missing/malformed sections
    /// contribute nothing rather than erroring — a partial universe is still useful.
    pub fn from_meta(meta: &Value, spot_meta: &Value) -> Self {
        let mut s = Symbology::default();
        s.load_perps(meta);
        s.load_spot(spot_meta);
        s
    }

    fn insert(&mut self, inst: InstrumentRef) {
        self.coin_to_symbol.insert(inst.coin.clone(), inst.symbol.clone());
        self.by_symbol.insert(inst.symbol.clone(), inst);
    }

    /// PERP: order asset id = the ARRAY POSITION in `meta.universe` (delisted rows still occupy an
    /// index, so we enumerate as-is), coin = symbol = `name`.
    fn load_perps(&mut self, meta: &Value) {
        let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) else {
            return;
        };
        for (idx, row) in universe.iter().enumerate() {
            let Some(name) = row.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let sz_decimals = row.get("szDecimals").and_then(|d| d.as_u64()).unwrap_or(0) as u32;
            let max_leverage = row.get("maxLeverage").and_then(|d| d.as_u64()).map(|v| v as u32);
            self.insert(InstrumentRef {
                symbol: name.to_string(),
                coin: name.to_string(),
                asset_id: idx as u32,
                product: Product::Perp,
                sz_decimals,
                max_leverage,
                only_isolated: parse_only_isolated(row),
                dex: None,
            });
        }
    }

    /// SPOT: order asset id = `10000 + universe[].index`; coin = universe `name` (`"@N"` or
    /// `"PURR/USDC"`); base/quote resolved positionally via `tokens = [baseIdx, quoteIdx]`. The
    /// SIZE grid uses the BASE token's `szDecimals`.
    fn load_spot(&mut self, spot_meta: &Value) {
        let token_index = build_token_index(spot_meta.get("tokens").and_then(|t| t.as_array()));
        let Some(universe) = spot_meta.get("universe").and_then(|u| u.as_array()) else {
            return;
        };
        for row in universe {
            let Some(coin) = row.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(index) = row.get("index").and_then(|i| i.as_u64()) else {
                continue;
            };
            // `tokens` must be exactly two TOKEN INDICES, both resolvable, else skip the pair
            // (malformed / testnet junk pair with a missing token name).
            let toks = row.get("tokens").and_then(|t| t.as_array());
            let (Some(base_idx), Some(quote_idx)) = (match toks {
                Some(a) if a.len() == 2 => (a[0].as_u64(), a[1].as_u64()),
                _ => (None, None),
            }) else {
                continue;
            };
            let (Some((base, base_szd)), Some((quote, _))) =
                (token_index.get(&base_idx), token_index.get(&quote_idx))
            else {
                continue;
            };
            self.insert(InstrumentRef {
                symbol: format!("{base}/{quote}"),
                coin: coin.to_string(),
                asset_id: SPOT_ASSET_OFFSET + index as u32,
                product: Product::Spot,
                sz_decimals: *base_szd,
                max_leverage: None,
                // Spot has no margin axis at all, so cross is not "unavailable" on it — `false` is
                // the truthful answer, not a placeholder. See the field doc for why this is `bool`
                // while `max_leverage` (which spot genuinely lacks a value for) stays `Option`.
                only_isolated: false,
                dex: None,
            });
        }
    }

    /// Fold a HIP-3 builder-deployed perp dex's `meta` (fetched with the `dex` param) into the
    /// resolver — ADDITIVE, called after [`Self::from_meta`]. Every market's asset id follows the
    /// HIP-3 offset schema (`PERP_DEX_ASSET_BASE + perp_dex_index * PERP_DEX_ASSET_STRIDE +
    /// universeIndex`, [`crate::consts`]), NOT the bare `universe` index the core perps use, so it
    /// lands in the disjoint `100000+` range. `coin` and `symbol` are the dex-qualified `name`
    /// verbatim (the documented `"{dex}:{coin}"` form — a HIP-3 `BTC` is `"test:BTC"`, never
    /// colliding with the core `"BTC"`), and every row is tagged with `dex_name`. `perp_dex_index`
    /// is the dex's position in the `perpDexs` array (see [`parse_perp_dexs`]).
    pub fn extend_with_perp_dex(&mut self, meta: &Value, perp_dex_index: u32, dex_name: &str) {
        let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) else {
            return;
        };
        for (idx, row) in universe.iter().enumerate() {
            let Some(name) = row.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let sz_decimals = row.get("szDecimals").and_then(|d| d.as_u64()).unwrap_or(0) as u32;
            let max_leverage = row.get("maxLeverage").and_then(|d| d.as_u64()).map(|v| v as u32);
            let asset_id =
                PERP_DEX_ASSET_BASE + perp_dex_index * PERP_DEX_ASSET_STRIDE + idx as u32;
            self.insert(InstrumentRef {
                symbol: name.to_string(),
                coin: name.to_string(),
                asset_id,
                product: Product::Perp,
                sz_decimals,
                max_leverage,
                // A HIP-3 builder dex publishes the same per-asset row shape as the core `meta`, so
                // a builder-deployed market can be isolated-only too — read it the same way.
                only_isolated: parse_only_isolated(row),
                dex: Some(dex_name.to_string()),
            });
        }
    }

    /// Look up by the unified symbol (`"BTC"` / `"PURR/USDC"`).
    pub fn by_symbol(&self, symbol: &str) -> Option<&InstrumentRef> {
        self.by_symbol.get(symbol)
    }
    /// Look up by the venue `coin` string (`"BTC"` / `"@107"` / `"PURR/USDC"`).
    pub fn by_coin(&self, coin: &str) -> Option<&InstrumentRef> {
        self.coin_to_symbol.get(coin).and_then(|s| self.by_symbol.get(s))
    }
    /// The `coin` string to put on a WS subscription / info query for `symbol`.
    pub fn coin_for(&self, symbol: &str) -> Option<&str> {
        self.by_symbol.get(symbol).map(|i| i.coin.as_str())
    }
    /// The order `asset_id` for `symbol`.
    pub fn asset_id_for(&self, symbol: &str) -> Option<u32> {
        self.by_symbol.get(symbol).map(|i| i.asset_id)
    }
    /// Map a venue `coin` string back to the unified symbol (e.g. `"@107"` → `"HYPE/USDC"`).
    pub fn symbol_for_coin(&self, coin: &str) -> Option<&str> {
        self.coin_to_symbol.get(coin).map(|s| s.as_str())
    }
    /// Number of resolved instruments.
    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
    /// Iterate all resolved instruments (insertion order: perps then spot).
    pub fn iter(&self) -> impl Iterator<Item = &InstrumentRef> {
        self.by_symbol.values()
    }
}

/// One builder-deployed perp dex parsed from the `perpDexs` info response. `index` is its POSITION
/// in that array — the `perp_dex_index` multiplier of the HIP-3 asset-id schema; `name` is both the
/// `dex` param for the per-dex `meta` fetch AND the `"{dex}:"` symbol prefix; `full_name`/`deployer`
/// are the descriptive fields carried for tagging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerpDexRef {
    pub index: u32,
    pub name: String,
    pub full_name: String,
    pub deployer: String,
}

/// Parse a `perpDexs` response (`[null, {name, fullName, deployer, …}, …]`) into the builder dexs.
/// The FIRST element (index 0) is the CORE perp dex (`null`) — its universe is the plain `meta`,
/// already loaded — so it is skipped. Each remaining entry KEEPS its array index as
/// `perp_dex_index` (positions are stable/positional on HL, so a middle null does not renumber the
/// dexs after it), which is why the first builder dex is index 1 — matching the HL docs' `test` →
/// asset-id-base 110000. Malformed / nameless entries are skipped rather than erroring (a partial
/// dex list is still useful).
pub fn parse_perp_dexs(perp_dexs: &Value) -> Vec<PerpDexRef> {
    let Some(arr) = perp_dexs.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, entry) in arr.iter().enumerate() {
        // Index 0 is the core perp dex (`null`); it is loaded via the plain `meta`, never here.
        if idx == 0 {
            continue;
        }
        let Some(name) = entry.get("name").and_then(|n| n.as_str()).filter(|s| !s.is_empty())
        else {
            continue;
        };
        let full_name = entry.get("fullName").and_then(|n| n.as_str()).unwrap_or(name).to_string();
        let deployer =
            entry.get("deployer").and_then(|d| d.as_str()).unwrap_or_default().to_string();
        out.push(PerpDexRef { index: idx as u32, name: name.to_string(), full_name, deployer });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Symbology {
        let meta = json!({
            "universe": [
                {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
                {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
            ]
        });
        // HYPE spot on mainnet: token index 150, spot pair index 107 -> coin "@107".
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
        Symbology::from_meta(&meta, &spot)
    }

    #[test]
    fn perp_asset_id_is_the_universe_array_index() {
        let s = fixture();
        let btc = s.by_symbol("BTC").expect("BTC");
        assert_eq!(btc.asset_id, 0);
        assert_eq!(btc.coin, "BTC");
        assert_eq!(btc.product, Product::Perp);
        assert_eq!(btc.sz_decimals, 5);
        assert_eq!(btc.max_leverage, Some(40));
        assert_eq!(s.by_symbol("ETH").unwrap().asset_id, 1);
    }

    #[test]
    fn spot_asset_id_is_10000_plus_pair_index() {
        let s = fixture();
        // PURR/USDC: pair index 0 -> asset id 10000; coin is the literal name, NOT "@0".
        let purr = s.by_symbol("PURR/USDC").expect("PURR/USDC");
        assert_eq!(purr.asset_id, SPOT_ASSET_OFFSET);
        assert_eq!(purr.coin, "PURR/USDC");
        assert_eq!(purr.product, Product::Spot);
        assert_eq!(purr.sz_decimals, 0); // base token (PURR) szDecimals
        assert!(purr.max_leverage.is_none());
    }

    #[test]
    fn spot_at_index_coin_and_positional_base_quote() {
        let s = fixture();
        // "@107" pair, tokens [150, 0] -> base HYPE / quote USDC -> symbol "HYPE/USDC".
        let hype = s.by_symbol("HYPE/USDC").expect("HYPE/USDC");
        assert_eq!(hype.coin, "@107");
        assert_eq!(hype.asset_id, SPOT_ASSET_OFFSET + 107);
        assert_eq!(hype.sz_decimals, 2);
    }

    #[test]
    fn coin_and_symbol_round_trip() {
        let s = fixture();
        assert_eq!(s.coin_for("BTC"), Some("BTC"));
        assert_eq!(s.asset_id_for("HYPE/USDC"), Some(10107));
        assert_eq!(s.symbol_for_coin("@107"), Some("HYPE/USDC"));
        assert_eq!(s.by_coin("PURR/USDC").unwrap().symbol, "PURR/USDC");
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn malformed_spot_pair_is_skipped() {
        let meta = json!({"universe": []});
        let spot = json!({
            "tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}],
            "universe": [{"name": "@2", "tokens": [99], "index": 2}] // 1 token -> skip
        });
        let s = Symbology::from_meta(&meta, &spot);
        assert!(s.is_empty());
    }

    #[test]
    fn core_rows_carry_no_dex_tag() {
        let s = fixture();
        assert_eq!(s.by_symbol("BTC").unwrap().dex, None);
        assert_eq!(s.by_symbol("HYPE/USDC").unwrap().dex, None);
    }

    #[test]
    fn parse_perp_dexs_skips_core_null_and_indexes_builders_from_one() {
        // The first element is the core dex (null); builder dexs follow at array index 1, 2, …
        let v = json!([
            null,
            {"name": "test", "fullName": "test dex", "deployer": "0xabc", "oracleUpdater": null},
            {"name": "vntls", "fullName": "Ventuals", "deployer": "0xdef"}
        ]);
        let dexs = parse_perp_dexs(&v);
        assert_eq!(dexs.len(), 2);
        assert_eq!(dexs[0].index, 1, "first builder dex is perp_dex_index 1 (0 = core null)");
        assert_eq!(dexs[0].name, "test");
        assert_eq!(dexs[0].full_name, "test dex");
        assert_eq!(dexs[0].deployer, "0xabc");
        assert_eq!(dexs[1].index, 2);
        assert_eq!(dexs[1].name, "vntls");
    }

    #[test]
    fn parse_perp_dexs_of_nonarray_or_core_only_is_empty() {
        assert!(parse_perp_dexs(&json!({})).is_empty(), "non-array → empty");
        assert!(parse_perp_dexs(&json!([null])).is_empty(), "core-only → no builder dexs");
        // a nameless middle entry is skipped but does NOT renumber the ones after it.
        let v = json!([null, {"deployer": "0x0"}, {"name": "keep"}]);
        let dexs = parse_perp_dexs(&v);
        assert_eq!(dexs.len(), 1);
        assert_eq!(dexs[0].index, 2, "positions stay positional across a skipped entry");
        assert_eq!(dexs[0].name, "keep");
    }

    #[test]
    fn hip3_asset_ids_use_the_offset_schema_and_tag_the_dex() {
        // Core BTC = universe index 0 (asset 0), spot empty.
        let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]});
        let spot =
            json!({"tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}], "universe": []});
        let mut s = Symbology::from_meta(&meta, &spot);
        // Dex "test" at perp_dex_index 1, markets test:ABC (meta idx 0) and test:XYZ (meta idx 1).
        let dex_meta = json!({"universe": [
            {"name": "test:ABC", "szDecimals": 2, "maxLeverage": 10},
            {"name": "test:XYZ", "szDecimals": 3}
        ]});
        s.extend_with_perp_dex(&dex_meta, 1, "test");

        // Core BTC untouched.
        assert_eq!(s.asset_id_for("BTC"), Some(0));
        // test:ABC = 100000 + 1*10000 + 0 = 110000 (the exact HL docs worked example).
        let abc = s.by_symbol("test:ABC").expect("test:ABC");
        assert_eq!(abc.asset_id, 110_000);
        assert_eq!(
            abc.coin, "test:ABC",
            "coin is the dex-qualified name verbatim (wire round-trips)"
        );
        assert_eq!(abc.product, Product::Perp);
        assert_eq!(abc.dex.as_deref(), Some("test"), "row tagged with its deployer dex");
        assert_eq!(abc.max_leverage, Some(10));
        // test:XYZ = 100000 + 1*10000 + 1 = 110001.
        assert_eq!(s.asset_id_for("test:XYZ"), Some(110_001));
        // coin ⇄ symbol round-trips through the qualified name.
        assert_eq!(s.symbol_for_coin("test:ABC"), Some("test:ABC"));
        assert_eq!(s.by_coin("test:XYZ").unwrap().symbol, "test:XYZ");
    }

    #[test]
    fn extend_with_perp_dex_is_purely_additive_to_core_rows() {
        let meta = json!({"universe": [{"name": "BTC", "szDecimals": 5}]});
        let spot = json!({});
        let core = Symbology::from_meta(&meta, &spot);
        let mut ext = Symbology::from_meta(&meta, &spot);
        ext.extend_with_perp_dex(
            &json!({"universe": [{"name": "test:ABC", "szDecimals": 2}]}),
            1,
            "test",
        );
        // The core BTC row is byte-identical (InstrumentRef: Eq) and untagged; only the count grows.
        assert_eq!(core.by_symbol("BTC"), ext.by_symbol("BTC"));
        assert_eq!(ext.by_symbol("BTC").unwrap().dex, None);
        assert_eq!(core.len(), 1);
        assert_eq!(ext.len(), 2);
    }

    /// The `onlyIsolated` parse, pinned against the REAL wire encoding rather than a symmetric
    /// guess. Fixture rows are verbatim-shaped `meta.universe` entries using the actual live field
    /// set (`name`/`szDecimals`/`maxLeverage`/`marginTableId`/`isDelisted`/`onlyIsolated`) and the
    /// actual live isolated-only names, so the four cases that matter are each nailed down:
    ///
    /// - **absent** (BTC — how 223 of 232 live rows look) ⇒ `false`, the venue spelling "cross is
    ///   available". This is the case a naive `Option<bool>` model would leave ambiguous.
    /// - **present `true`** (HPOS/RLB — how all 9 live isolated-only rows look) ⇒ `true`.
    /// - **explicit `false`** ⇒ `false`. HL emits this on nothing today, but if it ever starts, it
    ///   must NOT read as `true` — this is the "in either direction" half.
    /// - **non-bool junk** (the string `"true"`) ⇒ `false`, never `true`: an unreadable value may
    ///   not manufacture a restriction the venue did not state.
    #[test]
    fn only_isolated_matches_the_live_wire_encoding() {
        let meta = json!({"universe": [
            // Absent — the overwhelming majority shape (223/232 live).
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40, "marginTableId": 50},
            // Present `true` — the live isolated-only shape, real names from the 2026-08-05 body.
            {"name": "HPOS", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true},
            {"name": "RLB", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true,
             "isDelisted": true},
            // Explicit `false` — not emitted by HL today; must stay `false`, not flip to `true`.
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25, "onlyIsolated": false},
            // Malformed/non-bool — must degrade to `false` (never invent a restriction).
            {"name": "JUNK", "szDecimals": 1, "onlyIsolated": "true"},
        ]});
        let s = Symbology::from_meta(&meta, &json!({}));

        assert!(
            !s.by_symbol("BTC").unwrap().only_isolated,
            "absent onlyIsolated ⇒ cross available"
        );
        assert!(s.by_symbol("HPOS").unwrap().only_isolated, "present true ⇒ isolated-only");
        assert!(s.by_symbol("RLB").unwrap().only_isolated, "true survives alongside isDelisted");
        assert!(!s.by_symbol("ETH").unwrap().only_isolated, "explicit false stays false");
        assert!(
            !s.by_symbol("JUNK").unwrap().only_isolated,
            "non-bool degrades to false, not true"
        );

        // The flag is independent of every neighbouring fact it shares a row with — it must not be
        // inferred from low leverage, delisting, or array position.
        assert_eq!(s.by_symbol("HPOS").unwrap().max_leverage, Some(3));
        assert_eq!(s.by_symbol("HPOS").unwrap().asset_id, 1);
        assert_eq!(s.by_symbol("HPOS").unwrap().product, Product::Perp);
        // Exactly the two `true` rows across the whole universe — no over- or under-matching.
        let iso: Vec<&str> =
            s.iter().filter(|i| i.only_isolated).map(|i| i.symbol.as_str()).collect();
        assert_eq!(iso, vec!["HPOS", "RLB"]);
    }

    /// Spot rows are `false`: spot has no margin axis, so cross is not "unavailable" there. Pinned
    /// so the field is never quietly repurposed into "N/A" for spot (which is what `max_leverage`,
    /// deliberately `Option`, means by `None` — the two fields answer different questions).
    #[test]
    fn spot_rows_are_not_isolated_only_and_leverage_stays_none() {
        let s = fixture();
        for sym in ["PURR/USDC", "HYPE/USDC"] {
            let inst = s.by_symbol(sym).expect(sym);
            assert!(!inst.only_isolated, "{sym}: spot is never isolated-only");
            assert!(inst.max_leverage.is_none(), "{sym}: spot has no leverage value");
        }
        // …and the perps in the same fixture (no `onlyIsolated` key at all) are `false` too.
        assert!(!s.by_symbol("BTC").unwrap().only_isolated);
        assert!(s.iter().all(|i| !i.only_isolated), "fixture universe has no isolated-only asset");
    }

    /// **The per-asset vs per-venue divergence, pinned in both directions.**
    ///
    /// An isolated-only asset and an ordinary one sit in the SAME universe under the SAME
    /// `VenueCaps` row, and [`InstrumentRef::effective_margin_mode`] must separate them:
    ///
    /// - HPOS (`onlyIsolated: true`) ⇒ `Isolated`, which is what the venue itself reports for a
    ///   position on it (`recon_client`'s `parse_positions`, `leverage.type == "isolated"`) — and
    ///   is NOT what `caps_for("hyperliquid").default_margin_mode` says. That inequality is
    ///   asserted explicitly rather than left implicit: it IS the divergence, and a future PR that
    ///   "fixes" it by flipping the venue-level field to `Isolated` would break every ordinary
    ///   asset, so the assert must fail loudly if the per-venue field is ever made to agree.
    /// - BTC (flag absent) ⇒ exactly the venue default, byte-for-byte — narrowing applies to the
    ///   9 rows that carry the flag and to nothing else.
    ///
    /// Spot is asserted too: `only_isolated` is false there, so the venue default passes through
    /// unchanged. The accessor answers "has cross been removed?" and deliberately makes no claim
    /// about spot's cash funding.
    #[test]
    fn effective_margin_mode_is_per_asset_not_per_venue() {
        let venue_default = vike_model::caps_for(crate::consts::VENUE).default_margin_mode;
        assert_eq!(venue_default, MarginMode::Cross, "the per-venue declaration under test");

        let meta = json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
            // Real 2026-08-05 shapes: 8 of the 9 isolated-only assets are also delisted; CASHCAT
            // is the one still live, so the divergence is reachable, not historical.
            {"name": "HPOS", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true,
             "isDelisted": true},
            {"name": "CASHCAT", "szDecimals": 0, "maxLeverage": 3, "onlyIsolated": true},
        ]});
        let s = Symbology::from_meta(&meta, &json!({}));

        // The isolated-only rows: the venue's per-asset truth WINS over the per-venue default.
        for sym in ["HPOS", "CASHCAT"] {
            let inst = s.by_symbol(sym).expect(sym);
            assert_eq!(
                inst.effective_margin_mode(),
                MarginMode::Isolated,
                "{sym}: isolated-only ⇒ Isolated, whatever the per-venue row says"
            );
            assert_ne!(
                inst.effective_margin_mode(),
                venue_default,
                "{sym}: this INEQUALITY is the divergence — the per-venue field is wrong here"
            );
        }

        // The ordinary row: no narrowing at all, the venue default passes through verbatim.
        let btc = s.by_symbol("BTC").unwrap();
        assert_eq!(btc.effective_margin_mode(), venue_default, "BTC: unnarrowed venue default");
        assert_eq!(btc.effective_margin_mode(), MarginMode::Cross);

        // Spot: no margin axis, no narrowing — the venue default, unchanged.
        let spot = fixture();
        let purr = spot.by_symbol("PURR/USDC").expect("PURR/USDC");
        assert!(!purr.only_isolated);
        assert_eq!(
            purr.effective_margin_mode(),
            venue_default,
            "spot: venue default passes through"
        );
    }

    /// A HIP-3 builder-deployed perp publishes the same row shape, so it can be isolated-only too —
    /// the per-dex loader must read the flag, not default it.
    #[test]
    fn hip3_rows_carry_only_isolated_too() {
        let mut s = Symbology::from_meta(&json!({"universe": []}), &json!({}));
        s.extend_with_perp_dex(
            &json!({"universe": [
                {"name": "test:ISO", "szDecimals": 2, "maxLeverage": 3, "onlyIsolated": true},
                {"name": "test:X", "szDecimals": 2, "maxLeverage": 10}
            ]}),
            1,
            "test",
        );
        assert!(s.by_symbol("test:ISO").unwrap().only_isolated, "HIP-3 row reads the flag");
        assert!(!s.by_symbol("test:X").unwrap().only_isolated, "HIP-3 absent ⇒ false");
    }

    #[test]
    fn perp_dex_index_two_uses_the_next_stride() {
        // A second builder dex (perp_dex_index 2): 100000 + 2*10000 + 0 = 120000.
        let mut s = Symbology::from_meta(&json!({"universe": []}), &json!({}));
        s.extend_with_perp_dex(
            &json!({"universe": [{"name": "vntls:AAPL", "szDecimals": 2}]}),
            2,
            "vntls",
        );
        assert_eq!(s.asset_id_for("vntls:AAPL"), Some(120_000));
        assert_eq!(s.by_symbol("vntls:AAPL").unwrap().dex.as_deref(), Some("vntls"));
    }
}
