//! OANDA `ReconClient` (recon-breadth → OANDA) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) over the v20 REST surface, reusing the crate's existing
//! Bearer-token [`OandaRest`] transport. This is genuinely new: OANDA's existing "reconcile" is the
//! [`crate::history`] transactions-since backfill (the audit-A3 post-reconnect gap replay — a LIVE
//! order-event recovery path), NOT a `fetch_*` report seam the reconcile engine diffs against local
//! state. This module is that seam.
//!
//! ## Endpoints (four flat v20 GETs, one per report kind)
//! Each rides the SAME rustls `ureq` [`OandaRest`] the data/exec sides use — a DEDICATED reconcile
//! instance built at [`OandaReconClient::connect`], never the exec client's (the "dedicated
//! reconcile reads" discipline binance/deribit/ctrader/ibkr all follow). All requests carry
//! `Accept-Datetime-Format: UNIX`, so every OANDA timestamp arrives as an epoch-seconds STRING and
//! every numeric field (units/price/commission/balance) as a decimal STRING — hence [`json_num`]
//! (string-or-number tolerant) everywhere, never `as_f64`.
//! - orders — `GET /v3/accounts/{id}/orders?instrument={I}&state=PENDING` (the currently-resting
//!   set — a fully-closed order that fell out of it is what the fill/position reports reconcile,
//!   the same "pending only" semantics bybit/ctrader use).
//! - positions — `GET /v3/accounts/{id}/positions` (every instrument the account has traded,
//!   INCLUDING flat rows), net-folded and filtered to the mounted instrument.
//! - fills — `GET /v3/accounts/{id}/transactions/sinceid?id={floor}&type=ORDER_FILL`, the SAME
//!   `ORDER_FILL` wire shape [`crate::stream`]/[`crate::history`] already decode, so a reconcile
//!   fill maps identically to a live one. See the fill-floor note below.
//! - balance — `GET /v3/accounts/{id}/summary` → `account.balance` (home-currency cash truth).
//!
//! ## Per-symbol, instrument-filtered (the ctrader/ibkr/binance-perp shape)
//! One [`OandaReconClient`] per (account, canonical vike symbol e.g. `EURUSD`). The canonical symbol
//! is mapped ONCE at connect to its OANDA instrument form (`EURUSD` → `EUR_USD`, via
//! [`to_oanda_instrument`]); every report fetch is account-wide on the wire and filtered client-side
//! to that instrument, stamping the CANONICAL symbol back onto each row so it matches local state.
//! Positions synthesize a FLAT zero row when the instrument is absent (an instrument the account
//! never traded is omitted from `/positions` entirely), so `recon::diff` can still detect a stale
//! LOCAL position the venue has since closed — the same synthesize-flat contract okx/ctrader/ibkr use.
//!
//! ## The fill floor (why `since` is a no-op here)
//! OANDA's object-returning transaction endpoint is `sinceid` (keyed by a monotonic transaction id),
//! and there is no clean map from the trait's `since` (epoch-ms) to a transaction id. So `since` is
//! ignored — instead the client holds a `fill_floor` transaction-id watermark, initialized at
//! connect to the account's current `lastTransactionID` and advanced past each fetched batch (the
//! SAME watermark discipline `exec`/`stream`'s shared `last_seen` uses). The window is therefore
//! "fills since reconcile started, advancing each pass", not a `since`-bounded lookback: pre-connect
//! fills are the position report's job, and the reconcile engine's `trade_id` dedup absorbs any
//! re-seen row. Advancing the floor (rather than a fixed connect-time floor) also keeps each request
//! bounded, below OANDA's per-request transaction cap over a long session.
//!
//! ## No live fee lane
//! OANDA's cost is spread-based (standard accounts are commission-free; core-pricing commission is
//! reported per-fill, already carried on the fill report), and there is no per-account maker/taker
//! rate endpoint — so `fetch_fee_rates` stays the trait default (`Ok(None)`), and the caller keeps
//! the static [`vike_model::fee_schedule_for`] default (fail-soft).
//!
//! Every wire→report mapping is a PURE free function (`parse_*`) over the already-parsed
//! `serde_json::Value` (the crate convention — cf. [`crate::data::parse_candles`]), fixture-tested
//! offline in `tests/oanda_reconcile_parse.rs` (synthetic bodies — no network). The `#[ignore]`d,
//! cred-gated `tests/oanda_reconcile_smoke.rs` proves the fetch+parse read-only against a live
//! fxPractice account.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use vike_bridge_core::json::json_num;
use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::config::OandaConfig;
use crate::data::to_oanda_instrument;
use crate::rest::{OandaApiError, OandaRest};

/// The venue key stamped on every report row.
pub const VENUE: &str = "oanda";

// --- pure field helpers (OANDA sends every scalar as a STRING under `Accept-Datetime-Format: UNIX`) -

/// A present string field, `""` when absent/non-string (never panics).
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

/// A numeric field via [`json_num`] (parses OANDA's decimal STRINGS *and* bare numbers); `0.0` when
/// absent/unparseable.
fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(json_num).unwrap_or(0.0)
}

/// An OANDA epoch-seconds timestamp string (`"1478012400.000000000"`) → epoch-MILLIS. Absent/
/// unparseable → `0`. The SAME conversion [`crate::stream::fill_from_transaction`] applies.
fn ts_ms(v: &Value, key: &str) -> i64 {
    s(v, key).parse::<f64>().ok().map_or(0, |secs| (secs * 1000.0) as i64)
}

/// The order/fill's echoed `clientExtensions.id` (vike sets it = client_order_id). Empty/absent →
/// `None` — the "externally-placed order" convention every venue parser uses for an absent client id.
fn client_order_id(v: &Value) -> Option<String> {
    v.pointer("/clientExtensions/id")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// OANDA `units` are a SIGNED string (long > 0, short < 0). `< 0` → sell (`-1`), else buy (`+1`).
fn side_of(units: f64) -> i32 {
    if units < 0.0 {
        -1
    } else {
        1
    }
}

// --- pure parsers (over the already-parsed v20 JSON bodies — the fixture-tested units) -----------

/// Normalize an OANDA order `state` to the `OrderStatus::parse` FSM vocabulary `recon::diff` reads.
/// `FILLED`/`CANCELLED` are the only terminal states surfaced here; `PENDING`/`TRIGGERED` (and any
/// unknown value in the working set — a `/orders?state=PENDING` fetch only returns resting orders)
/// fold to `ACCEPTED`. OANDA carries no partial-fill state on the *order* object (the filled portion
/// becomes a separate Trade), so there is no `PARTIALLY_FILLED` derive here.
pub fn normalize_order_state(state: &str) -> String {
    match state {
        "FILLED" => "FILLED",
        "CANCELLED" => "CANCELED",
        _ => "ACCEPTED",
    }
    .to_string()
}

/// `{"orders":[Order,..]}` → [`OrderStatusReport`], filtered to `instrument` and stamped with the
/// canonical `symbol`. `id` → `venue_order_id`; `clientExtensions.id` → `client_order_id`; `units`
/// (signed) → side/qty; `type` lower-cased → `order_type`; `state` normalized; `createTime` → ts.
/// `filled_qty`/`avg_px` are `0.0` — a resting OANDA order carries no fill progress (the filled
/// portion is a separate Trade). Instrument-less exit orders (TAKE_PROFIT/STOP_LOSS/… reference a
/// `tradeID`, not an instrument) never match and are dropped — reconcile tracks entry orders.
pub fn parse_order_reports(v: &Value, instrument: &str, symbol: &str) -> Vec<OrderStatusReport> {
    let Some(orders) = v.get("orders").and_then(|o| o.as_array()) else {
        return Vec::new();
    };
    orders
        .iter()
        .filter(|o| s(o, "instrument") == instrument)
        .map(|o| {
            let units = num(o, "units");
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: s(o, "id").into(),
                client_order_id: client_order_id(o),
                side: side_of(units),
                order_type: s(o, "type").to_ascii_lowercase(),
                qty: units.abs(),
                filled_qty: 0.0,
                avg_px: 0.0,
                status: normalize_order_state(s(o, "state")),
                ts: ts_ms(o, "createTime"),
            }
        })
        .collect()
}

/// `{"positions":[Position,..]}` → [`PositionStatusReport`], filtered to `instrument` and stamped
/// with the canonical `symbol`. OANDA reports a Position as separate `long` (units ≥ 0) and `short`
/// (units ≤ 0) legs; the NET is their sum, and `avg_px` is the dominant leg's `averagePrice`
/// (`long`'s when net long, `short`'s when net short, `0.0` when flat). OANDA is a NET/one-way FX
/// account, so `position_side` is [`PositionSide::Both`] and the SIGN of `qty` carries direction —
/// the same net convention binance-perp/ibkr use. No isolated wallet → `margin_mode` stays the Cross
/// default. When NO row matches, the caller synthesizes a flat zero row (see
/// [`OandaReconClient::fetch_position_status_reports`]).
pub fn parse_position_reports(
    v: &Value,
    instrument: &str,
    symbol: &str,
) -> Vec<PositionStatusReport> {
    let Some(positions) = v.get("positions").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    positions
        .iter()
        .filter(|p| s(p, "instrument") == instrument)
        .map(|p| {
            let long = p.get("long");
            let short = p.get("short");
            let long_units = long.map(|l| num(l, "units")).unwrap_or(0.0);
            let short_units = short.map(|sh| num(sh, "units")).unwrap_or(0.0); // already ≤ 0
            let net = long_units + short_units;
            let avg_px = if net > 0.0 {
                long.map(|l| num(l, "averagePrice")).unwrap_or(0.0)
            } else if net < 0.0 {
                short.map(|sh| num(sh, "averagePrice")).unwrap_or(0.0)
            } else {
                0.0
            };
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                position_side: PositionSide::Both,
                qty: net,
                avg_px,
                ts: 0, // a Position row carries no timestamp
                margin_mode: MarginMode::default(),
                isolated_margin: None,
                delta: None,
            }
        })
        .collect()
}

/// `{"transactions":[Transaction,..]}` (a `sinceid` body) → [`FillReport`], keeping only
/// `ORDER_FILL` transactions for `instrument` and stamping the canonical `symbol`. `id` (the
/// transaction id) → `trade_id`; `orderID` → `venue_order_id`; `clientExtensions.id` →
/// `client_order_id`; `units` (signed) → side/qty; `price`/`commission` via [`json_num`]; `time` →
/// ts. `commission_asset` is the account `home_currency` (OANDA charges/reports in home currency;
/// the fill row itself carries no currency field); `liquidity_side` is always `Unknown` (no
/// maker/taker flag on the wire). The `type == "ORDER_FILL"` guard is kept even though the request
/// asks for `type=ORDER_FILL` — belt-and-suspenders, since a body may echo other kinds.
pub fn parse_fill_reports(
    v: &Value,
    instrument: &str,
    symbol: &str,
    home_currency: &str,
) -> Vec<FillReport> {
    let Some(txns) = v.get("transactions").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    txns.iter()
        .filter(|t| s(t, "type") == "ORDER_FILL" && s(t, "instrument") == instrument)
        // SKIP the row rather than admit an id-less report. `vike_exec::recon::diff` looks this id up
        // in `seen_trade_ids` to decide whether the fold thread already booked the fill; an empty id
        // can never match, so it does not go unrecognised — it FABRICATES a `MissingFill`, which the
        // `hybrid` policy AUTO-APPLIES, booking the fill a second time with no operator in front of
        // it. Skipping produces no divergence, which is the safe direction.
        .filter_map(|t| {
            let trade_id = match TradeId::new(s(t, "id")) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %symbol,
                        "ORDER_FILL transaction carries no `id` — skipping the fill report; an \
                         id-less one would fabricate a MissingFill that `hybrid` auto-applies"
                    );
                    return None;
                }
            };
            let units = num(t, "units");
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: s(t, "orderID").into(),
                client_order_id: client_order_id(t),
                side: side_of(units),
                last_qty: units.abs(),
                last_px: num(t, "price"),
                commission: num(t, "commission"),
                commission_asset: home_currency.to_string(),
                liquidity_side: LiquiditySide::Unknown,
                ts: ts_ms(t, "time"),
            })
        })
        .collect()
}

/// A `/summary` body → the account's home-currency `balance` (`account.balance`). `None` when the
/// field is absent — the account's realized cash truth `recon::diff_balance` reconciles against.
pub fn parse_balance(v: &Value) -> Option<f64> {
    v.get("account").and_then(|a| a.get("balance")).and_then(json_num)
}

/// The synthetic flat row for an instrument the account has never traded (absent from `/positions`)
/// — load-bearing: `recon::diff` needs the row PRESENT to detect a stale LOCAL position the venue
/// has since closed.
fn flat_position(symbol: &str) -> PositionStatusReport {
    PositionStatusReport::flat(VENUE, symbol)
}

// --- the client ----------------------------------------------------------------------------------

/// One reconcile client per (account, canonical vike symbol), holding its OWN dedicated
/// [`OandaRest`] (never the exec transport's). `ReconClient`'s methods take `&self`; the underlying
/// `ureq::Agent` is `Send + Sync` and the only mutable state is the `fill_floor` watermark (an
/// [`AtomicU64`]), so no lock is needed. See the module doc for the per-symbol filter, the flat-row
/// synthesis, and the fill-floor / `since`-no-op rationale.
pub struct OandaReconClient {
    rest: OandaRest,
    rest_base: String,
    account_id: String,
    /// Canonical vike symbol (e.g. `EURUSD`) stamped on every report row.
    symbol: String,
    /// OANDA instrument form (e.g. `EUR_USD`) the account-wide rows are filtered to.
    instrument: String,
    /// Account home currency (e.g. `USD`), stamped as each fill's `commission_asset`.
    home_currency: String,
    /// Highest transaction id consumed by a fill fetch — the `sinceid` floor, advanced each pass.
    fill_floor: AtomicU64,
}

impl OandaReconClient {
    /// Open a DEDICATED reconcile client: build a fresh [`OandaRest`] and probe
    /// `GET /v3/accounts/{id}/summary` — which validates the token+account (an `Err` leaves this
    /// venue-symbol unwired, exec unaffected, the same graceful degradation every absent-recon path
    /// uses) AND captures the account `currency` (fills' `commission_asset`) and current
    /// `lastTransactionID` (the initial fill floor). Never shares the exec side's transport.
    pub fn connect(config: &OandaConfig, symbol: &str) -> Result<OandaReconClient, OandaApiError> {
        let rest = OandaRest::new(config.api_token.clone());
        let path = format!("/v3/accounts/{}/summary", config.account_id);
        let v = rest.get(&config.rest_base, &path, "")?;
        let account = v.get("account");
        let home_currency = account
            .and_then(|a| a.get("currency"))
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let last_txn = account
            .and_then(|a| a.get("lastTransactionID"))
            .or_else(|| v.get("lastTransactionID"))
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(OandaReconClient {
            rest,
            rest_base: config.rest_base.clone(),
            account_id: config.account_id.clone(),
            symbol: symbol.to_string(),
            instrument: to_oanda_instrument(symbol),
            home_currency,
            fill_floor: AtomicU64::new(last_txn),
        })
    }
}

impl ReconClient for OandaReconClient {
    /// `_since` is a no-op: `/orders?state=PENDING` reports only the CURRENTLY resting set (no time
    /// filter) — a closed order that fell out of it is what the fill/position reports reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let path = format!("/v3/accounts/{}/orders", self.account_id);
        let query = format!("instrument={}&state=PENDING&count=500", self.instrument);
        let v = self.rest.get(&self.rest_base, &path, &query).map_err(|e| e.to_string())?;
        Ok(parse_order_reports(&v, &self.instrument, &self.symbol))
    }

    /// `_since` is a no-op: the window is the `fill_floor` transaction-id watermark, not the caller's
    /// epoch-ms `since` (see the module doc). Fetches `sinceid` from the floor, advances the floor
    /// past the returned batch, and maps the `ORDER_FILL` rows.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let floor = self.fill_floor.load(Ordering::Relaxed);
        let path = format!("/v3/accounts/{}/transactions/sinceid", self.account_id);
        let query = format!("id={floor}&type=ORDER_FILL");
        let v = self.rest.get(&self.rest_base, &path, &query).map_err(|e| e.to_string())?;
        // Advance the floor past this batch (reusing the crate's own watermark reader), so the next
        // pass requests only newer transactions and each request stays bounded.
        if let Some(max) = crate::history::max_transaction_id(&v) {
            self.fill_floor.fetch_max(max, Ordering::Relaxed);
        }
        Ok(parse_fill_reports(&v, &self.instrument, &self.symbol, &self.home_currency))
    }

    /// Account-wide `/positions` filtered to this instrument. OANDA OMITS an instrument the account
    /// never traded, so an empty match synthesizes a FLAT `qty == 0` row — load-bearing for
    /// `recon::diff` to detect a stale LOCAL position the venue has since closed (a flat row that IS
    /// present — a closed position OANDA still lists — already folds to net `0`, same result).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let path = format!("/v3/accounts/{}/positions", self.account_id);
        let v = self.rest.get(&self.rest_base, &path, "").map_err(|e| e.to_string())?;
        let matched = parse_position_reports(&v, &self.instrument, &self.symbol);
        if matched.is_empty() {
            return Ok(vec![flat_position(&self.symbol)]);
        }
        Ok(matched)
    }

    /// The account `/summary`'s home-currency `balance` — the realized cash truth `diff_balance`
    /// reconciles against.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let path = format!("/v3/accounts/{}/summary", self.account_id);
        let v = self.rest.get(&self.rest_base, &path, "").map_err(|e| e.to_string())?;
        Ok(parse_balance(&v))
    }
}

// --- the factory ---------------------------------------------------------------------------------

/// The venue → `ReconClient` factory (the ReconFactory seam) for OANDA: opens
/// [`OandaReconClient`]'s own dedicated reconcile transport from an already-resolved [`OandaConfig`].
/// `None` on any connect failure (unreachable / bad token / bad account id) — reconcile stays
/// unwired for this venue-symbol, exec unaffected.
///
/// **WIRED into `vike_mount::make_engine`** — its `("oanda", _)` arm calls this through
/// `recon_if_enabled`, so the dedicated transport and its blocking `/summary` probe are built only
/// when reconciliation is armed and never on an unset `VIKE_RECONCILE`. ⚠ This line used to say
/// "not yet wired", which it had stopped being: the header of a factory is the WRONG place to read
/// that from, because a mount arm can adopt a factory without touching it. Read the arm.
/// `crates/bridges/ig/CLAUDE.md` carries the family rule — every venue enrolled in the reconcile
/// roster after its factory was written inherited the same stale sentence.
pub fn recon_client(config: &OandaConfig, symbol: &str) -> Option<Box<dyn ReconClient>> {
    match OandaReconClient::connect(config, symbol) {
        Ok(c) => Some(Box::new(c) as Box<dyn ReconClient>),
        Err(e) => {
            tracing::warn!(target: "vike_oanda::recon", symbol, error = %e, "OANDA recon client unwired");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Inline sanity checks for the tiny helpers; the exhaustive body-parsing coverage (synthetic
    // JSON per endpoint) lives in `tests/oanda_reconcile_parse.rs`, mirroring the ctrader recon
    // parse tests.

    #[test]
    fn side_of_reads_the_units_sign() {
        assert_eq!(side_of(1000.0), 1);
        assert_eq!(side_of(-1000.0), -1);
        assert_eq!(side_of(0.0), 1); // flat/zero → the conservative buy default, never panics
    }

    #[test]
    fn order_state_normalizes() {
        assert_eq!(normalize_order_state("PENDING"), "ACCEPTED");
        assert_eq!(normalize_order_state("TRIGGERED"), "ACCEPTED");
        assert_eq!(normalize_order_state("FILLED"), "FILLED");
        assert_eq!(normalize_order_state("CANCELLED"), "CANCELED");
        assert_eq!(normalize_order_state("whatever"), "ACCEPTED");
    }

    #[test]
    fn ts_ms_converts_unix_seconds_string_to_millis() {
        let v = serde_json::json!({ "time": "1478012400.000000000" });
        assert_eq!(ts_ms(&v, "time"), 1_478_012_400_000);
        assert_eq!(ts_ms(&serde_json::json!({}), "time"), 0); // absent → 0
    }

    #[test]
    fn balance_reads_account_balance_as_string() {
        let v = serde_json::json!({ "account": { "balance": "100000.5000" } });
        assert_eq!(parse_balance(&v), Some(100000.5));
        assert_eq!(parse_balance(&serde_json::json!({ "account": {} })), None);
    }
}
