//! The perp lane's delivery gate (`perp::deny_delivery`, okx's rule for every venue over
//! `vike_venues::venue_delivery_guard::delivery_verdict`): an OPENING order on a dated contract inside its last 24 h
//! and any order once it is delivered are the emitter-split pair `[OrderSubmitted, OrderRejected]` with the wire NEVER
//! touched, on the single submit (a stop too: the gate runs before the algo route) and per order inside the native
//! batch; a reduce-only order inside the window, and a perpetual, still go out.

use std::sync::Mutex;

use vike_binance::execution::rest::perp::{VenueRestBinancePerp, deny_delivery};
use vike_bridge_core::execution_drive;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::RestTransport;
use vike_model::events::Event;
use vike_model::{MS_PER_DAY, MS_PER_HOUR, OrderRequest, OrderType, SymbolProperties};
use vike_venues::venue_delivery_guard::delivery_guard_for;

use crate::time_in_force_gate::{Capture, NoWire, NullSigner};

/// The user name of the dated USDS-M contract delivered on the UTC day of `ms` (`BTCUSDT_YYMMDD`, decision 0290).
fn dated_on(ms: i64) -> String {
    let date = vike_model::time::epoch_ms_to_utc_date(ms); // YYYY-MM-DD
    format!("BTCUSDT_{}{}{}", &date[2..4], &date[5..7], &date[8..10])
}

/// A contract whose 08:00 UTC delivery is less than a day away from now: the day of `now + 16 h`.
fn inside_window() -> String {
    dated_on(vike_model::now_ms() + 16 * MS_PER_HOUR)
}

fn limit(order_client_id: &str, symbol: &str, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        reduce_only,
        ..execution_drive::limit("binance", order_client_id, symbol, 0.01, 50000.0)
    }
}

fn client<T: RestTransport>(transport: T) -> VenueRestBinancePerp<NullSigner, T> {
    VenueRestBinancePerp {
        link_id: None,
        signer: NullSigner,
        transport,
        base_url: "https://unused.invalid".to_string(),
        ty: vike_venues::venue_urls::UrlType::Live,
        margining: vike_model::Margining::Linear,
        algo_order_client_ids: Default::default(),
        symbol: "BTCUSDT_261225".to_string(),
        properties: SymbolProperties::default(),
        leverage: 1.0,
    }
}

/// The verdict, read at a chosen instant: taken a day and a minute out, an opening order refused a minute inside the
/// last day and a reduce-only one taken, every order refused at the delivery. A perpetual names no day.
#[test]
fn the_gate_refuses_an_opening_order_inside_the_last_day_and_every_order_once_delivered() {
    let expiry = delivery_guard_for("binance").expiry_of("BTCUSDT_261225").expect("a dated name");
    let open = limit("c-open", "BTCUSDT_261225", false);
    let close = limit("c-close", "BTCUSDT_261225", true);
    assert!(deny_delivery(&open, expiry - MS_PER_DAY - 60_000).is_none(), "outside the window");
    let inside = deny_delivery(&open, expiry - MS_PER_DAY + 60_000).expect("refused inside");
    let not_sent = Some(vike_model::OrderRefusalId::NotSent.word());
    assert!(
        matches!(&inside, Event::OrderRejected(r)
            if r.reason.contains("takes no OPENING order") && r.refusal_id.as_deref() == not_sent),
        "{inside:?}"
    );
    assert!(deny_delivery(&close, expiry - 60_000).is_none(), "a reduce-only order is taken");
    for request in [&open, &close] {
        let gone = deny_delivery(request, expiry).expect("refused at the delivery");
        assert!(
            matches!(&gone, Event::OrderRejected(r) if r.reason.contains("was delivered")),
            "{gone:?}"
        );
    }
    assert!(deny_delivery(&limit("c-perp", "BTCUSDT.P", false), expiry).is_none(), "a perpetual");
}

/// The single submit refuses an opening limit AND an opening stop inside the window with no wire call, and refuses
/// a reduce-only order once the contract is delivered.
#[test]
fn perp_submit_refuses_without_touching_the_wire() {
    let client = client(NoWire);
    let symbol = inside_window();
    execution_drive::assert_denied_pair(
        &VenueRest::submit_order(&client, &limit("c-open", &symbol, false)),
        "c-open",
        "takes no OPENING order",
    );
    let stop = OrderRequest {
        order_type: OrderType::Stop,
        price: None,
        trigger_price: Some(40000.0),
        ..limit("c-stop", &symbol, false)
    };
    execution_drive::assert_denied_pair(
        &VenueRest::submit_order(&client, &stop),
        "c-stop",
        "takes no OPENING order",
    );
    let delivered = dated_on(vike_model::now_ms() - 2 * MS_PER_DAY);
    execution_drive::assert_denied_pair(
        &VenueRest::submit_order(&client, &limit("c-late", &delivered, true)),
        "c-late",
        "was delivered",
    );
}

/// A reduce-only order inside the window reaches the wire, so no position is trapped at the delivery.
#[test]
fn perp_submit_sends_a_reduce_only_order_inside_the_window() {
    let client = client(Capture { calls: Mutex::new(Vec::new()) });
    let events = VenueRest::submit_order(&client, &limit("c-close", &inside_window(), true));
    assert!(matches!(events.last(), Some(Event::OrderAccepted(_))), "{events:?}");
    assert_eq!(client.transport.calls.lock().unwrap().len(), 1, "one wire call");
}

/// Per order inside the native batch: the opening order inside the window is refused and never enters the wire
/// chunk; the reduce-only one and the far contract's batch as before.
#[test]
fn perp_batch_partitions_the_refused_opening_order_out_of_the_wire_chunk() {
    let client = client(Capture { calls: Mutex::new(Vec::new()) });
    let near = inside_window();
    let far = dated_on(vike_model::now_ms() + 30 * MS_PER_DAY);
    let events = client.submit_batch(&[
        limit("c-open", &near, false),
        limit("c-close", &near, true),
        limit("c-far", &far, false),
    ]);
    let rejected: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.order_client_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, ["c-open"], "{events:?}");
    let calls = client.transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "one wire chunk");
    let batch = calls[0].iter().find(|(k, _)| k == "batchOrders").map(|(_, v)| v.clone()).unwrap();
    assert!(
        batch.contains("c-close") && batch.contains("c-far") && !batch.contains("c-open"),
        "{batch}"
    );
}
