//! IG `ReconcileClient` (ReconFactory seam) — the venue-facing report seam
//! (`vike_execution::recon::ReconcileClient`) over a DEDICATED logged-in [`IgSession`], reconciling against
//! IG's REST deal endpoints:
//!
//!   `GET /positions` (v2) — open positions. IG is position-per-deal (like the JForex sidecar):
//!       one epic can have several deal rows, so [`parse_positions`] AGGREGATES every matched row
//!       through `vike_model::PositionStatusReport::from_legs`: deals in one direction are ONE
//!       signed net-qty report, deals in both directions a `LONG` and a `SHORT` report (below).
//!   `GET /workingorders` (v2) — resting LIMIT/STOP working orders (order reports). A working
//!       order is unfilled by definition, so every row maps to a resting `ACCEPTED` report.
//!   `GET /history/activity` (v3, `detailed=true`) — the executed-deal feed (fill reports),
//!       filtered client-side to `type == "POSITION"` + `status == "ACCEPTED"` (a rejected/deleted
//!       activity never executed volume) and to the engine epic. `since` (epoch-ms) maps to the
//!       endpoint's `from` ISO-8601 datetime; `since <= 0` falls back to a 24h lookback.
//!   `GET /accounts` (v1) — the started account's `balance.balance` (`fetch_balance`), the
//!       "wallet balance, not available/equity" convention every other venue's `fetch_balance`
//!       already picks.
//!
//! **Position side is `BOTH` for deals in one direction (load-bearing).** IG's live exec fold
//! ([`crate::execution`]) publishes every fill with `position_side == "BOTH"`, so local net positions
//! are keyed `(epic, "BOTH")`. `recon::diff` matches a position report by `(symbol, position_side)`
//! EXACTLY, so a report spelled `LONG`/`SHORT` against a local `BOTH` key would never match (a
//! `PositionOnlyExternal` every pass). [`parse_positions`] therefore reports `PositionSide::Both`
//! with the SIGN carried in `qty` (BUY deals `+`, SELL deals `-`) while the epic's deals face one
//! way. ⚠ Deals in BOTH directions (`forceOpen: true` opens an opposite order as a second deal) are a
//! `LONG` and a `SHORT` report ON PURPOSE (owner, 2026-11-12, IG variant A): one net number would hide
//! the hedge, so the pair stays a held divergence, and the rows still sum to the net the core's
//! reduce-only evidence reads. When the epic has no open deal row it synthesizes ONE flat `BOTH`
//! row so a stale local position stays detectable — the same convention alpaca/okx/ctrader document.
//!
//! **`order_client_id` is matched through ONE in-memory map, and `None` otherwise.** IG's deal flow
//! returns a `dealReference` at submit and a `dealId` at confirm; neither the working-order list nor
//! the activity feed echoes any client-supplied id. The exec half records `order_client_id <-> dealId` as it
//! learns it (the shared `OrderVenueIdRegistry<String>`, filled by the submit's confirm and by the confirm
//! watchdog) and hands the SAME map to this client (`reconcile_client_sharing`), so a live working
//! order that THIS process placed is reported under its own order client id. Everything else reports `None`,
//! the "externally-placed order" convention every other venue's parser uses: an order from before
//! a restart (the map starts empty), one whose `dealId` was never learned, one placed in the IG UI.
//!
//! ⚠ **The order-client-id-keyed paths DO apply to IG.** This module used to say the opposite ("reconciles by
//! presence ... the `MissingTerminal`/`OrphanLocalOrder` paths do not apply"), and that was the
//! defect: with every report's order client id `None`, `vike_execution::recon::diff` could never match a live local
//! order, so each one produced a pair — `UnknownOrder` (the venue order "local has not heard of")
//! AND `OrphanLocalOrder` (the local order "the venue no longer reports"). With the map, a matched
//! order produces neither. `MissingTerminal` still cannot fire: this feed lists RESTING orders only,
//! so every report is `Accepted` and never terminal (a local order IG no longer lists surfaces as
//! `OrphanLocalOrder`, which is correct).
//!
//! **No live fee-rate lane.** IG charges spread/overnight funding, not a per-account maker/taker
//! rate over this API, so `fetch_fee_rates` stays the trait default (`Ok(None)`) — same as every
//! un-wired venue.
//!
//! ## Admitted symbols (any-symbol, decision 0173)
//! The client is built on the MOUNTED symbol, and [`ReconcileClient::admit_symbol`] tells it each symbol the
//! account's engine was admitted on demand: USER names (`EURUSD`, decision 0290), each read on the epic of its KEPT
//! row (`crate::epic`) and reported under the user name again. All three endpoints are account-wide on the wire, so
//! a pass makes the request it always made and parses the ONE body once per epic (the mounted symbol's first, then
//! each admitted one's, in the order told); an admitted epic IG does not list is a FLAT row, so a stale local
//! position stays detectable. A symbol no kept row names fails the whole fetch in words (its epic is unknown), as
//! any parse error does: the restore planner reads an order a successful report lacks as gone. An admitted symbol is held UPPER-CASE
//! (`ReconcileAdmittedSymbols`), as every user name is spelled.
//!
//! ⚠ **The activity feed is account-wide AND paged** (`pageSize` defaults to 50, min 10, max 500; the response's
//! `metadata.paging.next` is a link to the next page: IG's REST reference for `/history/activity` and the REST
//! guide's "Paging" section), so one page can leave an epic's fills out: a MISSED fill, never a false one, which
//! a restore or `hybrid` pass can never see. Therefore, with an epic admitted, the fills read follows
//! `metadata.paging.next` for at most `ACTIVITY_PAGE_ROUNDS` requests, keeps a row two pages both carry once,
//! and FAILS the pass when the list is still truncated (a `next` after the last round, a `next` that adds no
//! row, or one that is not a `/history/activity` link: the session's tokens go only to the gateway it logged in
//! at). The mounted-only pass (nothing admitted) is unchanged: the one first page, whatever it says; the docs
//! make that page just as short for the mounted epic, so flipping it is a one-line decision for the owner
//! (`activity_body`'s `paging` argument). MEASURED on the demo gateway (2026-10-30): rows come newest first; `next`
//! is `/history/activity?version=3&from=..&to=..&detailed=true&pageSize=..` with `to` (a UTC `...Z` time) one second
//! before the page's last row; the LAST page says `next: null`, and a history that fills its last page to the
//! brim is followed by an EMPTY page (`size` 0, `next: null`); a `pageSize` below 10 is HTTP 400. The link's `from`
//! is IG's rewrite of ours with the London offset of that date (`+01:00` in summer time, `Z` in winter), and IG
//! REFUSES the `+01:00` spelling, written as it is or percent-encoded once (HTTP 400): `next_activity_query`
//! therefore follows the link with OUR `from`. UNMEASURED: the rows two pages share when several fall in the one
//! second a page ends on (`to` is that second less one, so such a row could fall between two pages), and the
//! historical-data allowance a page costs (the account's figure did not move over some 200 rows read).
//!
//! ⚠ **A fill report's `trade_id` is the key the ENGINE folds the same execution under**, because
//! `vike_execution::recon::diff` matches nothing else: an OPEN is its activity `dealId` (the position's own, the
//! one the confirm names), a CLOSE is its `details.dealReference` (the one the close POST returned): the
//! row of a close carries a NEW `dealId` no confirm names, so keyed on it every close the node sent came back
//! as a held `MissingFill` (`parse_activity_fills`; `crate::execution::event_mapper::confirm_trade_id` is the
//! engine's side, and `tests/offline/activity_close_matches_its_fill.rs` pins both over real captures).
//!
//! Every wire->report mapping delegates to a PURE free function (`parse_*`) over the raw JSON body,
//! fixture-tested (`tests/offline/reconcile_client_parse.rs`, no network) against captured IG REST shapes; the
//! `#[ignore]`d cred-gated live smoke (`tests/ig_smokes/ig_reconcile_smoke.rs`) proves the real fetch+parse.

pub(crate) mod venue_read;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use serde_json::Value;

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
// IG returns a numeric field either as a JSON number (positions/working orders) OR as a string (the
// activity `details` block quotes `size`/`level`, e.g. `"+1"` / `"1.09"`); `get_f64` reads both and
// folds an absent/unparseable one to `0.0`. Rust's `f64` parse accepts the leading `+` IG puts on a
// signed size. A FILL row reads them through `get_f64_opt` instead and is skipped without both
// (`parse_activity_fills`).
use vike_bridge_core::json::{get_f64, get_f64_opt};
use vike_bridge_core::reconcile_admitted::ReconcileAdmittedSymbols;
use vike_bridge_core::side::sign_default_buy;
use vike_execution::execution_engine::LOCAL_VIEW_QUANTITY_TOLERANCE;
use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_model::events::{LiquiditySide, TradeId};
use vike_model::{
    ContractValue, FillReport, OrderStatus, OrderStatusReport, OrderType, PositionLeg,
    PositionStatusReport,
};

use crate::config::IgConfig;
use crate::history::prices::parse_ig_time_utc;
use crate::transport::IgSession;
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings;

/// This venue's `vike_model::VENUES` id — every report here carries it, and so does the start.
pub(crate) const VENUE: &str = "ig";

// --- pure parsers ------------------------------------------------------------------------------

/// Epoch-ms (UTC) -> IG's `"YYYY-MM-DDTHH:MM:SS"` datetime (the `from` query the activity endpoint
/// takes), the second floored: the model's one split ([`vike_model::time::utc_parts`]) — no chrono.
pub fn ms_to_ig_datetime(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = vike_model::time::utc_parts(ms);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// `GET /positions` (v2) rows -> `epic`'s position rows. IG is position-per-deal, so every deal row
/// matching the epic is one [`PositionLeg`] (`size` is UNSIGNED on the wire and the sign comes from
/// `position.direction`, `SELL` -> short/negative; its own `level`; its `createdDateUTC`), netted by
/// the shared [`PositionStatusReport::from_legs`] (owner, 2026-11-12: IG variant A, the locked-pair
/// rule of every bridge whose rows come from legs):
///
/// - deals in ONE direction are one `PositionSide::Both` row with the sign in `qty` (see the module
///   doc), at their size-weighted level and the newest `createdDateUTC`. That level carries IG's own
///   profit and loss (the deals' summed P&L at a price `P` is `Σ s·(P − L)` times the contract size,
///   `(Σ s)·(P − avg_px)` exactly at this level), and it is bit for bit the old `Σ s·L / Σ s`;
/// - deals in BOTH directions (this bridge opens with `forceOpen: true`, so an opposite order opens a
///   second deal beside the first: buy 3 and sell 1 on one epic) are a `Long` row (3) and a `Short`
///   row (-1), each at its own deals' level, which reconcile holds: one signed net row of 2 hid the
///   hedge IG charges on both legs. The rows still sum to the net;
/// - a deal with no usable level makes its row's level `0.0`, "unknown", for the reconcile's
///   unpriced-row hold;
/// - no matched deal (IG omits an epic entirely once flat) is one synthesized flat row.
///
/// No contract money is formed here: the contract size cancels out of a level (linear, so
/// `ContractValue::default()` averages it), and an IG contract's money is its mount's
/// `vike_model::ContractValue`.
pub fn parse_positions(body: &str, epic: &str) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows =
        v.get("positions").and_then(|p| p.as_array()).ok_or("expected a `positions` array")?;
    let legs: Vec<PositionLeg> = rows
        .iter()
        .filter(|row| {
            row.get("market").and_then(|m| m.get("epic")).and_then(|e| e.as_str()).unwrap_or("")
                == epic
        })
        .filter_map(|row| row.get("position"))
        .map(|pos| {
            let size = get_f64(pos, "size").abs();
            let is_short = pos
                .get("direction")
                .and_then(|d| d.as_str())
                .is_some_and(|d| d.eq_ignore_ascii_case("sell"));
            PositionLeg {
                size: if is_short { -size } else { size },
                price: get_f64(pos, "level"),
                timestamp_ms: pos
                    .get("createdDateUTC")
                    .and_then(|t| t.as_str())
                    .map_or(0, parse_ig_time_utc),
            }
        })
        .collect();
    Ok(PositionStatusReport::from_legs(VENUE, epic, &legs, ContractValue::default()))
}

/// `GET /workingorders` (v2) rows -> `OrderStatusReport`, filtered to `epic`. A working order is
/// resting/unfilled by definition, so `status` is always `ACCEPTED`, `filled_qty`/`avg_px` are `0`.
/// `venue_order_id` is the `dealId`; `order_client_id` is always `None` here (see the module doc —
/// [`parse_working_orders_with_ids`] is the variant that can name it);
/// `order_type` maps `orderType` (`LIMIT` → `Limit`, `STOP` → `Stop`; any other kind stays
/// [`OrderType::Unrecognized`] lower-cased); `ts` is `createdDateUTC`.
pub fn parse_working_orders(body: &str, epic: &str) -> Result<Vec<OrderStatusReport>, String> {
    parse_working_orders_with_ids(body, epic, &|_| None)
}

/// [`parse_working_orders`] with `order_client_id_for(dealId)` consulted for each row's `order_client_id`: a
/// `dealId` this process placed reports under its own order client id (so `recon::diff` matches the
/// live local order instead of pairing an `UnknownOrder` with an `OrphanLocalOrder`), any other
/// stays `None`.
pub fn parse_working_orders_with_ids(
    body: &str,
    epic: &str,
    order_client_id_for: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<OrderStatusReport>, String> {
    let mut reports = parse_all_working_orders(body, order_client_id_for)?;
    reports.retain(|r| r.symbol == epic);
    Ok(reports)
}

/// Every row of a `GET /workingorders` (v2) body, each under its OWN epic (`workingOrderData.epic`, else
/// `marketData.epic`), mapped as [`parse_working_orders_with_ids`] maps one: the account-wide list an
/// operator's read shows (`crate::reconcile::venue_read`) and the exec thread reads an unconfirmed amend's level from
/// (`crate::execution::lifecycle`'s `Unsettled`), so the three readers cannot disagree about a row.
pub(crate) fn parse_all_working_orders(
    body: &str,
    order_client_id_for: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<OrderStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v
        .get("workingOrders")
        .and_then(|w| w.as_array())
        .ok_or("expected a `workingOrders` array")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let wo = row.get("workingOrderData")?;
            let row_epic = wo
                .get("epic")
                .and_then(|e| e.as_str())
                .or_else(|| {
                    row.get("marketData").and_then(|m| m.get("epic")).and_then(|e| e.as_str())
                })
                .unwrap_or("");
            let direction = wo.get("direction").and_then(|d| d.as_str()).unwrap_or("");
            let order_type = match wo.get("orderType").and_then(|t| t.as_str()).unwrap_or("") {
                "LIMIT" => OrderType::Limit,
                "STOP" => OrderType::Stop,
                other => OrderType::Unrecognized(other.to_ascii_lowercase()),
            };
            let ts = wo.get("createdDateUTC").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
            let deal_id = wo.get("dealId").and_then(|d| d.as_str()).unwrap_or("");
            // `orderLevel` is the one level a working order rests at, in PRICE units on a JPY epic
            // too (151.572, the "Price units" bullet of
            // `crates/bridges/ig/CLAUDE.md`'s "Measured on the demo gateway, 2026-10-28"): what
            // `execution/mod.rs` sent as a LIMIT's price or a STOP's trigger.
            let level = get_f64_opt(wo, "orderLevel").filter(|l| l.is_finite() && *l > 0.0);
            let (price, trigger_price) = match order_type {
                OrderType::Limit => (level, None),
                OrderType::Stop => (None, level),
                _ => (None, None),
            };
            Some(OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: row_epic.to_string(),
                venue_order_id: deal_id.into(),
                order_client_id: order_client_id_for(deal_id).filter(|c| !c.is_empty()),
                side: sign_default_buy(direction),
                order_type,
                qty: get_f64(wo, "orderSize"),
                filled_qty: 0.0,
                average_price: 0.0,
                status: OrderStatus::Accepted,
                timestamp: ts,
                price,
                trigger_price,
                trail: None,
                trail_percent: None,
            })
        })
        .collect())
}

/// Whether an activity row reports a position being CLOSED: one of its `details.actions[]` is a
/// `POSITION_CLOSED` (the row says "Position/s closed: ..", measured 2026-10-30; `affectedDealId` names the
/// closed position) or the `POSITION_PARTIALLY_CLOSED` its twin is documented as.
fn is_close_row(row: &Value) -> bool {
    row.pointer("/details/actions").and_then(Value::as_array).is_some_and(|actions| {
        actions.iter().any(|a| {
            matches!(
                a.get("actionType").and_then(Value::as_str),
                Some("POSITION_CLOSED" | "POSITION_PARTIALLY_CLOSED")
            )
        })
    })
}

/// `GET /history/activity` (v3, `detailed=true`) rows -> `FillReport`, filtered to executed deals
/// (`type == "POSITION"` + `status == "ACCEPTED"` — a rejected/deleted activity executed no volume)
/// and to `epic`. `trade_id` is the key the ENGINE folds the same execution under
/// (`crate::execution::event_mapper::confirm_trade_id`'s rule, which a test pins from both sides): the activity
/// `dealId` of an OPEN (the position's own), but the row's `details.dealReference` of a CLOSE — the row of a
/// close carries a NEW `dealId` that no confirm names (measured 2026-10-30), while its `dealReference` is the
/// one the close POST returned and the engine keyed the close on. Keyed on that `dealId` every close the node
/// sent came back as a held `MissingFill`, and under `hybrid`/`synthesize` it would be booked twice. A close
/// row without a reference is skipped (never keyed on its `dealId`, which is that defect).
/// `venue_order_id` prefers `details.dealReference`, falling back to `dealId`. `side`/`last_qty` come from `details.size`
/// (signed, e.g. `"-1"`), preferring `details.direction` for the sign; `last_price` is `details.level`. A row
/// with no finite non-zero size or no level above zero is skipped at ERROR, never reported at 0.
/// No commission/liquidity field rides this feed, so `commission: 0`/`commission_asset: ""`/
/// `liquidity_side: Unknown` (the same "field genuinely absent from the wire" fail-soft every other
/// venue's parser uses). `ts` is the activity `date` (already UTC).
pub fn parse_activity_fills(body: &str, epic: &str) -> Result<Vec<FillReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows =
        v.get("activities").and_then(|a| a.as_array()).ok_or("expected an `activities` array")?;
    Ok(rows
        .iter()
        .filter(|r| r.get("epic").and_then(|e| e.as_str()) == Some(epic))
        .filter(|r| r.get("type").and_then(|t| t.as_str()) == Some("POSITION"))
        .filter(|r| r.get("status").and_then(|s| s.as_str()) == Some("ACCEPTED"))
        // SKIP an id-less row rather than admit it: `vike_execution::recon::diff` matches this id against
        // `seen_trade_ids`, and an empty one can never match, so it FABRICATES a `MissingFill` — one
        // of the two kinds `hybrid` AUTO-APPLIES, i.e. it re-books the fill unattended. Skipping
        // yields no divergence, the safe direction. Note `venue_order_id` deliberately still falls
        // back to `deal_id`: that field is descriptive, not a dedup key.
        .filter_map(|r| {
            let details = r.get("details");
            let deal_id = r.get("dealId").and_then(|d| d.as_str()).unwrap_or("");
            let deal_reference = details
                .and_then(|d| d.get("dealReference"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty());
            let key = if is_close_row(r) { deal_reference.unwrap_or("") } else { deal_id };
            let trade_id = match TradeId::new(key) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %epic,
                        "activity row carries no execution id (a close's dealReference, any other \
                         row's dealId) — skipping the fill report; an id-less one would fabricate a \
                         MissingFill that `hybrid` auto-applies"
                    );
                    return None;
                }
            };
            let venue_order_id = deal_reference.unwrap_or(deal_id);
            let number = |k: &str| details.and_then(|d| get_f64_opt(d, k)).filter(|x| x.is_finite());
            let signed_size = number("size").filter(|s| *s != 0.0);
            let level = number("level").filter(|l| *l > 0.0);
            // A fill always states both, unlike an order row: a defaulted 0 would be booked at price 0.
            let (Some(signed_size), Some(last_price_value)) = (signed_size, level) else {
                tracing::error!(
                    venue = VENUE,
                    %epic,
                    %trade_id,
                    "activity row states no usable `details.size` or `details.level` — skipping the fill \
                     report rather than reporting a fill at 0"
                );
                return None;
            };
            // Sign: prefer the explicit direction, else the sign of the (possibly signed) size.
            let side = match details.and_then(|d| d.get("direction")).and_then(|d| d.as_str()) {
                Some(dir) => sign_default_buy(dir),
                None if signed_size < 0.0 => -1,
                None => 1,
            };
            let ts = r.get("date").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: epic.to_string(),
                trade_id,
                venue_order_id: venue_order_id.into(),
                order_client_id: None,
                side,
                last_qty: signed_size.abs(),
                last_price: last_price_value,
                commission: 0.0,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                timestamp: ts,
            })
        })
        .collect())
}

/// The close of `side` and `qty` on `epic` that an activity body reports at or after `since_ms`, in the shape of its
/// `/confirms` reply (`status` `CLOSED`, so the engine keys it on the reference as it does every close: `parse_activity_fills`
/// and `crate::execution::event_mapper::confirm_trade_id` agree), for an order IG's close POST left unknown. `None` when no such
/// row is there; malformed JSON is `Err`.
///
/// shortcut: the first page of the history, which holds the newest 50 rows of the window. Page it if an account ever deals
/// that much within a re-ask.
pub(crate) fn close_in_activity(
    body: &str,
    epic: &str,
    side: i32,
    qty: f64,
    since_ms: i64,
) -> Result<Option<Value>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows =
        v.get("activities").and_then(Value::as_array).ok_or("expected an `activities` array")?;
    let closes: Vec<&Value> = rows.iter().filter(|r| is_close_row(r)).collect();
    let reports =
        parse_activity_fills(&serde_json::json!({ "activities": closes }).to_string(), epic)?;
    Ok(reports
        .into_iter()
        .find(|r| {
            r.side == side
                && (r.last_qty - qty).abs() < LOCAL_VIEW_QUANTITY_TOLERANCE
                && r.timestamp >= since_ms
        })
        .map(|r| {
            serde_json::json!({
                "dealStatus": "ACCEPTED",
                "status": "CLOSED",
                "dealReference": r.trade_id.as_str(),
                "epic": r.symbol,
                "direction": if r.side < 0 { "SELL" } else { "BUY" },
                "size": r.last_qty,
                "level": r.last_price,
            })
        }))
}

/// IG's `actionType` words of an order DELETED (the `GET /history/activity` v3 reference page's enum); a fill is a
/// `..._FILLED`, never one of these. None was captured: what was measured (2026-10-28) is that the activity row of a
/// deleted working order names the order in `affectedDealId`.
const ORDER_DELETED_ACTIONS: [&str; 3] =
    ["WORKING_ORDER_DELETED", "LIMIT_ORDER_DELETED", "STOP_ORDER_DELETED"];

/// Does an activity body report the working order `deal_id` DELETED: an `ACCEPTED` row one of whose `details.actions[]` is
/// an order deletion ([`ORDER_DELETED_ACTIONS`]) naming `deal_id` in `affectedDealId`? Read for a cancel of unknown outcome
/// (`crate::execution::lifecycle`'s `Unsettled`), over the same first page as [`close_in_activity`]. Malformed JSON is `Err`.
pub(crate) fn deleted_in_activity(body: &str, deal_id: &str) -> Result<bool, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows =
        v.get("activities").and_then(Value::as_array).ok_or("expected an `activities` array")?;
    let deletes = |a: &Value| {
        a.get("actionType")
            .and_then(Value::as_str)
            .is_some_and(|t| ORDER_DELETED_ACTIONS.contains(&t))
            && a.get("affectedDealId").and_then(Value::as_str) == Some(deal_id)
    };
    Ok(rows.iter().filter(|r| r.get("status").and_then(Value::as_str) == Some("ACCEPTED")).any(
        |r| {
            r.pointer("/details/actions")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(&deletes))
        },
    ))
}

/// `GET /accounts` (v1) -> the started account's `balance.balance` (total account funds — NOT
/// `available`, which nets running margin, nor equity, which adds open P&L), matching every other
/// venue's "wallet balance" `fetch_balance` convention. Picks the row whose `accountId` matches
/// `account_id` and NO OTHER: the account list is the login's, and another account's funds (in its own currency)
/// are never this engine's balance, so a started account the list does not name is `None`, "not reported" (it
/// used to fall back to the first account, which a foreign-quoted ledger now adopts like any other). `None` when
/// the field is absent too (fail-soft); malformed JSON is a hard `Err`.
pub fn parse_balance(body: &str, account_id: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let accounts =
        v.get("accounts").and_then(|a| a.as_array()).ok_or("expected an `accounts` array")?;
    let account =
        accounts.iter().find(|a| a.get("accountId").and_then(|id| id.as_str()) == Some(account_id));
    Ok(account
        .and_then(|a| a.get("balance"))
        .and_then(|b| b.get("balance"))
        .and_then(serde_json::Value::as_f64))
}

// --- the client ----------------------------------------------------------------------------

/// A 24h lookback (ms) for the activity `from` param when the caller passes `since <= 0` (an
/// unbounded pass) — IG's activity endpoint requires a `from`, so a floor is needed.
const DEFAULT_ACTIVITY_LOOKBACK_MS: i64 = vike_model::MS_PER_DAY;

/// The most `GET /history/activity` requests one fill pass makes when an epic is admitted (the module doc).
const ACTIVITY_PAGE_ROUNDS: usize = 10;

const ACTIVITY_TRUNCATED: &str = "ig recon: the activity history has more pages than a pass reads \
    (`metadata.paging.next` after the last round, or a next page with no new row), so the fills of every \
    admitted epic cannot be claimed read";

/// The query of the page after `page`: its `metadata.paging.next` is IG's own link
/// (`/history/activity?version=3&from=..&to=..&detailed=true&pageSize=..`), absent, `null` or empty on the last
/// page. A link to anywhere else is an error, never followed: our `CST` / `X-SECURITY-TOKEN` go only to the
/// gateway the session logged in at.
///
/// ⚠ The link is followed with OUR `from` (the one `query`, the request that opened the read, carries), not IG's:
/// IG rewrites it with the offset it read the text in (`from=2026-10-14T03:03:36+01:00`) and then REFUSES its own
/// spelling, as written (`+` read as a space) AND percent-encoded once (`%2B`): HTTP 400 `Unable to parse
/// datetime=2026-10-16T19:45:10 01:00`, measured on the demo gateway 2026-10-30. A window never moves its start,
/// so the `from` we asked for is the one IG means; only `to` slides. Every other pair of the link is kept.
fn next_activity_query(page: &Value, query: &str) -> Result<Option<String>, String> {
    let next = match page.pointer("/metadata/paging/next") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) if s.is_empty() => return Ok(None),
        Some(Value::String(s)) => s,
        Some(other) => {
            return Err(format!("ig recon: `metadata.paging.next` is not a link: {other}"));
        }
    };
    let link = next.strip_prefix("/history/activity?").ok_or_else(|| {
        format!("ig recon: `metadata.paging.next` is not a /history/activity link: {next}")
    })?;
    let ours = query.split('&').find(|pair| pair.starts_with("from="));
    let pairs: Vec<&str> = link
        .split('&')
        .map(|pair| if pair.starts_with("from=") { ours.unwrap_or(pair) } else { pair })
        .collect();
    Ok(Some(pairs.join("&")))
}

/// One reconcile client for a started (account, symbol), holding its OWN dedicated logged-in
/// [`IgSession`] — a reconcile fetch never contends the exec side's session, and its tokens expire
/// independently. `symbol` is the mounted USER name (`"EURUSD"`, decision 0290), read on the epic of its kept
/// row (`crate::epic`) and reported under the user name. All fetches are blocking REST on the reconcile thread —
/// never the fold thread.
pub struct ReconcileClientIg {
    /// The dedicated session: logged in at connect, or by a later pass when that login never reached IG
    /// ([`Self::session`]).
    session: OnceLock<IgSession>,
    /// The login a pass still owes while `session` is empty; `None` once it is made.
    pending: Mutex<Option<PendingLogin>>,
    symbol: String,
    /// `order_client_id <-> dealId`, shared with the exec half when built through [`reconcile_client_sharing`]
    /// (otherwise a private empty map: every working order then reports `order_client_id: None`).
    deal_ids: Arc<Mutex<OrderVenueIdRegistry<String>>>,
    /// The symbols the account's engine admitted on demand, beyond `symbol`: every pass reads them too (the
    /// module doc).
    admitted: ReconcileAdmittedSymbols,
}

/// A reconcile login the start could not make because IG was never reached (`status` 0, the rule of the exec lane's
/// `login_with_retry`): the next pass logs in, and each one that fails waits a step of the exec lane's ladder
/// (`venue_timings::IG`'s exec `reconnect`) before the next, so a pass's later reads right behind a failed login make
/// none. A refusal ends it: asking again cannot cure one.
struct PendingLogin {
    config: IgConfig,
    backoff: ReconnectBackoff,
    /// No login before this instant.
    due: Instant,
    /// IG refused a retried login: these words fail every later read, and no pass logs in again.
    refused: Option<String>,
}

impl ReconcileClientIg {
    /// Log in a dedicated session for reconcile reads and bind it to the USER name `symbol`. `None` when IG
    /// REFUSES the login — reconcile stays unwired for this venue, exec unaffected (the same graceful
    /// degradation the exec half's own login uses as the live gate). A login that never reached IG (a network
    /// failure, the FX market closed) is made by a later pass instead ([`PendingLogin`]). Needs no kept row: a
    /// pass asks for each name's epic as it reads.
    pub fn connect(config: &IgConfig, symbol: &str) -> Option<Self> {
        Self::connect_sharing(config, symbol, Arc::default())
    }

    /// [`Self::connect`] over the exec half's `order_client_id <-> dealId` map.
    pub(crate) fn connect_sharing(
        config: &IgConfig,
        symbol: &str,
        deal_ids: Arc<Mutex<OrderVenueIdRegistry<String>>>,
    ) -> Option<Self> {
        let (session, pending) = match IgSession::login(config) {
            Ok(session) => (OnceLock::from(session), None),
            Err(e) if e.status != 0 => return None,
            Err(e) => {
                tracing::warn!(
                    venue = VENUE,
                    symbol,
                    error = %e.message,
                    "ig recon: the reconcile login did not reach IG; the next pass logs in again"
                );
                let reconnect = venue_timings::IG.exec().reconnect();
                let pending = PendingLogin {
                    config: config.clone(),
                    backoff: ReconnectBackoff::new(reconnect.initial, reconnect.max),
                    due: Instant::now(),
                    refused: None,
                };
                (OnceLock::new(), Some(pending))
            }
        };
        Some(ReconcileClientIg {
            session,
            pending: Mutex::new(pending),
            symbol: symbol.to_string(),
            deal_ids,
            admitted: ReconcileAdmittedSymbols::default(),
        })
    }

    /// The dedicated session, logging it in first when the start could not ([`PendingLogin`]).
    fn session(&self) -> Result<&IgSession, String> {
        if let Some(session) = self.session.get() {
            return Ok(session);
        }
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(login) = pending.as_mut() else {
            // Another read logged in while this one waited for the lock.
            return self.session.get().ok_or_else(|| "ig recon: no reconcile session".to_string());
        };
        if let Some(words) = &login.refused {
            return Err(words.clone());
        }
        let now = Instant::now();
        if now < login.due {
            return Err(format!(
                "ig recon: not logged in (IG was not reached); the next login is in {} ms",
                (login.due - now).as_millis()
            ));
        }
        match IgSession::login(&login.config) {
            Ok(session) => {
                tracing::info!(
                    venue = VENUE,
                    symbol = %self.symbol,
                    "ig recon: logged in on a later pass; reconciliation runs from this pass"
                );
                *pending = None;
                Ok(self.session.get_or_init(|| session))
            }
            Err(e) if e.status != 0 => {
                let words = format!(
                    "ig recon: IG refused the reconcile login ({e}); asking again cannot cure a refusal, so no \
                     pass logs in again for this mount"
                );
                tracing::error!(venue = VENUE, symbol = %self.symbol, "{words}");
                login.refused = Some(words.clone());
                Err(words)
            }
            Err(e) => {
                let wait = login.backoff.failed();
                login.due = Instant::now() + wait;
                Err(format!(
                    "ig recon: not logged in, IG was not reached ({}); the next login is in {} ms",
                    e.message,
                    wait.as_millis()
                ))
            }
        }
    }

    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, String> {
        self.session()?.get(path, version, query).map_err(|e| e.to_string())
    }

    /// The `(user name, epic)` pairs a pass reads: the mounted symbol first, then each admitted one. A name no kept
    /// row names fails the fetch in words (the module doc): its epic is unknown, and a report that quietly left it
    /// out would read its resting orders as gone.
    fn epics(&self) -> Result<Vec<(String, String)>, String> {
        std::iter::once(self.symbol.clone())
            .chain(self.admitted.symbols())
            .map(|symbol| {
                let epic = crate::epic::epic_of(&symbol).map_err(|why| {
                    format!("ig recon: {why}, so the orders, fills and position of {symbol} cannot be read")
                })?;
                Ok((symbol, epic))
            })
            .collect()
    }

    /// The `GET /history/activity` body a pass parses per epic. Without `paging` it is the first page, as
    /// ever; with it, the `activities` of every page up to `ACTIVITY_PAGE_ROUNDS`, a row two pages both carry
    /// kept once (identical rows, NOT identical `dealId`s: a close may reuse its open's `dealId`, and the
    /// single-page read has always kept both). Still truncated after the last round, or a next page that adds
    /// no row, is an `Err`: a short report is never passed off as the whole window.
    fn activity_body(&self, query: &str, paging: bool) -> Result<String, String> {
        let mut page = self.get("/history/activity", "3", query)?;
        if !paging {
            return Ok(page.to_string());
        }
        let mut rows: Vec<Value> = Vec::new();
        for round in 1.. {
            let held = rows.len();
            let fresh = page
                .get("activities")
                .and_then(Value::as_array)
                .ok_or("expected an `activities` array")?;
            // Only against the EARLIER pages: a repeat inside one page was always kept.
            for row in fresh {
                if !rows[..held].contains(row) {
                    rows.push(row.clone());
                }
            }
            let Some(next) = next_activity_query(&page, query)? else { break };
            if round == ACTIVITY_PAGE_ROUNDS || rows.len() == held {
                return Err(ACTIVITY_TRUNCATED.to_string());
            }
            page = self.get("/history/activity", "3", &next)?;
        }
        Ok(serde_json::json!({ "activities": rows }).to_string())
    }
}

impl ReconcileClient for ReconcileClientIg {
    /// IG working orders report the currently-resting set only, so `_since` is a no-op — a
    /// fully-closed/executed order is what the fill (not this) report reconciles. One account-wide
    /// body, parsed per epic.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let body = self.get("/workingorders", "2", "")?.to_string();
        let mut reports = Vec::new();
        for (symbol, epic) in self.epics()? {
            let parsed = parse_working_orders_with_ids(&body, &epic, &|deal_id| {
                self.deal_ids.lock().unwrap().order_client_id_of(deal_id).map(str::to_owned)
            })?;
            reports.extend(
                parsed.into_iter().map(|r| OrderStatusReport { symbol: symbol.clone(), ..r }),
            );
        }
        Ok(reports)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let from_ms =
            if since > 0 { since } else { vike_model::now_ms() - DEFAULT_ACTIVITY_LOOKBACK_MS };
        let query = format!("from={}&detailed=true&pageSize=50", ms_to_ig_datetime(from_ms));
        // ONE snapshot of the epics: the pages read are the pages for the epics then parsed. Paged when a symbol is
        // admitted beside the mounted one, read or not.
        let paging = !self.admitted.symbols().is_empty();
        let epics = self.epics()?;
        let body = self.activity_body(&query, paging)?;
        let mut reports = Vec::new();
        for (symbol, epic) in &epics {
            let parsed = parse_activity_fills(&body, epic)?;
            reports.extend(parsed.into_iter().map(|r| FillReport { symbol: symbol.clone(), ..r }));
        }
        Ok(reports)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let body = self.get("/positions", "2", "")?.to_string();
        let mut reports = Vec::new();
        for (symbol, epic) in self.epics()? {
            let parsed = parse_positions(&body, &epic)?;
            reports.extend(
                parsed.into_iter().map(|r| PositionStatusReport { symbol: symbol.clone(), ..r }),
            );
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (a USER name, any-symbol, decision 0173): every later
    /// pass reads its epic too. Idempotent, never fetches; the list is bounded and held upper-case.
    fn admit_symbol(&self, symbol: &str) {
        self.admitted.admit_logged(symbol, &self.symbol, VENUE, "a symbol");
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        self.admitted.forget_logged(symbol, VENUE, "a symbol");
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// Balance meaning: the started account's `balance.balance` from `GET /accounts` — see
    /// [`parse_balance`]. Its failure is the read's text, so it reads unclassified; only the node logs it.
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        let response = self.get("/accounts", "1", "").map_err(ReconcileFetchError::unclassified)?;
        let session = self.session().map_err(ReconcileFetchError::unclassified)?;
        parse_balance(&response.to_string(), &session.account_id)
            .map_err(ReconcileFetchError::unclassified)
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconcileClient` factory (ReconFactory seam): logs in a FRESH [`IgSession`] dedicated
/// to reconcile reads (isolated from the exec side's own session — the "one session per purpose"
/// idiom). `symbol` is the mounted USER name (decision 0290). `None` when IG refuses the login — reconcile stays
/// unwired, exec unaffected; a login that never reached IG is made by a later pass (`ReconcileClientIg::connect`).
///
/// **WIRED into this crate's start** — `crates/bridges/ig/src/adapter.rs`'s `VenueAdapterIg::start`
/// calls this through `reconcile_if_enabled`, LAZILY, because the login here is a BLOCKING network
/// round-trip at start time (inside `vike_tradehub_core::make_engine`): a start whose reconcile gate
/// answered NO must open no second authenticated session. IG reconciles on the periodic INTERVAL
/// only (no `reconcile_trigger`), and its feed string is withheld from `reconcile_feed_statuses` on purpose
/// (`StrategyLanesLive::reconcile_feed_statuses`' ig paragraph in `vike-tradehub`), so its health gate reads
/// `Healthy` and never blocks a pass.
///
/// ⚠ This line used to say "not yet wired", which it had stopped being. A factory's header is the
/// WRONG place to read that from, because a start can adopt a factory without touching it — read
/// the start. `crates/bridges/ig/CLAUDE.md` carries the family rule and the rollout caveat that
/// belongs with this venue's order-report completeness.
pub fn recon_client(config: &IgConfig, symbol: &str) -> Option<Box<dyn ReconcileClient>> {
    reconcile_client_sharing(config, symbol, Arc::default())
}

/// [`recon_client`] over the `order_client_id <-> dealId` map the exec client was spawned with
/// (`crate::execution::spawn_sharing`) — what the adapter's start passes, so this process's live
/// working orders match their local orders. `recon_client` is the same with a private empty map.
pub(crate) fn reconcile_client_sharing(
    config: &IgConfig,
    symbol: &str,
    deal_ids: Arc<Mutex<OrderVenueIdRegistry<String>>>,
) -> Option<Box<dyn ReconcileClient>> {
    ReconcileClientIg::connect_sharing(config, symbol, deal_ids)
        .map(|c| Box::new(c) as Box<dyn ReconcileClient>)
}
