//! **The desktop chart renders the series its backend actually publishes** — the whole loop, in
//! one headless test, over the REAL functions the shell calls.
//!
//! # What this reproduces
//!
//! MEASURED against a live daemon: a `vike-tradehub` built from `main`, mounting bybit `BTCUSDT`
//! `1m`, was stood up on a spare port and a thin client attached to it. The node published
//! `bybit:BTCUSDT@1m`. The GUI was subscribed to `BTCUSDT@1m` — [`startup::plan`]'s default chart,
//! keyed through [`workspace::series_key`] off `DEFAULT_VENUE == "binance"`. Every frame,
//! [`core_sync::sync_from_core`] logged
//!
//! ```text
//! a published bar series is not being rendered
//!   series="bybit:BTCUSDT@1m"
//!   reason="not subscribed GUI-side (`spawned` holds no such key)"
//!   subscribed=BTCUSDT@1m
//! ```
//!
//! and the chart stayed empty while the title bar said `● LIVE`.
//!
//! # Why the loop, and not the planner
//!
//! `crates/vike-app-core/src/series_follow.rs`'s own unit tests already state the RULE over
//! `WinState`s. They cannot state the DEFECT, because the defect is a disagreement between four
//! separate mechanisms — the startup layout, `ensure_feed_on`'s `spawned` set, the wire bridge, and
//! the snapshot fold's key filter — and a test that mocks any one of them would pass with the bug
//! in place. So every step below is the shipped function: [`startup::plan`] builds the window,
//! [`feed_lifecycle::ensure_feed_on`] fills `spawned` exactly as the shell's `ensure_feed_on`
//! wrapper does, [`observe_bridge::wire_to_core`] turns the daemon's wire frame into the snapshot,
//! and [`core_sync::sync_from_core`] folds it. The only thing written by hand is the wire frame,
//! which is the daemon's side of the seam.
//!
//! ⚠ **Step 3 asserts the OLD behaviour and must keep passing.** It is what makes step 6 mean
//! something: without it, a fix that accidentally made every series render would look identical to
//! a fix that made the right one render.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use vike_app_core::data_sink::TradeStore;
use vike_app_core::{core_sync, feed_lifecycle, observe_bridge, series_follow, startup, workspace};
use vike_chart::{DisplayTz, model};
use vike_tradehub_client::wire::{WireBar, WireBarSeries};

/// The daemon's frame: a node whose PRIMARY ENGINE reads `binance` / `BTCUSDT` (which is what
/// the CI box reports, and is NOT what it mounts) publishing one bybit series.
///
/// The two scalars are set to the misleading values ON PURPOSE — any fix that keys off
/// `WireSnapshot::venue` / `::symbol` instead of off the series list passes on a node where they
/// happen to agree and fails on the one that was measured.
fn bybit_node(seq: u64, closed: &[i64]) -> vike_tradehub_client::WireSnapshot {
    let mut w = vike_tradehub_client::WireSnapshot::empty();
    w.seq = seq;
    w.venue = "binance".into();
    w.symbol = "BTCUSDT".into();
    w.bars = vec![WireBarSeries {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        closed: closed
            .iter()
            .map(|&ts| WireBar { ts, o: 100.0, h: 101.0, l: 99.0, c: 100.5, v: 3.0 })
            .collect(),
        forming: None,
    }];
    w
}

/// Everything the shell owns that this loop touches — the same fields, under the same names.
#[derive(Default)]
struct Gui {
    feeds: feed_lifecycle::FeedMap,
    subs: HashMap<String, vike_data::SubscriptionId>,
    spawned: HashSet<String>,
    unroutable: HashSet<String>,
    retries: feed_lifecycle::FeedRetries,
    charts: HashMap<String, model::ChartState>,
    hidden: HashSet<String>,
    aggs: HashMap<String, (String, String, vike_app_core::tickvol::TickVolAgg)>,
    of_aggs: HashMap<String, (String, String, vike_app_core::orderflow::OrderflowAgg)>,
    bf_pending: HashMap<String, Vec<vike_model::TradeTick>>,
    published: Vec<series_follow::PublishedSeries>,
    last_seq: u64,
    last_direct_gen: u64,
    status: String,
    wins: Vec<workspace::WinState>,
}

impl Gui {
    /// `App::ensure_feed_on`, verbatim in shape: the shell's wrapper is a call to this with its own
    /// fields. `--observe` has no venue clients, so `feeds` stays EMPTY here exactly as it is
    /// there — which is the configuration in which `spawned` is the only thing that decides what
    /// renders.
    fn ensure(&mut self, spec: &startup::FeedSpec) {
        feed_lifecycle::ensure_feed_on(
            &mut feed_lifecycle::FeedSlots {
                feeds: &mut self.feeds,
                subs: &mut self.subs,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                retries: &mut self.retries,
            },
            &mut feed_lifecycle::SeriesSlots {
                charts: &mut self.charts,
                hidden: &mut self.hidden,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
            },
            feed_lifecycle::SeriesSpec {
                venue: &spec.venue,
                symbol: &spec.symbol,
                interval: &spec.interval,
                asset_class: spec.asset_class,
            },
            DisplayTz::Utc,
            false,
        );
    }

    /// `App::sync_from_core`, verbatim in shape.
    fn fold(&mut self, snap: &vike_core::CoreSnapshot) {
        let trades = TradeStore::default();
        let (_tx, bf_rx) = std::sync::mpsc::channel();
        let feed_status = Mutex::new(String::new());
        let no_gaps = |_: &str, _: &str| 0u64;
        core_sync::sync_from_core(
            core_sync::CoreSyncInputs {
                snap,
                spawned: &self.spawned,
                hidden: &self.hidden,
                display_tz: DisplayTz::Utc,
                trades: &trades,
                bf_rx: &bf_rx,
                feed_status: &feed_status,
                direct_bars: None,
                tape_gaps: &no_gaps,
            },
            core_sync::CoreSyncState {
                charts: &mut self.charts,
                aggs: &mut self.aggs,
                of_aggs: &mut self.of_aggs,
                bf_pending: &mut self.bf_pending,
                published: &mut self.published,
                last_seq: &mut self.last_seq,
                last_direct_gen: &mut self.last_direct_gen,
                status: &mut self.status,
            },
        );
    }

    /// The frame-loop block this change adds to `crates/vike-desktop/src/app_ui.rs`'s
    /// `draw_windows`: plan, apply, ensure, and force ONE refold so the bars already sitting in the
    /// snapshot paint on the next frame instead of waiting on the node's publish cadence.
    fn follow_backend(&mut self) {
        let adopted = series_follow::follow_backend(&mut self.wins, &self.charts, &self.published);
        if !adopted.is_empty() {
            self.last_seq = core_sync::FORCE_REFOLD;
        }
        for spec in &adopted {
            self.ensure(spec);
        }
    }

    fn bars_on(&self, key: &str) -> usize {
        self.charts.get(key).map(|c| c.bars.len()).unwrap_or(0)
    }
}

/// A GUI started exactly as the shell starts one with no saved workspace: [`startup::plan`]'s
/// default layout, every `FeedSpec` it asks for ensured.
fn fresh_gui() -> Gui {
    let mut g = Gui::default();
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
    let layout = startup::plan(area, &startup::StartupEnv::default(), None);
    for spec in &layout.ensure_feeds {
        g.ensure(spec);
    }
    g.wins = layout.wins;
    g
}

#[test]
fn a_bybit_node_paints_the_default_chart_that_started_on_binance() {
    let mut g = fresh_gui();

    // 1. The GUI starts on the invented default, and that is the whole defect: nothing chose it.
    assert_eq!(g.wins.len(), 1);
    assert_eq!(g.wins[0].key(), "BTCUSDT@1m");
    assert!(!g.wins[0].series_pinned, "the startup chart is a placeholder, not a choice");
    assert!(g.spawned.contains("BTCUSDT@1m"));
    assert!(!g.spawned.contains("bybit:BTCUSDT@1m"));

    // 2. The daemon's first frame, through the REAL wire bridge.
    let snap = observe_bridge::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
    assert_eq!(snap.venue, "binance", "the primary-engine scalars lie; nothing may key off them");
    g.fold(&snap);

    // 3. THE MEASURED FAILURE, still exactly as measured: the key the node publishes is not the key
    //    the GUI subscribed, so the fold drops it. This assertion must SURVIVE the fix — it is what
    //    proves step 6 renders the right series rather than every series.
    assert_eq!(g.bars_on("BTCUSDT@1m"), 0, "the binance-keyed chart has, and gets, nothing");
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 0, "...and the published key has no chart at all");

    // 4. The published list is now the GUI's view of what this node HAS — taken off the series,
    //    never off the scalars.
    assert_eq!(g.published, vec![series_follow::PublishedSeries::new("bybit", "BTCUSDT", "1m")]);
    assert_eq!(
        series_follow::chart_feed(false, &g.published, "BTCUSDT@1m"),
        series_follow::ChartFeed::Elsewhere,
        "the badge may not say LIVE over an empty grid"
    );

    // 5. THE FIX: the chart nobody chose follows the node.
    g.follow_backend();
    assert_eq!(g.wins[0].key(), "bybit:BTCUSDT@1m");
    assert_eq!(g.wins[0].title, "BYBIT BTCUSDT · 1m");
    assert!(g.spawned.contains("bybit:BTCUSDT@1m"), "adoption must SUBSCRIBE, not just retitle");

    // 6. ...and the very next frame paints it. No second publish from the daemon: the bars were
    //    already in the snapshot, which is what `FORCE_REFOLD` exists for.
    g.fold(&snap);
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 3, "THE DEFECT: the published series must render");
    assert_eq!(
        series_follow::chart_feed(true, &g.published, "bybit:BTCUSDT@1m"),
        series_follow::ChartFeed::Live,
        "and NOW the badge has earned the word"
    );

    // 7. Steady state: a later publish appends to the adopted chart, and the window does not move
    //    again (it has bars, so it is no longer adoptable at all).
    let next = observe_bridge::wire_to_core(&bybit_node(2, &[1_000, 61_000, 121_000, 181_000]));
    g.fold(&next);
    g.follow_backend();
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 4);
    assert_eq!(g.wins[0].key(), "bybit:BTCUSDT@1m");
}

/// A node publishing NOTHING invents nothing: the chart stays where it was, holds no bars, and the
/// badge says so instead of claiming a live feed.
#[test]
fn a_node_with_no_series_leaves_the_chart_alone_and_the_badge_honest() {
    let mut g = fresh_gui();
    let mut w = vike_tradehub_client::WireSnapshot::empty();
    w.seq = 7; // a real frame from a connected node that simply has no bars
    let snap = observe_bridge::wire_to_core(&w);
    g.fold(&snap);
    g.follow_backend();

    assert!(g.published.is_empty());
    assert_eq!(g.wins[0].key(), "BTCUSDT@1m", "nothing to adopt ⇒ nothing is invented");
    assert_eq!(g.bars_on("BTCUSDT@1m"), 0);
    assert_eq!(
        series_follow::chart_feed(false, &g.published, "BTCUSDT@1m"),
        series_follow::ChartFeed::Silent
    );
    assert_eq!(series_follow::ChartFeed::Silent.badge(), "○ NO DATA");
}

/// A chart the OPERATOR pointed somewhere is never yanked — the picker's backend section is how
/// that window reaches a published series, by an act.
#[test]
fn an_operator_pinned_chart_is_left_where_they_put_it() {
    let mut g = fresh_gui();
    // The symbol picker's apply, in miniature: retarget + PIN (see `draw_windows`' `new_symbol`).
    g.wins[0].venue = "okx".into();
    g.wins[0].symbol = "ETH-USDT".into();
    g.wins[0].retitle();
    g.wins[0].series_pinned = true;
    g.ensure(&startup::FeedSpec {
        venue: "okx".into(),
        symbol: "ETH-USDT".into(),
        interval: "1m".into(),
        asset_class: None,
    });

    g.fold(&observe_bridge::wire_to_core(&bybit_node(1, &[1_000])));
    g.follow_backend();

    assert_eq!(g.wins[0].key(), "okx:ETH-USDT@1m", "a chosen chart may not move behind their back");
    // ...and the node's series is REACHABLE: it is in the list the picker renders, labelled.
    assert_eq!(g.published[0].label(), "BYBIT:BTCUSDT · 1m");
    assert_eq!(g.published[0].key(), "bybit:BTCUSDT@1m");
}

/// A RESTORED workspace is a choice too — `persist::apply` pins every window it rebuilds, so a
/// saved layout survives a reconnect to a node mounting something else.
#[test]
fn a_restored_workspace_window_is_pinned() {
    let g = fresh_gui();
    let ws = workspace::persist::capture(&g.wins, DisplayTz::Utc, 2.0, false);
    let restored = workspace::persist::apply(&ws);
    assert_eq!(restored.len(), 1);
    assert!(restored[0].series_pinned);
}
