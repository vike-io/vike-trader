//! **A recovered kline session must clear the status string it faulted with** —
//! `vike_bybit::market_feed`'s `feed_body`, driven over scripted streams.
//!
//! # The defect, measured on the live box
//!
//! the CI box's trading daemon suppressed bybit's reconcile leg once a minute for 42 hours — 2,516
//! occurrences, no gaps — while the venue was perfectly healthy: four ESTABLISHED sockets, ~2,100
//! core events a minute, the public API answering in 204 ms, zero faults logged in the window. The
//! wallet figure it should have been re-reading stayed frozen at one value across all 2,209
//! summaries.
//!
//! The gate reads a STRING, not a connection. `vike_ops::reconcile_config`'s
//! `build_recon_config` looks the venue up in `vike_tradehub::feeds`'s
//! `LiveFeeds::recon_feed_statuses` and maps the TEXT through `health_from_feed_status`. And the
//! write path had no way back: `feed_main` published its healthy string ONCE, before the pump
//! started, and the pump's only status writer was the session-ERROR hook. One transient blip wrote
//! `"… ws error (reconnecting): …"`, the driver silently reconnected and resumed streaming, and
//! nothing ever rewrote it. Only a process restart cleared it — and a deliberate restart on
//! 2026-09-10 04:46 proved that, with the latch re-arming twenty minutes later.
//!
//! # What this file pins
//!
//! The BEHAVIOUR, at the venue level, over the real closures: session 1 faults, session 2
//! succeeds, and the status must read healthy afterwards. It drives the production
//! `route_frame`/hook pair through a real [`FeedCtx`] holding a real `Arc<Mutex<String>>`, and
//! classifies the result with `vike_model::parse_feed_status` — the same parser
//! `health_from_feed_status` sits on. (This crate cannot name `vike_ops` — layer direction — but
//! `vike-model` is below every bridge, and the `Error → Degraded` half is already pinned by
//! `reconcile_config`'s own unit tests, so classifying the string here closes the loop.)
//!
//! # Kill proof
//!
//! Remove the `SessionStatus::Live` arm from `feed_body` and
//! [`a_recovered_session_clears_the_faulted_status`]'s second assertion reads `Error` — which is
//! exactly what today's shipped code does and what ran on the CI box for 42 hours.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vike_bridge_core::scripted::{ScriptStep, ScriptedStream};
use vike_bridge_core::{MarketStream, StreamError};
use vike_bybit::market_feed::{FeedCtx, feed_body, pump_opts, subscribe_frame};
use vike_data::NoopSink;
use vike_model::feed_status::{ConnectionState, parse_feed_status};

const SERIES: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// A real Bybit subscribe ACK, the wire shape `route_frame` classifies as `KlineEvent::Ack` →
/// `FrameOutcome::Confirm`. Deliberately the ACK rather than a kline datum: it is the DATALESS
/// proof the venue accepted the subscription, so a success disclosure keyed on the first Confirm
/// must fire on it — a quiet-but-accepted stream is exactly the case a data-keyed disclosure
/// would miss.
const ACK_FRAME: &str = r#"{"op":"subscribe","success":true,"ret_msg":"","conn_id":"x"}"#;

/// Session 2's stream: deliver `ACK_FRAME` once, then raise the stop flag and report a read
/// timeout so the driver's next loop-top poll ends the session `Ok` and the feed loop breaks
/// WITHOUT disclosing an error.
///
/// ⚠ A plain [`ScriptedStream`] cannot express this and the difference is the whole test: an
/// exhausted script is a `Closed("script exhausted")` fault, so the session would end by writing
/// an error string over the very disclosure being asserted. The point is to observe the status a
/// HEALTHY, still-running session leaves behind.
///
/// It also captures the status text as it stood on its FIRST read — i.e. after session 1's fault
/// and before this session discloses anything — so both halves of the sequence are asserted from
/// one driver run rather than from two, which is what makes this a RECOVERY test rather than two
/// independent writes.
struct AckThenStop {
    status: Arc<Mutex<String>>,
    seen_on_entry: Rc<RefCell<Option<String>>>,
    stop: Arc<AtomicBool>,
    delivered: bool,
}

impl MarketStream for AckThenStop {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        if !self.delivered {
            *self.seen_on_entry.borrow_mut() = Some(self.status.lock().unwrap().clone());
            self.delivered = true;
            return Ok(ACK_FRAME.to_string());
        }
        self.stop.store(true, Ordering::Relaxed);
        Err(StreamError::Timeout)
    }

    fn send_text(&mut self, _s: &str) -> Result<(), StreamError> {
        Ok(())
    }
}

/// **The bar test.** A session that faults and a session that then succeeds, over the REAL bybit
/// kline closures — and the status the reconcile health gate reads must follow.
///
/// Session 1 is a scripted stream that exhausts, which the driver reports as
/// `Closed("script exhausted")`; `feed_body`'s session-error hook writes
/// `"BTCUSDT@1m ws error (reconnecting): script exhausted"`, which `parse_feed_status` reads as
/// `Error` (it tests `"error"` BEFORE `"reconnect"` — see that function's doc; the ordering is a
/// ratified verdict, not an accident, and `status_dot.rs`'s `#1569` note is why). Session 2 then
/// connects and the venue ACKs. After that ack the string must read `Connected`.
///
/// ⚠ The wall-clock cost is the venue's own reconnect backoff — `pump_spec`'s bybit row is
/// `Fixed(3 s)`, walked in 100 ms stop-poll ticks — and it is deliberately NOT overridden: the
/// row IS the production lifecycle, and a test that substitutes its own knobs stops being
/// evidence about the venue.
#[test]
fn a_recovered_session_clears_the_faulted_status() {
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new("connecting to Bybit…".to_string()));
    let ctx = FeedCtx {
        sink: Arc::new(NoopSink),
        status: Arc::clone(&status),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
    };
    // The pre-disclosure reading, captured by session 2 on its first read.
    let seen_on_entry: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Session 1: a script that exhausts == a transport close, the commonest real fault shape.
    let mut faulting = VecDeque::from([ScriptedStream::from_steps(vec![ScriptStep::Timeout])]);
    let mut session2 = Some(AckThenStop {
        status: Arc::clone(&status),
        seen_on_entry: Rc::clone(&seen_on_entry),
        stop: Arc::clone(&stop),
        delivered: false,
    });

    let sub = subscribe_frame("1", SERIES);
    let opts = pump_opts(&sub);
    feed_body(SERIES, INTERVAL, &opts, &ctx, || -> Result<Either, String> {
        if let Some(s) = faulting.pop_front() {
            return Ok(Either::Faulting(s));
        }
        session2.take().map(Either::Ack).ok_or_else(|| "a third connect".to_string())
    });

    // Half 1 — the fault reached the string. (If this ever reads otherwise the test has stopped
    // reproducing the incident and every later assertion is vacuous.)
    let after_fault = seen_on_entry.borrow().clone().expect("session 2 ran, so it captured");
    assert_eq!(
        parse_feed_status(&after_fault),
        ConnectionState::Error,
        "session 1's fault must reach the status the health gate reads: {after_fault:?}"
    );

    // Half 2 — THE DEFECT. The venue accepted session 2's subscription and is delivering on it;
    // the string must say so. Against the shipped code this reads `Error`, and stays `Error`
    // forever, which is the 42-hour suppression.
    let after_recovery = status.lock().unwrap().clone();
    assert_eq!(
        parse_feed_status(&after_recovery),
        ConnectionState::Connected,
        "a venue that ACKed the subscribe must not still read as faulted — this is the the CI box \
         latch: {after_recovery:?}"
    );
}

/// The two stream shapes this file drives, behind one type so the connect closure has a single
/// return type (the driver is generic over ONE `MarketStream`, by design — a session cannot
/// change transport mid-flight).
enum Either {
    Faulting(ScriptedStream),
    Ack(AckThenStop),
}

impl MarketStream for Either {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        match self {
            Either::Faulting(s) => s.read_frame(),
            Either::Ack(s) => s.read_frame(),
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        match self {
            Either::Faulting(x) => x.send_text(s),
            Either::Ack(x) => x.send_text(s),
        }
    }

    fn since_last_frame(&self) -> std::time::Duration {
        match self {
            Either::Faulting(s) => s.since_last_frame(),
            Either::Ack(s) => s.since_last_frame(),
        }
    }
}
