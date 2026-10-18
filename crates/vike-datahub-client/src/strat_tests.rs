//! `LinkSink`'s decisions, with no socket: which `GapStart` is the link's, what closes it, what must pass
//! untouched. The behaviour over a real link (the engine's own loss disclosure, the replay, the dial and
//! the open) is `tests/strat_feed.rs`.

use std::sync::atomic::{AtomicBool, Ordering};

use vike_data::{RecordingSink, SinkCall};
use vike_marketdata::{BookUpdateKind, QuoteTick, TradeTick};

use super::*;

const WORD: &str = STRAT_LINK_STREAM;

struct Rig {
    rec: Arc<RecordingSink>,
    sink: Arc<LinkSink>,
    down: Arc<AtomicBool>,
}

fn rig() -> Rig {
    let rec = Arc::new(RecordingSink::default());
    let sink = Arc::new(LinkSink::new(rec.clone()));
    let down = Arc::new(AtomicBool::new(false));
    let d = Arc::clone(&down);
    sink.down.set(Box::new(move || d.load(Ordering::SeqCst))).ok();
    // The feed counts a symbol before it asks the engine for it: the two these tests drive are held.
    sink.hold("S");
    sink.hold("T");
    Rig { rec, sink, down }
}

/// Every `stream_status` the host's sink was handed: `(symbol, stream word, status)`.
fn statuses(rec: &RecordingSink) -> Vec<(String, String, StreamStatus)> {
    rec.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { symbol, stream, status, .. } => Some((symbol, stream, status)),
            _ => None,
        })
        .collect()
}

fn gap(at: i64) -> StreamStatus {
    StreamStatus::GapStart { at_ts_ms: at }
}

fn live(at: Option<i64>) -> StreamStatus {
    StreamStatus::Live { gap_started_ts_ms: at }
}

fn bar() -> Bar {
    Bar {
        ts: 1,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Twelve verbs in, twelve calls out, each the verb that went in: a verb added to `LiveDataSink` with a
/// default body compiles in `LinkSink` without being forwarded, and this list is where it is missed.
#[test]
fn every_sink_verb_is_forwarded() {
    let r = rig();
    let s = &r.sink;
    s.seed_bars("v", "S", "1m", vec![bar()]);
    s.close_bar("v", "S", "1m", bar());
    s.forming_bar("v", "S", "1m", bar());
    s.mark_tick("v", "S", 1.0, 2);
    s.bar_close_tick("v", "S", 1.0, 2);
    s.l2_snapshot("v", "S", 0.5, vec![BookLevel { price: 1.0, qty: 1.0 }], vec![], 3);
    s.quote(
        "v",
        "S",
        QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    );
    s.trade(
        "v",
        "S",
        TradeTick {
            ts: 1,
            local_ts: 0,
            price: 1.5,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        },
    );
    s.book("v", "S", Arc::new(L2Book::new(0.5)));
    s.book_update(
        "v",
        "S",
        BookUpdate {
            ts: 1,
            local_ts: 0,
            seq: 1,
            kind: BookUpdateKind::Delta,
            tick_size: 0.5,
            bids: vec![],
            asks: vec![],
            symbol: String::new(),
        },
    );
    s.stream_status("v", "S", "quotes", live(None));
    s.data_delay("v", "S", "quotes", DataDelay::RealTime);
    let calls = r.rec.recorded();
    assert_eq!(calls.len(), 12, "every verb reached the host's sink exactly once: {calls:?}");
    let kinds: Vec<&str> = calls
        .iter()
        .map(|c| match c {
            SinkCall::SeedBars { .. } => "seed_bars",
            SinkCall::CloseBar { .. } => "close_bar",
            SinkCall::FormingBar { .. } => "forming_bar",
            SinkCall::MarkTick { .. } => "mark_tick",
            SinkCall::BarCloseTick { .. } => "bar_close_tick",
            SinkCall::L2Snapshot { .. } => "l2_snapshot",
            SinkCall::Quote { .. } => "quote",
            SinkCall::Trade { .. } => "trade",
            SinkCall::Book { .. } => "book",
            SinkCall::BookUpdate { .. } => "book_update",
            SinkCall::StreamStatus { .. } => "stream_status",
            SinkCall::DataDelay { .. } => "data_delay",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "seed_bars",
            "close_bar",
            "forming_bar",
            "mark_tick",
            "bar_close_tick",
            "l2_snapshot",
            "quote",
            "trade",
            "book",
            "book_update",
            "stream_status",
            "data_delay"
        ]
    );
}

/// A link loss reaches the host as ONE `GapStart` per held symbol under the strat-link word, carrying
/// the engine's stamp (when the gap opened), and never as the lane's own word: the venue's `Live` must
/// not be able to close it.
#[test]
fn a_link_loss_is_disclosed_once_per_symbol_under_the_strat_link_word() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.sink.stream_status("v", "S", "trades", gap(90));
    r.sink.stream_status("v", "S", "1m", gap(95));
    r.sink.stream_status("v", "T", "quotes", gap(120));
    assert_eq!(
        statuses(&r.rec),
        vec![("S".into(), WORD.into(), gap(100)), ("T".into(), WORD.into(), gap(120))],
        "one disclosure per symbol, the first lane's stamp, no lane word"
    );
    assert_eq!(r.sink.open_gaps(), 2);
}

/// A status the peer FORWARDED (the session is open) is the venue's own disclosure and passes untouched
/// with its lane word, and so does the `Live` that answers it: nothing the link did is withheld.
#[test]
fn a_status_the_peer_forwarded_passes_unchanged() {
    let r = rig();
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.sink.stream_status("v", "S", "quotes", live(Some(100)));
    assert_eq!(
        statuses(&r.rec),
        vec![
            ("S".into(), "quotes".into(), gap(100)),
            ("S".into(), "quotes".into(), live(Some(100)))
        ]
    );
    assert_eq!(r.sink.open_gaps(), 0);
}

/// A sink whose engine could not be probed (`down` unset) treats nothing as a link loss: the engine's
/// plain behaviour, never a guess.
#[test]
fn without_a_probe_nothing_is_a_link_loss() {
    let rec = Arc::new(RecordingSink::default());
    let sink = LinkSink::new(rec.clone());
    sink.stream_status("v", "S", "quotes", gap(100));
    assert_eq!(statuses(&rec), vec![("S".into(), "quotes".into(), gap(100))]);
    assert_eq!(sink.open_gaps(), 0);
}

/// The engine's `Live` for a re-acknowledged lane closes the symbol's strat-link gap with the stamp the
/// gap OPENED at, and the other lanes' `Live`s of that outage are withheld too.
#[test]
fn the_reacknowledgement_closes_the_gap_with_its_opening_stamp() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.sink.stream_status("v", "S", "trades", gap(90));
    r.down.store(false, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", live(Some(100)));
    r.sink.stream_status("v", "S", "trades", live(Some(90)));
    assert_eq!(
        statuses(&r.rec),
        vec![("S".into(), WORD.into(), gap(100)), ("S".into(), WORD.into(), live(Some(100)))],
        "one close, stamped when the gap opened; the lanes' own Lives are withheld"
    );
    assert_eq!(r.sink.open_gaps(), 0);
}

/// A venue `Live` the link did not withhold passes: eating one would close a venue-socket outage the
/// link never opened.
#[test]
fn a_venue_live_with_nothing_withheld_passes() {
    let r = rig();
    r.sink.stream_status("v", "S", "quotes", live(Some(5)));
    assert_eq!(statuses(&r.rec), vec![("S".into(), "quotes".into(), live(Some(5)))]);
}

/// `Stale` is never the link's: it passes with its lane word and opens no gap, down or up.
#[test]
fn stale_is_never_synthesized_or_withheld() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    let stale = StreamStatus::Stale { newest_data_ts_ms: 1, now_ms: 2 };
    r.sink.stream_status("v", "S", "quotes", stale);
    assert_eq!(statuses(&r.rec), vec![("S".into(), "quotes".into(), stale)]);
    assert_eq!(r.sink.open_gaps(), 0);
}

/// An unsubscribe of the symbol's LAST subscription during the outage closes the gap on the spot: the
/// engine drops it from its replay and says nothing, and the dead-man's window would otherwise stay open
/// until it tripped. The withheld `Live` of the pair is forgotten with it, so a later genuine one passes.
#[test]
fn giving_up_the_last_subscription_closes_the_gap_at_once() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.down.store(false, Ordering::SeqCst);
    r.sink.released("v", "S", "quotes", false, false);
    assert_eq!(
        statuses(&r.rec),
        vec![("S".into(), WORD.into(), gap(100)), ("S".into(), WORD.into(), live(Some(100)))]
    );
    assert_eq!(r.sink.open_gaps(), 0);
    r.sink.stream_status("v", "S", "quotes", live(Some(7)));
    assert_eq!(statuses(&r.rec).len(), 3, "the venue's Live for the pair is no longer withheld");
}

/// A symbol that still has another held subscription keeps its gap open (its replay will close it);
/// only the pair nobody holds is forgotten.
#[test]
fn giving_up_one_of_two_lanes_keeps_the_gap_open() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.sink.stream_status("v", "S", "trades", gap(100));
    r.down.store(false, Ordering::SeqCst);
    r.sink.released("v", "S", "quotes", false, true);
    assert_eq!(statuses(&r.rec).len(), 1, "no close while the symbol still has a lane");
    assert_eq!(r.sink.open_gaps(), 1);
    // The surviving lane's re-acknowledgement closes it; the given-up lane's Live is no longer withheld.
    r.sink.stream_status("v", "S", "trades", live(Some(100)));
    assert_eq!(statuses(&r.rec).last(), Some(&("S".into(), WORD.into(), live(Some(100)))));
    r.sink.stream_status("v", "S", "quotes", live(Some(100)));
    assert_eq!(statuses(&r.rec).last(), Some(&("S".into(), "quotes".into(), live(Some(100)))));
}

#[test]
fn a_stated_heartbeat_is_clamped_into_the_lane_range() {
    assert_eq!(stated_heartbeat(0), SL_HEARTBEAT_MIN);
    assert_eq!(stated_heartbeat(1), SL_HEARTBEAT_MIN);
    assert_eq!(stated_heartbeat(5_000), Duration::from_secs(5));
    assert_eq!(stated_heartbeat(3_600_000), SL_HEARTBEAT_MAX);
}

/// The opener states this lane's numbers and never prints a key.
#[test]
fn the_opener_states_the_lane_numbers_and_redacts_its_keys() {
    let o = StratOpener::new(
        "127.0.0.1:7878",
        HubKeys::new(b"observe-secret".to_vec(), b"control-secret".to_vec())
            .with_trade(b"trade-secret".to_vec()),
        StratTimings::default(),
    )
    .on_network(Some("testnet"));
    let limits = o.limits();
    assert_eq!(limits.up_frame_ceiling_bytes, SL_UP_FRAME_CEILING_BYTES);
    assert_eq!(limits.down_frame_ceiling_bytes, SL_FRAME_CEILING_BYTES);
    assert_eq!(limits.max_symbol_bytes, SL_MAX_SYMBOL_BYTES);
    assert_eq!(o.timings().stream_read_floor, SL_READ_FLOOR);
    assert_eq!(o.addr(), "127.0.0.1:7878");
    let shown = format!("{o:?}");
    assert!(shown.contains("testnet") && shown.contains("trade=set"), "{shown}");
    for secret in ["observe-secret", "control-secret", "trade-secret"] {
        assert!(!shown.contains(secret), "a key reached Debug: {shown}");
    }
    assert_eq!(o.failure_text(&"boom"), "datahub 127.0.0.1:7878: strategy lane: boom");
    assert_eq!(
        o.failure_text(&"datahub auth denied (Trade): no"),
        "datahub auth denied (Trade): no"
    );
}

// ---- W10b: the gap-closure holes and the holed book lane ---------------------------------------------

/// **A sibling lane that came back cannot vouch for one the peer refused for good** (review finding 5):
/// the symbol's gap closes at the LAST owed lane's re-acknowledgement, so a pump that never answers keeps
/// the window open (the dead-man trips on a feed that is truly not streaming) and the owner giving that
/// lane up closes it.
#[test]
fn a_live_sibling_lane_cannot_mask_a_lane_that_never_came_back() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.sink.stream_status("v", "S", "ticks", gap(100));
    r.down.store(false, Ordering::SeqCst);
    // Quotes come back; the pump (refused for good on replay) does not.
    r.sink.stream_status("v", "S", "quotes", live(Some(100)));
    assert_eq!(
        statuses(&r.rec).len(),
        1,
        "no close while the pump is owed: {:?}",
        statuses(&r.rec)
    );
    assert_eq!(r.sink.open_gaps(), 1, "the window stays open, for the dead-man to act on");
    // The owner gives the dead pump up while quotes are still served: nothing is owed any more.
    r.sink.released("v", "S", "ticks", false, true);
    assert_eq!(
        statuses(&r.rec).last(),
        Some(&("S".into(), WORD.into(), live(Some(100)))),
        "the last owed lane is gone, so the gap closes"
    );
    assert_eq!(r.sink.open_gaps(), 0);
}

/// **An unsubscribe racing a link loss leaves no window nobody closes** (review finding 4): the engine's
/// `GapStart` list is computed before the unsubscribe, and delivered after the feed released the symbol.
/// A symbol the feed no longer holds is not disclosed, so no gap opens that no replay `Live` could close.
#[test]
fn a_link_loss_for_a_symbol_the_feed_no_longer_holds_is_not_disclosed() {
    let r = rig();
    // The unsubscribe returned (the symbol's last subscription): the feed gave it back.
    r.sink.released("v", "S", "quotes", false, false);
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    assert!(statuses(&r.rec).is_empty(), "{:?}", statuses(&r.rec));
    assert_eq!(r.sink.open_gaps(), 0, "no window was opened for a symbol nobody holds");
    // A symbol that IS held is disclosed as ever, and a refused subscribe gives its count back.
    r.sink.stream_status("v", "T", "quotes", gap(110));
    assert_eq!(statuses(&r.rec).len(), 1);
    r.sink.hold("U");
    r.sink.hold_failed("U");
    r.sink.stream_status("v", "U", "quotes", gap(120));
    assert_eq!(statuses(&r.rec).len(), 1, "a subscribe that failed held nothing");
}

/// **A feed being torn down closes its open gaps** (W6 DOUBT 3, review finding 5): a hot reload or a
/// disarm leaves no lane to be in doubt about, and the window would otherwise stay open until it tripped.
/// Idempotent, and a `GapStart` the engine still delivers afterwards opens nothing.
#[test]
fn closing_a_feed_closes_every_open_gap_once_and_forgets_what_it_held() {
    let r = rig();
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "quotes", gap(100));
    r.sink.stream_status("v", "T", "quotes", gap(120));
    assert_eq!(r.sink.open_gaps(), 2);
    r.sink.close_all("v");
    let closes: Vec<_> = statuses(&r.rec)
        .into_iter()
        .filter(|(_, w, s)| w == WORD && matches!(s, StreamStatus::Live { .. }))
        .collect();
    assert_eq!(
        closes,
        vec![
            ("S".into(), WORD.into(), live(Some(100))),
            ("T".into(), WORD.into(), live(Some(120)))
        ]
    );
    assert_eq!(r.sink.open_gaps(), 0);
    r.sink.close_all("v");
    assert_eq!(statuses(&r.rec).len(), 4, "the second close says nothing");
    r.sink.stream_status("v", "S", "quotes", gap(130));
    assert_eq!(statuses(&r.rec).len(), 4, "a late GapStart for a closed feed opens nothing");
}

fn delta(seq: u64) -> BookUpdate {
    BookUpdate {
        ts: 1,
        local_ts: 0,
        seq,
        kind: BookUpdateKind::Delta,
        tick_size: 0.5,
        bids: vec![],
        asks: vec![],
        symbol: String::new(),
    }
}

fn snapshot(seq: u64) -> BookUpdate {
    BookUpdate { kind: BookUpdateKind::Snapshot, ..delta(seq) }
}

fn book_kinds(rec: &RecordingSink) -> Vec<(u64, BookUpdateKind)> {
    rec.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::BookUpdate { update, .. } => Some((update.seq, update.kind)),
            _ => None,
        })
        .collect()
}

/// **A holed book lane waits for its anchor** (review finding 3a): after a hole the engine disclosed under
/// the book word, deltas are withheld and counted (a delta applied past a hole corrupts the book silently),
/// an anchor passes and re-bases the lane, and the flag a consumer polls follows. The link's own loss
/// under the book word is NOT a hole: the replay brings a fresh anchor.
#[test]
fn a_holed_book_lane_withholds_deltas_until_the_next_anchor() {
    let r = rig();
    r.sink.book_update("v", "S", delta(1));
    assert!(!r.sink.book_stale("S"));
    // The engine's hole disclosure: a GapStart/Live pair under the book word while the session is up.
    r.sink.stream_status("v", "S", "book", gap(100));
    r.sink.stream_status("v", "S", "book", live(Some(100)));
    assert!(r.sink.book_stale("S"), "the lane is stale from the hole");
    assert!(!r.sink.book_stale("T"), "another symbol's lane is untouched");
    r.sink.book_update("v", "S", delta(2));
    r.sink.book_update("v", "S", delta(3));
    assert_eq!(r.sink.book_deltas_withheld(), 2);
    r.sink.book_update("v", "T", delta(1));
    r.sink.book_update("v", "S", snapshot(4));
    assert!(!r.sink.book_stale("S"), "an anchor re-bases the lane");
    r.sink.book_update("v", "S", delta(5));
    assert_eq!(
        book_kinds(&r.rec),
        vec![
            (1, BookUpdateKind::Delta),
            (1, BookUpdateKind::Delta),
            (4, BookUpdateKind::Snapshot),
            (5, BookUpdateKind::Delta)
        ],
        "the deltas between the hole and the anchor never reached the consumer"
    );
    assert_eq!(statuses(&r.rec).len(), 2, "the disclosure itself passes untouched");
    // A whole folded book re-anchors too.
    r.sink.stream_status("v", "S", "book", gap(200));
    assert!(r.sink.book_stale("S"));
    r.sink.book("v", "S", Arc::new(L2Book::new(0.5)));
    assert!(!r.sink.book_stale("S"));
    // The LINK's loss under the book word is not a hole.
    r.down.store(true, Ordering::SeqCst);
    r.sink.stream_status("v", "S", "book", gap(300));
    assert!(!r.sink.book_stale("S"), "the replay anchors a fresh book");
}
