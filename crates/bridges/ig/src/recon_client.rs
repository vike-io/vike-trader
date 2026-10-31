//! IG `ReconClient` (ReconFactory seam) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) over a DEDICATED logged-in [`IgSession`], reconciling against
//! IG's REST deal endpoints:
//!
//!   `GET /positions` (v2) — open positions. IG is position-per-deal (like the JForex sidecar):
//!       one epic can have several deal rows, so [`parse_positions`] AGGREGATES every matched row
//!       into ONE signed net-qty report. `POSITION_ONLY`-external / drift reconcile off that net.
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
//! **Position side is always `BOTH` (load-bearing).** IG's live exec fold
//! ([`crate::exec`]) publishes every fill with `position_side == "BOTH"`, so local net positions
//! are keyed `(epic, "BOTH")`. `recon::diff` matches a position report by `(symbol, position_side)`
//! EXACTLY, so a report spelled `LONG`/`SHORT` against a local `BOTH` key would silently never
//! match (a false `PositionOnlyExternal` every pass). [`parse_positions`] therefore reports
//! `PositionSide::Both` with the SIGN carried in `qty` (BUY deals `+`, SELL deals `-`), never a
//! directional side. When the epic has no open deal row it synthesizes ONE flat `BOTH` row so a
//! stale local position stays detectable — the same convention alpaca/okx/ctrader document.
//!
//! **`client_order_id` is matched through ONE in-memory map, and `None` otherwise.** IG's deal flow
//! returns a `dealReference` at submit and a `dealId` at confirm; neither the working-order list nor
//! the activity feed echoes any client-supplied id. The exec half records `coid <-> dealId` as it
//! learns it (`crate::exec::stream::DealIds`, filled by the submit's confirm and by the confirm
//! watchdog) and hands the SAME map to this client (`recon_client_sharing`), so a live working
//! order that THIS process placed is reported under its own coid. Everything else reports `None`,
//! the "externally-placed order" convention every other venue's parser uses: an order from before
//! a restart (the map starts empty), one whose `dealId` was never learned, one placed in the IG UI.
//!
//! ⚠ **The coid-keyed paths DO apply to IG.** This module used to say the opposite ("reconciles by
//! presence ... the `MissingTerminal`/`OrphanLocalOrder` paths do not apply"), and that was the
//! defect: with every report's coid `None`, `vike_exec::recon::diff` could never match a live local
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
//! ## Admitted epics (any-symbol, decision 0173)
//! The client is built on the MOUNTED epic, and [`ReconClient::admit_symbol`] tells it each epic the
//! account's engine was admitted on demand. All three endpoints are account-wide on the wire, so a pass
//! makes the request it always made and parses the ONE body once per epic (the mounted epic first, then
//! each admitted one, in the order told); an admitted epic IG does not list is a FLAT row, so a stale
//! local position stays detectable. Any parse error fails the whole fetch (the restore planner reads an
//! order a successful report lacks as gone). An admitted epic is held UPPER-CASE (`AdmittedSymbols`) and
//! matched exactly, which is why the door only admits an epic IG itself spells upper-case
//! (`market_facts`).
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
//! `vike_exec::recon::diff` matches nothing else: an OPEN is its activity `dealId` (the position's own, the
//! one the confirm names), a CLOSE is its `details.dealReference` (the one the close POST returned): the
//! row of a close carries a NEW `dealId` no confirm names, so keyed on it every close the node sent came back
//! as a held `MissingFill` (`parse_activity_fills`; `crate::exec::event_mapper::confirm_trade_id` is the
//! engine's side, and `tests/offline/activity_close_matches_its_fill.rs` pins both over real captures).
//!
//! Every wire->report mapping delegates to a PURE free function (`parse_*`) over the raw JSON body,
//! fixture-tested (`tests/offline/recon_client_parse.rs`, no network) against captured IG REST shapes; the
//! `#[ignore]`d cred-gated live smoke (`tests/ig_reconcile_smoke.rs`) proves the real fetch+parse.

use serde_json::Value;

// IG returns a numeric field either as a JSON number (positions/working orders) OR as a string (the
// activity `details` block quotes `size`/`level`, e.g. `"+1"` / `"1.09"`); `get_f64` reads both and
// folds an absent/unparseable one to `0.0`. Rust's `f64` parse accepts the leading `+` IG puts on a
// signed size.
use vike_bridge_core::json::get_f64;
use vike_bridge_core::side::sign_default_buy;
use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::config::IgConfig;
use crate::data::parse_ig_time_utc;
use crate::exec::stream::{DealIdMap, DealIds};
use crate::rest::{IgApiError, IgSession};

/// This venue's `vike_model::VENUES` id — every report here carries it, and so does the start.
pub(crate) const VENUE: &str = "ig";

// --- pure parsers ------------------------------------------------------------------------------

/// Epoch-ms (UTC) -> IG's `"YYYY-MM-DDTHH:MM:SS"` datetime (the `from` query the activity endpoint
/// takes). Pure civil-calendar math over [`vike_util::calendar::civil_from_days`] — no chrono.
pub fn ms_to_ig_datetime(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let tod = secs.rem_euclid(86_400);
    let (y, mo, d) = vike_util::calendar::civil_from_days(secs.div_euclid(86_400));
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// One synthesized flat (`qty: 0`, `BOTH`) position row for `epic` — emitted when IG reports no
/// open deal for the engine epic (IG omits an epic entirely once flat), so `recon::diff` can still
/// detect a stale local position.
fn flat_position(epic: &str) -> PositionStatusReport {
    PositionStatusReport::flat(VENUE, epic)
}

/// `GET /positions` (v2) rows -> ONE aggregated `PositionStatusReport` for `epic`. IG is
/// position-per-deal, so every deal row matching the epic is netted: `size` is UNSIGNED on the wire
/// and the sign comes from `position.direction` (`SELL` -> short/negative). `avg_px` is the
/// size-weighted mean of the matched rows' open `level`s; `ts` is the newest `createdDateUTC`.
/// `position_side` is always `BOTH` with the sign in `qty` — see the module doc. No matched row ->
/// one synthesized flat row.
pub fn parse_positions(body: &str, epic: &str) -> Result<Vec<PositionStatusReport>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows =
        v.get("positions").and_then(|p| p.as_array()).ok_or("expected a `positions` array")?;

    let mut net_qty = 0.0_f64;
    let mut weighted_level = 0.0_f64;
    let mut abs_size = 0.0_f64;
    let mut ts = 0_i64;
    let mut matched = false;

    for row in rows {
        let market_epic =
            row.get("market").and_then(|m| m.get("epic")).and_then(|e| e.as_str()).unwrap_or("");
        if market_epic != epic {
            continue;
        }
        matched = true;
        let Some(pos) = row.get("position") else { continue };
        let size = get_f64(pos, "size").abs();
        let is_short = pos
            .get("direction")
            .and_then(|d| d.as_str())
            .is_some_and(|d| d.eq_ignore_ascii_case("sell"));
        let level = get_f64(pos, "level");
        net_qty += if is_short { -size } else { size };
        weighted_level += size * level;
        abs_size += size;
        let row_ts =
            pos.get("createdDateUTC").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
        ts = ts.max(row_ts);
    }

    if !matched {
        return Ok(vec![flat_position(epic)]);
    }
    let avg_px = if abs_size != 0.0 { weighted_level / abs_size } else { 0.0 };
    Ok(vec![PositionStatusReport {
        venue: VENUE.to_string(),
        symbol: epic.to_string(),
        position_side: PositionSide::Both,
        qty: net_qty,
        avg_px,
        ts,
        margin_mode: MarginMode::default(),
        isolated_margin: None,
        delta: None,
    }])
}

/// `GET /workingorders` (v2) rows -> `OrderStatusReport`, filtered to `epic`. A working order is
/// resting/unfilled by definition, so `status` is always `ACCEPTED`, `filled_qty`/`avg_px` are `0`.
/// `venue_order_id` is the `dealId`; `client_order_id` is always `None` here (see the module doc —
/// [`parse_working_orders_with_ids`] is the variant that can name it);
/// `order_type` maps `orderType` (`LIMIT` → `Limit`, `STOP` → `Stop`; any other kind stays
/// [`OrderType::Unrecognized`] lower-cased); `ts` is `createdDateUTC`.
pub fn parse_working_orders(body: &str, epic: &str) -> Result<Vec<OrderStatusReport>, String> {
    parse_working_orders_with_ids(body, epic, &|_| None)
}

/// [`parse_working_orders`] with `coid_for(dealId)` consulted for each row's `client_order_id`: a
/// `dealId` this process placed reports under its own client order id (so `recon::diff` matches the
/// live local order instead of pairing an `UnknownOrder` with an `OrphanLocalOrder`), any other
/// stays `None`.
pub fn parse_working_orders_with_ids(
    body: &str,
    epic: &str,
    coid_for: &dyn Fn(&str) -> Option<String>,
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
            if row_epic != epic {
                return None;
            }
            let direction = wo.get("direction").and_then(|d| d.as_str()).unwrap_or("");
            let order_type = match wo.get("orderType").and_then(|t| t.as_str()).unwrap_or("") {
                "LIMIT" => OrderType::Limit,
                "STOP" => OrderType::Stop,
                other => OrderType::Unrecognized(other.to_ascii_lowercase()),
            };
            let ts = wo.get("createdDateUTC").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
            let deal_id = wo.get("dealId").and_then(|d| d.as_str()).unwrap_or("");
            Some(OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: epic.to_string(),
                venue_order_id: deal_id.into(),
                client_order_id: coid_for(deal_id).filter(|c| !c.is_empty()),
                side: sign_default_buy(direction),
                order_type,
                qty: get_f64(wo, "orderSize"),
                filled_qty: 0.0,
                avg_px: 0.0,
                status: OrderStatus::Accepted,
                ts,
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
/// (`crate::exec::event_mapper::confirm_trade_id`'s rule, which a test pins from both sides): the activity
/// `dealId` of an OPEN (the position's own), but the row's `details.dealReference` of a CLOSE — the row of a
/// close carries a NEW `dealId` that no confirm names (measured 2026-10-30), while its `dealReference` is the
/// one the close POST returned and the engine keyed the close on. Keyed on that `dealId` every close the node
/// sent came back as a held `MissingFill`, and under `hybrid`/`synthesize` it would be booked twice. A close
/// row without a reference is skipped (never keyed on its `dealId`, which is that defect).
/// `venue_order_id` prefers `details.dealReference`, falling back to `dealId`. `side`/`last_qty` come from `details.size`
/// (signed, e.g. `"-1"`), preferring `details.direction` for the sign; `last_px` is `details.level`.
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
        // SKIP an id-less row rather than admit it: `vike_exec::recon::diff` matches this id against
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
            // Sign: prefer the explicit direction, else the sign of the (possibly signed) size.
            let signed_size = details.map_or(0.0, |d| get_f64(d, "size"));
            let side = match details.and_then(|d| d.get("direction")).and_then(|d| d.as_str()) {
                Some(dir) => sign_default_buy(dir),
                None if signed_size < 0.0 => -1,
                None => 1,
            };
            let last_px = details.map_or(0.0, |d| get_f64(d, "level"));
            let ts = r.get("date").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: epic.to_string(),
                trade_id,
                venue_order_id: venue_order_id.into(),
                client_order_id: None,
                side,
                last_qty: signed_size.abs(),
                last_px,
                commission: 0.0,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                ts,
            })
        })
        .collect())
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
const DEFAULT_ACTIVITY_LOOKBACK_MS: i64 = 24 * 60 * 60 * 1000;

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

/// One reconcile client for a started (account, epic), holding its OWN dedicated logged-in
/// [`IgSession`] — a reconcile fetch never contends the exec side's session, and its tokens expire
/// independently. `epic` is IG's instrument id (e.g. `"CS.D.EURUSD.MINI.IP"`), taken as the vike
/// symbol (symbol->epic search is the same follow-up the exec half defers). All fetches are
/// blocking REST on the reconcile thread — never the fold thread.
pub struct IgReconClient {
    session: IgSession,
    epic: String,
    /// `coid <-> dealId`, shared with the exec half when built through [`recon_client_sharing`]
    /// (otherwise a private empty map: every working order then reports `client_order_id: None`).
    deal_ids: DealIdMap,
    /// The epics the account's engine admitted on demand, beyond `epic`: every pass reads them too (the
    /// module doc).
    admitted: AdmittedSymbols,
}

impl IgReconClient {
    /// Log in a dedicated session for reconcile reads and bind it to `epic`. `None` on login
    /// failure — reconcile stays unwired for this venue, exec unaffected (the same graceful
    /// degradation the exec half's own login uses as the live gate).
    pub fn connect(config: &IgConfig, epic: &str) -> Option<Self> {
        Self::connect_sharing(config, epic, DealIds::shared())
    }

    /// [`Self::connect`] over the exec half's `coid <-> dealId` map.
    pub(crate) fn connect_sharing(
        config: &IgConfig,
        epic: &str,
        deal_ids: DealIdMap,
    ) -> Option<Self> {
        let session = IgSession::login(config).ok()?;
        Some(IgReconClient {
            session,
            epic: epic.to_string(),
            deal_ids,
            admitted: AdmittedSymbols::default(),
        })
    }

    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, IgApiError> {
        self.session.get(path, version, query)
    }

    /// The epics a pass reads: the mounted one first, then each admitted one.
    fn epics(&self) -> impl Iterator<Item = String> {
        std::iter::once(self.epic.clone()).chain(self.admitted.symbols())
    }

    /// The `GET /history/activity` body a pass parses per epic. Without `paging` it is the first page, as
    /// ever; with it, the `activities` of every page up to `ACTIVITY_PAGE_ROUNDS`, a row two pages both carry
    /// kept once (identical rows, NOT identical `dealId`s: a close may reuse its open's `dealId`, and the
    /// single-page read has always kept both). Still truncated after the last round, or a next page that adds
    /// no row, is an `Err`: a short report is never passed off as the whole window.
    fn activity_body(&self, query: &str, paging: bool) -> Result<String, String> {
        let mut page = self.get("/history/activity", "3", query).map_err(|e| e.to_string())?;
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
            page = self.get("/history/activity", "3", &next).map_err(|e| e.to_string())?;
        }
        Ok(serde_json::json!({ "activities": rows }).to_string())
    }
}

impl ReconClient for IgReconClient {
    /// IG working orders report the currently-resting set only, so `_since` is a no-op — a
    /// fully-closed/executed order is what the fill (not this) report reconciles. One account-wide
    /// body, parsed per epic.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let body = self.get("/workingorders", "2", "").map_err(|e| e.to_string())?.to_string();
        let mut reports = Vec::new();
        for epic in self.epics() {
            reports.extend(parse_working_orders_with_ids(&body, &epic, &|deal_id| {
                self.deal_ids.lock().unwrap().coid_for(deal_id)
            })?);
        }
        Ok(reports)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let from_ms =
            if since > 0 { since } else { vike_model::now_ms() - DEFAULT_ACTIVITY_LOOKBACK_MS };
        let query = format!("from={}&detailed=true&pageSize=50", ms_to_ig_datetime(from_ms));
        // ONE snapshot of the epics: the pages read are the pages for the epics then parsed.
        let epics: Vec<String> = self.epics().collect();
        let body = self.activity_body(&query, epics.len() > 1)?;
        let mut reports = Vec::new();
        for epic in &epics {
            reports.extend(parse_activity_fills(&body, epic)?);
        }
        Ok(reports)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let body = self.get("/positions", "2", "").map_err(|e| e.to_string())?.to_string();
        let mut reports = Vec::new();
        for epic in self.epics() {
            reports.extend(parse_positions(&body, &epic)?);
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (an epic, any-symbol, decision 0173): every later
    /// pass reads it too. Idempotent, never fetches; the list is bounded and held upper-case.
    fn admit_symbol(&self, symbol: &str) {
        if self.admitted.admit(symbol, &self.epic) {
            tracing::info!(
                venue = VENUE, symbol = %symbol.to_ascii_uppercase(),
                "reconcile: the engine admitted an epic; every pass now reads it too"
            );
        }
    }

    /// The engine gave `symbol` back (decision 0205): the next pass stops reading it.
    fn forget_symbol(&self, symbol: &str) {
        if self.admitted.forget(symbol) {
            tracing::info!(
                venue = VENUE, symbol = %symbol.to_ascii_uppercase(),
                "reconcile: the engine gave an epic back; no pass reads it any more"
            );
        }
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
    }

    /// Balance meaning: the started account's `balance.balance` from `GET /accounts` — see
    /// [`parse_balance`].
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let resp = self.get("/accounts", "1", "").map_err(|e| e.to_string())?;
        parse_balance(&resp.to_string(), &self.session.account_id)
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam): logs in a FRESH [`IgSession`] dedicated
/// to reconcile reads (isolated from the exec side's own session — the "one session per purpose"
/// idiom). `symbol` is the IG epic. `None` on login failure — reconcile stays unwired, exec
/// unaffected.
///
/// **WIRED into this crate's start** — `crates/bridges/ig/src/adapter.rs`'s `IgVenueAdapter::start`
/// calls this through `recon_if_enabled`, LAZILY, because the login here is a BLOCKING network
/// round-trip at start time (inside `vike_tradehub_core::make_engine`): a start whose reconcile gate
/// answered NO must open no second authenticated session. IG reconciles on the periodic INTERVAL
/// only (no `recon_trigger`), and its feed string is withheld from `recon_feed_statuses` on purpose
/// (`LiveFeeds::recon_feed_statuses`' ig paragraph in `vike-tradehub`), so its health gate reads
/// `Healthy` and never blocks a pass.
///
/// ⚠ This line used to say "not yet wired", which it had stopped being. A factory's header is the
/// WRONG place to read that from, because a start can adopt a factory without touching it — read
/// the start. `crates/bridges/ig/CLAUDE.md` carries the family rule and the rollout caveat that
/// belongs with this venue's order-report completeness.
pub fn recon_client(config: &IgConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    recon_client_sharing(config, symbol, DealIds::shared())
}

/// [`recon_client`] over the `coid <-> dealId` map the exec client was spawned with
/// (`IgExecutionClient::spawn_sharing`) — what the adapter's start passes, so this process's live
/// working orders match their local orders. `recon_client` is the same with a private empty map.
pub(crate) fn recon_client_sharing(
    config: &IgConfig,
    symbol: &str,
    deal_ids: DealIdMap,
) -> Option<Box<dyn ReconClient>> {
    IgReconClient::connect_sharing(config, symbol, deal_ids)
        .map(|c| Box::new(c) as Box<dyn ReconClient>)
}
