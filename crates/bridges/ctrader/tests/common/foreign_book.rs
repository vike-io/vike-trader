//! A scripted venue whose account trades MORE THAN ONE symbol: three forex pairs (EURUSD 1, GBPUSD 2,
//! USDJPY 3 on a USD account) and a caller-scripted book of orders, positions and deals across them.
//! It serves the any-symbol reconcile tests (`tests/recon_admitted.rs`): the handshake's symbol list,
//! asset list and trader response, `RECONCILE_RES` and `DEAL_LIST_RES` come from the book; every other
//! request is answered by `serve::script_reply` like any other fake.

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;

use vike_ctrader::framing::{self, FrameReader};
use vike_ctrader::proto::{
    ProtoMessage, ProtoOaAsset, ProtoOaAssetListRes, ProtoOaDeal, ProtoOaDealListReq,
    ProtoOaDealListRes, ProtoOaDealStatus, ProtoOaLightSymbol, ProtoOaOrder, ProtoOaOrderStatus,
    ProtoOaOrderType, ProtoOaPosition, ProtoOaReconcileRes, ProtoOaSymbol, ProtoOaSymbolByIdRes,
    ProtoOaSymbolsListRes, ProtoOaTradeData, ProtoOaTradeSide, ProtoOaTrader, ProtoOaTraderRes, pt,
};

use super::fake::AccountsScript;
use super::serve::{EofStream, script_reply};

/// The account's deposit asset id (USD) the fake's trader response carries.
pub const DEPOSIT_USD: i64 = 16;
const EUR: i64 = 5;
const GBP: i64 = 6;
const JPY: i64 = 7;

/// `(symbol_id, name, base asset, quote asset, digits)` of the three pairs.
const PAIRS: [(i64, &str, i64, i64, i32); 3] = [
    (1, "EURUSD", EUR, DEPOSIT_USD, 5),
    (2, "GBPUSD", GBP, DEPOSIT_USD, 5),
    (3, "USDJPY", DEPOSIT_USD, JPY, 3),
];

/// What the venue holds, across the three pairs.
#[derive(Clone, Default)]
pub struct ForeignBook {
    /// `(symbol_id, order_id, coid)` of each RESTING limit order (buy, 1.00 unit).
    pub orders: Vec<(i64, i64, &'static str)>,
    /// `(symbol_id, position_id, volume_centi)` of each OPEN long position.
    pub positions: Vec<(i64, i64, i64)>,
    /// `(symbol_id, deal_id, filled_volume_centi)` of each FILLED buy deal.
    pub deals: Vec<(i64, i64, i64)>,
    /// What `DEAL_LIST_RES.has_more` says.
    pub deals_have_more: bool,
    /// The most deals one reply carries (`0` = no cap); a reply cut short says `has_more`. The window of
    /// the request (`from_timestamp`..`to_timestamp`, inclusive) is honoured either way.
    pub deals_page: usize,
    /// Whether a reply lists the NEWEST deals first (the cap then drops the oldest).
    pub deals_newest_first: bool,
    /// Pairs the venue's symbol list leaves out (by id): "the broker dropped it".
    pub omit_symbols: Vec<i64>,
    /// `ProtoOATrader.balance` as the venue states it: an integer scaled by the fake's two money digits
    /// (`1_000_067` is 10,000.67). `0` = the default account.
    pub balance: i64,
    /// The trader's `deposit_asset_id` when it is not the USD one (`5` is EUR; an id the asset list does
    /// not carry is a deposit currency nobody can name).
    pub deposit_asset: Option<i64>,
    /// The trader's `account_type` (`ProtoOaAccountType` as `i32`); `None` = the venue states none.
    pub account_type: Option<i32>,
}

fn order(symbol_id: i64, order_id: i64, coid: &str) -> ProtoOaOrder {
    ProtoOaOrder {
        order_id,
        trade_data: ProtoOaTradeData {
            symbol_id,
            volume: 100,
            trade_side: ProtoOaTradeSide::Buy as i32,
            label: Some(coid.to_string()),
            ..Default::default()
        },
        order_type: ProtoOaOrderType::Limit as i32,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        client_order_id: Some(coid.to_string()),
        limit_price: Some(1.10),
        ..Default::default()
    }
}

fn position(symbol_id: i64, position_id: i64, volume: i64) -> ProtoOaPosition {
    ProtoOaPosition {
        position_id,
        trade_data: ProtoOaTradeData {
            symbol_id,
            volume,
            trade_side: ProtoOaTradeSide::Buy as i32,
            ..Default::default()
        },
        price: Some(1.2),
        ..Default::default()
    }
}

fn deal(symbol_id: i64, deal_id: i64, volume: i64) -> ProtoOaDeal {
    ProtoOaDeal {
        deal_id,
        order_id: 9_000 + deal_id,
        symbol_id,
        filled_volume: volume,
        trade_side: ProtoOaTradeSide::Buy as i32,
        deal_status: ProtoOaDealStatus::Filled as i32,
        execution_price: Some(1.3),
        execution_timestamp: 1_700_000_000_000 + deal_id,
        ..Default::default()
    }
}

/// The frames this fake answers `msg` with: the book's own for the symbol-universe, trader, reconcile and
/// deal-list requests, the shared script's for the rest.
fn reply(msg: &ProtoMessage, book: &ForeignBook) -> Vec<(u32, Vec<u8>)> {
    let kept = |id: i64| !book.omit_symbols.contains(&id);
    match msg.payload_type {
        pt::TRADER_REQ => {
            let res = ProtoOaTraderRes {
                ctid_trader_account_id: 99,
                trader: ProtoOaTrader {
                    ctid_trader_account_id: 99,
                    money_digits: Some(2),
                    deposit_asset_id: book.deposit_asset.unwrap_or(DEPOSIT_USD),
                    balance: book.balance,
                    account_type: book.account_type,
                    ..Default::default()
                },
                ..Default::default()
            };
            vec![(pt::TRADER_RES, res.encode_to_vec())]
        }
        pt::ASSET_LIST_REQ => {
            let asset = |asset_id, name: &str| ProtoOaAsset {
                asset_id,
                name: name.to_string(),
                ..Default::default()
            };
            let res = ProtoOaAssetListRes {
                ctid_trader_account_id: 99,
                asset: vec![
                    asset(EUR, "EUR"),
                    asset(GBP, "GBP"),
                    asset(JPY, "JPY"),
                    asset(DEPOSIT_USD, "USD"),
                ],
                ..Default::default()
            };
            vec![(pt::ASSET_LIST_RES, res.encode_to_vec())]
        }
        pt::SYMBOLS_LIST_REQ => {
            let res = ProtoOaSymbolsListRes {
                ctid_trader_account_id: 99,
                symbol: PAIRS
                    .iter()
                    .filter(|p| kept(p.0))
                    .map(|&(symbol_id, name, base, quote, _)| ProtoOaLightSymbol {
                        symbol_id,
                        symbol_name: Some(name.to_string()),
                        enabled: Some(true),
                        symbol_category_id: Some(1),
                        base_asset_id: Some(base),
                        quote_asset_id: Some(quote),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            vec![(pt::SYMBOLS_LIST_RES, res.encode_to_vec())]
        }
        pt::SYMBOL_BY_ID_REQ => {
            let res = ProtoOaSymbolByIdRes {
                ctid_trader_account_id: 99,
                symbol: PAIRS
                    .iter()
                    .filter(|p| kept(p.0))
                    .map(|&(symbol_id, _, _, _, digits)| ProtoOaSymbol {
                        symbol_id,
                        digits,
                        pip_position: digits - 1,
                        lot_size: Some(10_000_000),
                        min_volume: Some(100_000),
                        step_volume: Some(100_000),
                        max_volume: Some(10_000_000_000),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            vec![(pt::SYMBOL_BY_ID_RES, res.encode_to_vec())]
        }
        pt::RECONCILE_REQ => {
            let res = ProtoOaReconcileRes {
                ctid_trader_account_id: 99,
                order: book.orders.iter().map(|&(s, id, coid)| order(s, id, coid)).collect(),
                position: book.positions.iter().map(|&(s, id, v)| position(s, id, v)).collect(),
                ..Default::default()
            };
            vec![(pt::RECONCILE_RES, res.encode_to_vec())]
        }
        pt::DEAL_LIST_REQ => {
            let asked = ProtoOaDealListReq::decode(msg.payload.as_deref().unwrap_or(&[]))
                .expect("a deal list request");
            // The window [from, to] (both ends inclusive, as the venue reads them), in the book's order.
            let mut deals: Vec<ProtoOaDeal> = book
                .deals
                .iter()
                .map(|&(s, id, v)| deal(s, id, v))
                .filter(|d| asked.from_timestamp.is_none_or(|f| d.execution_timestamp >= f))
                .filter(|d| asked.to_timestamp.is_none_or(|t| d.execution_timestamp <= t))
                .collect();
            deals.sort_by_key(|d| d.execution_timestamp);
            if book.deals_newest_first {
                deals.reverse();
            }
            let mut has_more = book.deals_have_more;
            if book.deals_page > 0 && deals.len() > book.deals_page {
                deals.truncate(book.deals_page);
                has_more = true;
            }
            let res = ProtoOaDealListRes {
                ctid_trader_account_id: 99,
                deal: deals,
                has_more,
                ..Default::default()
            };
            vec![(pt::DEAL_LIST_RES, res.encode_to_vec())]
        }
        _ => script_reply(msg, AccountsScript::OneDemo, false),
    }
}

/// Serve one connection until the client closes: record each request type, answer it from `book`.
pub(super) fn serve_foreign_book(stream: TcpStream, seen: Arc<Mutex<Vec<u32>>>, book: ForeignBook) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut writer = stream.try_clone().expect("clone fake stream");
    let (eof_stream, eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);
    loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                seen.lock().expect("seen lock").push(msg.payload_type);
                for (reply_type, body) in reply(&msg, &book) {
                    let frame = framing::encode(reply_type, &body, "srv");
                    if writer.write_all(&frame).is_err() {
                        return;
                    }
                    let _ = writer.flush();
                }
            }
            Ok(None) => {
                if eof.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}
