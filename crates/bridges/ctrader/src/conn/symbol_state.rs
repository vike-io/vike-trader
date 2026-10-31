//! The broker changed a symbol: read its trading state again, for every holder of the exec client's symbol map.
//!
//! `ProtoOASymbolChangedEvent` names symbol ids and nothing else. The inbound dispatcher
//! (`super::inbound::on_inbound`) notes the ids on the connection's map ([`SymbolMap::note_changed`]), and between two
//! frames the actor loop takes them ([`SymbolMap::take_changed`]) and calls [`refresh_symbol_state`]: ONE
//! `ProtoOASymbolsListReq` (the light list, where `enabled` lives) and ONE `ProtoOASymbolByIdReq` for the changed ids
//! (the full records, where `tradingMode` lives), merged over the old table by [`trading_state::refreshed`] and
//! written into the map the EXEC CLIENT holds, so its next order is judged on what the broker says now
//! (`crate::exec::CtraderExec`'s `symbol_state_refusal`). A reconnect writes the table afresh from its own handshake
//! (`super::actor::actor_loop`).
//!
//! ⚠ **Only the trading state is refreshed.** A changed digits, volume grid, pip or name is not applied to the map the
//! exec client holds (it keeps the handshake's), and the actor's own map is replaced only by a reconnect: the symbol
//! universe of a live account is reference data in practice, and a change of its price scale would need every holder
//! rebuilt at once. ⚠ **NOT measured live**: a demo account cannot be made to change a symbol, so whether and when the
//! broker sends the event unasked is read from the proto and played by the scripted venue
//! (`tests/symbol_state.rs`) only. ⚠ **A refresh that fails** (the venue refuses, the answer is late, the socket
//! dies) keeps the table as it was and says so at `warn`; the next event or reconnect reads it afresh. The two
//! round trips run on the actor thread under [`RECONCILE_TIMEOUT`] each, and every other frame that arrives meanwhile
//! is buffered and dispatched afterwards, never dropped (the contract of [`read_reply`]).

use prost::Message;

use vike_data::LiveDataSink;
use vike_exec::EventSender;

use super::inbound::on_inbound;
use super::reconcile::{Reply, read_reply};
use super::{CloseState, OrderIdMap, PositionMap, RECONCILE_TIMEOUT, Reader, send};
use crate::proto::{
    ProtoMessage, ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaSymbolByIdReq, ProtoOaSymbolByIdRes,
    ProtoOaSymbolsListReq, ProtoOaSymbolsListRes, pt,
};
use crate::symbols::SymbolMap;
use crate::trading_state;

/// Read the trading state of `changed` again on the actor's socket and write it into `shared`, the map the exec
/// client holds. `symbols` is the actor's own map (it decodes the frames buffered meanwhile).
#[expect(clippy::too_many_arguments)]
pub(super) fn refresh_symbol_state(
    changed: &[i64],
    reader: &mut Reader,
    ctid: i64,
    shared: &SymbolMap,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    match read_symbol_state(reader, ctid, changed, &mut buffered) {
        Some((light, full)) => {
            let before = shared.restriction_table();
            let after = trading_state::refreshed(&before, changed, &light, &full);
            for id in changed {
                let (was, now) = (before.get(id).copied(), after.get(id).copied());
                tracing::info!(
                    target: "ctrader",
                    symbol_id = *id,
                    symbol = shared.name_of(*id).unwrap_or("?"),
                    was = was.map_or("tradable", |r| r.wire_word()),
                    now = now.map_or("tradable", |r| r.wire_word()),
                    "the broker changed a symbol: its trading state was read again"
                );
            }
            shared.set_restrictions(after);
        }
        None => tracing::warn!(
            target: "ctrader",
            symbols = changed.len(),
            "the broker changed a symbol and its state could not be read again: the exec client keeps the state \
             it had until the next change or reconnect"
        ),
    }
    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
}

/// The light list and the full records of `changed`, or `None` when either round trip failed. EMPTY clientMsgId on
/// both requests, for the reason `super::reconcile::reconcile_after_reconnect` spells out: an `ERROR_RES` echoing a
/// non-empty id would be correlated with an order.
fn read_symbol_state(
    reader: &mut Reader,
    ctid: i64,
    changed: &[i64],
    buffered: &mut Vec<ProtoMessage>,
) -> Option<(Vec<ProtoOaLightSymbol>, Vec<ProtoOaSymbol>)> {
    let list_req = ProtoOaSymbolsListReq {
        ctid_trader_account_id: ctid,
        include_archived_symbols: Some(false),
        ..Default::default()
    };
    send(reader, pt::SYMBOLS_LIST_REQ, &list_req, "").ok()?;
    let Reply::Got(msg) = read_reply(reader, buffered, RECONCILE_TIMEOUT, pt::SYMBOLS_LIST_RES)
    else {
        return None;
    };
    let list = ProtoOaSymbolsListRes::decode(msg.payload.as_deref().unwrap_or(&[])).ok()?;

    let by_id_req = ProtoOaSymbolByIdReq {
        ctid_trader_account_id: ctid,
        symbol_id: changed.to_vec(),
        ..Default::default()
    };
    send(reader, pt::SYMBOL_BY_ID_REQ, &by_id_req, "").ok()?;
    let Reply::Got(msg) = read_reply(reader, buffered, RECONCILE_TIMEOUT, pt::SYMBOL_BY_ID_RES)
    else {
        return None;
    };
    let full = ProtoOaSymbolByIdRes::decode(msg.payload.as_deref().unwrap_or(&[])).ok()?;
    Some((list.symbol, full.symbol))
}
