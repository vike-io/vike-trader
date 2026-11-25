//! The Calendar window's `WindowToolContext` glue: the body (the day strip, the Economic table, the
//! Earnings, Dividends and IPO pages and their helpers) is `vike_calendar::calendar_window`, and
//! this hands it the poll's snapshot, the decoded flags, the zone and the window's
//! `WindowView::calendar`.

use super::WindowToolContext;
use crate::ui::windows::WindowView;
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

/// The Calendar tool body: the 7-day economic/earnings day strip + the per-page tables (flag
/// PNGs from `country_flags`), in the app's display time zone (`WindowToolContext::display_time_zone`, the Settings
/// window's `preferences.display_timezone`, read every frame) at the wall clock's now.
pub fn calendar_tool_content(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
) {
    let now_ms = chrono::Utc::now().timestamp_millis();
    calendar_tool_content_at(ui, context, tv, context.display_time_zone, now_ms);
}

/// [`calendar_tool_content`] at the instant `now_ms`, every date and time in `zone`, on the week
/// holding it moved by `tv.calendar.weeks_from_now` (Today, ← and → set that). The seam
/// `crates/vike-app-core/tests/screens/calendar_window_screens.rs` pins a week and a zone through.
pub fn calendar_tool_content_at(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    zone: ThemeDisplayTimeZone,
    now_ms: i64,
) {
    vike_calendar::calendar_window(
        ui,
        context.tool_feeds,
        context.country_flags,
        &mut tv.calendar,
        zone,
        now_ms,
    );
}
