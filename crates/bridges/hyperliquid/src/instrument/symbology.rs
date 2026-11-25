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
//!
//! **An instrument is looked up by its USER name** (decision 0290): `BTCUSDC.P`, `KPEPEUSDC.P`, the spot pair
//! `HYPEUSDC`, the builder-dex coin `XYZ:TSLAUSDC.P`. The name is READ, never spelled here (the owner's ruling «Б» of
//! 2026-11-16: a name is spelled once, by the kept list's writer): each listed instrument is joined to its KEPT row
//! (`vike_catalog::kept_list`, the datahub's `<project>/instruments/hyperliquid.json`) by the venue's own listing
//! name, [`InstrumentRef::raw_symbol`] (`BTC`, `kPEPE`, `HYPE/USDC`, `xyz:TSLA`: the same on both networks), and
//! takes the user name that row STORES (`vike_catalog::user_name_of_row`). Everything else comes from THIS network's
//! `meta` + `spotMeta`: the asset id and a spot pair's `@N` differ between mainnet (whose listing the kept file is)
//! and the testnet, so they are never read off the kept row. A listed instrument with no kept row, or whose row
//! stores no name (a list written before «Б»), is left out, and [`Symbology::why_unnamed`] says what cures it (the
//! refresh); two rows answering one user name are BOTH left out: a name that names two instruments names none (said
//! at WARN). Everything engine-facing speaks the user name; the venue's own spellings ([`InstrumentRef::raw_symbol`],
//! [`InstrumentRef::coin`]) stay inside this bridge.
//!
//! **The listing needs no kept list** ([`Symbology::listed`]: every row the venue lists, named or not): the catalog
//! lists from it, and the datahub's writer names what it lists, so neither a listing nor an account's start waits for,
//! or needs, a kept list. With none, the start runs and a path that needs a name refuses in words naming the fix.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use vike_model::{AssetClass, Instrument, MarginMode};

use crate::transport::consts::{
    PERP_DEX_ASSET_BASE, PERP_DEX_ASSET_STRIDE, SPOT_ASSET_OFFSET, VENUE,
};

/// One resolved instrument: its USER `symbol` (`"BTCUSDC.P"`, `"HYPEUSDC"`), the venue's listing name, the
/// venue `coin` string, the order `asset_id`, and the grid facts orders/rounding need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentRef {
    /// The USER name (decision 0290), what the engine, the journal and the fills carry: a perp `"BTCUSDC.P"`
    /// (`"KPEPEUSDC.P"`, upper case), a spot pair `"HYPEUSDC"`, a builder-dex coin `"XYZ:TSLAUSDC.P"`. Set by
    /// [`Symbology`] when the row is resolved (the module doc says how).
    pub symbol: String,
    /// The venue's own listing name, the kept row's `raw_symbol`: perp = the coin (`"BTC"`, `"kPEPE"`); spot =
    /// `"BASE/QUOTE"` (e.g. `"PURR/USDC"`); a builder-dex coin `"xyz:TSLA"`.
    pub raw_symbol: String,
    /// The venue `coin` string for WS/info/candle payloads: perp `"BTC"`; spot `"@<pairIndex>"`
    /// (except the literal `"PURR/USDC"`).
    pub coin: String,
    /// The base asset, read from the payload rather than split off [`Self::symbol`]: perp = the
    /// `meta.universe[].name` (a HIP-3 row with its `"{dex}:"` prefix removed, the dex being
    /// [`Self::dex`]); spot = the BASE token's `spotMeta.tokens[].name`.
    pub base: String,
    /// The quote asset: spot = the QUOTE token's `spotMeta.tokens[].name`; a builder-dex perp = its dex's
    /// collateral token ([`collateral_token`]: `USDC`, `USDT0`, `USDH`); a core perp = `None`, because `meta`
    /// publishes no quote per row (the catalog states the venue-wide USDC settlement).
    pub quote: Option<String>,
    /// The order asset id: perp = `meta.universe` array index; spot = `10000 + spotPairIndex`.
    pub asset_id: u32,
    /// The venue's own split of its metadata: [`AssetClass::CryptoPerp`] for a `meta` row (a HIP-3 dex's
    /// too), [`AssetClass::CryptoSpot`] for a `spotMeta` pair; no other class is ever built here.
    pub asset_class: AssetClass,
    /// `szDecimals` — size rounding grid AND (via `MAX_DECIMALS - szDecimals`) the price grid.
    pub sz_decimals: u32,
    /// Perp only: max leverage from metadata (`None` for spot).
    pub max_leverage: Option<u32>,
    /// `true` when this asset is **isolated-margin ONLY** — cross margin is not available on it
    /// (`meta.universe[].onlyIsolated`). A per-ASSET margin constraint, which is exactly what
    /// [`vike_venues::venue_margin_support::VenueMarginSupport`] is structurally unable to express:
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
    /// ⚠ **Nothing enforces this today, by design.** `vike_venues::venue_caps`' `HYPERLIQUID` row
    /// declares `margin_modes: &[MarginMode::Cross]`, so `preflight_order` already refuses an
    /// explicit `Isolated` on this venue outright and no order can express the mode this field
    /// constrains. The enforcement point arrives when that row gains `Isolated` — at which moment a
    /// per-asset check belongs wherever the mode is chosen, reading this field. Until then it is a
    /// parsed, tested, legible venue fact and NOT a live gate.
    ///
    /// [`InstrumentRef::effective_margin_mode`] is the one place that combines this flag with
    /// `VenueCapabilities::default_margin_mode` — read that when the question is *"which mode rules for
    /// this asset?"* rather than *"is cross unavailable?"*.
    pub only_isolated: bool,
    /// The HIP-3 builder-deployed perp dex this market belongs to (`Some("test")` for a
    /// builder-deployed perp; `None` for a core perp or a spot pair). Carries the per-dex tag
    /// downstream (catalog/risk/caps can branch per-dex) and marks the asset id as living in the
    /// HIP-3 `100000+` range rather than the core `0..N` / spot `10000+` ranges.
    pub dex: Option<String>,
    /// `true` when the venue marks this perp delisted (`meta.universe[].isDelisted`): it still
    /// occupies its array slot (its index is the order asset id of every row after it) but no new
    /// order can open on it. Present-only-when-true like [`Self::only_isolated`], so an absent,
    /// null or non-bool value reads `false`. Spot has no such flag.
    pub delisted: bool,
    /// `true` when the row's `marginMode` is `strictIsolated` or `noCross` (cross unavailable by the
    /// newer word). Read ONLY by the catalog row's venue word (the any-symbol facts verb refuses such a
    /// coin); it is NOT part of [`Self::only_isolated`] and never changes
    /// [`Self::effective_margin_mode`], so a mounted coin's margin mode is what it always was.
    pub no_cross: bool,
}

impl InstrumentRef {
    /// The margin mode that ACTUALLY rules for **this asset** when an `OrderRequest` names none —
    /// [`vike_venues::VenueCapabilities::default_margin_mode`] narrowed by this row's
    /// [`Self::only_isolated`].
    ///
    /// ## The divergence this exists to resolve
    /// `default_margin_mode` is a per-VENUE field, and for every venue except okx it records "the
    /// account-side default that consequently rules, because the adapter sends no margin field".
    /// On hyperliquid that sentence has an exception the venue publishes itself: for an
    /// isolated-only asset there is no cross option to fall back to, so the mode that rules is
    /// [`MarginMode::Isolated`] while `capabilities_for("hyperliquid").default_margin_mode` says
    /// [`MarginMode::Cross`]. The venue agrees with THIS function, not with that field:
    /// [`mod@crate::reconcile`]'s `parse_positions` maps `leverage.type == "isolated"` →
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
    /// [`vike_venues::preflight_order`] refuses an explicit `Isolated` on hyperliquid
    /// outright (`margin_modes: &[MarginMode::Cross]`), so no order can express the mode this
    /// returns. The live consumers of margin mode are all per-POSITION and read venue truth
    /// instead — `vike_execution`'s gate margin fold and `margin_call`'s pool partition, and
    /// `vike_core`'s liquidation-price badge, all keying on `PositionEntry.margin_mode`, which the
    /// reconcile snapshot overwrites with what the venue reported. This function is the call site
    /// waiting for the moment that caps row gains `Isolated`; until then it is the one place where
    /// the per-venue declaration and the per-asset flag are combined, and it is tested rather than
    /// asserted in prose.
    pub fn effective_margin_mode(&self) -> MarginMode {
        if self.only_isolated {
            MarginMode::Isolated
        } else {
            vike_venues::capabilities_for(VENUE).default_margin_mode
        }
    }
}

/// The resolver built once from a venue `meta` + `spotMeta` fetch. Insertion-ordered (`IndexMap`)
/// so iteration is stable/deterministic.
#[derive(Debug)]
pub struct Symbology {
    /// Every row the venue lists, named or not ([`InstrumentRef::symbol`] empty), in listing order.
    listed: Vec<InstrumentRef>,
    by_symbol: IndexMap<String, InstrumentRef>,
    coin_to_symbol: IndexMap<String, String>,
    /// The user names two listed rows answer: refused, neither row is held (decision 0290).
    ambiguous: HashSet<String>,
    /// Each kept row's own name (`raw_symbol`) to the user name it stores: the names this build reads.
    stored: HashMap<String, String>,
    /// Why a listed row may find no stored name ([`Self::why_unnamed`]).
    why_unnamed: Option<String>,
}

/// This process's kept hyperliquid rows (`vike_catalog::kept_list`): the stored names a build reads. This crate's
/// unit tests read the one test list (`crate::test_support::hold_kept_rows`).
fn kept() -> Option<Arc<Vec<Instrument>>> {
    #[cfg(all(test, feature = "execution"))]
    crate::test_support::hold_kept_rows();
    vike_catalog::kept_list(VENUE)
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
        let Some(index) = t.get("index").and_then(|i| i.as_u64()) else {
            continue;
        };
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
        let szd = t.get("szDecimals").and_then(|d| d.as_u64()).unwrap_or(0) as u32;
        map.insert(index, (name, szd));
    }
    map
}

/// **The collateral token of a builder dex**: its `meta`'s `collateralToken` (a TOKEN INDEX) named through
/// `spot_meta`'s token list (measured 2026-11-16: 0 = `USDC`, 268 = `USDT0`, 360 = `USDH`). `None` when the
/// `meta` states none or `spot_meta` lists no such token.
pub fn collateral_token(meta: &Value, spot_meta: &Value) -> Option<String> {
    let index = meta.get("collateralToken")?.as_u64()?;
    let tokens = build_token_index(spot_meta.get("tokens").and_then(|t| t.as_array()));
    tokens.get(&index).map(|(name, _)| name.clone()).filter(|name| !name.is_empty())
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

/// Read one `meta.universe` row's [`InstrumentRef::no_cross`] flag: a `marginMode` of `strictIsolated`
/// or `noCross` (the newer spelling of a restriction that removes the cross option). Any other word, a
/// non-string or none states no restriction. ⚠ Deliberately NOT folded into [`parse_only_isolated`]:
/// that flag decides the margin mode of a MOUNTED coin ([`InstrumentRef::effective_margin_mode`]) and
/// must read exactly as it always did; this one is read by the symbol-facts catalog row only.
fn parse_no_cross(row: &Value) -> bool {
    matches!(row.get("marginMode").and_then(|v| v.as_str()), Some("strictIsolated" | "noCross"))
}

/// Read one `meta.universe` row's [`InstrumentRef::delisted`] flag (`isDelisted`, present only when
/// true; anything else reads `false`).
fn parse_delisted(row: &Value) -> bool {
    row.get("isDelisted").and_then(|v| v.as_bool()).unwrap_or(false)
}

impl Symbology {
    /// Build from the raw `meta` (perp) + `spotMeta` (spot) JSON bodies, each row named by this process's kept list
    /// (the module doc). Missing/malformed sections contribute nothing rather than erroring — a partial universe is
    /// still useful.
    pub fn from_meta(meta: &Value, spot_meta: &Value) -> Self {
        Self::from_meta_named_by(meta, spot_meta, kept().as_deref().map(Vec::as_slice))
    }

    /// [`Self::from_meta`] with each row named by the kept rows `kept` (`None`: no kept list).
    pub fn from_meta_named_by(
        meta: &Value,
        spot_meta: &Value,
        kept: Option<&[Instrument]>,
    ) -> Self {
        let mut s = Self::named_by(kept);
        s.load_perps(meta);
        s.load_spot(spot_meta);
        s
    }

    /// An empty resolver whose rows are named by the kept rows `kept` (`None`: no kept list, so it names none and
    /// only lists, [`Self::listed`]). Rows of another venue are not read.
    pub fn named_by(kept: Option<&[Instrument]>) -> Self {
        let rows: Vec<&Instrument> =
            kept.unwrap_or_default().iter().filter(|row| row.venue == VENUE).collect();
        let why_unnamed = if rows.is_empty() {
            Some(vike_catalog::no_kept_list_refusal(VENUE))
        } else if rows.iter().any(|row| row.naming.is_none()) {
            Some(vike_catalog::predates_symbol_names(VENUE))
        } else {
            None
        };
        let stored = rows
            .iter()
            .filter_map(|row| {
                Some((row.raw_symbol.clone(), vike_catalog::user_name_of_row(row).ok()?))
            })
            .collect();
        Symbology {
            listed: Vec::new(),
            by_symbol: IndexMap::new(),
            coin_to_symbol: IndexMap::new(),
            ambiguous: HashSet::new(),
            stored,
            why_unnamed,
        }
    }

    /// This listing named again by this process's kept list as it is NOW (no venue read): what a holder that outlives
    /// a refresh of the list (the feed) asks on a miss.
    pub fn named_again(&self) -> Self {
        let mut again = Self::named_by(kept().as_deref().map(Vec::as_slice));
        for instrument in &self.listed {
            again.insert(instrument.clone());
        }
        again
    }

    /// **Why this build may name fewer rows than it lists, in words naming the fix**: no kept list
    /// (`vike_catalog::no_kept_list_refusal`), or one written before names were stored
    /// (`vike_catalog::predates_symbol_names`). `None` when the kept list stores names: a miss is then an instrument
    /// this network does not list, or one the list has no row of.
    pub fn why_unnamed(&self) -> Option<&str> {
        self.why_unnamed.as_deref()
    }

    /// List `instrument`, and hold it under the USER name its kept row stores (the module doc). A row with no stored
    /// name is listed only; a second row answering a held name is refused with the first (the name names two).
    fn insert(&mut self, mut instrument: InstrumentRef) {
        self.listed.push(instrument.clone());
        let Some(user) = self.stored.get(&instrument.raw_symbol).cloned() else {
            tracing::debug!(
                venue = VENUE,
                raw_symbol = %instrument.raw_symbol,
                "row listed but not named: no kept row stores its user name"
            );
            return;
        };
        if self.ambiguous.contains(&user) {
            return;
        }
        if self.by_symbol.get(&user).is_some_and(|held| held.coin != instrument.coin) {
            let first = self.by_symbol.shift_remove(&user).map(|held| held.coin);
            if let Some(coin) = &first {
                self.coin_to_symbol.shift_remove(coin);
            }
            tracing::warn!(
                venue = VENUE,
                user = %user,
                first = ?first,
                second = %instrument.coin,
                "two listed instruments answer one user name, so it names neither (decision 0290)"
            );
            self.ambiguous.insert(user);
            return;
        }
        instrument.symbol = user;
        self.coin_to_symbol.insert(instrument.coin.clone(), instrument.symbol.clone());
        self.by_symbol.insert(instrument.symbol.clone(), instrument);
    }

    /// PERP: order asset id = the ARRAY POSITION in `meta.universe` (delisted rows still occupy an
    /// index, so we enumerate as-is), coin = listing name = `name`.
    fn load_perps(&mut self, meta: &Value) {
        let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) else {
            return;
        };
        for (index, row) in universe.iter().enumerate() {
            let Some(name) = row.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let sz_decimals = row.get("szDecimals").and_then(|d| d.as_u64()).unwrap_or(0) as u32;
            let max_leverage = row.get("maxLeverage").and_then(|d| d.as_u64()).map(|v| v as u32);
            self.insert(InstrumentRef {
                symbol: String::new(),
                raw_symbol: name.to_string(),
                coin: name.to_string(),
                base: name.to_string(),
                quote: None,
                asset_id: index as u32,
                asset_class: AssetClass::CryptoPerp,
                sz_decimals,
                max_leverage,
                only_isolated: parse_only_isolated(row),
                dex: None,
                delisted: parse_delisted(row),
                no_cross: parse_no_cross(row),
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
            let (Some(base_index), Some(quote_index)) = (match toks {
                Some(a) if a.len() == 2 => (a[0].as_u64(), a[1].as_u64()),
                _ => (None, None),
            }) else {
                continue;
            };
            let (Some((base, base_szd)), Some((quote, _))) =
                (token_index.get(&base_index), token_index.get(&quote_index))
            else {
                continue;
            };
            self.insert(InstrumentRef {
                symbol: String::new(),
                raw_symbol: format!("{base}/{quote}"),
                coin: coin.to_string(),
                base: base.clone(),
                quote: Some(quote.clone()),
                asset_id,
                asset_class: AssetClass::CryptoSpot,
                sz_decimals: *base_szd,
                max_leverage: None,
                // Spot has no margin axis at all, so cross is not "unavailable" on it — `false` is
                // the truthful answer, not a placeholder. See the field doc for why this is `bool`
                // while `max_leverage` (which spot genuinely lacks a value for) stays `Option`.
                only_isolated: false,
                dex: None,
                delisted: false,
                no_cross: false,
            });
        }
    }

    /// Fold a HIP-3 builder-deployed perp dex's `meta` (fetched with the `dex` param) into the
    /// resolver — ADDITIVE, called after [`Self::from_meta`]. Every market's asset id follows the
    /// HIP-3 offset schema (`PERP_DEX_ASSET_BASE + perp_dex_index * PERP_DEX_ASSET_STRIDE +
    /// universeIndex`, [`crate::transport::consts`]), NOT the bare `universe` index the core perps use, so it
    /// lands in the disjoint `100000+` range. `coin` and the listing name are the dex-qualified `name`
    /// verbatim (the documented `"{dex}:{coin}"` form — a HIP-3 `BTC` is `"test:BTC"`, never
    /// colliding with the core `"BTC"`), and every row is tagged with `dex_name`. `perp_dex_index`
    /// is the dex's position in the `perpDexs` array (see [`parse_perp_dexs`]). The rows' quote is the dex's
    /// collateral token ([`collateral_token`] over `spot_meta`, the `spotMeta` body), the token its user names end
    /// in (`XYZ:TSLAUSDC.P`, `CASH:TSLAUSDT0.P`): a dex whose collateral `spot_meta` does not name is left out
    /// whole, said at WARN, since none of its coins has a user name.
    pub fn extend_with_perp_dex(
        &mut self,
        meta: &Value,
        perp_dex_index: u32,
        dex_name: &str,
        spot_meta: &Value,
    ) {
        let Some(universe) = meta.get("universe").and_then(|u| u.as_array()) else {
            return;
        };
        let Some(collateral) = collateral_token(meta, spot_meta) else {
            tracing::warn!(
                venue = VENUE,
                dex = dex_name,
                "a builder dex states no collateral token spotMeta names; its coins are left out (a coin's \
                 user name ends in that token, decision 0290)"
            );
            return;
        };
        for (index, row) in universe.iter().enumerate() {
            let Some(name) = row.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let sz_decimals = row.get("szDecimals").and_then(|d| d.as_u64()).unwrap_or(0) as u32;
            let max_leverage = row.get("maxLeverage").and_then(|d| d.as_u64()).map(|v| v as u32);
            let asset_id =
                PERP_DEX_ASSET_BASE + perp_dex_index * PERP_DEX_ASSET_STRIDE + index as u32;
            // The bare coin under the documented `"{dex}:{coin}"` name, stripped by the KNOWN dex
            // (`dex_name`) rather than by the last `:`; a name without that prefix stays whole.
            let base =
                name.strip_prefix(dex_name).and_then(|rest| rest.strip_prefix(':')).unwrap_or(name);
            self.insert(InstrumentRef {
                symbol: String::new(),
                raw_symbol: name.to_string(),
                coin: name.to_string(),
                base: base.to_string(),
                quote: Some(collateral.clone()),
                asset_id,
                asset_class: AssetClass::CryptoPerp,
                sz_decimals,
                max_leverage,
                // A HIP-3 builder dex publishes the same per-asset row shape as the core `meta`, so
                // a builder-deployed market can be isolated-only too — read it the same way.
                only_isolated: parse_only_isolated(row),
                dex: Some(dex_name.to_string()),
                delisted: parse_delisted(row),
                no_cross: parse_no_cross(row),
            });
        }
    }

    /// Look up by the USER name, exactly as the engine spells it (`"BTCUSDC.P"` / `"PURRUSDC"`).
    pub fn by_symbol(&self, symbol: &str) -> Option<&InstrumentRef> {
        self.by_symbol.get(symbol)
    }
    /// Look up by the venue `coin` string (`"BTC"` / `"@107"` / `"PURR/USDC"`).
    pub fn by_coin(&self, coin: &str) -> Option<&InstrumentRef> {
        self.coin_to_symbol.get(coin).and_then(|s| self.by_symbol.get(s))
    }
    /// The `coin` string to put on a WS subscription / info query for the user name `symbol`.
    pub fn coin_for(&self, symbol: &str) -> Option<&str> {
        self.by_symbol.get(symbol).map(|i| i.coin.as_str())
    }
    /// The order `asset_id` for the user name `symbol`.
    pub fn asset_id_for(&self, symbol: &str) -> Option<u32> {
        self.by_symbol.get(symbol).map(|i| i.asset_id)
    }
    /// Map a venue `coin` string back to the user name (e.g. `"@107"` → `"HYPEUSDC"`).
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
    /// Every row the venue lists, named or not, in listing order ([`InstrumentRef::symbol`] empty): what the catalog
    /// lists, which no kept list may gate (the datahub's writer names it).
    pub fn listed(&self) -> &[InstrumentRef] {
        &self.listed
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
    for (position, entry) in arr.iter().enumerate() {
        // Index 0 is the core perp dex (`null`); it is loaded via the plain `meta`, never here.
        if position == 0 {
            continue;
        }
        let Some(name) = entry.get("name").and_then(|n| n.as_str()).filter(|s| !s.is_empty())
        else {
            continue;
        };
        let full_name = entry.get("fullName").and_then(|n| n.as_str()).unwrap_or(name).to_string();
        let deployer =
            entry.get("deployer").and_then(|d| d.as_str()).unwrap_or_default().to_string();
        out.push(PerpDexRef {
            index: position as u32,
            name: name.to_string(),
            full_name,
            deployer,
        });
    }
    out
}

#[path = "symbology_tests.rs"]
#[cfg(test)]
mod symbology_tests;
