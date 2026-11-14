use super::*;

/// A box with neither patience row dials with today's windows: `LinkPatience::default()` IS the two
/// constants, so every call site that passes it behaves as the parameterless `connect` always did.
#[test]
fn the_default_patience_is_todays_constants() {
    let p = LinkPatience::default();
    assert_eq!(p.observe_stall, OBSERVE_READ_TIMEOUT);
    assert_eq!(p.control_reply, CONTROL_REPLY_TIMEOUT);
    assert_eq!(p.observe_stall, Duration::from_secs(45));
    assert_eq!(p.control_reply, Duration::from_secs(30));
}

/// The settings rows are milliseconds; the constructor is a plain conversion that neither clamps
/// nor validates (the bounds are `vike-config`'s, refused at the write and the load).
#[test]
fn the_patience_constructor_converts_milliseconds_without_coercion() {
    let p = LinkPatience::from_millis(45_000, 30_000);
    assert_eq!(p, LinkPatience::default());
    let raised = LinkPatience::from_millis(300_000, 600_000);
    assert_eq!(raised.observe_stall, Duration::from_secs(300));
    assert_eq!(raised.control_reply, Duration::from_secs(600));
    // Below the floor is not this layer's to refuse: it comes out as written.
    assert_eq!(LinkPatience::from_millis(1, 2).observe_stall, Duration::from_millis(1));
}

/// Every relation the compile-time assertions in `liveness.rs` hold for the constants also holds for
/// any window at or above them, which is all `vike-config` accepts: the floors ARE the constants, so
/// a raised window only widens each margin. Checked at the floors and the ceilings the settings
/// layer allows. (The one-shot bounds do not follow the control window, so their relations to the
/// handshake and to each other are the compile-time assertions' alone.)
#[test]
fn the_relations_hold_across_the_accepted_range() {
    for (observe_ms, control_ms) in [(45_000u32, 30_000u32), (45_001, 30_001), (300_000, 600_000)] {
        let p = LinkPatience::from_millis(observe_ms, control_ms);
        assert!(p.observe_stall >= 2 * OBSERVE_HEARTBEAT, "two missed heartbeats at least");
        assert!(p.observe_stall >= OBSERVE_READ_TIMEOUT);
        assert!(p.control_reply >= CONTROL_REPLY_TIMEOUT);
        assert!(p.control_reply >= CLIENT_HANDSHAKE_DEADLINE, "no tighter than the handshake");
    }
}
