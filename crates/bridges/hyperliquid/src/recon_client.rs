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
//!   `commission`, `feeToken` → `commission_asset` — the same field mapping [`crate::event_mapper`]
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
//! - `fetch_balance` per [`Product`]: PERP `clearinghouseState.marginSummary.accountValue`; SPOT the
//!   USDC row's `total` in `spotClearinghouseState.balances[]`.
//!
//! **`fetch_balance`** (the `ReconClient` trait's 4th method, default `Ok(None)`) is implemented in
//! the `impl ReconClient` block below, overriding the default so `dyn ReconClient` dispatch uses HL's
//! real balance. It began as an inherent method while this branch predated the
//! `feat/reconciliation-activation` merge that added the 4th trait method, and moved into the trait
//! impl when that landed in main (#360).
//!
//! Symbols are carried VERBATIM as the venue `coin` string (perp `"BTC"` == the unified symbol; spot
//! `"@<idx>"`), exactly like OKX's recon reports carry `instId` and like the rest of this crate
//! ("symbols are NOT resolved here" — [`crate::event_mapper`]); the recon manager keys on the venue
//! symbol. HL numeric `oid`/`tid` ids stringify via [`vike_bridge_core::json::json_str`]; px/sz/fee
//! are decimal strings decoded via [`json_num`].

use serde_json::{Value, json};

use vike_bridge_core::json::{json_int, json_num, json_str};
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::config::Product;
use crate::consts::VENUE;
use crate::transport::HyperliquidTransport;

// --- pure parsers ------------------------------------------------------------------------------

/// HL side field → sign: `"A"` (ask) = sell = −1; `"B"` (bid) and anything else = buy = +1 (matches
/// [`crate::event_mapper`]'s `side_sign`).
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

/// Normalize an HL order-status string to the `OrderStatus::parse` FSM vocabulary — the §10 29-value
/// enum mapped by SUFFIX (mirrors [`crate::event_mapper`]'s `is_cancel`/`is_reject`): every
/// `*Canceled` (plus the `-ed`-less `scheduledCancel`) → `CANCELED`, every `*Rejected` → `REJECTED`,
/// `open`/`resting` → `ACCEPTED`, `filled` → `FILLED`, `triggered` → `TRIGGERED`; anything unknown
/// upper-cases as a never-panic soft fallback. `frontendOpenOrders` rows carry NO status (all
/// resting), so this only fires for a status-bearing row — the open/partial classification there is
/// derived from fill progress instead.
fn normalize_order_status(raw: &str) -> String {
    match raw {
        "open" | "resting" => "ACCEPTED".to_string(),
        "filled" => "FILLED".to_string(),
        "triggered" => "TRIGGERED".to_string(),
        s if s == "canceled" || s.ends_with("Canceled") || s == "scheduledCancel" => {
            "CANCELED".to_string()
        }
        s if s == "rejected" || s.ends_with("Rejected") => "REJECTED".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// `frontendOpenOrders` (a top-level array) → [`OrderStatusReport`]s. `origSz` is the ORIGINAL order
/// size and `sz` the REMAINING unfilled size, so `qty = origSz` and `filled_qty = origSz − sz`;
/// `avgPrice` is not in this payload → `avg_px = 0.0`. Status is DERIVED (rows are all resting): any
/// fill progress → `PARTIALLY_FILLED`, else `ACCEPTED` — unless a row carries an explicit `status`,
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
                "PARTIALLY_FILLED".to_string()
            } else {
                "ACCEPTED".to_string()
            };
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: o.get("coin").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                venue_order_id: o.get("oid").map(json_str).unwrap_or_default().into(),
                client_order_id: cloid_opt(o),
                side: side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                order_type: o
                    .get("orderType")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase(),
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
/// [`crate::event_mapper`] also applies); `feeToken` → `commission_asset`; `coin` → `symbol`
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
            // right — see `crate::symbology::InstrumentRef::effective_margin_mode`. NOTE the one
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

// --- the client --------------------------------------------------------------------------------

/// The Hyperliquid reconcile client: a [`HyperliquidTransport`] (keyless `/info`), the **master**
/// account address every read scopes to, and the configured [`Product`] (the balance-endpoint axis).
pub struct HyperliquidReconClient {
    transport: HyperliquidTransport,
    master_address: String,
    product: Product,
}

impl HyperliquidReconClient {
    /// Build a reconcile client. `master_address` is the `0x…` MASTER account address (NOT the agent
    /// wallet — agent reads return empty); `product` selects the balance endpoint.
    pub fn new(
        transport: HyperliquidTransport,
        master_address: impl Into<String>,
        product: Product,
    ) -> Self {
        HyperliquidReconClient { transport, master_address: master_address.into(), product }
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
        parse_orders(&body)
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
        parse_fills(&body)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let user = self.master_address.as_str();
        let body = self.info(&json!({ "type": "clearinghouseState", "user": user }))?;
        parse_positions(&body)
    }

    /// Absolute account balance (USD) per the configured [`Product`]: PERP
    /// `marginSummary.accountValue`, SPOT the USDC `total`. Overrides the trait default (`Ok(None)`)
    /// so `dyn ReconClient` dispatch uses HL's real balance (the reconciliation engine merged in
    /// #360, so this now lives in the trait impl rather than as an inherent method).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let user = self.master_address.as_str();
        let req = match self.product {
            Product::Perp => json!({ "type": "clearinghouseState", "user": user }),
            Product::Spot => json!({ "type": "spotClearinghouseState", "user": user }),
        };
        let body = self.info(&req)?;
        parse_balance(&body, self.product)
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6) — a thin type-erasing
/// wrapper over [`HyperliquidReconClient::new`]. UNLIKE every other bridge's `recon_client`, this
/// one does NOT resolve `creds`/`env` itself: HL's `transport`/`master`/`product` are the OUTPUT of
/// the SAME `instruments` fetch `vike_mount::hyperliquid::hyperliquid_live_client` already performs
/// to build the exec client (the meta/spotMeta load that resolves `product` for the mounted
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Network;

    // --- parse_orders (frontendOpenOrders: a bare array; rows all resting, no status field) ---

    #[test]
    fn parse_orders_open_and_partial_derive_status_from_fill_progress() {
        // frontendOpenOrders: top-level ARRAY; oid/timestamp are NUMBERS; px/sz are STRINGS.
        let body = r#"[
            {"coin":"BTC","side":"B","limitPx":"50000.0","sz":"0.1","origSz":"0.1","oid":91490942,"timestamp":1681247412573,"orderType":"Limit","cloid":"0xabc","reduceOnly":false},
            {"coin":"@107","side":"A","limitPx":"30.0","sz":"2.0","origSz":"5.0","oid":91490943,"timestamp":1681247412600,"orderType":"Limit"}
        ]"#;
        let r = parse_orders(body).unwrap();
        assert_eq!(r.len(), 2);

        let open = &r[0];
        assert_eq!(open.venue, "hyperliquid");
        assert_eq!(open.symbol, "BTC", "coin carried verbatim (== unified perp symbol)");
        assert_eq!(open.venue_order_id.as_str(), "91490942", "numeric oid stringified");
        assert_eq!(open.client_order_id.as_deref(), Some("0xabc"));
        assert_eq!(open.side, 1, "B -> +1");
        assert_eq!(open.order_type, "limit");
        assert_eq!(open.qty, 0.1);
        assert_eq!(open.filled_qty, 0.0);
        assert_eq!(open.avg_px, 0.0);
        assert_eq!(open.status, "ACCEPTED", "fully resting -> ACCEPTED");
        assert_eq!(open.ts, 1681247412573);

        let partial = &r[1];
        assert_eq!(partial.symbol, "@107", "spot coin string carried verbatim");
        assert_eq!(partial.client_order_id, None, "absent cloid -> None");
        assert_eq!(partial.side, -1, "A -> -1");
        assert_eq!(partial.qty, 5.0, "qty = origSz");
        assert_eq!(partial.filled_qty, 3.0, "filled = origSz - remaining sz");
        assert_eq!(partial.status, "PARTIALLY_FILLED", "some filled -> PARTIALLY_FILLED");
    }

    #[test]
    fn parse_orders_uses_explicit_status_when_present() {
        // A status-bearing row (not the frontendOpenOrders norm) exercises normalize_order_status.
        let body = r#"[
            {"coin":"ETH","side":"B","sz":"1.0","origSz":"1.0","oid":1,"timestamp":1,"status":"marginCanceled"}
        ]"#;
        let r = parse_orders(body).unwrap();
        assert_eq!(r[0].status, "CANCELED", "*Canceled suffix -> CANCELED");
    }

    #[test]
    fn normalize_order_status_maps_the_hl_vocabulary_by_suffix() {
        assert_eq!(normalize_order_status("open"), "ACCEPTED");
        assert_eq!(normalize_order_status("resting"), "ACCEPTED");
        assert_eq!(normalize_order_status("filled"), "FILLED");
        assert_eq!(normalize_order_status("triggered"), "TRIGGERED");
        assert_eq!(normalize_order_status("canceled"), "CANCELED");
        assert_eq!(normalize_order_status("reduceOnlyCanceled"), "CANCELED");
        assert_eq!(normalize_order_status("scheduledCancel"), "CANCELED");
        assert_eq!(normalize_order_status("rejected"), "REJECTED");
        assert_eq!(normalize_order_status("tickRejected"), "REJECTED");
        assert_eq!(normalize_order_status("somethingNew"), "SOMETHINGNEW", "unknown soft-fallback");
    }

    // --- parse_fills (userFills / userFillsByTime: a bare array) ---

    #[test]
    fn parse_fills_taker_buy_and_maker_sell_rebate() {
        let body = r#"[
            {"coin":"BTC","px":"50000.0","sz":"0.1","side":"B","oid":100,"cloid":"0xc1","tid":900,"fee":"2.5","feeToken":"USDC","crossed":true,"time":111},
            {"coin":"ETH","px":"3000.0","sz":"1.0","side":"A","oid":101,"tid":901,"fee":"-0.3","feeToken":"USDC","crossed":false,"time":222}
        ]"#;
        let r = parse_fills(body).unwrap();
        assert_eq!(r.len(), 2);

        let taker = &r[0];
        assert_eq!(taker.venue, "hyperliquid");
        assert_eq!(taker.symbol, "BTC");
        assert_eq!(taker.trade_id.as_str(), "900", "numeric tid stringified");
        assert_eq!(taker.venue_order_id.as_str(), "100");
        assert_eq!(taker.client_order_id.as_deref(), Some("0xc1"));
        assert_eq!(taker.side, 1, "B -> +1");
        assert_eq!(taker.last_qty, 0.1);
        assert_eq!(taker.last_px, 50000.0);
        assert_eq!(taker.commission, 2.5, "positive fee = cost");
        assert_eq!(taker.commission_asset, "USDC");
        assert_eq!(taker.liquidity_side, LiquiditySide::Taker, "crossed:true -> Taker");
        assert_eq!(taker.ts, 111);

        let maker = &r[1];
        assert_eq!(maker.trade_id.as_str(), "901");
        assert_eq!(maker.client_order_id, None, "absent cloid -> None");
        assert_eq!(maker.side, -1, "A -> -1");
        assert_eq!(maker.commission, -0.3, "negative fee = maker rebate, signed as-is");
        assert_eq!(maker.liquidity_side, LiquiditySide::Maker, "crossed:false -> Maker");
    }

    /// A `tid`-less reconcile fill row is SKIPPED, not admitted with an empty id. This gates the
    /// `TradeId::new` handling in [`parse_fills`]: reverting it to `unwrap_or_default()` turns the
    /// asserted length from 1 back to 3 and re-arms the hazard — an id-less `FillReport` can never
    /// match `seen_trade_ids`, so it manufactures a `MissingFill` divergence, which is one of the
    /// two kinds the `hybrid` policy AUTO-APPLIES (booking the fill a second time, unattended).
    #[test]
    fn parse_fills_skips_a_row_with_no_tid() {
        let body = r#"[
            {"coin":"BTC","px":"1.0","sz":"1.0","side":"B","oid":1,"fee":"0","feeToken":"USDC","crossed":true,"time":1},
            {"coin":"BTC","px":"1.0","sz":"1.0","side":"B","oid":2,"tid":"","fee":"0","feeToken":"USDC","crossed":true,"time":2},
            {"coin":"BTC","px":"1.0","sz":"1.0","side":"B","oid":3,"tid":903,"fee":"0","feeToken":"USDC","crossed":true,"time":3}
        ]"#;
        let r = parse_fills(body).unwrap();
        assert_eq!(r.len(), 1, "the absent-tid and empty-tid rows are both skipped");
        assert_eq!(r[0].trade_id, "903", "only the identifiable fill is reported");
    }

    // --- parse_positions (clearinghouseState.assetPositions[].position; szi signed) ---

    #[test]
    fn parse_positions_signed_szi_long_short_and_kept_flat_leg() {
        let body = r#"{
            "marginSummary":{"accountValue":"1234.5"},
            "time":1681222254710,
            "assetPositions":[
                {"type":"oneWay","position":{"coin":"BTC","szi":"0.5","entryPx":"29000.0","leverage":{"type":"cross","value":20}}},
                {"type":"oneWay","position":{"coin":"ETH","szi":"-2.0","entryPx":"1800.0","leverage":{"type":"isolated","value":10}}},
                {"type":"oneWay","position":{"coin":"SOL","szi":"0.0","entryPx":"0.0"}}
            ]
        }"#;
        let r = parse_positions(body).unwrap();
        assert_eq!(r.len(), 3, "every row kept, flat leg included");

        assert_eq!(r[0].venue, "hyperliquid");
        assert_eq!(r[0].symbol, "BTC");
        assert_eq!(r[0].position_side, PositionSide::Both, "HL one-way -> Both");
        assert_eq!(r[0].qty, 0.5, "positive szi -> long");
        assert_eq!(r[0].avg_px, 29000.0);
        assert_eq!(r[0].ts, 1681222254710, "top-level snapshot time stamps every row");
        assert_eq!(r[0].margin_mode, MarginMode::Cross, "leverage.type cross -> Cross");
        assert_eq!(r[0].isolated_margin, None);

        assert_eq!(r[1].qty, -2.0, "negative szi -> short (szi already signed)");
        assert_eq!(r[1].margin_mode, MarginMode::Isolated, "leverage.type isolated -> Isolated");
        assert_eq!(r[1].isolated_margin, None, "no verified HL per-position wallet field");
        assert_eq!(r[2].qty, 0.0, "flat leg survives with qty 0");
        assert_eq!(r[2].margin_mode, MarginMode::Cross, "absent leverage -> fail-safe Cross");
    }

    // --- parse_balance (per Product) ---

    #[test]
    fn parse_balance_perp_reads_account_value() {
        let body = r#"{"marginSummary":{"accountValue":"1234.5","totalMarginUsed":"10.0"},"withdrawable":"1200.0"}"#;
        assert_eq!(parse_balance(body, Product::Perp).unwrap(), Some(1234.5));
    }

    #[test]
    fn parse_balance_spot_reads_usdc_total() {
        let body = r#"{"balances":[
            {"coin":"PURR","total":"100.0","hold":"0.0"},
            {"coin":"USDC","total":"555.25","hold":"5.0"}
        ]}"#;
        assert_eq!(parse_balance(body, Product::Spot).unwrap(), Some(555.25));
    }

    #[test]
    fn parse_balance_none_when_field_or_usdc_absent() {
        assert_eq!(parse_balance(r#"{"marginSummary":{}}"#, Product::Perp).unwrap(), None);
        assert_eq!(
            parse_balance(r#"{"balances":[{"coin":"PURR","total":"1.0"}]}"#, Product::Spot)
                .unwrap(),
            None
        );
    }

    // --- robustness: malformed bodies are errors, not panics ---

    #[test]
    fn malformed_bodies_are_errors_not_panics() {
        assert!(parse_orders("not json").is_err());
        assert!(parse_orders("{}").is_err(), "an object (not the expected array) is an error");
        assert!(parse_fills("null").is_err());
        assert!(parse_fills("42").is_err());
        assert!(parse_positions("\"oops\"").is_err());
        assert!(parse_positions("{}").is_err(), "no assetPositions key is an error");
        assert!(parse_balance("nope", Product::Perp).is_err());
    }

    // --- the client constructs offline and IS a ReconClient ---

    #[test]
    fn client_constructs_offline_and_impls_recon_client() {
        let t = HyperliquidTransport::new(Network::Testnet);
        let c = HyperliquidReconClient::new(t, "0xmaster", Product::Perp);
        // compile-time proof the trait is implemented (no network touched)
        let _dyn: &dyn ReconClient = &c;
    }

    /// The `recon_client` free-fn factory (ReconFactory seam, wave-2 task 6) is the SAME
    /// construction, just already type-erased to `Box<dyn ReconClient>` — no network touched.
    #[test]
    fn recon_client_factory_constructs_offline() {
        let t = HyperliquidTransport::new(Network::Testnet);
        let _boxed: Box<dyn ReconClient> = recon_client(t, "0xmaster", Product::Perp);
    }
}
