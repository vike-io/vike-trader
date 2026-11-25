//! The re-teaching worker's words when the lease closes under its re-telling, over the gatewayhub client's own
//! fake gatewayhub (`vike_gatewayhub_client::test_support::fake_trade_peer`).

use vike_gatewayhub_client::test_support::fake_trade_peer::{TradePeer, TradePeerConfig, open};
use vike_log::capture::captured;
use vike_log::poll::wait_until;

use super::*;

/// **The path lines follow the path's STATE, not the venue** (`venue_link`'s module doc, item 4). the CI box: hyperliquid's
/// testnet ends its private stream every ~10 min (`1000 Expired`) and the pump is back in about 1.5 s; each ask that found
/// it reopening was a WARN "path DOWN" and a WARN "back". Now a reopen one ask finds says nothing, one that outlives an
/// ask is one WARN DOWN and one WARN back, and a failure (a reopen or restart that failed) is an ERROR DOWN at once, on
/// any venue.
#[test]
fn the_path_lines_follow_the_state_and_a_routine_reconnect_says_nothing() {
    use tracing::Level;
    use vike_gatewayhub_client::proto::{TradeStatusReport, VenueLinkWire};

    let report = |link| TradeStatusReport {
        adapter_up: true,
        venue_link: Some(link),
        ..TradeStatusReport::default()
    };
    let up = || report(VenueLinkWire::working());
    let down = |failed_restarts| {
        report(VenueLinkWire {
            up: false,
            why: "server closed: 1000 Expired".to_string(),
            failed_restarts,
            retry_in_ms: 1_000,
            effect: String::new(),
        })
    };
    let said = |venue: &str, reports: Vec<TradeStatusReport>| {
        let mut watch = PathWatch::armed(Instant::now());
        let ((), logged) = captured(|| {
            for status in &reports {
                say_path(venue, "route", &watch.hear("route", status));
            }
        });
        let lines = logged.into_iter().filter(|e| e.target.ends_with("::reteach"));
        lines.map(|e| (e.level, e.message)).collect::<Vec<_>>()
    };

    let levels = |lines: &[(Level, String)]| lines.iter().map(|(l, _)| *l).collect::<Vec<_>>();

    let routine = said("hyperliquid", vec![up(), down(0), up(), down(0), up()]);
    assert!(routine.is_empty(), "{routine:?}");

    let held = said("hyperliquid", vec![down(0), down(0), down(0), up()]);
    assert_eq!(levels(&held), [Level::WARN, Level::WARN]);
    assert!(held[0].1.starts_with("hyperliquid: the venue path is DOWN"), "{held:?}");
    assert!(held[1].1.starts_with("hyperliquid: the venue path is back"), "{held:?}");

    let failed = said("dukascopy", vec![down(2), down(3), up()]);
    assert_eq!(levels(&failed), [Level::ERROR, Level::WARN]);
    assert!(failed[0].1.starts_with("dukascopy: the venue path is DOWN"), "{failed:?}");
}

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
    assert_eq!(
        line.message,
        "the lease closed before the restarted gatewayhub's reconcile client was told these \
         symbols (this tradehub is stopping, or the lease ended): nothing asks again on it; the \
         next tradehub start re-admits them at boot",
        "nothing is asked again and no patience ran out: {line:?}"
    );
    assert!(
        line.message.contains("lease closed") && line.message.contains("next tradehub start"),
        "{line:?}"
    );
    assert!(line.field("symbols").is_some_and(|s| s.contains(SYMBOL)), "{line:?}");
}

/// **The pauses between asks of a restarted gatewayhub, verbatim**: half a second doubling to five seconds, then five
/// for every ask after (the one ladder's arithmetic over this worker's two constants).
#[test]
fn a_restarted_gatewayhub_is_asked_again_after_these_pauses() {
    let mut ladder = retry_ladder();
    let ms: Vec<u128> = (0..6).map(|_| ladder.failed().as_millis()).collect();
    assert_eq!(ms, [500, 1000, 2000, 4000, 5000, 5000]);
}
