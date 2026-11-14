//! The CAPABILITY NEGOTIATION of [`Request::GatewayStatus`](vike_datahub_client::proto::Request::GatewayStatus), by
//! [`FEATURE_GATEWAY_STATUS`]: an old server is refused client-side with nothing sent. The real server's advertisement
//! and answers are `crates/vike-datahub/tests/default_build/gateway_status.rs`'s. Loopback only.

use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::FEATURE_GATEWAY_STATUS;
use vike_datahub_client::test_support::spawn_fake_peer;

#[test]
fn a_server_without_the_capability_is_refused_client_side_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(vec!["load_bars".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds: same version");
    assert!(!client.serves_gateway_status());
    let err = client.gateway_status().expect_err("refused client-side");
    assert!(err.contains(FEATURE_GATEWAY_STATUS), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    let received = rx.recv().expect("the fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("the fake joins");
}
