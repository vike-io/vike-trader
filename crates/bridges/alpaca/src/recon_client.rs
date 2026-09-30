//! Alpaca `ReconClient` (ReconFactory seam, wave-2 task 6) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) over the SAME Bearer `AlpacaRest` client [`crate::exec`]
//! already builds, reconciling against the Broker API:
//!
//!   `GET /v1/trading/accounts/{account_id}/orders` (order reports) — `status=all` plus a
//!       `symbols=` filter scopes the response to the mounted symbol SERVER-SIDE (unlike every
//!       other venue's "currently open only" report, Alpaca's `orders` endpoint returns the
//!       WHOLE account's order history when unfiltered, so the symbol filter is load-bearing,
//!       not just an optimization).
//!   `GET /v1/accounts/activities/FILL` (fill reports — Alpaca's per-fill "trade activity" feed;
//!       this endpoint has no server-side symbol filter, so [`parse_fills`] filters client-side).
//!   `GET /v1/trading/accounts/{account_id}/positions` (position reports — also no per-symbol
//!       filter; [`parse_positions`] filters client-side and synthesizes a flat row for the
//!       mounted symbol when it's absent — Alpaca omits a symbol entirely once its position is
//!       flat, the same convention `vike_okx::recon_client::parse_positions` already documents).
//!   `GET /v1/trading/accounts/{account_id}/account` (`fetch_balance` — the account's `cash`,
//!       the "wallet balance, not available/equity" convention every other venue's
//!       `fetch_balance` already picks).
//!
//! **Symbol spelling.** Alpaca's wire form slashes bare crypto pairs (`BTCUSD` -> `BTC/USD`, see
//! [`crate::event_mapper::to_alpaca_symbol`]) but every report row this client returns carries the
//! UNIFIED vike-side spelling (whatever `symbol` the caller mounted), never the wire form —
//! `recon::diff::diff` matches a position report's `.symbol` directly against `LocalView`'s keys,
//! so a report row spelled `"BTC/USD"` against local state keyed `"BTCUSD"` would silently never
//! match. [`parse_orders`]/[`parse_fills`]/[`parse_positions`] therefore take BOTH the Alpaca wire
//! symbol (to filter/match rows) and the vike symbol (written into every returned row).
//!
//! **`since` is currently a no-op on every fetch (documented gap, not a silent omission):**
//! Alpaca's `after`/`date` query params take an RFC3339 timestamp, not epoch-ms, and this factory
//! is not wired into any live mount yet (wave-2 task 6 ships the seam, not the live wiring — see
//! `vike_mount`'s doc). Every report is refetched in full each pass; wiring a ms->RFC3339
//! conversion is deferred to whenever a real mount needs bounded reconcile passes.
//!
//! **No live fee-rate lane:** the Broker API does not document a per-account maker/taker rate
//! endpoint (US equities/crypto here are commission-passthrough, not maker/taker), so
//! `fetch_fee_rates` stays the trait default (`Ok(None)`) — same as every un-wired venue.
//!
//! Every fetch delegates to a PURE free function (`parse_*`) over the raw JSON body — the
//! fixture-tested units (`tests/recon_client_parse.rs`, no network) against captured Broker API
//! shapes (`alpacahq/alpaca-docs`, verified 2026-07-20).

use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::config::AlpacaConfig;
use crate::data::parse_rfc3339_ms;
use crate::event_mapper::to_alpaca_symbol;
use crate::rest::AlpacaRest;

const VENUE: &str = "alpaca";

// --- pure parsers ------------------------------------------------------------------------------

/// `"buy"` -> `+1`, anything else (`"sell"`) -> `-1` — matches `event_mapper::build_order_body`'s
/// own `if req.side >= 0 { "buy" } else { "sell" }` inverse.
fn side_sign(s: &str) -> i32 {
    if s.eq_ignore_ascii_case("sell") { -1 } else { 1 }
}

/// A wire numeric field arrives as a JSON STRING (Alpaca quotes every decimal) or `null` — `null`/
/// absent/unparseable folds to `0.0` (notional-only orders carry no `qty`, for instance) rather
/// than erroring the whole row.
fn str_f64(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

/// Normalize an Alpaca order `status` to the `OrderStatus::parse` FSM vocabulary. Every
/// still-resting/in-flight status (`new`/`accepted`/`pending_new`/`accepted_for_bidding`/
/// `pending_replace`/`replaced`/`stopped`/`suspended`/`calculated`) maps to `ACCEPTED`;
/// `done_for_day` and `pending_cancel` map to `CANCELED` (the order stops being live either way).
fn normalize_order_status(raw: &str) -> String {
    match raw {
        "new"
        | "accepted"
        | "pending_new"
        | "accepted_for_bidding"
        | "pending_replace"
        | "replaced"
        | "stopped"
        | "suspended"
        | "calculated" => "ACCEPTED".to_string(),
        "partially_filled" => "PARTIALLY_FILLED".to_string(),
        "filled" => "FILLED".to_string(),
        "canceled" | "pending_cancel" | "done_for_day" => "CANCELED".to_string(),
        "expired" => "EXPIRED".to_string(),
        "rejected" => "REJECTED".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

/// `GET /v1/trading/accounts/{account_id}/orders?symbols=…` rows -> `OrderStatusReport`. Every
/// row is kept — an empty/absent `client_order_id` maps to `None` (an externally-placed order),
/// mirroring binance/bybit/OKX. `order_type` prefers the `order_type` field, falling back to
/// `type` (the Broker API documents both, normally identical). `ts` is `updated_at`.
pub fn parse_orders(
    body: &str,
    alpaca_symbol: &str,
    symbol: &str,
) -> Result<Vec<OrderStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter(|o| o.get("symbol").and_then(|s| s.as_str()) == Some(alpaca_symbol))
        .map(|o| {
            let client_order_id = o
                .get("client_order_id")
                .and_then(|c| c.as_str())
                .filter(|c| !c.is_empty())
                .map(|c| c.to_string());
            let order_type = o
                .get("order_type")
                .or_else(|| o.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let ts = o
                .get("updated_at")
                .and_then(|t| t.as_str())
                .and_then(parse_rfc3339_ms)
                .unwrap_or(0);
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: o.get("id").and_then(|s| s.as_str()).unwrap_or("").into(),
                client_order_id,
                side: side_sign(o.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                order_type,
                qty: str_f64(o, "qty"),
                filled_qty: str_f64(o, "filled_qty"),
                avg_px: str_f64(o, "filled_avg_price"),
                status: normalize_order_status(
                    o.get("status").and_then(|s| s.as_str()).unwrap_or(""),
                ),
                ts,
            }
        })
        .collect())
}

/// `GET /v1/accounts/activities/FILL` rows -> `FillReport`, filtered to `alpaca_symbol` (this
/// endpoint has no server-side symbol filter). `activity_type == "FILL"` is asserted defensively
/// (the path already scopes to FILL activities, but a future caller might hit the unscoped
/// `/v1/accounts/activities` endpoint instead). No commission/liquidity fields are documented on
/// this activity shape, so `commission: 0.0`/`commission_asset: ""`/`liquidity_side: Unknown` —
/// the same "field genuinely absent from the wire" fail-soft every other venue's parser uses.
/// `ts` is `transaction_time`.
pub fn parse_fills(
    body: &str,
    alpaca_symbol: &str,
    symbol: &str,
) -> Result<Vec<FillReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter(|r| r.get("activity_type").and_then(|t| t.as_str()) == Some("FILL"))
        .filter(|r| r.get("symbol").and_then(|s| s.as_str()) == Some(alpaca_symbol))
        // SKIP the row, do not fail the batch and do not admit an id-less report. An activity `id`
        // is Alpaca's documented per-fill identity and the key `vike_exec::recon::diff` looks up in
        // `seen_trade_ids` to decide whether the fold thread already booked this fill. An EMPTY id
        // can never match, so it does not merely go unrecognised — it manufactures a `MissingFill`
        // divergence, and `MissingFill` is one of the two kinds the `hybrid` policy AUTO-APPLIES:
        // the fill gets synthesized and booked a second time with no operator in front of it.
        // Skipping is the safe direction (no divergence, so nothing is auto-applied); failing the
        // whole batch would discard the good rows with the bad one.
        // ⚠ Was `.unwrap_or("")`.
        .filter_map(|f| {
            let trade_id = match TradeId::new(f.get("id").and_then(|s| s.as_str()).unwrap_or("")) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %symbol,
                        "FILL activity row carries no `id` — skipping it; an id-less fill report \
                         would fabricate a MissingFill divergence that `hybrid` auto-applies"
                    );
                    return None;
                }
            };
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: f.get("order_id").and_then(|s| s.as_str()).unwrap_or("").into(),
                // Activities carry no client-order-id field at all (unlike the orders endpoint).
                client_order_id: None,
                side: side_sign(f.get("side").and_then(|s| s.as_str()).unwrap_or("")),
                last_qty: str_f64(f, "qty"),
                last_px: str_f64(f, "price"),
                commission: 0.0,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                ts: f
                    .get("transaction_time")
                    .and_then(|t| t.as_str())
                    .and_then(parse_rfc3339_ms)
                    .unwrap_or(0),
            })
        })
        .collect())
}

/// `GET /v1/trading/accounts/{account_id}/positions` rows -> `PositionStatusReport`, filtered to
/// `alpaca_symbol` (no server-side symbol filter documented). `qty` is UNSIGNED on the wire — the
/// sign comes from `side` (`"long"`/`"short"`), mirroring Bybit's unsigned-`size`-plus-`side`
/// convention. An absent/non-matching row synthesizes ONE flat (`qty: 0.0`, `PositionSide::Both`)
/// row for `symbol` — Alpaca omits a symbol entirely once its position is flat (no `qty: "0"` row
/// sent), the same gap `vike_okx::recon_client::parse_positions` documents and works around, so a
/// stale local position stays detectable. `ts` is always `0` — the position object carries no
/// per-row timestamp.
pub fn parse_positions(
    body: &str,
    alpaca_symbol: &str,
    symbol: &str,
) -> Result<Vec<PositionStatusReport>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a JSON array".to_string())?;
    let matched: Vec<PositionStatusReport> = rows
        .iter()
        .filter(|p| p.get("symbol").and_then(|s| s.as_str()) == Some(alpaca_symbol))
        .map(|p| {
            let is_short = p
                .get("side")
                .and_then(|s| s.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("short"));
            let unsigned_qty = str_f64(p, "qty").abs();
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                position_side: if is_short { PositionSide::Short } else { PositionSide::Long },
                qty: if is_short { -unsigned_qty } else { unsigned_qty },
                avg_px: str_f64(p, "avg_entry_price"),
                ts: 0,
                margin_mode: MarginMode::default(),
                isolated_margin: None,
                delta: None,
            }
        })
        .collect();
    if matched.is_empty() {
        return Ok(vec![PositionStatusReport::flat(VENUE, symbol)]);
    }
    Ok(matched)
}

/// `GET /v1/trading/accounts/{account_id}/account` -> the account's `cash` (total tradable cash —
/// NOT `equity`, which includes unrealized P&L on open positions), matching every other venue's
/// "wallet balance, not available/equity" `fetch_balance` convention. `None` when the field is
/// absent (fail-soft); malformed JSON is a hard `Err`.
pub fn parse_balance(body: &str) -> Result<Option<f64>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("expected a JSON object".to_string());
    }
    Ok(v.get("cash").and_then(|c| c.as_str()).and_then(|s| s.parse().ok()))
}

// --- the client ----------------------------------------------------------------------------

/// One reconcile client per (account, venue-symbol) — the SAME `AlpacaRest` Bearer seam
/// [`crate::exec::AlpacaExecutionClient`] uses, dedicated to reconcile reads. `symbol` is the
/// unified vike-side spelling (e.g. `"AAPL"`, `"BTCUSD"`); the Alpaca wire form
/// (`event_mapper::to_alpaca_symbol`) is derived once at construction and reused by every fetch.
pub struct AlpacaReconClient {
    rest: AlpacaRest,
    base: String,
    account_id: String,
    symbol: String,
    alpaca_symbol: String,
}

impl AlpacaReconClient {
    pub fn new(
        rest: AlpacaRest,
        base: impl Into<String>,
        account_id: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        let symbol = symbol.into();
        let alpaca_symbol = to_alpaca_symbol(&symbol);
        AlpacaReconClient {
            rest,
            base: base.into(),
            account_id: account_id.into(),
            symbol,
            alpaca_symbol,
        }
    }
}

impl ReconClient for AlpacaReconClient {
    /// `_since` is a documented no-op — see the module doc.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let path = format!("/v1/trading/accounts/{}/orders", self.account_id);
        let query = vike_bridge_core::urlencode(&[
            ("status", "all".to_string()),
            ("symbols", self.alpaca_symbol.clone()),
        ]);
        let resp = self.rest.get(&self.base, &path, &query).map_err(|e| e.to_string())?;
        parse_orders(&resp.to_string(), &self.alpaca_symbol, &self.symbol)
    }

    /// `_since` is a documented no-op — see the module doc.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let query = vike_bridge_core::urlencode(&[("account_id", self.account_id.clone())]);
        let resp = self
            .rest
            .get(&self.base, "/v1/accounts/activities/FILL", &query)
            .map_err(|e| e.to_string())?;
        parse_fills(&resp.to_string(), &self.alpaca_symbol, &self.symbol)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let path = format!("/v1/trading/accounts/{}/positions", self.account_id);
        let resp = self.rest.get(&self.base, &path, "").map_err(|e| e.to_string())?;
        parse_positions(&resp.to_string(), &self.alpaca_symbol, &self.symbol)
    }

    /// Balance meaning (pinned per the task brief): the account's `cash` from
    /// `/v1/trading/accounts/{account_id}/account` — see [`parse_balance`].
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let path = format!("/v1/trading/accounts/{}/account", self.account_id);
        let resp = self.rest.get(&self.base, &path, "").map_err(|e| e.to_string())?;
        parse_balance(&resp.to_string())
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6). Builds a FRESH
/// `TokenSource`/`AlpacaRest` dedicated to reconcile reads — a second OAuth2 client-credentials
/// lifecycle, isolated from the exec side's own (mirrors the "one fresh REST client instance per
/// purpose" idiom every credentialed venue's factory already follows). `symbol` is the unified
/// vike-side symbol, NOT Alpaca's slashed crypto wire form (see the module doc). Always `Some` —
/// construction here is pure/infallible (no network); `Option` is kept so the return type matches
/// every other bridge's `recon_client`.
///
/// **WIRED into `vike_mount::make_engine`** — its `("alpaca", _)` arm calls this EAGERLY (unlike
/// the lazily-built venues, construction here is pure and network-free, so there is nothing to
/// defer), and `crates/vike-mount/src/startup.rs` reuses the same factory to build this venue's
/// preflight credential probe. Alpaca reconciles on the periodic INTERVAL only — no
/// `recon_trigger`.
///
/// ⚠ This line used to say "not yet wired ... Alpaca is not a live-mounted venue today", which it
/// had stopped being. A factory's header is the WRONG place to read that from, because a mount arm
/// can adopt a factory without touching it — read the arm. `crates/bridges/ig/CLAUDE.md` carries
/// the family rule: every venue enrolled in the reconcile roster after its factory was written
/// inherited the same stale sentence.
pub fn recon_client(config: &AlpacaConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    let token = std::sync::Arc::new(crate::auth::TokenSource::new(
        config.client_id.clone(),
        config.client_secret.clone(),
        config.hosts.authx.to_string(),
    ));
    let rest = AlpacaRest::new(token);
    Some(Box::new(AlpacaReconClient::new(
        rest,
        config.hosts.broker,
        config.account_id.clone(),
        symbol.to_string(),
    )))
}
