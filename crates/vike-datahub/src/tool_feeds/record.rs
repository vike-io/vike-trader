//! **The text recorder: every NEW news item and every new or revised economic-calendar event, into
//! the store's `kind=text` series**, armed by the `flags.record_texts` row
//! (`crate::datahub_cli`'s `lanes` phase `start_text_recorder`). `vike_data::text_log`'s module doc
//! is the row's contract; this is the writer.
//!
//! # It reads the feeds as a CLIENT of the lane, never as a second poller
//!
//! Every [`TEXT_RECORD_EVERY`] it asks the lane's own two verbs ([`ToolFeedLane::news`],
//! [`ToolFeedLane::economic_calendar`]) with the generation it last stored, exactly as a desktop
//! does: an unchanged feed costs nothing, and no provider is called by this file. Its asks are
//! interest like any other, so while it runs the two feeds never pause (`super::worker`'s
//! `PAUSE_AFTER`), which is the point: a day nobody watched is recorded too. The three equity feeds
//! are never asked and stay lazy.
//!
//! # The received time
//!
//! A row's `received_ms` is the snapshot's `fetched_at_ms`: when the poll that carried it completed.
//! A feed's generation moves only when its rows move, and [`TEXT_RECORD_EVERY`] is half the shortest
//! gap the lane leaves between two polls (`super::MIN_RETRY`, the floor of every cadence row), so the
//! first snapshot this sees holding an item is the poll that brought it. If the recorder falls behind
//! (a slow store), `received_ms` comes out LATER than the truth, never earlier: the safe direction for
//! a point-in-time read.
//!
//! # What is stored, and the dedup that decides it
//!
//! [`ToolFeedTextDedup`] remembers each text's `TextRow::text_id` with its `TextRow::revision`: a news
//! item is stored once, a calendar event again whenever a number of it changes. The memory is SEEDED
//! from the store at the first pass (the rows received within [`TEXT_DEDUP_WINDOW`]), so a restart
//! does not store again what the feeds still list, and forgets a text that has not been listed for
//! that long. shortcut: an item a feed still lists after a restart, first received more than
//! [`TEXT_DEDUP_WINDOW`] earlier, is stored a second time; a store read bounded by the item's own
//! publication time would close it, if a slow feed ever shows one.
//!
//! One append per feed per new snapshot, under `text:{kind}:{collector}:{received_ms}`: a snapshot
//! written twice is one batch. A failed append keeps the generation, so the next pass asks for the
//! same snapshot and writes it under the same key; the failure is logged once per transition.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use vike_data::text_log::{TEXT_KIND_ECONOMIC_CALENDAR, TEXT_KIND_NEWS};
use vike_data::{HistoryStore, TextRow};
use vike_datahub_client::tool_feeds::{CalendarEconomicEvent, DatahubPollOutcome, NewsItem};

use super::ToolFeedLane;

/// How often the recorder asks the two feeds: half [`super::MIN_RETRY`], so at most one poll lands
/// between two asks (the module doc's received-time argument).
pub const TEXT_RECORD_EVERY: Duration = Duration::from_secs(30);
const _: () = assert!(TEXT_RECORD_EVERY.as_secs() * 2 <= super::MIN_RETRY.as_secs());

/// How long the dedup remembers a text after the feeds last listed it, and how far back the first
/// pass reads the store to seed it.
pub const TEXT_DEDUP_WINDOW: Duration = Duration::from_millis(30 * vike_model::MS_PER_DAY_U64);

/// The series `symbol` of the news: the lane's sixteen-feed RSS poller.
pub const NEWS_COLLECTOR: &str = "rss";

/// The series `symbol` of the economic calendar: ForexFactory's weekly file.
pub const CALENDAR_COLLECTOR: &str = "forexfactory";

/// One snapshot of a feed, as rows: what [`ToolFeedTextFeed`]'s read hands the recorder.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolFeedTextSnapshot {
    /// The feed's generation, the `if_generation` of the next ask.
    pub generation: u64,
    /// When the poll that carried these rows completed: every row's `received_ms`.
    pub received_ms: i64,
    /// The feed's whole current content.
    pub rows: Vec<TextRow>,
}

/// A feed's answer as a [`ToolFeedTextSnapshot`]: only a `Snapshot` carries rows; `Unchanged`,
/// `Warming`, `NotConfigured` and `NotArmed` hold nothing to record.
pub fn snapshot_of<T>(
    outcome: DatahubPollOutcome<T>,
    row: impl Fn(&T, i64) -> TextRow,
) -> Option<ToolFeedTextSnapshot> {
    match outcome {
        DatahubPollOutcome::Snapshot { rows, fetched_at_ms, generation, .. } => {
            Some(ToolFeedTextSnapshot {
                generation,
                received_ms: fetched_at_ms,
                rows: rows.iter().map(|r| row(r, fetched_at_ms)).collect(),
            })
        }
        _ => None,
    }
}

/// A news item received at `received_ms`, as a `kind=text` row.
pub fn news_row(item: &NewsItem, received_ms: i64) -> TextRow {
    TextRow {
        published_ms: item.timestamp_ms,
        title: item.title.clone(),
        summary: item.summary.clone(),
        url: item.url.clone(),
        market: item.market.clone(),
        tags: TextRow::joined_tags(&item.tags),
        ..TextRow::new(TEXT_KIND_NEWS, &item.source, received_ms)
    }
}

/// A calendar event received at `received_ms`, as a `kind=text` row: its scheduled time is the
/// `published_ms`, its provider the `source`.
pub fn calendar_row(event: &CalendarEconomicEvent, received_ms: i64) -> TextRow {
    TextRow {
        published_ms: event.timestamp_ms,
        title: event.title.clone(),
        currency: event.currency.clone(),
        impact: event.impact.clone(),
        forecast: event.forecast.clone(),
        previous: event.previous.clone(),
        actual: event.actual.clone(),
        ..TextRow::new(TEXT_KIND_ECONOMIC_CALENDAR, super::econ::PROVIDER, received_ms)
    }
}

/// **What the recorder has stored**: each text's id, its stored revision and when the feeds
/// last listed it. See the module doc.
pub struct ToolFeedTextDedup {
    window_ms: i64,
    seen: HashMap<String, (String, i64)>,
}

impl ToolFeedTextDedup {
    /// A dedup that forgets a text not listed for `window`.
    pub fn new(window: Duration) -> Self {
        let window_ms = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
        Self { window_ms, seen: HashMap::new() }
    }

    /// Remember `row` as stored (or listed) at `seen_ms`; a later revision of it replaces the earlier.
    pub fn remember(&mut self, row: &TextRow, seen_ms: i64) {
        self.seen.insert(row.text_id(), (row.revision(), seen_ms));
    }

    /// The rows of `rows` not stored in their revision yet, each id once, in feed order.
    pub fn fresh(&self, rows: &[TextRow]) -> Vec<TextRow> {
        let mut in_batch = HashSet::new();
        rows.iter()
            .filter(|r| {
                let id = r.text_id();
                let stored = self.seen.get(&id).is_some_and(|(rev, _)| *rev == r.revision());
                !stored && in_batch.insert(id)
            })
            .cloned()
            .collect()
    }

    /// Forget every text the feeds have not listed since `now_ms - window`.
    pub fn prune(&mut self, now_ms: i64) {
        let horizon = now_ms.saturating_sub(self.window_ms);
        self.seen.retain(|_, (_, seen_ms)| *seen_ms >= horizon);
    }
}

/// How a feed is asked: `if_generation` in, a snapshot (or nothing to record) out.
type ReadFn = dyn Fn(Option<u64>) -> Option<ToolFeedTextSnapshot> + Send;

/// One recorded feed: the series it writes, how it is asked, and what has been stored from it.
pub struct ToolFeedTextFeed {
    text_kind: &'static str,
    collector: &'static str,
    read: Box<ReadFn>,
    generation: Option<u64>,
    dedup: ToolFeedTextDedup,
    seeded: bool,
    failing: bool,
}

impl ToolFeedTextFeed {
    /// The feed whose rows go to `(text_kind, collector)`, asked through `read`.
    pub fn new(
        text_kind: &'static str,
        collector: &'static str,
        read: impl Fn(Option<u64>) -> Option<ToolFeedTextSnapshot> + Send + 'static,
    ) -> Self {
        Self {
            text_kind,
            collector,
            read: Box::new(read),
            generation: None,
            dedup: ToolFeedTextDedup::new(TEXT_DEDUP_WINDOW),
            seeded: false,
            failing: false,
        }
    }

    /// Log a store failure on the transition into it only.
    fn failed(&mut self, what: &str, error: &vike_data::HistoryStoreError) {
        if !self.failing {
            tracing::warn!(
                text_kind = self.text_kind,
                collector = self.collector,
                error = %error,
                "vike-datahub text recorder: {what} failed; nothing is lost yet, the next pass \
                 tries again while the feed still lists the rows"
            );
        }
        self.failing = true;
    }
}

/// **The recorder**: the feeds it records and the store it writes. See the module doc.
pub struct ToolFeedTextRecorder {
    store: Arc<dyn HistoryStore + Send + Sync>,
    feeds: Vec<ToolFeedTextFeed>,
    /// [`Self::with_write_floor`]'s check, and whether the last pass found no room (said on each change).
    write_floor: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    held_back: bool,
}

impl ToolFeedTextRecorder {
    /// A recorder of `feeds` into `store`.
    pub fn new(store: Arc<dyn HistoryStore + Send + Sync>, feeds: Vec<ToolFeedTextFeed>) -> Self {
        Self { store, feeds, write_floor: None, held_back: false }
    }

    /// This recorder, asking `floor` before each pass (REVIEW-LIVE-MONEY H1: the optional writers' free-space
    /// floor over the store's root, `Some(why)` while there is no room). Below it a pass reads and writes
    /// nothing, so each feed's generation stays and the next pass with room asks for the rows again: what the
    /// feeds still list is lost to nothing.
    #[must_use]
    pub fn with_write_floor(
        mut self,
        floor: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        self.write_floor = Some(floor);
        self
    }

    /// The production recorder: the lane's news and economic calendar.
    pub fn over_lane(lane: Arc<ToolFeedLane>, store: Arc<dyn HistoryStore + Send + Sync>) -> Self {
        let calendar_lane = Arc::clone(&lane);
        let feeds = vec![
            ToolFeedTextFeed::new(TEXT_KIND_NEWS, NEWS_COLLECTOR, move |g| {
                snapshot_of(lane.news(g), news_row)
            }),
            ToolFeedTextFeed::new(TEXT_KIND_ECONOMIC_CALENDAR, CALENDAR_COLLECTOR, move |g| {
                snapshot_of(calendar_lane.economic_calendar(g), calendar_row)
            }),
        ];
        Self::new(store, feeds)
    }

    /// One pass over every feed at `now_ms`; returns how many rows were stored.
    pub fn record_once(&mut self, now_ms: i64) -> usize {
        match self.write_floor.as_ref().and_then(|floor| floor()) {
            Some(why) => {
                if !self.held_back {
                    tracing::warn!(
                        "vike-datahub text recorder: NOTHING is recorded until the disk has room: {why}"
                    );
                }
                self.held_back = true;
                return 0;
            }
            None if self.held_back => {
                tracing::info!("vike-datahub text recorder: the disk has room again; recording");
                self.held_back = false;
            }
            None => {}
        }
        let store = Arc::clone(&self.store);
        self.feeds.iter_mut().map(|feed| record_feed(store.as_ref(), feed, now_ms)).sum()
    }
}

/// One pass over one feed: seed its dedup from the store once, ask it, store what is fresh.
fn record_feed(
    store: &(dyn HistoryStore + Send + Sync),
    feed: &mut ToolFeedTextFeed,
    now_ms: i64,
) -> usize {
    let (kind, collector) = (feed.text_kind, feed.collector);
    if !feed.seeded {
        let window_ms = feed.dedup.window_ms;
        match store.texts_as_of(kind, collector, now_ms, window_ms) {
            Ok(stored) => {
                for row in &stored {
                    feed.dedup.remember(row, row.received_ms);
                }
                feed.seeded = true;
            }
            Err(e) => {
                feed.failed("reading what is already stored", &e);
                return 0;
            }
        }
    }
    let Some(snapshot) = (feed.read)(feed.generation) else { return 0 };
    let fresh = feed.dedup.fresh(&snapshot.rows);
    let mut stored = 0;
    if !fresh.is_empty() {
        let received_ms = snapshot.received_ms;
        let key = format!("text:{kind}:{collector}:{received_ms}");
        match store.append_texts(kind, collector, &fresh, Some(&key)) {
            Ok(n) => stored = n,
            Err(e) => {
                feed.failed("an append", &e);
                return 0;
            }
        }
    }
    if feed.failing {
        tracing::info!(text_kind = kind, collector, "vike-datahub text recorder: writing again");
        feed.failing = false;
    }
    for row in &snapshot.rows {
        feed.dedup.remember(row, now_ms);
    }
    feed.dedup.prune(now_ms);
    feed.generation = Some(snapshot.generation);
    stored
}

/// Start the recorder's thread over the lane and the SERVED store handle. It runs for the life of
/// the process: a stop mid-append is the store's own crash safety, and the next start's seed keeps
/// the rows already written from being written again.
pub fn start(
    lane: Arc<ToolFeedLane>,
    store: Arc<dyn HistoryStore + Send + Sync>,
    write_floor: Arc<dyn Fn() -> Option<String> + Send + Sync>,
) -> std::io::Result<()> {
    let mut recorder = ToolFeedTextRecorder::over_lane(lane, store).with_write_floor(write_floor);
    std::thread::Builder::new().name("vike-text-recorder".to_string()).spawn(move || {
        loop {
            recorder.record_once(vike_model::now_ms());
            std::thread::sleep(TEXT_RECORD_EVERY);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use vike_data::{HistoryStoreMemory, TimestampRange};

    use super::*;

    const T0: i64 = 1_791_786_600_000;

    fn item(url: &str) -> NewsItem {
        NewsItem {
            source: "CoinDesk".into(),
            title: format!("headline {url}"),
            summary: "s".into(),
            url: url.into(),
            market: "crypto".into(),
            tags: vec!["Markets".into(), "DeFi".into()],
            timestamp_ms: T0 - 1_000,
        }
    }

    fn event(actual: &str) -> CalendarEconomicEvent {
        CalendarEconomicEvent {
            timestamp_ms: T0 + 3_600_000,
            currency: "USD".into(),
            title: "Non-Farm Employment Change".into(),
            impact: "High".into(),
            forecast: "150K".into(),
            previous: "142K".into(),
            actual: actual.into(),
        }
    }

    fn outcome<T>(rows: Vec<T>, fetched_at_ms: i64, generation: u64) -> DatahubPollOutcome<T> {
        DatahubPollOutcome::Snapshot {
            rows,
            fetched_at_ms,
            generation,
            sources_ok: 1,
            sources_total: 1,
            cadence_secs: 300,
        }
    }

    /// A feed that answers the scripted outcomes in turn, then `Unchanged`, and records the
    /// `if_generation` of every ask.
    fn scripted<T: Send + 'static>(
        text_kind: &'static str,
        collector: &'static str,
        script: Vec<DatahubPollOutcome<T>>,
        row: fn(&T, i64) -> TextRow,
    ) -> (ToolFeedTextFeed, Arc<Mutex<Vec<Option<u64>>>>) {
        let asks = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&asks);
        let script = Mutex::new(script.into_iter().collect::<VecDeque<_>>());
        let feed = ToolFeedTextFeed::new(text_kind, collector, move |g| {
            seen.lock().unwrap().push(g);
            let next = script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(DatahubPollOutcome::Unchanged { generation: g.unwrap_or(0) });
            snapshot_of(next, row)
        });
        (feed, asks)
    }

    fn stored(store: &HistoryStoreMemory, kind: &str, collector: &str) -> Vec<TextRow> {
        store.scan_texts(kind, collector, TimestampRange::all()).unwrap()
    }

    /// REVIEW-LIVE-MONEY H1: below the free-space floor (a FAKE disk of 1 GiB free of 100 through
    /// `vike_boot::disk_floor`) a pass neither asks the feed nor writes; with room again the same snapshot is
    /// asked for and stored, so nothing the feed still lists is lost.
    #[test]
    fn below_the_free_space_floor_a_pass_records_nothing_and_room_records_it_after() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let store = Arc::new(HistoryStoreMemory::new());
        let (feed, asks) = scripted(
            TEXT_KIND_NEWS,
            NEWS_COLLECTOR,
            vec![outcome(vec![item("https://a.test/1")], T0, 7)],
            news_row,
        );
        let free = Arc::new(AtomicU64::new(1 << 30));
        let reader = Arc::clone(&free);
        let floor = move || {
            vike_boot::disk_floor::DiskFloor::of_mib(10_240).refusal_with(
                &std::env::temp_dir(),
                |_: &std::path::Path| {
                    Ok(vike_boot::disk_floor::FilesystemSpace {
                        available_bytes: reader.load(Ordering::SeqCst),
                        total_bytes: 100 << 30,
                    })
                },
            )
        };
        let mut recorder = ToolFeedTextRecorder::new(Arc::clone(&store) as _, vec![feed])
            .with_write_floor(Arc::new(floor));
        assert_eq!(recorder.record_once(T0), 0);
        assert!(asks.lock().unwrap().is_empty(), "below the floor the feed is not even asked");
        assert!(stored(&store, TEXT_KIND_NEWS, NEWS_COLLECTOR).is_empty());

        free.store(50 << 30, Ordering::SeqCst);
        assert_eq!(recorder.record_once(T0 + 30_000), 1, "room: the listed item is stored");
        assert_eq!(stored(&store, TEXT_KIND_NEWS, NEWS_COLLECTOR).len(), 1);
    }

    /// A news item is stored ONCE, with the received time of the poll that brought it; a later
    /// snapshot stores only what is new, and every ask carries the generation last stored.
    #[test]
    fn news_is_stored_once_with_the_time_of_the_poll_that_brought_it() {
        let store = Arc::new(HistoryStoreMemory::new());
        let (feed, asks) = scripted(
            TEXT_KIND_NEWS,
            NEWS_COLLECTOR,
            vec![
                outcome(vec![item("https://a.test/1"), item("https://a.test/2")], T0, 7),
                outcome(vec![item("https://a.test/3"), item("https://a.test/1")], T0 + 300_000, 8),
            ],
            news_row,
        );
        let mut recorder = ToolFeedTextRecorder::new(Arc::clone(&store) as _, vec![feed]);

        assert_eq!(recorder.record_once(T0 + 10), 2, "the first snapshot is all new");
        assert_eq!(recorder.record_once(T0 + 300_010), 1, "only the new item of the second");
        assert_eq!(recorder.record_once(T0 + 300_040), 0, "unchanged: nothing");
        assert_eq!(*asks.lock().unwrap(), [None, Some(7), Some(8)]);

        let rows = stored(&store, TEXT_KIND_NEWS, NEWS_COLLECTOR);
        let got: Vec<(&str, i64)> = rows.iter().map(|r| (r.url.as_str(), r.received_ms)).collect();
        assert_eq!(
            got,
            [
                ("https://a.test/1", T0),
                ("https://a.test/2", T0),
                ("https://a.test/3", T0 + 300_000)
            ]
        );
        let first = &rows[0];
        assert_eq!((first.published_ms, first.source.as_str()), (T0 - 1_000, "CoinDesk"));
        assert_eq!(first.tag_list().collect::<Vec<_>>(), ["Markets", "DeFi"]);
        assert!(first.currency.is_empty() && first.actual.is_empty(), "no calendar fields");
    }

    /// A calendar event is stored when it appears and again when its actual lands — two rows, each
    /// with its own received time — and not a third time while nothing changes.
    #[test]
    fn a_calendar_event_is_stored_again_only_when_a_number_changes() {
        let store = Arc::new(HistoryStoreMemory::new());
        let (feed, _) = scripted(
            TEXT_KIND_ECONOMIC_CALENDAR,
            CALENDAR_COLLECTOR,
            vec![
                outcome(vec![event("")], T0, 1),
                outcome(vec![event("")], T0 + 1_800_000, 2),
                outcome(vec![event("175K")], T0 + 3_600_500, 3),
            ],
            calendar_row,
        );
        let mut recorder = ToolFeedTextRecorder::new(Arc::clone(&store) as _, vec![feed]);
        let passes: Vec<usize> =
            [T0, T0 + 1_800_000, T0 + 3_600_500].iter().map(|t| recorder.record_once(*t)).collect();
        assert_eq!(passes, [1, 0, 1]);

        let rows = stored(&store, TEXT_KIND_ECONOMIC_CALENDAR, CALENDAR_COLLECTOR);
        let got: Vec<(&str, i64)> =
            rows.iter().map(|r| (r.actual.as_str(), r.received_ms)).collect();
        assert_eq!(got, [("", T0), ("175K", T0 + 3_600_500)]);
        assert_eq!(rows[0].source, super::super::econ::PROVIDER);
        assert_eq!((rows[0].currency.as_str(), rows[0].published_ms), ("USD", T0 + 3_600_000));
    }

    /// A restarted recorder seeds from the store: the same feed content stores nothing again.
    #[test]
    fn a_restarted_recorder_does_not_store_again_what_the_feed_still_lists() {
        let store = Arc::new(HistoryStoreMemory::new());
        let snapshot = || outcome(vec![item("https://a.test/1")], T0, 4);
        let (feed, _) = scripted(TEXT_KIND_NEWS, NEWS_COLLECTOR, vec![snapshot()], news_row);
        assert_eq!(
            ToolFeedTextRecorder::new(Arc::clone(&store) as _, vec![feed]).record_once(T0),
            1
        );

        // A new process: a fresh generation counter, the same item, a later poll.
        let (feed, _) = scripted(
            TEXT_KIND_NEWS,
            NEWS_COLLECTOR,
            vec![outcome(vec![item("https://a.test/1")], T0 + 60_000, 99)],
            news_row,
        );
        let mut again = ToolFeedTextRecorder::new(Arc::clone(&store) as _, vec![feed]);
        assert_eq!(again.record_once(T0 + 60_000), 0, "seeded from the store");
        assert_eq!(stored(&store, TEXT_KIND_NEWS, NEWS_COLLECTOR).len(), 1);
    }

    /// The dedup forgets a text the feeds stopped listing a window ago, and keeps one still listed.
    #[test]
    fn the_dedup_forgets_only_what_was_not_listed_for_a_window() {
        let mut dedup = ToolFeedTextDedup::new(Duration::from_millis(1_000));
        let a = news_row(&item("https://a.test/a"), T0);
        let b = news_row(&item("https://a.test/b"), T0);
        dedup.remember(&a, T0);
        dedup.remember(&b, T0 + 900);
        dedup.prune(T0 + 1_500);
        assert_eq!(
            dedup.fresh(&[a.clone(), b.clone(), a.clone()]),
            vec![a],
            "a was last listed 1.5 s ago and is forgotten (once per batch); b is still known"
        );
    }

    /// Only a `Snapshot` carries rows; every other answer records nothing.
    #[test]
    fn only_a_snapshot_is_recorded() {
        for nothing in [
            DatahubPollOutcome::Unchanged { generation: 3 },
            DatahubPollOutcome::Warming,
            DatahubPollOutcome::NotConfigured { key_name: "K".into() },
            DatahubPollOutcome::NotArmed,
        ] {
            assert!(snapshot_of::<NewsItem>(nothing, news_row).is_none());
        }
    }
}
