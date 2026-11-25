//! Pure Bybit private-WS → vike event mappers. Exact ports of `exec/bybit/mapper.py` +
//! `exec/bybit/perp_mapper.py`.
//!
//! `execution` rows → dual-publish [FillEvent, wrap]; `order` rows → lifecycle ONLY
//! (Filled/PartiallyFilled → [] — the execution topic owns fills, no double-fold);
//! `wallet` → AccountState from walletBalance totals; op-only ack/pong frames → [].
//! Perp dispatch additionally: BustTrade/AdlTrade execution rows → PositionLiquidated
//! ONLY (their execFee is fee-POSITIVE — never abs(), never negate), and Trade fills are
//! enriched with markPrice + position_side from positionIdx (0/absent→BOTH, 1→LONG,
//! 2→SHORT). Funding/Settle execution rows map to [] — Bybit funding comes from the REST
//! transaction-log (`execution/funding.rs`); the WS Funding row's execFee is the EXACT NEGATIVE of
//! that cashflow (the documented sign trap — routing around it, not flipping it).
//!
//! Fast-fill hint (opt-in `venue.bybit.fast_exec`, wired in [`crate::execution::user_data`]): `execution.fast`
//! rows — Bybit's low-latency fill stream ("significantly reduces data latency compared original
//! execution stream") — map to a single EARLY bare `Event::Fill` via [`map_execution_fast`], with NO
//! dual-publish wrap (the slim frame omits the terminal signal, so a wrap here could strand the FSM).
//! The `execId` is IDENTICAL to the slow `execution` twin, so the engine's `seen_trade_ids` guard
//! books the fill EXACTLY once; the RETAINED `execution` subscription still carries the fee + terminal
//! wrap that drives FSM terminalization. Inert unless an `execution.fast` frame actually arrives (only
//! under the subscribe-list gate), so a default build is byte-identical.

use serde_json::Value;

use crate::history::Category;
// Loose venue-JSON coercion: the shared keyed accessors, under this module's short local names.
// These wrappers were re-declared per venue; `vike_bridge_core::json` is the one home. `s` keeps
// `json_str`'s Bool arm here — no bool-valued field is read through it (`isMaker` uses `as_bool`).
use vike_bridge_core::event_map::{SpotFeeArm, SpotFeeArms, spot_base_fee_net};
use vike_bridge_core::json::{get_f64 as f, get_fee_f64 as fee_f, get_i64 as i, get_str as s};
use vike_model::events::LiquiditySide;
use vike_model::events::{
    AccountState, Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected,
    PositionLiquidated, PositionSide, TradeId,
};

/// The USER name of a row (decision 0290): the one the kept row of the venue's `symbol` in the row's `category`
/// spells (`crate::instrument::class::user_symbol_or_wire`): a linear `BTCUSDT` is `BTCUSDT.P`, a spot `BTCUSDT`
/// the pair `BTCUSDT`, an inverse `BTCUSD` `BTCUSD.P`. ⚠ The private WS carries EVERY book of the account in one
/// `execution` topic and a spot pair shares its venue name with the linear perpetual, so a name read without its
/// book folded a spot fill of the account (an operator's, from the venue's own app) into the perpetual's position.
/// A row with no `symbol` is the mounted `fallback`'s.
fn symbol_or(v: &Value, fallback: &str) -> String {
    match v.get("symbol") {
        Some(Value::String(x)) => crate::instrument::class::user_symbol_or_wire(
            v.get("category").and_then(Value::as_str).and_then(Category::of_word),
            x,
        ),
        _ => fallback.to_string(),
    }
}

/// Whether a row is of the SPOT book, by its own `category` word (a spot buy's fee is netted: [`SpotNet`]).
fn is_spot_row(v: &Value) -> bool {
    v.get("category").and_then(Value::as_str) == Some(Category::Spot.wire())
}

/// **A spot BUY's fee is charged in the BASE coin, so the fill is booked as the quantity the account received**
/// (decision 0231, the rule of decision 0215 for binance and of 0218 for hyperliquid; MEASURED on the demo host
/// 2026-10-30: `execQty 165.7`, `execFee 0.1657`, `feeCurrency DOGE`, and the wallet gained 165.5343). The fill's
/// quantity becomes `execQty - execFee` and the commission the fee at the fill price in the QUOTE coin, so the
/// engine's position is the account's holding and a later sale of it is not refused for want of coin that was
/// never there. A sell's fee is in the quote coin already (`feeCurrency USDT`) and a derivative's is the
/// settle coin: both stay as stated. Applies only to a SPOT fill (`spot`: the row's book) on the buy side whose fee
/// currency is the pair's base: the pair's name (`BTCUSDT`, a spot pair's user name is its venue name) starts with it
/// and does not end with it.
pub(crate) fn net_spot_base_fee(fill: &mut FillEvent, spot: bool) {
    if let Some(net) = SpotNet::of(
        fill.symbol.as_str(),
        spot,
        fill.side,
        fill.commission_asset.as_str(),
        fill.commission,
        fill.last_qty,
        fill.last_price,
    ) {
        fill.last_qty = net.qty;
        fill.commission = net.commission;
        fill.commission_asset = net.asset.into();
    }
}

/// The result of [`net_spot_base_fee`]'s rule over plain numbers (a `FillEvent` of the stream and the replay, a
/// `FillReport` of the reconcile read): one rule, one place.
pub(crate) struct SpotNet {
    /// The quantity the account received: `execQty - execFee`.
    pub qty: f64,
    /// The fee at the fill price, in the quote coin.
    pub commission: f64,
    /// The quote coin the commission is now in.
    pub asset: String,
}

impl SpotNet {
    /// `Some` for a SPOT fill (`spot`, the book of the row; `pair` its name) on the BUY side whose fee currency is the
    /// pair's base.
    pub(crate) fn of(
        pair: &str,
        spot: bool,
        side: i32,
        asset: &str,
        fee: f64,
        qty: f64,
        px: f64,
    ) -> Option<SpotNet> {
        if !spot || asset.is_empty() || !pair.starts_with(asset) || pair.ends_with(asset) {
            return None;
        }
        let (net, commission) = spot_base_fee_net(side, qty, px, fee, SPOT_FEE_ARMS)?;
        Some(SpotNet {
            qty: (net * NET_QTY_ROUND).round() / NET_QTY_ROUND,
            commission,
            asset: pair[asset.len()..].to_string(),
        })
    }
}

/// A sell pays in the quote (MEASURED: `feeCurrency USDT`), so this venue's SELL arm has never been reached, and no
/// spot rebate is in the tree. Were either stated in the base coin the wallet would move by it and this leaves it out:
/// the quantity short of the wallet and the fee in the wrong unit. Kept until a ruling or a measured row.
const SPOT_FEE_ARMS: SpotFeeArms =
    SpotFeeArms { sell: SpotFeeArm::AsStated, rebate: SpotFeeArm::AsStated };

/// bybit's own: the net quantity is rounded to `1 / NET_QTY_ROUND` of a coin, the one venue that rounds it. It moves the
/// last bits of a measured buy, and a remainder under half a step books a quantity of 0. Removing it needs a ruling.
const NET_QTY_ROUND: f64 = 1e12;

/// Python `float(item.get(key, default))` — raising (None) on unparseable, used where
/// the oracle wraps in try/except.
fn f_strict(v: &Value, key: &str, default: f64) -> Option<f64> {
    match v.get(key) {
        None => Some(default),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(x)) => x.parse::<f64>().ok(),
        Some(_) => None,
    }
}

/// One execution row → [FillEvent, OrderPartiallyFilled|OrderFilled].
/// execType != 'Trade' → [] (skips BustTrade/Funding/AdlTrade/SettleFundingFee here).
pub fn map_execution(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    if item.get("execType").and_then(|e| e.as_str()) != Some("Trade") {
        return Vec::new();
    }
    let order_client_id = s(item, "orderLinkId");
    let ts = i(item, "execTime");
    // `execId` is the fill DEDUP key and Bybit V5 documents it on every `execution` row — an
    // absent/empty one is a malformed frame, not a shape the venue has. DROP the whole fill: the
    // audit-A3 resync replays this same execution out of `/v5/execution/list` (`crate::execution::resync`'s
    // `map_bybit_history`), and with no id the engine's `seen_trade_ids` guard cannot collapse the
    // pair — qty, commission and realized PnL would be booked TWICE. The bare `Event::Fill` and the
    // wrap `vike_bridge_core::terminal_events` mints from it stand or fall TOGETHER: publishing the
    // wrap alone would terminalize the FSM for a fill the Account never folded. Nothing is
    // synthesized — a clock/counter id differs on the second run and so defeats dedup outright.
    let Ok(trade_id) = TradeId::new(s(item, "execId")) else {
        tracing::warn!(
            venue,
            symbol = %symbol_or(item, symbol),
            order_client_id = %order_client_id,
            "execution row carries no `execId` — dropping the fill and its wrap; an un-dedupable \
             fill double-books commission and realized PnL when the resync replays it"
        );
        return Vec::new();
    };
    let mut fill = FillEvent {
        trade_id,
        order_client_id: order_client_id.clone(),
        venue: venue.to_string().into(),
        symbol: symbol_or(item, symbol).into(),
        side: if item.get("side").and_then(|x| x.as_str()) == Some("Buy") { 1 } else { -1 },
        last_qty: f(item, "execQty"),
        last_price: f(item, "execPrice"),
        commission: fee_f(item, "execFee", venue),
        commission_asset: s(item, "feeCurrency").into(),
        liquidity_side: LiquiditySide::from_maker(
            item.get("isMaker").and_then(|m| m.as_bool()).unwrap_or(false),
        ),
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    net_spot_base_fee(&mut fill, is_spot_row(item));
    // Terminal detection, robust to string-dust: cumExecQty >= orderQty when both
    // present; fallback leavesQty == exactly 0.0; any parse failure -> partial.
    // (Python's try wraps BOTH branches — a bad leavesQty also lands on False.)
    let is_filled = (|| -> Option<bool> {
        let cum = f_strict(item, "cumExecQty", -1.0)?;
        let order_qty = f_strict(item, "orderQty", -1.0)?;
        if cum >= 0.0 && order_qty > 0.0 && cum >= order_qty {
            return Some(true);
        }
        // Python: float(item.get("leavesQty", 1) or 1) — falsy ("", 0, null) -> 1
        let leaves = match item.get("leavesQty") {
            None => 1.0,
            Some(Value::String(x)) if x.is_empty() => 1.0,
            Some(Value::Null) => 1.0,
            Some(Value::Number(n)) => {
                let v = n.as_f64()?;
                if v == 0.0 { 1.0 } else { v }
            }
            Some(Value::String(x)) => {
                let v = x.parse::<f64>().ok()?;
                // Python `or 1` applies to the STRING (non-empty is truthy) — "0" parses
                // to 0.0 and stays 0.0
                v
            }
            Some(_) => return None,
        };
        Some(leaves == 0.0)
    })()
    .unwrap_or(false);
    vike_bridge_core::terminal_events(order_client_id, fill, ts, is_filled)
}

/// True for Bybit's low-latency fill topic — the uncategorised `execution.fast` AND its categorised
/// variants (`execution.fast.linear` / `.spot` / …). Kept distinct from the exact `"execution"` arm:
/// the slow full stream is `"execution"`, the fast slim stream is `"execution.fast"`.
pub(crate) fn is_fast_execution_topic(topic: &str) -> bool {
    topic == "execution.fast" || topic.starts_with("execution.fast.")
}

/// One `execution.fast` row → an EARLY bare `[Event::Fill]` (deliberately NO dual-publish wrap).
///
/// Bybit's fast execution stream is a SLIM frame — category / symbol / orderId / isMaker /
/// orderLinkId / side / execId / execPrice / execQty / execTime — and OMITS execType, execFee +
/// feeCurrency, cumExecQty + orderQty + leavesQty, markPrice, and positionIdx. Consequences, each
/// load-bearing:
///
/// * **Commission UNKNOWN → `0.0`.** The fee is not on the fast wire; the slow `execution` twin
///   carries the real `execFee`. (That twin's bare Fill is dedup-dropped by the engine, so the fee is
///   NOT retro-applied — a documented cost of the early hint, not enrichable without an engine-level
///   fee-delta lane. See the crate report.)
/// * **NO wrap — bare Fill ONLY.** With no cum/order/leaves qty there is no terminal-vs-partial
///   signal, so we cannot emit `OrderFilled`/`OrderPartiallyFilled` correctly. Emitting a partial
///   wrap here would advance the engine's `seen_fsm_trade_ids` for this execId, causing the slow
///   twin's REAL terminal wrap (same execId) to be dedup-dropped — stranding the order at
///   PARTIALLY_FILLED. Emitting only the bare Fill lets the RETAINED `execution` subscription's wrap
///   drive FSM terminalization while the position + the fill-rate breaker move EARLY off the fast row.
/// * **Dedup key `trade_id = execId`** is IDENTICAL across `execution.fast` and `execution`, so the
///   engine's always-on `seen_trade_ids` guard folds the position for this execId EXACTLY once —
///   whichever topic arrives first wins, the other's bare Fill is dropped
///   (`vike_execution::Engine::on_event`).
/// * **No execType branch.** The fast stream is fills-only (Bybit never pushes BustTrade/AdlTrade/
///   Funding rows on it), so every row is a Trade fill. `mark_price`/`position_side` are absent →
///   `None` / `BOTH` (correct for one-way; a hedge-mode side is only learned from the slow twin).
pub fn map_execution_fast(item: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    // A SPOT row carries no fee on the fast wire, and a spot buy's quantity is `execQty - execFee` (decision
    // 0231, [`net_spot_base_fee`]): the early hint would book the GROSS quantity and its slow twin, dedup-dropped
    // on the shared `execId`, could never correct it. The slow `execution` row books the fill, a few ms later.
    if is_spot_row(item) {
        return Vec::new();
    }
    // The fast hint's ENTIRE correctness argument is the third bullet above — its `execId` equals the
    // slow `execution` twin's, so `seen_trade_ids` folds the trade exactly once. With no `execId`
    // that argument is gone: this row would fold, and then the slow twin would fold AGAIN. Drop it.
    // The slow twin still books the trade (with the real fee and the terminal wrap), so the only
    // thing lost is the few-ms head start.
    let Ok(trade_id) = TradeId::new(s(item, "execId")) else {
        tracing::warn!(
            venue,
            symbol = %symbol_or(item, symbol),
            order_client_id = %s(item, "orderLinkId"),
            "execution.fast row carries no `execId` — dropping the early fill hint; without the \
             shared dedup key it would double-fold against its slow `execution` twin"
        );
        return Vec::new();
    };
    let fill = FillEvent {
        trade_id,
        order_client_id: s(item, "orderLinkId"),
        venue: venue.to_string().into(),
        symbol: symbol_or(item, symbol).into(),
        side: if item.get("side").and_then(|x| x.as_str()) == Some("Buy") { 1 } else { -1 },
        last_qty: f(item, "execQty"),
        last_price: f(item, "execPrice"),
        // Fast frame omits execFee/feeCurrency — fee is unknown at the early hint (the slow
        // `execution` twin carries the real values, but is dedup-dropped by execId).
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: LiquiditySide::from_maker(
            item.get("isMaker").and_then(|m| m.as_bool()).unwrap_or(false),
        ),
        timestamp: i(item, "execTime"),
        mark_price: None,
        position_side: PositionSide::Both,
    };
    // A hint with no price or quantity would win the shared `execId` and book its twin's trade at 0.
    if !vike_bridge_core::event_map::fill_is_bookable(&fill) {
        return Vec::new();
    }
    // Bare Fill ONLY — see the fn doc: the FSM terminal is owned by the retained `execution` twin.
    vec![Event::Fill(fill)]
}

/// Is this order row a POST-ONLY order the venue ended because it would have taken liquidity? The
/// venue's own typed field, `rejectReason=EC_PostOnlyWillTakeLiquidity` (MEASURED on the demo host, see
/// `tests/bybit_post_only_smoke.rs`): the create call succeeded, so the order is ACCEPTED here and the
/// only terminal the state machine allows is a cancel.
pub fn is_post_only_cross(item: &Value) -> bool {
    item.get("rejectReason").and_then(|x| x.as_str()) == Some("EC_PostOnlyWillTakeLiquidity")
}

/// One order row → lifecycle ONLY (never fills).
pub fn map_order(item: &Value, _venue: &str, _symbol: &str) -> Vec<Event> {
    let status = item.get("orderStatus").and_then(|x| x.as_str()).unwrap_or("");
    let order_client_id = s(item, "orderLinkId");
    let ts = i(item, "updatedTime");
    // a post-only order the venue ended for crossing: a typed cancel, whatever status word it used
    if matches!(status, "Cancelled" | "Rejected") && is_post_only_cross(item) {
        return vec![Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: vike_model::events::POST_ONLY_CANCEL_REASON.into(),
            timestamp: ts,
            post_only_would_cross: true,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })];
    }
    match status {
        // REST already published Accepted; the engine swallows the duplicate transition
        "New" => vec![Event::OrderAccepted(OrderAccepted {
            order_client_id,
            venue_order_id: Some(s(item, "orderId").into()),
            timestamp: ts,
        })],
        // `Deactivated` is the venue's cancel of a conditional before it fires ("Spot tp/sl order, conditional
        // order, OCO order are cancelled before they are triggered", the v5 `orderStatus` enum), never an expiry.
        "Cancelled" | "PartiallyFilledCanceled" | "Deactivated" => {
            vec![Event::OrderCanceled(OrderCanceled {
                order_client_id,
                reason: s(item, "cancelType").into(),
                timestamp: ts,
                post_only_would_cross: false,
                refusal_id: None,
            })]
        }
        "Rejected" => vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id,
            reason: s(item, "rejectReason").into(),
            timestamp: ts,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })],
        _ => Vec::new(), // Filled/PartiallyFilled: execution topic owns fills
    }
}

fn wallet_account_state(frame: &Value, venue: &str) -> Option<Event> {
    // data[].coin[] with coin/walletBalance (TOTAL); malformed rows skipped silently
    let ts = i(frame, "creationTime");
    let mut balances: Vec<(String, f64)> = Vec::new();
    for account in frame.get("data").and_then(|d| d.as_array()).unwrap_or(&vec![]) {
        for c in account.get("coin").and_then(|c| c.as_array()).unwrap_or(&vec![]) {
            let asset = c.get("coin").and_then(|a| a.as_str()).unwrap_or("");
            // Python float(c.get("walletBalance", 0) or 0) in try/except -> skip row on bad
            let wb = match c.get("walletBalance") {
                None | Some(Value::Null) => Some(0.0),
                Some(Value::Number(n)) => n.as_f64(),
                Some(Value::String(x)) if x.is_empty() => Some(0.0),
                Some(Value::String(x)) => x.parse::<f64>().ok(),
                Some(_) => None,
            };
            let Some(wb) = wb else { continue };
            if !asset.is_empty() {
                balances.push((asset.to_string(), wb));
            }
        }
    }
    if balances.is_empty() {
        None
    } else {
        Some(Event::AccountState(AccountState {
            venue: venue.to_string().into(),
            balances,
            timestamp: ts,
            // A bridge holds ONE credential set and knows no account labels: the START stamps
            // the route key (`vike_tradehub_core::account_event_sender`), never a venue adapter.
            route_key: None,
        }))
    }
}

/// Spot dispatch: topic execution/order/wallet; op-only ack frames → [].
pub fn map_bybit_private(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let Some(topic) = frame.get("topic").and_then(|t| t.as_str()) else {
        return Vec::new(); // op-only ack (pong/auth/subscribe)
    };
    let no_rows = vec![];
    let data = frame.get("data").and_then(|d| d.as_array()).unwrap_or(&no_rows);
    let mut events = Vec::new();
    match topic {
        // Fast-fill hint (opt-in): the low-latency `execution.fast` twin → early bare Fill only.
        t if is_fast_execution_topic(t) => {
            for item in data {
                events.extend(map_execution_fast(item, venue, symbol));
            }
        }
        "execution" => {
            for item in data {
                events.extend(map_execution(item, venue, symbol));
            }
        }
        "order" => {
            for item in data {
                // The retired oracle's word for `Deactivated`, frozen in `fixtures/r6/bybit.json`'s
                // `spot_mapper` (a frozen byte moves only on an owner ruling, `fixtures/README.md`); no
                // production path runs this mapper, the live pump runs `map_bybit_perp`'s cancel.
                if item.get("orderStatus").and_then(Value::as_str) == Some("Deactivated") {
                    let (order_client_id, ts) = (s(item, "orderLinkId"), i(item, "updatedTime"));
                    events
                        .push(Event::OrderExpired(OrderExpired { order_client_id, timestamp: ts }));
                    continue;
                }
                events.extend(map_order(item, venue, symbol));
            }
        }
        "wallet" => events.extend(wallet_account_state(frame, venue)),
        _ => {}
    }
    events
}

/// Bybit V5 `positionIdx` → [`PositionSide`] — the ONE reading of the field (fills, liquidations,
/// history, the reconcile snapshot and the position reports all call it). The documented values:
/// `0` one-way → `Both`, `1` hedge buy side → `Long`, `2` hedge sell side → `Short`; an ABSENT
/// field is one-way (`Both`; the fast execution stream and spot rows carry none). Any other value
/// (an unknown number, a non-numeric string, another JSON type) is WARNED and read as `Both`, the
/// one-way bucket whose signed qty still carries the direction.
pub(crate) fn pside_from_index(row: &Value) -> PositionSide {
    let index = match row.get("positionIdx") {
        None | Some(Value::Null) => Some(0),
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(x)) => x.parse::<i64>().ok(),
        Some(_) => None,
    };
    match index {
        Some(0) => PositionSide::Both,
        Some(1) => PositionSide::Long,
        Some(2) => PositionSide::Short,
        _ => {
            let raw = row.get("positionIdx").map(|v| v.to_string()).unwrap_or_default();
            tracing::warn!(
                venue = crate::execution::rest::VENUE,
                position_idx = %raw,
                "unknown positionIdx — reading it as the one-way BOTH bucket"
            );
            PositionSide::Both
        }
    }
}

fn liquidation_event(item: &Value, venue: &str, symbol: &str) -> Event {
    Event::PositionLiquidated(PositionLiquidated {
        venue: venue.to_string().into(),
        symbol: symbol_or(item, symbol).into(),
        position_side: pside_from_index(item),
        qty: f(item, "execQty"),
        liquidation_price: f(item, "execPrice"),
        // execFee on a non-Trade (BustTrade/AdlTrade) row is fee-POSITIVE (a cost);
        // apply_liquidation does balance -= fee. NEVER abs(), NEVER negate.
        fee: fee_f(item, "execFee", venue),
        timestamp: i(item, "execTime"),
        trade_id: s(item, "execId").into(),
        // Stamped at the START, never here — a bridge holds one credential set and knows nothing
        // about accounts. See `vike_model::events::PositionLiquidated::route_key`.
        route_key: None,
        fee_currency: None,
    })
}

/// Perp dispatch: liquidation execTypes → PositionLiquidated ONLY; Trade fills enriched
/// with markPrice + position_side (wrap.fill stays identical to the bare fill).
pub fn map_bybit_perp(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    let Some(topic) = frame.get("topic").and_then(|t| t.as_str()) else {
        return Vec::new();
    };
    let no_rows = vec![];
    let data = frame.get("data").and_then(|d| d.as_array()).unwrap_or(&no_rows);
    let mut events = Vec::new();
    match topic {
        // Fast-fill hint (opt-in): the low-latency `execution.fast` twin → early bare Fill only.
        // No exec-type/liquidation branch — the fast stream is fills-only (bare Fill, mark/pside
        // absent so None/BOTH); the slow `execution` twin below still carries the enriched terminal.
        t if is_fast_execution_topic(t) => {
            for item in data {
                events.extend(map_execution_fast(item, venue, symbol));
            }
        }
        "execution" => {
            for item in data {
                let et = item.get("execType").and_then(|e| e.as_str());
                if matches!(et, Some("BustTrade") | Some("AdlTrade")) {
                    events.push(liquidation_event(item, venue, symbol));
                    continue; // liquidation -> PositionLiquidated ONLY
                }
                let pside = pside_from_index(item);
                let mark = match item.get("markPrice") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(x)) if x.is_empty() => None,
                    Some(Value::String(x)) => x.parse::<f64>().ok(),
                    Some(Value::Number(n)) => n.as_f64(),
                    Some(_) => None,
                };
                for mut event in map_execution(item, venue, symbol) {
                    // the fill a bare Fill or a wrap carries; every other event is not this tap's business
                    if let Some(fe) = event.fill_mut() {
                        fe.mark_price = mark;
                        fe.position_side = pside;
                    }
                    events.push(event);
                }
            }
        }
        "order" => {
            for item in data {
                events.extend(map_order(item, venue, symbol));
            }
        }
        "wallet" => events.extend(wallet_account_state(frame, venue)),
        _ => {}
    }
    events
}

#[cfg(test)]
mod spot_fee_grid {
    use super::SpotNet;

    /// The grid every spot venue walks (`vike_bridge_core::spot_fee_pins`), by this venue's rule.
    #[test]
    fn spot_fee_grid_is_the_pinned_one() {
        vike_bridge_core::spot_fee_pins::assert_spot_fee_grid(
            include_str!("../../tests/fixtures/spot_fee_grid.txt"),
            |side, qty, px, fee, asset| match SpotNet::of(
                "HYPEUSDC", true, side, asset, fee, qty, px,
            ) {
                Some(net) => (net.qty, net.commission, net.asset),
                None => (qty, fee, asset.to_string()),
            },
        );
    }
}
