use super::*;
use crate::ratelimit::RateGate;
use crate::signer::PreparedRequest;
use std::time::Duration;

struct NoopSigner;
impl Signer for NoopSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

// A closed localhost port: connection-refused is immediate and needs no DNS/network, so the
// send fails fast. We assert on the GATE, which is consulted BEFORE the send.
const DEAD: &str = "http://127.0.0.1:1";

#[test]
fn normal_public_call_consumes_the_gate() {
    let gate = RateGate::new(1, Duration::from_secs(30));
    let t = UreqTransport::new("test").with_rate_gate(gate.clone());
    let _ = t.public(DEAD, "/x", &[]); // fails at the network; the slot is already taken
    assert!(!gate.try_proceed(), "the public call took the gate's only slot before sending");
}

#[test]
fn t1_requery_is_exempt_from_the_gate() {
    let gate = RateGate::new(1, Duration::from_secs(30));
    let t = UreqTransport::new("test").with_rate_gate(gate.clone());
    let _ = t.signed_requery(DEAD, "/x", "GET", &[], &NoopSigner);
    assert!(gate.try_proceed(), "the latency-bounded T1 re-query must NOT ride the gate");
}

#[test]
fn no_gate_configured_is_a_passthrough() {
    // without with_rate_gate, calls just pass through — no panic, no gating.
    let t = UreqTransport::new("test");
    let _ = t.public(DEAD, "/x", &[]);
}
