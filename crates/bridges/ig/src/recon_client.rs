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
//!       activity never executed volume) and to the mounted epic. `since` (epoch-ms) maps to the
//!       endpoint's `from` ISO-8601 datetime; `since <= 0` falls back to a 24h lookback.
//!   `GET /accounts` (v1) — the mounted account's `balance.balance` (`fetch_balance`), the
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
//! **`client_order_id` is always `None`.** IG's deal flow returns a `dealReference` at submit and a
//! `dealId` at confirm; neither the working-order list nor the activity feed echoes any
//! client-supplied id, so every report carries `None` — the "externally-placed order" convention
//! every other venue's parser already uses for an absent client id. (Consequence: an IG working
//! order local placed reconciles by presence, not by coid; the coid-keyed `MissingTerminal`/
//! `OrphanLocalOrder` paths that need an echoed id do not apply to this venue — documented, not a
//! silent omission.)
//!
//! **No live fee-rate lane.** IG charges spread/overnight funding, not a per-account maker/taker
//! rate over this API, so `fetch_fee_rates` stays the trait default (`Ok(None)`) — same as every
//! un-wired venue.
//!
//! Every wire->report mapping delegates to a PURE free function (`parse_*`) over the raw JSON body,
//! fixture-tested (`tests/recon_client_parse.rs`, no network) against captured IG REST shapes; the
//! `#[ignore]`d cred-gated live smoke (`tests/ig_reconcile_smoke.rs`) proves the real fetch+parse.

use serde_json::Value;

use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::config::IgConfig;
use crate::data::parse_ig_time_utc;
use crate::rest::{IgApiError, IgSession};

const VENUE: &str = "ig";

// --- pure parsers ------------------------------------------------------------------------------

/// `"SELL"` -> `-1`, anything else (`"BUY"`) -> `+1` — the inverse of `exec::direction`.
fn side_sign(direction: &str) -> i32 {
    if direction.eq_ignore_ascii_case("sell") { -1 } else { 1 }
}

/// A numeric field that IG returns either as a JSON number (positions/working orders) OR as a
/// string (the activity `details` block quotes `size`/`level`, e.g. `"+1"` / `"1.09"`). Absent /
/// unparseable folds to `0.0` rather than erroring the whole row. Rust's `f64` parse accepts the
/// leading `+` IG puts on a signed size.
fn flex_f64(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Epoch-ms (UTC) -> IG's `"YYYY-MM-DDTHH:MM:SS"` datetime (the `from` query the activity endpoint
/// takes). Pure civil-calendar math over [`vike_model::time::civil_from_days`] — no chrono.
pub fn ms_to_ig_datetime(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let tod = secs.rem_euclid(86_400);
    let (y, mo, d) = vike_model::time::civil_from_days(secs.div_euclid(86_400));
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

/// One synthesized flat (`qty: 0`, `BOTH`) position row for `epic` — emitted when IG reports no
/// open deal for the mounted epic (IG omits an epic entirely once flat), so `recon::diff` can still
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
        let size = flex_f64(pos, "size").abs();
        let is_short = pos
            .get("direction")
            .and_then(|d| d.as_str())
            .is_some_and(|d| d.eq_ignore_ascii_case("sell"));
        let level = flex_f64(pos, "level");
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
/// `venue_order_id` is the `dealId`; `client_order_id` is always `None` (see the module doc);
/// `order_type` is `orderType` lower-cased (`limit`/`stop`); `ts` is `createdDateUTC`.
pub fn parse_working_orders(body: &str, epic: &str) -> Result<Vec<OrderStatusReport>, String> {
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
            let order_type =
                wo.get("orderType").and_then(|t| t.as_str()).unwrap_or("").to_ascii_lowercase();
            let ts = wo.get("createdDateUTC").and_then(|t| t.as_str()).map_or(0, parse_ig_time_utc);
            Some(OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: epic.to_string(),
                venue_order_id: wo.get("dealId").and_then(|d| d.as_str()).unwrap_or("").into(),
                client_order_id: None,
                side: side_sign(direction),
                order_type,
                qty: flex_f64(wo, "orderSize"),
                filled_qty: 0.0,
                avg_px: 0.0,
                status: "ACCEPTED".to_string(),
                ts,
            })
        })
        .collect())
}

/// `GET /history/activity` (v3, `detailed=true`) rows -> `FillReport`, filtered to executed deals
/// (`type == "POSITION"` + `status == "ACCEPTED"` — a rejected/deleted activity executed no volume)
/// and to `epic`. `trade_id` is the activity `dealId`; `venue_order_id` prefers
/// `details.dealReference`, falling back to `dealId`. `side`/`last_qty` come from `details.size`
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
            let trade_id = match TradeId::new(deal_id) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %epic,
                        "activity row carries no dealId — skipping the fill report; an id-less one \
                         would fabricate a MissingFill that `hybrid` auto-applies"
                    );
                    return None;
                }
            };
            let venue_order_id = details
                .and_then(|d| d.get("dealReference"))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(deal_id);
            // Sign: prefer the explicit direction, else the sign of the (possibly signed) size.
            let signed_size = details.map_or(0.0, |d| flex_f64(d, "size"));
            let side = match details.and_then(|d| d.get("direction")).and_then(|d| d.as_str()) {
                Some(dir) => side_sign(dir),
                None if signed_size < 0.0 => -1,
                None => 1,
            };
            let last_px = details.map_or(0.0, |d| flex_f64(d, "level"));
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

/// `GET /accounts` (v1) -> the mounted account's `balance.balance` (total account funds — NOT
/// `available`, which nets running margin, nor equity, which adds open P&L), matching every other
/// venue's "wallet balance" `fetch_balance` convention. Picks the row whose `accountId` matches
/// `account_id`, falling back to the first account. `None` when the field is absent (fail-soft);
/// malformed JSON is a hard `Err`.
pub fn parse_balance(body: &str, account_id: &str) -> Result<Option<f64>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let accounts =
        v.get("accounts").and_then(|a| a.as_array()).ok_or("expected an `accounts` array")?;
    let account = accounts
        .iter()
        .find(|a| a.get("accountId").and_then(|id| id.as_str()) == Some(account_id))
        .or_else(|| accounts.first());
    Ok(account
        .and_then(|a| a.get("balance"))
        .and_then(|b| b.get("balance"))
        .and_then(serde_json::Value::as_f64))
}

// --- the client ----------------------------------------------------------------------------

/// A 24h lookback (ms) for the activity `from` param when the caller passes `since <= 0` (an
/// unbounded pass) — IG's activity endpoint requires a `from`, so a floor is needed.
const DEFAULT_ACTIVITY_LOOKBACK_MS: i64 = 24 * 60 * 60 * 1000;

/// One reconcile client for a mounted (account, epic), holding its OWN dedicated logged-in
/// [`IgSession`] — a reconcile fetch never contends the exec side's session, and its tokens expire
/// independently. `epic` is IG's instrument id (e.g. `"CS.D.EURUSD.MINI.IP"`), taken as the vike
/// symbol (symbol->epic search is the same follow-up the exec half defers). All fetches are
/// blocking REST on the reconcile thread — never the fold thread.
pub struct IgReconClient {
    session: IgSession,
    epic: String,
}

impl IgReconClient {
    /// Log in a dedicated session for reconcile reads and bind it to `epic`. `None` on login
    /// failure — reconcile stays unwired for this venue, exec unaffected (the same graceful
    /// degradation the exec half's own login uses as the live gate).
    pub fn connect(config: &IgConfig, epic: &str) -> Option<Self> {
        let session = IgSession::login(config).ok()?;
        Some(IgReconClient { session, epic: epic.to_string() })
    }

    /// Test/seam constructor from an already-logged-in session (the live smoke reuses the session
    /// it logged in with; production goes through [`Self::connect`]).
    pub fn from_session(session: IgSession, epic: &str) -> Self {
        IgReconClient { session, epic: epic.to_string() }
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn get(&self, path: &str, version: &str, query: &str) -> Result<Value, IgApiError> {
        self.session.get(path, version, query)
    }
}

impl ReconClient for IgReconClient {
    /// IG working orders report the currently-resting set only, so `_since` is a no-op — a
    /// fully-closed/executed order is what the fill (not this) report reconciles.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let resp = self.get("/workingorders", "2", "").map_err(|e| e.to_string())?;
        parse_working_orders(&resp.to_string(), &self.epic)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let from_ms = if since > 0 { since } else { Self::now_ms() - DEFAULT_ACTIVITY_LOOKBACK_MS };
        let query = format!("from={}&detailed=true&pageSize=50", ms_to_ig_datetime(from_ms));
        let resp = self.get("/history/activity", "3", &query).map_err(|e| e.to_string())?;
        parse_activity_fills(&resp.to_string(), &self.epic)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let resp = self.get("/positions", "2", "").map_err(|e| e.to_string())?;
        parse_positions(&resp.to_string(), &self.epic)
    }

    /// Balance meaning: the mounted account's `balance.balance` from `GET /accounts` — see
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
/// **WIRED into `vike_mount::make_engine`** — its `("ig", _)` arm calls this through
/// `recon_if_enabled`, LAZILY, because the login here is a BLOCKING network round-trip inside
/// `make_engine`: an unset `VIKE_RECONCILE` must open no second authenticated session. IG
/// reconciles on the periodic INTERVAL only (no `recon_trigger`), and having no market_feed it is
/// absent from `recon_feed_statuses`, so its health gate reads `Healthy` and never blocks a pass.
///
/// ⚠ This line used to say "not yet wired", which it had stopped being. A factory's header is the
/// WRONG place to read that from, because a mount arm can adopt a factory without touching it —
/// read the arm. `crates/bridges/ig/CLAUDE.md` carries the family rule and the rollout caveat that
/// belongs with this venue's order-report completeness.
pub fn recon_client(config: &IgConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    IgReconClient::connect(config, symbol).map(|c| Box::new(c) as Box<dyn ReconClient>)
}
