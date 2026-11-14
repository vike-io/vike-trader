//! The session guard over the two price streams whose source is SHARED by the process (decision 0193 verdicts 4 and
//! 6): dukascopy's bus and fxcm's desk, each the venue's real source behind the shared `DataClientBus` engine, admitted
//! in the order `CredentialedStreamLane::open_in_session` admits them. The guard's half of those venues' contracts: the
//! ceiling row (`vike_gatewayhub_client::credentialed_limits`, which cannot name a bridge) held equal to the source's
//! own cap by this crate, which links both, and a replaced session that is told, never stops the newer one, and
//! returns once its replacement ended. The streams' own suites moved to their bridges with their venue code
//! (`crates/bridges/dukascopy/src/feed_stream_tests.rs`, `crates/bridges/fxcm/src/feed_stream_tests.rs`).

/// dukascopy's bus behind the guard.
mod dukascopy {
    use std::sync::Arc;
    use std::time::Duration;

    use vike_bridge_core::data_client::bus::DataClientBus;
    use vike_data::{DataClient, DataClientSink, RecordingSink};
    use vike_dukascopy::feed::{FEATURE_TICKS, MAX_PRICED_SYMBOLS, QuoteBus};
    use vike_dukascopy::test_support::{attached, subscribe_line, tick_now, unsubscribe_line};
    use vike_gatewayhub_client::credentialed_limits::stream_limits_for;
    use vike_log::poll::wait_for;

    use crate::data::session_guard::CredentialedStreamGuard;

    /// How long a wait in this module allows before `vike_log::poll::wait_for` fails it.
    const WAIT_FOR: Duration = Duration::from_secs(10);

    /// The ceiling for all holders is the bus's own cap: the row in `vike-gatewayhub-client` cannot name the
    /// bridge, so this crate, which links both, holds them equal. No spacing, since a stream open opens nothing.
    #[test]
    fn the_ceiling_row_is_the_bridges_own_cap() {
        let row = stream_limits_for("dukascopy").expect("dukascopy's row");
        assert_eq!(row.max_streams, MAX_PRICED_SYMBOLS);
        assert_eq!(row.min_open_spacing, Duration::ZERO);
    }

    /// One session of the venue under the production guard, built in the order `CredentialedStreamLane::open_in_session`
    /// builds it: the stream, which takes the bus, and then the admission.
    fn guarded(
        guard: &CredentialedStreamGuard,
        bus: &Arc<QuoteBus>,
        session: u64,
    ) -> (Box<dyn DataClient + Send>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let stream = DataClientBus::on(bus, Arc::clone(&sink) as Arc<dyn DataClientSink>);
        (guard.admit("dukascopy", session, Arc::clone(&sink) as _, Box::new(stream)), sink)
    }

    /// **A replaced session's wind-down never stops the newer session's symbol** (decision 0193 verdict 4): the
    /// older session discloses a gap on its key and is refused by the guard, and its stale hold on the bus reaches
    /// the sidecar with nothing, though both sessions asked for the same symbol.
    #[test]
    fn a_replaced_session_discloses_its_gap_and_never_stops_the_newer_sessions_symbol() {
        let guard = CredentialedStreamGuard::new(stream_limits_for);
        let (bus, wire) = attached(&[FEATURE_TICKS], None);
        let (mut older, older_sink) = guarded(&guard, &bus, 1);
        older.subscribe_quotes("EURUSD").expect("the first session");

        let (mut newer, newer_sink) = guarded(&guard, &bus, 2);
        wait_for(WAIT_FOR, "the older session's gap", || {
            !older_sink.stream_statuses_by_symbol().is_empty()
        });
        newer.subscribe_quotes("EURUSD").expect("the newer session");
        let stopped_by_the_takeover = [
            subscribe_line(&["EURUSD"]),
            unsubscribe_line(&["EURUSD"]),
            subscribe_line(&["EURUSD"]),
        ];
        assert_eq!(wire.lines(), stopped_by_the_takeover);

        tick_now(&bus, "EURUSD", 1.1, 1.2);
        wait_for(WAIT_FOR, "the newer session's quote", || newer_sink.quotes().len() == 1);
        assert!(older_sink.quotes().is_empty());
        let why = older.subscribe_quotes("GBPUSD").expect_err("replaced").to_string();
        assert!(why.contains("was replaced by session 2"), "{why}");
        // The older session's queued release has had its reaper by now: still no line of its own.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(wire.lines(), stopped_by_the_takeover);
    }

    /// **Verdict 4's last sentence, over the real bus.** Once the session that replaced it has ended the older
    /// session prices again on its next subscribe, because the bus is free and takes its hold back. The keys it
    /// held stay stopped until they are asked for again, and the bus keeps ONE listener throughout: a third
    /// session takes it from the returned one, which is refused again.
    #[test]
    fn a_replaced_session_opens_streams_again_once_its_replacement_ended() {
        let guard = CredentialedStreamGuard::new(stream_limits_for);
        let (bus, wire) = attached(&[FEATURE_TICKS], None);
        let (mut older, older_sink) = guarded(&guard, &bus, 1);
        older.subscribe_quotes("EURUSD").expect("the first session");
        let (mut newer, _newer_sink) = guarded(&guard, &bus, 2);
        wait_for(WAIT_FOR, "the older session's gap", || {
            !older_sink.stream_statuses_by_symbol().is_empty()
        });
        let why = older.subscribe_quotes("GBPUSD").expect_err("its replacement stands").to_string();
        assert!(why.contains("was replaced by session 2"), "{why}");

        newer.shutdown();
        drop(newer);

        older.subscribe_quotes("GBPUSD").expect("its replacement has ended");
        tick_now(&bus, "EURUSD", 1.1, 1.2); // a key it held: stopped, so this tick has no taker
        tick_now(&bus, "GBPUSD", 1.3, 1.31);
        wait_for(WAIT_FOR, "the returned session's quote", || !older_sink.quotes().is_empty());
        assert!(older_sink.quotes().iter().all(|q| q.1 == "GBPUSD"), "{:?}", older_sink.calls());

        older.subscribe_quotes("EURUSD").expect("a held key, asked for again");
        assert_eq!(wire.lines().last(), Some(&subscribe_line(&["EURUSD"])));
        tick_now(&bus, "EURUSD", 1.1, 1.2);
        wait_for(WAIT_FOR, "the key asked for again", || {
            older_sink.quotes().iter().any(|q| q.1 == "EURUSD")
        });

        let (mut third, third_sink) = guarded(&guard, &bus, 3);
        let why = older.subscribe_quotes("USDJPY").expect_err("replaced again").to_string();
        assert!(why.contains("was replaced by session 3"), "{why}");
        third.subscribe_quotes("USDJPY").expect("the third session");
        tick_now(&bus, "USDJPY", 150.0, 150.1);
        wait_for(WAIT_FOR, "the third session's quote", || !third_sink.quotes().is_empty());
        assert!(older_sink.quotes().iter().all(|q| q.1 != "USDJPY"), "one listener at a time");
    }
}

/// fxcm's desk behind the guard.
mod fxcm {
    use std::sync::Arc;
    use std::time::Duration;

    use vike_bridge_core::data_client::bus::DataClientBus;
    use vike_data::{DataClient, DataClientSink, RecordingSink};
    use vike_fxcm::feed::PriceDesk;
    use vike_fxcm::test_support::{ScriptedSession, paced_desk, standard_offers};
    use vike_gatewayhub_client::credentialed_limits::stream_limits_for;
    use vike_log::poll::wait_for;
    use vike_market_stream::market::CAP_MARK;
    use vike_model::StreamStatus;

    use crate::data::session_guard::CredentialedStreamGuard;

    /// How long a wait in this module allows before `vike_log::poll::wait_for` fails it.
    const WAIT_FOR: Duration = Duration::from_secs(10);

    /// **The row's ceiling is the desk's own cap, one number**, so neither refuses first (the bridge's constant cannot be
    /// named by the crate the table lives in), and it spaces nothing: a subscribe opens no connection.
    #[test]
    fn the_fxcm_row_is_the_desks_own_cap_and_makes_no_subscribe_wait() {
        let row = stream_limits_for("fxcm").expect("fxcm has a row");
        assert_eq!(row.max_streams, vike_fxcm::feed::MAX_PRICED_SYMBOLS);
        assert_eq!(row.min_open_spacing, Duration::ZERO);
    }

    /// One session the way `CredentialedStreamLane::open_in_session` makes it: the stream is BUILT first (which is what takes
    /// the desk's consumer place), then admitted.
    fn admitted(
        guard: &CredentialedStreamGuard,
        desk: &Arc<PriceDesk>,
        id: u64,
    ) -> (Box<dyn DataClient + Send>, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let wired = Arc::clone(&sink) as Arc<dyn DataClientSink>;
        let stream = Box::new(DataClientBus::on(desk, Arc::clone(&wired)));
        (guard.admit("fxcm", id, wired, stream), sink)
    }

    /// **A newer session replaces the older, the older is told, and the desk never has two consumers.** The older
    /// discloses a gap on every key it held and refuses naming the newer. Once the replacing session has ended (decision
    /// 0193 verdict 4) the desk is free and takes the older back on its next subscribe, the keys it held staying stopped;
    /// a third session then takes the desk from it, and it is refused again.
    #[test]
    fn a_replaced_session_is_told_and_opens_streams_again_once_its_replacement_ended() {
        let session = ScriptedSession::up(standard_offers());
        let desk = paced_desk(&session);
        let guard = CredentialedStreamGuard::new(stream_limits_for);
        let (mut older, older_sink) = admitted(&guard, &desk, 1);
        older.subscribe_quotes("EURUSD").expect("the first session");
        wait_for(WAIT_FOR, "the older session's quote", || !older_sink.quotes().is_empty());

        let (mut newer, newer_sink) = admitted(&guard, &desk, 2);
        wait_for(WAIT_FOR, "the older session's gap", || {
            older_sink
                .stream_statuses()
                .iter()
                .any(|s| s.1 == "EURUSD" && matches!(s.3, StreamStatus::GapStart { .. }))
        });
        let why = older.subscribe_quotes("GBPUSD").expect_err("replaced").to_string();
        assert!(why.contains("replaced by session 2"), "{why}");
        newer.subscribe_quotes("GBPUSD").expect("the newer session");
        wait_for(WAIT_FOR, "the newer session's quote", || !newer_sink.quotes().is_empty());
        assert!(
            newer_sink.quotes().iter().all(|q| q.1 == "GBPUSD"),
            "the older's wants went with it"
        );

        newer.shutdown();
        drop(newer);
        let before = older_sink.quotes().len();
        older.subscribe_quotes("GBPUSD").expect("its replacement has ended");
        wait_for(WAIT_FOR, "the returned session's quote", || older_sink.quotes().len() > before);
        assert!(
            older_sink.quotes()[before..].iter().all(|q| q.1 == "GBPUSD"),
            "the key it held stays stopped"
        );

        let (mut third, third_sink) = admitted(&guard, &desk, 3);
        let why = older.subscribe_quotes("USDJPY").expect_err("replaced again").to_string();
        assert!(why.contains("replaced by session 3"), "{why}");
        third.subscribe_quotes("USDJPY").expect("a third session starts clean");
        wait_for(WAIT_FOR, "the third session's quote", || !third_sink.quotes().is_empty());
        assert!(third_sink.quotes().iter().all(|q| q.1 == "USDJPY"), "it starts clean");
    }

    /// **At the ceiling the refusal is the guard's cap (the class the tradehub does not restart on), never the desk's own
    /// words, and a released pair's place is free for the very next ask**, because the guard makes the queued release
    /// before the open: the desk's cap and the row's ceiling are one number, so neither is met first.
    #[test]
    fn at_the_ceiling_the_refusal_is_a_cap_and_a_released_place_is_free_for_the_next_ask() {
        let session = ScriptedSession::up(standard_offers());
        let desk = paced_desk(&session);
        let guard = CredentialedStreamGuard::new(stream_limits_for);
        let (mut client, _sink) = admitted(&guard, &desk, 1);
        let pairs = ('A'..='Q').map(|c| format!("EUR{c}{c}{c}")).collect::<Vec<_>>();
        let cap = vike_fxcm::feed::MAX_PRICED_SYMBOLS;
        let mut ids = Vec::new();
        for pair in &pairs[..cap] {
            ids.push(client.subscribe_quotes(pair).unwrap_or_else(|e| panic!("{pair}: {e}")));
        }

        let why = client.subscribe_quotes(&pairs[cap]).expect_err("past the ceiling").to_string();
        assert!(why.contains(CAP_MARK) && why.contains(&format!("({cap})")), "{why}");

        client.unsubscribe(ids[2]);
        client.subscribe_quotes(&pairs[cap]).expect("the release is made before the open");
    }
}
