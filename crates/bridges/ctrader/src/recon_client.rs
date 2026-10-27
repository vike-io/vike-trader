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
//!   both are filtered client-side to the engine symbol; [`parse_positions`]' caller synthesizes
//!   a flat row when the symbol is absent — cTrader (like OKX/Alpaca) omits a CLOSED position
//!   entirely rather than sending a zero-volume row, so a stale local position stays detectable.
//!
//! ## Position report keys ([`net_positions`])
//! The venue lists one row per `positionId` (a HEDGED account can hold several per symbol, on both sides),
//! while the engine's book is ONE signed net per `(symbol, BOTH)`: every cTrader fill is published with the
//! default `position_side` (`event_mapper::deal_fill`), and `vike_exec::recon::diff` matches a report to the
//! book by `(symbol, position_side)` EXACTLY. A report spelled `Long`/`Short` therefore never matched, and an
//! open position, hedged or not, held an `OrphanLocalPosition` (the book's `BOTH` row, "no venue row") plus a
//! `PositionOnlyExternal` (the venue's `Long` row) on every pass until it closed. The fetch now reports a
//! symbol's positions as ONE `Both` row with the sign in `qty` (IG's convention), and keeps the directional
//! rows only for a symbol that holds BOTH directions (a locked or partly hedged pair), which one number cannot
//! hold and which stays a held, visible divergence.
//!   `ProtoOADealListReq`/`Res` (fill reports — historical deals by timestamp range; `since` maps
//!   directly to `from_timestamp`, both already epoch-ms, no unit conversion needed, unlike
//!   Alpaca's RFC3339 gap).
//!
//! ## Admitted symbols (any-symbol, decision 0173)
//! The client is built on the MOUNTED symbol, and [`ReconClient::admit_symbol`] tells it each symbol the
//! account's engine was admitted on demand. All three wire reads are account-wide (`RECONCILE_REQ` carries
//! every order and position, `DEAL_LIST_REQ` every deal), so a pass reads the SAME requests as before and
//! filters the answer on the mounted symbol FIRST and each admitted one after it: orders and positions by
//! name (an admitted symbol with no open position reports a synthesized flat row, as the mounted one does),
//! fills by symbol id out of the one deal list. With nothing admitted a pass is byte-identical to what it
//! was. A pass that cannot read a symbol it claims FAILS whole (the restore planner reads an order a
//! successful report lacks as gone): an admitted name the account's symbol list no longer carries, or two
//! names that differ only by case, and a deal list still truncated (`has_more`) after `DEAL_PAGE_ROUNDS`
//! requests (only paged when something is admitted: each next request starts, or ends, at the previous
//! page's edge; the mounted-only pass reads the first chunk, as ever). The told names are upper-case
//! ([`AdmittedSymbols`]); a broker's names are case-sensitive, so each is matched case-insensitively
//! against the handshake's list and the report carries the broker's own spelling, which is the one the
//! engine trades (the door sends the name exactly as the facts listed it).
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
//! **No live fee-rate lane**: cTrader's Open API exposes no per-account maker/taker rate over this
//! protocol, so `fetch_fee_rates` stays the trait default (`Ok(None)`).
//!
//! **The balance IS read** ([`parse_balance`], `ReconClient::fetch_balance`): one `ProtoOATraderReq`
//! per call, because without it the engine of a cTrader account never left its configured seed cash
//! (the trait default says "this venue reports none") and its buying-power gate judged every order
//! against that seed. It is refused (`Err`, the seed stays and the node logs `balance fetch failed`)
//! for a deposit currency other than USD, which the bare number cannot say.
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

use vike_exec::recon::{AdmittedSymbols, ReconClient};
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FillReport, MarginMode, OrderStatus, OrderStatusReport, OrderType, PositionStatusReport,
};

use crate::conn::{self, ConnConfig, Reader};
use crate::proto::{
    ProtoOaDeal, ProtoOaDealListReq, ProtoOaDealListRes, ProtoOaDealStatus,
    ProtoOaGetPositionUnrealizedPnLReq, ProtoOaGetPositionUnrealizedPnLRes, ProtoOaOrder,
    ProtoOaOrderStatus, ProtoOaOrderType, ProtoOaPosition, ProtoOaReconcileReq,
    ProtoOaReconcileRes, ProtoOaTradeSide, ProtoOaTrader, ProtoOaTraderReq, ProtoOaTraderRes, pt,
};
use crate::symbols::SymbolMap;

const VENUE: &str = "ctrader";

// --- pure parsers (over already-decoded prost types, not raw wire bytes) ------------------------

/// `tradeSide` -> signed side: `SELL` -> `-1`, anything else (`BUY`) -> `+1`. Matches
/// `event_mapper::deal_fill`'s own `deal.trade_side == ProtoOaTradeSide::Sell as i32` check.
fn side_sign(trade_side: i32) -> i32 {
    if trade_side == ProtoOaTradeSide::Sell as i32 { -1 } else { 1 }
}

/// The `ProtoOAOrderType` as an [`OrderType`]: `MARKET`, `LIMIT`, `STOP` and `STOP_LIMIT` are their
/// twins; `MARKET_RANGE` (a market order bounded by a slippage range) and `STOP_LOSS_TAKE_PROFIT` (a
/// position's protection order) have none and stay [`OrderType::Unrecognized`] with the enum's own
/// wire name lower-cased, as every report spelled them before the type existed. A raw value the
/// generated enum does not know is WARNED and kept as an empty `Unrecognized` — never panics.
fn order_type_of(order_type: i32) -> OrderType {
    match ProtoOaOrderType::try_from(order_type) {
        Ok(ProtoOaOrderType::Market) => OrderType::Market,
        Ok(ProtoOaOrderType::Limit) => OrderType::Limit,
        Ok(ProtoOaOrderType::Stop) => OrderType::Stop,
        Ok(ProtoOaOrderType::StopLimit) => OrderType::StopLimit,
        Ok(t @ (ProtoOaOrderType::MarketRange | ProtoOaOrderType::StopLossTakeProfit)) => {
            OrderType::Unrecognized(t.as_str_name().to_ascii_lowercase())
        }
        Err(_) => {
            tracing::warn!(venue = VENUE, order_type, "unknown ProtoOAOrderType value");
            OrderType::Unrecognized(String::new())
        }
    }
}

/// Map a `ProtoOAOrder` onto the FSM [`OrderStatus`] — every `ProtoOAOrderStatus` value,
/// explicitly. `ORDER_STATUS_ACCEPTED` (the only non-terminal status) is further split by fill
/// progress — `executedVolume` between `0` and the order's `volume` -> `PartiallyFilled`, matching
/// Hyperliquid's/Bybit's own derive-from-fill-progress convention for a venue whose resting-order
/// status has no distinct "partially filled" value; `FILLED`, `REJECTED`, `EXPIRED` and `CANCELLED`
/// are their twins. A raw value the generated enum does not know is WARNED and read as `Accepted`:
/// live, the conservative direction — `diff` raises no `MissingTerminal` on it, adoption synthesizes
/// no cumulative fill, and the restore planner keeps the order rather than declaring it gone.
fn order_status_of(order: &ProtoOaOrder) -> OrderStatus {
    match ProtoOaOrderStatus::try_from(order.order_status) {
        Ok(ProtoOaOrderStatus::OrderStatusAccepted) => {
            let filled = order.executed_volume.unwrap_or(0);
            if filled > 0 && filled < order.trade_data.volume {
                OrderStatus::PartiallyFilled
            } else {
                OrderStatus::Accepted
            }
        }
        Ok(ProtoOaOrderStatus::OrderStatusFilled) => OrderStatus::Filled,
        Ok(ProtoOaOrderStatus::OrderStatusRejected) => OrderStatus::Rejected,
        Ok(ProtoOaOrderStatus::OrderStatusExpired) => OrderStatus::Expired,
        Ok(ProtoOaOrderStatus::OrderStatusCancelled) => OrderStatus::Canceled,
        Err(_) => {
            tracing::warn!(
                venue = VENUE,
                order_status = order.order_status,
                "unknown ProtoOAOrderStatus value — reporting the order as live (Accepted)"
            );
            OrderStatus::Accepted
        }
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
                order_type: order_type_of(o.order_type),
                qty: o.trade_data.volume as f64 / 100.0,
                filled_qty: o.executed_volume.unwrap_or(0) as f64 / 100.0,
                avg_px: o.execution_price.unwrap_or(0.0),
                status: order_status_of(o),
                ts: o.utc_last_update_timestamp.unwrap_or(0),
            }
        })
        .collect()
}

/// `ProtoOAReconcileRes.position` rows -> `PositionStatusReport`. `tradeData.volume` is UNSIGNED
/// (in cents) — the sign comes from `tradeData.tradeSide` (`SELL` -> short/negative), the same
/// unsigned-magnitude-plus-side convention Bybit's/Alpaca's parsers already use. `price` is the
/// position's VWAP entry price (already a real double, no rescale). ONE row per venue position and
/// directional (`Long`/`Short`), exactly as the venue states them: [`net_positions`] is what puts a
/// symbol's rows in the shape the engine's book keys.
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

/// **One symbol's open positions in the shape the engine's book keys** (module doc, "Position report
/// keys"): `rows` are [`parse_positions`]' rows for ONE symbol, and a symbol that holds at most one
/// direction comes back as ONE `Both` row, `qty` the signed sum, `avg_px` the volume-weighted entry
/// price (`0.0`, "no usable price", when any leg lacks one: `resolve` then refuses to book at it), `ts`
/// the newest leg's. A symbol that holds BOTH directions (a locked or partly hedged pair, which one net
/// number would hide) comes back as one `Long` and one `Short` row, each summed the same way: the book
/// has no such key, so each stays a held `PositionOnlyExternal`, and the rows still sum to the net, which
/// is what the core reads as the venue's net position. A zero-volume row (the empty position a pending
/// order makes) carries no exposure and is dropped; no row left means the caller synthesizes the flat one.
pub fn net_positions(rows: &[PositionStatusReport]) -> Vec<PositionStatusReport> {
    let (longs, shorts): (Vec<&PositionStatusReport>, Vec<&PositionStatusReport>) =
        rows.iter().filter(|p| p.qty != 0.0).partition(|p| p.qty > 0.0);
    if longs.is_empty() || shorts.is_empty() {
        let legs: Vec<&PositionStatusReport> = longs.into_iter().chain(shorts).collect();
        merge_legs(PositionSide::Both, &legs).into_iter().collect()
    } else {
        [merge_legs(PositionSide::Long, &longs), merge_legs(PositionSide::Short, &shorts)]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// [`net_positions`]' one merged row for `legs` under `side`: the signed `qty` sum, the size-weighted
/// price (`0.0` unless every leg has a usable one), the newest `ts`; `None` for no legs.
fn merge_legs(side: PositionSide, legs: &[&PositionStatusReport]) -> Option<PositionStatusReport> {
    let first = legs.first()?;
    let weight: f64 = legs.iter().map(|p| p.qty.abs()).sum();
    let priced = legs.iter().all(|p| p.avg_px.is_finite() && p.avg_px > 0.0);
    Some(PositionStatusReport {
        position_side: side,
        qty: legs.iter().map(|p| p.qty).sum(),
        avg_px: if priced {
            legs.iter().map(|p| p.avg_px * p.qty.abs()).sum::<f64>() / weight
        } else {
            0.0
        },
        ts: legs.iter().map(|p| p.ts).max().unwrap_or(0),
        ..(**first).clone()
    })
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

/// **One open position's UNREALIZED PnL as the venue states it**, in the account's deposit currency
/// (`ProtoOAGetPositionUnrealizedPnLRes`, descaled by the RESPONSE's own `moneyDigits`, a required field: no
/// default is assumed). READ-ONLY and for VERIFICATION only: the multi-currency demo drill holds a larger
/// position (10,000 units) and sets the venue's figure beside the engine's, because a 1,000-unit round trip
/// books whole cents and cannot tell orientation, units and rate effects apart. Nothing in a reconcile pass or
/// the engine reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct UnrealizedPnl {
    pub position_id: i64,
    /// The broker's name for the position's symbol, `""` when the position is not in the reconcile reply.
    pub symbol: String,
    /// `+1` long, `-1` short, `0` when the position is not in the reconcile reply.
    pub side: i32,
    /// The position's size in base units (the wire's centi-units divided by 100), `0` when unknown.
    pub units: f64,
    /// `grossUnrealizedPnL` descaled: "denoted in the account deposit currency".
    pub gross: f64,
    /// `netUnrealizedPnL` descaled: "does not include potential closing commission".
    pub net: f64,
    /// The exponent the integers were descaled by.
    pub money_digits: u32,
}

impl UnrealizedPnl {
    /// One greppable line, the format the demo drill reads (`key=value` pairs, no spaces inside a value).
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "CTPNL position={} sym={} side={} units={} gross={} net={} digits={}",
            self.position_id,
            if self.symbol.is_empty() { "?" } else { &self.symbol },
            self.side,
            self.units,
            self.gross,
            self.net,
            self.money_digits,
        )
    }
}

/// `ProtoOAGetPositionUnrealizedPnLRes` rows -> [`UnrealizedPnl`], each joined by position id to the open
/// positions `open` of the same moment (symbol, side, size). Pure over already-decoded types.
pub fn unrealized_pnls_of(
    res: &ProtoOaGetPositionUnrealizedPnLRes,
    open: &[RawPosition],
) -> Vec<UnrealizedPnl> {
    let scale = 10f64.powi(i32::try_from(res.money_digits).unwrap_or(i32::MAX));
    res.position_unrealized_pn_l
        .iter()
        .map(|row| {
            let held = open.iter().find(|p| p.position_id == row.position_id);
            UnrealizedPnl {
                position_id: row.position_id,
                symbol: held.map(|p| p.symbol.clone()).unwrap_or_default(),
                side: held.map_or(0, |p| p.side),
                units: held.map_or(0.0, |p| p.volume as f64 / 100.0),
                gross: row.gross_unrealized_pn_l as f64 / scale,
                net: row.net_unrealized_pn_l as f64 / scale,
                money_digits: res.money_digits,
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
/// `event_mapper::deal_fill` already applies to the live fill stream, with the same venue conversion fee
/// added on a foreign-quoted close. `client_order_id` is always
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
            // Plus the venue's conversion fee on a foreign-quoted close, as the live fill carries it.
            let commission = -(d.commission.unwrap_or(0) as f64) / 10f64.powi(digits)
                + crate::close_detail::conversion_fee_of(d, money_digits);
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

/// **The account's cash, as the engine's ledger may hold it**: `ProtoOATrader.balance` descaled by the
/// trader's OWN `moneyDigits` (an absent one is refused in words, never read as a scale of 0:
/// `conn::handshake::require_money_digits`, the handshake's own refusal), when the account's deposit
/// currency is USD.
///
/// ⚠ **`deposit` is the whole fence, and it fails CLOSED.** A reconcile balance is a bare number that
/// carries no currency (`ReconClient::fetch_balance`), and the core adopts it into a ledger it assumes
/// is in the account's money: 1,000,000 JPY adopted as 1,000,000 dollars would let the buying-power gate
/// admit a hundred and fifty times the account. So a deposit that is not USD, or that the broker's asset
/// list does not name, is an `Err` the caller reports (`balance fetch failed`, the seed stays), never an
/// `Ok(None)` that reads like a venue with no balance. shortcut: USD only, because that is the one
/// currency the engine's ledger is guaranteed to share without the node having anchored it; lift it
/// when the report carries its currency.
pub fn parse_balance(trader: &ProtoOaTrader, deposit: Option<&str>) -> Result<f64, String> {
    match deposit {
        Some(ccy) if ccy.trim().eq_ignore_ascii_case("USD") => {}
        Some(ccy) => {
            return Err(format!(
                "ctrader recon: the account's deposit currency is {ccy}, and a reconcile balance \
                 carries no currency, so it is not adopted into the engine's USD ledger"
            ));
        }
        None => {
            return Err("ctrader recon: the account's deposit currency is unknown (the broker's \
                        asset list does not name it), so its balance is not adopted"
                .to_string());
        }
    }
    let digits =
        conn::handshake::require_money_digits(trader.money_digits).map_err(|e| e.to_string())?;
    Ok(trader.balance as f64 / 10f64.powi(digits as i32))
}

/// The admitted `name` (upper-cased by [`AdmittedSymbols`]) as the broker spells it, with its id: the ONE
/// symbol of `symbols` whose name matches without case. `Err` when none does (the broker dropped it from
/// the list) or several do (two names differing only by case: the report could not say which was meant),
/// because a pass that quietly skipped it would claim a symbol it never read.
fn resolve_admitted(symbols: &SymbolMap, name: &str) -> Result<(String, i64), String> {
    let mut hits = symbols.names().filter(|n| n.eq_ignore_ascii_case(name));
    match (hits.next(), hits.next()) {
        (Some(hit), None) => symbols
            .id_of(hit)
            .map(|id| (hit.to_string(), id))
            .ok_or_else(|| format!("ctrader recon: admitted symbol {name} has no id")),
        (None, _) => Err(format!(
            "ctrader recon: admitted symbol {name} is not in the account's symbol list, so its \
             orders, fills and position cannot be read"
        )),
        (Some(_), Some(_)) => Err(format!(
            "ctrader recon: admitted symbol {name} matches more than one name of the account's \
             symbol list (they differ only by case), so which one to read is not known"
        )),
    }
}

// --- the client ----------------------------------------------------------------------------

/// Re-handshake the dedicated connection once it has been idle this long. MUST stay below
/// cTrader's server-side idle-disconnect tolerance; [`conn::HEARTBEAT_INTERVAL`] (10s) is the
/// cadence the exec actor keeps its own socket alive at, so reviving at the same age is
/// conservative by construction — this connection is re-opened no later than the point the actor
/// would have had to send a keepalive to hold it.
const IDLE_REVIVE_AFTER: Duration = conn::HEARTBEAT_INTERVAL;

/// The most deal-list requests one fill pass makes when the list says `has_more` and a symbol is admitted.
const DEAL_PAGE_ROUNDS: usize = 10;

const DEAL_LIST_TRUNCATED: &str = "ctrader recon: the deal list has more rows than one reply carries \
    (`has_more`) and paging did not reach its end, so the fills of every admitted symbol cannot be claimed read";

/// The window after a TRUNCATED page of deals, or `None` when the page cannot move it (empty). The reply's
/// order is unmeasured, so the direction is read off the page: oldest first (the page ends at its newest
/// row) the next window STARTS at that row; newest first the next window ENDS at the oldest row. The
/// boundary instant is in both windows, so the caller drops a deal it already holds by id.
fn next_deal_window(
    page: &[ProtoOaDeal],
    (from, to): (Option<i64>, Option<i64>),
) -> Option<(Option<i64>, Option<i64>)> {
    let (first, last) = (page.first()?.execution_timestamp, page.last()?.execution_timestamp);
    let stamps = page.iter().map(|d| d.execution_timestamp);
    if first <= last { Some((stamps.max(), to)) } else { Some((from, stamps.min())) }
}

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
/// per-response wait (the exec lane's `response_timeout` in
/// `vike_venues::venue_timings::CTRADER`, 15s) — longer than the 10s cadence it would need
/// to keep, so it could not honor its own interval without interleaving a write into a half-read
/// exchange. Lazy revive needs no thread, no shared clock, and nothing to stop on drop. It also
/// costs no more than a per-pass reconnect would: the three `fetch_*` calls of one pass run
/// microseconds apart, so only the FIRST revives and its siblings reuse that fresh connection.
pub struct CtraderReconClient {
    state: Mutex<ConnState>,
    /// The connection config every revive handshakes with. A `Mutex` because a revive FOLLOWS the
    /// process's shared grant ([`ConnConfig::sync_grant`]) and so rewrites the pair it carries —
    /// the client used to keep the copy it was built with, which a sibling's rotation made stale
    /// for good (every later pass failed its handshake until the daemon restarted). Locked only by
    /// a revive, under `state` (that order), for the length of its handshake; never during a request.
    ///
    /// ⚠ **This client never SPENDS the refresh token**: it only adopts what the exec/data actors
    /// rotated. A pass that finds the grant lapsed with no sibling having renewed it fails and is
    /// reported, exactly as before.
    cfg: Mutex<ConnConfig>,
    symbol: String,
    idle_revive_after: Duration,
    /// The symbols the account's engine admitted on demand: every pass reads them beside the mounted
    /// one (the module doc).
    admitted: AdmittedSymbols,
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
        // The pair a sibling socket of this process holds NOW, not the snapshot `cfg` was built from.
        let mut cfg = cfg.clone();
        cfg.sync_grant();
        let (reader, ctid, symbols, money_digits) =
            conn::open_and_handshake(&cfg, cfg.account_id).ok()?;
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
            cfg: Mutex::new(cfg),
            symbol: symbol.to_string(),
            idle_revive_after,
            admitted: AdmittedSymbols::default(),
        })
    }

    /// The `(name, symbol id)` pairs a pass reads on THIS handshake's symbol list: the mounted symbol
    /// first, then each admitted one the list resolves (module doc). A name already in the set under
    /// another case is read once.
    fn read_set(&self, st: &ConnState) -> Result<Vec<(String, i64)>, String> {
        let mut set = vec![(self.symbol.clone(), st.symbol_id)];
        for name in self.admitted.symbols() {
            let (resolved, id) = resolve_admitted(&st.symbols, &name)?;
            if !set.iter().any(|(_, held)| *held == id) {
                set.push((resolved, id));
            }
        }
        Ok(set)
    }

    /// Re-handshake `st` in place when it has gone idle past [`Self::idle_revive_after`]; a
    /// still-fresh connection is left untouched. The resolved `ctid` is passed back as the FIXED
    /// account id so a revive can never re-discover and silently authorize a DIFFERENT account —
    /// the same invariant `open_and_handshake`'s own doc states for reconnect.
    fn revive_if_idle(&self, st: &mut ConnState) -> Result<(), String> {
        if st.last_used.elapsed() < self.idle_revive_after {
            return Ok(());
        }
        let (reader, ctid, symbols, money_digits) = {
            let mut cfg =
                self.cfg.lock().map_err(|_| "ctrader recon config poisoned".to_string())?;
            // Follow the grant: a sibling's rotation since the last pass must not leave this
            // handshake presenting the access token the venue already ended.
            cfg.sync_grant();
            conn::open_and_handshake(&cfg, Some(st.ctid)).map_err(|e| e.to_string())?
        };
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

    /// One `ProtoOADealListReq` over the window `(from, to)` (epoch ms, either bound optional).
    fn deal_page(
        &self,
        st: &mut ConnState,
        (from_timestamp, to_timestamp): (Option<i64>, Option<i64>),
    ) -> Result<ProtoOaDealListRes, String> {
        let req = ProtoOaDealListReq {
            ctid_trader_account_id: st.ctid,
            from_timestamp,
            to_timestamp,
            ..Default::default()
        };
        self.call(st, pt::DEAL_LIST_REQ, &req, pt::DEAL_LIST_RES)
    }

    fn reconcile(&self, st: &mut ConnState) -> Result<ProtoOaReconcileRes, String> {
        let req = ProtoOaReconcileReq { ctid_trader_account_id: st.ctid, ..Default::default() };
        self.call(st, pt::RECONCILE_REQ, &req, pt::RECONCILE_RES)
    }

    /// The account's currently-OPEN positions for this client's engine symbol as RAW legs — each
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

    /// **What the venue booked on every CLOSING deal since `since`** (epoch ms), ACCOUNT-wide and oldest
    /// first: the `ProtoOAClosePositionDetail` of each (`crate::close_detail`). READ-ONLY and for
    /// VERIFICATION only (the multi-currency demo drill sets the engine's realized PnL beside it): nothing
    /// in a reconcile pass reads it. One `ProtoOADealListReq`; a reply that says `has_more` is an error,
    /// never a quietly short list.
    pub fn close_details(
        &self,
        since: i64,
    ) -> Result<Vec<crate::close_detail::CloseDetail>, String> {
        let mut st = self.locked()?;
        let page = self.deal_page(&mut st, (Some(since).filter(|s| *s > 0), None))?;
        if page.has_more {
            return Err("ctrader recon: the deal list has more rows than one reply carries \
                        (`has_more`); narrow the window"
                .to_string());
        }
        Ok(crate::close_detail::close_details_of(&page.deal, &st.symbols, st.money_digits))
    }

    /// **The venue's own unrealized PnL of every open position** (`ProtoOAGetPositionUnrealizedPnLReq`),
    /// ACCOUNT-wide, each joined to its symbol, side and size by a reconcile read of the same moment.
    /// READ-ONLY and for VERIFICATION only ([`UnrealizedPnl`]); nothing in a reconcile pass reads it.
    pub fn unrealized_pnls(&self) -> Result<Vec<UnrealizedPnl>, String> {
        let mut st = self.locked()?;
        let req = ProtoOaGetPositionUnrealizedPnLReq {
            ctid_trader_account_id: st.ctid,
            ..Default::default()
        };
        let res: ProtoOaGetPositionUnrealizedPnLRes = self.call(
            &mut st,
            pt::GET_POSITION_UNREALIZED_PNL_REQ,
            &req,
            pt::GET_POSITION_UNREALIZED_PNL_RES,
        )?;
        let open = self.reconcile(&mut st)?;
        Ok(unrealized_pnls_of(&res, &parse_raw_positions(&open.position, &st.symbols)))
    }
}

impl ReconClient for CtraderReconClient {
    /// `_since` is a no-op here: `ProtoOAReconcileRes` reports only the CURRENTLY pending set
    /// (mirrors Bybit's `order/realtime`) — a fully-closed order that fell out of this set is
    /// what the fill/position reports (not this one) reconcile.
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        let set = self.read_set(&st)?;
        Ok(parse_orders(&res.order, &st.symbols)
            .into_iter()
            .filter(|o| set.iter().any(|(name, _)| *name == o.symbol))
            .collect())
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        let mut st = self.locked()?;
        let mut window = (Some(since).filter(|s| *s > 0), None);
        let mut page = self.deal_page(&mut st, window)?;
        let set = self.read_set(&st)?;
        // The deal list is account-wide and capped: with an admitted symbol in play a reply that says it
        // has more rows than it carries may be missing that symbol's fills, so the list is PAGED (the next
        // window starts at the last page's edge, at most `DEAL_PAGE_ROUNDS` requests) and the pass fails
        // only if it is still truncated, rather than claim them (the mounted-only pass has always read the
        // first chunk, unchanged: one request, no paging).
        let mut deals: Vec<ProtoOaDeal> = Vec::new();
        let mut rounds = 0;
        loop {
            rounds += 1;
            let paging = page.has_more && set.len() > 1;
            let next = if paging { next_deal_window(&page.deal, window) } else { None };
            if deals.is_empty() {
                deals = std::mem::take(&mut page.deal);
            } else {
                // The boundary instant is in both windows: a deal already held is not read twice.
                let known = deals.len();
                let held: std::collections::HashSet<i64> =
                    deals.iter().map(|d| d.deal_id).collect();
                let fresh = std::mem::take(&mut page.deal);
                deals.extend(fresh.into_iter().filter(|d| !held.contains(&d.deal_id)));
                // A page with nothing new cannot move the window: still truncated.
                if paging && deals.len() == known {
                    return Err(DEAL_LIST_TRUNCATED.to_string());
                }
            }
            if !paging {
                break;
            }
            let Some(next) = next.filter(|_| rounds < DEAL_PAGE_ROUNDS) else {
                return Err(DEAL_LIST_TRUNCATED.to_string());
            };
            window = next;
            page = self.deal_page(&mut st, window)?;
        }
        Ok(set
            .iter()
            .flat_map(|(name, id)| parse_fills(&deals, *id, name, st.money_digits))
            .collect())
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let mut st = self.locked()?;
        let res = self.reconcile(&mut st)?;
        let set = self.read_set(&st)?;
        let all = parse_positions(&res.position, &st.symbols);
        // Per symbol read: its open positions as the book keys them, or the flat row cTrader's omission calls for.
        let mut reports = Vec::new();
        for (name, _) in &set {
            let rows: Vec<_> = all.iter().filter(|p| p.symbol == *name).cloned().collect();
            let mut netted = net_positions(&rows);
            if netted.is_empty() {
                netted.push(PositionStatusReport::flat(VENUE, name));
            }
            reports.extend(netted);
        }
        Ok(reports)
    }

    /// Tell this client the engine admitted `symbol` (any-symbol, decision 0173): every later pass reads
    /// its orders, fills and position too. Idempotent, never fetches; the list is bounded and upper-cased
    /// ([`AdmittedSymbols`]), and matched against the broker's own spelling at read time.
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

    /// The account's cash from a FRESH `ProtoOATraderReq` on every call ([`parse_balance`]): the snapshot
    /// the handshake already read would be stale after the first fill, and the core's balance diff would
    /// raise a drift against it. `Err` (never `Ok(None)`) on anything the number cannot be trusted
    /// through, so a failed read keeps the engine's seed AND says so (`balance fetch failed`).
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let mut st = self.locked()?;
        let req = ProtoOaTraderReq { ctid_trader_account_id: st.ctid, ..Default::default() };
        let res: ProtoOaTraderRes = self.call(&mut st, pt::TRADER_REQ, &req, pt::TRADER_RES)?;
        // After the call: a revive inside it re-resolved `st.symbols`, and the deposit name is read off it.
        parse_balance(&res.trader, st.symbols.deposit_currency()).map(Some)
    }
}

// --- the factory -----------------------------------------------------------------------------

/// The venue -> `ReconClient` factory (ReconFactory seam, wave-2 task 6): opens
/// [`CtraderReconClient`]'s own dedicated authed connection from an already-resolved
/// [`crate::config::CtraderConfig`]. `None` on handshake failure — reconcile stays unwired for
/// this venue, exec unaffected (same as [`CtraderReconClient::connect`]'s doc).
///
/// **WIRED into [`crate::adapter::live_runtime_for_account`]** (decision 0088 B5) — called by
/// [`crate::adapter::CtraderVenueAdapter`] — which calls this LAZILY, gated on its own
/// `recon_enabled` parameter, because this factory CONNECTS during construction: a start whose
/// reconcile gate answered NO must not pay a second protobuf/TLS handshake. The preflight
/// credential probe defers it the same way: [`crate::adapter::CtraderVenueAdapter`]'s
/// `credential_probe` calls this factory inside the probe's closure (moved there from
/// `crates/vike-tradehub-core/src/startup.rs` with the venue-adapter contract). cTrader reconciles on the
/// periodic INTERVAL only — no `recon_trigger`.
///
/// ⚠ **That sentence said "an unset `VIKE_RECONCILE`" until S2, and the flip is a real cost on this
/// venue rather than a wording change.** Reconciliation is now ON by default for a start that arms
/// a live venue account (`docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md`), so
/// an armed cTrader start opens this SECOND authenticated socket at every start and holds it for
/// the process lifetime — a serial, blocking handshake on the startup path, and a second consumer
/// of the account's session/connection limits. `VIKE_RECONCILE_OFF=1` is the refusal that keeps the
/// venue armed and skips it.
///
/// ⚠ This line used to say "not yet wired ... cTrader is not a live-mounted venue today", which it
/// had stopped being. A factory's header is the WRONG place to read that from, because a start arm
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ProtoOaLightSymbol;

    fn map(names: &[&str]) -> SymbolMap {
        let light: Vec<ProtoOaLightSymbol> = names
            .iter()
            .enumerate()
            .map(|(i, n)| ProtoOaLightSymbol {
                symbol_id: i as i64 + 1,
                symbol_name: Some((*n).to_string()),
                ..Default::default()
            })
            .collect();
        SymbolMap::from_symbols(&light, &[])
    }

    /// An admitted name (upper-cased by the list) resolves to the broker's own spelling and id; one the
    /// list lacks, or two the list cannot tell apart, is an error naming the symbol - never a skip.
    #[test]
    fn an_admitted_name_resolves_to_the_brokers_spelling_or_is_an_error() {
        let symbols = map(&["EURUSD", "Wheat_H6", "GBPUSD", "gbpusd"]);
        assert_eq!(resolve_admitted(&symbols, "EURUSD"), Ok(("EURUSD".to_string(), 1)));
        assert_eq!(
            resolve_admitted(&symbols, "WHEAT_H6"),
            Ok(("Wheat_H6".to_string(), 2)),
            "the report carries the broker's case"
        );
        let missing = resolve_admitted(&symbols, "AUDUSD").expect_err("not listed");
        assert!(missing.contains("AUDUSD") && missing.contains("not in the account's symbol list"));
        let twin = resolve_admitted(&symbols, "GBPUSD").expect_err("two names differ by case");
        assert!(twin.contains("differ only by case"), "{twin}");
    }

    /// DOCS-SHAPED: a 10,000-unit long USDJPY position (1,000,000 centi-units) whose venue-stated unrealized PnL
    /// is 66.62 dollars gross and 66.00 net on an account whose RESPONSE states `moneyDigits` 2 (`6662` on the
    /// wire): the rows are joined to the open position by id, descaled by the response's own exponent, and a
    /// row whose position the reconcile read lacks keeps its figures with no symbol.
    #[test]
    fn the_venues_unrealized_pnl_is_joined_to_its_position_and_descaled_by_the_responses_digits() {
        use crate::proto::ProtoOaPositionUnrealizedPnL;
        let row = |position_id, gross, net| ProtoOaPositionUnrealizedPnL {
            position_id,
            gross_unrealized_pn_l: gross,
            net_unrealized_pn_l: net,
        };
        let res = ProtoOaGetPositionUnrealizedPnLRes {
            ctid_trader_account_id: 99,
            position_unrealized_pn_l: vec![row(7, 6662, 6600), row(8, -150, -190)],
            money_digits: 2,
            ..Default::default()
        };
        let open = [RawPosition {
            position_id: 7,
            symbol: "USDJPY".to_string(),
            side: 1,
            volume: 1_000_000,
        }];
        let got = unrealized_pnls_of(&res, &open);
        assert_eq!(got.len(), 2);
        assert_eq!(
            (got[0].symbol.as_str(), got[0].side, got[0].units, got[0].gross, got[0].net),
            ("USDJPY", 1, 10_000.0, 66.62, 66.0)
        );
        assert_eq!((got[1].symbol.as_str(), got[1].side, got[1].gross), ("", 0, -1.5));
        assert_eq!(
            got[0].line(),
            "CTPNL position=7 sym=USDJPY side=1 units=10000 gross=66.62 net=66 digits=2"
        );
        // Another exponent descales by itself: no digit count is assumed.
        let wide = ProtoOaGetPositionUnrealizedPnLRes {
            position_unrealized_pn_l: vec![row(7, 6_662_000, 0)],
            money_digits: 5,
            ..Default::default()
        };
        assert_eq!(unrealized_pnls_of(&wide, &open)[0].gross, 66.62);
    }
}
