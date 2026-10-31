//! The reconcile client's ONE account-summary subscription (`$LEDGER:ALL`), read from a cache.
//!
//! **Why.** `IbkrSocketReconClient::fetch_balance` used to send a fresh `reqAccountSummary` on every reconcile pass,
//! read it to its end marker and cancel it by dropping the subscription. IB allows TWO active summary subscriptions
//! ("only two active summary subscriptions are allowed at a time"), and the CI box's journal carries
//! `[322] Maximum number of account summary requests exceeded` in bursts of 29-168 at the Gateway's nightly `[1102]`
//! connectivity-restored event (04:15-04:39 and 19:18-19:38 UTC), as many as the passes the connection had made. Our
//! side cancels every request (`ibapi`'s `Subscription::drop`; the source of 4.2.0 and 5.1.0 shows no leak), so the
//! surviving theory is that the Gateway keeps every request the connection ever made and re-submits them all at the
//! restore. This module makes ONE request, whatever the number of passes.
//!
//! **The allowance, MEASURED 2026-10-20** (the ladder probe, `crates/bridges/vike-ibkr/tests/ibkr_summary_cancel_probe.rs`,
//! on the demo Gateway): the Gateway allows [`SESSION_REQUESTS`] requests per SESSION (one TCP connection), and a
//! CANCELLED request keeps counting. (1) Two requests were each read to their end and cancelled; the third was refused
//! `[322]` at +5 s, +30 s, +2 min, +5 min, +10 min and +21 min after the second's cancel (so: for at least 21 minutes,
//! the longest rung; how much longer was not measured). (2) A NEW connection was accepted at once with one request,
//! under the same client id and under a fresh one: a new connection starts clean. (3) On a fresh connection two
//! requests held open were answered and the third was refused. So a connection survives ONE reopen and the second is
//! refused for good: **nothing here asks again because it is quiet or merely to be safe**, and no path may send a third
//! request on one session.
//!
//! **What.** [`SummaryFeed`] opens the subscription on the first pass and keeps it for the life of the connection.
//! After its `End` marker IB keeps pushing rows: IB's page says "every three minutes those values which have changed will
//! be returned" (so a quiet account may be sent NOTHING: measured, a dump of 50 rows and then no push for ten minutes;
//! what a CHANGE of balance pushes on a quiet account is NOT measured), `ibapi` says a push can also resend unchanged
//! rows (not observed here). Each pass drains what was queued since the last one (`try_next`, no wait) into an
//! [`AccountSummarySnapshot`] (the latest value of every `(account, tag, currency)`) and answers from it. Nothing is
//! waited for after the first pass, so a pass costs no round trip. The rows are the same ones the per-pass request read
//! (`parse_socket_balance` is unchanged), a fold of the initial dump and of the pushes.
//!
//! **What each event does.** Every request is counted (`Slot::requests`); a session with none left sends NOTHING and its
//! passes fail with [`SPENT_WORDS`], the same words every pass, until a new session begins.
//!
//! * **Silence: no request.** The last figure is served, however long ago IB pushed, and the journal WARNS that it may
//!   be frozen (`account summary figure is at most N s old and may be FROZEN`, after [`QUIET_NOTE_AFTER`], and again
//!   every [`QUIET_NOTE_EVERY`] while the silence lasts). A quiet account looks exactly like a Gateway that resumed after
//!   a connectivity loss and stopped pushing, and ⚠ **the node cannot tell them apart**: its balance clock
//!   (`last_balance_ms`) advances on every figure served, and the balance carries no age, so this line is the only
//!   sign. The remedy it names is `account.active` off and on (a new session). It used to be dropped and asked for again
//!   after twenty minutes, which on a quiet account spends the whole allowance in forty.
//! * **The first dump has not ended** within `READ_DEADLINE`: the request is written, counted and KEPT. The pass fails
//!   with [`PENDING_WORDS`], and the next passes read the same stream (no new request) until its `End` arrives, which
//!   serves everything it sent, or [`DUMP_PATIENCE`] passes, which cancels it. Rows before the `End` are never served.
//!   Two slow dumps in a row write one request, not two.
//! * **The stream failed** (`Some(Err(_))`: a TWS error for the request, `ConnectionReset` while `ibapi` re-dials) **or
//!   the notice stream said `[1101]`** (connectivity restored, DATA LOST: "market and account data subscription
//!   requests must be resubmitted", and the stream stays open and silent): the subscription is cancelled FIRST and the
//!   same pass makes the one request the session has left. A SECOND such event on one session finds none: the pass
//!   fails and nothing is sent.
//! * **`-8`** ([`ibapi::TRANSPORT_RECONNECT_CODE`]): `ibapi` re-dialled the socket. Read from `ibapi` 5.1.0, that is a
//!   NEW TCP connection, handshake and `StartApi` under the same client id, and the notice is published only once the
//!   session is live ("so a consumer that resubscribes on it lands on the new session"). Every request of the old
//!   session died with it and the new one counts from none: the cache is void, the allowance starts again and the
//!   memory of failed requests (the backoff) is cleared. (The `ibapi` re-dial itself has not been run against the
//!   Gateway; the fresh connection of measurement (2) is the same wire event.)
//! * **The balance is withheld** (the engine holds a foreign-quoted stock, `SummaryFeed::fold_only`): the subscription is
//!   KEPT and folds its pushes as on any pass; nothing is answered. It used to be cancelled and asked for again when
//!   the withhold lifted, a request spent for nothing.
//!
//! **Freshness is an upper bound.** `ibapi` hands a queued row over without the time it arrived, so a row folded by a
//! pass is stamped with the time of the PREVIOUS look (it arrived after it): after a gap in the passes a queued push
//! does not make the figure look younger than the gap (`Feed::freshen`).
//!
//! **A request that fails** (a refusal included) answers that pass with the error and leaves nothing cached, and the
//! next 1, 2, 4, then 8 (at most) passes are answered with the SAME words and no request, until one works: the words
//! stay identical so the node's edge-triggered "balance fetch failed" line is not said every minute. The allowance
//! comes first: a written request counts whether or not it worked, so the backoff never sends a request the session
//! does not have.
//!
//! ⚠ `ibapi` delivers `[1100]`/`[1101]`/`[1102]` ONLY to a `NoticeStream` bound BEFORE the connect
//! (`ClientBuilder::connect_with_notice_stream`, as the exec connection does), so the recon connection binds one.
//! `[1100]`/`[2110]` (link lost) are DRAINED AND NOT ACTED ON, deliberately: nothing was lost (the Gateway says `[1101]`
//! when it lost the data), the figure served meanwhile is IB's last push, and a link lost for good ends in the exec link
//! watchdog's rebuild of the whole adapter (`crates/bridges/vike-ibkr/src/transport/link_watch.rs`), which dials this
//! connection afresh. A Gateway that keeps answering on a dead link (that file records one, 2026-10-17) is the
//! watchdog's case, not this feed's.
//!
//! ⚠ Shortcuts, each with its limit: a row IB stops sending (a currency whose line vanishes) stays in the cache for the
//! life of the session; the queue between two passes is unbounded (one push is a few dozen small rows, drained every
//! pass, a withheld one included); and a session whose allowance is spent stays failed until `-8` or until its
//! CONNECTION is replaced: the reconcile client does that itself since A33 (`IbkrSocketReconClient`'s `rebuild_spent`,
//! asked by `SummaryFeed::spent`, paced and gated by the gateway's turn), and the adapter's own rebuild (the Gateway's daily
//! logoff, the link watchdog, `account.active` off and on) still begins a new one.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummary, AccountSummaryResult, AccountSummarySnapshot};
use ibapi::client::blocking::{Client, NoticeStream, Subscription};
use ibapi::subscriptions::SubscriptionItem;

use super::socket::READ_DEADLINE;

/// The `account_summary` tag that asks for the per-currency ledger (every currency, plus `BASE`).
pub(crate) const LEDGER_ALL: &str = "$LEDGER:ALL";

/// The account group the `$LEDGER:ALL` request names, and the account the Gateway stamps on EVERY row it answers with
/// (OBSERVED 2026-10-20, the demo login: all 50 rows of a dump said `All`, none the account's own number). IB's page
/// says it sums that ledger over every account of the login (`crate::recon::socket::ledger_account_for`).
pub(crate) const LEDGER_GROUP: &str = "All";

/// How many `reqAccountSummary` requests ONE session (one connection) may make in all, **a cancelled one included**.
///
/// MEASURED 2026-10-20 on the demo Gateway (the module doc): a third request was refused `[322]` up to 21 minutes after
/// the second was read to its end and cancelled, and a fresh connection was accepted at once. The count is the
/// session's: only a new connection (`-8`, or a rebuilt adapter) starts it again.
pub(crate) const SESSION_REQUESTS: u32 = 2;

/// How long a feed may hear nothing (no row, not even a repeated one) before the journal warns that the figure may be
/// frozen.
///
/// **It is a warning, never a bound.** IB pushes only the values that CHANGED, so a quiet account is sent nothing for
/// as long as it stays quiet (a weekend), and a figure is not stale for that. The feed is not dropped, reopened or
/// failed on silence: every reopen spends the session's allowance of [`SESSION_REQUESTS`], and the second is refused
/// for good. The warning is said again every [`QUIET_NOTE_EVERY`].
const QUIET_NOTE_AFTER: Duration = Duration::from_secs(10 * 60);

/// How often the frozen-figure warning is said again while the silence lasts: every hour.
const QUIET_NOTE_EVERY: Duration = Duration::from_secs(60 * 60);

/// How long a first dump that has not ended is still read on later passes before its request is cancelled: ten minutes.
const DUMP_PATIENCE: Duration = Duration::from_secs(10 * 60);

/// The words of a pass that finds the session's allowance spent: one text for every such pass, so the node's
/// edge-triggered "balance fetch failed" line is said once. It names the operator's step.
const SPENT_WORDS: &str = "ibkr account summary: this connection has made the two account-summary requests IB allows a \
                           session (a cancelled one counts too), so none is sent on it; the reconcile connection is \
                           replaced by a new session within about a minute (if that re-dial fails it is retried, and \
                           turning account.active off and on rebuilds the whole adapter: ONE batched toggle for every \
                           account that needs it)";

/// The words of a pass whose first dump has not ended: its request stays open and is read again, no new one is sent.
const PENDING_WORDS: &str = "ibkr account summary: the first reply has not ended yet; its request stays open and is \
                             read again on the next pass";

/// The most passes a failing reopen skips between two requests (it skips 1, 2, 4, then this many, until one works).
const MAX_SKIP: u32 = 8;

/// The notice that voids the cache: IB says the connection's data is lost and its requests must be resubmitted.
const DATA_LOST: i32 = 1101;

/// Whether a global notice voids the cache: `[1101]`, or `-8` (the socket was re-dialled, which fails every request).
fn is_void(code: i32) -> bool {
    code == DATA_LOST || code == ibapi::TRANSPORT_RECONNECT_CODE
}

/// What one read of the stream yields (`ibapi`'s `next_timeout` / `try_next`).
pub(crate) type Item = Option<Result<SubscriptionItem<AccountSummaryResult>, ibapi::Error>>;

/// One open `reqAccountSummary`. **Dropping it cancels the request** (`Subscription::drop`).
pub(crate) trait SummaryStream: Send {
    /// Wait up to `within` for the next item; `None` = nothing came in time, or the stream is over.
    fn wait(&self, within: Duration) -> Item;
    /// The next item if one is queued right now.
    fn poll(&self) -> Item;
}

impl SummaryStream for Subscription<AccountSummaryResult> {
    fn wait(&self, within: Duration) -> Item {
        self.next_timeout(within)
    }

    fn poll(&self) -> Item {
        self.try_next()
    }
}

/// The gateway side of the feed: the seam a test replaces.
pub(crate) trait SummaryGateway: Send + Sync {
    /// `reqAccountSummary(All, $LEDGER:ALL)`: writes the request and returns its stream. An `Err` means nothing was
    /// written (the session is down, the write failed): the feed does not count it.
    fn open(&self) -> Result<Box<dyn SummaryStream>, String>;
    /// The codes of the connection's global notices queued since the last call (non-blocking).
    fn notices(&self) -> Vec<i32>;
}

/// The real gateway: the recon connection's client and the notice stream bound before its connect.
pub(crate) struct ClientGateway {
    client: Arc<Client>,
    notices: NoticeStream,
}

impl ClientGateway {
    pub(crate) fn new(client: Arc<Client>, notices: NoticeStream) -> Self {
        Self { client, notices }
    }
}

impl SummaryGateway for ClientGateway {
    fn open(&self) -> Result<Box<dyn SummaryStream>, String> {
        let group = AccountGroup(LEDGER_GROUP.to_string());
        self.client
            .account_summary(&group, &[LEDGER_ALL])
            .map(|sub| Box::new(sub) as Box<dyn SummaryStream>)
            .map_err(|e| e.to_string())
    }

    fn notices(&self) -> Vec<i32> {
        std::iter::from_fn(|| self.notices.try_next()).map(|n| n.code).collect()
    }
}

/// One open subscription and what it has said so far.
struct Feed {
    stream: Box<dyn SummaryStream>,
    rows: AccountSummarySnapshot,
    /// When a row (or an end marker) last arrived.
    heard: Instant,
    /// When the queue was last looked at: every row folded by the next look arrived after it.
    last_look: Instant,
    /// When the silence of this feed was last warned about in the journal (`None` = not in a silence).
    noted: Option<Instant>,
    /// Whether the first dump has reached its `End` marker: until then the rows are a PARTIAL dump and are never served.
    ready: bool,
    /// When the request was written.
    opened: Instant,
}

impl Feed {
    /// Read the initial dump of the request that just opened `stream`, for up to `deadline`. A stream that fails is an
    /// `Err` and is dropped (cancelled) on the way out. A dump that has not reached its `End` in time is NOT an error
    /// and not a reason to ask again: the feed comes back with `ready` false, its stream kept and the rows so far held
    /// (see [`DUMP_PATIENCE`]).
    fn dump(stream: Box<dyn SummaryStream>, deadline: Duration) -> Result<Feed, String> {
        let now = Instant::now();
        let mut feed = Feed {
            stream,
            rows: AccountSummarySnapshot::default(),
            heard: now,
            last_look: now,
            noted: None,
            ready: false,
            opened: now,
        };
        while !feed.ready {
            let remaining = deadline.saturating_sub(now.elapsed());
            if remaining.is_zero() {
                break;
            }
            match feed.stream.wait(remaining) {
                Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row)))) => {
                    feed.rows.apply(row);
                }
                Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => feed.ready = true,
                Some(Ok(SubscriptionItem::Notice(n))) => {
                    tracing::debug!(
                        target: "vike_ibkr::recon", code = n.code, "account summary notice: {}", n.message
                    );
                }
                Some(Err(e)) => return Err(format!("ibkr account summary: {e}")),
                None => break,
            }
        }
        Ok(feed)
    }

    /// Fold everything queued since the last look. `Err` = the stream failed. Silence is NOT a failure: past
    /// `note_after` without a row the journal WARNS that the figure may be frozen, and says so again every `note_every`
    /// (see [`QUIET_NOTE_AFTER`]).
    ///
    /// **A row is stamped with the time of the PREVIOUS look, not with the time it is folded.** `ibapi` hands a queued row
    /// over without the moment it arrived, and after a gap in the passes (the node does not ask while symbols are owed,
    /// the lane withholds, the leg is not ready) the first pass folds a queue whose newest row may be as old as the gap:
    /// stamping it `now` would make a figure that is already most of a gap old look new. The row arrived after the
    /// previous look, so that look is the newest it can be stamped with: `heard` is an UPPER BOUND on the age of the
    /// newest row.
    fn freshen(&mut self, note_after: Duration, note_every: Duration) -> Result<(), String> {
        let before = self.last_look;
        let mut arrived = false;
        loop {
            match self.stream.poll() {
                Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row)))) => {
                    self.rows.apply(row);
                    arrived = true;
                }
                Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => {
                    self.ready = true;
                    arrived = true;
                }
                Some(Ok(SubscriptionItem::Notice(n))) => {
                    tracing::debug!(
                        target: "vike_ibkr::recon", code = n.code, "account summary notice: {}", n.message
                    );
                }
                Some(Err(e)) => return Err(format!("ibkr account summary: {e}")),
                None => break,
            }
        }
        self.last_look = Instant::now();
        if arrived {
            self.heard = before;
            self.noted = None;
        }
        // The age of what is served, WARNED at the start of a silence and again every `note_every` (not per pass). A
        // quiet account and a Gateway that stopped pushing look the same here, and the node's balance clock advances on
        // every figure served, so this line is the only sign an operator gets that "balance 100" may be frozen.
        let quiet = self.heard.elapsed();
        if self.ready
            && quiet > note_after
            && self.noted.is_none_or(|at| at.elapsed() >= note_every)
        {
            self.noted = Some(Instant::now());
            tracing::warn!(
                target: "vike_ibkr::recon",
                "account summary figure is at most {} s old and may be FROZEN: no row newer than that is known. A quiet \
                 account is pushed only what changed, so this is normal for one; but a Gateway that resumed after a \
                 connectivity loss and stopped pushing looks the same, and the node's balance clock cannot tell. To \
                 read the balance afresh turn account.active off and on (ONE batched toggle for every account that \
                 needs it). Said again every hour while the silence lasts",
                quiet.as_secs()
            );
        }
        Ok(())
    }

    fn rows(&self) -> Vec<AccountSummary> {
        self.rows.iter().cloned().collect()
    }
}

/// What the feed keeps between two reads: the subscription, the session's request count and the memory of failed requests.
#[derive(Default)]
struct Slot {
    feed: Option<Feed>,
    /// Requests written on this connection's current session, a cancelled one included (the Gateway counts it): at most
    /// [`SESSION_REQUESTS`]. A `-8` starts it again.
    requests: u32,
    /// Whether the journal has said that the session's allowance is spent (once per session).
    spent_said: bool,
    /// Requests that failed since the last one that worked.
    failures: u32,
    /// Reads still to be answered with `last_err` WITHOUT a request (the backoff).
    skip: u32,
    last_err: String,
}

/// The recon connection's account-summary rows, from ONE kept subscription (the module doc).
pub(crate) struct SummaryFeed<G: SummaryGateway> {
    // The subscription is declared before the gateway so it is dropped (and cancelled) while the client is still up.
    slot: Mutex<Slot>,
    gateway: G,
    /// How long the feed may hear nothing before the journal warns that the figure may be frozen.
    note_after: Duration,
    /// How often that warning is said again while the silence lasts.
    note_every: Duration,
    /// How long the initial dump may take within one pass.
    deadline: Duration,
}

impl<G: SummaryGateway> SummaryFeed<G> {
    pub(crate) fn new(gateway: G) -> Self {
        Self::with_bounds(gateway, QUIET_NOTE_AFTER, READ_DEADLINE)
    }

    fn with_bounds(gateway: G, note_after: Duration, deadline: Duration) -> Self {
        Self {
            slot: Mutex::new(Slot::default()),
            gateway,
            note_after,
            note_every: QUIET_NOTE_EVERY,
            deadline,
        }
    }

    /// The ledger rows as of now: the cache after folding what arrived since the last read, or, with no open
    /// subscription, the dump of the one new request the session has left (made once, now, and kept), or the
    /// [`SPENT_WORDS`] error with nothing sent.
    pub(crate) fn read(&self) -> Result<Vec<AccountSummary>, String> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if self.settle(&mut slot)
            && let Some(feed) = slot.feed.as_ref()
        {
            return Ok(feed.rows());
        }
        // A first dump that has not ended: the request is open and counted, so it is read again, never asked again.
        if slot.feed.is_some() {
            return Err(PENDING_WORDS.to_string());
        }
        // No open subscription (`settle` dropped the old one, which is its cancel: BEFORE any new request, so at most
        // one is live). The allowance comes before the backoff: a session with no request left sends none.
        if slot.requests >= SESSION_REQUESTS {
            if !slot.spent_said {
                slot.spent_said = true;
                tracing::info!(
                    target: "vike_ibkr::recon", requests = slot.requests,
                    "account summary request not sent: this connection has made the requests IB allows a session (a \
                     cancelled one counts); the balance fails until the reconcile connection is replaced by a new session \
                     (the client re-dials it, paced) or the adapter is rebuilt"
                );
            }
            return Err(SPENT_WORDS.to_string());
        }
        // The backoff: after a failed request the next passes are answered with the SAME words and no request, or the
        // node's edge-triggered "balance fetch failed" line would be said every minute.
        if slot.skip > 0 {
            slot.skip -= 1;
            return Err(slot.last_err.clone());
        }
        match self.request(&mut slot) {
            Ok(feed) if !feed.ready => {
                tracing::info!(
                    target: "vike_ibkr::recon", rows = feed.rows.len(), requests = slot.requests,
                    "account summary request written, its first reply has not ended; kept open and read on the next \
                     passes (no new request)"
                );
                slot.feed = Some(feed);
                Err(PENDING_WORDS.to_string())
            }
            Ok(feed) => {
                tracing::info!(
                    target: "vike_ibkr::recon", rows = feed.rows.len(), requests = slot.requests,
                    "account summary subscription opened (kept; the passes read its cache)"
                );
                let rows = feed.rows();
                slot.feed = Some(feed);
                slot.failures = 0;
                slot.last_err.clear();
                Ok(rows)
            }
            Err(e) => {
                slot.failures = slot.failures.saturating_add(1);
                slot.skip = (1_u32 << (slot.failures - 1).min(30)).min(MAX_SKIP);
                tracing::info!(
                    target: "vike_ibkr::recon",
                    "account summary request failed ({e}); the next {} passes ask nothing", slot.skip
                );
                slot.last_err = e.clone();
                Err(e)
            }
        }
    }

    /// Whether the session has NO request left and none open: every pass now fails with [`SPENT_WORDS`] and sends
    /// nothing, until a new session begins (`-8`, or a new connection: the caller's re-dial, A33). `false` while a
    /// subscription is open (a healthy one, or a first dump still being read) and while the allowance has a request left.
    pub(crate) fn spent(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        slot.feed.is_none() && slot.requests >= SESSION_REQUESTS
    }

    /// A pass whose figure is withheld (the engine holds a foreign-quoted stock): nothing is asked and nothing is
    /// answered, but the subscription STAYS and the pass folds what it pushed, so the queue stays short and the figure is
    /// current the pass the withhold lifts. Cancelling it would spend a request of the session's allowance for nothing.
    pub(crate) fn fold_only(&self) {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        self.settle(&mut slot);
    }

    /// Take in what happened since the last look: the connection's notices, then the rows queued on the open
    /// subscription. `true` = a subscription is open and its cache is what IB has said so far. `false` = there is none
    /// to serve: it was never opened, `[1101]`/`-8` voided it or its stream failed (and it has been dropped, cancelled,
    /// here), or its first dump has not ended yet (`slot.feed` is still set, and is read again next time).
    fn settle(&self, slot: &mut Slot) -> bool {
        let codes = self.gateway.notices();
        if codes.contains(&ibapi::TRANSPORT_RECONNECT_CODE) {
            // A NEW session at the Gateway (the module doc): its allowance starts again, and the old session's failed
            // requests are not held against it.
            slot.requests = 0;
            slot.spent_said = false;
            slot.failures = 0;
            slot.skip = 0;
            slot.last_err.clear();
        }
        // Notices first: a `[1101]` voids the cache, and what was queued with it goes with it.
        if let Some(code) = codes.into_iter().find(|c| is_void(*c))
            && slot.feed.take().is_some()
        {
            tracing::info!(
                target: "vike_ibkr::recon", code,
                "account summary feed dropped (notice {code}: its data is lost)"
            );
        }
        let Some(feed) = slot.feed.as_mut() else { return false };
        match feed.freshen(self.note_after, self.note_every) {
            Ok(()) if feed.ready => true,
            Ok(()) if feed.opened.elapsed() > DUMP_PATIENCE => {
                tracing::info!(
                    target: "vike_ibkr::recon",
                    "account summary request cancelled: its first reply never ended (the request stays counted)"
                );
                slot.feed = None;
                false
            }
            Ok(()) => false,
            Err(why) => {
                tracing::info!(target: "vike_ibkr::recon", "account summary feed dropped ({why})");
                slot.feed = None;
                false
            }
        }
    }

    /// Send ONE request and read its dump. The request is counted the moment `open` has written it: from there the
    /// Gateway counts it for the session whatever becomes of it (a dump that never ends, a `[322]`, a cancel).
    fn request(&self, slot: &mut Slot) -> Result<Feed, String> {
        let stream = self.gateway.open().map_err(|e| format!("ibkr account summary: {e}"))?;
        slot.requests += 1;
        Feed::dump(stream, self.deadline)
    }
}

#[cfg(test)]
#[path = "summary_tests.rs"]
mod summary_tests;
