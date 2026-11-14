//! **The one writer of `link_session`** (`vike_secrets::link_sessions`, whose module doc says why the table sits in the
//! settings database and how it stays safe there): one thread, draining the two bounded queues
//! `vike_bridge_core::link_history`'s reports land on, so no venue thread ever waits for a disk and a burst across a session's
//! symbols costs no account's row.
//!
//! A batch is one short transaction, the accounts' reports first, and the next waits [`COMMIT_GAP`]: every commit moves the
//! store's change counter, which wakes the tradehub's account follower and can cost the nightly backup's copy a retry, so a
//! storm of closes must not become a storm of commits. A failed write costs its rows, counted and said once, never a
//! connection. There is no heartbeat, for the same reason: a row a dead hub left open says only what is known of it.
//!
//! A start closes what a process that never said goodbye left open, and tries until that lands, before any new session
//! can meet those rows. A clean stop (this module's [`LinkWriter`] going out of scope in the wire hook) waits for a commit in
//! flight, lets no other start, and says all it has in one transaction. A process that exits inside a commit leaves a journal:
//! the next read that may write replays it (`crates/vike-secrets/src/db/open.rs`'s `replay_hot_journal`), but a unit with no
//! `settings/db` grant is refused until then, so a clean stop leaves none.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::link_history::{
    self, LINK_PRICE_QUEUE, LINK_TRADE_QUEUE, VenueLinkSinks, dropped, install,
};
use vike_market_stream::sink::DataClientSinkStream;
use vike_model::StreamStatus;
use vike_secrets::StoreError;
use vike_secrets::link_sessions::{
    LinkEvent, LinkKey, PLANE_PRICE, close_left_open, flush_link_events, prune_link_sessions,
    write_link_events,
};

/// The most transitions in one transaction: 102 a second at [`COMMIT_GAP`], so one outage across a full session (2048 words)
/// drains in 20 s. MEASURED on the CI box's NVMe, a table at its cap, a busy box, the whole write (open, lock, commit), 30 runs:
/// 512 holds the store for a median of 62 ms and at most 90 ms; 1024 reached 258 ms, past [`vike_secrets::link_sessions::LINK_WRITE_BUSY`].
const BATCH_MAX: usize = 512;
/// The least time between two commits: at most this often does a storm wake the tradehub's follower. A wake reads the rows,
/// finds them as they were and asks this daemon its one question about a changed key
/// (`crates/vike-tradehub/src/account_follow.rs`'s `tick`). SQLite keeps one change counter per file, so nothing cheaper can
/// tell it this table moved.
const COMMIT_GAP: Duration = Duration::from_secs(5);
/// How often the retention runs.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);

/// The two queues, one per plane.
struct Queues {
    trade: Receiver<LinkEvent>,
    price: Receiver<LinkEvent>,
}

impl Queues {
    /// Up to `max` reports, the accounts' first.
    fn take(&self, max: usize) -> Vec<LinkEvent> {
        let mut batch: Vec<LinkEvent> = self.trade.try_iter().take(max).collect();
        batch.extend(self.price.try_iter().take(max - batch.len()));
        batch
    }
}

/// What a commit holds, so a stop can wait for it: `stopping` once no other may start.
struct Gate {
    stopping: bool,
    queues: Queues,
}

/// The writer thread's two facts.
struct Shared {
    gate: Mutex<Gate>,
    /// The rows a failed write cost.
    lost: AtomicU64,
}

impl Shared {
    fn new(queues: Queues) -> Self {
        Self { gate: Mutex::new(Gate { stopping: false, queues }), lost: AtomicU64::new(0) }
    }

    /// One act under the stop's gate; `None` when the daemon is stopping and nothing was done.
    fn under_gate<T>(&self, work: impl FnOnce(&Queues) -> T) -> Option<T> {
        let gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        (!gate.stopping).then(|| work(&gate.queues))
    }

    /// A failed write costs `rows`, counted and said once.
    fn lost(&self, rows: usize, why: &StoreError) {
        let count = u64::try_from(rows).unwrap_or(u64::MAX).max(1);
        if self.lost.fetch_add(count, Ordering::Relaxed) == 0 {
            tracing::warn!(
                error = %why,
                "link history: a write to the settings database failed, so its rows are lost; later ones are counted and \
                 not said (the count is logged when the daemon stops). No connection is affected"
            );
        }
    }
}

/// The running writer. Dropped when the daemon stops: see the module doc.
pub(crate) struct LinkWriter {
    shared: Arc<Shared>,
    settings_dir: PathBuf,
}

impl LinkWriter {
    /// Start the thread over the store in `settings_dir`, and give the process its sinks. A thread that will not start
    /// costs the history and nothing else, and says so.
    pub(crate) fn start(settings_dir: PathBuf) -> Self {
        let (trade_sink, trade) = sync_channel(LINK_TRADE_QUEUE);
        let (price_sink, price) = sync_channel(LINK_PRICE_QUEUE);
        let shared = Arc::new(Shared::new(Queues { trade, price }));
        let (for_thread, dir) = (Arc::clone(&shared), settings_dir.clone());
        let spawned = std::thread::Builder::new()
            .name("gw-link-history".into())
            .spawn(move || run(&dir, &for_thread, COMMIT_GAP));
        match spawned {
            Ok(_) => install(VenueLinkSinks { trade: trade_sink, price: price_sink }),
            Err(e) => {
                tracing::warn!(error = %e, "link history: its writer thread did not start, so no session is recorded")
            }
        }
        Self { shared, settings_dir }
    }
}

impl Drop for LinkWriter {
    fn drop(&mut self) {
        let mut gate = self.shared.gate.lock().unwrap_or_else(PoisonError::into_inner);
        gate.stopping = true;
        let reports = gate.queues.take(usize::MAX);
        if let Err(e) = flush_link_events(&self.settings_dir, &reports, vike_model::now_ms()) {
            self.shared.lost(reports.len(), &e);
        }
        let (lost, full) = (self.shared.lost.load(Ordering::Relaxed), dropped());
        if lost + full > 0 {
            tracing::warn!(
                lost,
                full,
                "link history: rows this process lost (a busy store, a full queue)"
            );
        }
    }
}

/// The thread: `gap` between two acts, so at most one commit in it (and the hourly prune, which commits only what it deleted).
fn run(settings_dir: &Path, shared: &Shared, gap: Duration) {
    loop {
        match shared.under_gate(|_| close_left_open(settings_dir)) {
            None => return,
            Some(Ok(())) => break,
            Some(Err(e)) => shared.lost(1, &e),
        }
        std::thread::sleep(gap);
    }
    let mut pruned_at: Option<Instant> = None;
    loop {
        let acted = shared.under_gate(|queues| {
            let batch = queues.take(BATCH_MAX);
            if !batch.is_empty()
                && let Err(e) = write_link_events(settings_dir, &batch)
            {
                shared.lost(batch.len(), &e);
            }
            if pruned_at.is_none_or(|at| at.elapsed() >= PRUNE_EVERY) {
                pruned_at = Some(Instant::now());
                if let Err(e) = prune_link_sessions(settings_dir, vike_model::now_ms()) {
                    shared.lost(1, &e);
                }
            }
        });
        if acted.is_none() {
            return;
        }
        std::thread::sleep(gap);
    }
}

/// **One data session's price streams, as rows on the price plane.** The session's sink tells this what it tells the
/// subscriber, so every venue that says `GapStart` and `Live` is covered with no bridge edit. (dukascopy and fxcm say neither:
/// their link row is `Inert`.) Dropped with the session, what is still open ends as ours.
pub(crate) struct PriceLinks(Arc<PriceOpen>);

struct PriceOpen {
    venue: String,
    /// Which network and which session of this daemon: one symbol's lane is one socket per both, so they key the row.
    scope: String,
    /// `(symbol, lane)` open now; `None` once the session has ended, when a late word from a feed thread writes nothing.
    open: Mutex<Option<BTreeSet<(String, String)>>>,
}

/// shortcut: `StreamStatus` carries no words (a measured crate), so a price row's `how` is this sentence and the journal
/// has the cause; upgrade when a status names one.
const PRICE_GAP_WORDS: &str = "the feed reported a gap in this stream (the journal has its words)";

impl PriceLinks {
    pub(crate) fn over(
        sink: &DataClientSinkStream,
        venue: &str,
        network: Option<&str>,
        session: u64,
    ) -> Self {
        let shared = Arc::new(PriceOpen::new(venue, network, session));
        let seen = Arc::clone(&shared);
        sink.tap_status(Box::new(move |symbol, said| seen.said(symbol, said)));
        Self(shared)
    }
}

impl PriceOpen {
    fn new(venue: &str, network: Option<&str>, session: u64) -> Self {
        let scope = network.map_or_else(|| format!("s{session}"), |n| format!("{n} s{session}"));
        Self { venue: venue.to_string(), scope, open: Mutex::new(Some(BTreeSet::new())) }
    }

    fn key(&self, symbol: &str, lane: &str) -> LinkKey {
        LinkKey {
            venue: self.venue.clone(),
            route: symbol.to_string(),
            plane: PLANE_PRICE,
            lane: format!("{lane} {}", self.scope),
        }
    }

    fn said(&self, symbol: &str, said: Option<(&str, StreamStatus)>) {
        let mut guard = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(open) = guard.as_mut() else { return };
        for (lane, moved) in price_moves(open, symbol, said) {
            let key = self.key(symbol, &lane);
            match moved {
                PriceMove::Opened => link_history::opened(key),
                PriceMove::Gap => link_history::closed(key, PRICE_GAP_WORDS),
                PriceMove::Released => link_history::closed_by_us(key),
            }
        }
    }
}

impl Drop for PriceLinks {
    fn drop(&mut self) {
        let open = self.0.open.lock().unwrap_or_else(PoisonError::into_inner).take();
        for (symbol, lane) in open.unwrap_or_default() {
            link_history::closed_by_us(self.0.key(&symbol, &lane));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PriceMove {
    Opened,
    Gap,
    Released,
}

/// What a sink's word moves. `Live` on a stream already open, and `Stale` (the socket is up), move nothing; `None` is the
/// symbol's subscription ending.
fn price_moves(
    open: &mut BTreeSet<(String, String)>,
    symbol: &str,
    said: Option<(&str, StreamStatus)>,
) -> Vec<(String, PriceMove)> {
    match said {
        Some((lane, StreamStatus::Live { .. })) => {
            if open.insert((symbol.to_string(), lane.to_string())) {
                vec![(lane.to_string(), PriceMove::Opened)]
            } else {
                Vec::new()
            }
        }
        Some((lane, StreamStatus::GapStart { .. })) => {
            if open.remove(&(symbol.to_string(), lane.to_string())) {
                vec![(lane.to_string(), PriceMove::Gap)]
            } else {
                Vec::new()
            }
        }
        Some(_) => Vec::new(),
        None => {
            let ending: Vec<(String, String)> =
                open.iter().filter(|(s, _)| s == symbol).cloned().collect();
            ending
                .into_iter()
                .map(|key| {
                    open.remove(&key);
                    (key.1, PriceMove::Released)
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{SyncSender, sync_channel};

    use vike_secrets::link_sessions::{HUB_STOP_WORDS, LEFT_OPEN_WORDS, PLANE_TRADE};

    use super::*;

    const TEST_GAP: Duration = Duration::from_millis(10);

    fn key() -> LinkKey {
        LinkKey {
            venue: "aster".to_string(),
            route: "aster".to_string(),
            plane: PLANE_TRADE,
            lane: "pump".to_string(),
        }
    }

    fn price_key(symbol: &str) -> LinkKey {
        LinkKey { route: symbol.to_string(), plane: PLANE_PRICE, ..key() }
    }

    /// A real store whose `venue` rows the first account's write topped up.
    fn planted_store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp");
        vike_bridge_core::credential_fixture::write_store(
            dir.path(),
            &[("aster", vike_model::AccountTier::Demo, None, Vec::new())],
        );
        dir
    }

    fn rows(dir: &Path) -> Vec<vike_model::LinkSessionRow> {
        vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(dir)),
            vike_secrets::CredentialScope::Full,
        )
        .link_sessions(0, None, None)
        .expect("read")
    }

    fn queues(depth: usize) -> (SyncSender<LinkEvent>, SyncSender<LinkEvent>, Shared) {
        let (trade_sink, trade) = sync_channel(depth);
        let (price_sink, price) = sync_channel(depth);
        (trade_sink, price_sink, Shared::new(Queues { trade, price }))
    }

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_price_stream_opens_on_live_ends_on_a_gap_and_a_stale_or_a_repeat_says_nothing() {
        let mut open = BTreeSet::new();
        let live = StreamStatus::Live { gap_started_timestamp_ms: None };
        let gap = StreamStatus::GapStart { at_timestamp_ms: 1 };
        let stale = StreamStatus::Stale { newest_data_timestamp_ms: 1, now_ms: 2 };
        let opened = vec![("quotes".to_string(), PriceMove::Opened)];
        assert_eq!(price_moves(&mut open, "BTC", Some(("quotes", live))), opened);
        assert!(price_moves(&mut open, "BTC", Some(("quotes", live))).is_empty(), "a repeat");
        assert!(
            price_moves(&mut open, "BTC", Some(("quotes", stale))).is_empty(),
            "the socket is up"
        );
        assert_eq!(
            price_moves(&mut open, "BTC", Some(("quotes", gap))),
            vec![("quotes".to_string(), PriceMove::Gap)]
        );
        assert!(price_moves(&mut open, "BTC", Some(("quotes", gap))).is_empty(), "already down");
        assert_eq!(price_moves(&mut open, "BTC", Some(("quotes", live))), opened);
    }

    /// An unsubscribe ends only that symbol's streams, and it is ours.
    #[test]
    fn a_released_symbol_ends_its_own_streams() {
        let mut open = BTreeSet::new();
        let live = StreamStatus::Live { gap_started_timestamp_ms: None };
        price_moves(&mut open, "BTC", Some(("quotes", live)));
        price_moves(&mut open, "BTC", Some(("trades", live)));
        price_moves(&mut open, "ETH", Some(("quotes", live)));
        let released = price_moves(&mut open, "BTC", None);
        assert_eq!(
            released,
            vec![
                ("quotes".to_string(), PriceMove::Released),
                ("trades".to_string(), PriceMove::Released)
            ]
        );
        assert_eq!(open.len(), 1);
    }

    /// One symbol's lane is one socket per network and per session, so none of them shares a row with another.
    #[test]
    fn a_network_and_a_session_each_key_their_own_row() {
        let lane = |network, session| {
            PriceOpen::new("hyperliquid", network, session).key("BTC", "quotes").lane
        };
        assert_eq!(lane(None, 7), "quotes s7");
        assert_eq!(lane(Some("testnet"), 7), "quotes testnet s7");
        assert_ne!(lane(None, 7), lane(Some("testnet"), 7));
        assert_ne!(lane(None, 7), lane(None, 8));
    }

    /// The thread, over a real store: what the queues hold becomes rows, and the stop's gate ends it.
    #[test]
    fn what_the_queues_hold_becomes_rows_and_the_stop_ends_the_thread() {
        let dir = planted_store();
        let (tx, _price, shared) = queues(8);
        let shared = Arc::new(shared);
        tx.send(LinkEvent::Opened { key: key(), at_ms: 1_000 }).expect("queued");
        tx.send(LinkEvent::Closed { key: key(), at_ms: 2_000, how: "closed".into(), ours: false })
            .expect("queued");
        let (for_thread, settings_dir) = (Arc::clone(&shared), dir.path().to_path_buf());
        let handle = std::thread::spawn(move || run(&settings_dir, &for_thread, TEST_GAP));

        wait_for("the row", || !rows(dir.path()).is_empty());
        let got = rows(dir.path());
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].lived_s, Some(1.0));

        shared.gate.lock().expect("lock").stopping = true;
        tx.send(LinkEvent::Opened { key: key(), at_ms: 3_000 }).expect("queued");
        handle.join().expect("the stop's gate ended the thread");
        assert_eq!(rows(dir.path()).len(), 1, "nothing starts after the stop");
    }

    /// ⚠ The accounts' reports are taken first, whatever the price queue holds.
    #[test]
    fn a_batch_takes_the_accounts_reports_first() {
        let (trade, price, shared) = queues(16);
        for at_ms in 0..10 {
            price.send(LinkEvent::Opened { key: price_key("BTC"), at_ms }).expect("queued");
        }
        for at_ms in 0..2 {
            trade.send(LinkEvent::Opened { key: key(), at_ms }).expect("queued");
        }
        let batch = shared.under_gate(|queues| queues.take(3)).expect("not stopping");
        let planes: Vec<&str> = batch
            .iter()
            .map(|e| {
                let (LinkEvent::Opened { key, .. } | LinkEvent::Closed { key, .. }) = e;
                key.plane
            })
            .collect();
        assert_eq!(planes, [PLANE_TRADE, PLANE_TRADE, PLANE_PRICE]);
    }

    /// ⚠ A store busy at the start must not let a new session meet a dead process's rows: the sweep is tried until it lands,
    /// and only then is a report taken.
    #[test]
    fn the_sweep_is_tried_until_it_lands_before_any_report_is_written() {
        let dir = planted_store();
        vike_secrets::link_sessions::write_link_events(
            dir.path(),
            &[LinkEvent::Opened { key: key(), at_ms: 1_000 }],
        )
        .expect("a row a dead process left");

        let holder = vike_secrets::hold_write_lock(dir.path());
        let (tx, _price, shared) = queues(8);
        let shared = Arc::new(shared);
        tx.send(LinkEvent::Opened { key: key(), at_ms: 2_000 }).expect("queued");
        let (for_thread, settings_dir) = (Arc::clone(&shared), dir.path().to_path_buf());
        let handle = std::thread::spawn(move || run(&settings_dir, &for_thread, TEST_GAP));
        wait_for("a refused sweep", || shared.lost.load(Ordering::Relaxed) > 0);
        drop(holder);

        wait_for("both rows", || rows(dir.path()).len() == 2);
        let got = rows(dir.path());
        let old = got.iter().find(|r| r.opened_ms == 1_000).expect("the dead process's row");
        assert_eq!(old.how.as_deref(), Some(LEFT_OPEN_WORDS), "swept, not met by the new session");
        shared.gate.lock().expect("lock").stopping = true;
        handle.join().expect("no panic");
    }

    /// ⚠ A clean stop says all it has, however much is queued, and what is still open ends as a stop of ours: no row of this
    /// process reads as open after it.
    #[test]
    fn a_clean_stop_says_all_it_has_in_one_go() {
        let dir = planted_store();
        let (trade, price, shared) = queues(512);
        for at_ms in 0..300 {
            price
                .send(LinkEvent::Opened { key: price_key(&format!("S{at_ms}")), at_ms })
                .expect("queued");
        }
        trade.send(LinkEvent::Opened { key: key(), at_ms: 500 }).expect("queued");
        let writer =
            LinkWriter { shared: Arc::new(shared), settings_dir: dir.path().to_path_buf() };
        drop(writer);
        let got = rows(dir.path());
        assert_eq!(got.len(), 301, "more than one batch, in one go");
        assert!(got.iter().all(|r| r.closed_ms.is_some() && r.ours), "none reads as open");
        assert!(got.iter().all(|r| r.how.as_deref() == Some(HUB_STOP_WORDS)));
    }

    /// A row the store refused is a counted loss and the writer goes on.
    #[test]
    fn a_failed_write_is_counted_and_the_writer_goes_on() {
        let (_trade, _price, shared) = queues(1);
        let refused = StoreError {
            path: PathBuf::from("db"),
            kind: vike_secrets::StoreErrorKind::ForeignKeys,
        };
        shared.lost(3, &refused);
        shared.lost(2, &refused);
        assert_eq!(shared.lost.load(Ordering::Relaxed), 5);
        assert!(shared.under_gate(|_| ()).is_some(), "still taking work");
    }
}
