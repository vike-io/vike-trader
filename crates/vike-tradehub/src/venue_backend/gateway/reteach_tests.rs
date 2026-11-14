//! The re-teaching worker's words when the lease closes under its re-telling, over the gatewayhub client's own
//! fake gatewayhub (`vike_gatewayhub_client::test_support::fake_trade_peer`).

use vike_gatewayhub_client::test_support::fake_trade_peer::{TradePeer, TradePeerConfig, open};
use vike_log::capture::captured;
use vike_log::poll::wait_until;

use super::*;

/// The one symbol the fake daemon's default script grants a commit for (the CI box's were `NVDAUSDT.P` and
/// `BTCUSDT_261225.P`).
const SYMBOL: &str = "SOLUSDT.P";

/// **The 2026-11-07 21:05 deploy on the CI box**: the gatewayhub restarted, the re-teaching began telling the new daemon
/// the admitted symbols, and 11 s later the deploy stopped THIS tradehub, whose own detach closed the lease
/// (`stopped`). The worker said "asking again" per symbol and then "gave up ... after 60s; their orders stay HELD
/// until an order on the symbol" although it had asked for no 60 s, would ask nothing again, and the next start's
/// boot re-admission told both symbols half a minute later. A lease that closed is said ONCE, as what it is.
#[test]
fn a_lease_closed_under_the_retelling_is_said_once_as_closed_never_as_sixty_seconds_of_asking() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "node");
    session.admit_symbol(SYMBOL).expect("the daemon grants the commit");
    peer.restart();
    assert!(
        wait_until(Duration::from_secs(8), || !session.has_told(SYMBOL)
            && session.link_state() == TradeLinkState::Up),
        "the reconnect to the new epoch owes the symbol"
    );
    // The tradehub's own stop: the lease's final detach.
    session.detach();

    let worker = Reteach {
        session,
        start: AdapterStart {
            rid: 0,
            symbol: "BTCUSDT.P".to_string(),
            declared_legs: Vec::new(),
            reconcile_enabled: true,
            takes_trigger: false,
            risk: None,
            market_slippage: None,
            halt_admit: Default::default(),
            withhold: None,
            account_leverage: None,
            read_account_currency: false,
            route_key: "binance".to_string(),
        },
        venue: "binance".to_string(),
        label: None,
        tier: AccountTier::Demo,
        trigger: None,
        cell: SnapshotCell::default(),
        late: None,
        facts: Arc::new(TradeLeaseRegistry::default()),
        path: PathWatch::armed(Instant::now()),
    };
    let (_tx, rx) = std::sync::mpsc::channel();
    let began = Instant::now();
    let ((refused, next), logged) = captured(|| {
        let mut refused = worker.retell();
        let next = worker.retell_until_told(&rx, &mut refused);
        (refused, next)
    });

    assert!(began.elapsed() < Duration::from_secs(5), "a closed lease is not waited on");
    assert!(next.is_none());
    assert_eq!(refused.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), [SYMBOL], "still owed");
    let said: Vec<_> = logged.iter().filter(|e| e.target.ends_with("::reteach")).collect();
    assert_eq!(said.len(), 1, "one line for the closed lease, none per symbol: {said:?}");
    let line = &said[0];
    assert_eq!(line.level, tracing::Level::WARN, "{line:?}");
    assert!(
        !line.message.contains("asking again") && !line.message.contains("60s"),
        "nothing is asked again and no patience ran out: {line:?}"
    );
    assert!(
        line.message.contains("lease closed") && line.message.contains("next tradehub start"),
        "{line:?}"
    );
    assert!(line.field("symbols").is_some_and(|s| s.contains(SYMBOL)), "{line:?}");
}
