//! [`run_user_data_forever_with_idle`]'s silent-stall watchdog, over scripted streams.
use super::*;
use std::sync::atomic::AtomicUsize;

/// The half-dead socket: open, never a frame, never an error, never a close. This is the
/// shape `StreamError::Timeout => continue` parks on forever, so it is exactly what the
/// watchdog exists to catch. Each `recv` sleeps a slice so wall-clock actually advances.
struct StalledStream {
    tick: Duration,
    ticks: Arc<AtomicUsize>,
    /// Raise this stop flag once `ticks` reaches N — lets a watchdog-DISABLED run terminate.
    stop_at: Option<(usize, Arc<AtomicBool>)>,
}

impl UserStream for StalledStream {
    fn recv(&mut self) -> Result<StreamMsg, StreamError> {
        let n = self.ticks.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some((at, stop)) = &self.stop_at
            && n >= *at
        {
            stop.store(true, Ordering::Relaxed);
        }
        std::thread::sleep(self.tick);
        Err(StreamError::Timeout)
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), StreamError> {
        Ok(())
    }
}

/// With a threshold set, a silent socket is declared dead and the session RECONNECTS — which
/// is the whole point: the re-open fires `on_reconnect`, so the resync supervisor gets its gen
/// bump and can replay whatever landed in the hole.
#[test]
fn silent_stall_trips_the_watchdog_and_reconnects() {
    let stop = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicUsize::new(0));
    let reconnects = Arc::new(AtomicUsize::new(0));
    let ticks = Arc::new(AtomicUsize::new(0));

    let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
    let open_ws = move || {
        // Stop on the SECOND open: by then the watchdog has already proven it fires.
        if opens_o.fetch_add(1, Ordering::Relaxed) + 1 >= 2 {
            stop_o.store(true, Ordering::Relaxed);
        }
        OpenOutcome::Ready(StalledStream {
            tick: Duration::from_millis(2),
            ticks: ticks_o.clone(),
            stop_at: None,
        })
    };

    let reconnects_r = reconnects.clone();
    let result = run_user_data_forever_with_idle(
        open_ws,
        |_| vec![],
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(5), // cap the post-trip backoff so the test stays quick
        None,
        move || {
            reconnects_r.fetch_add(1, Ordering::Relaxed);
        },
        Some(Duration::from_millis(20)),
    );

    assert!(result.is_ok(), "a stall is a transport hiccup, never an auth error");
    assert!(
        opens.load(Ordering::Relaxed) >= 2,
        "watchdog must end the dead session and re-open (opens={})",
        opens.load(Ordering::Relaxed)
    );
    assert!(
        reconnects.load(Ordering::Relaxed) >= 1,
        "the re-open must fire on_reconnect — that gen bump is what drives the A3 resync"
    );
}

/// `None` = the watchdog OFF: the same silent socket is tolerated indefinitely, no session end,
/// no reconnect. The arm changes nothing for a caller that does not opt in.
#[test]
fn no_threshold_tolerates_silence_forever() {
    let stop = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicUsize::new(0));
    let reconnects = Arc::new(AtomicUsize::new(0));
    let ticks = Arc::new(AtomicUsize::new(0));

    // 40 ticks x 2ms = ~80ms of silence — many times over any threshold the sibling test uses.
    let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
    let open_ws = move || {
        opens_o.fetch_add(1, Ordering::Relaxed);
        OpenOutcome::Ready(StalledStream {
            tick: Duration::from_millis(2),
            ticks: ticks_o.clone(),
            stop_at: Some((40, stop_o.clone())),
        })
    };

    let reconnects_r = reconnects.clone();
    let result = run_user_data_forever_with_idle(
        open_ws,
        |_| vec![],
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(5),
        None,
        move || {
            reconnects_r.fetch_add(1, Ordering::Relaxed);
        },
        None, // watchdog OFF
    );

    assert!(result.is_ok());
    assert!(ticks.load(Ordering::Relaxed) >= 40, "the stream really did stay silent");
    assert_eq!(opens.load(Ordering::Relaxed), 1, "no threshold ⇒ the session never ends early");
    assert_eq!(reconnects.load(Ordering::Relaxed), 0, "and therefore never reconnects");
}

/// A server that pings on a WALL-CLOCK cadence, which is the only cadence a real server has.
///
/// Never a CALL COUNT: the pump measures elapsed time, and a scheduling stall stretches the
/// wall-clock gap between pings without advancing a call count, so a call-count double lets the
/// pump see silence the "server" believes it broke. `hiccup` injects one such stall deliberately,
/// so the faithfulness is gated rather than assumed.
///
/// `ping_every: None` is the negative control — the same stream with the pings taken away.
struct PingingStream {
    ticks: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    /// Whole-test deadline, shared across re-opens so a run that reconnects still terminates.
    run_until: Instant,
    ping_every: Option<Duration>,
    last_ping: Instant,
    /// One-shot injected scheduling stall, sprung on the tick below.
    hiccup: Option<Duration>,
    hiccup_at: usize,
}

impl UserStream for PingingStream {
    fn recv(&mut self) -> Result<StreamMsg, StreamError> {
        let n = self.ticks.fetch_add(1, Ordering::Relaxed) + 1;
        if Instant::now() >= self.run_until {
            self.stop.store(true, Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(2));
        if n == self.hiccup_at
            && let Some(h) = self.hiccup.take()
        {
            std::thread::sleep(h); // the CI stall, modelled
        }
        // A real server's ping is already queued when we resume from a stall, so a gap that has
        // outrun the cadence yields a ping on the very next read. No DATA ever, only control.
        match self.ping_every {
            Some(every) if self.last_ping.elapsed() >= every => {
                self.last_ping = Instant::now();
                Ok(StreamMsg::Ping(vec![]))
            }
            _ => Err(StreamError::Timeout),
        }
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), StreamError> {
        Ok(())
    }
}

/// Threshold for the pair of runs below. 300 ms against a 6 ms ping cadence leaves ~294 ms of
/// slack for a scheduling stall (CI has produced ~21 ms ones). The run is deliberately LONGER
/// than this (see `PING_RUN`), because a threshold the run cannot reach would make the positive
/// assertion vacuous: it would pass with the pings deleted.
/// `a_ping_less_stream_is_declared_dead_at_the_same_threshold` is that control.
const PING_IDLE_THRESHOLD: Duration = Duration::from_millis(300);
/// Stream time per run — must exceed `PING_IDLE_THRESHOLD` or neither run proves anything.
const PING_RUN: Duration = Duration::from_millis(500);
/// Nominal server ping cadence.
const PING_EVERY: Duration = Duration::from_millis(6);

/// Control frames are liveness: a server that pings on a cadence SHORTER than the threshold
/// keeps the session up forever even with zero data. This is the property that makes an
/// idle-account user-data stream safe to watchdog at all — and it must survive a scheduling
/// stall, since a stalled reader is not a dead socket.
#[test]
fn server_pings_alone_keep_the_session_alive() {
    let stop = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicUsize::new(0));
    let ticks = Arc::new(AtomicUsize::new(0));
    let run_until = Instant::now() + PING_RUN;

    let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
    let open_ws = move || {
        opens_o.fetch_add(1, Ordering::Relaxed);
        OpenOutcome::Ready(PingingStream {
            ticks: ticks_o.clone(),
            stop: stop_o.clone(),
            run_until,
            ping_every: Some(PING_EVERY),
            last_ping: Instant::now(),
            // A 40 ms stall — about twice the largest CI scheduling stall seen (~21 ms).
            hiccup: Some(Duration::from_millis(40)),
            hiccup_at: 5,
        })
    };

    let result = run_user_data_forever_with_idle(
        open_ws,
        |_| vec![],
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(5),
        None,
        || {},
        Some(PING_IDLE_THRESHOLD),
    );

    assert!(result.is_ok());
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "server pings are inbound frames — they must reset the idle clock, so no reconnect \
             (a stall must not read as a dead socket either)"
    );
    assert!(
        ticks.load(Ordering::Relaxed) >= 50,
        "the stream must really have run the whole window (ticks={})",
        ticks.load(Ordering::Relaxed)
    );
}

/// The negative control for the test above, and the reason its threshold may be generous: strip
/// the pings and NOTHING else, and the watchdog must still declare the socket dead inside the
/// same run. Without this, raising `PING_IDLE_THRESHOLD` past `PING_RUN` would silently turn its
/// sibling into a test that passes whether or not pings reset the idle clock.
#[test]
fn a_ping_less_stream_is_declared_dead_at_the_same_threshold() {
    let stop = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicUsize::new(0));
    let ticks = Arc::new(AtomicUsize::new(0));
    let run_until = Instant::now() + PING_RUN;

    let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
    let open_ws = move || {
        // Stop on the SECOND open: by then the threshold has proven it is reachable in-run.
        if opens_o.fetch_add(1, Ordering::Relaxed) + 1 >= 2 {
            stop_o.store(true, Ordering::Relaxed);
        }
        OpenOutcome::Ready(PingingStream {
            ticks: ticks_o.clone(),
            stop: stop_o.clone(),
            run_until, // backstop: if the watchdog never trips, end rather than hang
            ping_every: None,
            last_ping: Instant::now(),
            hiccup: None,
            hiccup_at: 0,
        })
    };

    let result = run_user_data_forever_with_idle(
        open_ws,
        |_| vec![],
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(5),
        None,
        || {},
        Some(PING_IDLE_THRESHOLD),
    );

    assert!(result.is_ok());
    assert!(
        opens.load(Ordering::Relaxed) >= 2,
        "the threshold must be reachable inside this run, or its sibling proves nothing \
             (opens={})",
        opens.load(Ordering::Relaxed)
    );
}
