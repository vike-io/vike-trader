//! The exec-plane kit of the OKX decoder harness: private / REST row shapes, drivers, a canned transport.

use crate::kit::{
    INST, INSTS, arb_json, arb_leaf, array_of, idv, inst_row, junk_or, numv, obj_of, pick,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::Value;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_model::SymbolProperties;
use vike_okx::perp::{OkxPerpRest, parse_okx_perp_instruments};
use vike_okx::transport::{OkxTransport, unwrap_okx};
use vike_okx::{account_config, event_mapper, funding, history, recon_client, ws_auth};
use vike_secrets::credentials::Credentials;

pub const PRIVATE_CHANNELS: &[&str] = &["orders", "orders", "account", "positions", ""];
pub const PRIVATE_KEYS: &[&str] = &[
    "event",
    "arg",
    "data",
    "channel",
    "code",
    "msg",
    "state",
    "clOrdId",
    "ordId",
    "fillTime",
    "uTime",
    "posSide",
    "fillSz",
    "tradeId",
    "instId",
    "side",
    "fillPx",
    "fillFee",
    "fillFeeCcy",
    "execType",
    "accFillSz",
    "sz",
    "category",
    "px",
    "fillMarkPx",
    "markPx",
    "cancelSource",
    "details",
    "ccy",
    "cashBal",
];
pub const ALL_KEYS: &[&str] = &[
    "data",
    "code",
    "msg",
    "instId",
    "ordId",
    "clOrdId",
    "tradeId",
    "side",
    "ordType",
    "sz",
    "px",
    "accFillSz",
    "avgPx",
    "state",
    "uTime",
    "ts",
    "fillSz",
    "fillPx",
    "fee",
    "feeCcy",
    "execType",
    "pos",
    "posSide",
    "mgnMode",
    "margin",
    "markPx",
    "details",
    "ccy",
    "cashBal",
    "makerU",
    "takerU",
    "maker",
    "taker",
    "type",
    "pnl",
    "balChg",
    "billId",
    "uid",
    "mainUid",
    "sCode",
    "sMsg",
    "event",
    "last",
    "instType",
    "tickSz",
    "lotSz",
    "minSz",
    "maxMktSz",
    "ctVal",
    "ctMult",
    "ctValCcy",
    "lever",
];

/// One `orders` channel / `orders-pending` row.
pub fn order_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("code", pick(&["0", "0", "0", "51000", ""])),
        ("msg", arb_leaf()),
        ("state", pick(&["live", "canceled", "mmp_canceled", "filled", "partially_filled", ""])),
        ("clOrdId", idv()),
        ("ordId", idv()),
        ("ordType", pick(&["limit", "market", "post_only", "LIMIT", ""])),
        ("fillTime", numv()),
        ("uTime", numv()),
        ("posSide", pick(&["long", "short", "net", ""])),
        ("fillSz", numv()),
        ("tradeId", idv()),
        ("instId", pick(INSTS)),
        ("side", pick(&["buy", "sell", "BUY", ""])),
        ("fillPx", numv()),
        ("fillFee", numv()),
        ("fillFeeCcy", pick(&["USDT", "BTC", ""])),
        ("execType", pick(&["M", "T", ""])),
        ("accFillSz", numv()),
        ("sz", numv()),
        ("px", numv()),
        ("avgPx", numv()),
        ("category", pick(&["normal", "full_liquidation", "partial_liquidation", "adl"])),
        ("fillMarkPx", numv()),
        ("markPx", numv()),
        ("cancelSource", arb_leaf()),
    ])
}

/// One `fills` / `fills-history` row.
pub fn fill_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("tradeId", idv()),
        ("ordId", idv()),
        ("clOrdId", idv()),
        ("instId", pick(INSTS)),
        ("side", pick(&["buy", "sell", ""])),
        ("fillSz", numv()),
        ("fillPx", numv()),
        ("fee", numv()),
        ("feeCcy", pick(&["USDT", ""])),
        ("execType", pick(&["M", "T", ""])),
        ("fillMarkPx", numv()),
        ("ts", numv()),
    ])
}

/// One `account/positions` row.
fn position_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instId", pick(INSTS)),
        ("posSide", pick(&["long", "short", "net", ""])),
        ("pos", numv()),
        ("avgPx", numv()),
        ("markPx", numv()),
        ("uTime", numv()),
        ("mgnMode", pick(&["isolated", "cross", "ISOLATED", ""])),
        ("margin", numv()),
    ])
}

/// One `account/balance` / `account` channel entry.
pub fn account_row() -> BoxedStrategy<Value> {
    let detail = obj_of(vec![("ccy", pick(&["USDT", "BTC", ""])), ("cashBal", numv())]);
    obj_of(vec![("uTime", numv()), ("details", junk_or(array_of(detail, 4)))])
}

/// One account-bill row (funding is `type` "8").
pub fn bill_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("type", pick(&["8", "2", ""])),
        ("pnl", numv()),
        ("balChg", numv()),
        ("ts", numv()),
        ("billId", idv()),
        ("instId", pick(INSTS)),
    ])
}

/// One `trade-fee` row, one `account/config` row, one per-order result row.
pub fn misc_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("makerU", numv()),
        ("takerU", numv()),
        ("maker", numv()),
        ("taker", numv()),
        ("uid", pick(&["111", " ", ""])),
        ("mainUid", pick(&["111", " ", ""])),
        ("sCode", pick(&["0", "51000", ""])),
        ("sMsg", arb_leaf()),
        ("last", numv()),
    ])
}

pub fn any_row() -> BoxedStrategy<Value> {
    prop_oneof![
        order_row(),
        fill_row(),
        position_row(),
        account_row(),
        bill_row(),
        misc_row(),
        inst_row(),
        arb_json(ALL_KEYS),
    ]
    .boxed()
}

pub fn rows(max: usize) -> BoxedStrategy<Value> {
    array_of(any_row(), max)
}

pub fn ct_val() -> BoxedStrategy<f64> {
    prop_oneof![Just(0.01), Just(1.0), Just(0.0), Just(-1.0), any::<f64>()].boxed()
}

/// The three private-WS dispatchers on one frame; no event flood (a row yields <= 2 events,
/// the account arm <= 1).
pub fn check_private(frame: &Value, ct: f64) -> Result<(), TestCaseError> {
    let n_rows = frame.get("data").and_then(Value::as_array).map_or(0, Vec::len);
    let bound = 2 * n_rows + 1;
    let spot = event_mapper::map_okx_private(frame, "okx", INST);
    prop_assert!(spot.len() <= bound, "spot flood: {} events from {n_rows} rows", spot.len());
    let perp = event_mapper::map_okx_perp(frame, "okx", INST, ct);
    prop_assert!(perp.len() <= bound, "perp flood: {} events from {n_rows} rows", perp.len());
    let one = event_mapper::map_okx_order(frame, "okx", INST);
    prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
    Ok(())
}

/// Every REST-body text decoder of the reconcile client on one body.
pub fn drive_recon_text(body: &str, ct: f64) {
    let _ = recon_client::parse_orders_pending(body, ct);
    let _ = recon_client::parse_fills(body, ct);
    let _ = recon_client::parse_positions(body, INST, ct);
    let _ = recon_client::parse_balance(body);
    let _ = recon_client::parse_fee_rate(body);
}

/// Every exec-plane `Value` decoder on one payload.
pub fn drive_exec_value(v: &Value, ct: f64) {
    let _ = recon_client::parse_mgn_mode(v);
    let _ = account_config::parse_account_identity(v);
    let _ = parse_okx_perp_instruments(v);
    let _ = unwrap_okx(v.clone());
    for event in ["login", "subscribe", "error", ""] {
        let _ = ws_auth::match_event_ack(v, event);
    }
    let empty = Value::Null;
    let _ = history::map_okx_history(v, &empty, "okx", INST, ct);
    let _ = history::map_okx_history(&empty, v, "okx", INST, ct);
    let _ = history::map_okx_history(v, v, "okx", INST, ct);
    if let Some(bills) = v.as_array() {
        let _ = funding::decode_okx_funding_bills(bills, "okx", INST);
    }
}

/// Answers every request, signed or public, with the same canned body: drives the decoders
/// INSIDE `OkxPerpRest` (`reconcile_positions`, `fetch_usdt_balance`, `fetch_open_orders`,
/// `last_price`) that no free function exposes.
pub struct Canned(Value);

impl OkxTransport for Canned {
    fn signed(
        &self,
        _base_url: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        Ok(self.0.clone())
    }

    fn public(
        &self,
        _base_url: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        Ok(self.0.clone())
    }
}

pub fn rest_over(body: Value, ct: f64) -> OkxPerpRest<Canned> {
    OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            || 0,
        ),
        transport: Canned(body),
        base_url: "https://unused.invalid".to_string(),
        symbol: INST.to_string(),
        properties: SymbolProperties {
            tick_size: 0.1,
            step_size: 0.01,
            min_qty: 0.01,
            ..Default::default()
        },
        ct_val: ct,
        leverage: 2.0,
        broker_code: None,
    }
}
