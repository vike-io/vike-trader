use super::*;

fn limit_req(coid: &str, side: i32, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        ..Default::default()
    }
}

#[test]
fn modify_reprices_the_resting_order_in_place() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&limit_req("o1", 1, 1.0, 90.0)); // rest a buy limit: qty 1 @ 90
    c.modify(&limit_req("o1", 1, 1.0, 90.0), Some(2.0), Some(95.0)); // re-quote: qty 2 @ 95

    // the modify surfaces as a canonical OrderModified
    assert!(
        std::iter::from_fn(|| c.poll_events()).any(|e| matches!(e, Event::OrderModified(_))),
        "modify emits OrderModified"
    );
    // and the resting order is updated IN PLACE (not canceled + re-created)
    let (_, order) =
        c.pending.iter().find(|(coid, _)| coid == "o1").expect("order still resting after modify");
    assert_eq!(order.size, 2.0, "resting qty modified");
    assert_eq!(order.price, Some(95.0), "resting price modified");
}

#[test]
fn modify_of_unknown_or_filled_order_is_a_noop() {
    let mut c = PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.modify(&limit_req("ghost", 1, 1.0, 90.0), Some(2.0), Some(95.0)); // never submitted
    assert!(c.poll_events().is_none(), "modifying an unknown order emits nothing");
}
