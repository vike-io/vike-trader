use super::*;
use crate::ui::workspace::DEFAULT_VENUE;
use vike_chart::model;

fn win(id: &str, venue: &str, symbol: &str, interval: &str) -> WinState {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
    let mut w = WinState::new(id, symbol, interval, WinKind::Chart, r);
    w.venue = venue.to_string();
    w.retitle();
    w
}

fn charted(keys: &[&str]) -> HashMap<String, model::ChartState> {
    keys.iter()
        .map(|k| {
            let mut cs = model::ChartState::default();
            cs.bars.push(model::Bar { t: 0.0, ot: 0, o: 1.0, h: 1.0, l: 1.0, c: 1.0, v: 0.0 });
            (k.to_string(), cs)
        })
        .collect()
}

/// THE MEASURED CASE, end to end over the real `WinState`: the node publishes
/// `bybit:BTCUSDT@1m`, the startup chart is the default `binance` `BTCUSDT` `1m`, and the
/// chart moves onto the published series — title and ensure-spec included.
#[test]
fn the_default_chart_adopts_the_series_the_node_publishes() {
    let mut wins = vec![win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m")];
    let pub_ = vec![PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    let specs = follow_backend(&mut wins, &HashMap::new(), &pub_);
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@1m");
    assert_eq!(wins[0].title, "BYBIT BTCUSDT · 1m");
    assert_eq!(specs.len(), 1, "the adopted series must be ensure-subscribed");
    assert_eq!((specs[0].venue.as_str(), specs[0].interval.as_str()), ("bybit", "1m"));
    assert!(specs[0].asset_class.is_none(), "a wire series carries no local asset class");
}

/// A chart the OPERATOR chose is never yanked, however wrong its venue is for this node.
#[test]
fn a_pinned_chart_is_never_retargeted() {
    let mut wins = vec![win("chart-0", "okx", "ETH-USDT", "5m")];
    wins[0].series_pinned = true;
    let pub_ = vec![PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    assert!(follow_backend(&mut wins, &HashMap::new(), &pub_).is_empty());
    assert_eq!(wins[0].key(), "okx:ETH-USDT@5m");
}

/// ⚠ **THE INTERVAL PIN, both halves at once.** A window the operator switched to `5m`
/// (`interval_pinned`, `series_pinned` still false — the split) against a node publishing only
/// `bybit BTCUSDT 1m`: it MOVES onto bybit, and it KEEPS `5m`.
///
/// The old behaviour is the mutation this pins against: restore the interval menu's
/// unconditional `series_pinned = true` and the first assertion fails (the window never
/// moves); drop the flag entirely instead and the second fails (the window snaps back to
/// `1m`). Neither wrong answer can pass both.
#[test]
fn an_interval_pinned_chart_keeps_its_resolution_and_still_follows_the_node() {
    let mut wins = vec![win("chart-0", DEFAULT_VENUE, "BTCUSDT", "5m")];
    wins[0].interval_pinned = true;
    let pub_ = vec![PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    let specs = follow_backend(&mut wins, &HashMap::new(), &pub_);
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@5m", "venue adopted, RESOLUTION preserved");
    assert_eq!(wins[0].title, "BYBIT BTCUSDT · 5m");
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].interval, "5m", "the ensure-spec carries the operator's interval");
    // …and it is STABLE: the venue+symbol now match a published row, so the next pass leaves
    // it alone rather than re-planning it every frame on an interval nobody publishes.
    assert!(follow_backend(&mut wins, &HashMap::new(), &pub_).is_empty());
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@5m");
}

/// The RANKING under an interval pin: the candidate's interval is discarded, so the choice is
/// made on the SYMBOL alone. A same-symbol row wins even when another row's interval happens
/// to equal the window's.
#[test]
fn an_interval_pinned_chart_ranks_on_symbol_because_the_interval_is_discarded() {
    let mut wins = vec![win("chart-0", DEFAULT_VENUE, "BTCUSDT", "5m")];
    wins[0].interval_pinned = true;
    let pub_ = vec![
        PublishedSeries::new("aster", "SOLUSDT", "5m"), // interval matches, symbol does not
        PublishedSeries::new("bybit", "BTCUSDT", "1m"), // symbol matches
    ];
    follow_backend(&mut wins, &HashMap::new(), &pub_);
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@5m");
}

/// A SERIES pin still beats everything, interval pin or not — the symbol picker's act is
/// unchanged by the split.
#[test]
fn a_series_pin_still_refuses_adoption_even_with_an_interval_pin() {
    let mut wins = vec![win("chart-0", "okx", "ETH-USDT", "15m")];
    wins[0].series_pinned = true;
    wins[0].interval_pinned = true;
    let pub_ = vec![PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    assert!(follow_backend(&mut wins, &HashMap::new(), &pub_).is_empty());
    assert_eq!(wins[0].key(), "okx:ETH-USDT@15m");
}

/// A chart that is RENDERING something is never yanked either, even unpinned.
#[test]
fn a_chart_with_bars_is_never_retargeted() {
    let mut wins = vec![win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m")];
    let charts = charted(&["BTCUSDT@1m"]);
    let pub_ = vec![PublishedSeries::new("bybit", "ETHUSDT", "1m")];
    assert!(follow_backend(&mut wins, &charts, &pub_).is_empty());
    assert_eq!(wins[0].key(), "BTCUSDT@1m");
}

/// A node publishing NOTHING invents nothing: no adoption, and the badge says so.
#[test]
fn a_silent_node_leaves_every_chart_where_it_was() {
    let mut wins = vec![win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m")];
    assert!(follow_backend(&mut wins, &HashMap::new(), &[]).is_empty());
    assert_eq!(wins[0].key(), "BTCUSDT@1m");
    assert_eq!(chart_feed(false, &[], "BTCUSDT@1m"), ChartFeed::Silent);
}

/// Already pointed at a published series ⇒ nothing to adopt; the bars are in flight.
#[test]
fn a_chart_already_on_a_published_series_does_not_move() {
    let mut wins = vec![win("chart-0", "bybit", "BTCUSDT", "1m")];
    let pub_ = vec![
        PublishedSeries::new("bybit", "BTCUSDT", "1m"),
        PublishedSeries::new("okx", "BTC-USDT", "1m"),
    ];
    assert!(follow_backend(&mut wins, &HashMap::new(), &pub_).is_empty());
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@1m");
    assert_eq!(chart_feed(false, &pub_, "bybit:BTCUSDT@1m"), ChartFeed::Waiting);
}

/// The RANKING: the same symbol+interval on another venue beats a better-sorted stranger.
/// `aster` sorts before `bybit`, so a first-in-sorted-order pick would take the wrong row.
#[test]
fn the_same_symbol_and_interval_on_another_venue_wins_the_rank() {
    let mut wins = vec![win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m")];
    let pub_ = vec![
        PublishedSeries::new("aster", "SOLUSDT", "5m"),
        PublishedSeries::new("bybit", "BTCUSDT", "1m"),
    ];
    follow_backend(&mut wins, &HashMap::new(), &pub_);
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@1m");
}

/// Two blank charts against a two-series node land on DIFFERENT series.
#[test]
fn two_blank_charts_spread_across_two_published_series() {
    let mut wins = vec![
        win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m"),
        win("chart-1", DEFAULT_VENUE, "BTCUSDT", "1m"),
    ];
    let pub_ = vec![
        PublishedSeries::new("bybit", "BTCUSDT", "1m"),
        PublishedSeries::new("bybit", "ETHUSDT", "1m"),
    ];
    follow_backend(&mut wins, &HashMap::new(), &pub_);
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@1m");
    assert_eq!(wins[1].key(), "bybit:ETHUSDT@1m");
}

/// ...but a single published series is shown TWICE rather than leaving a chart blank.
#[test]
fn one_series_and_two_blank_charts_shows_it_twice() {
    let mut wins = vec![
        win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m"),
        win("chart-1", DEFAULT_VENUE, "BTCUSDT", "1m"),
    ];
    let pub_ = vec![PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    assert_eq!(follow_backend(&mut wins, &HashMap::new(), &pub_).len(), 2);
    assert_eq!(wins[0].key(), "bybit:BTCUSDT@1m");
    assert_eq!(wins[1].key(), "bybit:BTCUSDT@1m");
}

/// Tool windows carry no series and must never appear in a plan.
#[test]
fn a_tool_window_is_not_a_chart_target() {
    let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0));
    let wins = vec![WinState::tool("tool-trade", WinKind::Trade, r)];
    assert!(chart_targets(&wins, &HashMap::new()).is_empty());
}

/// `WinState::new` leaves a chart ADOPTABLE — the startup default and a fresh `New chart
/// window` are both series nobody named.
#[test]
fn a_fresh_window_is_unpinned_and_a_restored_one_is_pinned() {
    let fresh = win("chart-0", DEFAULT_VENUE, "BTCUSDT", "1m");
    assert!(!fresh.series_pinned);
    assert!(!fresh.interval_pinned, "…and its interval is a placeholder too");
    // Round-trip that same window through the real capture/restore pair rather than
    // hand-building a `WinSnap`: the claim is about what `persist::apply` produces.
    let ws = crate::ui::workspace::persist::capture(
        std::slice::from_ref(&fresh),
        vike_chart::DisplayTz::Local,
        false,
    );
    let restored = crate::ui::workspace::persist::apply(&ws);
    assert!(restored[0].series_pinned, "a saved layout is a choice; it may not be yanked");
    assert!(restored[0].interval_pinned, "…and it chose the resolution as deliberately");
}

/// The badge is a FUNCTION of the data, in all four states.
#[test]
fn the_badge_only_says_live_when_bars_exist() {
    let pub_ = vec![PublishedSeries::new("bybit", "BTCUSDT", "1m")];
    assert_eq!(chart_feed(true, &[], "bybit:BTCUSDT@1m").badge(), "● LIVE");
    assert_eq!(chart_feed(false, &pub_, "bybit:BTCUSDT@1m").badge(), "○ WAITING");
    assert_eq!(chart_feed(false, &pub_, "BTCUSDT@1m").badge(), "○ NO FEED");
    assert_eq!(chart_feed(false, &[], "BTCUSDT@1m").badge(), "○ NO DATA");
    assert!(!chart_feed(false, &[], "BTCUSDT@1m").is_live());
    // Every state says something — `hint` is a TOOLTIP as well as the empty-plot overlay, and
    // an empty one renders as a blank box under the pointer.
    for f in [ChartFeed::Live, ChartFeed::Waiting, ChartFeed::Elsewhere, ChartFeed::Silent] {
        assert!(!f.hint().is_empty(), "{f:?} has no hint");
    }
}

/// The picker row always spells the venue, Binance included — see [`PublishedSeries::label`].
#[test]
fn a_picker_row_always_names_its_venue() {
    assert_eq!(PublishedSeries::new("bybit", "BTCUSDT", "1m").label(), "BYBIT:BTCUSDT · 1m");
    assert_eq!(
        PublishedSeries::new(DEFAULT_VENUE, "BTCUSDT", "1m").label(),
        "BINANCE:BTCUSDT · 1m"
    );
    // ...while the KEY keeps the historical Binance-bare spelling.
    assert_eq!(PublishedSeries::new(DEFAULT_VENUE, "BTCUSDT", "1m").key(), "BTCUSDT@1m");
}
