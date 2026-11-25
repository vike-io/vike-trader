//! An AMEND of a resting stop, stop-limit and take-profit on the OKX DEMO account, through the bridge's
//! own `VenueRestOkx`: `amend-algos` by `algoClOrdId` (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s
//! `amend_algo`; the official "Amend algo order" table,
//! <https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-amend-algo-order>).
//! `tests/offline/algo_amend.rs` pins the same logic offline. PASSED on the demo 2026-11-08, 2 of 2 (main
//! `d1624c326e` / `f1a5acec7c`, store account `#15 okx/demo`): the okx amend-mode rows amend in place behind it.
//!
//!     MSYS_NO_PATHCONV=1 BOX=the CI box LANE=<pinned> just the CI box <branch> env \
//!       VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture --test-threads=1 okx_algo_amend_smoke::
//!
//! ORDER-PLACING (`BTC-USDT-SWAP`), DEMO ONLY, the instrument's minimum size, through the stop-limit smoke's
//! `Run` (`okx_stop_limit_smoke.rs`: the same client, guard and flatten):
//!   * `okx_a_resting_trigger_orders_prices_move_in_place`: four far SELL orders that cannot trigger in a
//!     smoke's lifetime (a stop and a stop-limit at HALF the last price, a market-if-touched and a
//!     limit-if-touched at TWICE it), each amended once through the seam's own door (a stop's trigger by
//!     `modify_order`, the others' trigger AND limit by `modify_trigger`), read back from `order-algo`
//!     (the venue must hold the new words and the SAME `algoId`), then cancelled.
//!   * `okx_the_amend_smokes_leave_the_account_flat_and_empty`: READ-ONLY, last by name.
//!
//! What a run must print before its asserts, and what a miss costs: `MEASURED order-algo after the amend`
//! showing the unmoved word of the side changed (a restated limit or trigger read as something else) means
//! the venue does not take a restated value as "unchanged", and every amend would move both; a changed
//! `algoId` means the venue replaced the order, and the bridge's `OrderModified` venue id is wrong.
//!
//! Gated like every `*_smoke.rs` here (the DEMO account's creds through `vike_secrets::smoke_directory`, an early
//! return when absent; the simulated-trading header on every transport). Credentials are never printed.

use std::time::Duration;

use serde_json::json;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest};
use vike_model::events::Event;
use vike_okx::execution::rest::PATH_ORDERS_ALGO_PENDING;

use crate::demo_credentials::load_demo_credentials;
use crate::demo_swap::build_rest;
use crate::okx_stop_limit_smoke::{INST_ID, Run, SYMBOL, accepted_id, pick, position, px};

const ROW_KEYS: &[&str] =
    &["state", "sz", "slTriggerPx", "slOrdPx", "tpTriggerPx", "tpOrdPx", "algoId"];

#[test]
#[ignore = "network + demo creds — places, amends and cancels four real (non-triggering) demo trigger orders — run manually (see module doc)"]
fn okx_a_resting_trigger_orders_prices_move_in_place() {
    vike_log::test_init();
    let Some(mut run) = Run::start() else { return };
    let last = run.rest.last_price().expect("ticker");
    assert!(last > 0.0, "a last price is required");
    let (low, high) = (last * 0.5, last * 2.0);
    // (tag, kind, trigger, limit, the amend's new trigger, the amend's new limit, the side's word prefix)
    let cases = [
        ("vtamstop", "stop", low, None, low * 0.99, None, "sl"),
        ("vtamstlm", "stop_limit", low, Some(low * 0.99), low * 0.98, Some(low * 0.97), "sl"),
        ("vtammit", "take_profit", high, None, high * 1.01, None, "tp"),
        ("vtamlit", "take_profit", high, Some(high * 0.99), high * 1.01, Some(high), "tp"),
    ];
    for (tag, kind, trigger, limit, new_trigger, new_limit, side) in cases {
        // --- 1. place, then amend through the seam's door for the kind ---------------------------------
        let (order_client_id, events) = run.place(tag, -1, kind, trigger, limit);
        println!(
            "MEASURED submit of {kind} {order_client_id} trigger={trigger} limit={limit:?}: {events:?}"
        );
        let algo_id = accepted_id(&events);
        let before = run.algo_row(&order_client_id);
        println!(
            "MEASURED order-algo before the amend: {:?}",
            before.as_ref().map(|r| pick(r, ROW_KEYS))
        );
        let order: vike_model::OrderRequest = serde_json::from_value(json!({
            "order_client_id": order_client_id, "venue": "okx", "symbol": SYMBOL, "side": -1, "qty": run.leg,
            "order_type": kind, "trigger_price": trigger, "price": limit
        }))
        .unwrap();
        let answer = if kind == "stop" {
            VenueRest::modify_order(&run.rest, &order, None, Some(new_trigger))
        } else {
            VenueRest::modify_trigger(&run.rest, &order, None, new_limit, new_trigger)
        };
        println!(
            "MEASURED bridge amend of {kind} {order_client_id} to trigger={new_trigger} limit={new_limit:?}: {answer:?}"
        );
        std::thread::sleep(Duration::from_secs(1));
        let after = run.algo_row(&order_client_id);
        println!(
            "MEASURED order-algo after the amend: {:?}",
            after.as_ref().map(|r| pick(r, ROW_KEYS))
        );
        let cancel = run.rest.cancel_order(&order_client_id);
        println!("MEASURED bridge cancel of the amended {kind}: {cancel:?}");

        // --- 2. ASSERT, last --------------------------------------------------------------------------
        assert!(algo_id.is_some(), "{kind}: ACCEPTED with an algoId");
        match &answer[..] {
            [Event::OrderModified(m)] => {
                assert_eq!(
                    m.venue_order_id.as_deref(),
                    algo_id.as_deref(),
                    "{kind}: the same algoId"
                );
            }
            other => panic!("{kind}: one OrderModified, got {other:?}"),
        }
        let after = after.expect("order-algo after the amend");
        assert_eq!(
            after["algoId"].as_str(),
            algo_id.as_deref(),
            "{kind}: amended in place, not replaced"
        );
        let trigger_word = format!("{side}TriggerPx");
        assert_eq!(px(&after[trigger_word.as_str()]), run.tick(new_trigger).parse().ok(), "{kind}");
        let ord_word = format!("{side}OrdPx");
        let want = new_limit.map_or_else(|| "-1".to_string(), |l| run.tick(l));
        assert_eq!(px(&after[ord_word.as_str()]), want.parse().ok(), "{kind}");
        assert_eq!(
            cancel.expect("the bridge cancel of the amended order"),
            OrderCancelAck::Canceled
        );
    }
    println!(
        "okx algo amend smoke green: stop, stop-limit, MIT and LIT moved in place under one algoId each"
    );
}

/// READ-ONLY, and last of the module by name: no order of the amend smoke rests (its `vtam` order client id prefix) and
/// the position is flat. Other clients' orders are counted, never touched.
#[test]
#[ignore = "network + demo creds — read-only check that the amend smoke left nothing behind — run manually (see module doc)"]
fn okx_the_amend_smokes_leave_the_account_flat_and_empty() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    let rest = build_rest(&creds, INST_ID);
    let leg = rest.properties.min_qty * rest.ct_val;
    let algo = rest
        .call_get(
            PATH_ORDERS_ALGO_PENDING,
            &[("ordType", json!("conditional")), ("instId", json!(INST_ID))],
        )
        .expect("orders-algo-pending");
    let left: Vec<String> = algo
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r["algoClOrdId"].as_str())
        .filter(|c| c.starts_with("vtam"))
        .map(String::from)
        .collect();
    // The position as the live reconcile pass reads it (the production factory, demo host).
    let recon =
        vike_okx::recon_client(&creds, INST_ID, rest.ct_val, &vike_okx::test_support::demo_net())
            .expect("okx recon_client is always Some");
    let position = position(recon.as_ref());
    println!("MEASURED account at the end: position {position:?}; ours {left:?}");
    assert!(left.is_empty(), "a smoke order still rests: {left:?}");
    assert!(position.is_ok_and(|q| q.abs() < leg * 0.5), "the account must end FLAT");
}
