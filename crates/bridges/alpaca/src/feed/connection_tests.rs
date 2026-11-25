//! [`RouteLink`] against a loopback WebSocket venue: one dial for every lane, one frame per key, every frame to every
//! lane, the server's ping answered as liveness, and a socket that ends (a server close, the idle bound) told to every
//! lane, with the next lane dialling a new one.

use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, Sender};

use tungstenite::Message;
use vike_venues::venue_timings::VenueTimingsReconnect;

use super::*;

/// What the venue does next.
enum Say {
    Text(&'static str),
    Ping,
    Close,
}

/// A loopback venue: it accepts one socket after another, reports every text frame it hears (`pong` for a pong,
/// `closed` when a socket ends) and says what it is told. Its thread outlives the test.
fn venue() -> (String, Receiver<String>, Sender<Say>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("ws://{}/v1beta3/crypto/us", listener.local_addr().expect("addr"));
    let (heard_tx, heard) = mpsc::channel();
    let (say, said) = mpsc::channel::<Say>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let Ok(mut ws) = tungstenite::accept(stream) else { return };
            // After the handshake: a read timeout there would cut it short.
            ws.get_ref().set_read_timeout(Some(Duration::from_millis(20))).expect("a read timeout");
            loop {
                while let Ok(next) = said.try_recv() {
                    let _ = match next {
                        Say::Text(text) => ws.send(Message::Text(text.into())),
                        Say::Ping => ws.send(Message::Ping(Vec::new().into())),
                        Say::Close => ws.close(None),
                    };
                }
                match ws.read() {
                    Ok(Message::Text(text)) => {
                        let _ = heard_tx.send(text.as_str().to_string());
                    }
                    Ok(Message::Pong(_)) => {
                        let _ = heard_tx.send("pong".to_string());
                    }
                    Ok(_) => {}
                    Err(e) if is_timeout(&e) => {}
                    Err(_) => {
                        let _ = heard_tx.send("closed".to_string());
                        break;
                    }
                }
            }
        }
    });
    (url, heard, say)
}

/// The next `n` things the venue heard.
fn next(heard: &Receiver<String>, n: usize) -> Vec<String> {
    (0..n)
        .map(|_| heard.recv_timeout(Duration::from_secs(5)).expect("the venue heard it"))
        .collect()
}

/// A link at `url` polling every 20 ms, with `idle` as its idle bound.
fn link(url: String, idle: Option<Duration>) -> Arc<RouteLink> {
    let token = TokenSource::with_clock_and_exchanger(
        Box::new(|| 1_000_000),
        Box::new(|_body: &str| Ok(("tok".to_string(), 900))),
    );
    Arc::new(RouteLink::new(
        StreamRoute::Crypto,
        url,
        Arc::new(token),
        WebsocketSessionKnobs {
            subscribe_frame: false,
            keepalive_every: None,
            ack_timeout: None,
            idle_threshold: idle,
            read_timeout: Duration::from_millis(20),
            backoff: VenueTimingsReconnect::fixed(Duration::from_secs(3), "a test"),
            connect_timeout: Some(Duration::from_secs(5)),
        },
    ))
}

/// The next frame a lane reads, past its poll ticks.
fn frame(lane: &mut LaneStream) -> Result<String, WebsocketSessionUserError> {
    for _ in 0..250 {
        match lane.read_frame() {
            Err(WebsocketSessionUserError::Timeout) => continue,
            other => return other,
        }
    }
    panic!("the lane read nothing in 5 s")
}

/// **One socket for every lane of a route, one frame per key, every frame to every lane**: the first lane dials and
/// authenticates; a second lane of the same key writes nothing, a lane of a new key one `subscribe`; a frame reaches
/// each lane; the key's last lane writes the `unsubscribe`, and the route's last lane closes the socket.
#[test]
fn one_socket_carries_every_lane_with_one_frame_per_key() {
    let (url, heard, say) = venue();
    let link = link(url, None);
    let stop = AtomicBool::new(false);
    let mut quotes = link.attach(&stop, Verb::Quotes, "BTC/USD").expect("the first lane dials");
    let mut again = link.attach(&stop, Verb::Quotes, "BTC/USD").expect("the same key");
    let mut trades = link.attach(&stop, Verb::Trades, "BTC/USD").expect("a new key");
    assert_eq!(
        next(&heard, 3),
        [
            r#"{"action":"auth","key":"oauth","secret":"tok"}"#,
            r#"{"action":"subscribe","quotes":["BTC/USD"]}"#,
            r#"{"action":"subscribe","trades":["BTC/USD"]}"#,
        ]
    );
    say.send(Say::Text(r#"[{"T":"q","S":"BTC/USD"}]"#)).expect("the venue");
    for lane in [&mut quotes, &mut again, &mut trades] {
        assert_eq!(frame(lane), Ok(r#"[{"T":"q","S":"BTC/USD"}]"#.to_string()));
    }
    drop(again);
    drop(quotes);
    assert_eq!(next(&heard, 1), [r#"{"action":"unsubscribe","quotes":["BTC/USD"]}"#]);
    drop(trades);
    assert_eq!(next(&heard, 1), ["closed"], "the route's last lane closes the socket");
    link.shutdown();
}

/// **A key let go while another lane takes it ends subscribed under the lane that holds it.** Two lanes attach and drop
/// one key as fast as they can on one socket (a lane of another key keeps it open), then one lane keeps the key. The
/// venue hears the key's frames alternate `subscribe` / `unsubscribe` and end on a `subscribe`: a frame queued out of the
/// order its decision was taken in shows as two of a kind in a row, and as a key left unsubscribed reading `LIVE`.
#[test]
fn a_key_let_go_while_another_lane_takes_it_ends_subscribed() {
    let (url, heard, _say) = venue();
    let link = link(url, None);
    let stop = AtomicBool::new(false);
    let _open = link.attach(&stop, Verb::Trades, "BTC/USD").expect("the first lane dials");
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                for _ in 0..500 {
                    drop(link.attach(&stop, Verb::Quotes, "BTC/USD").expect("the same socket"));
                }
            });
        }
    });
    let _kept = link.attach(&stop, Verb::Quotes, "BTC/USD").expect("the same socket");
    // Queued after every frame above, so the venue hears it last.
    let _last = link.attach(&stop, Verb::Bars, "BTC/USD").expect("the same socket");
    let subscribe = r#"{"action":"subscribe","quotes":["BTC/USD"]}"#;
    let unsubscribe = r#"{"action":"unsubscribe","quotes":["BTC/USD"]}"#;
    let last = r#"{"action":"subscribe","bars":["BTC/USD"]}"#;
    let mut subscribed = false;
    let (mut subscribes, mut frames) = (0, 0);
    loop {
        let frame = next(&heard, 1).remove(0);
        if frame == last {
            break;
        }
        frames += 1;
        if frame == subscribe {
            assert!(!subscribed, "two subscribes in a row, frame {frames}");
            subscribed = true;
            subscribes += 1;
        } else if frame == unsubscribe {
            assert!(subscribed, "an unsubscribe of a key not subscribed, frame {frames}");
            subscribed = false;
        }
    }
    assert!(subscribed, "the key ends unsubscribed under the lane that holds it");
    assert!(subscribes > 1, "the lanes never let the key go: {subscribes} subscribe(s)");
    link.shutdown();
}

/// **The server's ping is answered and is the socket's liveness**, and a socket the server closes is told to every
/// lane; the next lane dials a new one.
#[test]
fn a_ping_is_answered_and_a_server_close_is_told_to_every_lane() {
    let (url, heard, say) = venue();
    let link = link(url, Some(Duration::from_secs(60)));
    let stop = AtomicBool::new(false);
    let mut lane = link.attach(&stop, Verb::Quotes, "AAPL").expect("dial");
    assert_eq!(next(&heard, 2).len(), 2, "the auth and the subscribe");
    std::thread::sleep(Duration::from_millis(100));
    let before = lane.since_last_frame();
    say.send(Say::Ping).expect("the venue");
    assert_eq!(next(&heard, 1), ["pong"]);
    assert!(lane.since_last_frame() < before, "the ping is the lane's liveness");
    say.send(Say::Close).expect("the venue");
    assert_eq!(frame(&mut lane), Err(WebsocketSessionUserError::Closed("server closed".into())));
    drop(lane);
    let _again = link.attach(&stop, Verb::Quotes, "AAPL").expect("a new socket");
    let after = next(&heard, 3);
    assert_eq!(after[0], "closed");
    assert_eq!(after[2], r#"{"action":"subscribe","quotes":["AAPL"]}"#, "{after:?}");
    link.shutdown();
}

/// **A socket silent past the idle bound is ended by its reader**, and every lane on it is told why.
#[test]
fn a_silent_socket_is_ended_at_the_idle_bound() {
    let (url, heard, _say) = venue();
    let link = link(url, Some(Duration::from_millis(300)));
    let stop = AtomicBool::new(false);
    let mut lane = link.attach(&stop, Verb::Trades, "BTC/USD").expect("dial");
    assert_eq!(next(&heard, 2).len(), 2, "the auth and the subscribe");
    let Err(WebsocketSessionUserError::Closed(why)) = frame(&mut lane) else {
        panic!("the silent socket ends")
    };
    assert_eq!(why, AlpacaFeedRefusal::SilentStall { secs: 0 }.to_string(), "the 300 ms bound");
    link.shutdown();
}

/// A stopped lane is refused before any dial.
#[test]
fn a_stopped_lane_dials_nothing() {
    let link = link("ws://127.0.0.1:9".into(), None);
    let Err(why) = link.attach(&AtomicBool::new(true), Verb::Bars, "AAPL") else {
        panic!("a stopped lane is refused")
    };
    assert_eq!(why, "stop requested");
}
