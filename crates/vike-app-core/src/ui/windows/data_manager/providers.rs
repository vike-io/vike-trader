//! The Providers destination: the historical, event and streaming source planes and their rows.

use super::{dead_action, legend};
use crate::backend::registry::BackendRecord;
use crate::ui::windows::WindowToolContext;
use crate::ui::windows::WindowView;
use vike_datahub_client::tool_feeds::poll::DatahubPollHealth;
use vike_ui_theme::components::segmented::{self, ThemeSegment};
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, section};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data;

/// The Providers destination: the three source planes behind a segmented control, then the
/// Polymarket egress proxy box.
pub(super) fn data_providers(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    t: &ThemeTokens,
    backend: Option<&BackendRecord>,
) {
    use egui::vec2;
    // ===== Providers — the merge of three tabs that rendered ONE list =====
    //
    // `Historical Providers`, `Event Providers` and `Streaming Providers` were three top-level
    // tabs sharing a single trailing `else` arm, so all three printed this same hardcoded list
    // of seven names. Three tabs, one list, and no way to tell from the outside — which is why
    // they are one destination and this comment exists.
    //
    // ⚠ **The three tab NAMES survive as the segmented control below, and they now mean
    // something.** The list they used to share was seven hardcoded venue names with one
    // marked `(live)` — it answered none of the three tabs' questions, because a venue is
    // not a source: the same venue answers a historical backfill, a live socket and no
    // event feed at all, through three different things. Each segment therefore renders
    // the sources of ITS plane, with whatever state this window can actually observe for
    // them — and where it can observe none, it says so instead of colouring a dot.
    //
    // ⚠ **What this screen deliberately does NOT render is the design's capability MATRIX**
    // (source × store kind, lit where that source serves that kind). The authority for it
    // is `crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`, and `crate::data::
    // backfill::plan`'s module doc argues at length why this crate has no `vike-datahub`
    // edge and must not grow one: reaching it would drag the six kline bridge crates its
    // rows name into the GUI's tree to feed a table nothing here decides with. Guessing
    // the matrix locally would be worse than omitting it — a client-side roster that
    // disagrees with the server's reads as a capability the operator does not have, or
    // hides one they do.
    let plane_id = ui.scope_id().with("data_manager_providers_plane");
    let mut plane: DataManagerProviderPlane =
        ui.data_mut(|d| d.get_temp::<DataManagerProviderPlane>(plane_id)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut plane, &PROVIDER_SEGMENTS);
        dead_action(
            ui,
            None,
            "+ Add source",
            "A source is not a row an operator adds: each one is a linked collector or a \
                     configured endpoint, and which exist is decided by the build and by the \
                     daemon this window talks to. Nothing here can register one.",
        );
    });
    ui.add_space(space::SM);
    match plane {
        DataManagerProviderPlane::Historical => providers_historical(ui, context),
        DataManagerProviderPlane::Event => providers_event(ui, context),
        DataManagerProviderPlane::Streaming => providers_streaming(ui, context),
    }
    // The Event plane is where the Finnhub and FMP rows are, so it is where their KEYS are entered:
    // stored on the server, never on this PC (`provider_keys`' module doc).
    if plane == DataManagerProviderPlane::Event {
        ui.add_space(space::MD);
        super::super::backend::provider_keys::provider_keys_group(
            ui,
            &mut tv.provider_keys,
            backend,
        );
    }
    ui.data_mut(|d| d.insert_temp(plane_id, plane));

    // The Polymarket egress proxy, moved here from the stored grid's body. This is the
    // destination for "where can data come from, and why is none arriving" — and for
    // Polymarket the usual answer is geo-blocking rather than anything about the store.
    //
    // ⚠ Its old site carried a hard-won constraint: it had to sit ABOVE the stored body's
    // loading early-return, or it vanished on exactly the screen an operator reaches it
    // from — an empty or still-loading store. This destination has no early return at all,
    // so the property holds by construction rather than by remembering it.
    section::strip_rule(ui);
    if let Some(value) = vike_data_manager::polymarket_proxy_ui(ui, &mut tv.stored_proxy) {
        tv.stored_proxy_save = Some(value);
    }
}

/// Providers, Historical plane: where a BACKFILL draws history from.
fn providers_historical(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    legend(
        ui,
        "Where a BACKFILL draws history from. What each one can serve is the \
                         server's answer, not this window's — see the note below.",
    );
    ui.add_space(space::SM);
    // The store this window is reading is the one fact it holds first-hand: the
    // resolved datahub address (`WindowToolContext::execution_history`'s `datahub_address`) is the value
    // `crate::data::data_manager::mode`'s `stored_mode` decides the grid from and the shell
    // decides a backfill's route from. A Historical-sources screen asking "is the
    // datahub the one answering" is asking that question directly.
    let remote = context.execution_history.datahub_address.is_some();
    source_row(
        ui,
        if remote { ThemeStatus::Ok.color() } else { ThemeStatus::Muted.color() },
        "datahub",
        "the wire backfill verb",
        if remote {
            "answering — this grid is a REMOTE store"
        } else {
            "none resolved — this window reads no store"
        },
    );
    // ⚠ No "local store" row: this build has no local store engine (it went with the fat build), so
    // a row offering "already-held history, read from disk" named a source that does not exist.
    source_row(
        ui,
        ThemeStatus::Muted.color(),
        "venue REST",
        "kind=bar klines",
        "reached THROUGH the route above, never directly from this window",
    );
    ui.add_space(space::MD);
    legend(
        ui,
        "The one client-side gate a backfill applies is kind=bar with an interval; \
                         every other selected series is counted as a skip. WHICH venues have a \
                         collector is the server's roster, and an off-roster venue is still sent \
                         so the refusal that comes back names the server's own supported set.",
    );
}

/// Providers, Event plane: where the Calendar and News tools get their data.
fn providers_event(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    legend(
        ui,
        "Where the Calendar and News tools get their data: the backend's datahub, which polls the \
         providers on its own schedule (the provider keys live on ITS box). Each status below is \
         that tool's OWN last line from the datahub, not a probe this screen ran, and a tool is \
         only asked while its window is open.",
    );
    ui.add_space(space::SM);
    // Every row is real: each status string is the last sentence the poll published for that
    // feed, and each dot the health it set beside it. A feed that has not been asked yet
    // publishes an empty string, which renders as "not fetched yet" rather than as a failure —
    // the two are different states and look different.
    source_row(
        ui,
        fetch_dot(context.tool_feeds.calendar_health),
        "forexfactory",
        "economic calendar",
        &fetch_state(&context.tool_feeds.calendar_status),
    );
    source_row(
        ui,
        fetch_dot(context.tool_feeds.news_health),
        "rss",
        "news headlines",
        &fetch_state(&context.tool_feeds.news_status),
    );
    // The equity calendars are held per shown week, every week asked of the same three sources:
    // the earliest week shown speaks for them.
    let unasked = vike_datahub_client::tool_feeds::poll::CalendarEquityWeek::default();
    let equity = context.tool_feeds.calendar_weeks.values().next().unwrap_or(&unasked);
    source_row(
        ui,
        fetch_dot(equity.earnings_health),
        "finnhub",
        "earnings calendar",
        &fetch_state(&equity.earnings_status),
    );
    source_row(
        ui,
        fetch_dot(equity.dividends_health),
        "fmp",
        "dividends calendar",
        &fetch_state(&equity.dividends_status),
    );
    source_row(
        ui,
        fetch_dot(equity.ipo_health),
        "nasdaq",
        "IPO calendar",
        &fetch_state(&equity.ipo_status),
    );
    ui.add_space(space::MD);
    legend(
        ui,
        "A provider key the datahub does not hold blanks that one row and says which key to \
         store on the datahub's box; the keyless feeds still land, and no venue credential is \
         involved in any of them.",
    );
}

/// Providers, Streaming plane: where a live tick arrives from.
fn providers_streaming(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    legend(
        ui,
        "Where a live tick arrives from. One row per venue that has actually \
                         REGISTERED a feed-status handle with this process.",
    );
    ui.add_space(space::SM);
    // ⚠ A venue with no producer is ABSENT from `feed_statuses` and is therefore
    // absent here too — it is deliberately not listed as `Unknown`. `Unknown` is a
    // state a producer can publish (a line it has set no state for);
    // "no producer registered" is a different fact, and rendering the second as
    // the first invents a stream that was never opened. The count line below is
    // what states the absence, once, in terms of the roster.
    let mut venues: Vec<(String, vike_model::feed_status::DataClientStatus)> = context
        .feed_statuses
        .iter()
        .map(|(venue, handle)| (venue.clone(), handle.lock().unwrap().clone()))
        .collect();
    venues.sort_by(|a, b| a.0.cmp(&b.0));
    if venues.is_empty() {
        state::view(
            ui,
            ThemeEmptyPane::Empty(
                "No venue has registered a feed-status handle in this process — the \
                                 connected datahub has advertised none, or has not answered yet. \
                                 Nothing is streaming, and nothing is claimed to be.",
            ),
        );
    }
    for (venue, status) in &venues {
        source_row(
            ui,
            crate::ui::status_bar::dot::feed_dot_color(status),
            venue,
            "live market data",
            if status.text.trim().is_empty() { "—" } else { status.text.as_str() },
        );
    }
    ui.add_space(space::MD);
    legend(
        ui,
        &format!(
            "{} venue(s) publish a status line; {} of {} roster venues publish \
                             none, so they are absent above rather than shown as Unknown. {} bar \
                             feed(s) are cached right now — the Cached feeds destination lists \
                             them.",
            venues.len(),
            vike_model::VENUES.len().saturating_sub(venues.len()),
            vike_model::VENUES.len(),
            context.feeds.len(),
        ),
    );
}

/// Which plane of source the Providers destination is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DataManagerProviderPlane {
    /// Where a BACKFILL can draw history from.
    #[default]
    Historical,
    /// Where the calendar and news tools fetch scheduled/published events from.
    Event,
    /// Where a live tick arrives from.
    Streaming,
}

/// The Providers segments, in render order — the three old tab NAMES, now meaning three different
/// answers rather than three routes to one list.
pub(super) const PROVIDER_SEGMENTS: [ThemeSegment<'static, DataManagerProviderPlane>; 3] = [
    ThemeSegment {
        value: DataManagerProviderPlane::Historical,
        label: "Historical",
        why: "Where a backfill draws already-past data from",
    },
    ThemeSegment {
        value: DataManagerProviderPlane::Event,
        label: "Event",
        why: "Where the Calendar and News tools get scheduled and published events from",
    },
    ThemeSegment {
        value: DataManagerProviderPlane::Streaming,
        label: "Streaming",
        why: "Where a live tick arrives from, right now, in this process",
    },
];

/// One source row: a state dot, the source's name, what it serves, and what this window can say
/// about it right now.
///
/// Fixed columns rather than an `egui::Grid` because the three planes supply different numbers of
/// rows and a `Grid` sizes its columns per instance — so switching segments would shift the name
/// column sideways, which reads as a different table rather than a different cut of one.
fn source_row(ui: &mut egui::Ui, dot: egui::Color32, name: &str, serves: &str, state_line: &str) {
    let t = ThemeTokens::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{25CF}").font(t.font(ThemeTextRole::Caption)).color(dot));
        ui.allocate_ui_with_layout(
            egui::vec2(data::SOURCE_WIDTH_NAME, t.metrics.row_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(name).font(t.mono(ThemeTextRole::Body)).color(t.theme.text),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(data::SOURCE_WIDTH_SERVES, t.metrics.row_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(serves)
                        .font(t.font(ThemeTextRole::Body))
                        .color(t.theme.text_secondary),
                );
            },
        );
        ui.label(
            egui::RichText::new(state_line)
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
    });
}

/// Dot colour for a polled feed, from the [`DatahubPollHealth`] the poll set beside its line, never
/// from the words. Each health wears the colour the News window's footer gives it
/// (`crates/vike-news/src/window/mod.rs`'s `footer_ink`), so the two views of the news feed agree.
///
/// ⚠ `Unasked` is `MUTED`, not a fault: the two states an operator must be able to tell apart on
/// this screen are "it failed" and "it has not happened". `Reconnecting` (a link riding out a short
/// failure) is muted too; the poll turns it `Unavailable`, red, once the failure streak is long.
pub(super) fn fetch_dot(health: DatahubPollHealth) -> egui::Color32 {
    match health {
        DatahubPollHealth::Unasked | DatahubPollHealth::Reconnecting => ThemeStatus::Muted,
        DatahubPollHealth::Live => ThemeStatus::Ok,
        DatahubPollHealth::Stale => ThemeStatus::Warning,
        DatahubPollHealth::Warming => ThemeStatus::Info,
        DatahubPollHealth::Unavailable => ThemeStatus::Error,
    }
    .color()
}

/// The text beside [`fetch_dot`] — the fetcher's own line, or the honest absence of one.
fn fetch_state(status: &str) -> String {
    if status.trim().is_empty() {
        "not fetched yet this session".to_string()
    } else {
        status.to_string()
    }
}
