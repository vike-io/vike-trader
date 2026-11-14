//! Hyperliquid `ReconcileClient` — the venue-facing report seam (`vike_execution::recon::ReconcileClient`).
//!
//! All reads are **keyless** `POST /info` against the **MASTER** account address (never the agent
//! wallet — agent-address reads return empty; research §8 pitfall #9). Mirrors the Bybit
//! `recon_client` template: every fetch delegates to a PURE `parse_*(body: &str) -> Result<Vec<…>,
//! String>` free function — the fixture-tested unit (recorded `/info` JSON bodies, NO network) — and
//! the [`ReconcileClientHyperliquid`] holds only what a report fetch needs: a [`HyperliquidTransport`],
//! the master address, and the configured [`AssetClass`].
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
//!   `commission`, `feeToken` → `commission_asset` — the same field mapping [`crate::execution::event_mapper`]
//!   applies to the live WS `userFills` stream.
//! - `clearinghouseState.assetPositions[].position` → [`PositionStatusReport`]s. HL `szi` is ALREADY
//!   SIGNED (+ long / − short) → `qty` directly, with NO `side`-string re-signing (unlike Bybit's
//!   unsigned `size` + `side`). HL is one-way/net (no hedge legs) → `PositionSide::Both`. EVERY row
//!   is kept, a flat (`szi == 0`) leg included: `recon::diff` needs the row PRESENT to detect a
//!   locally-open position the venue has since flattened (a reported flat row diffs against the
//!   local qty as `PositionDrift`). NOTE the blind spot: HL OMITS closed positions entirely, and
//!   `vike_execution::recon::diff`'s position leg iterates VENUE rows only — there is NO local-side
//!   position sweep (unlike orders, where step 3 raises `OrphanLocalOrder` for a live local order
//!   the venue no longer reports) — so a cached local position with NO row in the response raises
//!   NO divergence today; its closing activity surfaces through the fill lane (`userFills` →
//!   `MissingFill`) instead. Keeping every reported row is therefore load-bearing, and we never
//!   filter rows here. Mirrors the Bybit/binance "keep every row" choice.
//! - `fetch_balance` per [`AssetClass`]: PERP `clearinghouseState.marginSummary.accountValue`, EXCEPT on
//!   a `unifiedAccount` / `portfolioMargin` account (`userAbstraction`), whose perp summary reads 0
//!   because SPOT USDC is its perp collateral — there the USDC row's `total` in
//!   `spotClearinghouseState.balances[]` is the figure; SPOT the same USDC row.
//!
//! **`fetch_balance`** (the `ReconcileClient` trait's 4th method, default `Ok(None)`) is implemented in
//! the `impl ReconcileClient` block below, overriding the default so `dyn ReconcileClient` dispatch uses HL's
//! real balance. It began as an inherent method while this branch predated the
//! `feat/reconciliation-activation` merge that added the 4th trait method, and moved into the trait
//! impl when that landed in main (#360).
//!
//! Symbols are carried VERBATIM as the venue `coin` string (perp `"BTC"` == the unified symbol; spot
//! `"@<idx>"`), exactly like OKX's recon reports carry `instId` and like the rest of this crate
//! ("symbols are NOT resolved here" — [`crate::execution::event_mapper`]); the recon manager keys on the venue
//! symbol. HL numeric `oid`/`tid` ids stringify via [`vike_bridge_core::json::json_str`]; px/sz/fee
//! are decimal strings decoded via [`json_number`].
//!
//! **Whose id a row carries.** The venue's `cloid` is `keccak(order client id)`, one-way, and the pure parsers
//! keep it verbatim: `state::ExecutionState::cancel_wire_cloid` looks a printed id up in them, and the
//! core's restore translates the hash itself (decision 0121). The CLIENT then names a fetched row of
//! OUR order by our order client id, through the map the account's exec client writes at every submit and every
//! `adopt_restored` ([`ReconcileClientHyperliquid::with_cloids`]), as the live stream does. A `cloid` the
//! map does not hold keeps the hash: it matches no local order, so a foreign order stays foreign, and
//! right after a restart (an empty map) it is exactly the id the core's translation asks
//! `wire_id_for` about.

// An operator's READ of the account (balance, open orders): keyless `/info` against the master address,
// asked and never folded. `exec`-gated for the same reason `leverage_cap` is.
pub(crate) mod venue_read;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_number, json_str};
use vike_bridge_core::ratelimit::RateGatePause;
use vike_bridge_core::reconcile_admitted::{
    ReconcileAdmittedReadSpec, ReconcileAdmittedSymbols, ReconcileReadError, not_backing_off,
};
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_execution::recon::ReconcileClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    AssetClass, FeeSchedule, FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::config::unlisted_class;
use crate::instrument::properties::HyperliquidInstruments;
use crate::instrument::symbology::InstrumentRef;
use crate::transport::HyperliquidTransport;
use crate::transport::consts::VENUE;

use crate::execution::event_mapper::{order_status_word, side_sign};
use crate::execution::registry::OrderClientIdVenueRegistry;

// --- pure parsers ------------------------------------------------------------------------------

/// A venue `cloid` (`0x…`, the hash of an order client id) → `order_client_id`, `None` when absent/empty (an
/// externally-placed order the venue never echoed our id for) — same convention as the Bybit/OKX
/// recon clients. It stays the hash here: only the client holds the map that names it back
/// ([`ReconcileClientHyperliquid::with_cloids`]).
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
/// (the stream's own `is_cancel` and `is_reject`): every `*Canceled` (plus the
/// `-ed`-less `scheduledCancel`) → `Canceled`, every `*Rejected` → `Rejected`, `open`/`resting` →
/// `Accepted`, `filled` → `Filled`, `triggered` → `Triggered`. `frontendOpenOrders` rows carry NO
/// status (all resting), so this only fires for a status-bearing row — the open/partial
/// classification there is derived from fill progress instead.
///
/// The words are the stream's ([`order_status_word`]); a word outside that set is WARNED and read as
/// `Accepted` (`vike_bridge_core::order_status::unknown_status_word`).
fn normalize_order_status(raw: &str) -> OrderStatus {
    order_status_word(raw)
        .unwrap_or_else(|| vike_bridge_core::order_status::unknown_status_word(VENUE, raw))
}

/// Map an HL `orderType` (`frontendOpenOrders` spells it in words: `Limit`, `Market`, `Stop Market`,
/// `Stop Limit`, `Take Profit Market`, `Take Profit Limit`) onto [`OrderType`]: `Limit`/`Market` are
/// their twins, the stop pair `Stop`/`StopLimit`, and both take-profit kinds `TakeProfit` (the limit
/// flavour's limit rides the report's `price`). Any other word has no twin
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
            let orig = o.get("origSz").and_then(json_number);
            let remaining = o.get("sz").and_then(json_number).unwrap_or(0.0);
            let qty = orig.unwrap_or(remaining);
            let filled_qty = (qty - remaining).max(0.0);
            let raw_status = o.get("status").and_then(|s| s.as_str()).unwrap_or("");
            let status = if !raw_status.is_empty() {
                normalize_order_status(raw_status)
            } else {
                OrderStatus::from_fill_progress(filled_qty)
            };
            let raw_type = o.get("orderType").and_then(|t| t.as_str()).unwrap_or("");
            let order_type = normalize_order_type(raw_type);
            // The terms the row states: `limitPx` is a limit only on the three limit words (a
            // trigger MARKET order's `limitPx` is the slippage bound `build_trigger_kind` put off
            // the trigger, not a limit the order named), `triggerPx` a trigger kind's level.
            let px = |k: &str| o.get(k).and_then(json_number).filter(|v| v.is_finite() && *v > 0.0);
            let price = px("limitPx")
                .filter(|_| matches!(raw_type, "Limit" | "Stop Limit" | "Take Profit Limit"));
            let trigger_price = px("triggerPx").filter(|_| order_type.is_trigger());
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: o.get("coin").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                venue_order_id: o.get("oid").map(json_str).unwrap_or_default().into(),
                order_client_id: cloid_opt(o),
                side: side_sign(o),
                order_type,
                qty,
                filled_qty,
                average_price: 0.0,
                status,
                timestamp: o.get("timestamp").and_then(json_int).unwrap_or(0),
                price,
                trigger_price,
                trail: None,
                trail_percent: None,
            }
        })
        .collect())
}

/// `userFills` / `userFillsByTime` (a top-level array) → [`FillReport`]s. `tid` → `trade_id`, `oid` →
/// `venue_order_id` (both HL JSON numbers, stringified via [`json_str`]); `crossed` true → taker,
/// false → maker; `fee` is SIGNED (positive cost / negative rebate — kept as-is, the vike convention
/// [`crate::execution::event_mapper`] also applies); `feeToken` → `commission_asset`; `coin` → `symbol`
/// verbatim.
///
/// A row with no `tid` is **SKIPPED**, not admitted with an empty id. `trade_id` is what
/// `vike_execution::recon::diff` looks up in `seen_trade_ids`; an id-less report can never match, so it
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
                order_client_id: cloid_opt(f),
                side: side_sign(f),
                last_qty: f.get("sz").and_then(json_number).unwrap_or(0.0),
                last_price: f.get("px").and_then(json_number).unwrap_or(0.0),
                commission: vike_bridge_core::json::get_fee_f64(f, "fee", VENUE),
                commission_asset: f
                    .get("feeToken")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                liquidity_side: LiquiditySide::from_maker(!crossed),
                timestamp: f.get("time").and_then(json_int).unwrap_or(0),
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
            // `VenueCapabilities::default_margin_mode` on the 9 isolated-only assets, and the venue is
            // right — see `crate::instrument::symbology::InstrumentRef::effective_margin_mode`. NOTE the one
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
                qty: pos.get("szi").and_then(json_number).unwrap_or(0.0),
                average_price: pos.get("entryPx").and_then(json_number).unwrap_or(0.0),
                timestamp: ts,
                margin_mode: if isolated { MarginMode::Isolated } else { MarginMode::Cross },
                isolated_margin: None,
                delta: None,
            })
        })
        .collect())
}

/// `spotClearinghouseState` -> one [`PositionStatusReport`] per spot pair the engine trades (decision 0218; the
/// mounted one too): a spot holding is a balance row of the pair's BASE token, not a position, so the reconcile read of
/// positions (a perpetual's `clearinghouseState`) never showed it. The report names the pair by the engine's symbol
/// (`HYPE/USDC`) and carries the held `total` (which includes what open orders hold) through the one spot
/// constructor (`PositionStatusReport::spot_holding`: `Cash`, flat under ONE size step, so dust a transfer or a fee
/// left is a FLAT row that is still present for `recon::diff`). Its price is `0.0`, never the wallet's `entryNtl /
/// total`: the wallet's blended cost basis is not the strategy's, and a priced row let a confirm or `hybrid` adopt
/// the user's own coins at it (the spot rule, owner 2026-11-11). A body with no `balances` array is malformed
/// (`Err`), never read as flat.
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
        .map(|instrument| {
            let total = rows
                .iter()
                .find(|r| r.get("coin").and_then(|c| c.as_str()) == Some(instrument.base.as_str()))
                .and_then(|r| r.get("total"))
                .and_then(json_number)
                .unwrap_or(0.0);
            let step = crate::instrument::properties::pow10_neg(instrument.sz_decimals);
            PositionStatusReport::spot_holding(VENUE, &instrument.symbol, total, step, ts)
        })
        .collect())
}

/// Account balance per [`AssetClass`] (research §8): PERP = `clearinghouseState.marginSummary
/// .accountValue` (total account value in USD); SPOT = the USDC row's `total` in
/// `spotClearinghouseState.balances[]`. `Ok(None)` when the field/row is absent (venue surfaced no
/// balance); malformed JSON, and a class this venue lists none of (`unlisted_class`), are an `Err`.
/// All HL numerics are decimal STRINGS.
pub fn parse_balance(body: &str, class: AssetClass) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(match class {
        AssetClass::CryptoPerp => {
            v.get("marginSummary").and_then(|m| m.get("accountValue")).and_then(json_number)
        }
        AssetClass::CryptoSpot => v
            .get("balances")
            .and_then(|b| b.as_array())
            .and_then(|arr| {
                arr.iter().find(|c| c.get("coin").and_then(|x| x.as_str()) == Some("USDC"))
            })
            .and_then(|row| row.get("total"))
            .and_then(json_number),
        other => return Err(unlisted_class(other)),
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
        .and_then(json_number)
        .unwrap_or(0.0))
}

/// The balance read, with its three venue reads injected so a test drives it without a network.
/// PERP class: `abstraction()` first; a perp-wallet account then reads `clearinghouseState` exactly
/// as before, a unified one reads `spotClearinghouseState`. SPOT class: the spot read, no
/// abstraction. Any other class is refused before anything is asked (`unlisted_class`). Fail-closed:
/// any failed read is the whole read's `Err` — never a 0 or a stale figure.
fn read_balance(
    class: AssetClass,
    user: &str,
    abstraction: impl FnOnce() -> Result<Collateral, String>,
    info: impl Fn(&Value) -> Result<String, String>,
) -> Result<Option<f64>, String> {
    let perp = json!({ "type": "clearinghouseState", "user": user });
    let spot = json!({ "type": "spotClearinghouseState", "user": user });
    match class {
        AssetClass::CryptoSpot => parse_balance(&info(&spot)?, AssetClass::CryptoSpot),
        AssetClass::CryptoPerp => match abstraction()? {
            Collateral::Perp => parse_balance(&info(&perp)?, AssetClass::CryptoPerp),
            Collateral::SpotUsdc => parse_unified_balance(&info(&spot)?).map(Some),
        },
        other => Err(unlisted_class(other)),
    }
}

/// `userFees` -> the account's own maker/taker rate for `class` (FEES-AUDIT 2026-11-08): `userAddRate` /
/// `userCrossRate` for a perpetual, `userSpotAddRate` / `userSpotCrossRate` for a spot pair. Those are the
/// account's TIER rate with its staking discount already in (the venue's example answers `0.000315` against a
/// `0.00045` base at a 30 % discount); the referral discount (`activeReferralDiscount`) is applied on top, to a
/// maker FEE and to the taker rate but not to a maker rebate, exactly as the venue's fee formula
/// (`hyperliquid-docs/trading/fees`, "Fee formula for developers") does for a validator-operated perpetual.
/// `Ok(None)` when either rate is absent; a body that is not an object, and a class this venue lists none of
/// (`unlisted_class`), are an `Err`.
///
/// shortcut: the venue's aligned-quote and stable-pair (spot quote asset against spot quote asset, 80 % lower)
/// multipliers are not applied: neither applies to a USDC perpetual or a spot pair against USDC; upgrade when
/// such a market is mounted.
pub fn parse_user_fees(body: &str, class: AssetClass) -> Result<Option<FeeSchedule>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err(format!("userFees: expected an object, got {v}"));
    }
    let (add, cross) = match class {
        AssetClass::CryptoPerp => ("userAddRate", "userCrossRate"),
        AssetClass::CryptoSpot => ("userSpotAddRate", "userSpotCrossRate"),
        other => return Err(unlisted_class(other)),
    };
    let (Some(maker), Some(taker)) =
        (v.get(add).and_then(json_number), v.get(cross).and_then(json_number))
    else {
        return Ok(None);
    };
    let referral = v.get("activeReferralDiscount").and_then(json_number).unwrap_or(0.0);
    let maker = if maker > 0.0 { maker * (1.0 - referral) } else { maker };
    Ok(Some(FeeSchedule::from_fractions(maker, taker * (1.0 - referral))))
}

/// The words a HIP-3 builder-dex perpetual (`dex:COIN`) is refused a fee read in, or `None` for any other
/// mount. Its fee is the account's perp rate times the deployer's fee scale `s` (`1 + s` under 1, `2s` from 1,
/// `s` up to 3), a tenth of that in growth mode, with an aligned-quote discount on top (the venue's fee
/// formula): none of `/info`'s documented reads states the scale, so the account's core perp rate, which
/// would UNDERSTATE it, is not reported as this mount's.
fn hip3_fee_refusal(symbol: &str) -> Option<String> {
    symbol.contains(':').then(|| {
        format!(
            "{symbol} is a HIP-3 builder-dex perpetual: its fee is the account's perp rate times the \
             deployer's fee scale (1+s under 1, 2s from 1, a tenth in growth mode), which no read this bridge \
             makes states, so the account's core perp rate is not reported as its fee schedule"
        )
    })
}

/// The fee read, with its one venue read injected so a test drives it without a network: a HIP-3 mount is
/// refused before anything is asked ([`hip3_fee_refusal`], said at WARN), any other asks `userFees` of the
/// MASTER address and reads it for `class` ([`parse_user_fees`]).
fn read_fee_rates(
    class: AssetClass,
    mounted: Option<&str>,
    user: &str,
    info: impl FnOnce(&Value) -> Result<String, String>,
) -> Result<Option<FeeSchedule>, String> {
    if let Some(why) = mounted.and_then(hip3_fee_refusal) {
        tracing::warn!(venue = VENUE, %why, "the account's fee schedule is not read for this mount");
        return Err(why);
    }
    parse_user_fees(&info(&json!({ "type": "userFees", "user": user }))?, class)
}

// --- the client --------------------------------------------------------------------------------

/// The Hyperliquid reconcile client: a [`HyperliquidTransport`] (keyless `/info`), the **master**
/// account address every read scopes to, and the configured [`AssetClass`] (the balance-endpoint axis).
pub struct ReconcileClientHyperliquid {
    transport: HyperliquidTransport,
    master_address: String,
    asset_class: AssetClass,
    /// The last successful `userAbstraction` read and when it was made ([`ABSTRACTION_TTL`]).
    abstraction: Mutex<Option<(Instant, Collateral)>>,
    /// The start's universe, when the caller gave it ([`Self::with_instruments`]): names a spot pair's orders,
    /// fills and holdings by the engine's symbol (`@107` -> `HYPE/USDC`) and nets a spot fee paid in the base
    /// token out of a fill (`crate::execution::spot`), exactly as the live stream does. `None` reads every row verbatim
    /// (a perpetual's coin IS its symbol), as this client always did.
    instruments: Option<Arc<HyperliquidInstruments>>,
    /// The symbols the engine was admitted that these reads do NOT already span (decision 0218): a builder-dex
    /// coin (`dex:COIN`, its orders and positions are read per dex) and a spot pair (`BASE/QUOTE`, its holding is
    /// a balance row), in the engine's spelling (case kept: a dex and a pair name are case-sensitive), bounded by the
    /// engine's own cap; it shrinks only when the engine gives a symbol back (`forget_symbol`).
    admitted: ReconcileAdmittedSymbols,
    /// The account's `cloid -> order_client_id` map, shared with its exec client ([`Self::with_cloids`]). Empty for a
    /// client built without one, which reads every `cloid` verbatim.
    cloids: OrderClientIdVenueRegistry,
    /// The engine symbol the account was mounted on ([`Self::with_mounted_symbol`]): a HIP-3 one (`dex:COIN`) is
    /// refused the fee read ([`hip3_fee_refusal`]). `None` reads the account's rate for `asset_class`.
    mounted: Option<String>,
    /// The pass-wide quiet period after a read made for an admitted symbol was answered a rate limit
    /// ([`ADMITTED_READ`]).
    pause: RateGatePause,
}

fn is_rate_limited(e: &VenueApiError) -> bool {
    e.kind() == ErrorKind::RateLimited
}

/// No read here is judged "not listed": a dex the venue does not know answers `null`, which fails the
/// whole fetch, as it must.
fn never_not_listed(_: &VenueApiError) -> bool {
    false
}

fn venue_words(e: VenueApiError) -> String {
    e.msg
}

/// How a read made for ADMITTED symbols (a builder dex's book, the spot holdings) is judged (owner ruling
/// B-5): a rate limit (the `/info` door's last answer after its own bounded retries) makes every fetch of the
/// next `vike_bridge_core::reconcile_admitted::RATE_LIMIT_BACKOFF` refuse before it asks; the error text is
/// the venue's words, as every fetch has always surfaced them.
const ADMITTED_READ: ReconcileAdmittedReadSpec = ReconcileAdmittedReadSpec {
    not_listed: never_not_listed,
    rate_limited: is_rate_limited,
    text: venue_words,
};

impl ReconcileClientHyperliquid {
    /// Build a reconcile client. `master_address` is the `0x…` MASTER account address (NOT the agent
    /// wallet — agent reads return empty); `asset_class` selects the balance endpoint (a class this venue lists
    /// none of is refused at the read, `unlisted_class`).
    pub fn new(
        transport: HyperliquidTransport,
        master_address: impl Into<String>,
        asset_class: AssetClass,
    ) -> Self {
        ReconcileClientHyperliquid {
            transport,
            master_address: master_address.into(),
            asset_class,
            abstraction: Mutex::new(None),
            instruments: None,
            admitted: ReconcileAdmittedSymbols::case_kept(),
            cloids: OrderClientIdVenueRegistry::new(),
            mounted: None,
            pause: RateGatePause::default(),
        }
    }

    /// Name the engine symbol the account was mounted on (see the field).
    #[must_use]
    pub fn with_mounted_symbol(mut self, symbol: &str) -> Self {
        self.mounted = Some(symbol.to_string());
        self
    }

    /// Give the client the universe it names rows with (see the field).
    #[must_use]
    pub fn with_instruments(mut self, instruments: Arc<HyperliquidInstruments>) -> Self {
        self.instruments = Some(instruments);
        self
    }

    /// Give the client the account's `cloid -> order_client_id` map
    /// (the one the start hands [`crate::execution::client::spawn_with_market_slippage`]), so a row of our own order reads by our
    /// order client id.
    #[must_use]
    pub fn with_cloids(mut self, cloids: OrderClientIdVenueRegistry) -> Self {
        self.cloids = cloids;
        self
    }

    /// A fetched order row in the engine's words: the coin by its symbol, our order by our order client id.
    pub(crate) fn name_order(&self, order: &mut OrderStatusReport) {
        if let Some(instrument) = self.symbol_of(&order.symbol) {
            order.symbol = instrument.symbol.clone();
        }
        self.name_ours(&mut order.order_client_id);
    }

    /// A fetched fill row in the engine's words: [`Self::name_order`], and a spot fee netted out.
    pub(crate) fn name_fill(&self, fill: &mut FillReport) {
        if let Some(instrument) = self.symbol_of(&fill.symbol) {
            fill.symbol = instrument.symbol.clone();
            // The same netting as the live stream's, so the two agree to the digit (decision 0218).
            crate::execution::spot::net_fill_report(fill, instrument);
        }
        self.name_ours(&mut fill.order_client_id);
    }

    /// A `cloid` the account's map holds becomes our order client id; any other id stays as the venue sent it (the
    /// module doc says why a miss must keep the hash).
    fn name_ours(&self, id: &mut Option<String>) {
        if let Some(order_client_id) = id.as_deref().and_then(|cloid| self.cloids.resolve(cloid)) {
            *id = Some(order_client_id);
        }
    }

    /// The builder dexes the admitted coins live on, in admission order, without repeats.
    fn admitted_dexes(&self) -> Vec<String> {
        let held = self.admitted.symbols();
        let mut dexes: Vec<String> = Vec::new();
        for dex in held.iter().filter_map(|s| s.split_once(':').map(|(dex, _)| dex)) {
            if !dexes.iter().any(|d| d == dex) {
                dexes.push(dex.to_string());
            }
        }
        dexes
    }

    /// The SPOT pairs whose holdings the position read reports: the MOUNTED pair when the account is mounted on
    /// one (the spot rule: its holding is checked against the wallet like any other), then the admitted pairs
    /// (`BASE/QUOTE`), each once, resolved in the start's universe; a pair it does not list is skipped (it was
    /// listed after the start: its holding is not read until the account's next start).
    fn spot_pairs(&self) -> Vec<InstrumentRef> {
        let Some(universe) = &self.instruments else { return Vec::new() };
        let held = self.admitted.symbols();
        let mut pairs: Vec<InstrumentRef> = Vec::new();
        for s in self.mounted.iter().chain(held.iter().filter(|s| s.contains('/'))) {
            if let Some(instrument) = universe.symbology().by_symbol(s)
                && instrument.asset_class == AssetClass::CryptoSpot
                && !pairs.iter().any(|p| p.symbol == instrument.symbol)
            {
                pairs.push(instrument.clone());
            }
        }
        pairs
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

    /// [`Self::info`] made for the ADMITTED symbols `what` names (a dex, the spot pairs), parsed by
    /// `parse` and judged by [`ADMITTED_READ`].
    fn admitted_read<R>(
        &self,
        what: &str,
        body: &Value,
        parse: impl FnOnce(&str) -> Result<Vec<R>, String>,
    ) -> Result<Vec<R>, String> {
        let read = self
            .transport
            .info(body)
            .map_err(ReconcileReadError::Venue)
            .and_then(|v| parse(&v.to_string()).map_err(ReconcileReadError::Parse));
        ADMITTED_READ.admitted_rows(VENUE, what, &self.pause, read)
    }
}

impl ReconcileClient for ReconcileClientHyperliquid {
    /// `since` is a no-op: `frontendOpenOrders` has no time filter and reports only the currently
    /// open set (like Bybit's `order/realtime`).
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let user = self.master_address.as_str();
        let body = self.info(&json!({ "type": "frontendOpenOrders", "user": user }))?;
        let mut out = parse_orders(&body)?;
        // A builder dex keeps its own book: the orders on an admitted dex coin are read per dex, and the
        // whole fetch fails when any read does (an order a successful report lacks is read as gone).
        for dex in self.admitted_dexes() {
            let req = json!({ "type": "frontendOpenOrders", "user": user, "dex": dex });
            out.extend(self.admitted_read(&dex, &req, parse_orders)?);
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|o| o.venue_order_id.is_empty() || seen.insert(o.venue_order_id.to_string()));
        for order in &mut out {
            self.name_order(order);
        }
        Ok(out)
    }

    /// `userFillsByTime` with `startTime = since` when `since > 0`, else the unbounded `userFills`.
    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let user = self.master_address.as_str();
        let req = if since > 0 {
            json!({ "type": "userFillsByTime", "user": user, "startTime": since })
        } else {
            json!({ "type": "userFills", "user": user })
        };
        let body = self.info(&req)?;
        let mut out = parse_fills(&body)?;
        for fill in &mut out {
            self.name_fill(fill);
        }
        Ok(out)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        not_backing_off(VENUE, &self.pause)?;
        let user = self.master_address.as_str();
        let body = self.info(&json!({ "type": "clearinghouseState", "user": user }))?;
        let mut out = parse_positions(&body)?;
        for dex in self.admitted_dexes() {
            let req = json!({ "type": "clearinghouseState", "user": user, "dex": dex });
            out.extend(self.admitted_read(&dex, &req, parse_positions)?);
        }
        // A spot holding is a balance row, not a position: read once for every spot pair, the mounted one included.
        let pairs = self.spot_pairs();
        if !pairs.is_empty() {
            let what: Vec<&str> = pairs.iter().map(|p| p.symbol.as_str()).collect();
            let req = json!({ "type": "spotClearinghouseState", "user": user });
            out.extend(self.admitted_read(&what.join(","), &req, |body| {
                parse_spot_positions(body, &pairs, vike_model::now_ms())
            })?);
        }
        Ok(out)
    }

    /// Remember the engine's admitted symbols that these reads do not already span (decision 0218): a builder-dex
    /// coin adds its dex to the per-dex reads, a spot pair adds its holding to the position read. A core perp
    /// needs nothing (the account-wide reads cover it), the mounted symbol is already read, and past the
    /// engine's own cap nothing is kept.
    fn admit_symbol(&self, symbol: &str) {
        if symbol.contains(':') || symbol.contains('/') {
            // No mounted symbol to compare: this client never compared it (the engine never admits it).
            self.admitted.admit(symbol, "");
        }
    }

    /// The engine GAVE `symbol` BACK (decision 0205, owner ruling F-3): the next pass stops reading it and its
    /// place under the cap is free, so the engine's next admission is read. Matched as `admit_symbol` stores it
    /// (case kept: `kPEPE`); a symbol never held is a no-op.
    fn forget_symbol(&self, symbol: &str) {
        self.admitted.forget(symbol);
    }

    /// Absolute account balance (USD) per the configured [`AssetClass`]: PERP
    /// `marginSummary.accountValue` — or, for a `unifiedAccount` / `portfolioMargin` account (read
    /// from `userAbstraction`, cached [`ABSTRACTION_TTL`]), the spot USDC `total` that collateralises
    /// its perps — and SPOT the USDC `total`. Overrides the trait default (`Ok(None)`) so
    /// `dyn ReconcileClient` dispatch uses HL's real balance. A failed abstraction or spot read is an
    /// `Err` ([`read_balance`]).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        read_balance(
            self.asset_class,
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

    /// The account's own fee tier (`userFees` of the MASTER address, [`read_fee_rates`]): the rate the
    /// account is really charged for the mount's class, in place of the static base-tier row, as
    /// binance, bybit and okx read theirs. A HIP-3 mount is refused in words.
    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        read_fee_rates(self.asset_class, self.mounted.as_deref(), &self.master_address, |req| {
            self.info(req)
        })
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam, wave-2 task 6) — a thin type-erasing
/// wrapper over [`ReconcileClientHyperliquid::new`]. UNLIKE every other bridge's `recon_client`, this
/// one does NOT resolve `creds`/`env` itself: HL's `transport`/`master`/`asset_class` are the OUTPUT of
/// the SAME `instruments` fetch `crate::adapter::live_runtime_for_account` already performs
/// to build the exec client (the meta/spotMeta load that resolves `asset_class` for the started
/// symbol) — recomputing them here would cost a second network round-trip and risk exec/recon
/// disagreeing about which class a symbol routes to. So the caller builds those three pieces
/// once and hands them in; this function only performs the trivial (infallible) construction +
/// trait-object erasure every bridge's factory does, under the SAME `recon_client` name.
pub fn recon_client(
    transport: HyperliquidTransport,
    master_address: impl Into<String>,
    asset_class: AssetClass,
) -> Box<dyn ReconcileClient> {
    Box::new(ReconcileClientHyperliquid::new(transport, master_address, asset_class))
}

#[path = "reconcile_tests.rs"]
#[cfg(test)]
mod reconcile_tests;
