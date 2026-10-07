//! The retry policy: permanent vs transient `getUpdates` failure, driven through the real poller thread.

use super::*;

// ---------------------------------------------------------------------------------------------
// The retry policy — permanent vs transient `getUpdates` failure
// ---------------------------------------------------------------------------------------------
//
// These two are the only tests in this file that drive the REAL poller THREAD
// (`vike_tradehub::telegram::spawn`) rather than calling `poll_once` directly, because what they
// assert is a property OF THE LOOP: how many requests a failing endpoint receives, and whether the
// loop is still there afterwards. The schedule itself (0.5s, 1s, 2s … 60s) and the announcement
// throttle are pinned exactly, without sleeping, in
// `crates/vike-tradehub/src/telegram/failure.rs`'s unit tests.

/// The scripted endpoint for the two loop tests: a `getUpdates` that can be made to fail, a poll
/// COUNTER (the request volume the retry policy exists to bound), and the replies it produced.
///
/// Deliberately NOT [`StubDeps`]. `spawn` takes ownership of its deps, so observing one afterwards
/// means sharing it through an `Arc` — and `Arc<T>` is `Send` only when `T: Sync`, which `StubDeps`
/// is not (its `preview_fn`/`accept_fn` are `Box<dyn Fn … + Send>`). These two tests need none of
/// that machinery: nothing here previews, confirms or reaches a core.
#[derive(Default)]
struct PollStub {
    /// While `Some`, EVERY `get_updates` fails with this instead of serving a batch.
    fail: Mutex<Option<PollError>>,
    batches: Mutex<VecDeque<Vec<TgUpdate>>>,
    /// One per `get_updates` call — the REQUEST count.
    polls: AtomicUsize,
    replies: Mutex<Vec<String>>,
}

impl PollStub {
    fn failing_with(e: PollError) -> Arc<Self> {
        let s = Arc::new(PollStub::default());
        *s.fail.lock().unwrap() = Some(e);
        s
    }
    /// End the scripted outage: subsequent polls serve batches again.
    fn heal(&self) {
        *self.fail.lock().unwrap() = None;
    }
    fn push_batch(&self, updates: Vec<TgUpdate>) {
        self.batches.lock().unwrap().push_back(updates);
    }
    fn polls(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }
    fn replies(&self) -> usize {
        self.replies.lock().unwrap().len()
    }
}

/// The `Box<dyn TelegramDeps + Send>` view onto a shared [`PollStub`]. (A blanket
/// `impl TelegramDeps for Arc<PollStub>` is not available — `Arc` is not `#[fundamental]`, so the
/// orphan rule rejects it — hence the newtype.)
struct SharedDeps(Arc<PollStub>);

impl TelegramDeps for SharedDeps {
    fn get_updates(&self, _offset: i64) -> Result<Vec<TgUpdate>, PollError> {
        self.0.polls.fetch_add(1, Ordering::Relaxed);
        if let Some(e) = self.0.fail.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.0.batches.lock().unwrap().pop_front().unwrap_or_default())
    }
    fn send_message(&self, _chat_id: i64, text: &str) {
        self.0.replies.lock().unwrap().push(text.to_string());
    }
    fn preview(&self, _cmd: &WireCommand) -> Option<String> {
        None
    }
    fn accept(&self, _cmd: WireCommand, _reason: &str) -> Result<String, String> {
        unreachable!("these tests never confirm anything")
    }
    fn read(&self, verb: ReadVerb) -> String {
        format!("read:{verb:?}")
    }
    fn mint_coid(&self) -> String {
        "tg-coid".to_string()
    }
    fn mint_token(&self) -> String {
        "tok".to_string()
    }
    fn now_ms(&self) -> i64 {
        0
    }
}

/// Poll `cond` until it holds or `deadline` elapses. Generous deadlines everywhere below: these
/// tests assert BOUNDS (a count that must not grow, an answer that must eventually arrive), never
/// that something happened by a particular millisecond, so a loaded CI box makes them slower rather
/// than red.
fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let until = std::time::Instant::now() + deadline;
    while std::time::Instant::now() < until {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// ⚠ **THE DEFECT.** `getUpdates` with a wrong bot token answers `404` INSTANTLY — there is no 20 s
/// long-poll to wait through — so a loop that answers every failure with `POLL_GAP` and another
/// attempt runs at ~2 requests/second. Measured on a clean install with a mistyped
/// `VIKE_TELEGRAM_BOT_TOKEN`: 19 WARN lines in 10 s, i.e. ~164,000 requests to `api.telegram.org`
/// and ~164,000 WARN lines (~37 MB) per day — at `warn`, the level the deploy runbook tells
/// operators to run the file layer at, so the recommended configuration could not turn it down.
///
/// A 404 is a rejected CREDENTIAL: it cannot start working by being asked again. So the channel
/// STOPS, which is `maybe_spawn`'s own "a control channel whose precondition cannot be met does not
/// exist" refusal, fired at the first pass that carries the evidence.
#[test]
fn a_permanent_getupdates_failure_stops_the_poller_instead_of_retrying_forever() {
    let stub = PollStub::failing_with(PollError::from_status(404));
    let (_dir, led) = ledger();
    let handle = spawn(config(), led, Box::new(SharedDeps(stub.clone())));

    assert!(
        wait_until(Duration::from_secs(10), || stub.polls() >= 1),
        "the poller must make its first request"
    );
    // Three times the old 500 ms cadence, so the old loop would be at 3-4 requests by now.
    std::thread::sleep(Duration::from_millis(1_500));
    assert_eq!(
        stub.polls(),
        1,
        "a rejected bot token must cost exactly ONE request and ONE log line, ever — the old loop \
         kept asking ~2x/second forever"
    );

    handle.shutdown(); // the thread already ended; shutdown/Drop still joins cleanly
}

/// The other half, which the fix must NOT buy the first half with: a 5xx, a rate limit or a dead
/// socket is the endpoint's problem, not the credential's, so the channel keeps retrying and is
/// still there when the endpoint comes back.
#[test]
fn a_transient_getupdates_failure_keeps_the_channel_alive_and_recovers() {
    let stub = PollStub::failing_with(PollError::from_status(503));
    let (_dir, led) = ledger();
    let handle = spawn(config(), led, Box::new(SharedDeps(stub.clone())));

    assert!(
        wait_until(Duration::from_secs(10), || stub.polls() >= 2),
        "a transient failure must be RETRIED — stopping the channel on a 503 would make one bad \
         minute at Telegram cost an operator their remote control until they noticed and restarted"
    );

    // …and the channel answers again once the endpoint returns. (Queue the batch BEFORE healing so
    // the first reachable poll already has something to serve.)
    stub.push_batch(vec![update(1, CHAT, "/status")]);
    stub.heal();
    assert!(
        wait_until(Duration::from_secs(30), || stub.replies() > 0),
        "the poller must still be running and must answer once getUpdates succeeds again"
    );

    handle.shutdown();
}
