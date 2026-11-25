//! `venue_read` — an OPERATOR's read of a Deribit account, asked of the venue now: its cash in each
//! currency, its open orders and its positions (`vike_model::VenueReadKind`; the gatewayhub's
//! `TradeUp::VenueRead` asks it). Three `private/get_*` calls: nothing changes at the venue and nothing
//! is folded.
//!
//! ## Why this is not the reconcile client
//! [`mod@crate::reconcile`] reads by instrument (the mounted one and the admitted ones), so a list through
//! it would call an account with an order resting elsewhere empty. These three calls span the account, as
//! the venue's own API description states (`test.deribit.com/static/deribit_api_v2.json`, read 2026-11-07):
//! `private/get_open_orders` ("across all currencies"), `private/get_positions` with `currency=any` (also
//! measured on the testnet 2026-10-17, `ReconcileClientDeribit::fetch_position_status_reports`) and
//! `private/get_account_summaries` (one summary per currency).
//!
//! ## A wallet per currency
//! Deribit keeps one wallet per currency and states no total for every account, so one figure would read
//! one wallet as the whole account. The balance is each wallet's cash (`balance`, the field the reconcile
//! client reads) with money in it: the first in [`VenueReadBalance::asset`] and the others in
//! [`VenueReadBalance::other_assets`]. The venue lists every currency it has, most of them empty, so an empty
//! wallet is left out; an account with no money at all answers the venue's first wallet at zero.
//!
//! ## One short-lived socket, on the testnet
//! A read opens its own authed order-WS on the testnet socket (`crate::transport::client::DeribitUrls`'s `ws`, the
//! `demo` `trade` `ws` row) and drops it after one call (a fifth socket
//! beside the four the crate page counts, for the length of that call), so it cannot delay an order on the
//! exec socket. Its dial and its reply are bounded by the exec lane's timings. The rows go through the
//! reconcile parsers, so the two reads cannot disagree about a side, a size or a status.
//!
//! ## What is not proven
//! The account-summaries body and a `private/get_open_orders` row are read from the venue's description and
//! from the by-instrument rows of the same shape, not from a capture of these two calls: no deployed
//! gatewayhub has made them yet. An empty position (`size` 0, which the venue keeps listing after a close)
//! is left out, unlike Polymarket's list: it holds nothing.

use serde_json::{Value, json};
use vike_bridge_core::json::json_number;
use vike_model::{
    MAX_VENUE_POSITIONS, SymbolProperties, VenueRead, VenueReadAssetBalance, VenueReadBalance,
    VenueReadKind, VenueReadOpenOrder, VenueReadPosition,
};
use vike_secrets::credentials::Credentials;

use crate::execution::rest::VenueRestDeribit;
use crate::reconcile::{PATH_POSITIONS, parse_open_orders, position_amount};
use crate::refusals::DeribitVenueReadRefusal;
use crate::transport::client::DeribitTransport;

const PATH_OPEN_ORDERS: &str = "private/get_open_orders";
const PATH_ACCOUNT_SUMMARIES: &str = "private/get_account_summaries";

/// The answer to `kind` for the account whose pair is `creds`, on the testnet socket `ws`
/// ([`ExecUrls::ws`](crate::transport::client::DeribitUrls::ws)). `Err` is the operator's sentence.
pub(crate) fn venue_read(
    ws: &str,
    creds: &Credentials,
    kind: VenueReadKind,
) -> Result<VenueRead, DeribitVenueReadRefusal> {
    let mut transport =
        DeribitTransport::new(ws, creds.api_key.expose(), creds.api_secret.expose(), None);
    transport.connect().map_err(|e| DeribitVenueReadRefusal::Unconnected { why: e.to_string() })?;
    read_over(&VenueRestDeribit::new(transport, "", SymbolProperties::default(), ""), kind)
}

/// The read itself, over a connected client: the seam the offline tests drive.
pub fn read_over(
    rest: &VenueRestDeribit,
    kind: VenueReadKind,
) -> Result<VenueRead, DeribitVenueReadRefusal> {
    let ask = |method: &str, params: Value| {
        rest.private_result(method, &params).map_err(|e| DeribitVenueReadRefusal::Unread {
            method: method.to_string(),
            code: e.error().code,
            msg: e.error().msg.clone(),
        })
    };
    match kind {
        VenueReadKind::Balance => {
            balance_from(&ask(PATH_ACCOUNT_SUMMARIES, json!({}))?).map(VenueRead::Balance)
        }
        VenueReadKind::OpenOrders => {
            // `type: all`, as the reconcile read asks: a resting stop is an open order too.
            let body = ask(PATH_OPEN_ORDERS, json!({"type": "all"}))?;
            let (orders, truncated) = open_orders_from(&body)?;
            Ok(VenueRead::OpenOrders { orders, truncated })
        }
        VenueReadKind::Positions => {
            let body = ask(PATH_POSITIONS, json!({"currency": "any"}))?;
            let (positions, truncated) = positions_from(&body)?;
            Ok(VenueRead::Positions { positions, truncated })
        }
    }
}

/// `private/get_account_summaries` → the cash of each wallet with money in it (module doc).
fn balance_from(body: &Value) -> Result<VenueReadBalance, DeribitVenueReadRefusal> {
    let rows = body
        .get("summaries")
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or(DeribitVenueReadRefusal::NoWallet)?;
    let mut wallets = Vec::with_capacity(rows.len());
    for row in rows {
        let asset = row.get("currency").and_then(Value::as_str).unwrap_or("").to_string();
        let balance = row
            .get("balance")
            .and_then(json_number)
            .ok_or_else(|| DeribitVenueReadRefusal::NoBalance { asset: asset.clone() })?;
        wallets.push(VenueReadAssetBalance { asset, balance });
    }
    let first = wallets.iter().position(|w| w.balance != 0.0).unwrap_or(0);
    let shown = wallets.remove(first);
    wallets.retain(|w| w.balance != 0.0);
    Ok(VenueReadBalance {
        asset: shown.asset,
        balance: shown.balance,
        allowances: Vec::new(),
        other_assets: wallets,
    })
}

/// `private/get_open_orders` → the rows with the limit price each rests at (a `market_price` word states
/// none and reads `None`), the kind and a trigger kind's `trigger_price` as the reconcile parser reads
/// them (a `trailing_stop`'s row `trigger_price` is its MOVING stop, not a trigger term, and reads `None`)
/// and the instant it was placed, cut at [`vike_model::MAX_VENUE_OPEN_ORDERS`] with the cut said.
fn open_orders_from(
    body: &Value,
) -> Result<(Vec<VenueReadOpenOrder>, bool), DeribitVenueReadRefusal> {
    let rows =
        body.as_array().ok_or(DeribitVenueReadRefusal::NotAList { answer: "get_open_orders" })?;
    // One report per row, in the rows' order (`parse_open_orders` maps every row), so a row pairs with its order.
    // every row is read by the operator under its USER name (decision 0290)
    let reports = super::user_named(
        parse_open_orders(&body.to_string())
            .map_err(|why| DeribitVenueReadRefusal::Unparsed { why })?,
        |r| &mut r.symbol,
    );
    let (mut orders, truncated) = vike_model::open_orders_read(reports);
    // The read's own columns, which this venue's row states and the report does not carry in the read's meaning.
    for (order, row) in orders.iter_mut().zip(rows) {
        order.price = row.get("price").and_then(json_number);
        order.timestamp_ms =
            row.get("creation_timestamp").and_then(Value::as_i64).unwrap_or(order.timestamp_ms);
    }
    Ok((orders, truncated))
}

/// `private/get_positions` → what the account holds, cut at [`MAX_VENUE_POSITIONS`] with the cut said. The
/// size is the reconcile read's ([`position_amount`]: what an order counts on that instrument, signed).
fn positions_from(body: &Value) -> Result<(Vec<VenueReadPosition>, bool), DeribitVenueReadRefusal> {
    let rows =
        body.as_array().ok_or(DeribitVenueReadRefusal::NotAList { answer: "get_positions" })?;
    let mut positions: Vec<VenueReadPosition> = rows
        .iter()
        .filter_map(|row| {
            let symbol = row.get("instrument_name").and_then(Value::as_str).unwrap_or("");
            let qty = position_amount(row, symbol);
            (qty != 0.0).then(|| VenueReadPosition {
                symbol: crate::names::user_name_of(symbol),
                title: String::new(),
                outcome: if qty > 0.0 { "long" } else { "short" }.to_string(),
                qty,
                average_price: row.get("average_price").and_then(json_number),
                price: row.get("mark_price").and_then(json_number),
                value: None,
                redeemable: false,
            })
        })
        .collect();
    let truncated = positions.len() > MAX_VENUE_POSITIONS;
    positions.truncate(MAX_VENUE_POSITIONS);
    Ok((positions, truncated))
}

#[cfg(test)]
mod tests {
    use vike_model::MAX_VENUE_OPEN_ORDERS;

    use super::*;

    /// One wallet with money comes first wherever the venue lists it, an empty one is left out, and each
    /// other wallet with money is kept: a single figure would read one wallet as the account.
    #[test]
    fn every_wallet_with_money_is_kept_and_an_empty_one_is_not() {
        let body = json!({"id": 1, "summaries": [
            {"currency": "ETH", "balance": 0.0, "equity": 0.0},
            {"currency": "BTC", "balance": 1.5, "equity": 1.6},
            {"currency": "USDC", "balance": "250.5"},
            {"currency": "SOL", "balance": 0},
        ]});
        let got = balance_from(&body).unwrap();
        assert_eq!((got.asset.as_str(), got.balance), ("BTC", 1.5));
        assert_eq!(
            got.other_assets,
            [VenueReadAssetBalance { asset: "USDC".into(), balance: 250.5 }]
        );
        assert!(got.allowances.is_empty());
    }

    #[test]
    fn an_account_with_no_money_is_an_answer_and_no_wallet_at_all_is_an_error() {
        let empty = json!({"summaries": [
            {"currency": "ETH", "balance": 0.0}, {"currency": "BTC", "balance": 0.0},
        ]});
        let got = balance_from(&empty).unwrap();
        assert_eq!((got.asset.as_str(), got.balance), ("ETH", 0.0));
        assert!(got.other_assets.is_empty());

        assert_eq!(balance_from(&json!({"summaries": []})), Err(DeribitVenueReadRefusal::NoWallet));
        assert_eq!(balance_from(&json!({"id": 1})), Err(DeribitVenueReadRefusal::NoWallet));
        let why = balance_from(&json!({"summaries": [{"currency": "BTC"}]})).unwrap_err();
        assert_eq!(
            why,
            DeribitVenueReadRefusal::NoBalance { asset: "BTC".to_string() },
            "a wallet with no figure is said, never read as zero"
        );
    }

    /// The row keeps its limit price, side, progress, status word, our id and placing time; a stop's
    /// `market_price` word is "no price", never a zero, and its `trigger_price` is listed with its kind.
    /// The stop row is the venue's documented `get_open_orders` shape (`order_type`, `trigger_price`,
    /// `trigger`), not a capture.
    #[test]
    fn an_open_order_keeps_its_price_side_progress_and_time() {
        crate::names::hold_deribit_kept_rows();
        let body = json!([
            {"instrument_name": "ETH_USDC-PERPETUAL", "order_id": "ETH-1", "label": "vike-1",
             "direction": "buy", "order_type": "limit", "price": 2500.5, "amount": 0.5,
             "filled_amount": 0.1, "creation_timestamp": 1_700_000_000_000i64,
             "last_update_timestamp": 1_700_000_009_000i64},
            {"instrument_name": "BTC-PERPETUAL", "order_id": "BTC-2", "label": "",
             "direction": "sell", "order_type": "stop_market", "price": "market_price",
             "trigger_price": 61000.0, "trigger": "last_price", "order_state": "untriggered",
             "amount": 10, "filled_amount": 0, "last_update_timestamp": 1_700_000_001_000i64},
        ]);
        let (orders, truncated) = open_orders_from(&body).unwrap();
        assert!(!truncated);
        let (a, b) = (&orders[0], &orders[1]);
        // read under its USER name (decision 0290): `ETH_USDC-PERPETUAL` is `ETHUSDC.P`
        assert_eq!((a.symbol.as_str(), a.side, a.price), ("ETHUSDC.P", 1, Some(2500.5)));
        assert_eq!((a.qty, a.filled_qty, a.status.as_str()), (0.5, 0.1, "PARTIALLY_FILLED"));
        assert_eq!(a.order_client_id.as_deref(), Some("vike-1"));
        assert_eq!(a.timestamp_ms, 1_700_000_000_000, "when it was placed, not when it last moved");
        assert_eq!((a.trigger_price, a.order_type.as_str()), (None, "limit"));
        assert_eq!((b.side, b.price, b.order_client_id.as_deref()), (-1, None, None));
        assert_eq!((b.status.as_str(), b.timestamp_ms), ("ACCEPTED", 1_700_000_001_000));
        assert_eq!((b.trigger_price, b.order_type.as_str()), (Some(61000.0), "stop"));
    }

    #[test]
    fn rows_past_the_cap_are_cut_and_said_to_be() {
        let rows =
            |n: usize| {
                json!((0..n)
                .map(|i| json!({"instrument_name": format!("I-{i}"), "order_id": i, "amount": 1,
                                "size": 1, "direction": "buy"}))
                .collect::<Vec<_>>())
            };
        let (orders, cut) = open_orders_from(&rows(MAX_VENUE_OPEN_ORDERS + 1)).unwrap();
        assert_eq!((orders.len(), cut), (MAX_VENUE_OPEN_ORDERS, true));
        let (positions, cut) = positions_from(&rows(MAX_VENUE_POSITIONS + 1)).unwrap();
        assert_eq!((positions.len(), cut), (MAX_VENUE_POSITIONS, true));
        let (_, cut) = positions_from(&rows(MAX_VENUE_POSITIONS)).unwrap();
        assert!(!cut, "a full list that is complete is not called cut");
    }

    /// The rows of a REAL testnet capture of `private/get_positions` read as the venue sizes them: an
    /// inverse future in dollars and a linear one in coin (the reconcile read's rule), the mark and the
    /// average price from the row, and a flat row left out.
    #[test]
    fn a_captured_positions_body_reads_as_the_venue_sizes_it() {
        crate::names::hold_deribit_kept_rows();
        let captured: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/deribit_classes_testnet_positions.json"
        ))
        .expect("the capture is JSON");
        let mut rows: Vec<Value> = captured
            .as_object()
            .expect("rows by name")
            .iter()
            .filter(|(name, _)| name.starts_with("position "))
            .map(|(_, row)| row.clone())
            .collect();
        assert!(!rows.is_empty(), "the capture's rows are named `position <instrument>`");
        rows.push(json!({"instrument_name": "BTC-1JAN26", "kind": "future", "size": 0.0,
                         "direction": "zero", "average_price": 0.0, "mark_price": 1.0}));
        let (positions, truncated) = positions_from(&Value::Array(rows)).unwrap();
        assert!(!truncated);
        let of = |name: &str| positions.iter().find(|p| p.symbol == name);
        // each row under its USER name (decision 0290): `BTC-3NOV26` is `BTCUSD_261103`
        let inverse = of("BTCUSD_261103").expect("the inverse future");
        assert_eq!((inverse.qty, inverse.outcome.as_str()), (10.0, "long"));
        assert_eq!((inverse.average_price, inverse.price), (Some(76837.5), Some(76837.15)));
        let linear = of("BTCUSDC_261103").expect("the linear future (`BTC_USDC-3NOV26`)");
        assert_eq!(linear.qty, 0.0001, "what an order counts, the coin, not the quote notional");
        assert!(of("BTC-1JAN26").is_none(), "a flat row holds nothing");
    }

    #[test]
    fn an_empty_list_is_an_answer_and_a_non_list_is_an_error() {
        assert_eq!(open_orders_from(&json!([])).unwrap(), (Vec::new(), false));
        assert_eq!(positions_from(&json!([])).unwrap(), (Vec::new(), false));
        assert!(open_orders_from(&json!({"error": "x"})).is_err());
        assert!(positions_from(&json!({"error": "x"})).is_err());
    }
}
