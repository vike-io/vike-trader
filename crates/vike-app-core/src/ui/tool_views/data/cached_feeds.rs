//! The Cached feeds destination: the live-feed catalog, its action set and its table.

use super::{dead_action, legend};
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::table::{self, Column};
use vike_ui_theme::fmt::fmt_thousands;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// The Cached feeds destination: the live-feed catalog, its action strip and its table.
pub(super) fn data_cached_feeds(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
) {
    use egui::RichText;
    use egui::{Align, Layout, vec2};
    let feeds = ctx.feeds;
    // ===== Cached feeds — the live-feed catalog (the only one with real data) =====
    //
    // ⚠ **The action set below is the DESIGN's, and it REPLACED a different one.** This
    // strip used to carry ten dead data-ENGINE verbs — Update all, Download / Extend…,
    // Import CSV…, Inspect, Repair gaps, Clean data, Pin / Unpin, Instruments…, Truncate…,
    // Remove inactive… — a wishlist for a stored-series manager, rendered on the one
    // screen in this window that shows no stored series at all. The design replaces them
    // with the FEED-plane verbs a live-socket catalogue actually has, and every one of
    // them now states, on its disabled-hover, the thing that does not exist behind it.
    // That is the difference between a strip that is honest and a strip that is merely
    // grey: the old ten were dimmed and said nothing, so "why can I not click this" had no
    // answer anywhere in the window.
    //
    // ⚠ **One of the design's ten is NOT dead here, and the design's own mock dims it:**
    // `Remove feed`. `tools::ToolView::data_delete` is a live out-slot —
    // `crates/vike-desktop/src/app_ui.rs` drains it, stops that subscription and logs the
    // stop — so the verb works today. It shipped spelled `🗑 Delete`, which on a screen
    // about sockets reads as deleting DATA; what it does is end a subscription. The LABEL
    // moves onto the verb and the behaviour does not move at all. Matching the mock by
    // dimming a button that works would be the opposite of the honesty rule, not an
    // instance of it.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        if ui.add(ActionButton::secondary((icons::REFRESH, "Refresh"))).clicked() {
            let ts = vike_chart::to_naive(chrono::Utc::now().timestamp_millis(), ctx.display_tz)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
            tv.data_log.push(format!("{ts}  Refreshed · {} series", feeds.len()));
        }
        for (icon, label, why) in FEED_ACTIONS_BEFORE_REMOVE {
            dead_action(ui, icon, label, why);
        }
        // The design's `Remove feed`, in the design's position, LIVE — see the arm's
        // header comment. Selection-gated because it stops exactly one subscription.
        let remove = ActionButton::secondary((icons::REMOVE, "Remove feed"));
        let remove = if tv.data_sel.is_some() {
            remove
        } else {
            remove.disabled_because(
                "Pick a row first — this stops ONE subscription, the one selected",
            )
        };
        if ui
            .add(remove)
            .on_hover_text("Stop the selected subscription; its cached bars go with it")
            .clicked()
        {
            tv.data_delete = tv.data_sel.take();
        }
        for (icon, label, why) in FEED_ACTIONS_AFTER_REMOVE {
            dead_action(ui, icon, label, why);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} series", feeds.len()))
                    .font(t.font(TextRole::Caption))
                    .color(t.theme.text3),
            );
        });
    });
    legend(
        ui,
        "A feed is started by a chart window subscribing a series, never from this \
                 catalogue — so the one lifecycle verb here is the one that ends a subscription.",
    );
    ui.add_space(space::SM);

    // The kit's data table (owner decision 3): every cell is text. The selected row is
    // derived from `data_sel`, so a feed that vanishes simply selects nothing.
    let selected = tv.data_sel.as_deref().and_then(|k| feeds.iter().position(|f| f.0 == k));
    let seen = table::data_table(ui, "data_tbl", &FEED_COLUMNS, feeds.len(), selected, |r, c| {
        feed_cell(&feeds[r], c, ctx.directory)
    });
    if let Some(r) = seen.clicked {
        tv.data_sel = Some(feeds[r].0.clone());
    }
    if feeds.is_empty() {
        state::view(ui, Load::Empty("No cached series — open a chart to start a feed."));
    }
}

// ===== The Cached-feeds action set =====
//
// The design's ten feed-plane verbs, split in TWO because the one that is live — `Remove feed` —
// sits seventh in the design's order and a single loop cannot render a live button in the middle
// of a dead run. Splitting the array is the smaller evil: the alternative is an `Option<&str>` per
// row where `None` means "this one is real", which puts the interesting fact in the absence of a
// string. Keep the two halves in the design's order; the join point is the live verb.
//
// ⚠ Each `why` names the thing that DOES NOT EXIST, never "not implemented yet". The reader of a
// disabled-hover is deciding whether to go looking for another way to do it, and "no verb exists"
// and "this window cannot reach the verb" send them to different places.

/// Design order, positions 1–6: icon, words, why. `Add feed` keeps its ASCII `+` in the words and
/// takes no registry icon, as the Store screen's `+ Mount store…` does.
const FEED_ACTIONS_BEFORE_REMOVE: [(Option<Icon>, &str, &str); 6] = [
    (
        Some(icons::PAUSE),
        "Pause",
        "No suspend state exists. `crate::ui::feed_lifecycle`'s model has a series subscribed or not \
         subscribed, and a paused socket is neither — nothing would hold the gap it left.",
    ),
    (
        Some(icons::RUN),
        "Resume",
        "The twin of Pause: with nothing suspended there is nothing to resume. Reopening a chart \
         on the series re-subscribes it, which is the whole of the restart path today.",
    ),
    (
        Some(icons::RESTART),
        "Restart",
        "No single verb does it. `crate::ui::feed_lifecycle`'s reaper frees a subscription's slot only \
         once no window references it, so a restart is Remove feed plus reopening the chart.",
    ),
    (
        Some(icons::RECONNECT),
        "Reconnect",
        "The bridge reconnects itself — the WS pump owns its own retry — and nothing in this \
         window can poke that loop from outside it.",
    ),
    (
        Some(icons::CLEAR),
        "Drop cache",
        "The cached bars ARE the subscription's own ring; there is no verb that empties one \
         without stopping the feed, which is what Remove feed already does.",
    ),
    (
        None,
        "+ Add feed",
        "A feed is created by a chart window subscribing a series, and `tools::ToolView` carries no \
         out-slot for starting one from here. Open a chart on the series instead.",
    ),
];

/// Design order, positions 8–10 (position 7 is the live `Remove feed`): icon, words, why.
const FEED_ACTIONS_AFTER_REMOVE: [(Option<Icon>, &str, &str); 3] = [
    (
        Some(icons::RATE_LIMIT),
        "Rate limit",
        "No client-side throttle exists: the venue's own limits govern this stream, and a knob \
         here would suggest this window could change them.",
    ),
    (
        Some(icons::EXPORT),
        "Export",
        "Nothing in a tool body writes a file — the window has no out-slot for one — so there is \
         no path from this table to disk.",
    ),
    (
        Some(icons::BACKFILL),
        "Backfill",
        "These rows are LIVE feeds, not stored series. The bulk backfill plans over the stored \
         inventory's keys, which is the All-series, Has-gaps and Stale destinations.",
    ),
];

/// Cached feeds' columns. Every cell is text, so it is the kit's data table. Bars and the two
/// bounds are numeric: mono and right-aligned.
const FEED_COLUMNS: [Column<'static>; 6] = [
    Column {
        title: "Symbol",
        weight: data::FEEDS_SYMBOL_WEIGHT,
        min_w: data::FEEDS_SYMBOL_MIN_W,
        numeric: false,
    },
    Column {
        title: "Timeframe",
        weight: data::FEEDS_TIMEFRAME_WEIGHT,
        min_w: data::FEEDS_TIMEFRAME_MIN_W,
        numeric: false,
    },
    Column {
        title: "Bars",
        weight: data::FEEDS_BARS_WEIGHT,
        min_w: data::FEEDS_BARS_MIN_W,
        numeric: true,
    },
    Column {
        title: "From",
        weight: data::FEEDS_FROM_WEIGHT,
        min_w: data::FEEDS_FROM_MIN_W,
        numeric: true,
    },
    Column {
        title: "To",
        weight: data::FEEDS_TO_WEIGHT,
        min_w: data::FEEDS_TO_MIN_W,
        numeric: true,
    },
    Column {
        title: "Source",
        weight: data::FEEDS_SOURCE_WEIGHT,
        min_w: data::FEEDS_SOURCE_MIN_W,
        numeric: false,
    },
];

/// One Cached-feeds cell. The key is the `[venue:]SYMBOL@interval` the chart windows subscribe
/// under (`crate::ui::workspace::series_key`). The Source column is the feed's own venue
/// (`crate::backend::venue_routing::venue_of_key`), spelled as the settings database spells it, else
/// its key (I15(b)); the Symbol column is the symbol alone.
pub(super) fn feed_cell(
    feed: &(String, usize, i64, i64),
    col: usize,
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
) -> String {
    let (key, n, first, last) = feed;
    let (sym, tf) = key.split_once('@').unwrap_or((key.as_str(), ""));
    match col {
        0 => sym.split_once(':').map_or(sym, |(_, s)| s).to_string(),
        1 => tf.to_string(),
        2 => fmt_thousands(*n as f64).trim_end_matches(".00").to_string(),
        3 => fmt_series_dt(*first),
        4 => fmt_series_dt(*last),
        _ => {
            let venue = crate::backend::venue_routing::venue_of_key(key);
            super::venue_label(directory, venue).to_string()
        }
    }
}

/// Format a series-coverage epoch-millisecond bound for the Cached-Series table. Non-positive (the
/// "no coverage yet" sentinel the feed cache reports for an empty series) and un-representable
/// values render as an em-dash rather than a misleading 1970 timestamp. Always UTC — this column
/// pins the raw store bound, unlike the activity log, which stamps in the display timezone.
pub(super) fn fmt_series_dt(ms: i64) -> String {
    if ms > 0 {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    } else {
        "—".into()
    }
}
