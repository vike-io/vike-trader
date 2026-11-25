//! The Economic page's table: the column header and the week's events grouped by day, each row
//! its time, flag, country, importance glyph, title and the actual, forecast and prior numbers.

use std::collections::HashMap;

use super::flag::{draw_flag, draw_impact};
use super::{CalendarEconomicEventView, CalendarEconomicImportance};
use crate::week::CalendarWeek;
use crate::window::empty_note;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::fonts;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::calendar;

/// The Economic page (Calendar page 0): the shown week's events, each on its day in the week's zone
/// (the day cards' rule), filtered by High only and the selected card (`selection`), at `full`, the
/// panel width the window captured before its cards (a local `available_width()` under-reports here).
pub(crate) fn economic_table(
    ui: &mut egui::Ui,
    events: &[CalendarEconomicEventView],
    week: CalendarWeek,
    high_only: bool,
    selection: Option<usize>,
    status: &str,
    now_ms: i64,
    full: f32,
    flags: &HashMap<String, egui::TextureHandle>,
) {
    use egui::RichText;
    use egui::{Align, Color32, Layout, vec2};
    let tokens = ThemeTokens::of(ui.ctx());
    let number = |s: &str| -> Option<f64> {
        let t: String =
            s.trim().chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
        t.parse::<f64>().ok()
    };
    // ---- column widths: Time/Country/impact fixed; Actual/Forecast/Prior scale with
    // width and right-align (vike _NUM_COL_W=210 in the full-width tab); Event stretches.
    // (uses the `full` captured at the top — local available_width() under-reports here.)
    let (w_time, country_width, w_imp) = (
        calendar::COLUMN_TIME_WIDTH,
        calendar::COLUMN_COUNTRY_WIDTH,
        calendar::COLUMN_IMPACT_WIDTH,
    );
    // vike _NUM_COL_W = 210 fixed when the panel is wide enough; scale down on narrow ones.
    let w_number = if full >= calendar::NUMBER_COLUMN_WIDE_AT {
        calendar::NUMBER_COLUMN_WIDTH
    } else {
        (full * calendar::NUMBER_COLUMN_FRACTION)
            .clamp(calendar::NUMBER_COLUMN_MIN_WIDTH, calendar::NUMBER_COLUMN_WIDTH)
    };
    let (w_act, w_fc, w_pr) = (w_number, w_number, w_number);
    // right margin = vike _RIGHT_PAD(40) + scrollbar clearance, so Prior doesn't collide
    // with the window edge / vertical scrollbar.
    let w_fixed = w_time + country_width + w_imp + w_act + w_fc + w_pr;
    let w_evt = (full - w_fixed - calendar::RIGHT_PADDING).max(calendar::EVENT_MIN_WIDTH);
    let hcell = |ui: &mut egui::Ui, w: f32, t: &str, right: bool| {
        let lay = if right {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center)
        };
        ui.allocate_ui_with_layout(vec2(w, calendar::HEADER_HEIGHT), lay, |ui| {
            ui.set_min_width(w); // reserve the FULL column width (else it collapses to text)
            ui.add(
                egui::Label::new(
                    RichText::new(t)
                        .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                        .color(tokens.theme.text_tertiary),
                )
                .truncate(),
            );
            // Py header 12px TEXT3
        });
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::NONE; // columns land exactly on computed widths
        hcell(ui, w_time, "Time", false);
        hcell(ui, country_width + w_imp, "Country", false);
        hcell(ui, w_evt, "Event", false);
        hcell(ui, w_act, "Actual", true);
        hcell(ui, w_fc, "Forecast", true);
        hcell(ui, w_pr, "Prior", true);
    });
    ui.painter().hline(
        ui.min_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(stroke::HAIRLINE, tokens.theme.border),
    );

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if events.is_empty() {
            ui.weak(empty_note(status));
        }
        let mut current_day = None;
        let (mut previous_time, mut previous_currency) = (String::new(), String::new());
        let mut marker_done = false;
        // Only the shown week's events, each on its day in the week's zone (the cards' rule).
        for (day, e) in events.iter().filter_map(|e| {
            let day = week.day_of(e.timestamp_ms)?;
            ((!high_only || e.importance == CalendarEconomicImportance::High)
                && selection.is_none_or(|s| s == day))
            .then_some((day, e))
        }) {
            if current_day != Some(day) {
                current_day = Some(day);
                previous_time.clear();
                previous_currency.clear();
                ui.add_space(space::MD);
                ui.label(
                    fonts::semibold(week.date(day).format("%A, %B %-d").to_string())
                        .size(role_px(ui.ctx(), ThemeTextRole::Title))
                        .color(tokens.theme.text),
                );
                // Py day-group bold/13 (700 folds into 600: `reading-heading`)
            }
            // red "● now HH:MM" marker before the first future event
            if !marker_done && e.timestamp_ms > now_ms {
                marker_done = true;
                let hhmm = week
                    .wall_clock(now_ms)
                    .map(|d| d.format("%H:%M").to_string())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.colored_label(
                        ThemeStatus::Error.color(),
                        RichText::new("●").size(role_px(ui.ctx(), ThemeTextRole::Strong)),
                    );
                    ui.label(
                        RichText::new(format!("now  {hhmm}")).color(ThemeStatus::Error.color()),
                    );
                });
            }
            let time = week
                .wall_clock(e.timestamp_ms)
                .map(|d| d.format("%H:%M").to_string())
                .unwrap_or_default();
            let show_time = time != previous_time;
            let show_currency = e.currency != previous_currency;
            previous_time.clone_from(&time);
            previous_currency = e.currency.clone();
            let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
            // Actual: countdown (red) for a future event with no print, else beat/miss color
            let future = e.actual.is_empty() && e.timestamp_ms > now_ms;
            let (act_txt, act_col) = if future {
                let d = (e.timestamp_ms - now_ms).max(0) / 1000;
                (
                    format!("Coming in {}:{:02}:{:02}", d / 3600, (d / 60) % 60, d % 60),
                    ThemeStatus::Error.color(),
                )
            } else if e.actual.is_empty() {
                ("—".to_string(), tokens.theme.text)
            } else {
                let col = match (number(&e.actual), number(&e.forecast)) {
                    (Some(a), Some(f)) if a > f => {
                        ThemeSidePair::ForecastAboveBelow.text(true, &tokens)
                    }
                    (Some(a), Some(f)) if a < f => {
                        ThemeSidePair::ForecastAboveBelow.text(false, &tokens)
                    }
                    _ => tokens.theme.text,
                };
                (e.actual.clone(), col)
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::NONE; // align rows to the header columns
                // TIME (mono)
                ui.allocate_ui_with_layout(
                    vec2(w_time, calendar::ROW_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(w_time);
                        if show_time {
                            ui.label(
                                RichText::new(&time)
                                    .monospace()
                                    .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                    .color(tokens.theme.text_tertiary),
                            );
                        }
                    },
                );
                // COUNTRY: flag + name + impact glyph
                ui.allocate_ui_with_layout(
                    vec2(country_width, calendar::ROW_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(country_width);
                        if show_currency {
                            let (fr, _) =
                                ui.allocate_exact_size(calendar::FLAG_SIZE, egui::Sense::hover());
                            if let Some(tex) = flags.get(&e.iso2) {
                                // real flag PNG texture (accurate); fall back to a drawn flag
                                ui.painter().image(
                                    tex.id(),
                                    fr,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    vike_ui_theme::color::UNTINTED,
                                );
                            } else if !e.iso2.is_empty() {
                                draw_flag(ui.painter(), fr, &e.iso2);
                            }
                            ui.add_space(space::MD);
                            let nm = if e.country_name.is_empty() {
                                &e.currency
                            } else {
                                &e.country_name
                            };
                            ui.add(
                                egui::Label::new(RichText::new(nm).color(tokens.theme.text))
                                    .truncate(),
                            );
                        }
                    },
                );
                ui.allocate_ui_with_layout(
                    vec2(w_imp, calendar::ROW_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(w_imp);
                        let (ir, _) =
                            ui.allocate_exact_size(calendar::IMPACT_SIZE, egui::Sense::hover());
                        draw_impact(ui.painter(), &tokens, ir, e.importance);
                    },
                );
                // EVENT
                ui.allocate_ui_with_layout(
                    vec2(w_evt, calendar::ROW_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_min_width(w_evt);
                        ui.add(
                            egui::Label::new(RichText::new(&e.title).color(tokens.theme.text))
                                .truncate(),
                        );
                    },
                );
                // ACTUAL / FORECAST / PRIOR (mono, right-aligned)
                let rcell = |ui: &mut egui::Ui, w: f32, t: String, c: Color32| {
                    ui.allocate_ui_with_layout(
                        vec2(w, calendar::ROW_HEIGHT),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            ui.set_min_width(w);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(t)
                                        .monospace()
                                        .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                        .color(c),
                                )
                                .truncate(),
                            );
                        },
                    );
                };
                rcell(ui, w_act, act_txt, act_col);
                rcell(ui, w_fc, dash(&e.forecast), tokens.theme.text);
                rcell(ui, w_pr, dash(&e.previous), tokens.theme.text);
            });
        }
    });
}
