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
    /// [`vike_model::venues::venue_margin_support::VenueMarginSupport`] is structurally unable to express:
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
    /// ⚠ **Nothing enforces this today, by design.** `vike_model::venues::venue_caps`' `HYPERLIQUID` row
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
    /// [`mod@crate::recon_client`]'s `parse_positions` maps `leverage.type == "isolated"` →
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
    /// [`vike_model::preflight_order`] refuses an explicit `Isolated` on hyperliquid
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
            // an index that does not fit the u32 order asset id is junk, not a wrap-around id
            let Some(asset_id) =
                u32::try_from(index).ok().and_then(|i| SPOT_ASSET_OFFSET.checked_add(i))
            else {
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
                asset_id,
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

#[path = "symbology_tests.rs"]
#[cfg(test)]
mod symbology_tests;
