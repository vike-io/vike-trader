//! The margin-mode step-2 un-hardcode, offline: `swap_td_mode`'s total mapping, the
//! byte-identity of the unset/Cross default vs the pre-flip hardcode, and the loud
//! no-wire deny for Cash (the `NoWire` transport PANICS on any REST call, proving a
//! denied mode never leaves the process). The golden-fixture pins live in
//! `tests/offline/r6_okx_parity.rs`; these are the scripted-transport twins.

use super::*;
use std::assert_matches;
use vike_bridge_core::credentials::Credentials;

/// A transport that must never be reached: every verb panics. Used to prove the
/// margin-mode deny happens BEFORE any wire call.
struct NoWire;
impl OkxTransport for NoWire {
    fn signed(
        &self,
        _base_url: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("denied order must not reach the wire (signed)");
    }
    fn signed_json(
        &self,
        _base_url: &str,
        _path: &str,
        _method: &str,
        _body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        panic!("denied order must not reach the wire (signed_json)");
    }
    fn public(
        &self,
        _base_url: &str,
        _path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("denied order must not reach the wire (public)");
    }
}

fn client() -> OkxPerpRest<NoWire> {
    OkxPerpRest {
        signer: OkxV5Signer::new(
            &Credentials {
                api_key: "k".into(),
                api_secret: "s".into(),
                passphrase: Some("p".into()),
            },
            || 0,
        ),
        transport: NoWire,
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties: SymbolProperties {
            tick_size: 0.1,
            step_size: 0.01,
            min_qty: 0.01,
            ..Default::default()
        },
        ct_val: 0.01,
        leverage: 2.0,
        broker_code: None,
    }
}

fn limit_req(mode: Option<MarginMode>) -> OrderRequest {
    OrderRequest {
        client_order_id: "vtMgnT1".into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 0.0002,
        order_type: "limit".into(),
        price: Some(30000.0),
        margin_mode: mode,
        ..Default::default()
    }
}

#[test]
fn swap_td_mode_total_mapping() {
    assert_eq!(swap_td_mode(None).unwrap(), "cross", "unset = the pre-flip hardcode");
    assert_eq!(swap_td_mode(Some(MarginMode::Cross)).unwrap(), "cross");
    assert_eq!(swap_td_mode(Some(MarginMode::Isolated)).unwrap(), "isolated");
    let err = swap_td_mode(Some(MarginMode::Cash)).unwrap_err();
    assert!(err.contains("cash"), "{err}");
}

/// THE ADAPTER TIE for `VenueCaps::default_margin_mode` — the reason that field moved out of
/// the venue-OFFERS table and into the enforced one.
///
/// Every other field on the margin axis is vendor-doc transcription with nothing in the repo
/// to check it against. `default_margin_mode` is different in kind: it claims something about
/// bytes THIS adapter emits, and okx is the one venue whose builder actually emits them
/// (`swap_td_mode` → `tdMode`). So the declaration is asserted against the REAL builder rather
/// than against another declaration — the shape
/// `crates/bridges/deribit/src/exec_tests.rs`'s `modify_is_default_noop_for_deribit` uses to tie
/// `supports_modify` to a real `rest.modify_order` call.
///
/// What is proven here, precisely: the mode the table NAMES as vike's default maps through the
/// real builder to the exact wire value an UNSET request produces. Declaring `Isolated` while
/// the builder still emits `"cross"` fails; changing the builder's unset arm to `"isolated"`
/// while the table still says `Cross` fails too. Neither side can move alone.
#[test]
fn okx_default_margin_mode_matches_the_td_mode_builder() {
    let declared = vike_model::caps_for(VENUE).default_margin_mode;

    // The declared default, driven through the real builder, is what an unset request sends.
    assert_eq!(
        swap_td_mode(Some(declared)).expect("declared default must be a mode okx accepts"),
        swap_td_mode(None).expect("unset always builds"),
        "okx: default_margin_mode must name the mode an unset request actually wires"
    );

    // ...and that is `"cross"` today — spelled out so the pair above cannot pass by both
    // sides drifting together.
    assert_eq!(swap_td_mode(None).unwrap(), "cross");
    assert_eq!(declared, MarginMode::Cross);

    // The same value, through the FULL order path rather than the helper alone: tdMode is the
    // second param and carries exactly the declared default.
    let params = client().build_order_params(&limit_req(None), None).expect("unset builds");
    assert_eq!(params[1], ("tdMode", json!(swap_td_mode(Some(declared)).unwrap())));

    // Intra-table: an explicit request for the default is preflight-honorable.
    assert!(vike_model::caps_for(VENUE).margin_modes.contains(&declared));
}

/// BYTE-IDENTITY: an unset margin_mode builds EXACTLY the params it always built (tdMode
/// "cross"), and an explicit Cross is indistinguishable from unset. Isolated flips ONLY the
/// tdMode pair.
#[test]
fn unset_and_cross_are_byte_identical_isolated_flips_only_tdmode() {
    let c = client();
    let unset = c.build_order_params(&limit_req(None), None).expect("unset builds");
    let cross =
        c.build_order_params(&limit_req(Some(MarginMode::Cross)), None).expect("cross builds");
    assert_eq!(unset, cross, "explicit Cross must equal the unset default");
    assert_eq!(unset[1], ("tdMode", json!("cross")));

    let iso = c
        .build_order_params(&limit_req(Some(MarginMode::Isolated)), None)
        .expect("isolated builds");
    assert_eq!(iso[1], ("tdMode", json!("isolated")));
    for (i, (pair_unset, pair_iso)) in unset.iter().zip(iso.iter()).enumerate() {
        if i != 1 {
            assert_eq!(pair_unset, pair_iso, "only tdMode may differ (index {i})");
        }
    }
    assert_eq!(unset.len(), iso.len());
}

/// Unified cross-venue attribution (task 4): a configured FD-broker code stamps `tag` onto the
/// wire body; absent, the key never appears at all — byte-identical to before `broker_code`
/// existed.
#[test]
fn tag_is_stamped_when_broker_code_present_and_absent_otherwise() {
    let c = client();
    let with =
        c.build_order_params(&limit_req(None), Some("5328c82e5542BCDE")).expect("with-code builds");
    assert!(with.iter().any(|(k, v)| *k == "tag" && v == &json!("5328c82e5542BCDE")));
    let without = c.build_order_params(&limit_req(None), None).expect("without-code builds");
    assert!(without.iter().all(|(k, _)| *k != "tag"));
}

#[test]
fn cash_never_reaches_the_wire_on_submit() {
    let c = client();
    let events = c.submit_order(&limit_req(Some(MarginMode::Cash)));
    assert_eq!(events.len(), 2, "{events:?}");
    assert_matches!(events[0], Event::OrderSubmitted(_));
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "vtMgnT1");
            assert!(r.reason.contains("cash"), "{}", r.reason);
        }
        other => panic!("want OrderRejected, got {other:?}"),
    }
}

#[test]
fn cash_never_reaches_the_wire_on_stop_algo() {
    let c = client();
    let mut req = limit_req(Some(MarginMode::Cash));
    req.order_type = "stop".into();
    req.trigger_price = Some(25000.0);
    let events = c.submit_order(&req); // routes to submit_stop_algo
    assert_eq!(events.len(), 2, "{events:?}");
    assert_matches!(events[0], Event::OrderSubmitted(_));
    assert_matches!(&events[1], Event::OrderRejected(r) if r.reason.contains("cash"));
}

/// Trigger-source pins on the pure stop-algo body: `None` emits NO `slTriggerPxType`
/// (byte-identical to the pre-`trigger_by` params — the venue default `last` rules); a
/// requested source appends the authority row's wire string as the tail param, base
/// params untouched.
#[test]
fn stop_algo_trigger_by_maps_and_default_stays_bare() {
    let c = client();
    let stop = |tb: Option<vike_model::TriggerBy>| {
        let mut r = limit_req(None);
        r.order_type = "stop".into();
        r.price = None;
        r.trigger_price = Some(25000.0);
        r.trigger_by = tb;
        r
    };
    let bare = c.build_stop_algo_params(&stop(None), "cross");
    assert!(bare.iter().all(|(k, _)| *k != "slTriggerPxType"), "None = venue default, no param");
    assert_eq!(bare.last().unwrap().0, "slOrdPx", "unchanged tail");
    for (tb, wire) in [
        (vike_model::TriggerBy::Last, "last"),
        (vike_model::TriggerBy::Mark, "mark"),
        (vike_model::TriggerBy::Index, "index"),
    ] {
        let params = c.build_stop_algo_params(&stop(Some(tb)), "cross");
        assert_eq!(params.last().unwrap(), &("slTriggerPxType", json!(wire)), "{tb:?}");
        assert_eq!(&params[..params.len() - 1], &bare[..], "{tb:?}: base params untouched");
    }
}

#[test]
fn batch_of_denied_modes_rejects_all_without_wire() {
    let c = client();
    let mut r2 = limit_req(Some(MarginMode::Cash));
    r2.client_order_id = "vtMgnT2".into();
    let events = OkxPerpRest::submit_batch(&c, &[limit_req(Some(MarginMode::Cash)), r2]);
    // 2 Submitted then 2 Rejected — no chunk is ever sent (NoWire would panic).
    assert_eq!(events.len(), 4, "{events:?}");
    assert_matches!(events[0], Event::OrderSubmitted(_));
    assert_matches!(events[1], Event::OrderSubmitted(_));
    assert_matches!(&events[2], Event::OrderRejected(r) if r.client_order_id == "vtMgnT1");
    assert_matches!(&events[3], Event::OrderRejected(r) if r.client_order_id == "vtMgnT2");
}
