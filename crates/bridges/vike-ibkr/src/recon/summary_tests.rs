//! `SummaryFeed` over a scripted gateway: no Gateway, no socket. The fake counts the requests (`open`) and the
//! cancels (a stream dropped), in order, which is what the module doc promises: ONE request however many passes, a
//! stream is always cancelled BEFORE its replacement is requested, and no SESSION ever sees more than the
//! `SESSION_REQUESTS` the Gateway allows (the fake keeps the count per session: a `-8` notice begins a new one, as
//! `ibapi`'s re-dial does).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::Level;
use vike_log::capture::captured;

use super::*;
use crate::recon::socket::{LedgerBalance, balance_unless_foreign_stock, parse_socket_balance};

type Queue = Arc<Mutex<VecDeque<Item>>>;
type Log = Arc<Mutex<Vec<&'static str>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A scripted subscription: its items are served in order and an exhausted script is silence. Dropping it IS the
/// cancel (`Subscription::drop`), and the log records it.
struct Scripted {
    queue: Queue,
    log: Log,
}

impl SummaryStream for Scripted {
    fn wait(&self, _within: Duration) -> Item {
        lock(&self.queue).pop_front().flatten()
    }

    fn poll(&self) -> Item {
        lock(&self.queue).pop_front().flatten()
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        lock(&self.log).push("cancel");
    }
}

/// What one `open()` answers: a stream holding these items, or this error.
type Opening = Result<Vec<Item>, String>;

struct Fake {
    log: Log,
    opens: AtomicUsize,
    /// `open()` calls per session, the current one last: the Gateway refuses a session's third.
    sessions: Mutex<Vec<usize>>,
    script: Mutex<VecDeque<Opening>>,
    queues: Mutex<Vec<Queue>>,
    notices: Mutex<Vec<i32>>,
}

impl Fake {
    fn new(script: Vec<Opening>) -> Arc<Fake> {
        Arc::new(Fake {
            log: Log::default(),
            opens: AtomicUsize::new(0),
            sessions: Mutex::new(vec![0]),
            script: Mutex::new(script.into()),
            queues: Mutex::new(Vec::new()),
            notices: Mutex::new(Vec::new()),
        })
    }

    fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }

    /// How many requests each session has been sent, oldest first.
    fn per_session(&self) -> Vec<usize> {
        lock(&self.sessions).clone()
    }

    fn log(&self) -> Vec<&'static str> {
        lock(&self.log).clone()
    }

    /// Queue `item` on the stream opened last, as the Gateway pushing it.
    fn push(&self, item: Item) {
        let queues = lock(&self.queues);
        lock(queues.last().expect("a stream is open")).push_back(item);
    }

    /// Queue a global notice on the connection's notice stream. `-8` is `ibapi`'s re-dial: a NEW session begins.
    fn notice(&self, code: i32) {
        if code == ibapi::TRANSPORT_RECONNECT_CODE {
            lock(&self.sessions).push(0);
        }
        lock(&self.notices).push(code);
    }
}

impl SummaryGateway for Arc<Fake> {
    fn open(&self) -> Result<Box<dyn SummaryStream>, String> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        *lock(&self.sessions).last_mut().expect("a session") += 1;
        lock(&self.log).push("open");
        let opening =
            lock(&self.script).pop_front().expect("the script ran out: an unexpected request");
        let items = opening?;
        let queue: Queue = Arc::new(Mutex::new(items.into()));
        lock(&self.queues).push(Arc::clone(&queue));
        Ok(Box::new(Scripted { queue, log: Arc::clone(&self.log) }))
    }

    fn notices(&self) -> Vec<i32> {
        std::mem::take(&mut *lock(&self.notices))
    }
}

const ACCOUNT: &str = "DU1234567";

fn stamped(account: &str, tag: &str, currency: &str, value: &str) -> Item {
    Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(AccountSummary {
        account: account.into(),
        tag: tag.into(),
        value: value.into(),
        currency: currency.into(),
    }))))
}

fn row(tag: &str, currency: &str, value: &str) -> Item {
    stamped(ACCOUNT, tag, currency, value)
}

fn cash(currency: &str, value: f64) -> Item {
    row("CashBalance", currency, &value.to_string())
}

fn end() -> Item {
    Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End)))
}

/// The initial dump of a request: a USD cash line and the end marker.
fn dump(cash_usd: f64) -> Opening {
    Ok(vec![cash("USD", cash_usd), end()])
}

/// `n` dumps of 100, 101, 102...: more than a session may use, so a feed that ignored the allowance would be handed a
/// stream every time it asked, and the figure says WHICH request answered.
fn dumps(n: i32) -> Vec<Opening> {
    (0..n).map(|i| dump(100.0 + f64::from(i))).collect()
}

fn feed(fake: &Arc<Fake>) -> SummaryFeed<Arc<Fake>> {
    SummaryFeed::with_bounds(Arc::clone(fake), Duration::from_secs(60), Duration::from_secs(1))
}

/// The kept feed's rows read under ONE stamp, as the per-pass request's rows used to be: `parse_socket_balance` over
/// `SummaryFeed::read`, with none of `LedgerBalance`'s stamps or gates.
fn cached_cash(
    feed: &SummaryFeed<Arc<Fake>>,
    account: &str,
    currency: &str,
) -> Result<Option<f64>, String> {
    parse_socket_balance(&feed.read()?, account, currency)
}

/// What a reconcile pass gets for a USD mount.
fn usd(feed: &SummaryFeed<Arc<Fake>>) -> Result<Option<f64>, String> {
    cached_cash(feed, ACCOUNT, "USD")
}

/// Pretend the feed last heard something `by` ago.
fn age(feed: &SummaryFeed<Arc<Fake>>, by: Duration) {
    let mut slot = lock(&feed.slot);
    let open = slot.feed.as_mut().expect("a feed is open");
    open.heard = Instant::now().checked_sub(by).expect("an Instant that far back");
}

/// Pretend no pass looked at the feed for `by` (the node does not ask while symbols are owed, the lane withholds, the
/// leg is not ready): nothing was heard in that time either, as far as the feed knows.
fn gap(feed: &SummaryFeed<Arc<Fake>>, by: Duration) {
    let mut slot = lock(&feed.slot);
    let open = slot.feed.as_mut().expect("a feed is open");
    let then = Instant::now().checked_sub(by).expect("an Instant that far back");
    open.heard = then;
    open.last_look = then;
}

/// How old the newest row of the open feed is claimed to be.
fn heard_ago(feed: &SummaryFeed<Arc<Fake>>) -> Duration {
    let slot = lock(&feed.slot);
    slot.feed.as_ref().expect("a feed is open").heard.elapsed()
}

/// Whether the open feed has warned, in the journal, that its figure may be frozen (in the silence it is in).
fn noted(feed: &SummaryFeed<Arc<Fake>>) -> bool {
    lock(&feed.slot).feed.as_ref().expect("a feed is open").noted.is_some()
}

/// Pretend the open feed's request was written `by` ago (its first dump has been pending that long).
fn stale_dump(feed: &SummaryFeed<Arc<Fake>>, by: Duration) {
    let mut slot = lock(&feed.slot);
    let open = slot.feed.as_mut().expect("a feed is open");
    open.opened = Instant::now().checked_sub(by).expect("an Instant that far back");
}

/// **The point of the redesign**: N reconcile passes are ONE `reqAccountSummary`, not N.
#[test]
fn one_subscription_serves_every_pass() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    for pass in 0..6 {
        assert_eq!(usd(&f), Ok(Some(100.0)), "pass {pass}");
    }
    assert_eq!(fake.opens(), 1, "six passes, ONE request");
    assert_eq!(fake.log(), ["open"], "and nothing is cancelled between passes");
}

/// What IB pushes after the end marker is folded into the cache: the latest value of every line, a line the push did
/// not resend is kept.
#[test]
fn rows_pushed_after_the_end_marker_reach_the_next_pass() {
    let fake = Fake::new(vec![Ok(vec![cash("USD", 100.0), cash("EUR", 7.0), end()])]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    fake.push(cash("USD", 150.0));
    assert_eq!(usd(&f), Ok(Some(150.0)), "a push moves the figure");
    assert_eq!(
        cached_cash(&f, ACCOUNT, "EUR"),
        Ok(Some(7.0)),
        "a line the push did not resend is kept"
    );
    assert_eq!(fake.opens(), 1);
}

/// A stream that errors (a TWS error for the request, `ConnectionReset` after an `ibapi` reconnect) is cancelled, and
/// the SAME pass is answered by one new request: the failure costs no pass, and the old stream is cancelled before the
/// new request is made.
#[test]
fn an_error_on_the_feed_reopens_it_after_cancelling_the_old_one() {
    let fake = Fake::new(vec![dump(100.0), dump(200.0)]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert_eq!(fake.opens(), 1);
    fake.push(Some(Err(ibapi::Error::ConnectionReset)));
    assert_eq!(
        usd(&f),
        Ok(Some(200.0)),
        "the pass that met the failure is answered by the new request"
    );
    assert_eq!(fake.log(), ["open", "cancel", "open"], "cancelled BEFORE the new request");
    assert_eq!(usd(&f), Ok(Some(200.0)));
    assert_eq!(fake.opens(), 2, "and the new request is kept");
}

/// `[1101]` (data lost: the stream stays open and goes silent) and `-8` (the socket was re-dialled) void the cache; what
/// was queued with them is voided too, and the figure is the new request's.
#[test]
fn data_lost_voids_the_cache_and_forces_a_reopen() {
    for code in [DATA_LOST, ibapi::TRANSPORT_RECONNECT_CODE] {
        let fake = Fake::new(vec![dump(100.0), dump(300.0)]);
        let f = feed(&fake);
        assert_eq!(usd(&f), Ok(Some(100.0)), "{code}");
        assert_eq!(usd(&f), Ok(Some(100.0)), "{code}");
        assert_eq!(fake.opens(), 1, "{code}");
        fake.push(cash("USD", 111.0));
        fake.notice(code);
        assert_eq!(
            usd(&f),
            Ok(Some(300.0)),
            "{code}: the figure is the new request's, not the voided cache's"
        );
        assert_eq!(fake.log(), ["open", "cancel", "open"], "{code}");
        assert_eq!(usd(&f), Ok(Some(300.0)), "{code}");
        assert_eq!(fake.opens(), 2, "{code}: kept again");
    }
}

/// The other connectivity notices do not touch the cache: `[1100]`/`[2110]` (link lost: nothing to resubmit, and a link
/// gone for good is the exec link watchdog's rebuild of the whole adapter), `[1102]` (restored, data MAINTAINED) and
/// the farm status.
#[test]
fn other_notices_leave_the_cache_alone() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    for code in [1100, 2110, 1102, 2103, 2104, 2106, 2157, 2158] {
        fake.notice(code);
        assert_eq!(usd(&f), Ok(Some(100.0)), "{code}");
    }
    assert_eq!(fake.opens(), 1);
}

/// **Silence is not a reason to ask again** (it used to be: a feed that heard nothing for the bound was cancelled and
/// requested anew, which on a quiet account is every twenty minutes and spends a session's two requests in forty). A
/// quiet account is pushed only what CHANGED; the last figure is served however long ago IB pushed, and the journal
/// says how old it is at most.
#[test]
fn a_quiet_account_is_served_from_the_cache_and_never_asked_again() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert_eq!(usd(&f), Ok(Some(100.0)), "inside the bound it is served");
    for quiet in [90, 180, 600] {
        age(&f, Duration::from_secs(quiet));
        assert_eq!(usd(&f), Ok(Some(100.0)), "{quiet} s of silence, against a bound of 60 s");
    }
    assert_eq!(fake.log(), ["open"], "no cancel, no second request");
    assert!(noted(&f), "the age was said in the journal");
}

/// The age is said once per silence, and a row (even a repeat of an unchanged value) ends the silence: the next one is
/// said again.
#[test]
fn the_age_is_said_once_per_silence_and_a_row_re_arms_it() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert!(!noted(&f));
    age(&f, Duration::from_secs(90));
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert!(noted(&f), "past the bound it is said");
    fake.push(cash("USD", 100.0));
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert!(!noted(&f), "a row ends the silence");
    age(&f, Duration::from_secs(90));
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert!(noted(&f), "and the next silence is said again");
    assert_eq!(fake.opens(), 1);
}

/// **A frozen figure is WARNED about, and the warning repeats.** The node's balance clock advances on every figure
/// served and the balance carries no age, so the journal line is the only sign that a cache may have stopped being
/// fed: it is a `warn`, it says the figure may be frozen and names the remedy (`account.active` off and on), it is said
/// once per silence rather than per pass, and it is said AGAIN once `note_every` has passed while the silence lasts.
#[test]
fn a_silent_figure_is_warned_about_as_possibly_frozen_and_the_warning_repeats() {
    let fake = Fake::new(vec![dump(100.0)]);
    let mut f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    age(&f, Duration::from_secs(90));
    let warnings = |events: &[vike_log::capture::CapturedEvent]| -> Vec<String> {
        events.iter().filter(|e| e.level == Level::WARN).map(|e| e.message.clone()).collect()
    };
    let (_, events) = captured(|| {
        assert_eq!(usd(&f), Ok(Some(100.0)));
        assert_eq!(usd(&f), Ok(Some(100.0)));
    });
    let said = warnings(&events);
    assert_eq!(said.len(), 1, "once for the silence, not once per pass: {said:?}");
    assert!(said[0].contains("FROZEN") && said[0].contains("account.active"), "{}", said[0]);
    assert!(said[0].contains("at most 90 s old"), "the age is said: {}", said[0]);
    f.note_every = Duration::from_millis(1);
    std::thread::sleep(Duration::from_millis(5));
    let (_, events) = captured(|| assert_eq!(usd(&f), Ok(Some(100.0))));
    assert_eq!(
        warnings(&events).len(),
        1,
        "said again once the interval has passed, silence still lasting"
    );
}

/// **THE BITE: no event may send a third request on one session** (the Gateway refuses it for good, 2026-10-20). Silence
/// three times over and then `[1101]` spend the session's ONE reopen; a second `[1101]` finds none, and the pass fails
/// with the allowance words and sends nothing, however many passes follow. A feed that reopened on silence, or that did
/// not count its reopen, would call `open` a third time here and the session count would read 3.
#[test]
fn silence_then_data_lost_spends_the_one_reopen_and_no_more() {
    let fake = Fake::new(dumps(6));
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    for _ in 0..3 {
        age(&f, Duration::from_secs(200));
        assert_eq!(usd(&f), Ok(Some(100.0)), "silence is served");
    }
    assert_eq!(fake.per_session(), [1], "silence asked for nothing");
    fake.notice(DATA_LOST);
    assert_eq!(usd(&f), Ok(Some(101.0)), "the one reopen the session has");
    assert_eq!(fake.per_session(), [2]);
    age(&f, Duration::from_secs(200));
    assert_eq!(usd(&f), Ok(Some(101.0)), "and silence after it is served again");
    fake.notice(DATA_LOST);
    for pass in 0..5 {
        assert_eq!(
            usd(&f),
            Err(SPENT_WORDS.to_string()),
            "pass {pass}: no request left, the same words"
        );
    }
    assert_eq!(fake.per_session(), [2], "never a third request on the session");
    assert_eq!(
        fake.log(),
        ["open", "cancel", "open", "cancel"],
        "the voided feed is cancelled, nothing follows"
    );
}

/// A stream error is the same: it spends the one reopen, and a second one asks for nothing.
#[test]
fn a_second_stream_error_on_one_session_asks_nothing() {
    let fake = Fake::new(dumps(4));
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    fake.push(Some(Err(ibapi::Error::NotImplemented)));
    assert_eq!(usd(&f), Ok(Some(101.0)), "the first error is answered by the one reopen");
    fake.push(Some(Err(ibapi::Error::NotImplemented)));
    for _ in 0..3 {
        assert_eq!(usd(&f), Err(SPENT_WORDS.to_string()));
    }
    assert_eq!(fake.per_session(), [2]);
}

/// A request that fails still counts (the Gateway counts what was written, answered or not), and **the backoff never
/// sends a request the session does not have**: after the second request of a session failed, the passes that the 1, 2,
/// 4, 8 schedule would have asked on are refused locally.
#[test]
fn a_failed_request_counts_and_the_backoff_does_not_outrun_the_allowance() {
    let fake = Fake::new(vec![
        dump(100.0),
        Ok(vec![Some(Err(ibapi::Error::NotImplemented))]),
        dump(300.0),
        dump(400.0),
    ]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    fake.push(Some(Err(ibapi::Error::ConnectionReset)));
    let refused = usd(&f).unwrap_err();
    assert!(refused.starts_with("ibkr account summary:"), "{refused}");
    assert_eq!(fake.per_session(), [2], "the reopen was written, so it is counted");
    for pass in 0..20 {
        assert_eq!(usd(&f), Err(SPENT_WORDS.to_string()), "pass {pass}");
    }
    assert_eq!(fake.per_session(), [2], "twenty passes, the whole backoff schedule, no request");
}

/// **A re-dial (`-8`) is a new session with its own two requests**: `ibapi` reconnects with a new TCP connection and the
/// Gateway counts from none. The spent state ends, the cache of the old session is void, and the new session has its
/// first request and its one reopen.
#[test]
fn a_redial_begins_a_new_session_with_its_own_allowance() {
    let fake = Fake::new(dumps(8));
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    fake.notice(DATA_LOST);
    assert_eq!(usd(&f), Ok(Some(101.0)));
    fake.notice(DATA_LOST);
    assert_eq!(usd(&f), Err(SPENT_WORDS.to_string()), "the first session is spent");
    fake.notice(ibapi::TRANSPORT_RECONNECT_CODE);
    assert_eq!(usd(&f), Ok(Some(102.0)), "the new session's first request");
    fake.notice(DATA_LOST);
    assert_eq!(usd(&f), Ok(Some(103.0)), "and its one reopen");
    fake.notice(DATA_LOST);
    assert_eq!(usd(&f), Err(SPENT_WORDS.to_string()), "then it is spent too");
    assert_eq!(fake.per_session(), [2, 2], "two requests per session, never more");
}

/// A request that fails (the `[322]` case) answers that pass with its error and leaves nothing cached; the pass after
/// it is answered with the SAME words and no request (the backoff), and the one after that asks again.
#[test]
fn a_failed_request_answers_its_pass_and_caches_nothing() {
    let fake = Fake::new(vec![
        Err("[322] Maximum number of account summary requests exceeded".into()),
        dump(100.0),
    ]);
    let f = feed(&fake);
    let err = usd(&f).unwrap_err();
    assert!(err.starts_with("ibkr account summary:") && err.contains("322"), "{err}");
    assert_eq!(fake.opens(), 1, "one attempt");
    assert_eq!(
        usd(&f).unwrap_err(),
        err,
        "the backed-off pass says the same words and asks nothing"
    );
    assert_eq!(fake.opens(), 1);
    assert_eq!(usd(&f), Ok(Some(100.0)), "then it asks again");
    assert_eq!(usd(&f), Ok(Some(100.0)));
    assert_eq!(fake.opens(), 2);
}

/// **A first dump that has not ended is KEPT and read again, never asked again.** Its request is written (and counted)
/// once; the passes that follow read the same stream and answer with the same words until its `End` arrives, so a slow
/// or flapping Gateway that misses the deadline twice in a row costs ONE request, not the session's two. The rows that
/// came before the `End` are a partial dump and are never served meanwhile, and are all there when it ends.
#[test]
fn a_dump_that_has_not_ended_is_kept_and_never_asked_again() {
    let fake = Fake::new(vec![Ok(vec![cash("USD", 100.0)]), dump(200.0)]);
    let f = feed(&fake);
    assert_eq!(
        usd(&f),
        Err(PENDING_WORDS.to_string()),
        "the first pass: no End within the deadline"
    );
    assert_eq!(
        usd(&f),
        Err(PENDING_WORDS.to_string()),
        "the second pass: still no End, still no new request"
    );
    assert_eq!(fake.per_session(), [1], "two timed-out passes, ONE request written");
    assert_eq!(fake.log(), ["open"], "and nothing was cancelled");
    fake.push(end());
    assert_eq!(usd(&f), Ok(Some(100.0)), "the End arrives: the rows from before it are served");
    assert_eq!(fake.per_session(), [1]);
    assert_eq!(fake.log(), ["open"]);
}

/// ...but not for ever: a dump still pending past the patience is cancelled (it stays counted), and the next pass makes
/// the one request the session has left.
#[test]
fn a_dump_that_never_ends_is_cancelled_after_the_patience_and_asked_again_once() {
    let fake = Fake::new(vec![Ok(vec![cash("USD", 100.0)]), dump(200.0)]);
    let f = feed(&fake);
    assert!(usd(&f).is_err());
    stale_dump(&f, DUMP_PATIENCE + Duration::from_secs(60));
    assert_eq!(
        usd(&f),
        Ok(Some(200.0)),
        "the stale request is cancelled and replaced by the session's second"
    );
    assert_eq!(fake.log(), ["open", "cancel", "open"]);
    assert_eq!(fake.per_session(), [2]);
}

/// A re-dial (`-8`) is a new session, and the memory of the old session's failed requests (the backoff) goes with it:
/// the first pass after it asks at once instead of waiting out passes the old session owed.
#[test]
fn a_redial_clears_the_backoff_of_the_old_session() {
    let fake = Fake::new(vec![Err("down".into()), dump(100.0)]);
    let f = feed(&fake);
    assert!(usd(&f).is_err(), "the first request failed: one pass is owed to the backoff");
    fake.notice(ibapi::TRANSPORT_RECONNECT_CODE);
    assert_eq!(usd(&f), Ok(Some(100.0)), "the new session asks at once");
    assert_eq!(fake.opens(), 2);
}

/// **F4: a reopen that keeps failing is not retried every pass.** After the 1st, 2nd, 3rd, 4th... failure the next
/// 1, 2, 4, 8, 8... passes are answered with the same error and NO request, and the first request that works resets the
/// count. Each failure here is an `open` that wrote nothing (the session was down), so none is counted against the
/// session's allowance and four of them leave both requests; a refusal that WAS written counts, and ends the asking
/// (`a_failed_request_counts_and_the_backoff_does_not_outrun_the_allowance`).
#[test]
fn a_reopen_that_keeps_failing_backs_off_1_2_4_8_passes() {
    let refused =
        || -> Opening { Err("[322] Maximum number of account summary requests exceeded".into()) };
    let fake =
        Fake::new(vec![refused(), refused(), refused(), refused(), dump(100.0), dump(200.0)]);
    let f = feed(&fake);
    let first = usd(&f).unwrap_err();
    // Passes 2..=19: the requests made are on passes 1, 3, 6 and 11 (1, 2, 4 passes skipped, then 8 more).
    for pass in 2..=19 {
        let err = usd(&f).unwrap_err();
        assert_eq!(
            err, first,
            "pass {pass}: the same words every pass, so a journal line is not repeated"
        );
        let expected_opens = match pass {
            2 => 1,
            3..=5 => 2,
            6..=10 => 3,
            _ => 4,
        };
        assert_eq!(fake.opens(), expected_opens, "pass {pass}");
    }
    assert_eq!(usd(&f), Ok(Some(100.0)), "pass 20: the fifth request works");
    assert_eq!(fake.opens(), 5);
    fake.push(Some(Err(ibapi::Error::ConnectionReset)));
    assert_eq!(
        usd(&f),
        Ok(Some(200.0)),
        "a success reset the backoff: the next failure reopens at once"
    );
    assert_eq!(fake.opens(), 6);
}

/// **F3: a gap in the passes does not make the figure look young.** Rows are stamped with the time of the look BEFORE the
/// one that folded them (they arrived after it), not the time they were folded. A push that arrived somewhere in a
/// 90 s gap, with a 60 s bound, is served (nothing is asked again), but the figure is claimed to be at most 90 s old and
/// the journal says so.
#[test]
fn a_gap_in_the_passes_does_not_make_the_figure_look_young() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    gap(&f, Duration::from_secs(90));
    fake.push(cash("USD", 150.0));
    assert_eq!(usd(&f), Ok(Some(150.0)), "the queued push is folded and served");
    assert_eq!(fake.log(), ["open"], "and nothing is asked again");
    assert!(heard_ago(&f) >= Duration::from_secs(90), "the push may be as old as the gap");
    assert!(noted(&f), "so the age is said");
}

/// ...and a shorter gap is served, but the push is never claimed newer than the last look.
#[test]
fn a_push_after_a_short_gap_is_never_claimed_newer_than_the_last_look() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    assert_eq!(usd(&f), Ok(Some(100.0)));
    gap(&f, Duration::from_secs(5));
    fake.push(cash("USD", 150.0));
    assert_eq!(usd(&f), Ok(Some(150.0)));
    assert_eq!(fake.opens(), 1);
    assert!(
        heard_ago(&f) >= Duration::from_secs(5),
        "the push arrived after the look 5 s ago, not just now"
    );
}

/// `fetch_balance`'s gate, with the feed behind it: while the engine holds a stock in another currency the figure is
/// withheld and NO request is made; the subscription already open is KEPT (cancelling it spends a request of the
/// session's two, and a request is all that resuming would cost) and keeps folding what IB pushes; when the withhold
/// lifts the figure is the cache's, current, with no request.
#[test]
fn a_foreign_stock_keeps_the_feed_folding_and_the_resume_asks_nothing() {
    let fake = Fake::new(vec![dump(100.0)]);
    let f = feed(&fake);
    let names = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let pass = |admitted: &[String]| {
        balance_unless_foreign_stock(admitted, "USD", || usd(&f), || f.fold_only())
    };
    assert_eq!(pass(&[]), Ok(Some(100.0)));
    assert_eq!(fake.log(), ["open"], "the served pass keeps its subscription open");
    let foreign = names(&["AAPL.SMART.USD", "SAP.SMART.EUR"]);
    for pushed in [110.0, 120.0, 130.0] {
        fake.push(cash("USD", pushed));
        assert_eq!(pass(&foreign), Ok(None), "the figure is withheld");
    }
    assert_eq!(
        fake.log(),
        ["open"],
        "nothing is cancelled and nothing is asked while it is withheld"
    );
    assert!(
        lock(&fake.queues).last().is_some_and(|q| lock(q).is_empty()),
        "the pushes were folded, not queued"
    );
    assert_eq!(
        pass(&names(&["AAPL.SMART.USD"])),
        Ok(Some(130.0)),
        "the withhold lifts: the cache has what IB pushed meanwhile"
    );
    assert_eq!(fake.per_session(), [1], "one request for the whole of it");
}

/// A `[1101]` that arrives while the figure is withheld is not lost: the withheld pass drops the void feed, and the first
/// pass that is served spends the session's one reopen.
#[test]
fn data_lost_during_a_withhold_is_met_by_the_first_served_pass() {
    let fake = Fake::new(vec![dump(100.0), dump(200.0)]);
    let f = feed(&fake);
    let names = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let pass = |admitted: &[String]| {
        balance_unless_foreign_stock(admitted, "USD", || usd(&f), || f.fold_only())
    };
    assert_eq!(pass(&[]), Ok(Some(100.0)));
    fake.notice(DATA_LOST);
    assert_eq!(pass(&names(&["SAP.SMART.EUR"])), Ok(None), "withheld");
    assert_eq!(
        fake.log(),
        ["open", "cancel"],
        "the void feed is cancelled, and nothing is asked for a figure nobody reads"
    );
    assert_eq!(pass(&[]), Ok(Some(200.0)), "the first served pass asks, once");
    assert_eq!(fake.per_session(), [2]);
}

/// The cached ledger is read exactly as the per-pass request's rows were: the mount's own `CashBalance` line, else the
/// `BASE` block when it declares the mount's currency as the base, else no figure; another account's lines are ignored.
#[test]
fn the_cached_ledger_is_read_like_the_per_pass_request_was() {
    let fake = Fake::new(vec![Ok(vec![
        row("Currency", "BASE", "EUR"),
        row("CashBalance", "BASE", "5000"),
        cash("USD", 4000.0),
        end(),
    ])]);
    let f = feed(&fake);
    assert_eq!(cached_cash(&f, ACCOUNT, "USD"), Ok(Some(4000.0)));
    assert_eq!(cached_cash(&f, ACCOUNT, "EUR"), Ok(Some(5000.0)), "the BASE block declares EUR");
    assert_eq!(cached_cash(&f, ACCOUNT, "GBP"), Ok(None));
    assert_eq!(
        cached_cash(&f, "DU9999999", "USD"),
        Ok(None),
        "another account's lines are not this one's"
    );
    assert_eq!(fake.opens(), 1);
}

/// The balance of a USD-mounted connection over a scripted gateway: `LedgerBalance` is what `fetch_balance` runs, and
/// `open` is what `connect_within` calls with the login's `managed_accounts`.
fn ledger_balance(fake: &Arc<Fake>, managed: &[&str]) -> LedgerBalance<Arc<Fake>> {
    let managed: Vec<String> = managed.iter().map(|m| (*m).to_string()).collect();
    LedgerBalance::open(feed(fake), &managed, ACCOUNT, "USD".to_string())
        .expect("the login manages the configured account")
}

/// The warnings an action put in the journal, as their sentences.
fn warnings(events: &[vike_log::capture::CapturedEvent]) -> Vec<String> {
    events.iter().filter(|e| e.level == Level::WARN).map(|e| e.message.clone()).collect()
}

/// **The venue's cash reaches `fetch_balance`** (the defect of 2026-10-20: every pass answered "not reported" because
/// the Gateway stamps each `$LEDGER:ALL` row `All`, not the account's number). `LedgerBalance` is the code
/// `fetch_balance` runs, built as `connect_within` builds it. A login that manages only this account gets the figure of
/// the Gateway's rows (decoded from the ladder probe), and a push moves it; one that manages two gets none from the
/// same rows, and the feed is still kept and read (one request).
#[test]
fn the_all_stamped_ledger_reaches_the_balance_pass_only_on_a_one_account_login() {
    let gateway_rows = || {
        Ok(vec![
            stamped("All", "Currency", "USD", "USD"),
            stamped("All", "CashBalance", "USD", "1006910.91"),
            stamped("All", "Currency", "BASE", "BASE"),
            stamped("All", "CashBalance", "BASE", "1006910.91"),
            end(),
        ])
    };

    let fake = Fake::new(vec![gateway_rows()]);
    let one = ledger_balance(&fake, &[ACCOUNT]);
    assert_eq!(one.fetch(&[]), Ok(Some(1_006_910.91)), "the first pass opens the feed");
    fake.push(stamped("All", "CashBalance", "USD", "1007000.5"));
    assert_eq!(one.fetch(&[]), Ok(Some(1_007_000.5)), "a push moves the figure");
    assert_eq!(fake.opens(), 1);

    let fake = Fake::new(vec![gateway_rows()]);
    let two = ledger_balance(&fake, &[ACCOUNT, "DU7654321"]);
    for pass in 0..3 {
        assert_eq!(two.fetch(&[]), Ok(None), "pass {pass}: the summed ledger is not adopted");
    }
    assert_eq!(fake.opens(), 1, "the feed is read all the same, never asked again");
}

/// `LedgerBalance::open` is the connect-time refusal too: a login that does not manage the configured account builds
/// nothing (so `connect_within` returns before the contract is asked for), and the foreign-stock gate still withholds
/// the figure and reads nothing.
#[test]
fn a_wrong_account_is_refused_and_a_foreign_stock_withholds_the_figure() {
    let fake = Fake::new(vec![]);
    let managed = ["DU7654321".to_string()];
    let refused = LedgerBalance::open(feed(&fake), &managed, ACCOUNT, "USD".to_string());
    assert!(refused.is_err(), "the login does not manage the configured account");

    let fake = Fake::new(vec![dump(100.0)]);
    let one = ledger_balance(&fake, &[ACCOUNT]);
    let foreign = ["SAP.SMART.EUR".to_string()];
    assert_eq!(one.fetch(&foreign), Ok(None), "a foreign stock withholds the figure");
    assert_eq!(fake.opens(), 0, "and nothing is asked for it");
    assert_eq!(one.fetch(&[]), Ok(Some(100.0)));
}

/// **Both stamps are read on a one-account login** (NOT VERIFIED that any login stamps its rows with the account's own
/// number; the original failure was silent, so the other stamp is tried too): `All` first, the account's own number when
/// `All` has no such row. A login that manages two accounts reads only its own number.
#[test]
fn a_one_account_login_reads_all_first_and_its_own_number_second() {
    let own = |value: &str| stamped(ACCOUNT, "CashBalance", "USD", value);
    let all = |value: &str| stamped("All", "CashBalance", "USD", value);

    let fake = Fake::new(vec![Ok(vec![own("12.5"), end()])]);
    assert_eq!(ledger_balance(&fake, &[ACCOUNT]).fetch(&[]), Ok(Some(12.5)), "own number only");

    let fake = Fake::new(vec![Ok(vec![own("12.5"), all("99.5"), end()])]);
    assert_eq!(ledger_balance(&fake, &[ACCOUNT]).fetch(&[]), Ok(Some(99.5)), "`All` wins");

    let fake = Fake::new(vec![Ok(vec![own("12.5"), all("99.5"), end()])]);
    let two = ledger_balance(&fake, &[ACCOUNT, "DU7654321"]);
    assert_eq!(two.fetch(&[]), Ok(Some(12.5)), "two accounts: only its own number is read");
}

/// **Rows that carry neither stamp are said ONCE per connection** (the shape of the original silent failure: the feed
/// has rows, every pass answers "not reported", and nothing was logged). A feed whose rows carry the account's own
/// number, or `All`, says nothing; a login that manages two accounts said so at connect and does not say it again.
#[test]
fn rows_that_carry_neither_stamp_are_said_once_per_connection() {
    let fake = Fake::new(vec![Ok(vec![stamped("DU7654321", "CashBalance", "USD", "7"), end()])]);
    let one = ledger_balance(&fake, &[ACCOUNT]);
    let (_, events) = captured(|| {
        for pass in 0..3 {
            assert_eq!(one.fetch(&[]), Ok(None), "pass {pass}");
        }
    });
    let said = warnings(&events);
    assert_eq!(said.len(), 1, "once, not once per pass: {said:?}");
    assert!(said[0].contains("1 rows") && said[0].contains("`All`"), "{}", said[0]);
    assert!(said[0].contains("NOT adopted"), "{}", said[0]);
    assert!(!said[0].contains("DU7654321") && !said[0].contains(ACCOUNT), "{}", said[0]);

    for rows in
        [vec![cash("USD", 5.0), end()], vec![stamped("All", "CashBalance", "USD", "5"), end()]]
    {
        let fake = Fake::new(vec![Ok(rows)]);
        let one = ledger_balance(&fake, &[ACCOUNT]);
        let (got, events) = captured(|| one.fetch(&[]));
        assert_eq!(got, Ok(Some(5.0)));
        assert!(warnings(&events).is_empty(), "{events:?}");
    }

    let fake = Fake::new(vec![Ok(vec![stamped("All", "CashBalance", "USD", "5"), end()])]);
    let (two, events) = captured(|| ledger_balance(&fake, &[ACCOUNT, "DU7654321"]));
    assert_eq!(warnings(&events).len(), 1, "the connect-time line about the several accounts");
    let (got, events) = captured(|| two.fetch(&[]));
    assert_eq!(got, Ok(None));
    assert!(warnings(&events).is_empty(), "already said at connect: {events:?}");
}
