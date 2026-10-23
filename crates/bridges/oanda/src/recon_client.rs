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
//!   INCLUDING flat rows), net-folded and filtered to the engine instrument.
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
//! ## Admitted symbols (any-symbol, decision 0173)
//! The client is built on the MOUNTED symbol, and [`ReconClient::admit_symbol`] tells it each symbol the
//! account's engine was admitted on demand. Every pass then reads the mounted instrument FIRST and each
//! admitted one after it: the orders read once per instrument with the request it always made
//! (`instrument=` and `state=PENDING`; no unfiltered read, whose 500-row cap could cut an instrument's
//! orders off silently), and the fills and positions, which are account-wide on the wire, parsed once per
//! instrument out of the one body. ANY failed read fails the whole fetch: the restore planner reads an
//! order a successful report lacks as gone. Two spellings of one instrument (`GBPUSD` and `GBP_USD`) are
//! read once, under the first one told. With nothing admitted a pass makes exactly the requests it made
//! before. `AdmittedSymbols` caps the list at the engine's own, so a pass costs at most that many extra
//! order reads.
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
//! ⚠ The floor is ACCOUNT-wide, so a pass advances it over the fills of instruments the client does not
//! read (yet). An instrument admitted after that (a pass that ran before the admission reached this client,
//! or a client rebuilt and re-taught its admitted list after its first passes) would never report the
//! fills the floor already passed. So the client remembers which instruments it has read from the floor
//! (`fills_read`) and reads a newly admitted one ONCE from the floor it started with (`start_floor`):
//! one more `sinceid` GET, parsed for that instrument alone, and the floor moves only when every read of
//! the fetch succeeded. Fills before the client started stay the position report's job, as above.
//!
//! ## No live fee lane
//! OANDA's cost is spread-based (standard accounts are commission-free; core-pricing commission is
//! reported per-fill, already carried on the fill report), and there is no per-account maker/taker
//! rate endpoint — so `fetch_fee_rates` stays the trait default (`Ok(None)`), and the caller keeps
//! the static [`vike_venues::fee_schedule_for`] default (fail-soft).
//!
//! Every wire→report mapping is a PURE free function (`parse_*`) over the already-parsed
//! `serde_json::Value` (the crate convention — cf. [`crate::data::parse_candles`]), fixture-tested
//! offline in `tests/offline/oanda_reconcile_parse.rs` (synthetic bodies — no network). The `#[ignore]`d,
//! cred-gated `tests/oanda_reconcile_smoke.rs` proves the fetch+parse read-only against a live
//! fxPractice account.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde_json::Value;

use vike_bridge_core::json::{get_f64, json_num};
use vike_catalog::to_oanda_instrument;
use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::config::OandaConfig;
use crate::rest::{OandaApiError, OandaRest};

/// The venue key stamped on every report row.
pub const VENUE: &str = "oanda";

// --- pure field helpers (OANDA sends every scalar as a STRING under `Accept-Datetime-Format: UNIX`) -

/// A present string field, `""` when absent/non-string (never panics).
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
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
    if units < 0.0 { -1 } else { 1 }
}

// --- pure parsers (over the already-parsed v20 JSON bodies — the fixture-tested units) -----------

/// Map an OANDA order `state` onto the FSM [`OrderStatus`] `recon::diff` reads — every state v20
/// documents, explicitly: `PENDING` and `TRIGGERED` → `Accepted` (both live); `FILLED` → `Filled`;
/// `CANCELLED` → `Canceled`. OANDA carries no partial-fill state on the *order* object (the filled
/// portion becomes a separate Trade), so there is no `PartiallyFilled` derive here.
///
/// A word outside that list is WARNED and read as `Accepted` (a `/orders?state=PENDING` fetch only
/// returns resting orders): live, the conservative direction — `diff` raises no `MissingTerminal` on
/// it, adoption synthesizes no cumulative fill, and the restore planner keeps the order rather than
/// declaring it gone.
pub fn normalize_order_state(state: &str) -> OrderStatus {
    match state {
        "PENDING" | "TRIGGERED" => OrderStatus::Accepted,
        "FILLED" => OrderStatus::Filled,
        "CANCELLED" => OrderStatus::Canceled,
        other => {
            tracing::warn!(
                venue = VENUE,
                state = other,
                "unknown order state word — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
    }
}

/// Map an OANDA order `type` onto [`OrderType`]: `MARKET`, `LIMIT`, `STOP` and `TAKE_PROFIT` are
/// their twins. Every other kind (`MARKET_IF_TOUCHED`, `STOP_LOSS`, `GUARANTEED_STOP_LOSS`,
/// `TRAILING_STOP_LOSS`, `FIXED_PRICE`) has none and stays [`OrderType::Unrecognized`] lower-cased,
/// as every report spelled it before the type existed.
fn normalize_order_type(raw: &str) -> OrderType {
    match raw {
        "MARKET" => OrderType::Market,
        "LIMIT" => OrderType::Limit,
        "STOP" => OrderType::Stop,
        "TAKE_PROFIT" => OrderType::TakeProfit,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// `{"orders":[Order,..]}` → [`OrderStatusReport`], filtered to `instrument` and stamped with the
/// canonical `symbol`. `id` → `venue_order_id`; `clientExtensions.id` → `client_order_id`; `units`
/// (signed) → side/qty; `type` → `order_type` (`normalize_order_type`); `state` normalized;
/// `createTime` → ts. `filled_qty`/`avg_px` are `0.0` — a resting OANDA order carries no fill
/// progress (the filled portion is a separate Trade). Instrument-less exit orders (TAKE_PROFIT/STOP_LOSS/… reference a
/// `tradeID`, not an instrument) never match and are dropped — reconcile tracks entry orders.
pub fn parse_order_reports(v: &Value, instrument: &str, symbol: &str) -> Vec<OrderStatusReport> {
    let Some(orders) = v.get("orders").and_then(|o| o.as_array()) else {
        return Vec::new();
    };
    orders
        .iter()
        .filter(|o| s(o, "instrument") == instrument)
        .map(|o| {
            let units = get_f64(o, "units");
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                venue_order_id: s(o, "id").into(),
                client_order_id: client_order_id(o),
                side: side_of(units),
                order_type: normalize_order_type(s(o, "type")),
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
            let long_units = long.map(|l| get_f64(l, "units")).unwrap_or(0.0);
            let short_units = short.map(|sh| get_f64(sh, "units")).unwrap_or(0.0); // already ≤ 0
            let net = long_units + short_units;
            let avg_px = if net > 0.0 {
                long.map(|l| get_f64(l, "averagePrice")).unwrap_or(0.0)
            } else if net < 0.0 {
                short.map(|sh| get_f64(sh, "averagePrice")).unwrap_or(0.0)
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
            let units = get_f64(t, "units");
            Some(FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                trade_id,
                venue_order_id: s(t, "orderID").into(),
                client_order_id: client_order_id(t),
                side: side_of(units),
                last_qty: units.abs(),
                last_px: get_f64(t, "price"),
                commission: get_f64(t, "commission"),
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
    /// The floor this client started with (the account's `lastTransactionID` at connect): the oldest id a
    /// newly admitted instrument's fills are re-read from (the module doc's "backfill").
    start_floor: u64,
    /// The OANDA instruments whose fills a pass has read from the floor it advanced over: the mounted one
    /// from birth, an admitted one once its backfill (or its first pass, when the floor had not moved yet)
    /// succeeded. An instrument not in it has fills the floor may already have passed.
    fills_read: Mutex<HashSet<String>>,
    /// The symbols the account's engine admitted on demand (any-symbol, decision 0173), in the engine's
    /// spelling: every pass reads them beside the mounted one (the module doc).
    admitted: AdmittedSymbols,
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
            start_floor: last_txn,
            fills_read: Mutex::new(HashSet::from([to_oanda_instrument(symbol)])),
            admitted: AdmittedSymbols::default(),
        })
    }

    /// `GET /transactions/sinceid` for the `ORDER_FILL`s after transaction `id` (account-wide).
    fn fills_after(&self, id: u64) -> Result<Value, String> {
        let path = format!("/v3/accounts/{}/transactions/sinceid", self.account_id);
        let query = format!("id={id}&type=ORDER_FILL");
        self.rest.get(&self.rest_base, &path, &query).map_err(|e| e.to_string())
    }

    /// The `(OANDA instrument, engine symbol)` pairs a pass reads: the mounted symbol first, then each
    /// admitted one. A symbol whose INSTRUMENT is already in the list is skipped (`GBPUSD` and `GBP_USD`
    /// are one instrument at the venue), so no report row is produced twice.
    fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs = vec![(self.instrument.clone(), self.symbol.clone())];
        for symbol in self.admitted.symbols() {
            let instrument = to_oanda_instrument(&symbol);
            if !pairs.iter().any(|(i, _)| *i == instrument) {
                pairs.push((instrument, symbol));
            }
        }
        pairs
    }
}

impl ReconClient for OandaReconClient {
    /// `_since` is a no-op: `/orders?state=PENDING` reports only the CURRENTLY resting set (no time
    /// filter) — a closed order that fell out of it is what the fill/position reports reconcile. One
    /// read per instrument (the mounted one, then each admitted one), the first failure fails the fetch.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let path = format!("/v3/accounts/{}/orders", self.account_id);
        let mut reports = Vec::new();
        for (instrument, symbol) in &self.pairs() {
            let query = format!("instrument={instrument}&state=PENDING&count=500");
            let v = self.rest.get(&self.rest_base, &path, &query).map_err(|e| e.to_string())?;
            reports.extend(parse_order_reports(&v, instrument, symbol));
        }
        Ok(reports)
    }

    /// `_since` is a no-op: the window is the `fill_floor` transaction-id watermark, not the caller's
    /// epoch-ms `since` (see the module doc). Fetches `sinceid` from the floor and maps the `ORDER_FILL`
    /// rows of every instrument the pass reads out of that ONE body; an instrument admitted after the floor
    /// had moved is read from the floor the client started with, in one more read. Advances the floor past
    /// the returned batch only once every read succeeded.
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        let floor = self.fill_floor.load(Ordering::Relaxed);
        let pairs = self.pairs();
        let v = self.fills_after(floor)?;
        // An instrument admitted after the floor had moved: the floor is account-wide, so earlier passes
        // advanced over its fills unread. Its fills come from ONE read from the floor this client started
        // with (the others' rows are not taken from it: they were reported already), and a failed read fails
        // the fetch before the floor moves, so nothing is lost and the next pass asks again. While the floor
        // has not moved, the normal read already starts where the backfill would.
        let unread: Vec<&str> = {
            let read = self.fills_read.lock().unwrap_or_else(PoisonError::into_inner);
            pairs.iter().map(|(i, _)| i.as_str()).filter(|i| !read.contains(*i)).collect()
        };
        let backfill = if !unread.is_empty() && floor > self.start_floor {
            Some(self.fills_after(self.start_floor)?)
        } else {
            None
        };
        let mut reports = Vec::new();
        for (instrument, symbol) in &pairs {
            let body = match &backfill {
                Some(old) if unread.contains(&instrument.as_str()) => old,
                _ => &v,
            };
            reports.extend(parse_fill_reports(body, instrument, symbol, &self.home_currency));
        }
        // Every read succeeded: advance the floor past this batch (reusing the crate's own watermark
        // reader), so the next pass requests only newer transactions and each request stays bounded, and
        // remember the instruments this pass read from the floor.
        if let Some(max) = crate::history::max_transaction_id(&v) {
            self.fill_floor.fetch_max(max, Ordering::Relaxed);
        }
        self.fills_read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(unread.into_iter().map(str::to_string));
        Ok(reports)
    }

    /// Account-wide `/positions` filtered to each instrument the pass reads. OANDA OMITS an instrument
    /// the account never traded, so an empty match synthesizes a FLAT `qty == 0` row — load-bearing for
    /// `recon::diff` to detect a stale LOCAL position the venue has since closed (a flat row that IS
    /// present — a closed position OANDA still lists — already folds to net `0`, same result).
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let path = format!("/v3/accounts/{}/positions", self.account_id);
        let v = self.rest.get(&self.rest_base, &path, "").map_err(|e| e.to_string())?;
        let mut reports = Vec::new();
        for (instrument, symbol) in &self.pairs() {
            let matched = parse_position_reports(&v, instrument, symbol);
            if matched.is_empty() {
                reports.push(flat_position(symbol));
            } else {
                reports.extend(matched);
            }
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (any-symbol, decision 0173): every later pass reads
    /// its orders, fills and position too. Idempotent, never fetches; the list is bounded and keeps the
    /// engine's spelling (upper-cased).
    fn admit_symbol(&self, symbol: &str) {
        if self.admitted.admit(symbol, &self.symbol) {
            tracing::info!(
                venue = VENUE, symbol = %symbol.to_ascii_uppercase(),
                "reconcile: the engine admitted a symbol; every pass now reads it too"
            );
        }
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        Some(self.admitted.symbols())
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
/// **WIRED into this crate's start** — [`crate::adapter::OandaVenueAdapter`]'s `start` calls this
/// through `recon_if_enabled`, so the dedicated transport and its blocking `/summary` probe are
/// built only when reconciliation is armed and never on a start whose reconcile gate answered no.
/// (The caller was `vike_tradehub_core::make_engine`'s oanda arm until the venue-adapter contract moved the
/// arm into this crate.) ⚠ This line used to say "not yet wired", which it had stopped being: the
/// header of a factory is the WRONG place to read that from, because a start can adopt a factory
/// without touching it. Read the start.
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
    // JSON per endpoint) lives in `tests/offline/oanda_reconcile_parse.rs`, mirroring the ctrader recon
    // parse tests.

    #[test]
    fn side_of_reads_the_units_sign() {
        assert_eq!(side_of(1000.0), 1);
        assert_eq!(side_of(-1000.0), -1);
        assert_eq!(side_of(0.0), 1); // flat/zero → the conservative buy default, never panics
    }

    #[test]
    fn order_state_normalizes() {
        assert_eq!(normalize_order_state("PENDING"), OrderStatus::Accepted);
        assert_eq!(normalize_order_state("TRIGGERED"), OrderStatus::Accepted);
        assert_eq!(normalize_order_state("FILLED"), OrderStatus::Filled);
        assert_eq!(normalize_order_state("CANCELLED"), OrderStatus::Canceled);
        assert_eq!(normalize_order_state("whatever"), OrderStatus::Accepted);
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
