//! The Providers destination: the historical, event and streaming source planes and their rows.

use super::{dead_action, legend};
use crate::backend::backend_registry::BackendRecord;
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use crate::ui::tool_views::data_rail;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens, section};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// The Providers destination: the three source planes behind a segmented control, then the
/// Polymarket egress proxy box.
pub(super) fn data_providers(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
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
    // is `crates/vike-datahub/src/backfill.rs`'s `KLINE_SOURCES`, and `crate::data::
    // backfill_plan`'s module doc argues at length why this crate has no `vike-datahub`
    // edge and must not grow one: reaching it would drag the six kline bridge crates its
    // rows name into the GUI's tree to feed a table nothing here decides with. Guessing
    // the matrix locally would be worse than omitting it — a client-side roster that
    // disagrees with the server's reads as a capability the operator does not have, or
    // hides one they do.
    let plane_id = ui.id().with("dm_providers_plane");
    let mut plane: ProviderPlane =
        ui.data_mut(|d| d.get_temp::<ProviderPlane>(plane_id)).unwrap_or_default();
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
        ProviderPlane::Historical => providers_historical(ui, ctx),
        ProviderPlane::Event => providers_event(ui, ctx),
        ProviderPlane::Streaming => providers_streaming(ui, ctx),
    }
    // The Event plane is where the Finnhub and FMP rows are, so it is where their KEYS are entered:
    // stored on the server, never on this PC (`provider_keys`' module doc).
    if plane == ProviderPlane::Event {
        ui.add_space(space::MD);
        super::provider_keys::provider_keys_group(ui, &mut tv.provider_keys, backend);
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
fn providers_historical(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    legend(
        ui,
        "Where a BACKFILL draws history from. What each one can serve is the \
                         server's answer, not this window's — see the note below.",
    );
    ui.add_space(space::SM);
    // The store this window is reading is the one fact it holds first-hand.
    // `StoredCtx::delete_unavailable` is the signal, and it is exactly the right
    // one for THIS screen rather than a convenient proxy: `crate::data::stored_mode`'s
    // `stored_mode` sets it on — and only on — the `BackfillRoute::Wire` arm, so
    // the value that grays Delete is the same value that decides which route a
    // backfill takes. A Historical-sources screen asking "is the datahub the one
    // answering" is asking that question directly.
    let remote = ctx.stored.delete_unavailable.is_some();
    source_row(
        ui,
        if remote { Status::Ok.color() } else { Status::Muted.color() },
        "datahub",
        "the wire backfill verb",
        if remote {
            "answering — this grid is a REMOTE store"
        } else {
            "not in use — this grid is the LOCAL store"
        },
    );
    let held = data_rail::RailCounts::from_ctx(ctx);
    source_row(
        ui,
        if remote { Status::Muted.color() } else { Status::Ok.color() },
        "local store",
        "already-held history",
        &format!("{} series across {} venues, read from disk", held.series, held.venues),
    );
    source_row(
        ui,
        Status::Muted.color(),
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
fn providers_event(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    legend(
        ui,
        "Where the Calendar and News tools get their data: the backend's datahub, which polls the \
         providers on its own schedule (the provider keys live on ITS box). Each status below is \
         that tool's OWN last line from the datahub, not a probe this screen ran, and a tool is \
         only asked while its window is open.",
    );
    ui.add_space(space::SM);
    // Every row is real: each status string is the last sentence the poll published for that
    // feed. A feed that has not been asked yet publishes an empty string, which renders as
    // "not fetched yet" rather than as a failure — the two are different states and look
    // different.
    source_row(
        ui,
        fetch_dot(&ctx.td.cal_status),
        "forexfactory",
        "economic calendar",
        &fetch_state(&ctx.td.cal_status),
    );
    source_row(
        ui,
        fetch_dot(&ctx.td.news_status),
        "rss",
        "news headlines",
        &fetch_state(&ctx.td.news_status),
    );
    source_row(
        ui,
        fetch_dot(&ctx.td.cal_earnings_status),
        "finnhub",
        "earnings calendar",
        &fetch_state(&ctx.td.cal_earnings_status),
    );
    source_row(
        ui,
        fetch_dot(&ctx.td.cal_dividends_status),
        "fmp",
        "dividends calendar",
        &fetch_state(&ctx.td.cal_dividends_status),
    );
    source_row(
        ui,
        fetch_dot(&ctx.td.cal_ipo_status),
        "nasdaq",
        "IPO calendar",
        &fetch_state(&ctx.td.cal_ipo_status),
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
fn providers_streaming(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    legend(
        ui,
        "Where a live tick arrives from. One row per venue that has actually \
                         REGISTERED a feed-status handle with this process.",
    );
    ui.add_space(space::SM);
    // ⚠ A venue with no producer is ABSENT from `feed_statuses` and is therefore
    // absent here too — it is deliberately not listed as `Unknown`. `Unknown` is a
    // state a producer can publish (a line this window's parser cannot classify);
    // "no producer registered" is a different fact, and rendering the second as
    // the first invents a stream that was never opened. The count line below is
    // what states the absence, once, in terms of the roster.
    let mut venues: Vec<(String, String)> = ctx
        .feed_statuses
        .iter()
        .map(|(venue, handle)| (venue.clone(), handle.lock().unwrap().clone()))
        .collect();
    venues.sort_by(|a, b| a.0.cmp(&b.0));
    if venues.is_empty() {
        state::view(
            ui,
            Load::Empty(
                "No venue has registered a feed-status handle in this process — \
                                 nothing is streaming, and nothing is claimed to be.",
            ),
        );
    }
    for (venue, status) in &venues {
        source_row(
            ui,
            crate::ui::status_dot::feed_dot_color(status),
            venue,
            "live market data",
            if status.trim().is_empty() { "—" } else { status.as_str() },
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
            ctx.feeds.len(),
        ),
    );
}

/// Which plane of source the Providers destination is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ProviderPlane {
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
pub(super) const PROVIDER_SEGMENTS: [Segment<'static, ProviderPlane>; 3] = [
    Segment {
        value: ProviderPlane::Historical,
        label: "Historical",
        why: "Where a backfill draws already-past data from",
    },
    Segment {
        value: ProviderPlane::Event,
        label: "Event",
        why: "Where the Calendar and News tools get scheduled and published events from",
    },
    Segment {
        value: ProviderPlane::Streaming,
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
    let t = Tokens::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{25CF}").font(t.font(TextRole::Caption)).color(dot));
        ui.allocate_ui_with_layout(
            egui::vec2(data::SOURCE_W_NAME, t.metrics.row_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(name).font(t.mono(TextRole::Body)).color(t.theme.text),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(data::SOURCE_W_SERVES, t.metrics.row_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(serves).font(t.font(TextRole::Body)).color(t.theme.text2),
                );
            },
        );
        ui.label(
            egui::RichText::new(state_line).font(t.font(TextRole::Caption)).color(t.theme.text3),
        );
    });
}

/// Dot colour for a background fetcher's own published status line.
///
/// ⚠ An EMPTY line is `MUTED`, not a fault: a fetcher that has not run yet publishes nothing, and
/// the two states an operator must be able to tell apart on this screen are "it failed" and "it
/// has not happened". `crate::ui::status_dot::feed_dot_color` classifies the non-empty case through
/// the ONE shared parser, so a fetcher line and a feed line are never judged by two rules.
fn fetch_dot(status: &str) -> egui::Color32 {
    if status.trim().is_empty() {
        Status::Muted.color()
    } else {
        crate::ui::status_dot::feed_dot_color(status)
    }
}

/// The text beside [`fetch_dot`] — the fetcher's own line, or the honest absence of one.
fn fetch_state(status: &str) -> String {
    if status.trim().is_empty() {
        "not fetched yet this session".to_string()
    } else {
        status.to_string()
    }
}
