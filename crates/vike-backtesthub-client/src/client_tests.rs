//! `DialRefusal::shown`: the one sentence `vike-cli` and `vike-mcp` show for a failed backtest dial, with ONE id.

use std::io;

use vike_hub_protocol::call::HubCallError;

use super::DialRefusal;

/// **A refused key shows the door's id alone; a daemon that is not there shows the dial's.** The access refusal
/// already carries `hub.access.denied`, so wrapping it in `cli.backtest.addr.cannot_connect` would show two ids and
/// tell a person to start a daemon that answered.
#[test]
fn a_failed_dial_shows_one_id() {
    let denied = DialRefusal::CannotConnect {
        addr: "box:1".into(),
        e: HubCallError::AccessDenied("backtesthub auth denied (Write): access denied".into()),
    };
    assert_eq!(
        denied.shown(),
        "the backtest daemon at box:1 refused this box: error [hub.access.denied]: backtesthub auth denied \
         (Write): access denied"
    );

    let down = DialRefusal::CannotConnect {
        addr: "box:1".into(),
        e: HubCallError::NotSent(io::Error::other("connection refused")),
    };
    assert_eq!(
        down.shown(),
        "error [cli.backtest.addr.cannot_connect]: cannot connect to the backtest daemon at box:1: connection \
         refused (start it with `vike-backend backtesthub --addr`)"
    );
}
