//! **The desktop chart renders the series its backend actually publishes** — the whole loop, in
//! one headless test, over the REAL functions the shell calls.
//!
//! # What this reproduces
//!
//! MEASURED against a live daemon: a `vike-tradehub` built from `main`, mounting bybit `BTCUSDT`
//! `1m`, was stood up on a spare port and a thin client attached to it. The node published
//! `bybit:BTCUSDT@1m`. The GUI was subscribed to `BTCUSDT@1m` — [`startup::plan`]'s default chart,
//! keyed through [`chart_key::series_key`](vike_app_core::data::chart_key::series_key) in Binance's then-bare shape. Every frame,
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
//! `crates/vike-app-core/src/ui/series_follow.rs`'s own unit tests already state the RULE over
//! `WindowState`s. They cannot state the DEFECT, because the defect is a disagreement between four
//! separate mechanisms — the startup layout, `ensure_feed_on`'s `spawned` set, the wire bridge, and
//! the snapshot fold's key filter — and a test that mocks any one of them would pass with the bug
//! in place. So every step below is the shipped function: [`startup::plan`] or
//! [`window::spawn::plan_window_spawn`] builds the window (the startup chart is EMPTY since the owner's
//! ruling of 2026-11-13, so the measured binance start is now a DataSet default's window),
//! [`feed_lifecycle::ensure_feed_on`] fills `spawned` exactly as the shell's `ensure_feed_on`
//! wrapper does, [`observe_decode::wire_to_core`] turns the daemon's wire frame into the snapshot,
//! and [`core_sync::sync_from_core`] folds it. The only thing written by hand is the wire frame,
//! which is the daemon's side of the seam.
//!
//! ⚠ **Step 3 asserts the OLD behaviour and must keep passing.** It is what makes step 6 mean
//! something: without it, a fix that accidentally made every series render would look identical to
//! a fix that made the right one render.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use vike_app_core::backend::split_plane::{BackendBarPlane, BackendMode, bar_plane};
use vike_app_core::data::data_sink::{StoredBarStore, TickStore};
use vike_app_core::{
    data::stored, ui::core_sync, ui::feed_lifecycle, ui::series_follow, ui::startup, ui::window,
};
use vike_chart::chart::state;
use vike_tradehub_client::observe_decode;
use vike_tradehub_client::wire::{BarSeriesWire, BarWire};
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

/// The SHIPPED desktop's render-source plane, taken from the arm table rather than written down —
/// `crates/vike-desktop/src/app_state.rs`'s `BAR_PLANE` is the same call on the same arm.
const BAR_PLANE: BackendBarPlane = bar_plane(BackendMode::ObserveOnly);

/// The daemon's frame: a node whose PRIMARY ENGINE reads `binance` / `BTCUSDT` (which is what
/// the CI box reports, and is NOT what it mounts) publishing one bybit series.
///
/// The two scalars are set to the misleading values ON PURPOSE — any fix that keys off
/// `CoreSnapshotWire::venue` / `::symbol` instead of off the series list passes on a node where they
/// happen to agree and fails on the one that was measured.
fn bybit_node(seq: u64, closed: &[i64]) -> vike_tradehub_client::wire::CoreSnapshotWire {
    let mut w = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    w.sequence = seq;
    w.venue = "binance".into();
    w.symbol = "BTCUSDT".into();
    w.bars = vec![BarSeriesWire {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        closed: closed
            .iter()
            .map(|&timestamp| BarWire { timestamp, o: 100.0, h: 101.0, l: 99.0, c: 100.5, v: 3.0 })
            .collect(),
        forming: None,
    }];
    w
}

/// Everything the shell owns that this loop touches — the same fields, under the same names.
#[derive(Default)]
struct Gui {
    feeds: vike_app_core::data::market_data_session::DataClientMap,
    subscriptions: HashMap<String, vike_data::DataClientSubscriptionId>,
    spawned: HashSet<String>,
    unroutable: HashSet<String>,
    retries: vike_app_core::data::datahub::feed_retry::DatahubFeedRetries,
    charts: HashMap<String, state::ChartState>,
    hidden: HashSet<String>,
    tick_volume_aggregators: HashMap<String, (String, String, vike_orderflow::tickvol::TickVolAgg)>,
    orderflow_aggregators: HashMap<String, (String, String, vike_orderflow::bar_agg::OrderflowAgg)>,
    published: Vec<series_follow::PublishedSeries>,
    last_sequence: u64,
    last_direct_gen: u64,
    status: vike_model::feed_status::DataClientStatus,
    wins: Vec<window::WindowState>,
    /// `App::direct_bars` — the GUI-side bar store the desktop now mounts, filled here by the same
    /// `stored` read the shell performs off-thread.
    direct_bars: StoredBarStore,
    /// `App::chart_history` — the session's ledger `plan_store_reads` consults and records into.
    history: stored::ChartHistory,
}

impl Gui {
    /// `App::ensure_feed_on`, verbatim in shape: the shell's wrapper is a call to this with its own
    /// fields. `--observe` has no venue clients, so `feeds` stays EMPTY here exactly as it is
    /// there — which is the configuration in which `spawned` is the only thing that decides what
    /// renders.
    fn ensure(&mut self, spec: &startup::SeriesSpec) {
        feed_lifecycle::ensure_feed_on(
            &mut feed_lifecycle::FeedSlots {
                feeds: &mut self.feeds,
                subscriptions: &mut self.subscriptions,
                spawned: &mut self.spawned,
                unroutable: &mut self.unroutable,
                retries: &mut self.retries,
            },
            &mut feed_lifecycle::SeriesSlots {
                charts: &mut self.charts,
                hidden: &mut self.hidden,
                tick_volume_aggregators: &mut self.tick_volume_aggregators,
                orderflow_aggregators: &mut self.orderflow_aggregators,
            },
            spec,
            ThemeDisplayTimeZone::Utc,
            false,
        );
    }

    /// `App::sync_from_core`, verbatim in shape — INCLUDING the desktop's own bar plane and bar
    /// store, so a series the node does not publish takes the same route here as it does there.
    fn fold(&mut self, snapshot: &vike_execution::CoreSnapshot) {
        let trades = TickStore::default();
        let feed_status = Mutex::default();
        let no_gaps = |_: &str, _: &str| 0u64;
        core_sync::sync_from_core(
            core_sync::CoreSyncInputs {
                snapshot,
                spawned: &self.spawned,
                hidden: &self.hidden,
                display_time_zone: ThemeDisplayTimeZone::Utc,
                trades: &trades,
                feed_status: &feed_status,
                direct_bars: Some(&self.direct_bars),
                bar_plane: BAR_PLANE,
                tape_gaps: &no_gaps,
            },
            core_sync::CoreSyncState {
                charts: &mut self.charts,
                tick_volume_aggregators: &mut self.tick_volume_aggregators,
                orderflow_aggregators: &mut self.orderflow_aggregators,
                published: &mut self.published,
                last_sequence: &mut self.last_sequence,
                last_direct_gen: &mut self.last_direct_gen,
                status: &mut self.status,
            },
        );
    }

    /// The frame-loop block this change adds to `crates/vike-desktop/src/app_ui/mod.rs`'s
    /// `draw_windows`: plan, apply, ensure, and force ONE refold so the bars already sitting in the
    /// snapshot paint on the next frame instead of waiting on the node's publish cadence.
    fn follow_backend(&mut self) {
        let adopted = series_follow::follow_backend(&mut self.wins, &self.charts, &self.published);
        if !adopted.is_empty() {
            self.last_sequence = core_sync::FORCE_REFOLD;
        }
        for spec in &adopted {
            self.ensure(spec);
        }
    }

    fn bars_on(&self, key: &str) -> usize {
        self.charts.get(key).map(|c| c.bars.len()).unwrap_or(0)
    }

    /// The store-read block `crates/vike-desktop/src/app_ui/mod.rs`'s `draw_windows` runs right after
    /// the ensure fan-out, with the thread collapsed into a synchronous call so the test has no
    /// race to lose: plan, record every key as asked BEFORE the read, then perform it.
    ///
    /// Returns the plan, so a test can state what was (and was not) asked for.
    fn ask_the_store(
        &mut self,
        store: &dyn vike_data::HistoryStore,
    ) -> Vec<stored::StoredBarRequest> {
        let plan = stored::plan_store_reads(
            &series_follow::chart_targets(&self.wins, &self.charts),
            &self.published,
            &mut self.history,
            NOW_MS,
        );
        let cache = self.history.cache();
        stored::read_into_store(&plan, store, &self.direct_bars, &cache, NOW_MS, HISTORY_BARS);
        plan
    }
}

/// A fixed "now" for the store reads — the harness pins it so `read_range`'s window is a function
/// of the test rather than of when it ran.
const NOW_MS: i64 = 1_700_000_000_000;

/// How deep the harness's store reads go: `preferences.chart_history_bars`' default, the depth a GUI
/// with no row asks for.
const HISTORY_BARS: i64 = vike_config::preferences::DEFAULT_CHART_HISTORY_BARS as i64;

/// A GUI started exactly as the shell starts one with no saved workspace: [`startup::plan`]'s
/// default layout, every `SeriesSpec` it asks for ensured. Since the owner's ruling of 2026-11-13 its
/// one chart is EMPTY (no symbol is compiled in), so it asks for nothing.
fn fresh_gui() -> Gui {
    let mut g = Gui::default();
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
    let layout = startup::plan(area, &startup::StartupEnvironment::default(), Ok(None));
    for spec in &layout.ensure_feeds {
        g.ensure(spec);
    }
    g.wins = layout.wins;
    g
}

/// A GUI whose one chart was opened on a default series NOBODY on this node publishes — the user's
/// DataSet naming binance `BTCUSDT` (`window::spawn::default_series`) — through the shipped planner,
/// its feed ensured. Until 2026-11-13 the startup chart itself was this window, on a compiled binance
/// `BTCUSDT`; since 2026-11-16 a window opened on a series is a CHOICE and never adopts.
fn gui_on_a_symbol_set_default() -> Gui {
    let mut g = Gui::default();
    let series = startup::SeriesSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        asset_class: None,
    };
    let plan = window::spawn::plan_window_spawn(
        window::spawn::WindowSpawnRequest::NewWindowChart,
        egui::pos2(0.0, 0.0),
        0,
        egui::vec2(0.0, 0.0),
        Some(&series),
    );
    if let Some(spec) = &plan.ensure_feed {
        g.ensure(spec);
    }
    g.wins = plan.window.into_iter().collect();
    g
}

/// The startup chart names nothing and asks for nothing, says so on its badge, and takes up the
/// series the node publishes — the same adoption as below, from an empty start.
#[test]
fn a_bybit_node_paints_the_empty_startup_chart() {
    let mut g = fresh_gui();
    assert_eq!(g.wins.len(), 1);
    assert_eq!((g.wins[0].venue.as_str(), g.wins[0].symbol.as_str()), ("", ""));
    assert!(g.spawned.is_empty(), "an empty chart subscribes nothing");
    assert!(!g.wins[0].series_pinned);
    let snapshot = observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
    g.fold(&snapshot);
    assert_eq!(
        series_follow::chart_feed(false, &g.published, &g.wins[0].symbol, &g.wins[0].key()),
        series_follow::ChartSeriesFollowBadge::NoSymbol
    );
    g.follow_backend();
    assert_eq!(g.wins[0].key(), "bybit:BTCUSDT@1m");
    assert!(g.spawned.contains("bybit:BTCUSDT@1m"));
    g.fold(&snapshot);
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 3);
}

/// The measured defect from the other side: a chart OPENED on a series the node does not publish is
/// the user's choice. It is never adopted away; the node's series is one pick away in the picker.
#[test]
fn a_chart_opened_on_a_series_the_node_does_not_publish_stays_where_it_was_opened() {
    let mut g = gui_on_a_symbol_set_default();

    // 1. The chart starts on a default series the user's own DataSet named.
    assert_eq!(g.wins.len(), 1);
    assert_eq!(g.wins[0].key(), "binance:BTCUSDT@1m");
    assert!(g.wins[0].series_pinned, "a window opened on a series is a choice");
    assert!(g.spawned.contains("binance:BTCUSDT@1m"));
    assert!(!g.spawned.contains("bybit:BTCUSDT@1m"));

    // 2. The daemon's first frame, through the REAL wire bridge.
    let snapshot = observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
    assert_eq!(
        snapshot.venue, "binance",
        "the primary-engine scalars lie; nothing may key off them"
    );
    g.fold(&snapshot);

    // 3. THE MEASURED FAILURE, still exactly as measured: the key the node publishes is not the key
    //    the GUI subscribed, so the fold drops it. This assertion must SURVIVE the fix — it is what
    //    proves step 6 renders the right series rather than every series.
    assert_eq!(
        g.bars_on("binance:BTCUSDT@1m"),
        0,
        "the binance-keyed chart has, and gets, nothing"
    );
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 0, "...and the published key has no chart at all");

    // 4. The published list is now the GUI's view of what this node HAS — taken off the series,
    //    never off the scalars.
    assert_eq!(g.published, vec![series_follow::PublishedSeries::new("bybit", "BTCUSDT", "1m")]);
    assert_eq!(
        series_follow::chart_feed(false, &g.published, "BTCUSDT", "binance:BTCUSDT@1m"),
        series_follow::ChartSeriesFollowBadge::Elsewhere,
        "the badge may not say LIVE over an empty grid"
    );

    // 5. The chosen chart is not moved, frame after frame; the node's series stays reachable.
    for _frame in 0..10 {
        g.follow_backend();
        g.fold(&snapshot);
    }
    assert_eq!(
        g.wins[0].key(),
        "binance:BTCUSDT@1m",
        "a chosen chart may not move behind their back"
    );
    assert!(!g.spawned.contains("bybit:BTCUSDT@1m"));
    assert_eq!(g.published[0].label(), "BYBIT:BTCUSDT · 1m");
}

/// **The owner's console, 2026-11-16: a PICKED series is never adopted away while its history
/// loads.** Each Data Manager "Open in chart" logged "chart adopted a series this node publishes
/// (nothing chose the old one; it was empty)" and the chart jumped to the node's `bybit:BTCUSDT@1m`.
/// Every opener that names a series is driven here, the venue-less palette pick (no default, so
/// `:ETHUSDT.P@1m`) included, over many frames with their history still absent: no window moves and
/// `series_follow` says nothing.
#[test]
fn a_picked_series_whose_history_is_loading_is_never_adopted_away() {
    let snapshot = observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
    let fxcm = startup::SeriesSpec {
        venue: "fxcm".into(),
        symbol: "GBPUSD".into(),
        interval: "1d".into(),
        asset_class: None,
    };
    let picks = [
        window::spawn::WindowSpawnRequest::StoredOpen {
            venue: "dukascopy".into(),
            symbol: "USDCHF".into(),
            interval: Some("1m".into()),
        },
        window::spawn::WindowSpawnRequest::StoredOpen {
            venue: "binance".into(),
            symbol: "SOLUSDT.P".into(),
            interval: Some("1m".into()),
        },
        window::spawn::WindowSpawnRequest::TestSymbol { series: fxcm },
        window::spawn::WindowSpawnRequest::Palette { raw: "ethusdt.p".into() },
        window::spawn::WindowSpawnRequest::CloneWindow {
            venue: "oanda".into(),
            symbol: "GBP_USD".into(),
            interval: "5s".into(),
        },
    ];
    let (keys, events) = vike_log::capture::captured(|| {
        let mut g = Gui::default();
        for (n, req) in (0u32..).zip(picks) {
            let plan = window::spawn::plan_window_spawn(
                req,
                egui::pos2(0.0, 0.0),
                n,
                egui::vec2(0.0, 0.0),
                None,
            );
            if let Some(spec) = &plan.ensure_feed {
                g.ensure(spec);
            }
            g.wins.extend(plan.window);
        }
        let opened: Vec<String> = g.wins.iter().map(window::WindowState::key).collect();
        for _frame in 0..120 {
            g.fold(&snapshot);
            g.follow_backend();
            let now: Vec<String> = g.wins.iter().map(window::WindowState::key).collect();
            assert_eq!(now, opened, "a picked series was adopted away");
        }
        opened
    });
    assert!(keys.contains(&":ETHUSDT.P@1m".to_string()), "the venue-less pick is driven: {keys:?}");
    let said: Vec<_> = events.iter().filter(|e| e.target.ends_with("series_follow")).collect();
    assert!(said.is_empty(), "series_follow logged on a pick: {said:?}");
}

/// The startup chart adopts ONCE, and says so in ONE line at DEBUG, never an INFO per frame.
#[test]
fn the_empty_startup_chart_adopts_once_with_one_debug_line() {
    let mut g = fresh_gui();
    let snapshot = observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
    let ((), events) = vike_log::capture::captured(|| {
        for _frame in 0..60 {
            g.fold(&snapshot);
            g.follow_backend();
        }
    });
    assert_eq!(g.wins[0].key(), "bybit:BTCUSDT@1m");
    let said: Vec<_> = events.iter().filter(|e| e.target.ends_with("series_follow")).collect();
    assert_eq!(said.len(), 1, "one adoption, one line: {said:?}");
    assert_eq!(said[0].level, tracing::Level::DEBUG);
}

/// A node publishing NOTHING invents nothing: the chart stays where it was, holds no bars, and the
/// badge says so instead of claiming a live feed.
#[test]
fn a_node_with_no_series_leaves_the_chart_alone_and_the_badge_honest() {
    let mut g = gui_on_a_symbol_set_default();
    let mut w = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    w.sequence = 7; // a real frame from a connected node that simply has no bars
    let snapshot = observe_decode::wire_to_core(&w);
    g.fold(&snapshot);
    g.follow_backend();

    assert!(g.published.is_empty());
    assert_eq!(g.wins[0].key(), "binance:BTCUSDT@1m", "nothing to adopt ⇒ nothing is invented");
    assert_eq!(g.bars_on("binance:BTCUSDT@1m"), 0);
    assert_eq!(
        series_follow::chart_feed(false, &g.published, "BTCUSDT", "binance:BTCUSDT@1m"),
        series_follow::ChartSeriesFollowBadge::Silent
    );
    assert_eq!(series_follow::ChartSeriesFollowBadge::Silent.badge(), "○ NO DATA");
}

/// A chart the OPERATOR pointed somewhere is never yanked — the picker's backend section is how
/// that window reaches a published series, by an act.
#[test]
fn an_operator_pinned_chart_is_left_where_they_put_it() {
    let mut g = gui_on_a_symbol_set_default();
    // The symbol picker's apply, in miniature: retarget + PIN (see `draw_windows`' `new_symbol`).
    g.wins[0].venue = "okx".into();
    g.wins[0].symbol = "ETH-USDT".into();
    g.wins[0].retitle();
    g.wins[0].series_pinned = true;
    g.ensure(&startup::SeriesSpec {
        venue: "okx".into(),
        symbol: "ETH-USDT".into(),
        interval: "1m".into(),
        asset_class: None,
    });

    g.fold(&observe_decode::wire_to_core(&bybit_node(1, &[1_000])));
    g.follow_backend();

    assert_eq!(g.wins[0].key(), "okx:ETH-USDT@1m", "a chosen chart may not move behind their back");
    // ...and the node's series is REACHABLE: it is in the list the picker renders, labelled.
    assert_eq!(g.published[0].label(), "BYBIT:BTCUSDT · 1m");
    assert_eq!(g.published[0].key(), "bybit:BTCUSDT@1m");
}

/// A RESTORED workspace is a choice too — `workspace::apply` pins every window it rebuilds, so a
/// saved layout survives a reconnect to a node mounting something else.
#[test]
fn a_restored_workspace_window_is_pinned() {
    let g = gui_on_a_symbol_set_default();
    let ws = window::workspace::capture(&g.wins);
    let restored = window::workspace::apply(&ws);
    assert_eq!(restored.len(), 1);
    assert!(restored[0].series_pinned);
    assert!(restored[0].interval_pinned, "a saved layout chose its resolution too");
}

/// THE OWNER'S CASE, END TO END: *"5m/15m/1h/1s charts can never paint"*.
///
/// One GUI, one daemon publishing only `bybit BTCUSDT 1m`, one backend store holding `5m` rows,
/// and the whole chain in the order the shell runs it — interval menu, adoption, ensure, store
/// read, fold. Every step is the shipped function.
#[test]
fn a_five_minute_chart_paints_from_the_backends_store() {
    // The EMPTY startup chart: a chart opened on a series is a choice and adopts nothing.
    let mut g = fresh_gui();
    let snapshot = observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
    g.fold(&snapshot);

    // 1. THE INTERVAL MENU (`draw_windows`' `act.new_interval` arm, in miniature): the resolution
    //    is pinned, the SERIES is not. That split is the whole of change A.
    g.wins[0].interval = "5m".into();
    g.wins[0].interval_pinned = true;
    g.wins[0].retitle();

    // 2. Adoption still runs, and carries the operator's resolution across: the window lands on
    //    the venue this node actually mounts, at 5m. ⚠ Before the split this window was
    //    `series_pinned` and never moved at all; without any pin it would have been yanked back
    //    to the daemon's 1m.
    g.follow_backend();
    assert_eq!(g.wins[0].key(), "bybit:BTCUSDT@5m", "venue adopted, 5m preserved");
    assert!(g.spawned.contains("bybit:BTCUSDT@5m"), "and it is subscribed");

    // 3. Nothing paints yet, and that is the DEFECT: the daemon does not stream 5m, so the fold
    //    has nothing for this key. This assertion must survive — it is what makes step 5 mean
    //    "the store answered" rather than "something happened to be there".
    g.fold(&snapshot);
    assert_eq!(g.bars_on("bybit:BTCUSDT@5m"), 0, "the daemon publishes no 5m — nothing to fold");

    // 4. THE FIX: the chart asks the BACKEND'S store, which holds 5m rows for that symbol.
    let store = vike_data::HistoryStoreMemory::default();
    let rows: Vec<vike_marketdata::Bar> =
        (0..6).map(|i| store_bar(NOW_MS - 300_000 * (6 - i))).collect();
    vike_data::HistoryStore::append_bars(&store, "bybit", "BTCUSDT", "5m", &rows, None)
        .expect("seed the backend store");
    let plan = g.ask_the_store(&store);
    assert_eq!(plan.len(), 1, "exactly one series asked for");
    assert_eq!(plan[0].key(), "bybit:BTCUSDT@5m");

    // 5. ...and the next frame paints it.
    g.fold(&snapshot);
    assert_eq!(g.bars_on("bybit:BTCUSDT@5m"), 6, "THE DEFECT: a 5m chart paints");

    // 6. THE REGRESSION GUARD: the daemon-published 1m series still folds from the LIVE snapshot,
    //    with nothing in the store for it. Open a second window on it and it paints as before.
    g.wins.push(window::WindowState::new(
        "chart-1",
        "BTCUSDT",
        "1m",
        window::WindowKind::Chart,
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0)),
    ));
    g.wins[1].venue = "bybit".into();
    g.wins[1].series_pinned = true; // the operator picked it; adoption must not touch it
    g.ensure(&startup::SeriesSpec {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        asset_class: None,
    });
    let asked = g.ask_the_store(&store);
    assert!(
        asked.is_empty(),
        "a series the backend PUBLISHES is not read from the store unless panned left of its tail"
    );
    g.fold(&observe_decode::wire_to_core(&bybit_node(2, &[1_000, 61_000, 121_000, 181_000])));
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 4, "the daemon-published series still paints");
    assert_eq!(g.bars_on("bybit:BTCUSDT@5m"), 6, "…and the store-read one is not disturbed");
}

/// **ASKED ONCE, NOT ONCE PER FRAME** — over the real loop. The store holds NOTHING for the key,
/// which is the worst case for this rule: the chart stays empty forever, so every frame re-passes
/// rules 1–3 and only the `asked` ledger stops it.
#[test]
fn a_chart_with_no_bars_asks_the_store_once_not_once_per_frame() {
    let mut g = gui_on_a_symbol_set_default();
    g.wins[0].interval = "1h".into();
    g.wins[0].interval_pinned = true;
    g.ensure(&startup::SeriesSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        asset_class: None,
    });

    let empty = vike_data::HistoryStoreMemory::default();
    assert_eq!(g.ask_the_store(&empty).len(), 1, "the first frame asks");
    for _frame in 0..10 {
        assert!(g.ask_the_store(&empty).is_empty(), "…and no later frame asks again");
    }
    assert_eq!(
        g.bars_on("binance:BTCUSDT@1h"),
        0,
        "an empty store leaves an empty chart, honestly"
    );
}

/// **THE OWNER'S CHART, 2026-11-19: panning a published chart left loads its older bars.** The
/// startup chart adopts what the node publishes (`bybit:BTCUSDT.P@1m`, its newest closes only), the
/// operator pans left of that tail, and the store is asked for the minutes in between: they land
/// UNDER the tail, which keeps coming. Before the fix the plan was empty and the chart held the
/// tail alone, its left side blank.
#[test]
fn panning_an_adopted_published_chart_left_loads_its_older_bars() {
    const MINUTE: i64 = 60_000;
    const KEY: &str = "bybit:BTCUSDT.P@1m";
    let newest = NOW_MS - NOW_MS % MINUTE - MINUTE;
    let node = |seq: u64, last: i64| {
        let mut w = bybit_node(seq, &[last - 2 * MINUTE, last - MINUTE, last]);
        w.bars[0].symbol = "BTCUSDT.P".into();
        observe_decode::wire_to_core(&w)
    };
    let opens = |g: &Gui| -> Vec<i64> { g.charts[KEY].bars.iter().map(|b| b.ot).collect() };
    let mut g = fresh_gui();
    let snapshot = node(1, newest);
    g.fold(&snapshot);
    g.follow_backend();
    g.fold(&snapshot);
    assert_eq!((g.wins[0].key().as_str(), g.bars_on(KEY)), (KEY, 3), "adopted: the node's tail");

    let first = newest - 2 * MINUTE;
    let store = vike_data::HistoryStoreMemory::default();
    let hour: Vec<_> = (1..=60).rev().map(|i| store_bar(first - i * MINUTE)).collect();
    vike_data::HistoryStore::append_bars(&store, "bybit", "BTCUSDT.P", "1m", &hour, None)
        .expect("seed the backend store");
    let id = g.wins[0].id;
    let view = |left: i64| vike_chart::chart::ChartViewSpan {
        timestamps: Some((left, newest + MINUTE)),
        width_px: 800.0,
    };

    // A view of the tail itself asks nothing.
    g.history.observe_view(id, Some(view(first - MINUTE)), NOW_MS - 1_000);
    assert!(g.ask_the_store(&store).is_empty(), "the tail's own view asks nothing");

    // THE DEFECT: panned 30 minutes left of the tail, the store is asked for those minutes...
    g.history.observe_view(id, Some(view(first - 30 * MINUTE)), NOW_MS - 1_000);
    let plan = g.ask_the_store(&store);
    assert_eq!(plan.len(), 1, "a published chart panned left asked the store for nothing");
    assert_eq!(plan[0].key(), KEY);
    // ...and the next frame paints them under the tail, with no new snapshot needed.
    g.fold(&snapshot);
    let expected: Vec<i64> = (0..33).map(|i| first - 30 * MINUTE + i * MINUTE).collect();
    assert_eq!(opens(&g), expected, "30 older minutes under the 3 of the tail");
    assert!(g.ask_the_store(&store).is_empty(), "the view now holds them: asked once");

    // The tail keeps coming and slides on without a hole: the bar it dropped stays.
    g.fold(&node(2, newest + MINUTE));
    assert_eq!(opens(&g).len(), 34);
    assert!(opens(&g).windows(2).all(|w| w[1] - w[0] == MINUTE), "one minute apart");

    // A tail that slid past the history while nothing folded is drawn alone, never across the hole.
    g.fold(&node(3, newest + 10 * MINUTE));
    assert_eq!(opens(&g).len(), 3, "the hole is not drawn over");
}

/// A bar as the backend's store answers with one.
fn store_bar(timestamp: i64) -> vike_marketdata::Bar {
    vike_marketdata::Bar::ohlcv(timestamp, 100.0, 101.0, 99.0, 100.5, 2.0)
}

/// The datahub's per-venue feed as the desktop mounts it once the handshake lands: it serves the
/// book and tape lanes and REFUSES the bar lane (`vike_app_core::data::market_data_session::data_client`'s
/// `subscribe_bars`), and it records every lane it is asked for.
struct DatahubShapedFeed(std::sync::Arc<Mutex<Vec<String>>>);

impl vike_data::DataClient for DatahubShapedFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
        self.0.lock().unwrap().push(format!("bars {symbol} {interval}"));
        Err(vike_data::DataClientError::Unsupported("the datahub wire serves no live bar lane"))
    }
    fn subscribe_quotes(
        &mut self,
        symbol: &str,
    ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
        self.0.lock().unwrap().push(format!("quotes {symbol}"));
        Ok(vike_data::DataClientSubscriptionId(1))
    }
    fn subscribe_trades(
        &mut self,
        symbol: &str,
    ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
        self.0.lock().unwrap().push(format!("trades {symbol}"));
        Ok(vike_data::DataClientSubscriptionId(2))
    }
    fn subscribe_book(
        &mut self,
        symbol: &str,
    ) -> Result<vike_data::DataClientSubscriptionId, vike_data::DataClientError> {
        self.0.lock().unwrap().push(format!("book {symbol}"));
        Ok(vike_data::DataClientSubscriptionId(3))
    }
    fn unsubscribe(&mut self, _id: vike_data::DataClientSubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// **The owner's console, 2026-11-16: no bar lane is asked, and the chart has its bars.** A bybit
/// `1m` chart opened before the datahub's handshake (no feed mounted) and again after it (the feed
/// mounted, refusing bars as the real one does) asks NO feed for a bar lane: the two WARNs it used to
/// log ("no market-data feed mounted … nothing in this process will produce them", "subscribe_bars
/// refused … this series stays empty") had nothing true to say, because the chart paints from the
/// node's snapshot — and a series the node does not publish paints from its store
/// (`a_five_minute_chart_paints_from_the_backends_store`).
#[test]
fn a_chart_asks_no_bar_lane_and_paints_from_the_snapshot() {
    let mut g = Gui::default();
    let spec = startup::SeriesSpec {
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        asset_class: None,
    };
    g.ensure(&spec); // before the handshake: no feed mounted
    let asked = std::sync::Arc::new(Mutex::new(Vec::new()));
    g.feeds.insert("bybit", Box::new(DatahubShapedFeed(asked.clone())));
    g.ensure(&spec); // after it: the feed is there
    feed_lifecycle::retry_unroutable(&mut feed_lifecycle::FeedSlots {
        feeds: &mut g.feeds,
        subscriptions: &mut g.subscriptions,
        spawned: &mut g.spawned,
        unroutable: &mut g.unroutable,
        retries: &mut g.retries,
    });
    assert!(asked.lock().unwrap().is_empty(), "a lane was asked: {:?}", asked.lock().unwrap());
    assert!(g.unroutable.is_empty() && g.retries.is_empty(), "nothing to warn about");

    g.fold(&observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000])));
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 3, "the chart paints from the node's snapshot");
    g.fold(&observe_decode::wire_to_core(&bybit_node(2, &[1_000, 61_000, 121_000, 181_000])));
    assert_eq!(g.bars_on("bybit:BTCUSDT@1m"), 4, "and its live tail keeps coming");
}

/// **The adoption path asks no bar lane either** — the path the owner's start takes: the empty startup
/// chart adopts the series the node publishes (`series_follow::follow_backend`, "an empty chart nobody
/// chose adopted a series this node publishes"), and the shell ensures it through the same `feed_lifecycle::ensure_feed_on`.
/// Before the datahub's handshake (no feed) and after it (a datahub-shaped feed refusing bars), the
/// adopted chart asks no feed for a bar lane, records nothing to warn about, and paints from the snapshot.
#[test]
fn an_adopted_chart_asks_no_bar_lane_and_paints_from_the_snapshot() {
    for mounted in [false, true] {
        let mut g = fresh_gui();
        let asked = std::sync::Arc::new(Mutex::new(Vec::new()));
        if mounted {
            g.feeds.insert("bybit", Box::new(DatahubShapedFeed(asked.clone())));
        }
        let snapshot = observe_decode::wire_to_core(&bybit_node(1, &[1_000, 61_000, 121_000]));
        g.fold(&snapshot);
        g.follow_backend();
        assert_eq!(g.wins[0].key(), "bybit:BTCUSDT@1m", "mounted={mounted}: adopted");
        assert!(asked.lock().unwrap().is_empty(), "mounted={mounted}: {:?}", asked.lock().unwrap());
        assert!(
            g.unroutable.is_empty() && g.retries.is_empty(),
            "mounted={mounted}: nothing to warn"
        );
        g.fold(&snapshot);
        assert_eq!(
            g.bars_on("bybit:BTCUSDT@1m"),
            3,
            "mounted={mounted}: painted from the snapshot"
        );
    }
}
