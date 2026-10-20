//! `config.node_ack_wait_ms` as this crate reads it: the row's bounds against the client that
//! enforces its own, and the default against the wait both surfaces always waited.

use std::time::Duration;

/// **The row's ceiling IS the control client's reply timeout.** `vike-config` sits below
/// `vike-tradehub-client`, so it cannot import `CONTROL_REPLY_TIMEOUT` and restates the number
/// (`vike_config::config::MAX_NODE_ACK_WAIT_MS`). A wait longer than that timeout could never be
/// answered, because the client declares the link dead and resolves the command's outcome first;
/// and `vike-cli mcp` serves one request at a time, so the same number bounds how long a write tool
/// can hold the whole server. This is the test that holds the two equal, the way
/// `vike_config::config::RECONCILE_POLICIES` is held to its reader's words.
#[test]
fn the_ack_wait_ceiling_is_the_control_reply_timeout() {
    assert_eq!(
        u128::from(vike_config::config::MAX_NODE_ACK_WAIT_MS),
        vike_tradehub_client::liveness::CONTROL_REPLY_TIMEOUT.as_millis(),
        "the row's ceiling must be the reply timeout the control client enforces itself"
    );
    assert!(
        u128::from(vike_config::config::MIN_NODE_ACK_WAIT_MS)
            < vike_tradehub_client::liveness::CONTROL_REPLY_TIMEOUT.as_millis(),
        "the floor sits below the ceiling"
    );
}

/// A box with no `config.node_ack_wait_ms` row waits the control door's price hold plus a margin, so a
/// refusal the door gives after holding an order for a price (4 s) is read by `trade` and `mcp` instead
/// of being hung up on (it used to be 2 s: the v0.1.81 IG drill's "sent, but the node has not
/// answered"). A settings tree that holds no row resolves to it, and a row keeps winning.
#[test]
fn the_default_ack_wait_outlasts_the_doors_price_hold() {
    assert_eq!(
        Duration::from_millis(u64::from(vike_config::config::DEFAULT_NODE_ACK_WAIT_MS)),
        Duration::from_millis(u64::from(
            vike_config::config::DOOR_PRICE_WAIT_MS + vike_config::config::ACK_WAIT_MARGIN_MS
        ))
    );
    const {
        assert!(
            vike_config::config::DEFAULT_NODE_ACK_WAIT_MS > vike_config::config::DOOR_PRICE_WAIT_MS,
            "the client's default wait must end after the door's hold"
        );
    }
    assert_eq!(
        vike_config::Settings::default().config.node_ack_wait_ms,
        vike_config::config::DEFAULT_NODE_ACK_WAIT_MS,
        "a box with no row resolves the default"
    );
}
