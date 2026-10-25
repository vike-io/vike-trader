//! Pins for `limits`: the exact values the hand-copied constants carried before they were hoisted,
//! so a tweak is a visible edit to this file rather than a silent disagreement between daemons.

use std::time::Duration;

use super::{
    CLIENT_CONNECT_TIMEOUT, CLIENT_HANDSHAKE_DEADLINE, SERVER_HANDSHAKE_DEADLINE,
    SERVER_HANDSHAKE_MAX_FRAME_LEN, SERVER_IDLE_READ_TIMEOUT, SERVER_IDLE_READ_TIMEOUT_MAX,
    SERVER_IDLE_READ_TIMEOUT_MIN, server_idle_read_timeout,
};

#[test]
fn the_server_limits_are_where_the_copies_left_them() {
    assert_eq!(SERVER_HANDSHAKE_MAX_FRAME_LEN, 64 * 1024);
    assert_eq!(SERVER_HANDSHAKE_DEADLINE, Duration::from_secs(10));
    assert_eq!(SERVER_IDLE_READ_TIMEOUT, Duration::from_secs(300));
}

#[test]
fn the_client_limits_are_where_the_copies_left_them() {
    assert_eq!(CLIENT_CONNECT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(CLIENT_HANDSHAKE_DEADLINE, Duration::from_secs(10));
}

/// The range a `config.node_idle_read_timeout_ms` row is accepted in. `vike-config` restates both
/// ends in milliseconds (it cannot name this crate) and each daemon holds its restatement equal to
/// these; a deliberate change is an edit to this pin AND to those, never to one.
#[test]
fn the_idle_read_timeout_row_range_is_where_it_was_argued() {
    assert_eq!(SERVER_IDLE_READ_TIMEOUT_MIN, Duration::from_secs(120));
    assert_eq!(SERVER_IDLE_READ_TIMEOUT_MAX, Duration::from_secs(3600));
    assert!(SERVER_IDLE_READ_TIMEOUT_MIN <= SERVER_IDLE_READ_TIMEOUT);
    assert!(SERVER_IDLE_READ_TIMEOUT <= SERVER_IDLE_READ_TIMEOUT_MAX);
}

/// A box without the row runs the constant, byte for byte; a box with one runs its milliseconds,
/// at both ends of the range and at the default's own value. The function does not clamp: it hands
/// back what it was given (the range is `vike-config`'s refusal, and a daemon test relies on being
/// able to hand a daemon a shorter one).
#[test]
fn the_row_decides_the_idle_read_timeout_and_its_absence_decides_nothing() {
    assert_eq!(server_idle_read_timeout(None), SERVER_IDLE_READ_TIMEOUT);
    assert_eq!(server_idle_read_timeout(Some(300_000)), SERVER_IDLE_READ_TIMEOUT);
    assert_eq!(server_idle_read_timeout(Some(120_000)), SERVER_IDLE_READ_TIMEOUT_MIN);
    assert_eq!(server_idle_read_timeout(Some(3_600_000)), SERVER_IDLE_READ_TIMEOUT_MAX);
    assert_eq!(server_idle_read_timeout(Some(250)), Duration::from_millis(250));
}
