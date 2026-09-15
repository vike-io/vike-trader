//! `series_follow` — **the chart follows the series the backend actually publishes**.
//!
//! # The defect this module exists to close
//!
//! `vike-desktop` mounts no venue and owns no core (rulings 1 and 2 of the 2026-09-09 rename
//! design): every bar it can ever draw arrives over the node protocol as the wire snapshot's bar
//! tails, rebuilt into the core bar cache by [`crate::observe_bridge::wire_to_core`]. The GUI
//! decides WHICH of those series to render from its own `spawned` set, and `spawned` is filled
//! from the WORKSPACE — a saved layout, or [`crate::startup::plan`]'s default chart, which invents
//! `binance` / `BTCUSDT` / `1m` because [`crate::workspace::DEFAULT_VENUE`] is `binance` and
//! something has to be charted.
//!
//! So the two sides name series independently, and on any node that is not mounting Binance they
//! never meet. MEASURED end to end against a real daemon mounting bybit `BTCUSDT` `1m`: the node
//! published `bybit:BTCUSDT@1m`, the GUI was subscribed to `BTCUSDT@1m`, and
//! [`crate::core_sync::sync_from_core`] dropped every frame with
//! `reason="not subscribed GUI-side (spawned holds no such key)"`. The chart was empty, the title
//! bar said `● LIVE`, and the price axis had autoscaled to −1.00…0.90 over no data.
//!
//! ⚠ **And the operator could not reach the series by hand either.** `spawn_catalog_fetcher` in
//! `crates/vike-desktop/src/main.rs` builds one provider (`vike_deribit::DeribitCatalog`) — the
//! eleven venue catalogs beside it went with the venue bridges — and the `SYMS` quick-picks
//! hard-reset `new_venue` to `DEFAULT_VENUE`. Through the title bar an operator can reach
//! `binance` and `deribit`. Not bybit. Not any other venue a node might mount.
//!
//! # The two halves, and why neither is sufficient alone
//!
//! * **ADOPTION** ([`follow_backend`]) — a chart NOBODY CHOSE, rendering NOTHING, retargets itself
//!   onto a series the node publishes. Narrow by construction: see [`ChartTarget::pinned`].
//! * **REACH** ([`published_series`]) — the snapshot's series list, rendered as a section of the
//!   symbol picker, so every published series is one click away whatever the catalog holds. This
//!   is the half that serves a chart the operator DID choose, which adoption deliberately refuses
//!   to touch.
//!
//! The list is taken from the SERIES, never from [`vike_core::CoreSnapshot::venue`] /
//! [`symbol`](vike_core::CoreSnapshot::symbol): those two scalars are the PRIMARY ENGINE's, and on
//! the measured node they read `binance` / `BTCUSDT` while the only wired feed was bybit. They are
//! the same lie the GUI was already telling, arriving from the other end.

use crate::workspace::{self, WinKind, WinState};
use std::collections::HashMap;

/// One `(venue, symbol, interval)` triple a backend node is PUBLISHING bars for.
///
/// Owned strings rather than borrows of the snapshot: the list outlives the `arc_swap` guard the
/// caller loaded it through, and the shell holds it across frames so the symbol picker can render
/// it without re-reading the cell mid-layout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublishedSeries {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
}

impl PublishedSeries {
    pub fn new(venue: &str, symbol: &str, interval: &str) -> Self {
        Self {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
        }
    }

    /// The GUI-side chart/feed key — the SAME builder `ensure_feed_on` and [`WinState::key`] use,
    /// so "is this series subscribed" is one string comparison and never a re-derivation.
    pub fn key(&self) -> String {
        workspace::series_key(&self.venue, &self.symbol, &self.interval)
    }

    /// How the symbol picker names this row: `BYBIT:BTCUSDT · 1m`.
    ///
    /// ⚠ The venue is ALWAYS spelled, Binance included — unlike [`workspace::series_key`] and the
    /// window title, both of which drop it for `DEFAULT_VENUE` to keep the historical single-venue
    /// spellings byte-identical. In THIS list the venue is the whole point: the row exists to say
    /// which venue the node is mounting, and a bare `BTCUSDT · 1m` beside a `BYBIT:BTCUSDT · 1m`
    /// would read as "some other symbol" rather than as "the Binance one".
    pub fn label(&self) -> String {
        format!("{}:{} · {}", self.venue.to_uppercase(), self.symbol, self.interval)
    }

    fn matches(&self, venue: &str, symbol: &str, interval: &str) -> bool {
        self.venue == venue && self.symbol == symbol && self.interval == interval
    }
}

/// Every series in a snapshot's bar cache, deduplicated and in a STABLE order.
///
/// Sorted rather than left in `IndexMap` insertion order, and that is load-bearing twice over: the
/// picker rows must not reshuffle under the pointer between frames, and [`plan_follow`]'s
/// tie-break is "the first candidate in this order", which is only a rule if the order is a
/// function of the CONTENT rather than of whatever the publisher happened to build first.
pub fn published_series(snap: &vike_core::CoreSnapshot) -> Vec<PublishedSeries> {
    let mut out: Vec<PublishedSeries> = snap
        .bars
        .keys()
        .map(|(venue, symbol, interval)| PublishedSeries::new(venue, symbol, interval))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// What one chart window looks like to the adoption decision — the facts the rule reads, lifted
/// off [`WinState`] + the render model so [`plan_follow`] is a pure function of data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartTarget {
    pub id: egui::Id,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// **The operator CHOSE this window's series** — [`WinState::series_pinned`]. Adoption refuses
    /// a pinned window outright; the picker section is how a pinned window reaches a published
    /// series, by an act rather than behind the operator's back.
    pub pinned: bool,
    /// This window's `ChartState` holds at least one bar. A chart that is rendering something is
    /// never retargeted, whatever anybody publishes — the defect being fixed is an EMPTY chart.
    pub has_bars: bool,
}

/// One window retargeted onto one published series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoption {
    pub window: egui::Id,
    pub series: PublishedSeries,
}

/// THE RULE, as a pure function. A window adopts a published series when **all four** hold:
///
/// 1. it is a chart window (tool windows have no series — [`chart_targets`] filters them);
/// 2. **nobody chose its series** — `!`[`ChartTarget::pinned`]. A saved workspace, a symbol-picker
///    pick and an interval change all pin; [`crate::startup::plan`]'s default chart and a fresh
///    `New chart window` do not, because neither names a series anybody asked for;
/// 3. **it is rendering nothing** — `!`[`ChartTarget::has_bars`]. A chart with data on it is never
///    yanked, even unpinned;
/// 4. **its own triple is not in the published list** — if the node publishes exactly what this
///    window is subscribed to, the bars are already on their way and there is nothing to adopt.
///
/// Which series it adopts is RANKED, so the common case — the same chart pointed at the wrong
/// venue, which is the measured defect — resolves to the obvious answer rather than to "the first
/// thing published": same symbol AND interval, then same symbol, then same interval, then
/// anything, each tier broken by [`published_series`]' sorted order.
///
/// A series already claimed by an earlier window in THIS plan is deprioritised, so two blank
/// charts against a two-series node land on different series. It is a preference and not a
/// refusal: when every candidate is claimed, the best one is adopted again rather than leaving a
/// second chart blank — showing one series twice is a lesser failure than showing nothing.
///
/// ⚠ **No fabrication.** Every field of every [`Adoption`] is copied out of a [`PublishedSeries`]
/// this node actually published. An empty `published` returns an empty plan: a node with nothing
/// on it leaves every chart exactly where it was, which is what [`ChartFeed::Silent`] then says in
/// the title bar.
pub fn plan_follow(published: &[PublishedSeries], wins: &[ChartTarget]) -> Vec<Adoption> {
    let mut out = Vec::new();
    if published.is_empty() {
        return out;
    }
    let mut claimed: Vec<&PublishedSeries> = Vec::new();
    for w in wins {
        if w.pinned || w.has_bars {
            continue;
        }
        if published.iter().any(|p| p.matches(&w.venue, &w.symbol, &w.interval)) {
            continue; // already pointed at a published series — the bars are coming
        }
        let pick = published
            .iter()
            .enumerate()
            .min_by_key(|(i, p)| {
                let tier =
                    match (p.symbol.eq_ignore_ascii_case(&w.symbol), p.interval == w.interval) {
                        (true, true) => 0,
                        (true, false) => 1,
                        (false, true) => 2,
                        (false, false) => 3,
                    };
                (usize::from(claimed.contains(p)), tier, *i)
            })
            .map(|(_, p)| p);
        if let Some(p) = pick {
            claimed.push(p);
            out.push(Adoption { window: w.id, series: p.clone() });
        }
    }
    out
}

/// Lift the [`ChartTarget`] rows out of the live window list + render model.
///
/// Tool windows are filtered here rather than inside [`plan_follow`], so the planner's input is
/// exactly "the charts" and a test can state a case without inventing a `WinKind`.
pub fn chart_targets(
    wins: &[WinState],
    charts: &HashMap<String, vike_chart::model::ChartState>,
) -> Vec<ChartTarget> {
    wins.iter()
        .filter(|w| w.kind == WinKind::Chart)
        .map(|w| ChartTarget {
            id: w.id,
            venue: w.venue.clone(),
            symbol: w.symbol.clone(),
            interval: w.interval.clone(),
            pinned: w.series_pinned,
            has_bars: charts.get(&w.key()).is_some_and(|c| !c.bars.is_empty()),
        })
        .collect()
}

/// **Apply the plan** — the one call the shell makes. Retargets each adopting window in place and
/// returns the subscriptions the caller must `ensure_feed_on`, as [`crate::startup::FeedSpec`]s.
///
/// The retarget is the SAME five writes the symbol picker's own apply performs
/// (`crates/vike-desktop/src/app_ui.rs`'s `draw_windows`, its `new_symbol` arm): venue, symbol,
/// interval, `retitle`, and `follow.on_series_change` — the last so the adopted series autoscales
/// instead of inheriting the placeholder's zoom. `asset_class` is cleared to `None`, which is what
/// [`crate::venue_routing::venue_bar_instrument`] wants for a series arriving over the wire: the
/// node already resolved the native instrument, and a stale `CryptoPerp` tag from the previous
/// target would make that resolver refuse a venue outside its perp allowlist outright.
///
/// ⚠ It does NOT set [`WinState::series_pinned`]. An adoption is not a choice, and leaving the
/// window unpinned is what lets it follow a node that later moves — the adopted window stops
/// adopting because [`ChartTarget::has_bars`] goes true the moment the series paints, and starts
/// again by itself if that series goes away and leaves the chart empty.
pub fn follow_backend(
    wins: &mut [WinState],
    charts: &HashMap<String, vike_chart::model::ChartState>,
    published: &[PublishedSeries],
) -> Vec<crate::startup::FeedSpec> {
    let plan = plan_follow(published, &chart_targets(wins, charts));
    let mut specs = Vec::new();
    for a in plan {
        let Some(w) = wins.iter_mut().find(|w| w.id == a.window) else { continue };
        tracing::info!(
            was = %workspace::series_key(&w.venue, &w.symbol, &w.interval),
            now = %a.series.key(),
            "chart adopted a series this node publishes (nothing chose the old one; it was empty)"
        );
        w.venue = a.series.venue.clone();
        w.symbol = a.series.symbol.clone();
        w.interval = a.series.interval.clone();
        w.asset_class = None;
        w.retitle();
        w.follow.on_series_change();
        specs.push(crate::startup::FeedSpec {
            venue: a.series.venue,
            symbol: a.series.symbol,
            interval: a.series.interval,
            asset_class: None,
        });
    }
    specs
}

/// What a chart's title-bar badge is ENTITLED to claim.
///
/// The badge was an unconditional `● LIVE` label — a `RichText` constant in
/// `crates/vike-desktop/src/chart_window.rs`'s `title_bar` with no input at all — so an empty grid
/// over a node publishing nothing read exactly like a live candle stream. That is the one claim in
/// the window an operator cannot check by looking, which is precisely why it may not be a
/// constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartFeed {
    /// Bars have arrived for this series. The historical `● LIVE`, and the ONLY state that says it.
    Live,
    /// No bars yet, and the node publishes EXACTLY this series — they are on their way.
    Waiting,
    /// No bars, and the node publishes other series but not this one. The picker's backend section
    /// is the way out, and the badge tooltip says so.
    Elsewhere,
    /// No bars, and the node publishes nothing at all.
    Silent,
}

impl ChartFeed {
    /// The title-bar badge. The filled `●` is reserved for [`Self::Live`]; every other state gets a
    /// hollow `○`, so the distinction survives a monochrome screenshot and does not rest on colour.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Live => "● LIVE",
            Self::Waiting => "○ WAITING",
            Self::Elsewhere => "○ NO FEED",
            Self::Silent => "○ NO DATA",
        }
    }

    /// The badge's hover text, and — for every state but [`Self::Live`] — the sentence
    /// [`paint_empty_hint`] paints across the empty plot.
    ///
    /// ⚠ [`Self::Live`]'s is a real sentence rather than `""` DELIBERATELY: it is a tooltip as well
    /// as an overlay, and an empty hover text renders as an empty tooltip box under the pointer.
    /// The "paint nothing on a live chart" rule is the early return in [`paint_empty_hint`], where
    /// it is a property of the function, not a consequence of a string being blank.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Live => "bars are arriving from the backend for this series",
            Self::Waiting => "waiting for the first bars of this series from the backend",
            Self::Elsewhere => {
                "this backend publishes no bars for this series — pick one it does publish from \
                 the symbol menu"
            }
            Self::Silent => "this backend is publishing no bar series at all",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// Classify one chart for the badge, by its [`WinState::key`]. Pure, and deliberately reads the
/// SAME published list the picker and the adoption planner read, so the three surfaces cannot
/// disagree about what the node has.
///
/// Keyed on the STRING rather than on the triple because the caller already holds `w.key()` — and
/// because that key is what `spawned` and the fold's filter are keyed on, so "the badge says
/// WAITING" and "the fold would render this" are the same comparison rather than two that could
/// drift.
pub fn chart_feed(has_bars: bool, published: &[PublishedSeries], key: &str) -> ChartFeed {
    if has_bars {
        return ChartFeed::Live;
    }
    if published.is_empty() {
        return ChartFeed::Silent;
    }
    if published.iter().any(|p| p.key() == key) { ChartFeed::Waiting } else { ChartFeed::Elsewhere }
}

/// Paint [`ChartFeed::hint`] across an empty chart body — the sentence that replaces "an empty grid
/// badged LIVE".
///
/// It lives HERE rather than at the `crates/vike-desktop/src/app_ui.rs` call site for the reason
/// every other decision in this module does: that file is in `EXCLUDE_FROM_CI`, and the
/// `ci_excluded_gui_shell_ratchet` says logic testable without eframe or wgpu belongs one crate
/// down. This touches egui only (no eframe, no wgpu), which is the same line `crate::workspace`
/// already sits on.
///
/// A [`ChartFeed::Live`] chart paints NOTHING, by the early return below rather than by its hint
/// happening to be blank — see [`ChartFeed::hint`], which is also a tooltip and therefore says
/// something in every state.
pub fn paint_empty_hint(ui: &egui::Ui, feed: ChartFeed) {
    if feed.is_live() {
        return;
    }
    ui.painter().text(
        ui.max_rect().center(),
        egui::Align2::CENTER_CENTER,
        feed.hint(),
        egui::FontId::proportional(13.0),
        ui.visuals().weak_text_color(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::DEFAULT_VENUE;
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
        // Round-trip that same window through the real capture/restore pair rather than
        // hand-building a `WinSnap`: the claim is about what `persist::apply` produces.
        let ws = crate::workspace::persist::capture(
            std::slice::from_ref(&fresh),
            vike_chart::DisplayTz::Local,
            2.0,
            false,
        );
        let restored = crate::workspace::persist::apply(&ws);
        assert!(restored[0].series_pinned, "a saved layout is a choice; it may not be yanked");
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
}
