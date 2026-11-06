//! post-only LIVE Deribit smoke (network + testnet creds — run manually):
//!     MSYS_NO_PATHCONV=1 LANE=<free lane> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-deribit --test deribit_post_only_smoke -- --ignored --nocapture
//!
//! Proves the deribit post-only wire against test.deribit.com on the most liquid BTC option, with
//! the venue's OWN records as the witness:
//!
//! 1. a far-below post-only LIMIT BUY rests: the order record says `post_only=true`, it is open, then
//!    cancelled;
//! 2. a post-only LIMIT BUY priced AT the ask would take liquidity: the venue REFUSES it
//!    (`reject_post_only`), the submit answers a terminal `OrderRejected` whose typed cause is
//!    `PostOnlyWouldCross`, nothing executes and nothing rests (without `reject_post_only` the venue
//!    would silently re-price the order to the best bid and rest it — the reason the pair is sent);
//! 3. a post-only MARKET order is loud-denied client-side (wire untouched);
//! 4. a CROSSING EDIT of a resting post-only order, which the docs leave open (`cancel_reason`
//!    `edit_post_only_reject`, the reply's shape unstated). Recorded, not assumed, on three orders, each
//!    cancelled after: `CAPTURED ... the bridge` drives `modify_order` and fails only when the event
//!    disagrees with the order read back; `CAPTURED ... the pair` is the same edit's raw reply;
//!    `CAPTURED ... reprice` is the alternative, `reject_post_only` false, for comparison. The
//!    `OUTCOME` line says which way the venue went.
//!
//! Double-gated like the other smokes: `#[ignore]` + self-skip when the demo account's creds are absent.

use std::assert_matches;

use serde_json::json;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::smoke::buy;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_deribit::client::{DeribitRest, parse_deribit_option_instruments};
use vike_deribit::transport::{DeribitOrderTransport, TESTNET_REST, TESTNET_WS};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, RejectCause};
use vike_model::{OrderRequest, OrderType, TimeInForce, now_ms};
use vike_secrets::credentials::api_credentials;

fn po_req(coid: &str, symbol: &str, qty: f64, price: Option<f64>, ot: OrderType) -> OrderRequest {
    buy(coid, "deribit", symbol, qty, ot, price, TimeInForce::PostOnly)
}

#[test]
#[ignore = "network + testnet creds — run manually (see module doc)"]
fn deribit_post_only_ladder() {
    vike_log::test_init();
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("deribit", &AccountLabel::Default, AccountTier::Demo);
    let Some(creds) = api_credentials(&account, "deribit") else {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
        return;
    };

    // most liquid BTC option with a live ask (same discovery as deribit_tif_smoke)
    let public = UreqTransport::new("deribit");
    let book = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_book_summary_by_currency",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("book summary");
    let mut best: Option<(String, f64, f64)> = None;
    for row in book["result"].as_array().unwrap_or(&vec![]) {
        let Some(ask) = row.get("ask_price").and_then(|a| a.as_f64()) else { continue };
        let oi = row.get("open_interest").and_then(|o| o.as_f64()).unwrap_or(0.0);
        let name = row.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("");
        if ask > 0.0 && !name.is_empty() && best.as_ref().is_none_or(|(_, b_oi, _)| oi > *b_oi) {
            best = Some((name.to_string(), oi, ask));
        }
    }
    let (symbol, oi, ask) = best.expect("a live-ask BTC option on testnet");
    tracing::info!(target: "vike_deribit", "instrument: {symbol} (oi={oi}, ask={ask})");

    let info = public
        .public(
            TESTNET_REST,
            "/api/v2/public/get_instruments",
            &[("currency", "BTC".into()), ("kind", "option".into())],
        )
        .expect("instruments");
    let inst = parse_deribit_option_instruments(&info)[&symbol].clone();

    let mut transport = DeribitOrderTransport::new(
        TESTNET_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    transport.connect().expect("order-WS auth");
    let rest = DeribitRest::new(transport, &symbol, inst.properties, "BTC");

    let qty = inst.properties.min_qty;
    let low_px = (ask * 0.25).min(0.004).max(inst.properties.tick_size);

    // 1. RESTS: far below the ask a post-only buy is a plain maker order
    let coid = format!("vtrpo1{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&po_req(&coid, &symbol, qty, Some(low_px), OrderType::Limit));
    tracing::info!(target: "vike_deribit", "resting submit events: {events:?}");
    assert_matches!(events.last(), Some(Event::OrderAccepted(_)), "resting post-only: {events:?}");
    rest.cancel_order(&coid).expect("cancel the resting post-only order");
    // the order history lists the order once it ended, with the flag the venue recorded
    let hist = rest.get_order_history(50).expect("order history");
    let row = hist
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|o| o.get("label").and_then(|l| l.as_str()) == Some(coid.as_str()))
        })
        .cloned();
    tracing::info!(target: "vike_deribit", "ended row: {row:?}");
    if let Some(row) = &row {
        assert_eq!(row.get("post_only").and_then(|v| v.as_bool()), Some(true), "{row}");
        assert_eq!(row.get("order_state").and_then(|v| v.as_str()), Some("cancelled"), "{row}");
    }

    // 2. CROSSES: a buy AT the ask lifts it — the venue refuses it, typed
    let coid = format!("vtrpo2{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&po_req(&coid, &symbol, qty, Some(ask), OrderType::Limit));
    tracing::info!(target: "vike_deribit", "crossing submit events: {events:?}");
    assert_matches!(
        events.as_slice(),
        [Event::OrderSubmitted(_), Event::OrderRejected(r)]
            if r.cause == Some(RejectCause::PostOnlyWouldCross),
        "a crossing post-only order is refused with the typed cause: {events:?}"
    );

    // 3. a post-only MARKET order is refused locally
    let coid = format!("vtrpo3{}", now_ms() % 100_000_000);
    let events = rest.submit_order(&po_req(&coid, &symbol, qty, None, OrderType::Market));
    assert_matches!(
        &events[1], Event::OrderRejected(r) if r.reason.contains("PostOnly needs a limit order"),
        "{events:?}"
    );

    // 4. A CROSSING EDIT: a resting buy moved to the ask. Whatever the venue does is the finding, so the one
    // assertion is that the engine is told what the order IS.
    let mut raw = DeribitOrderTransport::new(
        TESTNET_WS,
        creds.api_key.expose(),
        creds.api_secret.expose(),
        None,
    );
    raw.connect().expect("the raw socket's auth");
    let rest_far = |tag: &str| {
        let coid = format!("{tag}{}", now_ms() % 100_000_000);
        let request = po_req(&coid, &symbol, qty, Some(low_px), OrderType::Limit);
        let events = rest.submit_order(&request);
        let Some(Event::OrderAccepted(accepted)) = events.last() else {
            panic!("{tag} rests: {events:?}");
        };
        let order_id = accepted.venue_order_id.as_ref().expect("the venue's id").to_string();
        (coid, order_id, request)
    };

    let (coid, order_id, request) = rest_far("vtrpo4");
    let events = rest.modify_order(&request, None, Some(ask));
    let after = raw.call("private/get_order_state", &json!({ "order_id": order_id }));
    let after = after.expect("the order read back");
    tracing::info!(target: "vike_deribit", "CAPTURED crossing edit, the bridge: events={events:?} order after={after}");
    rest.cancel_order(&coid).expect("cancel the edited order");
    let outcome = match (after["result"]["order_state"].as_str(), events.as_slice()) {
        (Some("cancelled" | "rejected"), [Event::OrderCanceled(_)]) => {
            "the venue ENDED the order; the engine hears OrderCanceled"
        }
        (Some("open"), [Event::OrderModifyRejected(_)]) => {
            "the venue KEPT the order at its old price; the engine hears OrderModifyRejected"
        }
        (Some("open"), [Event::OrderModified(_)]) => {
            "the edit did not cross (the book moved from the ask read): rerun for a finding"
        }
        other => panic!("the engine was told something the order is not: {other:?} {after}"),
    };
    tracing::info!(target: "vike_deribit", "OUTCOME crossing edit: {outcome}");

    for (tag, word, flags) in [
        ("vtrpo5", "the pair", json!({"post_only": true, "reject_post_only": true})),
        ("vtrpo6", "reprice", json!({"post_only": true, "reject_post_only": false})),
    ] {
        let (coid, order_id, _) = rest_far(tag);
        let mut params = flags;
        params["order_id"] = json!(order_id);
        params["amount"] = json!(qty);
        params["price"] = json!(ask);
        let reply = raw.call("private/edit", &params);
        let after = raw.call("private/get_order_state", &json!({ "order_id": order_id }));
        tracing::info!(target: "vike_deribit", "CAPTURED crossing edit, {word}: reply={reply:?} order after={after:?}");
        rest.cancel_order(&coid).expect("cancel the edited order");
    }

    // nothing of ours rests
    let hist = rest.get_order_history(50).expect("order history");
    for row in hist.as_array().unwrap_or(&vec![]) {
        let label = row.get("label").and_then(|l| l.as_str()).unwrap_or("");
        if label.starts_with("vtrpo") {
            let state = row.get("order_state").and_then(|v| v.as_str());
            assert_ne!(state, Some("open"), "{label} must not rest: {row}");
            assert_eq!(row.get("filled_amount").and_then(|v| v.as_f64()), Some(0.0), "{row}");
        }
    }
    tracing::info!(target: "vike_deribit", "deribit post-only ladder green: rests far, crossing refused with the typed cause, market denied, book flat");
}
