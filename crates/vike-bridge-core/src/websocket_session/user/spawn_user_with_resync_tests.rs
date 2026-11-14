//! [`spawn_user_with_resync`] from a spec over scripted streams: its threads' names, both producers
//! through the fence, the bare lane without one; [`spawn_user`]'s hook on a re-open; and [`dial_user_stream`]'s window.
use super::*;
use crate::scripted::WebsocketSessionUserStreamScript;
use vike_execution::event_channel;

/// The spawn's waits are the mechanism's own (a 1 s reconnect, poll and settle), so a reconnect replay
/// lands about 3 s in; this bounds a BROKEN run only.
const SPAWN_DEADLINE: Duration = Duration::from_secs(30);

/// A session that says nothing until the pump is stopped.
struct Quiet;

impl WebsocketSessionUserStream for Quiet {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        std::thread::sleep(Duration::from_millis(1));
        Err(WebsocketSessionUserError::Timeout)
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        Ok(())
    }
}

/// One frame, then the session's end on the first open; quiet sessions after it.
enum Session {
    Script(WebsocketSessionUserStreamScript),
    Quiet(Quiet),
}

impl WebsocketSessionUserStream for Session {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        match self {
            Session::Script(s) => s.recv(),
            Session::Quiet(q) => q.recv(),
        }
    }
    fn pong(&mut self, payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        match self {
            Session::Script(s) => s.pong(payload),
            Session::Quiet(q) => q.pong(payload),
        }
    }
}

/// The venue's open over the script: one frame (a fill of `in-flight`) and the session's end, then quiet.
fn scripted_open()
-> impl FnMut(&AtomicBool) -> WebsocketSessionUserOpenOutcome<Session> + Send + 'static {
    let mut opens = 0;
    move |_stop: &AtomicBool| {
        opens += 1;
        WebsocketSessionUserOpenOutcome::Ready(if opens == 1 {
            Session::Script(WebsocketSessionUserStreamScript::new(vec![
                Ok(WebsocketSessionUserMessage::Text(r#"{"fill":"t-live"}"#.into())),
                Err(WebsocketSessionUserError::Closed("the venue hung up".into())),
            ]))
        } else {
            Session::Quiet(Quiet)
        })
    }
}

/// The script's one frame: a fill of `in-flight`.
fn decode_fill(_frame: &Value) -> Vec<Event> {
    vec![fill_at("in-flight", "t-live", vike_model::now_ms())]
}

/// The spawn over the script.
fn spawn_scripted(
    symbol: &str,
    events: EventSender,
    fence: Option<Arc<SubmitFence>>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
) -> WebsocketSessionUserResyncHandle {
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: "test",
        lane: "test-lane",
        symbol: symbol.to_string(),
        events,
        fence,
        resync_fetch,
        on_reconcile: None,
        open: scripted_open(),
        decode: decode_fill,
        keepalive: None,
        idle_threshold: None,
    })
}

/// Poll the lane until its first fill, which must be `t-live`.
fn first_fill_is_the_live_one(ingest: &mut tokio::sync::mpsc::Receiver<vike_execution::Ingest>) {
    let deadline = Instant::now() + SPAWN_DEADLINE;
    loop {
        if let Ok(vike_execution::Ingest::Event(Event::Fill(f))) = ingest.try_recv() {
            assert_eq!(f.trade_id.to_string(), "t-live");
            return;
        }
        assert!(Instant::now() < deadline, "the pump's fill never reached the lane");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The fence's whole point, held for the builder: with `in-flight`'s submit open, the pump's own fill
/// AND the reconnect replay's both wait behind it. One producer straight to the lane and the count stays
/// short of two.
#[test]
fn both_producers_wait_behind_the_fence_and_the_threads_carry_the_lane() {
    let (events, _ingest) = event_channel(16);
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let (marked, is_marked) = mpsc::channel();
    let fence_s = Arc::clone(&fence);
    let submit = std::thread::spawn(move || {
        fence_s.submit("in-flight", || {
            marked.send(()).unwrap();
            let deadline = Instant::now() + SPAWN_DEADLINE;
            while fence_s.parked("in-flight") < 2 {
                assert!(Instant::now() < deadline, "a producer went past the fence");
                std::thread::sleep(Duration::from_millis(2));
            }
            Vec::new()
        });
    });
    is_marked.recv().unwrap();

    let replay = || vec![fill_at("in-flight", "t-replay", vike_model::now_ms())];
    let feed = spawn_scripted("BTCUSDT", events, Some(fence), replay);
    assert_eq!(feed.pump.thread().name(), Some("test-lane-userdata-BTCUSDT"));
    assert_eq!(feed.resync.thread().name(), Some("test-lane-resync-BTCUSDT"));

    submit.join().unwrap();
    feed.shutdown().unwrap();
}

/// No fence: the pump's events go straight to the lane, and a stream of the whole account names its
/// threads without a symbol.
#[test]
fn without_a_fence_the_pump_sends_to_the_lane() {
    let (events, mut ingest) = event_channel(16);
    let feed = spawn_scripted("", events, None, Vec::new);
    assert_eq!(feed.pump.thread().name(), Some("test-lane-userdata"));
    assert_eq!(feed.resync.thread().name(), Some("test-lane-resync"));
    first_fill_is_the_live_one(&mut ingest);
    feed.shutdown().unwrap();
}

/// [`spawn_user`], the stream with no replay: one named thread, its fill on the lane, and the venue's own hook fired
/// on the RE-open (the scripted session ends, the next opens) and never on the first open.
#[test]
fn spawn_user_fires_its_hook_on_the_reopen_alone() {
    let (events, mut ingest) = event_channel(16);
    let (reopened, reopens) = mpsc::channel();
    let feed = spawn_user(WebsocketSessionUserSpec {
        venue: "test",
        lane: "test-lane",
        symbol: String::new(),
        events,
        fence: None,
        open: scripted_open(),
        decode: decode_fill,
        keepalive: None,
        idle_threshold: None,
        on_reconnect: move || {
            let _ = reopened.send(());
        },
    });
    assert_eq!(feed.handle.thread().name(), Some("test-lane-userdata"));
    first_fill_is_the_live_one(&mut ingest);
    reopens.recv_timeout(SPAWN_DEADLINE).expect("the re-open fired the hook");
    assert!(reopens.try_recv().is_err(), "the first open fired nothing");
    feed.shutdown().unwrap();
}

/// The dial takes its window: a peer that accepts and says nothing fails it as the open's transport
/// failure, in the lanes' words.
#[test]
fn the_dial_gives_up_on_a_silent_peer_inside_its_window() {
    let dial = crate::silent_peer::dial_silent_peer("127.0.0.1:0", |url| {
        dial_user_stream(url, Duration::from_millis(300)).map(drop)
    });
    assert!(dial.failure.starts_with("connect: "), "the lanes' words: {}", dial.failure);
}
