//! The link's demultiplexer, its subscription bookkeeping and its login guard, over channels: no socket and no
//! login. The socket half (one login and one session for several epics, a connection that ends, a refused
//! subscription beside a healthy one) is driven end to end by `crates/bridges/ig/tests/offline/quote_feed.rs`.

use super::*;

const SCHEMA: &str = "BIDPRICE1 ASKPRICE1";

fn config() -> IgConfig {
    IgConfig {
        api_key: "key".to_string(),
        identifier: "ident".to_string(),
        password: "pw".to_string(),
        rest_base: "http://127.0.0.1:9".to_string(),
        account_pin: None,
    }
}

/// `epic`'s quote item, named after the account the way production's `Lane::item` does.
fn quote_item(epic: &'static str) -> impl Fn(&str) -> (&'static str, String) {
    move |account| ("Pricing", format!("PRICE:{account}:{epic}"))
}

/// A link that already holds a live connection (generation 7), and the queue its reader would write.
struct Wire {
    link: Arc<Link>,
    conn: ConnHandle,
    queued: Receiver<Cmd>,
}

fn wire() -> Wire {
    let link = Arc::new(Link::new(config(), Duration::from_secs(1800)));
    let (cmds, queued) = mpsc::channel();
    let conn = ConnHandle {
        generation: 7,
        cmds,
        routes: Routes::default(),
        next_sub: Arc::new(AtomicU32::new(1)),
        account: "ACC1".to_string(),
    };
    lock(&link.state).conn = Some(conn.clone());
    Wire { link, conn, queued }
}

impl Wire {
    fn attach(&self, epic: &'static str) -> Result<LaneStream, AttachErr> {
        self.link.attach(&AtomicBool::new(false), &quote_item(epic), SCHEMA)
    }

    /// Everything a lane has asked the reader to write so far.
    fn written(&self) -> Vec<String> {
        let mut frames = Vec::new();
        while let Ok(Cmd::Send(text)) = self.queued.try_recv() {
            frames.push(text);
        }
        frames
    }

    fn subs(&self) -> Vec<u32> {
        let mut subs: Vec<u32> = lock(&self.conn.routes).keys().copied().collect();
        subs.sort_unstable();
        subs
    }

    /// The reader's side of this wire, for `route_line` and `end`.
    fn reader(&self) -> ReaderEnv {
        let (_unused, cmds) = mpsc::channel();
        ReaderEnv {
            cmds,
            routes: Arc::clone(&self.conn.routes),
            state: Arc::clone(&self.link.state),
            generation: self.conn.generation,
            stop: Arc::new(AtomicBool::new(false)),
            idle: None,
            relogin_after: Duration::from_secs(1800),
        }
    }
}

/// What a lane has been handed so far: its lines, then whether the connection was closed on it.
fn drain(rx: &Receiver<Inbound>) -> (Vec<String>, Option<String>) {
    let (mut lines, mut closed) = (Vec::new(), None);
    while let Ok(inbound) = rx.try_recv() {
        match inbound {
            Inbound::Line(line) => lines.push(line),
            Inbound::Closed(why) => closed = Some(why),
        }
    }
    (lines, closed)
}

/// A reader env with a route for each of `subs`, and what each lane would read.
fn demux(subs: &[u32]) -> (ReaderEnv, Vec<Receiver<Inbound>>) {
    let w = wire();
    let env = w.reader();
    let lanes = subs
        .iter()
        .map(|sub| {
            let (tx, rx) = mpsc::channel();
            lock(&env.routes).insert(*sub, tx);
            rx
        })
        .collect();
    (env, lanes)
}

fn fresh_auth() -> LsAuth {
    LsAuth {
        ws_url: "ws://127.0.0.1:9/lightstreamer".to_string(),
        user: "ACC1".to_string(),
        password: "CST-abc|XST-def".to_string(),
        logged_in: Instant::now(),
    }
}

#[test]
fn lanes_subscribe_on_the_live_connection_with_ids_of_their_own_and_dial_nothing() {
    let w = wire();
    let (one, two) = (w.attach("CS.D.EURUSD.MINI.IP"), w.attach("CS.D.GBPUSD.MINI.IP"));
    assert!(one.is_ok() && two.is_ok());
    assert_eq!(
        w.written(),
        [
            subscribe_request(
                1,
                1,
                "Pricing",
                "MERGE",
                "PRICE:ACC1:CS.D.EURUSD.MINI.IP",
                SCHEMA,
                true
            ),
            subscribe_request(
                2,
                2,
                "Pricing",
                "MERGE",
                "PRICE:ACC1:CS.D.GBPUSD.MINI.IP",
                SCHEMA,
                true
            ),
        ],
        "each lane's add names its own id, as subscription and as request"
    );
    assert_eq!(w.subs(), [1, 2]);
    let (login, failure) = {
        let state = lock(&w.link.state);
        (state.auth.is_some(), state.failed.is_some())
    };
    assert!(!login && !failure, "no login, no dial");
}

#[test]
fn dropping_a_lane_ends_its_subscription_and_nothing_else() {
    let w = wire();
    let first = w.attach("CS.D.EURUSD.MINI.IP").expect("first lane");
    let _second = w.attach("CS.D.GBPUSD.MINI.IP").expect("second lane");
    w.written();
    drop(first);
    assert_eq!(w.written(), [unsubscribe_request(DELETE_REQ_BASE + 1, 1)]);
    assert_eq!(w.subs(), [2], "the other lane keeps its route");
    assert!(w.link.is_current(7), "and the connection stays");
}

#[test]
fn a_lane_of_a_connection_that_ended_ends_quietly_when_it_is_dropped() {
    let w = wire();
    let lane = w.attach("CS.D.EURUSD.MINI.IP").expect("lane");
    w.written();
    end(&w.reader(), "the socket closed");
    drop(lane);
    assert!(
        w.written().is_empty(),
        "a dead connection took the subscription with it: nothing to delete"
    );
}

#[test]
fn a_lane_that_registers_on_a_connection_that_just_ended_is_refused_not_left_waiting() {
    let w = wire();
    let seen_by_the_lane = w.conn.clone();
    end(&w.reader(), "END,31,closed");
    // The lane got `conn` before the reader ended and registers after it drained: what the drain found is
    // gone, so only the check against the live connection can tell this lane.
    let refused = w.link.subscribe(&seen_by_the_lane, &quote_item("CS.D.EURUSD.MINI.IP"), SCHEMA);
    assert!(
        matches!(&refused, Err(AttachErr::Transport(why)) if why.contains("closed while subscribing")),
        "{:?}",
        refused.err()
    );
    assert!(w.subs().is_empty(), "no route is left behind");
    assert!(w.written().is_empty(), "and no add is queued on a dead reader");
}

#[test]
fn a_reader_that_is_gone_forgets_its_connection_and_refuses_the_lane() {
    let w = wire();
    let Wire { link, conn, queued } = w;
    drop(queued);
    let refused = link.subscribe(&conn, &quote_item("CS.D.EURUSD.MINI.IP"), SCHEMA);
    assert!(matches!(refused, Err(AttachErr::Transport(_))));
    assert!(!link.is_current(7), "the next lane dials a new connection");
    assert!(lock(&conn.routes).is_empty());
}

#[test]
fn the_forty_first_subscription_is_refused_in_words_and_never_queued() {
    let w = wire();
    let held: Vec<_> = (0..MAX_SUBSCRIPTIONS)
        .map(|_| w.attach("CS.D.EURUSD.MINI.IP").expect("within the limit"))
        .collect();
    assert_eq!(w.written().len(), MAX_SUBSCRIPTIONS);
    let refused = w.attach("CS.D.GBPUSD.MINI.IP");
    assert!(
        matches!(&refused, Err(AttachErr::Transport(why)) if why.contains("40 subscriptions")),
        "{:?}",
        refused.err()
    );
    assert!(w.written().is_empty(), "an add IG would refuse is not sent");
    drop(held);
}

#[test]
fn a_stopped_link_dials_nothing_and_says_so() {
    let link = Arc::new(Link::new(config(), Duration::from_secs(1800)));
    link.raise_stop();
    let refused = link.attach(&AtomicBool::new(false), &quote_item("CS.D.EURUSD.MINI.IP"), SCHEMA);
    assert!(matches!(refused, Err(AttachErr::Transport(why)) if why.contains("stop")));
    let lane_stopped = Arc::new(Link::new(config(), Duration::from_secs(1800)));
    let refused =
        lane_stopped.attach(&AtomicBool::new(true), &quote_item("CS.D.EURUSD.MINI.IP"), SCHEMA);
    assert!(matches!(refused, Err(AttachErr::Transport(why)) if why.contains("stop")));
}

#[test]
fn a_link_nobody_holds_any_more_stops_its_reader() {
    let link = Arc::new(Link::new(config(), Duration::from_secs(1800)));
    let reader_stop = Arc::clone(&link.stop);
    let lane_view = Arc::clone(&link);
    drop(link);
    assert!(!reader_stop.load(Ordering::Relaxed), "a lane still holds it");
    drop(lane_view);
    assert!(reader_stop.load(Ordering::Relaxed), "the last holder ended the reader");
}

#[test]
fn a_lane_asking_right_behind_a_failed_dial_gets_that_answer_without_dialing() {
    let link = Arc::new(Link::new(config(), Duration::from_secs(1800)));
    let why = AttachErr::Login {
        text: "IG error 401: error.security.invalid-details".to_string(),
        for_good: true,
    };
    lock(&link.state).failed = Some((Instant::now(), why.clone()));
    let refused = link.attach(&AtomicBool::new(false), &quote_item("CS.D.EURUSD.MINI.IP"), SCHEMA);
    assert_eq!(refused.err(), Some(why), "the shared answer, with its verdict intact");
}

#[test]
fn a_lane_stream_hands_over_its_lines_then_its_close_and_never_writes() {
    let w = wire();
    let mut lane = w.attach("CS.D.EURUSD.MINI.IP").expect("lane");
    lane.poll = Duration::from_millis(5);
    let env = w.reader();
    let mut delivered = false;
    assert_eq!(route_line("U,1,1,0.84279|0.84288|1792433954526|DEAL", &env, &mut delivered), None);
    assert_eq!(lane.read_frame().expect("the update"), "U,1,1,0.84279|0.84288|1792433954526|DEAL");
    assert!(matches!(lane.read_frame(), Err(StreamError::Timeout)), "a quiet link is a read tick");
    end(&env, "the socket closed");
    assert!(
        matches!(lane.read_frame(), Err(StreamError::Closed(why)) if why == "the socket closed")
    );
    assert!(matches!(lane.send_text("control\r\n"), Err(StreamError::Closed(_))));
}

#[test]
fn an_update_goes_to_the_lane_that_owns_it_under_the_lane_local_id() {
    let (env, lanes) = demux(&[1, 2]);
    let mut delivered = false;
    for (line, lane_line) in [
        ("U,2,1,0.84|0.85|1792433954526|DEAL", "U,1,1,0.84|0.85|1792433954526|DEAL"),
        ("SUBOK,2,1,4", "SUBOK,1,1,4"),
        ("EOS,2,1", "EOS,1,1"),
        ("UNSUB,2", "UNSUB,1"),
    ] {
        assert_eq!(route_line(line, &env, &mut delivered), None, "{line}");
        assert_eq!(
            drain(&lanes[1]).0,
            [lane_line],
            "{line}: the owner reads it as its own subscription 1"
        );
        assert!(drain(&lanes[0]).0.is_empty(), "{line}: and nobody else reads it");
    }
}

#[test]
fn a_line_for_a_subscription_nobody_holds_is_dropped() {
    let (env, lanes) = demux(&[1]);
    let mut delivered = false;
    assert_eq!(route_line("U,9,1,1|2|3|DEAL", &env, &mut delivered), None);
    assert_eq!(route_line("U,not-a-number,1,x", &env, &mut delivered), None);
    assert!(drain(&lanes[0]).0.is_empty());
}

#[test]
fn a_reqerr_goes_to_the_lane_whose_add_it_answers_and_a_deletes_goes_to_nobody() {
    let (env, lanes) = demux(&[1, 2]);
    let mut delivered = false;
    assert_eq!(route_line("REQERR,2,21,Invalid group", &env, &mut delivered), None);
    assert_eq!(drain(&lanes[1]).0, ["REQERR,2,21,Invalid group"]);
    assert!(drain(&lanes[0]).0.is_empty());
    let refused_delete = format!("REQERR,{},19,Unknown subscription", DELETE_REQ_BASE + 2);
    assert_eq!(route_line(&refused_delete, &env, &mut delivered), None);
    assert!(drain(&lanes[0]).0.is_empty() && drain(&lanes[1]).0.is_empty());
    assert_eq!(
        route_line("REQOK,2", &env, &mut delivered),
        None,
        "an accepted request is nobody's news"
    );
    assert!(drain(&lanes[1]).0.is_empty());
}

#[test]
fn a_line_without_a_subscription_id_is_every_lanes_heartbeat() {
    let (env, lanes) = demux(&[1, 2, 3]);
    let mut delivered = false;
    for line in ["PROBE", "SYNC,12", "NOOP,x"] {
        assert_eq!(route_line(line, &env, &mut delivered), None, "{line}");
        for lane in &lanes {
            assert_eq!(drain(lane).0, [line]);
        }
    }
}

#[test]
fn the_frames_that_end_a_session_end_the_connection_and_every_lane_reads_them() {
    for line in ["END,31,closed by the server", "LOOP,0", "CONERR,41,session gone"] {
        let (env, lanes) = demux(&[1, 2]);
        let mut delivered = false;
        assert_eq!(route_line(line, &env, &mut delivered), Some(line.to_string()), "{line}");
        for lane in &lanes {
            assert_eq!(drain(lane).0, [line], "{line}");
        }
    }
}

#[test]
fn a_lane_that_is_gone_loses_its_route_and_the_others_still_read() {
    let (env, mut lanes) = demux(&[1, 2]);
    let mut delivered = false;
    drop(lanes.remove(0));
    assert_eq!(route_line("U,1,1,1|2|3|DEAL", &env, &mut delivered), None);
    assert_eq!(lock(&env.routes).keys().copied().collect::<Vec<_>>(), [2]);
    assert_eq!(route_line("PROBE", &env, &mut delivered), None);
    assert_eq!(drain(&lanes[0]).0, ["PROBE"]);
}

#[test]
fn the_first_update_of_a_connection_ends_the_run_of_failures_and_a_subok_does_not() {
    let (env, _lanes) = demux(&[1]);
    lock(&env.state).guard.spent = true;
    let mut delivered = false;
    route_line("SUBOK,1,1,4", &env, &mut delivered);
    assert!(!delivered && lock(&env.state).guard.spent, "an acknowledgement proves nothing");
    route_line("U,1,1,0.84|0.85|1|DEAL", &env, &mut delivered);
    assert!(delivered && !lock(&env.state).guard.spent, "an update gives the free drop back");
}

#[test]
fn a_connection_that_ends_tells_every_lane_and_forgets_only_itself() {
    let (env, lanes) = demux(&[1, 2]);
    assert_eq!(lock(&env.state).conn.as_ref().map(|c| c.generation), Some(7));

    // A reader of an OLDER connection (its own, empty routes) must not forget a newer one.
    let mut stale = env.reader_like();
    stale.generation = 6;
    stale.routes = Routes::default();
    end(&stale, "an old socket closed");
    assert_eq!(lock(&env.state).conn.as_ref().map(|c| c.generation), Some(7));
    assert_eq!(lock(&env.routes).len(), 2, "and must not tell the newer connection's lanes");

    end(&env, "the socket closed");
    assert!(lock(&env.state).conn.is_none(), "the next lane dials");
    for lane in &lanes {
        let (lines, closed) = drain(lane);
        assert!(lines.is_empty());
        assert_eq!(closed.as_deref(), Some("the socket closed"));
    }
    assert!(lock(&env.routes).is_empty(), "nothing is left to tell twice");
}

impl ReaderEnv {
    /// A second env over the same routes and state (the receiver is a stranger's), for a stale-reader test.
    fn reader_like(&self) -> ReaderEnv {
        let (_unused, cmds) = mpsc::channel();
        ReaderEnv {
            cmds,
            routes: Arc::clone(&self.routes),
            state: Arc::clone(&self.state),
            generation: self.generation,
            stop: Arc::clone(&self.stop),
            idle: self.idle,
            relogin_after: self.relogin_after,
        }
    }
}

/// The cached login is dropped by the FIRST `CONERR` of a run only: a second after a fresh login is not
/// expiry, and re-logging-in on every attempt is a `POST /session` every few seconds. A delivered update gives
/// the free drop back.
#[test]
fn a_cached_login_is_dropped_by_the_first_conerr_of_a_run_only() {
    let mut guard = ReloginGuard::default();
    let guard_after = Duration::from_secs(1800);
    assert!(
        guard.allow(Duration::ZERO, guard_after),
        "the first CONERR of a run may drop the login"
    );
    assert!(
        !guard.allow(Duration::ZERO, guard_after) && !guard.allow(Duration::ZERO, guard_after),
        "later ones keep a login this young"
    );
    guard.delivered();
    assert!(guard.allow(Duration::ZERO, guard_after), "a recovered client has its one login back");
}

/// A run only ends on a delivered update, so a client refused for longer than a token lives used to keep an
/// EXPIRED pair for ever after its one drop (`CONERR 1` on every dial until a restart). A login older than the
/// guard is dropped again by a later `CONERR` of the same run, ONCE: the fresh login it is replaced by is young,
/// and a `CONERR` on it spends no `POST /session`.
#[test]
fn a_login_older_than_the_guard_is_dropped_again_once_by_a_later_conerr_of_the_same_run() {
    let mut guard = ReloginGuard::default();
    let after = Duration::from_secs(30 * 60);
    let age = Duration::from_secs;
    // the first CONERR of the run drops the login whatever its age (the common cause is expiry)
    assert!(guard.allow(age(5), after));
    // a later one on a young login keeps it ...
    assert!(!guard.allow(age(60), after));
    assert!(!guard.allow(age(29 * 60 + 59), after), "a second short of the guard");
    // ... and on one that has outlived the guard drops it: hours into a stuck run, the pair is replaced
    assert!(guard.allow(age(30 * 60), after), "the guard is reached");
    assert!(guard.allow(age(7 * 3600), after), "an old pair is always replaceable");
    // the replacement is young again: the next CONERR keeps it, so a stuck client is bounded to one
    // `POST /session` per guard, not one per attempt
    assert!(!guard.allow(Duration::ZERO, after));
    assert!(!guard.allow(age(10 * 60), after));
    // a test (or a future retune) shortens the guard and nothing else changes
    assert!(guard.allow(age(3), Duration::from_secs(2)));
    assert!(!guard.allow(age(1), Duration::from_secs(2)));
}

#[test]
fn a_conerr_drops_the_kept_login_through_the_guard_and_not_otherwise() {
    let state = Mutex::new(State::default());
    let after = Duration::from_secs(1800);
    lock(&state).auth = Some(fresh_auth());
    on_conerr(&state, after);
    assert!(lock(&state).auth.is_none(), "the first CONERR of a run drops it");
    lock(&state).auth = Some(fresh_auth());
    on_conerr(&state, after);
    assert!(lock(&state).auth.is_some(), "the second, on a login made just now, keeps it");
    lock(&state).guard.delivered();
    on_conerr(&state, after);
    assert!(lock(&state).auth.is_none(), "a delivered update gave the drop back");
}
