//! Hyperliquid `ReconClient` — the venue-facing report seam (`vike_exec::recon::ReconClient`).
//!
//! All reads are **keyless** `POST /info` against the **MASTER** account address (never the agent
//! wallet — agent-address reads return empty; research §8 pitfall #9). Mirrors the Bybit
//! `recon_client` template: every fetch delegates to a PURE `parse_*(body: &str) -> Result<Vec<…>,
//! String>` free function — the fixture-tested unit (recorded `/info` JSON bodies, NO network) — and
//! the [`HyperliquidReconClient`] holds only what a report fetch needs: a [`HyperliquidTransport`],
//! the master address, and the configured [`Product`].
//!
//! The four report reads (research §8, `docs/research/2026-07-16-hyperliquid-adapters/README.md`):
//! - `frontendOpenOrders` (a top-level ARRAY) → [`OrderStatusReport`]s. Like Bybit's `order/realtime`
//!   this returns only the CURRENTLY-open set with no time filter, so `since` is a no-op (a
//!   fully-closed order that fell out of this set is what the fill/position reports, not this one,
//!   reconcile). HL rows carry NO per-order status (every row is resting) — the FSM status is DERIVED
//!   from fill progress: `origSz > sz` (some already filled) → `PARTIALLY_FILLED`, else `ACCEPTED`.
//!   [`normalize_order_status`] maps the general §10 HL status vocabulary by suffix for any row that
//!   does carry an explicit `status`.
//! - `userFillsByTime` (`startTime = since`) / `userFills` (when `since <= 0`), each a top-level
//!   ARRAY → [`FillReport`]s. `tid` → `trade_id`, `crossed` → taker/maker, signed `fee` →
//!   `commission`, `feeToken` → `commission_asset` — the same field mapping [`crate::exec::event_mapper`]
//!   applies to the live WS `userFills` stream.
//! - `clearinghouseState.assetPositions[].position` → [`PositionStatusReport`]s. HL `szi` is ALREADY
//!   SIGNED (+ long / − short) → `qty` directly, with NO `side`-string re-signing (unlike Bybit's
//!   unsigned `size` + `side`). HL is one-way/net (no hedge legs) → `PositionSide::Both`. EVERY row
//!   is kept, a flat (`szi == 0`) leg included: `recon::diff` needs the row PRESENT to detect a
//!   locally-open position the venue has since flattened (a reported flat row diffs against the
//!   local qty as `PositionDrift`). NOTE the blind spot: HL OMITS closed positions entirely, and
//!   `vike_exec::recon::diff`'s position leg iterates VENUE rows only — there is NO local-side
//!   position sweep (unlike orders, where step 3 raises `OrphanLocalOrder` for a live local order
//!   the venue no longer reports) — so a cached local position with NO row in the response raises
//!   NO divergence today; its closing activity surfaces through the fill lane (`userFills` →
//!   `MissingFill`) instead. Keeping every reported row is therefore load-bearing, and we never
//!   filter rows here. Mirrors the Bybit/binance "keep every row" choice.
//! - `fetch_balance` per [`Product`]: PERP `clearinghouseState.marginSummary.accountValue`, EXCEPT on
//!   a `unifiedAccount` / `portfolioMargin` account (`userAbstraction`), whose perp summary reads 0
//!   because SPOT USDC is its perp collateral — there the USDC row's `total` in
//!   `spotClearinghouseState.balances[]` is the figure; SPOT the same USDC row.
//!
//! **`fetch_balance`** (the `ReconClient` trait's 4th method, default `Ok(None)`) is implemented in
//! the `impl ReconClient` block below, overriding the default so `dyn ReconClient` dispatch uses HL's
//! real balance. It began as an inherent method while this branch predated the
//! `feat/reconciliation-activation` merge that added the 4th trait method, and moved into the trait
//! impl when that landed in main (#360).
//!
//! Symbols are carried VERBATIM as the venue `coin` string (perp `"BTC"` == the unified symbol; spot
//! `"@<idx>"`), exactly like OKX's recon reports carry `instId` and like the rest of this crate
//! ("symbols are NOT resolved here" — [`crate::exec::event_mapper`]); the recon manager keys on the venue
//! symbol. HL numeric `oid`/`tid` ids stringify via [`vike_bridge_core::json::json_str`]; px/sz/fee
//! are decimal strings decoded via [`json_num`].

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num, json_str};
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::base::config::Product;
use crate::base::consts::VENUE;
use crate::base::symbology::InstrumentRef;
use crate::base::transport::HyperliquidTransport;
use crate::feeds::instruments::HyperliquidInstruments;

// --- pure parsers ------------------------------------------------------------------------------

/// HL side field → sign: `"A"` (ask) = sell = −1; `"B"` (bid) and anything else = buy = +1 (matches
/// [`crate::exec::event_mapper`]'s `side_sign`).
fn side_sign(s: &str) -> i32 {
    if s == "A" { -1 } else { 1 }
}

/// A venue `cloid` (`0x…`) → `client_order_id`, `None` when absent/empty (an externally-placed order
/// the venue never echoed our id for) — same convention as the Bybit/OKX recon clients.
fn cloid_opt(row: &Value) -> Option<String> {
    row.get("cloid").and_then(|c| c.as_str()).filter(|c| !c.is_empty()).map(|c| c.to_string())
}

/// Parse `body` as the top-level JSON ARRAY that `frontendOpenOrders` / `userFills` return, erroring
/// (never panicking) on non-array / malformed JSON.
fn top_array(body: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    v.as_array().cloned().ok_or_else(|| "expected a top-level JSON array".to_string())
}

/// Map an HL order status onto the FSM [`OrderStatus`] — the §10 29-value enum mapped by SUFFIX
/// (mirrors [`crate::exec::event_mapper`]'s `is_cancel`/`is_reject`): every `*Canceled` (plus the
/// `-ed`-less `scheduledCancel`) → `Canceled`, every `*Rejected` → `Rejected`, `open`/`resting` →
/// `Accepted`, `filled` → `Filled`, `triggered` → `Triggered`. `frontendOpenOrders` rows carry NO
/// status (all resting), so this only fires for a status-bearing row — the open/partial
/// classification there is derived from fill progress instead.
///
/// A word outside that set is WARNED and read as `Accepted`: live, the conservative direction —
/// `diff` raises no `MissingTerminal` on it, adoption synthesizes no cumulative fill, and the restore
/// planner keeps the order rather than declaring it gone.
fn normalize_order_status(raw: &str) -> OrderStatus {
    match raw {
        "open" | "resting" => OrderStatus::Accepted,
        "filled" => OrderStatus::Filled,
        "triggered" => OrderStatus::Triggered,
        s if s == "canceled" || s.ends_with("Canceled") || s == "scheduledCancel" => {
            OrderStatus::Canceled
        }
        s if s == "rejected" || s.ends_with("Rejected") => OrderStatus::Rejected,
        other => {
            tracing::warn!(
                venue = VENUE,
                status = other,
                "unknown order status word — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

/// Map an HL `orderType` (`frontendOpenOrders` spells it in words: `Limit`, `Market`, `Stop Market`,
/// `Stop Limit`, `Take Profit Market`, `Take Profit Limit`) onto [`OrderType`]: `Limit`/`Market` are
/// their twins, the stop pair `Stop`/`StopLimit`, and both take-profit kinds `TakeProfit` (the report
/// carries no price, so the limit flavour has nothing further to keep). Any other word has no twin
/// and stays [`OrderType::Unrecognized`] lower-cased, as every report spelled it before the type
/// existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "Limit" => OrderType::Limit,
        "Market" => OrderType::Market,
        "Stop Market" => OrderType::Stop,
        "Stop Limit" => OrderType::StopLimit,
        "Take Profit Market" | "Take Profit Limit" => OrderType::TakeProfit,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// `frontendOpenOrders` (a top-level array) → [`OrderStatusReport`]s. `origSz` is the ORIGINAL order
/// size and `sz` the REMAINING unfilled size, so `qty = origSz` and `filled_qty = origSz − sz`;
/// `avgPrice` is not in this payload → `avg_px = 0.0`. Status is DERIVED (rows are all resting): any
/// fill progress → `PartiallyFilled`, else `Accepted` — unless a row carries an explicit `status`,
/// then [`normalize_order_status`]. `coin` rides on `symbol` verbatim; numeric `oid` stringifies.
pub fn parse_orders(body: &str) -> Result<Vec<OrderStatusReport>, String> {
    let rows = top_array(body)?;
    Ok(rows
        .iter()
        .map(|o| {
            let orig = o.get("origSz").and_then(json_num);
            let remaining = o.get("sz").and_then(json_num).unwrap_or(0.0);
            let qty = orig.unwrap_or(remaining);
            let filled_qty = (qty - remaining).max(0.0);
            let raw_status = o.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let status = if !raw_status.is_empty() {
                normalize_order_status(raw_status)
            } else if filled_qty > 0.0 {
                OrderStatus::PartiallyFilled
            } else {
                OrderStatus::Accepted
            };
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: o.get("coin").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                venue_order_id: o.get("oid").map(json_str).unwrap_or_default().into(),
                client_order_id: cloid_opt(o),
                side: side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                order_type: normalize_order_type(
                    o.get("orderType").and_then(|t| t.as_str()).unwrap_or(""),
                ),
                qty,
                filled_qty,
                avg_px: 0.0,
                status,
                ts: o.get("timestamp").and_then(json_int).unwrap_or(0),
            }
        })
        .collect())
}

/// `userFills` / `userFillsByTime` (a top-level array) → [`FillReport`]s. `tid` → `trade_id`, `oid` →
/// `venue_order_id` (both HL JSON numbers, stringified via [`json_str`]); `crossed` true → taker,
/// false → maker; `fee` is SIGNED (positive cost / negative rebate — kept as-is, the vike convention
/// [`crate::exec::event_mapper`] also applies); `feeToken` → `commission_asset`; `coin` → `symbol`
/// verbatim.
///
/// A row with no `tid` is **SKIPPED**, not admitted with an empty id. `trade_id` is what
/// `vike_exec::recon::diff` looks up in `seen_trade_ids`; an id-less report can never match, so it
/// does not merely fail to dedup — it FABRICATES a `MissingFill` divergence, and `MissingFill` is
/// one of the two kinds the `hybrid` policy AUTO-APPLIES. The invented divergence would therefore
/// book the fill a second time with no operator in front of it. Skipping costs at most a real fill
/// this pass cannot see (the next pass re-reads the same window); admitting costs double-booked
/// commission and realized PnL. `tid` is mandatory in HL's `userFills` schema, so this is a
/// malformed-frame path, and nothing is synthesized in its place (`oid` is per-ORDER, so oid-keyed
/// reports would collapse a partially-filled order's legs into one).
pub fn parse_fills(body: &str) -> Result<Vec<FillReport>, String> {
    let rows = top_array(body)?;
    let out: Vec<FillReport> = rows
        .iter()
        .filter_map(|f| {
            let trade_id = TradeId::new(f.get("tid").map(json_str).unwrap_or_default()).ok()?;
            let crossed = f.get("crossed").and_then(|b| b.as_bool()).unwrap_or(false);
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: f.get("coin").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                trade_id,
                venue_order_id: f.get("oid").map(json_str).unwrap_or_default().into(),
                client_order_id: cloid_opt(f),
                side: side_sign(f.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                last_qty: f.get("sz").and_then(json_num).unwrap_or(0.0),
                last_px: f.get("px").and_then(json_num).unwrap_or(0.0),
                commission: f.get("fee").and_then(json_num).unwrap_or(0.0),
                commission_asset: f
                    .get("feeToken")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                liquidity_side: if crossed { LiquiditySide::Taker } else { LiquiditySide::Maker },
                ts: f.get("time").and_then(json_int).unwrap_or(0),
            })
        })
        .collect();
    let skipped = rows.len() - out.len();
    if skipped > 0 {
        tracing::warn!(
            venue = VENUE,
            skipped,
            "reconcile `userFills` rows carry no `tid` — skipped; an id-less FillReport cannot \
             match `seen_trade_ids` and would manufacture a MissingFill divergence that `hybrid` \
             auto-applies (double-booking the fill)"
        );
    }
    Ok(out)
}

/// `clearinghouseState` → [`PositionStatusReport`]s from `assetPositions[].position`. HL `szi` is
/// ALREADY SIGNED (+ long / − short) → `qty` directly (no `side`-string re-signing). HL is one-way
/// (net) → `PositionSide::Both`. The snapshot's top-level `time` stamps every row (positions carry
/// no own ts). EVERY row is kept — a flat (`szi == 0`) leg included — so `recon::diff` can spot a
/// locally-open position the venue has since flattened; a malformed entry lacking a `position` object
/// is skipped (never panics).
pub fn parse_positions(body: &str) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v
        .get("assetPositions")
        .and_then(|a| a.as_array())
        .ok_or_else(|| "expected an `assetPositions` array".to_string())?;
    let ts = v.get("time").and_then(json_int).unwrap_or(0);
    Ok(rows
        .iter()
        .filter_map(|ap| {
            let pos = ap.get("position")?;
            // Margin-mode step-2 (read-side only): HL reports the position's mode inside
            // `leverage: {"type": "cross"|"isolated", "value": n}`. Anything but an explicit
            // "isolated" — including an absent `leverage` object — is the fail-safe Cross
            // (pre-field behavior). No isolated-wallet carrier: the fixture-verified payload
            // shape carries no per-position wallet field we can vouch for, so it stays `None`.
            //
            // This read is what makes the venue disagree with the per-VENUE
            // `VenueCaps::default_margin_mode` on the 9 isolated-only assets, and the venue is
            // right — see `crate::base::symbology::InstrumentRef::effective_margin_mode`. NOTE the one
            // spot where the fail-safe is knowingly coarse: on an isolated-only asset an absent
            // `leverage` object still reads Cross, where the asset admits nothing but Isolated.
            // Consulting `only_isolated` here would mean holding a `Symbology` (a `meta` fetch) in
            // the recon client purely for a case HL has never been observed to emit — deliberately
            // not done; the wire always carries `leverage`.
            let isolated = pos
                .get("leverage")
                .and_then(|l| l.get("type"))
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.eq_ignore_ascii_case("isolated"));
            Some(PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: pos.get("coin").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                position_side: PositionSide::Both,
                qty: pos.get("szi").and_then(json_num).unwrap_or(0.0),
                avg_px: pos.get("entryPx").and_then(json_num).unwrap_or(0.0),
                ts,
                margin_mode: if isolated { MarginMode::Isolated } else { MarginMode::Cross },
                isolated_margin: None,
                delta: None,
            })
        })
        .collect())
}

/// `spotClearinghouseState` -> one [`PositionStatusReport`] per ADMITTED spot pair (decision 0218): a spot holding is
/// a balance row of the pair's BASE token, not a position, so the reconcile read of positions (a perpetual's
/// `clearinghouseState`) never showed it. The report names the pair by the engine's symbol (`HYPE/USDC`), carries
/// the held `total` (which includes what open orders hold) as its quantity and `entryNtl / total` as its price, and
/// books `Cash` (fully funded). A pair the account holds none of, or holds less than ONE size step of (dust a
/// transfer or a fee left), is a FLAT row: like a perpetual's, the row must be present for `recon::diff` to see a
/// locally-open position the venue has since flattened. A body with no `balances` array is malformed (`Err`), never
/// read as flat.
pub fn parse_spot_positions(
    body: &str,
    pairs: &[InstrumentRef],
    ts: i64,
) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v
        .get("balances")
        .and_then(|b| b.as_array())
        .ok_or_else(|| "spotClearinghouseState: expected a `balances` array".to_string())?;
    Ok(pairs
        .iter()
        .map(|inst| {
            let row = rows
                .iter()
                .find(|r| r.get("coin").and_then(|c| c.as_str()) == Some(inst.base.as_str()));
            let total = row.and_then(|r| r.get("total")).and_then(json_num).unwrap_or(0.0);
            let entry = row.and_then(|r| r.get("entryNtl")).and_then(json_num).unwrap_or(0.0);
            let qty = if total >= crate::feeds::instruments::pow10_neg(inst.sz_decimals) {
                total
            } else {
                0.0
            };
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: inst.symbol.clone(),
                position_side: PositionSide::Both,
                qty,
                avg_px: if qty > 0.0 { entry / qty } else { 0.0 },
                ts,
                margin_mode: MarginMode::Cash,
                isolated_margin: None,
                delta: None,
            }
        })
        .collect())
}

/// Account balance per [`Product`] (research §8): PERP = `clearinghouseState.marginSummary
/// .accountValue` (total account value in USD); SPOT = the USDC row's `total` in
/// `spotClearinghouseState.balances[]`. `Ok(None)` when the field/row is absent (venue surfaced no
/// balance); only malformed JSON is an `Err`. All HL numerics are decimal STRINGS.
pub fn parse_balance(body: &str, product: Product) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(match product {
        Product::Perp => {
            v.get("marginSummary").and_then(|m| m.get("accountValue")).and_then(json_num)
        }
        Product::Spot => v
            .get("balances")
            .and_then(|b| b.as_array())
            .and_then(|arr| {
                arr.iter().find(|c| c.get("coin").and_then(|x| x.as_str()) == Some("USDC"))
            })
            .and_then(|row| row.get("total"))
            .and_then(json_num),
    })
}

/// How long a successful `userAbstraction` read is trusted. The mode changes only by an explicit
/// account action, so a few minutes is safe; a FAILED read is never cached.
const ABSTRACTION_TTL: Duration = Duration::from_secs(300);

/// Where an account's perp collateral lives, from `userAbstraction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Collateral {
    /// Separate perp wallet: the perp `marginSummary.accountValue` is the figure (the original read).
    Perp,
    /// `unifiedAccount` / `portfolioMargin`: SPOT USDC is the perp collateral, and the perp summary
    /// reads 0 on a funded account.
    SpotUsdc,
}

/// `userAbstraction` (a bare JSON string) → where the collateral is. `unifiedAccount` and
/// `portfolioMargin` → spot USDC; `default`/`disabled`/`dexAbstraction` → the perp wallet. A word
/// outside both sets reads as [`Collateral::Perp`] (warned): that is the figure every account got
/// before this read existed, it can only under-state a funded account (never over-state it), and
/// failing instead would make `fetch_balance` — the startup preflight's credential probe — refuse
/// the account on a word we merely do not know. A non-string body (null, an object) is an `Err`.
fn parse_abstraction(body: &str) -> Result<Collateral, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    match v.as_str() {
        Some("unifiedAccount" | "portfolioMargin") => Ok(Collateral::SpotUsdc),
        Some("default" | "disabled" | "dexAbstraction") => Ok(Collateral::Perp),
        Some(other) => {
            tracing::warn!(
                venue = VENUE,
                abstraction = other,
                "unknown userAbstraction word — reading the perp account value as before"
            );
            Ok(Collateral::Perp)
        }
        None => Err(format!("userAbstraction: expected a string, got {v}")),
    }
}

/// The collateral of a unified / portfolio-margin account: the USDC row's `total` of
/// `spotClearinghouseState.balances[]` (`total` includes `hold`, the part pledged to open orders and
/// margin). The venue omits a coin it holds none of, so a `balances` array with no USDC row is a
/// real `0`; a body with no `balances` array is malformed (`Err`), never read as 0.
fn parse_unified_balance(body: &str) -> Result<f64, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v
        .get("balances")
        .and_then(|b| b.as_array())
        .ok_or_else(|| "spotClearinghouseState: expected a `balances` array".to_string())?;
    Ok(rows
        .iter()
        .find(|c| c.get("coin").and_then(|x| x.as_str()) == Some("USDC"))
        .and_then(|row| row.get("total"))
        .and_then(json_num)
        .unwrap_or(0.0))
}

/// The balance read, with its three venue reads injected so a test drives it without a network.
/// PERP product: `abstraction()` first; a perp-wallet account then reads `clearinghouseState` exactly
/// as before, a unified one reads `spotClearinghouseState`. SPOT product: the spot read, no
/// abstraction. Fail-closed: any failed read is the whole read's `Err` — never a 0 or a stale figure.
fn read_balance(
    product: Product,
    user: &str,
    abstraction: impl FnOnce() -> Result<Collateral, String>,
    info: impl Fn(&Value) -> Result<String, String>,
) -> Result<Option<f64>, String> {
    let perp = json!({ "type": "clearinghouseState", "user": user });
    let spot = json!({ "type": "spotClearinghouseState", "user": user });
    match product {
        Product::Spot => parse_balance(&info(&spot)?, Product::Spot),
        Product::Perp => match abstraction()? {
            Collateral::Perp => parse_balance(&info(&perp)?, Product::Perp),
            Collateral::SpotUsdc => parse_unified_balance(&info(&spot)?).map(Some),
        },
    }
}

// --- the client --------------------------------------------------------------------------------

/// The Hyperliquid reconcile client: a [`HyperliquidTransport`] (keyless `/info`), the **master**
/// account address every read scopes to, and the configured [`Product`] (the balance-endpoint axis).
pub struct HyperliquidReconClient {
    transport: HyperliquidTransport,
    master_address: String,
    product: Product,
    /// The last successful `userAbstraction` read and when it was made ([`ABSTRACTION_TTL`]).
    abstraction: Mutex<Option<(Instant, Collateral)>>,
    /// The start's universe, when the caller gave it ([`Self::with_instruments`]): names a spot pair's orders,
    /// fills and holdings by the engine's symbol (`@107` -> `HYPE/USDC`) and nets a spot fee paid in the base
    /// token out of a fill (`super::spot`), exactly as the live stream does. `None` reads every row verbatim
    /// (a perpetual's coin IS its symbol), as this client always did.
    instruments: Option<Arc<HyperliquidInstruments>>,
    /// The symbols the engine was admitted that these reads do NOT already span (decision 0218): a builder-dex
    /// coin (`dex:COIN`, its orders and positions are read per dex) and a spot pair (`BASE/QUOTE`, its holding is
    /// a balance row), in the engine's spelling, append-only and bounded by the engine's own cap.
    admitted: Mutex<Vec<String>>,
}

impl HyperliquidReconClient {
    /// Build a reconcile client. `master_address` is the `0x…` MASTER account address (NOT the agent
    /// wallet — agent reads return empty); `product` selects the balance endpoint.
    pub fn new(
        transport: HyperliquidTransport,
        master_address: impl Into<String>,
        product: Product,
    ) -> Self {
        HyperliquidReconClient {
            transport,
            master_address: master_address.into(),
            product,
            abstraction: Mutex::new(None),
            instruments: None,
            admitted: Mutex::new(Vec::new()),
        }
    }

    /// Give the client the universe it names rows with (see the field).
    #[must_use]
    pub fn with_instruments(mut self, instruments: Arc<HyperliquidInstruments>) -> Self {
        self.instruments = Some(instruments);
        self
    }

    /// The builder dexes the admitted coins live on, in admission order, without repeats.
    fn admitted_dexes(&self) -> Vec<String> {
        let held = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        let mut dexes: Vec<String> = Vec::new();
        for dex in held.iter().filter_map(|s| s.split_once(':').map(|(dex, _)| dex)) {
            if !dexes.iter().any(|d| d == dex) {
                dexes.push(dex.to_string());
            }
        }
        dexes
    }

    /// The admitted SPOT pairs (`BASE/QUOTE`), resolved in the start's universe; a pair it does not list is
    /// skipped (it was listed after the start: its holding is not read until the account's next start).
    fn admitted_pairs(&self) -> Vec<InstrumentRef> {
        let Some(universe) = &self.instruments else { return Vec::new() };
        let held = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        held.iter()
            .filter(|s| s.contains('/'))
            .filter_map(|s| universe.symbology().by_symbol(s))
            .filter(|i| matches!(i.product, Product::Spot))
            .cloned()
            .collect()
    }

    /// Name a row's coin by the engine's symbol (`@107` -> `HYPE/USDC`); a coin the universe does not know, and a
    /// client built without one, leave it verbatim.
    fn symbol_of(&self, coin: &str) -> Option<&InstrumentRef> {
        self.instruments.as_ref()?.symbology().by_coin(coin)
    }

    /// The account's collateral home: the cached answer while younger than [`ABSTRACTION_TTL`], else
    /// `fetch` (an `Err` is returned and not cached).
    fn collateral(
        &self,
        now: Instant,
        fetch: impl FnOnce() -> Result<Collateral, String>,
    ) -> Result<Collateral, String> {
        let mut slot = self.abstraction.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, c)) = *slot
            && now.saturating_duration_since(at) < ABSTRACTION_TTL
        {
            return Ok(c);
        }
        let c = fetch()?;
        *slot = Some((now, c));
        Ok(c)
    }

    /// One keyless `POST /info` with a `{type, user, …}` body, the 200 body re-serialized once for
    /// the pure `parse_*` seam (the Bybit `result` pairing — HL has no envelope to unwrap).
    fn info(&self, body: &Value) -> Result<String, String> {
        self.transport.info(body).map(|v| v.to_string()).map_err(|e| e.msg)
    }
}

impl ReconClient for HyperliquidReconClient {
    /// `since` is a no-op: `frontendOpenOrders` has no time filter and reports only the currently
    /// open set (like Bybit's `order/realtime`).
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let user = self.master_address.as_str();
        let body = self.info(&json!({ "type": "frontendOpenOrders", "user": user }))?;
        let mut out = parse_orders(&body)?;
        // A builder dex keeps its own book: the orders on an admitted dex coin are read per dex, and the
        // whole fetch fails when any read does (an order a successful report lacks is read as gone).
        for dex in self.admitted_dexes() {
            let body =
                self.info(&json!({ "type": "frontendOpenOrders", "user": user, "dex": dex }))?;
            out.extend(parse_orders(&body)?);
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|o| o.venue_order_id.is_empty() || seen.insert(o.venue_order_id.to_string()));
        for order in &mut out {
            if let Some(inst) = self.symbol_of(&order.symbol) {
                order.symbol = inst.symbol.clone();
            }
        }
        Ok(out)
    }

    /// `userFillsByTime` with `startTime = since` when `since > 0`, else the unbounded `userFills`.
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let user = self.master_address.as_str();
        let req = if since > 0 {
            json!({ "type": "userFillsByTime", "user": user, "startTime": since })
        } else {
            json!({ "type": "userFills", "user": user })
        };
        let body = self.info(&req)?;
        let mut out = parse_fills(&body)?;
        for fill in &mut out {
            if let Some(inst) = self.symbol_of(&fill.symbol) {
                fill.symbol = inst.symbol.clone();
                // The same netting as the live stream's, so the two agree to the digit (decision 0218).
                super::spot::net_fill_report(fill, inst);
            }
        }
        Ok(out)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let user = self.master_address.as_str();
        let body = self.info(&json!({ "type": "clearinghouseState", "user": user }))?;
        let mut out = parse_positions(&body)?;
        for dex in self.admitted_dexes() {
            let body =
                self.info(&json!({ "type": "clearinghouseState", "user": user, "dex": dex }))?;
            out.extend(parse_positions(&body)?);
        }
        // A spot holding is a balance row, not a position: read once for every admitted pair.
        let pairs = self.admitted_pairs();
        if !pairs.is_empty() {
            let body = self.info(&json!({ "type": "spotClearinghouseState", "user": user }))?;
            out.extend(parse_spot_positions(&body, &pairs, vike_model::now_ms())?);
        }
        Ok(out)
    }

    /// Remember the engine's admitted symbols that these reads do not already span (decision 0218): a builder-dex
    /// coin adds its dex to the per-dex reads, a spot pair adds its holding to the position read. A core perp
    /// needs nothing (the account-wide reads cover it), the mounted symbol is already read, and past the
    /// engine's own cap nothing is kept.
    fn admit_symbol(&self, symbol: &str) {
        if !(symbol.contains(':') || symbol.contains('/')) {
            return;
        }
        let mut held = self.admitted.lock().unwrap_or_else(PoisonError::into_inner);
        if held.len() < vike_exec::MAX_EXTRA_SYMBOLS && !held.iter().any(|s| s == symbol) {
            held.push(symbol.to_string());
        }
    }

    /// Absolute account balance (USD) per the configured [`Product`]: PERP
    /// `marginSummary.accountValue` — or, for a `unifiedAccount` / `portfolioMargin` account (read
    /// from `userAbstraction`, cached [`ABSTRACTION_TTL`]), the spot USDC `total` that collateralises
    /// its perps — and SPOT the USDC `total`. Overrides the trait default (`Ok(None)`) so
    /// `dyn ReconClient` dispatch uses HL's real balance. A failed abstraction or spot read is an
    /// `Err` ([`read_balance`]).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        read_balance(
            self.product,
            &self.master_address,
            || {
                self.collateral(Instant::now(), || {
                    let req = json!({ "type": "userAbstraction", "user": self.master_address });
                    parse_abstraction(&self.info(&req)?)
                })
            },
            |req| self.info(req),
        )
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — a thin type-erasing
/// wrapper over [`HyperliquidReconClient::new`]. UNLIKE every other bridge's `recon_client`, this
/// one does NOT resolve `creds`/`env` itself: HL's `transport`/`master`/`product` are the OUTPUT of
/// the SAME `instruments` fetch `crate::adapter::live_runtime_for_account` already performs
/// to build the exec client (the meta/spotMeta load that resolves `product` for the started
/// symbol) — recomputing them here would cost a second network round-trip and risk exec/recon
/// disagreeing about which product a symbol routes to. So the caller builds those three pieces
/// once and hands them in; this function only performs the trivial (infallible) construction +
/// trait-object erasure every bridge's factory does, under the SAME `recon_client` name.
pub fn recon_client(
    transport: HyperliquidTransport,
    master_address: impl Into<String>,
    product: Product,
) -> Box<dyn ReconClient> {
    Box::new(HyperliquidReconClient::new(transport, master_address, product))
}

#[path = "recon_client_tests.rs"]
#[cfg(test)]
mod recon_client_tests;
