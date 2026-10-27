use vike_gatewayhub_client::proto::{OptionIndexKind, read_frame};

use super::*;

fn facts() -> RestFacts {
    RestFacts {
        klines: vec!["binance".into(), "bybit".into()],
        funding: vec!["binance".into()],
        ticks: vec!["dukascopy".into()],
        catalog: vec!["binance".into()],
        credentialed: vec!["oanda".into()],
        options: true,
        family: vec![],
        presence: true,
    }
}

fn klines(venue: &str) -> FetchWhat {
    FetchWhat::Klines {
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        start_ms: 0,
        end_ms: 1,
    }
}

#[test]
fn a_served_lane_and_venue_passes() {
    assert_eq!(pre_check(&facts(), &klines("binance"), false), None);
    assert_eq!(pre_check(&facts(), &klines("oanda"), true), None);
    let index = FetchWhat::OptionIndex { kind: OptionIndexKind::Future };
    assert_eq!(pre_check(&facts(), &index, false), None);
}

#[test]
fn an_unserved_lane_is_a_positional_error_naming_the_string() {
    let no_ticks = RestFacts { ticks: vec![], ..facts() };
    let ticks = FetchWhat::Ticks {
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        start_ms: 0,
        end_ms: 1,
    };
    match pre_check(&no_ticks, &ticks, false) {
        Some(Refusal::Error(text)) => assert!(text.contains("\"gw_ticks\""), "{text}"),
        other => panic!("{other:?}"),
    }
    match pre_check(&RestFacts { credentialed: vec![], ..facts() }, &klines("oanda"), true) {
        Some(Refusal::Error(text)) => assert!(text.contains("\"gw_fetch_credentialed\""), "{text}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unserved_venue_is_a_terminal_refusal_that_cannot_be_retried() {
    match pre_check(&facts(), &klines("okx"), false) {
        Some(Refusal::Failed(FetchFailure::Refused(text))) => {
            assert!(text.contains("`okx`") && text.contains("binance, bybit"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    // A keyless venue is never reachable down the credentialed lane, nor the converse.
    assert!(matches!(
        pre_check(&facts(), &klines("binance"), true),
        Some(Refusal::Failed(FetchFailure::Refused(_)))
    ));
    assert!(matches!(
        pre_check(&facts(), &klines("oanda"), false),
        Some(Refusal::Failed(FetchFailure::Refused(_)))
    ));
}

#[test]
fn a_credentialed_fetch_serves_klines_only() {
    let funding = FetchWhat::Funding {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        start_ms: 0,
        end_ms: 1,
    };
    match pre_check(&facts(), &funding, true) {
        Some(Refusal::Error(text)) => assert!(text.contains("klines only"), "{text}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_lane_names_the_row_kind_it_answers_with() {
    assert_eq!(expected_kind(&klines("binance")), "Bars");
    assert_eq!(expected_kind(&FetchWhat::CatalogList { venue: "binance".into() }), "Instruments");
    assert_eq!(
        expected_kind(&FetchWhat::Funding {
            venue: "binance".into(),
            symbol: "X".into(),
            start_ms: 0,
            end_ms: 1
        }),
        "Funding"
    );
}

/// An [`Out`] over a loopback socket, and the other end of that socket.
fn out_over_loopback(every: Duration) -> (Out, TcpStream) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let peer = TcpStream::connect(listener.local_addr().expect("the port")).expect("connect");
    let (server_side, _) = listener.accept().expect("accept");
    let out = Out {
        rid: 7,
        started: Instant::now(),
        every,
        inner: Mutex::new(OutInner {
            stream: server_side,
            last_write: Instant::now(),
            pages: 0,
            rows: 0,
            terminal: false,
        }),
    };
    (out, peer)
}

/// **No progress frame follows the terminal frame**, even when the monitor had already decided to write
/// one. The monitor reads how long the fetch has been silent and only then calls `Out::progress`; the
/// worker's terminal write can land between the two (its last write was `every` or more ago). This
/// plays that interleaving in the order the race has it, one step at a time and with no thread and no
/// sleep: look at the silence, let the terminal frame land, then do what the look said.
#[test]
fn a_progress_decision_taken_before_the_terminal_frame_writes_nothing_after_it() {
    let (out, mut peer) = out_over_loopback(Duration::ZERO);
    // 1. The monitor looks: silent for the cadence or more, so it means to write a progress frame.
    assert!(out.since_last_write() >= out.every, "the monitor's decision to write is taken");
    // 2. The worker's terminal write lands first.
    out.terminal(&Response::FetchDone { rid: 7, pages: 0, rows: 0 }).expect("the terminal frame");
    // 3. The monitor carries the decision out. It is not an error, and it writes nothing.
    out.progress(FetchState::Running).expect("a suppressed progress frame is not a fault");

    let first: Response = read_frame(&mut peer).expect("the terminal frame arrives");
    assert!(matches!(first, Response::FetchDone { rid: 7, .. }), "{first:?}");
    // Closing the server side ends the stream: whatever was written has already been delivered, in
    // order, so a stray frame would be read here instead of the end of the stream.
    drop(out);
    let after =
        read_frame::<_, Response>(&mut peer).expect_err("nothing follows the terminal frame");
    assert_eq!(after.kind(), io::ErrorKind::UnexpectedEof, "a frame followed FetchDone: {after}");
}

/// The control: before the terminal frame a progress frame IS written (so the test above fails for the
/// right reason), and every terminal kind closes the door, not only `FetchDone`.
#[test]
fn progress_is_written_until_any_terminal_frame_and_not_after() {
    for terminal in [
        Response::FetchDone { rid: 7, pages: 0, rows: 0 },
        Response::FetchCancelled { rid: 7 },
        Response::FetchFailed { rid: 7, failure: FetchFailure::Fetch("venue said no".into()) },
    ] {
        let (out, mut peer) = out_over_loopback(Duration::ZERO);
        out.progress(FetchState::Queued).expect("progress before the terminal frame");
        out.terminal(&terminal).expect("the terminal frame");
        out.progress(FetchState::Running).expect("suppressed");
        drop(out);
        let first: Response = read_frame(&mut peer).expect("the progress frame");
        assert!(matches!(first, Response::FetchProgress(_)), "{first:?}");
        let second: Response = read_frame(&mut peer).expect("the terminal frame");
        assert_eq!(
            serde_json::to_value(&second).expect("json"),
            serde_json::to_value(&terminal).expect("json"),
            "the terminal frame is the one written"
        );
        let after = read_frame::<_, Response>(&mut peer).expect_err("the stream ends");
        assert_eq!(after.kind(), io::ErrorKind::UnexpectedEof, "{after}");
    }
}
