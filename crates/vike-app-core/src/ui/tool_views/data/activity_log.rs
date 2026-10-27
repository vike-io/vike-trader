//! The Activity log destination: this session's log, its plane filter and the plane classifier.

use super::{dead_action, legend};
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// The Activity log destination: this session's in-memory log, with a plane filter, a find box,
/// Export (dead, and says why) and Clear.
pub(super) fn data_activity_log(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
) {
    use egui::RichText;
    use egui::vec2;
    let feeds = ctx.feeds;
    // ===== Activity log — promoted out of the Cached-feeds body =====
    //
    // It was a 110px-tall box under that table, which is the wrong home for the only record of
    // what this window has done: a reconnect, a bulk backfill's outcome and a delete all land
    // here, and all three were read through a letterbox. As its own destination it gets the
    // window.
    //
    // The strip is the design's: a segmented plane filter, a find box, Export and Clear.
    // Two of the four are REAL — the filter and Clear both act on `tv.data_log`, which
    // this module owns outright — and the other two are argued where they are rendered.
    let filter_id = ui.scope_id().with("dm_log_filter");
    let find_id = ui.scope_id().with("dm_log_find");
    let mut filter: Option<LogPlane> =
        ui.data_mut(|d| d.get_temp::<Option<LogPlane>>(filter_id)).unwrap_or_default();
    let mut find: String = ui.data_mut(|d| d.get_temp::<String>(find_id)).unwrap_or_default();
    let mut clear = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut filter, &LOG_SEGMENTS);
        ui.label(RichText::new("Find").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::LOG_FIND_W;
            input::text(
                ui,
                &mut find,
                Field { hint: "substring, case-insensitive", ..Field::default() },
            );
        });
        dead_action(
            ui,
            Some(icons::EXPORT),
            "Export",
            "Nothing in a tool body writes a file — the window carries no out-slot for one \
                     — so this log reaches disk only through the JSON file sink, which is a \
                     different sink with its own level.",
        );
        let clear_b = ActionButton::secondary((icons::CLEAR, "Clear"));
        let clear_b = if tv.data_log.is_empty() {
            clear_b.disabled_because("The log is already empty")
        } else {
            clear_b
        };
        if ui
            .add(clear_b)
            .on_hover_text(
                "Drop every line — this log lives in memory and nothing else holds a copy",
            )
            .clicked()
        {
            clear = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
    ui.data_mut(|d| d.insert_temp(find_id, find.clone()));
    if clear {
        tv.data_log.clear();
    }

    // The filter runs over the line TEXT, because nothing tags a line — `log_plane`'s doc
    // carries why that seam is where it is and what it would take to move it.
    let needle = find.trim().to_ascii_lowercase();
    let shown: Vec<&String> = tv
        .data_log
        .iter()
        .filter(|l| filter.is_none_or(|p| log_plane(l.as_str()) == p))
        .filter(|l| needle.is_empty() || l.to_ascii_lowercase().contains(&needle))
        .collect();
    // The count is stated even when nothing is filtered out, so an EMPTY plane reads as
    // "0 of 14 lines" — a filter that matched nothing — rather than as an empty log.
    ui.label(RichText::new("ACTIVITY LOG").size(t.text.px(TextRole::Caption)).color(t.theme.text3));
    legend(ui, &format!("{} of {} lines shown", shown.len(), tv.data_log.len()));
    ui.add_space(space::XS);
    // Every line is read character by character (timestamps, keys): mono, the Body role.
    let line_font = t.mono(TextRole::Body);
    // The footnote is drawn AFTER the log panel, and the panel's `ScrollArea` takes every pixel the
    // body has left (`auto_shrink([false, false])`), so the footnote used to land below the body's
    // bottom edge — on the foot line (a real-GPU capture, 2026-10-05). Its height is reserved
    // first, and the panel is shown in a child capped to what is left.
    const FOOTNOTE: &str = "This is the in-memory session log — it is not persisted and is not the \
                            JSON file log, whose level is the preferences.log_file_level setting.";
    let footnote_h = ui
        .painter()
        .layout(FOOTNOTE.to_owned(), t.font(TextRole::Caption), t.theme.text3, ui.available_width())
        .rect
        .height()
        + space::MD
        + ui.spacing().item_spacing.y;
    ui.scope(|ui| {
        ui.set_max_height((ui.available_height() - footnote_h).max(0.0));
        egui::Frame::new()
            .fill(t.theme.surface)
            .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
            .corner_radius(egui::CornerRadius::same(RADIUS))
            .inner_margin(t.metrics.gap)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("data_log")
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_height(data::LOG_MIN_H);
                        if tv.data_log.is_empty() {
                            // A line, not an empty state: the log is ready, not empty-handed.
                            ui.label(
                                RichText::new(format!(
                                    "Ready — {} live feeds (Binance WebSocket).",
                                    feeds.len()
                                ))
                                .font(line_font.clone())
                                .color(t.theme.text2),
                            );
                        } else if shown.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No line matches this filter — the log is not empty.",
                                )
                                .font(line_font.clone())
                                .color(t.theme.text3),
                            );
                        }
                        for line in &shown {
                            ui.label(
                                RichText::new(*line).font(line_font.clone()).color(t.theme.text2),
                            );
                        }
                    });
            });
    });
    ui.add_space(space::MD);
    legend(ui, FOOTNOTE);
}

/// Which plane an activity-log line belongs to — the classification the log's segmented filter
/// runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogPlane {
    /// Live sockets: a subscription started, stopped, reconnected, or the catalogue refreshed.
    Feeds,
    /// A bulk backfill's planning and outcome.
    Backfill,
    /// The stored inventory: a load, a rollup, a delete, a gap or stale sweep.
    Store,
}

/// Classify one activity-log line.
///
/// ⚠ **This is a HEURISTIC over free text, and it is one because no producer tags its lines.**
/// `tools::ToolView::data_log` is a `Vec<String>` and both writers today push a bare sentence —
/// this module's Refresh, and `crates/vike-desktop/src/app_ui/tool_pane.rs`'s `data_delete` drain, which
/// logs the feed it stopped. The structural fix is a tag on the entry (a typed line, or a leading
/// marker every producer writes), which needs a change to that `Vec<String>` in
/// `crates/vike-app-core/src/tools.rs` and to the other producer — neither of which is this
/// module's to make. So the filter classifies what it can see, and this doc is the record of why
/// the seam is where it is.
///
/// ⚠ **A consequence worth stating rather than discovering: today every line reads `Feeds`.** Both
/// producers are feed-plane events, so `Backfill` and `Store` are empty filters until something
/// writes such a line — which is the honest answer, not a broken one. The needles below are
/// deliberately the vocabulary the stored/backfill side ALREADY uses in its status strings
/// (`crates/vike-app-core/src/ui/tool_views/stored.rs` renders `backfill_status`), so the classifier
/// is right the day one of them starts logging instead of needing to be revisited then.
///
/// Backfill is checked before Store because a backfill line routinely names the store it wrote to,
/// and "what did this do" is the more specific question of the two.
pub(super) fn log_plane(line: &str) -> LogPlane {
    let l = line.to_ascii_lowercase();
    const BACKFILL: [&str; 5] = ["backfill", "backfilled", "queued", "appended", "unsupported"];
    const STORE: [&str; 8] =
        ["store", "deleted", "rollup", "gap", "stale", "scanned", "inventory", "truncat"];
    if BACKFILL.iter().any(|n| l.contains(n)) {
        LogPlane::Backfill
    } else if STORE.iter().any(|n| l.contains(n)) {
        LogPlane::Store
    } else {
        LogPlane::Feeds
    }
}

/// The log filter's segments, in render order. The first is "no filter" (`None`); the rest name
/// one [`LogPlane`] each. The segment carries its value, so no index maps one onto the other.
pub(super) const LOG_SEGMENTS: [Segment<'static, Option<LogPlane>>; 4] = [
    Segment { value: None, label: "All", why: "Every line this session has produced" },
    Segment {
        value: Some(LogPlane::Feeds),
        label: "Feeds",
        why: "Live-socket events: a subscription started, stopped or the catalogue refreshed",
    },
    Segment {
        value: Some(LogPlane::Backfill),
        label: "Backfill",
        why: "Bulk backfill planning and outcomes — empty until a backfill logs a line",
    },
    Segment {
        value: Some(LogPlane::Store),
        label: "Store",
        why: "Stored-inventory work: loads, rollups, deletes, gap and stale sweeps",
    },
];
