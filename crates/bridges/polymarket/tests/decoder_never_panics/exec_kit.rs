//! The exec-plane kit of the Polymarket decoder harness: user-channel, recon, chain-log strategies.

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_polymarket::PolymarketRegistry;
use vike_polymarket::exec_plane::settlement::chain::{
    TOPIC_CONDITION_RESOLUTION, TOPIC_CTF_PAYOUT_REDEMPTION, TOPIC_CTF_POSITION_MERGE,
    TOPIC_CTF_POSITION_SPLIT, TOPIC_NEG_RISK_PAYOUT_REDEMPTION, TOPIC_NEG_RISK_POSITIONS_CONVERTED,
    TOPIC_ORDER_FILLED_V1, TOPIC_ORDER_FILLED_V2, TOPIC_TRANSFER_BATCH, TOPIC_TRANSFER_SINGLE,
    TOPIC_USDC_TRANSFER,
};

use crate::kit::{
    arb_asset, arb_f64, arb_json, arb_leaf, arb_numeric_string, arb_side, arb_ts, arb_wire_num,
};

pub(super) const USER_KEYS: &[&str] = &[
    "event_type",
    "status",
    "id",
    "asset_id",
    "taker_order_id",
    "maker_orders",
    "order_id",
    "matched_amount",
    "size",
    "price",
    "timestamp",
    "type",
    "original_size",
    "size_matched",
    "side",
];
pub(super) const RECON_KEYS: &[&str] = &[
    "data",
    "id",
    "asset_id",
    "asset",
    "side",
    "order_type",
    "original_size",
    "size_matched",
    "status",
    "created_at",
    "taker_order_id",
    "maker_orders",
    "order_id",
    "matched_amount",
    "price",
    "size",
    "match_time",
    "timestamp",
    "last_update",
    "avgPrice",
    "balance",
    "conditionId",
    "redeemable",
    "negativeRisk",
    "outcomeIndex",
    "title",
    "curPrice",
];
pub(super) const LOG_KEYS: &[&str] =
    &["topics", "data", "transactionHash", "blockNumber", "blockTimestamp"];
pub(super) const SMALL_KEYS: &[&str] = &["scoring", "0xabc", "0xdef"];
/// (coid, CLOB id) pairs the sequences accept/remove/register; the last one has an EMPTY CLOB
/// id (the registry accepts it and the decoders must cope).
pub(super) const SLOTS: [(&str, &str); 4] =
    [("coid-T", "0xT"), ("coid-M", "0xM"), ("coid-U", "0xU"), ("coid-E", "")];

// ---- user channel, history, reconcile -------------------------------------------------------

fn arb_clob_id() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => prop::sample::select(vec!["0xT", "0xM", "0xU", "0xFOREIGN"]).prop_map(|s| json!(s)),
        1 => Just(json!("")),
        1 => arb_leaf(),
    ]
    .boxed()
}

pub(super) fn arb_user_trade() -> impl Strategy<Value = Value> {
    (
        prop_oneof![
            6 => prop::sample::select(vec!["MATCHED", "MINED", "CONFIRMED", "RETRYING", "FAILED"])
                .prop_map(|s| json!(s)),
            1 => arb_leaf(),
        ],
        prop_oneof![
            3 => prop::sample::select(vec!["t1", "t2", "t3"]).prop_map(|s| json!(s)),
            1 => Just(json!("")),
            1 => arb_leaf(),
        ],
        arb_asset(),
        arb_clob_id(),
        prop::collection::vec((arb_clob_id(), arb_wire_num(), arb_wire_num()), 0..4),
        (arb_wire_num(), arb_wire_num(), prop::option::of(arb_ts())),
    )
        .prop_map(|(status, id, asset, taker, makers, (size, price, ts))| {
            let makers: Vec<Value> = makers
                .into_iter()
                .map(|(o, a, p)| json!({ "order_id": o, "matched_amount": a, "price": p }))
                .collect();
            let mut v = json!({
                "event_type": "trade", "status": status, "id": id, "asset_id": asset,
                "taker_order_id": taker, "maker_orders": makers, "size": size, "price": price
            });
            if let Some(ts) = ts {
                v["timestamp"] = ts;
            }
            v
        })
}

fn arb_user_order() -> impl Strategy<Value = Value> {
    (
        prop_oneof![
            6 => prop::sample::select(vec!["PLACEMENT", "UPDATE", "CANCELLATION"])
                .prop_map(|s| json!(s)),
            1 => arb_leaf(),
        ],
        arb_clob_id(),
        arb_asset(),
        arb_wire_num(),
        arb_wire_num(),
    )
        .prop_map(|(ty, id, asset, original, matched)| {
            json!({
                "event_type": "order", "type": ty, "id": id, "asset_id": asset,
                "original_size": original, "size_matched": matched
            })
        })
}

fn arb_user_event() -> BoxedStrategy<Value> {
    prop_oneof![
        4 => arb_user_trade(),
        3 => arb_user_order(),
        1 => arb_json(USER_KEYS),
    ]
    .boxed()
}

/// A user-channel frame: one event, or an array of them.
fn arb_user_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => arb_user_event(),
        2 => prop::collection::vec(arb_user_event(), 0..4).prop_map(Value::Array),
    ]
    .boxed()
}

#[derive(Debug, Clone)]
pub(super) enum UserOp {
    Accept { slot: usize, side: i32 },
    Remove { slot: usize },
    Register { slot: usize, qty: f64 },
    Frame(Value),
    History { trades: Value, orders: Value },
}

pub(super) fn arb_user_op() -> impl Strategy<Value = UserOp> {
    prop_oneof![
        2 => (0usize..4, prop_oneof![Just(1i32), Just(-1i32), Just(0i32), any::<i32>()])
            .prop_map(|(slot, side)| UserOp::Accept { slot, side }),
        1 => (0usize..4).prop_map(|slot| UserOp::Remove { slot }),
        2 => (0usize..4, arb_f64()).prop_map(|(slot, qty)| UserOp::Register { slot, qty }),
        6 => arb_user_frame().prop_map(UserOp::Frame),
        1 => (
            prop::collection::vec(arb_user_trade(), 0..3),
            prop::collection::vec(arb_user_order(), 0..3),
        )
            .prop_map(|(trades, orders)| UserOp::History {
                trades: Value::Array(trades),
                orders: Value::Array(orders),
            }),
    ]
}

pub(super) fn arb_recon_order() -> impl Strategy<Value = Value> {
    (
        arb_clob_id(),
        arb_asset(),
        arb_side(),
        arb_leaf(),
        arb_wire_num(),
        arb_wire_num(),
        arb_leaf(),
        arb_wire_num(),
    )
        .prop_map(|(id, asset, side, kind, original, matched, status, created)| {
            json!({
                "id": id, "asset_id": asset, "side": side, "order_type": kind,
                "original_size": original, "size_matched": matched, "status": status,
                "created_at": created
            })
        })
}

pub(super) fn arb_recon_position() -> impl Strategy<Value = Value> {
    (arb_asset(), arb_wire_num(), arb_wire_num(), arb_leaf(), arb_leaf(), arb_leaf()).prop_map(
        |(asset, size, avg, cond, redeemable, outcome_index)| {
            json!({
                "asset": asset, "size": size, "avgPrice": avg, "conditionId": cond,
                "redeemable": redeemable, "outcomeIndex": outcome_index,
                "negativeRisk": false, "title": "t", "curPrice": 0.5
            })
        },
    )
}

/// Rows as a bare array or wrapped under `data`, the two list shapes the CLOB uses.
pub fn arb_rows(row: impl Strategy<Value = Value> + 'static) -> impl Strategy<Value = Value> {
    (prop::collection::vec(row, 0..4), any::<bool>()).prop_map(|(rows, wrapped)| {
        if wrapped { json!({ "data": rows }) } else { Value::Array(rows) }
    })
}

pub(super) fn seeded_registry() -> PolymarketRegistry {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("coid-T", "0xT", 1);
    let _ = reg.on_accept("coid-M", "0xM", -1);
    reg
}

// ---- Polygon logs -----------------------------------------------------------------------------

pub(super) fn hex_word(v: u128) -> String {
    format!("{v:064x}")
}

/// One 32-byte ABI word as the RPC spells it: ordinary values, the awkward ones (array offsets,
/// `u64::MAX` and `u128::MAX` lengths, high-half-set values), random hex, and 64-BYTE strings
/// carrying a multi-byte character at a position chosen to straddle the decoders' byte slices
/// (`[..32]` in `word_u128`, `[24..]` in `word_address`).
pub(super) fn arb_word() -> BoxedStrategy<String> {
    prop_oneof![
        4 => prop::sample::select(vec![
            0u128,
            1,
            2,
            32,
            64,
            96,
            128,
            1_000_000,
            u128::from(u64::MAX),
            u128::MAX,
            1u128 << 64,
            1u128 << 127,
        ])
        .prop_map(hex_word),
        2 => "[0-9a-f]{64}",
        1 => "[0-9a-fA-F]{0,70}",
        1 => prop_oneof![Just(31usize), Just(23usize), Just(0usize), 0usize..=62]
            .prop_map(|p| format!("{}é{}", "0".repeat(p), "0".repeat(62 - p))),
        1 => prop_oneof![
            Just(30usize), Just(31usize), Just(22usize), Just(23usize), 0usize..=61
        ]
        .prop_map(|p| format!("{}€{}", "0".repeat(p), "0".repeat(61 - p))),
    ]
    .boxed()
}

/// A log topic: a 32-byte word with `0x`, a bare word, or noise.
fn arb_topic_word() -> BoxedStrategy<String> {
    prop_oneof![
        4 => "0x[0-9a-f]{64}",
        2 => arb_word().prop_map(|w| format!("0x{w}")),
        1 => any::<String>(),
    ]
    .boxed()
}

fn arb_quantity() -> BoxedStrategy<Value> {
    prop_oneof![
        2 => Just(json!("0x10")),
        2 => Just(json!("0xffffffffffffffff")),
        2 => Just(json!("0x7fffffffffffffff")),
        1 => Just(json!("0x0")),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A log shaped like `eth_getLogs` output carrying one of the eleven event topics the decoders
/// dispatch on.
pub(super) fn arb_log() -> impl Strategy<Value = Value> {
    (
        prop::sample::select(vec![
            TOPIC_CONDITION_RESOLUTION,
            TOPIC_CTF_PAYOUT_REDEMPTION,
            TOPIC_NEG_RISK_PAYOUT_REDEMPTION,
            TOPIC_TRANSFER_SINGLE,
            TOPIC_TRANSFER_BATCH,
            TOPIC_ORDER_FILLED_V2,
            TOPIC_ORDER_FILLED_V1,
            TOPIC_CTF_POSITION_SPLIT,
            TOPIC_CTF_POSITION_MERGE,
            TOPIC_NEG_RISK_POSITIONS_CONVERTED,
            TOPIC_USDC_TRANSFER,
        ]),
        prop::collection::vec(arb_topic_word(), 0..4),
        prop::collection::vec(arb_word(), 0..8),
        prop_oneof![Just(String::new()), "[0-9a-f]{1,40}"],
        (arb_quantity(), arb_quantity()),
    )
        .prop_map(|(t0, topics, words, tail, (block, block_ts))| {
            let mut all = vec![json!(t0)];
            all.extend(topics.into_iter().map(Value::String));
            json!({
                "topics": all,
                "data": format!("0x{}{tail}", words.concat()),
                "transactionHash": "0xabc",
                "blockNumber": block,
                "blockTimestamp": block_ts
            })
        })
}

pub(super) fn arb_u128() -> impl Strategy<Value = u128> {
    prop_oneof![
        Just(0u128),
        Just(1u128),
        Just(1_000_000u128),
        Just(u128::from(u64::MAX)),
        Just(u128::MAX),
        any::<u128>(),
    ]
}

pub(super) fn arb_header_name() -> BoxedStrategy<String> {
    prop_oneof![
        4 => prop::sample::select(vec![
            "Poly-RateLimit-Warning",
            "poly-ratelimit-order-remaining",
            "Poly-RateLimit-Cancel-Remaining",
            "POLY-RATELIMIT-RESET",
        ])
        .prop_map(|s| s.to_string()),
        1 => any::<String>(),
    ]
    .boxed()
}

pub(super) fn arb_header_value() -> impl Strategy<Value = String> {
    prop_oneof![arb_numeric_string(), any::<String>(), Just("false".to_string())]
}

/// A syntactically valid CTF `PayoutRedemption` log whose data is `[conditionId, 0x60, payout,
/// ..tail]`, for the reproducers below.
pub(super) fn redemption_log(tail: &[String], block_timestamp: &str) -> Value {
    let topic = format!("0x{}", "0".repeat(64));
    let mut words = vec![hex_word(0xabc), hex_word(0x60), hex_word(0)];
    words.extend(tail.iter().cloned());
    json!({
        "topics": [TOPIC_CTF_PAYOUT_REDEMPTION, topic],
        "data": format!("0x{}", words.concat()),
        "transactionHash": "0xabc",
        "blockNumber": "0x1",
        "blockTimestamp": block_timestamp
    })
}
