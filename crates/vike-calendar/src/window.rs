//! The Calendar window body: the nav bar (Today, the week's range, High only, the zone), the
//! 7-day day strip with each day's counts, the page pills and the page's status line, over one
//! [`CalendarWeek`]; the Economic page draws [`economic_table`], the other three
//! [`calendar_equity_table`]. With the small notes every page shares ([`empty_note`], [`footer_status`],
//! [`calendar_day_label`]).

use std::collections::HashMap;

use crate::CalendarState;
use crate::economic::table::economic_table;
use crate::economic::{CalendarEconomicEventView, CalendarEconomicImportance};
use crate::equity::calendar_equity_table;
use crate::week::{CalendarWeek, week_range_label};
use vike_datahub_client::tool_feeds::poll::{
    CalendarEquityWeek, DatahubPollInterest, DatahubPollSnapshot,
};
use vike_ui_theme::components::{ThemeTokens, role_px};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::calendar;
use vike_ui_theme::{fonts, icons};

/// The Calendar window: the 7-day economic/earnings day strip + the per-page tables (flag PNGs
/// from `flags`), at the instant `now_ms`, every date and time in `zone`, on the week holding it
/// moved by `state.weeks_from_now` (Today, ← and → set that): the week, its range, the cards, the
/// selected card and the list's day groups are ONE `CalendarWeek`. `vike-app-core`'s
/// `calendar_tool_content_at` hands it the poll's snapshot, the flags, the window's state and the
/// zone; `crates/vike-app-core/tests/screens/calendar_window_screens.rs` pins a week and a zone
/// through that seam.
pub fn calendar_window(
    ui: &mut egui::Ui,
    td: &DatahubPollSnapshot,
    flags: &HashMap<String, egui::TextureHandle>,
    state: &mut CalendarState,
    zone: ThemeDisplayTimeZone,
    now_ms: i64,
) {
    use egui::RichText;
    use egui::{Align, Layout, vec2};
    let tokens = ThemeTokens::of(ui.ctx());
    let weeks_from_now = state.weeks_from_now;
    let week = CalendarWeek::holding(now_ms, zone).weeks_later(weeks_from_now);
    // Record the week shown and that this window shows the calendars (all four pages feed the
    // day-strip counts), so the poll thread keeps asking the datahub for that week's.
    td.feeds_link.show_week(week.monday);
    td.feeds_link.want(DatahubPollInterest::Calendar);
    // This window's week of earnings, dividends and IPOs (none until the poll has answered it).
    let unanswered = CalendarEquityWeek::default();
    let equity =
        td.calendar_weeks.get(&week.monday.format("%Y-%m-%d").to_string()).unwrap_or(&unanswered);
    // The economic events as the views draw them: the poll holds the datahub's RAW rows.
    let events: Vec<CalendarEconomicEventView> =
        td.calendar.iter().map(CalendarEconomicEventView::from_wire).collect();
    // capture the FULL panel width ONCE up front — reading available_width() again lower
    // down (after the cards/pills rows) under-reports it, so cards + table share this.
    let full = ui.available_width();
    // per-window view state (read this frame; widgets below mutate state for next frame)
    let high_only = state.high_only;
    let page = state.page;
    let selection_day = state.selected_day;
    const PAGES: [&str; 4] = ["Economic", "Earnings", "Dividends", "IPO"];

    // ---- nav bar: Today  ‹ ›  range ........  High only  Countries  Local ----
    // vike's nav controls are taller/roomier than egui's default — bump the padding
    let navigation_spacing = |ui: &mut egui::Ui| {
        ui.spacing_mut().button_padding = vec2(space::XL, space::LG);
        ui.spacing_mut().item_spacing.x = space::MD;
    };
    let range = week_range_label(week.monday);
    // The week a click asks for (weeks from now), `None` without one. The buttons' responses were
    // thrown away (`let _ =`), so ← and → did nothing.
    let navigation_left = |ui: &mut egui::Ui| -> Option<i32> {
        let today = ui.button("Today").clicked();
        let earlier = icons::named(ui.button(icons::EARLIER), "Earlier").clicked();
        let later = icons::named(ui.button(icons::LATER), "Later").clicked();
        ui.add_space(space::MD);
        // never broken: a wrapping row moves the whole range below the buttons instead
        ui.add(
            egui::Label::new(
                fonts::semibold(&range)
                    .size(role_px(ui.ctx(), ThemeTextRole::Title))
                    .color(tokens.theme.text),
            )
            .extend(),
        ); // Py 16px/600
        if today {
            Some(0)
        } else if earlier {
            Some(weeks_from_now - 1)
        } else if later {
            Some(weeks_from_now + 1)
        } else {
            None
        }
    };
    let navigation_right = |ui: &mut egui::Ui, high_only: &mut bool| {
        ui.checkbox(high_only, RichText::new("High only").color(tokens.theme.text_secondary));
        ui.add_space(space::SM);
        let _ = ui.add(
            egui::Button::new("Countries").min_size(vec2(calendar::COUNTRIES_BUTTON_WIDTH, 0.0)),
        );
        // names the zone every date below is in
        let _ = ui
            .add(egui::Button::new(zone.label()).min_size(vec2(calendar::LOCAL_BUTTON_WIDTH, 0.0)));
    };
    // Both groups are measured first, unseen: a window too narrow for one row puts the right group
    // on a row of its own (the owner's capture of 2026-11-16 had it drawn over the week's range).
    let size_of = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
        let mut probe = ui.new_child(
            egui::UiBuilder::new()
                .sizing_pass()
                .invisible()
                .layout(Layout::left_to_right(Align::Center)),
        );
        navigation_spacing(&mut probe);
        add(&mut probe);
        probe.min_rect().size()
    };
    let mut probe_high_only = state.high_only;
    let left_w = size_of(ui, &mut |ui| {
        navigation_left(ui);
    })
    .x;
    let right = size_of(ui, &mut |ui| navigation_right(ui, &mut probe_high_only));
    let right_w = right.x;
    let go = if left_w + space::XL + right_w <= full {
        ui.horizontal(|ui| {
            navigation_spacing(ui);
            let go = navigation_left(ui);
            ui.add_space((ui.available_width() - right_w).max(space::NONE));
            navigation_right(ui, &mut state.high_only);
            go
        })
        .inner
    } else {
        let go = ui
            .horizontal_wrapped(|ui| {
                navigation_spacing(ui);
                navigation_left(ui)
            })
            .inner;
        // As tall as its buttons from the start, so the check box (drawn first) centres on them.
        ui.horizontal_wrapped(|ui| {
            navigation_spacing(ui);
            ui.set_min_height(right.y);
            ui.add_space((ui.available_width() - right_w).max(space::NONE));
            navigation_right(ui, &mut state.high_only);
        });
        go
    };
    if let Some(weeks) = go {
        state.weeks_from_now = weeks;
    }
    ui.add_space(space::MD);

    // ---- day-strip cards: FIXED 7-day Mon→Sun week (vike CalendarSpace), each with
    // Economic (ForexFactory) + Earnings/Dividends/IPO (Nasdaq) counts; zero rows hidden.
    {
        let mut economic = [0usize; 7];
        for day in events
            .iter()
            .filter(|e| !high_only || e.importance == CalendarEconomicImportance::High)
            .filter_map(|e| week.day_of(e.timestamp_ms))
        {
            economic[day] += 1;
        }
        // Equity rows carry a calendar date and no instant: a card counts its own date's.
        let cards: Vec<(String, [usize; 4])> = (0..7)
            .map(|day| {
                let date = week.date(day);
                let (earn, div, ipo) = equity
                    .counts
                    .get(&date.format("%Y-%m-%d").to_string())
                    .copied()
                    .unwrap_or((0, 0, 0));
                (date.format("%a %-d").to_string(), [economic[day], earn, div, ipo])
            })
            .collect();
        let gap = space::LG;
        let title_px = role_px(ui.ctx(), ThemeTextRole::Title);
        let row_px = role_px(ui.ctx(), ThemeTextRole::Strong);
        let title = |s: &str| fonts::semibold(s).size(title_px).color(tokens.theme.text);
        let word = |s: &str| RichText::new(s).size(row_px).color(tokens.theme.text_secondary);
        // a count is a number: mono
        let count_text = |n: usize| {
            RichText::new(n.to_string()).monospace().size(row_px).color(tokens.theme.text)
        };
        // A card is never narrower than what it holds: its title, and its widest WORD, the strip's
        // gap, then the week's widest count, measured in the fonts this frame draws them in. A
        // share of the window clamped to a floor ran the word under its count (`Economi24`,
        // `Earning134` in the owner's capture of 2026-11-16), and the words are never shortened.
        let measure = |ui: &egui::Ui, t: RichText| {
            egui::WidgetText::from(t)
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::FontSelection::Default,
                )
                .size()
                .x
        };
        let words_w = PAGES.iter().map(|c| measure(ui, word(c))).fold(0.0, f32::max);
        let counts_w = cards
            .iter()
            .flat_map(|(_, c)| c)
            .map(|n| measure(ui, count_text(*n)))
            .fold(0.0, f32::max);
        let titles_w = cards.iter().map(|(l, _)| measure(ui, title(l))).fold(0.0, f32::max);
        let content_w = titles_w.max(words_w + gap + counts_w);
        // The Frame's outer width also carries its stroke on both sides: leave it out and seven
        // cards overrun the row by 14 hairlines (Sun clipped).
        let chrome_w = 2.0 * space::LG + 2.0 * stroke::HAIRLINE;
        let cw = ((full - gap * 6.0) / 7.0)
            .clamp(calendar::CARD_MIN_WIDTH, calendar::CARD_MAX_WIDTH)
            .max(content_w + chrome_w);
        // Seven cards wider than the window scroll sideways rather than cut Saturday and Sunday off.
        egui::ScrollArea::horizontal().id_salt("cal_days").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (i, (label, counts)) in cards.iter().enumerate() {
                    let on = i as i8 == selection_day; // selected card → HOVER fill + ACCENT border (title stays text)
                    let inner = egui::Frame::new()
                        .fill(if on { tokens.theme.hover } else { tokens.theme.surface })
                        .stroke(egui::Stroke::new(
                            stroke::HAIRLINE,
                            if on { tokens.theme.accent } else { tokens.theme.border },
                        ))
                        .corner_radius(egui::CornerRadius::same(RADIUS))
                        .inner_margin(egui::Margin::symmetric(space::LG as i8, space::SM as i8))
                        .show(ui, |ui| {
                            // force VERTICAL — the Frame inherits the cards-row left_to_right
                            // layout otherwise, so title + rows flow sideways and overlap.
                            ui.vertical(|ui| {
                                ui.set_width(cw - chrome_w);
                                ui.set_min_height(calendar::CARD_MIN_HEIGHT);
                                // rows packed at their own height: a compact card moves the table up
                                ui.spacing_mut().item_spacing.y = space::NONE;
                                ui.label(title(label));
                                ui.add_space(space::XS);
                                for (ci, count) in counts.iter().enumerate() {
                                    if *count == 0 {
                                        continue; // hide zero rows (vike _DayCard.set_counts)
                                    }
                                    ui.horizontal(|ui| {
                                        ui.label(word(PAGES[ci]));
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.label(count_text(*count));
                                            },
                                        );
                                    });
                                }
                            });
                        });
                    let r = ui.interact(
                        inner.response.rect,
                        egui::Id::unique(("cal_card", i)),
                        egui::Sense::click(),
                    );
                    if r.clicked() {
                        // toggle: click the selected card again to clear
                        state.selected_day = if on { -1 } else { i as i8 };
                        state.page = 0; // selecting a day jumps to the Economic page
                    }
                }
            });
        });
    }
    ui.add_space(space::MD);

    // ---- category pills + All categories ----
    ui.horizontal(|ui| {
        for (i, name) in PAGES.iter().enumerate() {
            let active = i == page as usize;
            if ui
                .selectable_label(
                    active,
                    RichText::new(*name)
                        .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                        .color(if active { tokens.theme.text } else { tokens.theme.text_tertiary }),
                )
                .clicked()
            {
                state.page = i as u8;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let _ = ui.button("All categories"); // (Py hides the data-source status here)
        });
    });
    ui.add_space(space::XS);

    // selected day-card (`-1`: none) → filter the Economic table to that day of the week
    let selection = usize::try_from(selection_day).ok();

    // The page's own feed status: the economic page's, or the equity feed the page shows. ⚠ It is
    // reserved as a BOTTOM PANEL, declared before the table's scroll area: a status label drawn
    // after a `ScrollArea` that takes all the remaining height lands below the window's floor (the
    // same defect `crates/vike-app-core/src/ui/windows/data_manager/mod.rs`'s `data_manager_footline` records), and on
    // every page it was invisible.
    let (status, rows_empty) = match page {
        0 => (&td.calendar_status, events.is_empty()),
        1 => (&equity.earnings_status, equity.earnings.is_empty()),
        2 => (&equity.dividends_status, equity.dividends.is_empty()),
        _ => (&equity.ipo_status, equity.ipos.is_empty()),
    };
    if let Some(line) = footer_status(rows_empty, status) {
        egui::Panel::bottom(ui.scope_id().with("calendar_status"))
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(space::NONE as i8, space::SM as i8)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(line)
                        .size(role_px(ui.ctx(), ThemeTextRole::Body))
                        .color(tokens.theme.text_tertiary),
                );
            });
    }

    if page == 0 {
        economic_table(ui, &events, week, high_only, selection, status, now_ms, full, flags);
    } else {
        // Earnings / Dividends / IPO equity tables (built below in a follow-on edit)
        calendar_equity_table(ui, equity, page, status);
    }
}

/// What an EMPTY page says in place of its rows: the feed's own status sentence (a refusal, an
/// unarmed lane, a missing key, a LIVE feed that returned nothing), or "Loading…" while the feed has
/// not answered at all. Never "Loading…" beside a status that already says why nothing is here.
pub(crate) fn empty_note(status: &str) -> &str {
    if status.is_empty() { "Loading…" } else { status }
}

/// The status line drawn at the page's foot: the feed's sentence when there are rows beside it.
/// An empty page carries the sentence in the table's own body ([`empty_note`]), so the foot says
/// nothing then — one sentence, shown once.
fn footer_status(rows_empty: bool, status: &str) -> Option<&str> {
    (!rows_empty && !status.is_empty()).then_some(status)
}

/// "YYYY-MM-DD" → "Monday, June 29" (day-group header).
pub(crate) fn calendar_day_label(iso: &str) -> String {
    use chrono::Datelike;
    chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .map(|d| format!("{}, {} {}", d.format("%A"), d.format("%B"), d.day()))
        .unwrap_or_else(|_| iso.to_string())
}

#[cfg(test)]
mod tests {
    use super::{calendar_day_label, empty_note, footer_status};

    #[test]
    fn day_label_formats_iso_dates() {
        // "YYYY-MM-DD" → "Weekday, Month D" (no zero-padded day)
        assert_eq!(calendar_day_label("2026-06-29"), "Monday, June 29");
        assert_eq!(calendar_day_label("2026-07-04"), "Saturday, July 4");
    }

    #[test]
    fn day_label_passes_unparsable_input_through() {
        assert_eq!(calendar_day_label("not-a-date"), "not-a-date");
        assert_eq!(calendar_day_label(""), "");
    }

    /// A page with no rows says WHY in its own body (the status sentence, or "Loading…" before the
    /// feed answered), and the foot stays silent so the sentence is not drawn twice; a page with
    /// rows carries the sentence at its foot.
    #[test]
    fn an_empty_page_carries_the_status_in_its_body_and_a_full_page_at_its_foot() {
        let refusal = "the datahub holds no FINNHUB_API_KEY";
        assert_eq!(empty_note(refusal), refusal);
        assert_eq!(empty_note(""), "Loading…");
        assert_eq!(footer_status(true, refusal), None);
        assert_eq!(footer_status(true, ""), None);
        assert_eq!(footer_status(false, ""), None);
        assert_eq!(footer_status(false, "3 rows · LIVE"), Some("3 rows · LIVE"));
    }
}
