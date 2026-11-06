use super::*;

/// `post_action` (both pre-merge copies, `transfer::usd_class_transfer` and
/// `builder_fee::approve_builder_fee`, did the same): the body is built over a wall-clock ms nonce,
/// and a body that cannot be built is the call's answer, `?`-returned before the gate or the POST.
/// The bytes of each action's body are the venue-certified gate's
/// (`crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs`).
#[test]
fn the_body_is_built_over_a_clock_nonce_and_its_failure_is_the_answer() {
    let transport = HyperliquidTransport::new(Network::Testnet);
    let before = vike_model::time::clock::now_ms_u64();
    let mut seen = None;
    let got = post_action(&transport, Network::Testnet, "usdClassTransfer", |nonce| {
        seen = Some(nonce);
        Err(VenueApiError { code: -7, msg: "no body".into() })
    });
    let after = vike_model::time::clock::now_ms_u64();
    match got {
        Err(e) => assert_eq!((e.code, e.msg.as_str()), (-7, "no body")),
        Ok(v) => panic!("an unbuilt body must not be posted, got {v}"),
    }
    let nonce = seen.expect("the body builder ran");
    assert!((before..=after).contains(&nonce), "nonce {nonce} is not the ms clock");
}
