//! cTrader `ReconClient` (ReconFactory seam, wave-2 task 6) — the venue-facing report seam
//! (`vike_exec::recon::ReconClient`) over a DEDICATED authed connection [`CtraderReconClient::connect`]
//! opens via the SAME handshake [`crate::conn::open_and_handshake`] runs for the exec/data actor
//! (mirrors Deribit's own dedicated-socket `ReconClient`: a reconcile fetch never contends the
//! exec side's ONE multiplexed socket, and a resync burst here can never delay an order write).
//!
//! Reconciles against:
//!   `ProtoOAReconcileReq`/`Res` (order + position reports) — the SAME reconcile payload
//!   [`crate::conn`]'s `reconcile_after_reconnect` already decodes on every reconnect (F3); this
//!   client just exposes it through the trait instead of only the reconnect-recovery inline path.
//!   Neither report carries a per-symbol filter on the wire (`ProtoOAReconcileReq` has none), so
//!   both are filtered client-side to the mounted symbol; [`parse_positions`]' caller synthesizes
//!   a flat row when the symbol is absent — cTrader (like OKX/Alpaca) omits a CLOSED position
//!   entirely rather than sending a zero-volume row, so a stale local position stays detectable.
//!   `ProtoOADealListReq`/`Res` (fill reports — historical deals by timestamp range; `since` maps
//!   directly to `from_timestamp`, both already epoch-ms, no unit conversion needed, unlike
//!   Alpaca's RFC3339 gap).
//!
//! **Known gap**: a `ProtoOADeal` carries no `client_order_id`/`label` field at all (only
//! `order_id`), so every fill report's `client_order_id` is `None` — the same "externally-placed
//! order" convention every other venue's parser already uses for an absent client id.
//!
//! **Known simplification**: `fetch_order_status_reports` and `fetch_position_status_reports`
//! each issue their OWN `ProtoOAReconcileReq` round trip even though both reports come off the
//! SAME response — no cross-method caching (the `ReconClient` trait gives each fetch no way to
//! share state with its siblings within one pass). A rare periodic/on-reconnect sync doubling one
//! cheap request is an acceptable trade-off; revisit if this becomes a real polling hot path.
//!
//! **No live fee-rate lane / no balance**: cTrader's Open API exposes no per-account maker/taker
//! rate over this protocol, and the account balance (`ProtoOATrader.balance`) needs a THIRD
//! request/response pair this client doesn't otherwise make — deferred; `fetch_balance`/
//! `fetch_fee_rates` stay the trait defaults (`Ok(None)`).
//!
//! **Idle connection (was the MUST-FIX-BEFORE-MOUNT gate).** This socket sends no heartbeats —
//! `HEARTBEAT_INTERVAL` (10s, see [`crate::conn`]) is kept only by the exec actor's own loop — and
//! it sits idle between reconcile passes, while `conn`'s module doc records that cTrader
//! disconnects idle sockets. At the default `VIKE_RECONCILE_INTERVAL_MS` (60s) cadence it would
//! therefore be server-closed before the second pass, failing every later fetch. Resolved by
//! LAZY REVIVE rather than a keepalive: a fetch that finds the connection idle longer than
//! `IDLE_REVIVE_AFTER` re-runs the handshake before its request. See
//! [`CtraderReconClient`]'s own doc for why that beats a heartbeat thread and why it costs no more
//! than a per-pass reconnect. Proven by `tests/recon_client_revive.rs` against a fake server that
//! closes after every pass — no live venue, no real idle wait.
//!
//! Every wire->report mapping delegates to a PURE free function (`parse_*`) over the
//! ALREADY-DECODED prost types (cTrader is protobuf, not REST/JSON) — fixture-tested
//! (`tests/offline/recon_client_parse.rs`, constructs the prost structs directly, no network).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use prost::Message;

use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{FillReport, MarginMode, OrderStatusReport, PositionStatusReport};

use crate::conn::{self, ConnConfig, Reader};
use crate::proto::{
    pt, ProtoOaDeal, ProtoOaDealListReq, ProtoOaDealListRes, ProtoOaDealStatus, ProtoOaOrder,
    ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaPosition, ProtoOaReconcileReq,
    ProtoOaReconcileRes, ProtoOaTradeSide,
};
use crate::symbols::SymbolMap;

const VENUE: &str = "ctrader";

// --- pure parsers (over already-decoded prost types, not raw wire bytes) ------------------------

/// `tradeSide` -> signed side: `SELL` -> `-1`, anything else (`BUY`) -> `+1`. Matches
/// `event_mapper::deal_fill`'s own `deal.trade_side == ProtoOaTradeSide::Sell as i32` check.
fn side_sign(trade_side: i32) -> i32 {
    if trade_side == ProtoOaTradeSide::Sell as i32 {
        -1
    } else {
        1
    }
}

/// The generated enum's own wire name (`"MARKET"`, `"LIMIT"`, …), lower-cased to this crate's
/// report convention. An unrecognized/absent raw value folds to `""` — never panics.
fn order_type_str(order_type: i32) -> String {
    ProtoOaOrderType::try_from(order_type)
        .map(|t| t.as_str_name().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Normalize a `ProtoOAOrder` to the `OrderStatus::parse` FSM vocabulary. `ORDER_STATUS_ACCEPTED`
/// (the only non-terminal status) is further split by fill progress — `executedVolume` between
/// `0` and the order's `volume` -> `PARTIALLY_FILLED`, matching Hyperliquid's/Bybit's own
/// derive-from-fill-progress convention for a venue whose resting-order status has no distinct
/// "partially filled" value.
fn order_status_str(order: &ProtoOaOrder) -> String {
    match ProtoOaOrderStatus::try_from(order.order_status) {
        Ok(ProtoOaOrderStatus::OrderStatusAccepted) => {
            let filled = order.executed_volume.unwrap_or(0);
            if filled > 0 && filled < order.trade_data.volume {
                "PARTIALLY_FILLED".to_string()
            } else {
                "ACCEPTED".to_string()
            }
        }
        Ok(ProtoOaOrderStatus::OrderStatusFilled) => "FILLED".to_string(),
        Ok(ProtoOaOrderStatus::OrderStatusRejected) => "REJECTED".to_string(),
        Ok(ProtoOaOrderStatus::OrderStatusExpired) => "EXPIRED".to_string(),
        Ok(ProtoOaOrderStatus::OrderStatusCancelled) => "CANCELED".to_string(),
        Err(_) => "ACCEPTED".to_string(),
    }
}

/// `ProtoOAReconcileRes.order` rows -> `OrderStatusReport`, resolving each row's numeric
/// `symbolId` to the vike symbol name via `symbols`. `client_order_id` prefers the dedicated
/// `clientOrderId` field, falling back to `tradeData.label` — the SAME precedence
/// `conn::reconcile_order_coid` already uses to rebuild the coid->orderId map on reconnect.
/// `volume`/`executedVolume` are in CENTS (divide by 100 for real units, per the wire doc).
pub fn parse_orders(orders: &[ProtoOaOrder], symbols: &SymbolMap) -> Vec<OrderStatusReport> {
    orders
        .iter()
        .map(|o| {
            let client_order_id = o
                .client_order_id
                .clone()
                .or_else(|| o.trade_data.label.clone())
                .filter(|c| !c.is_empty());
            OrderStatusReport {
                venue: VENUE.to_string(),
                symbol: symbols.name_of(o.trade_data.symbol_id).unwrap_or_default().to_string(),
                venue_order_id: o.order_id.to_string().into(),
                client_order_id,
                side: side_sign(o.trade_data.trade_side),
                order_type: order_type_str(o.order_type),
                qty: o.trade_data.volume as f64 / 100.0,
                filled_qty: o.executed_volume.unwrap_or(0) as f64 / 100.0,
                avg_px: o.execution_price.unwrap_or(0.0),
                status: order_status_str(o),
                ts: o.utc_last_update_timestamp.unwrap_or(0),
            }
        })
        .collect()
}

/// `ProtoOAReconcileRes.position` rows -> `PositionStatusReport`. `tradeData.volume` is UNSIGNED
/// (in cents) — the sign comes from `tradeData.tradeSide` (`SELL` -> short/negative), the same
/// unsigned-magnitude-plus-side convention Bybit's/Alpaca's parsers already use. `price` is the
/// position's VWAP entry price (already a real double, no rescale).
pub fn parse_positions(
    positions: &[ProtoOaPosition],
    symbols: &SymbolMap,
) -> Vec<PositionStatusReport> {
    positions
        .iter()
        .map(|p| {
            let is_short = p.trade_data.trade_side == ProtoOaTradeSide::Sell as i32;
            let unsigned_qty = p.trade_data.volume as f64 / 100.0;
            PositionStatusReport {
                venue: VENUE.to_string(),
                symbol: symbols.name_of(p.trade_data.symbol_id).unwrap_or_default().to_string(),
                position_side: if is_short { PositionSide::Short } else { PositionSide::Long },
                qty: if is_short { -unsigned_qty } else { unsigned_qty },
                avg_px: p.price.unwrap_or(0.0),
                ts: p.utc_last_update_timestamp.unwrap_or(0),
                margin_mode: MarginMode::default(),
                isolated_margin: None,
                delta: None,
            }
        })
        .collect()
}

/// One RAW open position from a reconcile, INCLUDING the numeric `position_id` that
/// [`parse_positions`]'s `PositionStatusReport` output drops (it has no position-id field). This is
/// the input [`crate::exec::CtraderExec::close_all`] needs to issue a `ProtoOAClosePositionReq` per
/// position — the flatten path for PRE-EXISTING positions this session never tracked (a fresh
/// connect's exec position-map is empty, so a reduce ORDER would OPEN a hedge against it instead of
/// closing). `side` is `+1` long / `-1` short; `volume` is centi-units (the wire scale).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawPosition {
    pub position_id: i64,
    pub symbol: String,
    pub side: i32,
    pub volume: i64,
}

/// `ProtoOAReconcileRes.position` rows -> [`RawPosition`], resolving each row's `symbolId` to its
/// vike name and — unlike [`parse_positions`] — KEEPING the `position_id`. Only rows with positive
/// `volume` are kept: cTrader's reconcile lists the currently-OPEN set (it omits a closed position
/// entirely), and a zero-volume `CREATED` placeholder (the empty position a pending order makes)
/// carries no exposure to flatten. Pure over already-decoded prost types, like every other `parse_*`
/// here.
pub fn parse_raw_positions(positions: &[ProtoOaPosition], symbols: &SymbolMap) -> Vec<RawPosition> {
    positions
        .iter()
        .filter(|p| p.trade_data.volume > 0)
        .map(|p| {
            let is_short = p.trade_data.trade_side == ProtoOaTradeSide::Sell as i32;
            RawPosition {
                position_id: p.position_id,
                symbol: symbols.name_of(p.trade_data.symbol_id).unwrap_or_default().to_string(),
                side: if is_short { -1 } else { 1 },
                volume: p.trade_data.volume,
            }
        })
        .collect()
}

/// `ProtoOADealListRes.deal` rows -> `FillReport`, filtered to `symbol_id` (the request carries no
/// per-symbol filter) and to executed deals only (`FILLED`/`PARTIALLY_FILLED` — a `REJECTED`/
/// `INTERNALLY_REJECTED`/`ERROR` deal never actually executed volume). Commission descales by
/// `moneyDigits` (preferring the deal's own, falling back to the account-level `money_digits`
/// passed in) and is NEGATED — cTrader's wire commission is negative for a charged fee, the
/// opposite of this report's `> 0 == cost` convention — the SAME descale+negate
/// `event_mapper::deal_fill` already applies to the live fill stream. `client_order_id` is always
/// `None` (see the module doc); `commission_asset` is always `""` (the deal itself carries no
/// currency field) and `liquidity_side` is always `Unknown` (no maker/taker flag on the wire).
pub fn parse_fills(
    deals: &[ProtoOaDeal],
    symbol_id: i64,
    symbol: &str,
    money_digits: u32,
) -> Vec<FillReport> {
    deals
        .iter()
        .filter(|d| d.symbol_id == symbol_id)
        .filter(|d| {
            matches!(
                ProtoOaDealStatus::try_from(d.deal_status),
                Ok(ProtoOaDealStatus::Filled) | Ok(ProtoOaDealStatus::PartiallyFilled)
            )
        })
        .map(|d| {
            let digits = d.money_digits.unwrap_or(money_digits) as i32;
            let commission = -(d.commission.unwrap_or(0) as f64) / 10f64.powi(digits);
            FillReport {
                venue: VENUE.to_string(),
                symbol: symbol.to_string(),
                // Integer wire field — cannot render empty (see `crate::event_mapper`'s
                // `deal_to_fill` for the same reasoning and the prost `0`-default residual).
                trade_id: TradeId::new(d.deal_id.to_string())
                    .expect("a protobuf i64 deal_id always renders at least one digit"),
                venue_order_id: d.order_id.to_string().into(),
                client_order_id: None,
                side: side_sign(d.trade_side),
                last_qty: d.filled_volume as f64 / 100.0,
                last_px: d.execution_price.unwrap_or(0.0),
                commission,
                commission_asset: String::new(),
                liquidity_side: LiquiditySide::Unknown,
                ts: d.execution_timestamp,
            }
        })
        .collect()
}

// --- the client ----------------------------------------------------------------------------

/// Re-handshake the dedicated connection once it has been idle this long. MUST stay below
/// cTrader's server-side idle-disconnect tolerance; [`conn::HEARTBEAT_INTERVAL`] (10s) is the
/// cadence the exec actor keeps its own socket alive at, so reviving at the same age is
/// conservative by construction — this connection is re-opened no later than the point the actor
/// would have had to send a keepalive to hold it.
const IDLE_REVIVE_AFTER: Duration = conn::HEARTBEAT_INTERVAL;

/// The live connection + everything the handshake resolved with it. Grouped under ONE mutex (not
/// a bare `Mutex<Reader>`) because an idle revive replaces the socket AND re-resolves the symbol
/// map/ctid/moneyDigits together — they must never be observed from different handshakes.
struct ConnState {
    reader: Reader,
    ctid: i64,
    symbols: SymbolMap,
    money_digits: u32,
    symbol_id: i64,
    /// When the last successful exchange completed — the idle clock [`IDLE_REVIVE_AFTER`] reads.
    last_used: Instant,
}

/// One reconcile client per (account, venue-symbol), holding its OWN dedicated authed connection
/// — see the module doc for why it doesn't share the exec/data actor's socket. The mutex is
/// because `ReconClient`'s methods take `&self` but reading/writing the socket needs `&mut`.
///
/// **Idle handling (the MUST-FIX-BEFORE-MOUNT the module doc flagged):** this socket sends no
/// heartbeats and sits idle between reconcile passes, and cTrader disconnects idle sockets — so at
/// the default 60s `ReconDriver` cadence it would be server-closed before the second pass, failing
/// every later fetch. Resolved by REVIVING the connection lazily rather than keeping it warm: a
/// fetch that finds the connection older than [`IDLE_REVIVE_AFTER`] re-runs the handshake first.
///
/// Chosen over a heartbeat thread deliberately. A keepalive writer would have to share this same
/// mutex with an in-flight `call`, whose `read_until` can legitimately block for the full
/// `HANDSHAKE_TIMEOUT` (15s) — longer than the 10s cadence it would need to keep, so it could not
/// honor its own interval without interleaving a write into a half-read exchange. Lazy revive
/// needs no thread, no shared clock, and nothing to stop on drop. It also costs no more than a
/// per-pass reconnect would: the three `fetch_*` calls of one pass run microseconds apart, so only
/// the FIRST revives and its siblings reuse that fresh connection.
pub struct CtraderReconClient {
    state: Mutex<ConnState>,
    cfg: ConnConfig,
    symbol: String,
    idle_revive_after: Duration,
}

impl CtraderReconClient {
    /// Open a DEDICATED authed connection for reconcile reads: the SAME handshake
    /// [`conn::open_and_handshake`] runs for exec/data (`cfg.account_id`, when set, skips account
    /// discovery — the same "never re-discover, always pass the resolved ctid" convention
    /// reconnect uses). `None` on handshake failure OR when `symbol` isn't in the discovered
    /// symbol list — reconcile stays unwired for this venue, exec unaffected (the same graceful
    /// degradation every other absent-recon path uses).
    pub fn connect(cfg: &ConnConfig, symbol: &str) -> Option<Self> {
        Self::connect_with_idle_revive(cfg, symbol, IDLE_REVIVE_AFTER)
    }

    /// [`Self::connect`] with an explicit idle-revive age. Exposed so a test can force the revive
    /// path deterministically (`Duration::ZERO` revives on every call) without sleeping out a real
    /// idle interval, and so an operator can tighten the age for a venue that disconnects sooner.
    pub fn connect_with_idle_revive(
        cfg: &ConnConfig,
        symbol: &str,
        idle_revive_after: Duration,
    ) -> Option<Self> {
        let (reader, ctid, symbols, money_digits) =
            conn::open_and_handshake(cfg, cfg.account_id).ok()?;
        let symbol_id = symbols.id_of(symbol)?;
        Some(CtraderReconClient {
            state: Mutex::new(ConnState {
                reader,
                ctid,
                symbols,
                money_digits,
                symbol_id,
                last_used: Instant::now(),
            }),
            cfg: cfg.clone(),
            symbol: symbol.to_string(),
            idle_revive_after,
        })
    }

    /// Re-handshake `st` in place when it has gone idle past [`Self::idle_revive_after`]; a
    /// still-fresh connection is left untouched. The resolved `ctid` is passed back as the FIXED
    /// account id so a revive can never re-discover and silently authorize a DIFFERENT account —
    /// the same invariant `open_and_handshake`'s own doc states for reconnect.
    fn revive_if_idle(&self, st: &mut ConnState) -> Result<(), String> {
        if st.last_used.elapsed() < self.idle_revive_after {
            return Ok(());
        }
        let (reader, ctid, symbols, money_digits) =
            conn::open_and_handshake(&self.cfg, Some(st.ctid)).map_err(|e| e.to_string())?;
        let symbol_id = symbols
            .id_of(&self.symbol)
            .ok_or_else(|| format!("ctrader recon: symbol {} absent after revive", self.symbol))?;
        *st =
            ConnState { reader, ctid, symbols, money_digits, symbol_id, last_used: Instant::now() };
        Ok(())
    }

    /// Blocking request/response over the dedicated connection: revive it if it has gone idle,
    /// then encode+write `req` as `req_type` and read frames until `res_type` (or an
    /// `ERROR_RES`/timeout) arrives — the SAME `send`/`read_until` primitives
    /// `conn::reconcile_after_reconnect` uses, reused rather than reinvented.
    fn call<Req: Message, Res: Message + Default>(
        &self,
        st: &mut ConnState,
        req_type: u32,
        req: &Req,
        res_type: u32,
    ) -> Result<Res, String> {
        self.revive_if_idle(st)?;
        conn::send(&mut st.reader, req_type, req, "").map_err(|e| e.to_string())?;
        let msg = conn::read_until(&mut st.reader, res_type).map_err(|e| e.to_string())?;
        // Only a COMPLETED exchange refreshes the idle clock: a failed one leaves the connection
        // suspect, so the next fetch revives rather than retrying down the same dead socket.
        st.last_used = Instant::now();
        conn::decode_payload(&msg).map_err(|e| e.to_string())
    }

    /// Lock the connection for one fetch. Separated from [`Self::call`] so a fetch that needs both
    /// a response AND the handshake-resolved fields (`symbols`, `money_digits`, `symbol_id`) reads
    /// them from the SAME guard the call ran under — after a revive they are freshly resolved.
    fn locked(&self) -> Result<std::sync::MutexGuard<'_, ConnState>, String> {
        self.state.lock().map_err(|_| "ctrader recon connection poisoned".to_string())
    }

    fn reconcile(&self, st: &mut ConnState) -> Result<ProtoOaReconcileRes, String> {
        let req = ProtoOaReconcileReq { ctid_trader_account_id: st.ctid, ..Default::default() };
        self.call(st, pt::RECONCILE_REQ, &req, pt::RECONCILE_RES)
    }

    /// The account's currently-OPEN positions for this client's mounted symbol as RAW legs — each
    /// carrying the numeric `position_id` that [`ReconClient::fetch_position_status_reports`] drops.
    /// The input an operator flatten ([`crate::exec::CtraderExec::close_all`]) needs to close every
    /// PRE-EXISTING position by id (a reduce ORDER can't reach them — the exec position-map only
    /// tracks this session's own fills). Reconciles over the dedicated socket (idle-revived like
    /// every other fetch), filtered to `self.symbol`; `Ok(vec![])` when flat in this symbol.
    pub fn open_positions(&self) -> Result<Vec<RawPosition>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        Ok(parse_raw_positions(&res.position, &st.symbols)
            .into_iter()
            .filter(|p| p.symbol == self.symbol)
            .collect())
    }
}

impl ReconClient for CtraderReconClient {
    /// `_since` is a no-op here: `ProtoOAReconcileRes` reports only the CURRENTLY pending set
    /// (mirrors Bybit's `order/realtime`) — a fully-closed order that fell out of this set is
    /// what the fill/position reports (not this one) reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        Ok(parse_orders(&res.order, &st.symbols)
            .into_iter()
            .filter(|o| o.symbol == self.symbol)
            .collect())
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut st = self.locked()?;
        let req = ProtoOaDealListReq {
            ctid_trader_account_id: st.ctid,
            from_timestamp: (since > 0).then_some(since),
            ..Default::default()
        };
        let res: ProtoOaDealListRes =
            self.call(&mut st, pt::DEAL_LIST_REQ, &req, pt::DEAL_LIST_RES)?;
        Ok(parse_fills(&res.deal, st.symbol_id, &self.symbol, st.money_digits))
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        let matched: Vec<PositionStatusReport> = parse_positions(&res.position, &st.symbols)
            .into_iter()
            .filter(|p| p.symbol == self.symbol)
            .collect();
        if matched.is_empty() {
            return Ok(vec![PositionStatusReport::flat(VENUE, &self.symbol)]);
        }
        Ok(matched)
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6): opens
/// [`CtraderReconClient`]'s own dedicated authed connection from an already-resolved
/// [`crate::config::CtraderConfig`]. `None` on handshake failure — reconcile stays unwired for
/// this venue, exec unaffected (same as [`CtraderReconClient::connect`]'s doc).
///
/// **WIRED into `vike_mount::make_engine`** — its `("ctrader", _)` arm calls this through
/// `recon_if_enabled`, LAZILY, because this factory CONNECTS during construction: an unset
/// `VIKE_RECONCILE` must not pay a second protobuf/TLS handshake. `crates/vike-mount/src/startup.rs`
/// defers it the same way for the preflight credential probe. cTrader reconciles on the periodic
/// INTERVAL only — no `recon_trigger`.
///
/// ⚠ This line used to say "not yet wired ... cTrader is not a live-mounted venue today", which it
/// had stopped being. A factory's header is the WRONG place to read that from, because a mount arm
/// can adopt a factory without touching it — read the arm. `crates/bridges/ig/CLAUDE.md` carries
/// the family rule: every venue enrolled in the reconcile roster after its factory was written
/// inherited the same stale sentence.
pub fn recon_client(
    config: &crate::config::CtraderConfig,
    symbol: &str,
) -> Option<Box<dyn ReconClient>> {
    CtraderReconClient::connect(&config.to_conn_config(), symbol)
        .map(|c| Box::new(c) as Box<dyn ReconClient>)
}
