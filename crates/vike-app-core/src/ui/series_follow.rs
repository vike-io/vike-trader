//! `series_follow` — **the chart follows the series the backend actually publishes**.
//!
//! # The defect this module exists to close
//!
//! `vike-desktop` mounts no venue and owns no core (rulings 1 and 2 of the 2026-09-09 rename
//! design): every bar it can ever draw arrives over the node protocol as the wire snapshot's bar
//! tails, rebuilt into the core bar cache by [`crate::backend::observe_bridge::wire_to_core`]. The GUI
//! decides WHICH of those series to render from its own `spawned` set, and `spawned` is filled
//! from the WORKSPACE — a saved layout, or [`crate::ui::startup::plan`]'s default chart, which invents
//! `binance` / `BTCUSDT` / `1m` because [`crate::ui::workspace::DEFAULT_VENUE`] is `binance` and
//! something has to be charted.
//!
//! So the two sides name series independently, and on any node that is not mounting Binance they
//! never meet. MEASURED end to end against a real daemon mounting bybit `BTCUSDT` `1m`: the node
//! published `bybit:BTCUSDT@1m`, the GUI was subscribed to `BTCUSDT@1m`, and
//! [`crate::ui::core_sync::sync_from_core`] dropped every frame with
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
//! The list is taken from the SERIES, never from [`vike_exec::CoreSnapshot::venue`] /
//! [`symbol`](vike_exec::CoreSnapshot::symbol): those two scalars are the PRIMARY ENGINE's, and on
//! the measured node they read `binance` / `BTCUSDT` while the only wired feed was bybit. They are
//! the same lie the GUI was already telling, arriving from the other end.

use crate::ui::workspace::{self, WinKind, WinState};
use std::collections::HashMap;
use vike_ui_theme::components::role_px;
use vike_ui_theme::type_scale::TextRole;

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
pub fn published_series(snap: &vike_exec::CoreSnapshot) -> Vec<PublishedSeries> {
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
    /// **The operator CHOSE this window's RESOLUTION** — [`WinState::interval_pinned`]. A weaker
    /// claim than [`Self::pinned`] and deliberately so: adoption may still move this window's
    /// venue and symbol, but must carry the window's OWN interval across rather than the
    /// candidate's. See that field's doc for why the two are separate acts.
    pub interval_pinned: bool,
    /// This window's `ChartState` holds at least one bar. A chart that is rendering something is
    /// never retargeted, whatever anybody publishes — the defect being fixed is an EMPTY chart.
    pub has_bars: bool,
    /// **What KIND of instrument this window's symbol names** — [`WinState::asset_class`], carried
    /// rather than dropped since `docs/decisions/0061`'s Phase 2.
    ///
    /// It is the one field of a window's series identity this struct used to have no slot for, and
    /// the drop was silent: the symbol picker writes it atomically with venue and symbol
    /// (`crates/vike-desktop/src/app_ui.rs`'s `draw_windows`), `crate::ui::workspace::persist` round-trips
    /// it across restarts, and `crate::ui::startup::plan` already carries it into a `FeedSpec` — so the
    /// class survived startup and died here.
    ///
    /// ⚠ **[`plan_follow`] does not read it and must not start.** Adoption copies a series off the
    /// wire, where the node has already resolved the native instrument; [`follow_backend`] clears
    /// the window's own tag to `None` for the reason its doc gives, and 0061's caller table
    /// classifies that path as one that must ASK rather than assume. This field's destination is
    /// [`crate::data::store_bars::StoreBarRequest`] and, beyond it, the seed request's optional class.
    pub class: Option<vike_model::AssetClass>,
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
///    pick and an interval change all pin; [`crate::ui::startup::plan`]'s default chart and a fresh
///    `New chart window` do not, because neither names a series anybody asked for;
/// 3. **it is rendering nothing** — `!`[`ChartTarget::has_bars`]. A chart with data on it is never
///    yanked, even unpinned;
/// 4. **its own triple is not in the published list** — if the node publishes exactly what this
///    window is subscribed to, the bars are already on their way and there is nothing to adopt.
///    ⚠ On an INTERVAL-PINNED window this test drops to `(venue, symbol)`: its interval is by
///    definition one the node may not be publishing, so comparing the triple would make every such
///    window permanently adoptable and re-plan it every frame.
///
/// # ⚠ The INTERVAL-PINNED case — a RE-TARGET that preserves the operator's resolution
///
/// A window whose [`ChartTarget::interval_pinned`] is set still adopts, but the [`Adoption`] it
/// produces carries **the window's own interval**, not the candidate's. The candidate supplies
/// only `(venue, symbol)`. So a chart the operator switched to `5m`, on a node mounting
/// `bybit BTCUSDT 1m`, lands on `bybit:BTCUSDT@5m` — the venue this node actually has, at the
/// resolution the operator actually asked for — instead of being yanked back to `1m`.
///
/// ⚠ **That is a deliberate, narrow break with "no fabrication" below**, and it is the only one.
/// The venue and symbol are still copied verbatim out of a [`PublishedSeries`]; only the interval
/// comes from the window, and it comes from an explicit operator act rather than from thin air.
/// The resulting triple may be a series this node does NOT stream — which is exactly the case the
/// backend-STORE read ([`crate::data::store_bars`]) exists to serve, and why the two changes only make
/// sense together: without the store read the window would be correctly targeted and empty.
///
/// Which series it adopts is RANKED, so the common case — the same chart pointed at the wrong
/// venue, which is the measured defect — resolves to the obvious answer rather than to "the first
/// thing published": same symbol AND interval, then same symbol, then same interval, then
/// anything, each tier broken by [`published_series`]' sorted order. On an interval-pinned window
/// the interval half of that tier is MEANINGLESS (every candidate's interval is discarded), so it
/// is scored as matching and the ranking collapses to symbol-then-sorted-order.
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
        // An interval-pinned window is judged on `(venue, symbol)` alone: its interval is the
        // operator's and is carried across, so the node publishing it is neither necessary nor
        // expected. Comparing the full triple here would leave such a window adoptable forever.
        let already = if w.interval_pinned {
            published.iter().any(|p| p.venue == w.venue && p.symbol == w.symbol)
        } else {
            published.iter().any(|p| p.matches(&w.venue, &w.symbol, &w.interval))
        };
        if already {
            continue; // already pointed at a published series — the bars are coming
        }
        let pick = published
            .iter()
            .enumerate()
            .min_by_key(|(i, p)| {
                // The candidate's interval is discarded for an interval-pinned window, so scoring
                // it would rank on a fact that cannot reach the result.
                let interval_match = w.interval_pinned || p.interval == w.interval;
                let tier = match (p.symbol.eq_ignore_ascii_case(&w.symbol), interval_match) {
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
            // THE ONE FABRICATED FIELD, and only on an explicit operator act: the venue and symbol
            // are the node's, the RESOLUTION is the window's. See the fn doc's interval-pinned
            // section.
            let interval = if w.interval_pinned { w.interval.clone() } else { p.interval.clone() };
            out.push(Adoption {
                window: w.id,
                series: PublishedSeries::new(&p.venue, &p.symbol, &interval),
            });
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
            interval_pinned: w.interval_pinned,
            has_bars: charts.get(&w.key()).is_some_and(|c| !c.bars.is_empty()),
            // 0061 Phase 2's first drop seam, closed. A COPY rather than a clone —
            // `vike_model::AssetClass` is `Copy`.
            class: w.asset_class,
        })
        .collect()
}

/// **Apply the plan** — the one call the shell makes. Retargets each adopting window in place and
/// returns the subscriptions the caller must `ensure_feed_on`, as [`crate::ui::startup::FeedSpec`]s.
///
/// The retarget is the SAME five writes the symbol picker's own apply performs
/// (`crates/vike-desktop/src/app_ui.rs`'s `draw_windows`, its `new_symbol` arm): venue, symbol,
/// interval, `retitle`, and `follow.on_series_change` — the last so the adopted series autoscales
/// instead of inheriting the placeholder's zoom. `asset_class` is cleared to `None`, which is what
/// [`crate::backend::venue_routing::venue_bar_instrument`] wants for a series arriving over the wire: the
/// node already resolved the native instrument, and a stale `CryptoPerp` tag from the previous
/// target would make that resolver refuse a venue outside its perp allowlist outright.
///
/// ⚠ It does NOT set [`WinState::series_pinned`] — nor [`WinState::interval_pinned`]. An adoption
/// is not a choice, and leaving the window unpinned is what lets it follow a node that later moves
/// — the adopted window stops adopting because [`ChartTarget::has_bars`] goes true the moment the
/// series paints, and starts again by itself if that series goes away and leaves the chart empty.
/// An interval-pinned window keeps the pin it already had, which is what the retarget preserved.
pub fn follow_backend(
    wins: &mut [WinState],
    charts: &HashMap<String, vike_chart::model::ChartState>,
    published: &[PublishedSeries],
) -> Vec<crate::ui::startup::FeedSpec> {
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
        specs.push(crate::ui::startup::FeedSpec {
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
/// down. This touches egui only (no eframe, no wgpu), which is the same line `crate::ui::workspace`
/// already sits on.
///
/// A [`ChartFeed::Live`] chart paints NOTHING, by the early return below rather than by its hint
/// happening to be blank — see [`ChartFeed::hint`], which is also a tooltip and therefore says
/// something in every state.
///
/// ⚠ **`note` is the CHART-SEED sentence and it is painted BELOW the hint, never instead of it.**
/// The hint answers "which of the three ways is this chart empty"; the note answers "and here is
/// what your SERVER did or would not do about it"
/// ([`crate::data::chart_seed::render_chart_seed_status`]). They are different questions and an operator
/// staring at a blank pane needs both: without the note, a datahub whose chart-seed lane is unarmed
/// and a symbol the venue does not list produce the identical picture, and the next move is to
/// doubt the venue. `None` restores the pre-note rendering exactly.
pub fn paint_empty_hint(ui: &egui::Ui, feed: ChartFeed, note: Option<&str>) {
    if feed.is_live() {
        return;
    }
    let centre = ui.max_rect().center();
    ui.painter().text(
        centre,
        egui::Align2::CENTER_CENTER,
        feed.hint(),
        egui::FontId::proportional(role_px(ui.ctx(), TextRole::Title)),
        ui.visuals().weak_text_color(),
    );
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        ui.painter().text(
            centre + egui::vec2(0.0, 20.0),
            egui::Align2::CENTER_CENTER,
            note,
            egui::FontId::proportional(role_px(ui.ctx(), TextRole::Body)),
            ui.visuals().weak_text_color(),
        );
    }
}

#[path = "series_follow_tests.rs"]
#[cfg(test)]
mod series_follow_tests;
