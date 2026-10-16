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

/// A box with no `config.node_ack_wait_ms` row waits what `trade` and `mcp` always waited: the
/// row's default is two seconds, and a settings tree that holds no row resolves to it.
#[test]
fn the_default_ack_wait_is_the_two_seconds_both_surfaces_always_waited() {
    assert_eq!(
        Duration::from_millis(u64::from(vike_config::config::DEFAULT_NODE_ACK_WAIT_MS)),
        Duration::from_secs(2)
    );
    assert_eq!(
        vike_config::Settings::default().config.node_ack_wait_ms,
        vike_config::config::DEFAULT_NODE_ACK_WAIT_MS,
        "a box with no row resolves the default"
    );
}
