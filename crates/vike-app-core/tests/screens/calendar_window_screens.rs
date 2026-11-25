//! **The Calendar window, read off real frames.** The owner's capture of 2026-11-16 (the week
//! Nov 16 — Nov 22, 2026) showed defects no test could see:
//!
//! 1. the day cards drew a category and its count on top of each other (`Economi24`, `Earning134`):
//!    a card's width was a share of the window clamped to a floor, and nothing compared it to the
//!    words it holds;
//! 2. the cards were tall and the window opened at the plain tool size, so the event table below
//!    them showed about two rows;
//! 3. the event list opened with `Sunday, November 15` (a New Zealand print at 23:30), the day BEFORE
//!    the week the range and the cards named: the list showed every event the feed held, and only the
//!    cards were cut to the week;
//! 4. on a narrow window the nav bar's High only, Countries and zone controls drew over the week's
//!    range.
//!
//! Each test renders `calendar_tool_content_at` at a pinned instant and zone and reads what was
//! painted: a `Shape::Text`'s galley says what it says (the whole word, even when the layout cut it
//! with an ellipsis), where it sits and whether it was elided, and the clip rect it was painted under
//! is the scroll area that holds it.

use chrono::NaiveDate;
use egui::epaint::{ClippedShape, Shape};
use egui::{Pos2, Rect, vec2};
use vike_app_core::ui::window::spawn::{WindowSpawnRequest, plan_window_spawn};
use vike_app_core::ui::window::{
    TOOL_BODY_MARGIN, WindowKind, WindowState, show_window, tool_title_bar,
};
use vike_app_core::ui::windows::WindowView;
use vike_app_core::ui::windows::calendar_tool_content_at;
use vike_calendar::CalendarState;
use vike_datahub_client::tool_feeds::CalendarEconomicEvent;
use vike_execution::CoreSnapshot;
use vike_ui_theme::appearance::{self, ThemeAppearance};
use vike_ui_theme::metrics::ThemeDensity;
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;
use vike_ui_theme::type_scale::ThemeTextSize;
use vike_ui_theme::value::calendar;

use crate::tool_context::{DENSITIES, Owned, WIDTHS};

/// The week's seven card titles, Monday first.
const CARDS: [&str; 7] = ["Mon 16", "Tue 17", "Wed 18", "Thu 19", "Fri 20", "Sat 21", "Sun 22"];
/// The categories a card counts, in the order it draws them.
const CATEGORIES: [&str; 4] = ["Economic", "Earnings", "Dividends", "IPO"];
/// One week's counts, Monday first: economic events, earnings, dividends, IPOs. The capture's
/// Monday and Tuesday, then a four-digit count, the widest a card has to hold.
const WEEK: [[usize; 4]; 7] = [
    [24, 134, 1, 0],
    [18, 77, 3, 1],
    [21, 1234, 12, 2],
    [26, 98, 7, 0],
    [19, 41, 2, 1],
    [0, 0, 0, 0],
    [12, 0, 0, 0],
];
/// The week's range, as the nav bar names it.
const RANGE: &str = "Nov 16 — Nov 22, 2026";
/// The title of the week's first event: the row the event table is found by.
const FIRST_EVENT: &str = "CPI m/m";
/// Texts on one line sit within a few points of each other.
const SAME_LINE: f32 = 6.0;
/// The capture's width (1098 pixels), as logical points at 100 % and at 125 %.
const CAPTURE_WIDTHS: [f32; 2] = [1098.0, 878.4];
/// A body narrower than seven cards: the strip scrolls sideways there.
const NARROW: f32 = 420.0;

/// One run of painted text: what it says, where it sits, whether the layout cut it short, and the
/// clip rect it was painted under.
struct Txt {
    text: String,
    rect: Rect,
    elided: bool,
    clip: Rect,
}

/// One frame: its texts, and the rectangles it painted (button frames, the check box, card frames).
struct Frame {
    texts: Vec<Txt>,
    boxes: Vec<Rect>,
}

/// 2026-11-`day` at `hour`:`minute` UTC, in epoch milliseconds.
fn utc(day: u32, hour: u32, minute: u32) -> i64 {
    NaiveDate::from_ymd_opt(2026, 11, day)
        .and_then(|d| d.and_hms_opt(hour, minute, 0))
        .expect("a November 2026 instant")
        .and_utc()
        .timestamp_millis()
}

/// The pinned now: Wednesday 2026-11-18 10:00 UTC, in the week of Monday the 16th in every zone
/// below.
fn now_ms() -> i64 {
    utc(18, 10, 0)
}

/// One RAW economic event as the poll holds it (the window derives its country, New Zealand, and
/// its flag where it draws).
fn event(timestamp_ms: i64) -> CalendarEconomicEvent {
    CalendarEconomicEvent {
        timestamp_ms,
        currency: "NZD".into(),
        title: "BusinessNZ Services Index".into(),
        impact: String::new(),
        forecast: String::new(),
        previous: String::new(),
        actual: String::new(),
    }
}

/// [`WEEK`] as a window's data: each day's economic events (a minute apart from 01:00 UTC, the
/// week's first titled [`FIRST_EVENT`]) and its equity counts.
fn week_owned() -> Owned {
    let mut owned = Owned::new(CoreSnapshot::empty("binance", "BTCUSDT"));
    for (day, counts) in (16..=22u32).zip(WEEK) {
        for minute in 0..counts[0] {
            let mut e = event(utc(day, 1, 0) + minute as i64 * 60_000);
            e.forecast = "0.3%".into();
            e.previous = "0.2%".into();
            if owned.tool_feeds.calendar.is_empty() {
                e.title = FIRST_EVENT.into();
            }
            owned.tool_feeds.calendar.push(e);
        }
        let week = owned.tool_feeds.calendar_weeks.entry("2026-11-16".into()).or_default();
        week.counts.insert(format!("2026-11-{day}"), (counts[1], counts[2], counts[3]));
    }
    owned.tool_feeds.calendar_status =
        format!("{} events · LIVE · 1 of 1 sources", owned.tool_feeds.calendar.len());
    owned
}

fn read(shapes: Vec<ClippedShape>) -> Frame {
    let mut frame = Frame { texts: Vec::new(), boxes: Vec::new() };
    for clipped in shapes {
        let clip = clipped.clip_rect;
        for shape in vike_ui_theme::painted::flatten([clipped]) {
            match shape {
                Shape::Text(t) => frame.texts.push(Txt {
                    text: t.galley.text().to_string(),
                    rect: t.galley.rect.translate(t.pos.to_vec2()),
                    elided: t.galley.elided,
                    clip,
                }),
                Shape::Rect(r) => frame.boxes.push(r.rect),
                _ => {}
            }
        }
    }
    frame
}

/// The window body alone over `owned` on `page`, `width` points wide at `ppp`, under `look`, every
/// date in `zone`, settled.
fn paint(
    owned: &Owned,
    look: &ThemeAppearance,
    size: (f32, f32),
    zone: ThemeDisplayTimeZone,
    page: u8,
) -> Frame {
    paint_view(
        owned,
        look,
        size,
        zone,
        WindowView {
            calendar: CalendarState { page, ..CalendarState::default() },
            ..WindowView::default()
        },
    )
}

/// [`paint`] with the window's whole view state `tv` (its page and its week).
fn paint_view(
    owned: &Owned,
    look: &ThemeAppearance,
    (width, ppp): (f32, f32),
    zone: ThemeDisplayTimeZone,
    mut tv: WindowView,
) -> Frame {
    let context = egui::Context::default();
    appearance::install(&context, look);
    let mut shapes = Vec::new();
    // Pass 0 lets the bundled fonts land (they apply on the NEXT pass); the last is the one read.
    for pass in 0..3u32 {
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1600.0))),
            time: Some(f64::from(pass) / 60.0),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point =
            Some(ppp);
        let mut full = context.run_ui(raw, |ui| {
            calendar_tool_content_at(ui, &owned.context(), &mut tv, zone, now_ms());
        });
        full.textures_delta.clear();
        shapes = std::mem::take(&mut full.shapes);
    }
    read(shapes)
}

/// One category row of a day card.
struct CardRow<'a> {
    card: &'static str,
    category: &'a Txt,
    count: &'a Txt,
    /// Where the next painted card's title starts (none: unbounded).
    next_card: f32,
}

/// Every category row of the PAINTED day cards with the count on its line in its card, in card
/// order. A strip wider than the window scrolls sideways: a card out of view paints nothing, and a
/// row whose count is out of view is not a row here.
fn card_rows(texts: &[Txt]) -> Vec<CardRow<'_>> {
    let titles: Vec<(&'static str, Rect)> = CARDS
        .iter()
        .filter_map(|card| {
            let found: Vec<&Txt> = texts.iter().filter(|t| t.text == *card).collect();
            assert!(found.len() <= 1, "the card {card:?} is painted once");
            found.first().map(|t| (*card, t.rect))
        })
        .collect();
    // The cards end above the category pills.
    let floor = texts
        .iter()
        .find(|t| t.text == "All categories")
        .expect("the pills row is painted")
        .rect
        .top();
    let mut rows = Vec::new();
    for label in
        texts.iter().filter(|t| CATEGORIES.contains(&t.text.as_str()) && t.rect.center().y < floor)
    {
        let Some(k) = titles.iter().rposition(|(_, r)| r.left() <= label.rect.left() + 0.5) else {
            continue; // under a card whose title is scrolled out of view
        };
        let next_card = titles.get(k + 1).map_or(f32::INFINITY, |(_, r)| r.left());
        let counts: Vec<&Txt> = texts
            .iter()
            .filter(|t| !t.text.is_empty() && t.text.chars().all(|c| c.is_ascii_digit()))
            .filter(|t| (t.rect.center().y - label.rect.center().y).abs() < SAME_LINE)
            .filter(|t| t.rect.left() >= titles[k].1.left() - 0.5 && t.rect.left() < next_card)
            .collect();
        match counts.as_slice() {
            [count] => rows.push(CardRow { card: titles[k].0, category: label, count, next_card }),
            [] => {} // the count is scrolled out of view
            _ => panic!(
                "{} {}: one count on its line, not {}",
                titles[k].0,
                label.text,
                counts.len()
            ),
        }
    }
    rows.sort_by_key(|r| CARDS.iter().position(|c| *c == r.card));
    rows
}

/// **A card's word is whole and its count stands clear of it, right-aligned**: at every text size,
/// density, width and pixel density, no word is elided, every word ends before its count begins,
/// the count stays inside its card, and one card's counts share one right edge.
#[test]
fn a_cards_word_is_whole_and_clear_of_its_right_aligned_count_at_every_size() {
    let owned = week_owned();
    let every_row = WEEK.iter().flatten().filter(|c| **c > 0).count();
    let widths: Vec<f32> = std::iter::once(NARROW).chain(CAPTURE_WIDTHS).chain(WIDTHS).collect();
    let mut failures = Vec::new();
    for text_size in ThemeTextSize::ALL {
        for density in ThemeDensity::ALL {
            let look = ThemeAppearance { text_size, density, ..ThemeAppearance::default() };
            for &width in &widths {
                for ppp in DENSITIES {
                    let at = format!("{text_size:?} {density:?} {width}pt @{ppp}x");
                    // The Earnings page: the cards count every feed, and no event list is drawn.
                    let frame = paint(&owned, &look, (width, ppp), ThemeDisplayTimeZone::Utc, 1);
                    let rows = card_rows(&frame.texts);
                    if width >= 1200.0 {
                        assert_eq!(rows.len(), every_row, "{at}: every non-zero count is a row");
                    } else {
                        assert!(rows.len() >= 3, "{at}: only {} card rows painted", rows.len());
                    }
                    for r in &rows {
                        let row = format!("{at} {} {} {}", r.card, r.category.text, r.count.text);
                        if r.category.elided {
                            failures.push(format!("{row}: the word was cut short"));
                        }
                        if r.category.rect.right() >= r.count.rect.left() {
                            failures.push(format!(
                                "{row}: the word {:?} meets the count {:?}",
                                r.category.rect, r.count.rect
                            ));
                        }
                        if r.count.rect.right() >= r.next_card {
                            failures.push(format!("{row}: the count runs into the next card"));
                        }
                    }
                    for r in &rows {
                        for other in rows.iter().filter(|o| o.card == r.card) {
                            if (r.count.rect.right() - other.count.rect.right()).abs() >= 0.5 {
                                failures.push(format!(
                                    "{at} {}: the counts {:?} and {:?} end at {} and {}",
                                    r.card,
                                    r.count.text,
                                    other.count.text,
                                    r.count.rect.right(),
                                    other.count.rect.right()
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// **Nothing is drawn over the week's range, at any width**: the nav bar puts High only,
/// Countries and the zone on a row of their own when one row cannot hold them beside the range,
/// and every one of them stays painted.
#[test]
fn the_nav_bar_draws_nothing_over_the_weeks_range_at_any_width() {
    let owned = week_owned();
    let mut failures = Vec::new();
    for text_size in ThemeTextSize::ALL {
        for density in ThemeDensity::ALL {
            let look = ThemeAppearance { text_size, density, ..ThemeAppearance::default() };
            for width in [320.0, NARROW, 560.0, 640.0, 760.0, CAPTURE_WIDTHS[1], 1200.0] {
                let at = format!("{text_size:?} {density:?} {width}pt");
                let frame = paint(&owned, &look, (width, 1.0), ThemeDisplayTimeZone::Utc, 0);
                let range = frame
                    .texts
                    .iter()
                    .find(|t| t.text == RANGE)
                    .unwrap_or_else(|| panic!("{at}: the range is painted"))
                    .rect;
                // Anything painted that meets the range and is not a background holding all of it.
                let over = frame
                    .texts
                    .iter()
                    .filter(|t| t.text != RANGE)
                    .map(|t| (t.text.as_str(), t.rect))
                    .chain(frame.boxes.iter().map(|b| ("a painted box", *b)))
                    .filter(|(_, r)| r.intersects(range) && !r.contains_rect(range))
                    .map(|(what, r)| {
                        format!("{at}: {what} at {r:?} is drawn over the range {range:?}")
                    });
                failures.extend(over);
                for control in ["High only", "Countries", ThemeDisplayTimeZone::Utc.label()] {
                    if !frame.texts.iter().any(|t| t.text == control) {
                        failures.push(format!("{at}: {control:?} is not painted"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// **The range, the cards and the list hold exactly the shown week's days, in the zone shown.**
/// An event at 23:30 UTC on the Sunday before the week and one at 23:30 UTC on its last day: in UTC
/// the first is outside the week and the second is its Sunday; in Tokyo (UTC+9) the first is the
/// week's Monday morning and the second is the NEXT Monday's. Neither zone lists a day outside the
/// week, the cards count the same days the list groups, and the now marker reads the zone's clock.
#[test]
fn the_list_and_the_cards_hold_only_the_shown_weeks_days_in_the_shown_zone() {
    let mut owned = Owned::new(CoreSnapshot::empty("binance", "BTCUSDT"));
    owned.tool_feeds.calendar =
        vec![event(utc(15, 23, 30)), event(utc(18, 12, 0)), event(utc(22, 23, 30))];
    let headings: Vec<String> = (15..=23u32)
        .map(|d| NaiveDate::from_ymd_opt(2026, 11, d).unwrap().format("%A, %B %-d").to_string())
        .collect();
    for (zone, cards, groups, now) in [
        (
            ThemeDisplayTimeZone::Utc,
            ["Wed 18", "Sun 22"],
            ["Wednesday, November 18", "Sunday, November 22"],
            "now  10:00",
        ),
        (
            ThemeDisplayTimeZone::parse("Asia/Tokyo"),
            ["Mon 16", "Wed 18"],
            ["Monday, November 16", "Wednesday, November 18"],
            "now  19:00",
        ),
    ] {
        let at = zone.label();
        let look = ThemeAppearance::default();
        let frame = paint(&owned, &look, (CAPTURE_WIDTHS[0], 1.0), zone, 0);
        let painted = |want: &str| frame.texts.iter().any(|t| t.text == want);
        assert!(painted(RANGE), "{at}: the range names the week");
        assert!(painted(zone.label()), "{at}: the zone button names the zone the dates are in");
        let listed: Vec<&str> =
            headings.iter().map(String::as_str).filter(|h| painted(h)).collect();
        assert_eq!(listed, groups, "{at}: the list's day groups");
        let rows = card_rows(&frame.texts);
        let counted: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| (r.card, r.category.text.as_str(), r.count.text.as_str()))
            .collect();
        let want: Vec<(&str, &str, &str)> = cards.iter().map(|c| (*c, "Economic", "1")).collect();
        assert_eq!(counted, want, "{at}: the cards count the days the list groups");
        assert!(painted(now), "{at}: the now marker reads {now:?}");
    }
}

/// The window as `crates/vike-desktop/src/app_ui/tool_pane.rs` draws it (title bar, body margin,
/// `WindowBodyBounds`) at `w`'s geometry, settled: the rect the event table's rows are clipped to (its
/// scroll area), and the window's rect.
fn window_pass(owned: &Owned, w: &mut WindowState) -> (Rect, Rect) {
    let context = egui::Context::default();
    appearance::install(&context, &ThemeAppearance::default());
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0));
    let mut tv = WindowView::default();
    let mut shapes = Vec::new();
    for pass in 0..4u32 {
        context.begin_pass(egui::RawInput {
            screen_rect: Some(screen),
            time: Some(f64::from(pass) / 60.0),
            ..Default::default()
        });
        let _ = show_window(&context, w, screen, |ui, bounds| {
            let _ = tool_title_bar(ui, WindowKind::Calendar, false);
            egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                bounds.show(ui, |ui| {
                    calendar_tool_content_at(
                        ui,
                        &owned.context(),
                        &mut tv,
                        ThemeDisplayTimeZone::Utc,
                        now_ms(),
                    );
                });
            });
            egui::Vec2::ZERO
        });
        let mut full = context.end_pass();
        full.textures_delta.clear();
        shapes = std::mem::take(&mut full.shapes);
    }
    let table = read(shapes)
        .texts
        .into_iter()
        .find(|t| t.text == FIRST_EVENT)
        .expect("the week's first event is painted")
        .clip;
    (table, Rect::from_min_size(w.screen_position, w.size))
}

/// The Calendar opens at its own size with a page of events below compact cards, and every point
/// the window is dragged taller goes to the event table.
#[test]
fn the_window_opens_on_a_page_of_events_and_the_table_takes_what_the_window_gains() {
    let owned = week_owned();
    let request = WindowSpawnRequest::Kind {
        kind: WindowKind::Calendar,
        polymarket_seed_token: String::new(),
        data_destination_seed: None,
    };
    let mut w = plan_window_spawn(request, Pos2::ZERO, 0, vec2(600.0, 700.0), None)
        .window
        .expect("the launcher opens a Calendar window");
    let (table, window) = window_pass(&owned, &mut w);
    let rows = table.height() / calendar::ROW_HEIGHT;
    assert!(
        rows >= 15.0,
        "a {:?} window shows {rows:.1} event rows ({table:?} in {window:?})",
        window.size()
    );
    assert!(
        table.top() - window.top() < window.height() / 2.0,
        "the cards push the event table below the window's middle: {table:?} in {window:?}"
    );

    let grow = 240.0;
    w.size.y += grow;
    w.user_sized = true; // dragged by its edge: the window keeps the size it was given
    let (taller, taller_window) = window_pass(&owned, &mut w);
    assert!((taller_window.height() - window.height() - grow).abs() < 0.5, "{taller_window:?}");
    assert!(
        (taller.height() - table.height() - grow).abs() < 1.0,
        "the window grew {grow} points and the event table {:.1} ({table:?} -> {taller:?})",
        taller.height() - table.height()
    );
}

/// **Today, ← and → move the week, clicked where the owner clicks them.** All three were drawn
/// with their responses thrown away (`let _ = ui.button(..)`) over a week fixed to now, so the
/// arrows did nothing (the owner's report of 2026-11-17). Clicked as pointer presses at each
/// button's centre in a Calendar window, a wide one and a narrow one whose nav bar wraps: each arrow
/// moves the range and the cards seven days, and Today brings them back.
#[test]
fn the_arrows_move_the_week_seven_days_and_today_brings_it_back() {
    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::NodeT as _;

    type State = (WindowState, WindowView);
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0));
    for width in [1100.0, NARROW + 40.0] {
        let owned = week_owned();
        let request = WindowSpawnRequest::Kind {
            kind: WindowKind::Calendar,
            polymarket_seed_token: String::new(),
            data_destination_seed: None,
        };
        let mut w = plan_window_spawn(request, Pos2::ZERO, 0, vec2(600.0, 700.0), None)
            .window
            .expect("the launcher opens a Calendar window");
        w.size.x = width;
        let mut h = Harness::builder().with_size(screen.size()).build_ui_state(
            move |ui, (w, tv): &mut State| {
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                let context = ui.ctx().clone();
                let _ = show_window(&context, w, screen, |ui, bounds| {
                    let _ = tool_title_bar(ui, WindowKind::Calendar, false);
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                        bounds.show(ui, |ui| {
                            calendar_tool_content_at(
                                ui,
                                &owned.context(),
                                tv,
                                ThemeDisplayTimeZone::Utc,
                                now_ms(),
                            );
                        });
                    });
                    egui::Vec2::ZERO
                });
            },
            (w, WindowView::default()),
        );
        h.run_steps(4);
        let shows = |h: &Harness<'_, State>, range: &str, monday: &str| {
            let texts = vike_ui_theme::harness::tree_node_texts(h);
            assert!(
                texts.lines().any(|t| t == range) && texts.lines().any(|t| t == monday),
                "a {width}pt window shows {range} from {monday}:\n{texts}"
            );
        };
        let click = |h: &mut Harness<'_, State>, name: &str| {
            {
                // The nav bar's unseen measuring pass draws a disabled twin of each button.
                let live: Vec<_> = vike_ui_theme::harness::nodes(h, Role::Button, name)
                    .into_iter()
                    .filter(|n| !n.accesskit_node().is_disabled())
                    .collect();
                assert_eq!(live.len(), 1, "one live {name} button in a {width}pt window");
                live[0].click();
            }
            h.run_steps(3);
        };

        shows(&h, RANGE, "Mon 16");
        click(&mut h, "Later");
        shows(&h, "Nov 23 — Nov 29, 2026", "Mon 23");
        click(&mut h, "Later");
        shows(&h, "Nov 30 — Dec 6, 2026", "Mon 30");
        click(&mut h, "Today");
        shows(&h, RANGE, "Mon 16");
        click(&mut h, "Earlier");
        shows(&h, "Nov 9 — Nov 15, 2026", "Mon 9");
        click(&mut h, "Later");
        shows(&h, RANGE, "Mon 16");
    }
}

/// **Two Calendar windows on two weeks, over the one shared data, each draw their own week.** The
/// equity rows were one week's for the whole app, so a second window moved to the next week
/// showed the first one's counts (or the first showed the second's). The window on this week
/// counts this week's four-digit Wednesday; the one on the next week counts only its own.
#[test]
fn two_windows_on_two_weeks_each_draw_their_own_weeks_counts() {
    const NEXT_WEDNESDAY: &str = "777";
    let mut owned = week_owned();
    let next = owned.tool_feeds.calendar_weeks.entry("2026-11-23".into()).or_default();
    next.counts.insert("2026-11-25".into(), (777, 0, 0));
    let look = ThemeAppearance::default();
    let counts = |weeks_from_now: i32| -> Vec<String> {
        let tv = WindowView {
            calendar: CalendarState { weeks_from_now, ..CalendarState::default() },
            ..WindowView::default()
        };
        let frame =
            paint_view(&owned, &look, (CAPTURE_WIDTHS[0], 1.0), ThemeDisplayTimeZone::Utc, tv);
        frame.texts.into_iter().map(|t| t.text).collect()
    };
    let (this, next) = (counts(0), counts(1));
    assert!(
        this.iter().any(|t| t == "1234") && !this.iter().any(|t| t == NEXT_WEDNESDAY),
        "the window on this week draws this week's counts: {this:?}"
    );
    assert!(
        next.iter().any(|t| t == NEXT_WEDNESDAY) && !next.iter().any(|t| t == "1234"),
        "the window on the next week draws only its own: {next:?}"
    );
}
