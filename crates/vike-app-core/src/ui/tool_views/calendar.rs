//! The Calendar tool body — the 7-day economic/earnings day strip, the ForexFactory economic
//! table (drawn flags + impact glyphs) and the Nasdaq equity tables (Earnings/Dividends/IPO),
//! with its private helpers (`draw_flag`/`draw_impact`/`cal_day_label`/`cal_equity_table`).
//! Moved verbatim from `vike-app`'s `main.rs` (tool-view extraction batch 1).

use super::ToolCtx;
use crate::tools::{self, Importance};
use vike_ui_theme::components::{Status, Tokens, role_px};
use vike_ui_theme::maps::{self, MapRow};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::side::Pair;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::calendar;
use vike_ui_theme::{font, icons};

/// The Calendar tool body: the 7-day economic/earnings day strip + the per-page tables (flag
/// PNGs from `flags`). Split out of `tool_content`'s match arm verbatim (dedup-app refactor).
pub fn calendar_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::RichText;
    use egui::{Align, Color32, Layout, vec2};
    let tok = Tokens::of(ui.ctx());
    let (td, flags) = (ctx.td, ctx.flags);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let num = |s: &str| -> Option<f64> {
        let t: String =
            s.trim().chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
        t.parse::<f64>().ok()
    };
    // capture the FULL panel width ONCE up front — reading available_width() again lower
    // down (after the cards/pills rows) under-reports it, so cards + table share this.
    let full = ui.available_width();
    // per-window view state (read this frame; widgets below mutate tv for next frame)
    let high_only = tv.cal_high_only;
    let page = tv.cal_page;
    let sel_day = tv.cal_selected_day;
    const PAGES: [&str; 4] = ["Economic", "Earnings", "Dividends", "IPO"];

    // ---- nav bar: Today  ‹ ›  range ........  High only  Countries  Local ----
    ui.horizontal(|ui| {
        // vike's nav controls are taller/roomier than egui's default — bump the padding
        ui.spacing_mut().button_padding = vec2(space::XL, space::LG);
        ui.spacing_mut().item_spacing.x = space::MD;
        let _ = ui.button("Today");
        let _ = icons::named(ui.button(icons::EARLIER), "Earlier");
        let _ = icons::named(ui.button(icons::LATER), "Later");
        ui.add_space(space::MD);
        ui.label(
            font::semibold(&td.cal_range)
                .size(role_px(ui.ctx(), TextRole::Title))
                .color(tok.theme.text),
        ); // Py 16px/600
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let _ = ui.add(egui::Button::new("Local").min_size(vec2(calendar::LOCAL_BTN_W, 0.0)));
            let _ = ui
                .add(egui::Button::new("Countries").min_size(vec2(calendar::COUNTRIES_BTN_W, 0.0)));
            ui.add_space(space::SM);
            ui.checkbox(&mut tv.cal_high_only, RichText::new("High only").color(tok.theme.text2));
        });
    });
    ui.add_space(space::MD);

    // ---- day-strip cards: FIXED 7-day Mon→Sun week (vike CalendarSpace), each with
    // Economic (ForexFactory) + Earnings/Dividends/IPO (Nasdaq) counts; zero rows hidden.
    {
        use chrono::{Datelike, Duration, Local};
        const WD3: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        const CATS: [&str; 4] = ["Economic", "Earnings", "Dividends", "IPO"];
        let today = Local::now().date_naive(); // Local week — matches event bucketing + Python
        let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
        let mut week: Vec<(String, [usize; 4])> = Vec::with_capacity(7);
        for i in 0..7 {
            let d = monday + Duration::days(i);
            let iso = d.format("%Y-%m-%d").to_string();
            let econ = td
                .calendar
                .iter()
                .filter(|e| e.date_iso == iso && (!high_only || e.importance == Importance::High))
                .count();
            let (earn, div, ipo) = td.cal_equity.get(&iso).copied().unwrap_or((0, 0, 0));
            week.push((format!("{} {}", WD3[i as usize], d.day()), [econ, earn, div, ipo]));
        }
        let gap = space::LG;
        let cw = ((full - gap * 6.0) / 7.0).clamp(calendar::CARD_MIN_W, calendar::CARD_MAX_W);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, (label, counts)) in week.iter().enumerate() {
                let on = i as i8 == sel_day; // selected card → HOVER fill + ACCENT border (title stays text)
                let inner = egui::Frame::new()
                    .fill(if on { tok.theme.hover } else { tok.theme.surface })
                    .stroke(egui::Stroke::new(
                        stroke::HAIRLINE,
                        if on { tok.theme.accent } else { tok.theme.border },
                    ))
                    .corner_radius(egui::CornerRadius::same(RADIUS))
                    .inner_margin(egui::Margin::symmetric(space::XL as i8, space::LG as i8))
                    .show(ui, |ui| {
                        // force VERTICAL — the Frame inherits the cards-row left_to_right
                        // layout otherwise, so title + rows flow sideways and overlap.
                        ui.vertical(|ui| {
                            ui.set_width(cw - 2.0 * space::XL);
                            ui.set_min_height(calendar::CARD_MIN_H);
                            ui.label(
                                font::semibold(label)
                                    .size(role_px(ui.ctx(), TextRole::Title))
                                    .color(tok.theme.text),
                            ); // Py 13/600
                            ui.add_space(space::SM);
                            for (ci, cnt) in counts.iter().enumerate() {
                                if *cnt == 0 {
                                    continue; // hide zero rows (vike _DayCard.set_counts)
                                }
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(CATS[ci])
                                            .size(role_px(ui.ctx(), TextRole::Strong))
                                            .color(tok.theme.text2),
                                    ); // 12/400
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(
                                            RichText::new(cnt.to_string())
                                                .monospace()
                                                .size(role_px(ui.ctx(), TextRole::Strong))
                                                .color(tok.theme.text),
                                        ); // 12/400, a count is a number: mono
                                    });
                                });
                            }
                        });
                    });
                let r = ui.interact(
                    inner.response.rect,
                    egui::Id::new(("cal_card", i)),
                    egui::Sense::click(),
                );
                if r.clicked() {
                    // toggle: click the selected card again to clear
                    tv.cal_selected_day = if on { -1 } else { i as i8 };
                    tv.cal_page = 0; // selecting a day jumps to the Economic page
                }
            }
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
                        .size(role_px(ui.ctx(), TextRole::Strong))
                        .color(if active { tok.theme.text } else { tok.theme.text3 }),
                )
                .clicked()
            {
                tv.cal_page = i as u8;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let _ = ui.button("All categories"); // (Py hides the data-source status here)
        });
    });
    ui.add_space(space::XS);

    // selected day-card → filter the Economic table to that ISO date
    let sel_iso: Option<String> = if sel_day >= 0 {
        use chrono::{Datelike, Duration, Local};
        let today = Local::now().date_naive();
        let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
        Some((monday + Duration::days(sel_day as i64)).format("%Y-%m-%d").to_string())
    } else {
        None
    };

    if page == 0 {
        // ---- column widths: Time/Country/impact fixed; Actual/Forecast/Prior scale with
        // width and right-align (vike _NUM_COL_W=210 in the full-width tab); Event stretches.
        // (uses the `full` captured at the top — local available_width() under-reports here.)
        let (w_time, w_ctry, w_imp) =
            (calendar::COL_TIME_W, calendar::COL_COUNTRY_W, calendar::COL_IMPACT_W);
        // vike _NUM_COL_W = 210 fixed when the panel is wide enough; scale down on narrow ones.
        let w_num = if full >= calendar::NUM_COL_WIDE_AT {
            calendar::NUM_COL_W
        } else {
            (full * calendar::NUM_COL_FRAC).clamp(calendar::NUM_COL_MIN_W, calendar::NUM_COL_W)
        };
        let (w_act, w_fc, w_pr) = (w_num, w_num, w_num);
        // right margin = vike _RIGHT_PAD(40) + scrollbar clearance, so Prior doesn't collide
        // with the window edge / vertical scrollbar.
        let w_fixed = w_time + w_ctry + w_imp + w_act + w_fc + w_pr;
        let w_evt = (full - w_fixed - calendar::RIGHT_PAD).max(calendar::EVENT_MIN_W);
        let hcell = |ui: &mut egui::Ui, w: f32, t: &str, right: bool| {
            let lay = if right {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            ui.allocate_ui_with_layout(vec2(w, calendar::HEADER_H), lay, |ui| {
                ui.set_min_width(w); // reserve the FULL column width (else it collapses to text)
                ui.add(
                    egui::Label::new(
                        RichText::new(t)
                            .size(role_px(ui.ctx(), TextRole::Strong))
                            .color(tok.theme.text3),
                    )
                    .truncate(),
                );
                // Py header 12px TEXT3
            });
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::NONE; // columns land exactly on computed widths
            hcell(ui, w_time, "Time", false);
            hcell(ui, w_ctry + w_imp, "Country", false);
            hcell(ui, w_evt, "Event", false);
            hcell(ui, w_act, "Actual", true);
            hcell(ui, w_fc, "Forecast", true);
            hcell(ui, w_pr, "Prior", true);
        });
        ui.painter().hline(
            ui.min_rect().x_range(),
            ui.cursor().top(),
            egui::Stroke::new(stroke::HAIRLINE, tok.theme.border),
        );

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if td.calendar.is_empty() {
                ui.weak("loading…");
            }
            let mut cur_day = String::new();
            let (mut prev_time, mut prev_ctry) = (String::new(), String::new());
            let mut marker_done = false;
            for e in td.calendar.iter().filter(|e| {
                (!high_only || e.importance == Importance::High)
                    && sel_iso.as_ref().is_none_or(|s| s == &e.date_iso)
            }) {
                if e.day != cur_day {
                    cur_day = e.day.clone();
                    prev_time.clear();
                    prev_ctry.clear();
                    ui.add_space(space::MD);
                    ui.label(
                        font::semibold(&cur_day)
                            .size(role_px(ui.ctx(), TextRole::Title))
                            .color(tok.theme.text),
                    );
                    // Py day-group bold/13 (700 folds into 600: `reading-heading`)
                }
                // red "● now HH:MM" marker before the first future event
                if !marker_done && e.ts_ms > now_ms {
                    marker_done = true;
                    let hhmm = chrono::DateTime::from_timestamp_millis(now_ms)
                        .map(|d| d.format("%H:%M").to_string())
                        .unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.colored_label(
                            Status::Error.color(),
                            RichText::new("●").size(role_px(ui.ctx(), TextRole::Strong)),
                        );
                        ui.label(
                            RichText::new(format!("now  {hhmm}")).color(Status::Error.color()),
                        );
                    });
                }
                let show_time = e.time != prev_time;
                let show_ctry = e.country != prev_ctry;
                prev_time = e.time.clone();
                prev_ctry = e.country.clone();
                let dash = |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
                // Actual: countdown (red) for a future event with no print, else beat/miss color
                let future = e.actual.is_empty() && e.ts_ms > now_ms;
                let (act_txt, act_col) = if future {
                    let d = (e.ts_ms - now_ms).max(0) / 1000;
                    (
                        format!("Coming in {}:{:02}:{:02}", d / 3600, (d / 60) % 60, d % 60),
                        Status::Error.color(),
                    )
                } else if e.actual.is_empty() {
                    ("—".to_string(), tok.theme.text)
                } else {
                    let col = match (num(&e.actual), num(&e.forecast)) {
                        (Some(a), Some(f)) if a > f => Pair::ForecastAboveBelow.text(true, &tok),
                        (Some(a), Some(f)) if a < f => Pair::ForecastAboveBelow.text(false, &tok),
                        _ => tok.theme.text,
                    };
                    (e.actual.clone(), col)
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::NONE; // align rows to the header columns
                    // TIME (mono)
                    ui.allocate_ui_with_layout(
                        vec2(w_time, calendar::ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_time);
                            if show_time {
                                ui.label(
                                    RichText::new(&e.time)
                                        .monospace()
                                        .size(role_px(ui.ctx(), TextRole::Title))
                                        .color(tok.theme.text3),
                                );
                            }
                        },
                    );
                    // COUNTRY: flag + name + impact glyph
                    ui.allocate_ui_with_layout(
                        vec2(w_ctry, calendar::ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_ctry);
                            if show_ctry {
                                let (fr, _) = ui
                                    .allocate_exact_size(calendar::FLAG_SIZE, egui::Sense::hover());
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
                                    &e.country
                                } else {
                                    &e.country_name
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(nm).color(tok.theme.text))
                                        .truncate(),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        vec2(w_imp, calendar::ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_imp);
                            let (ir, _) =
                                ui.allocate_exact_size(calendar::IMPACT_SIZE, egui::Sense::hover());
                            draw_impact(ui.painter(), &tok, ir, e.importance);
                        },
                    );
                    // EVENT
                    ui.allocate_ui_with_layout(
                        vec2(w_evt, calendar::ROW_H),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_evt);
                            ui.add(
                                egui::Label::new(RichText::new(&e.title).color(tok.theme.text))
                                    .truncate(),
                            );
                        },
                    );
                    // ACTUAL / FORECAST / PRIOR (mono, right-aligned)
                    let rcell = |ui: &mut egui::Ui, w: f32, t: String, c: Color32| {
                        ui.allocate_ui_with_layout(
                            vec2(w, calendar::ROW_H),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.set_min_width(w);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(t)
                                            .monospace()
                                            .size(role_px(ui.ctx(), TextRole::Title))
                                            .color(c),
                                    )
                                    .truncate(),
                                );
                            },
                        );
                    };
                    rcell(ui, w_act, act_txt, act_col);
                    rcell(ui, w_fc, dash(&e.forecast), tok.theme.text);
                    rcell(ui, w_pr, dash(&e.previous), tok.theme.text);
                });
            }
        });
    } else {
        // Earnings / Dividends / IPO equity tables (built below in a follow-on edit)
        cal_equity_table(ui, td, page);
    }
    ui.add_space(space::SM);
    if !td.cal_status.is_empty() {
        ui.label(
            RichText::new(&td.cal_status)
                .size(role_px(ui.ctx(), TextRole::Body))
                .color(tok.theme.text3),
        );
    }
}

/// Draw a small recognizable flag for `iso` (lowercase alpha-2) into `r`. Ported from
/// economic_calendar.py `_FLAG_BANDS` + `_FLAG_SPECIAL` (drawn, no PNG assets).
fn draw_flag(p: &egui::Painter, r: egui::Rect, iso: &str) {
    use egui::{Color32 as C, Rect, pos2};
    let white = calendar::FLAG_WHITE;
    let navy = calendar::FLAG_NAVY;
    let red = calendar::FLAG_RED;
    let hb = |cols: &[C]| {
        let n = cols.len() as f32;
        for (i, c) in cols.iter().enumerate() {
            let y0 = r.top() + r.height() * i as f32 / n;
            let y1 = r.top() + r.height() * (i as f32 + 1.0) / n;
            p.rect_filled(Rect::from_min_max(pos2(r.left(), y0), pos2(r.right(), y1)), 0.0, *c);
        }
    };
    let vb = |cols: &[C]| {
        let n = cols.len() as f32;
        for (i, c) in cols.iter().enumerate() {
            let x0 = r.left() + r.width() * i as f32 / n;
            let x1 = r.left() + r.width() * (i as f32 + 1.0) / n;
            p.rect_filled(Rect::from_min_max(pos2(x0, r.top()), pos2(x1, r.bottom())), 0.0, *c);
        }
    };
    match iso {
        "de" => hb(&[calendar::FLAG_DE_BLACK, calendar::FLAG_DE_RED, calendar::FLAG_DE_GOLD]),
        "fr" => vb(&[calendar::FLAG_FR_BLUE, white, calendar::FLAG_FR_RED]),
        "it" => vb(&[calendar::FLAG_IT_GREEN, white, calendar::FLAG_IT_RED]),
        "in" => hb(&[calendar::FLAG_IN_SAFFRON, white, calendar::FLAG_IN_GREEN]),
        "ru" => hb(&[white, calendar::FLAG_RU_BLUE, calendar::FLAG_RU_RED]),
        "mx" => vb(&[calendar::FLAG_MX_GREEN, white, calendar::FLAG_MX_RED]),
        "ca" => vb(&[red, white, red]),
        "id" => hb(&[calendar::FLAG_ID_RED, white]),
        "sg" => hb(&[calendar::FLAG_SG_RED, white]),
        "za" => hb(&[calendar::FLAG_ZA_GREEN, white, calendar::FLAG_ZA_RED]),
        "cn" => {
            p.rect_filled(r, 0.0, calendar::FLAG_CN_RED);
        }
        "sa" => {
            p.rect_filled(r, 0.0, calendar::FLAG_SA_GREEN);
        }
        "hk" => {
            p.rect_filled(r, 0.0, calendar::FLAG_CN_RED);
        }
        "tr" => {
            p.rect_filled(r, 0.0, calendar::FLAG_TR_RED);
        }
        "au" | "nz" => {
            p.rect_filled(r, 0.0, navy);
        }
        "eu" => {
            p.rect_filled(r, 0.0, calendar::FLAG_EU_BLUE);
            p.circle_filled(r.center(), calendar::FLAG_EU_DOT_R, calendar::FLAG_EU_GOLD); // vike _flag_eu dot
        }
        "us" => {
            for i in 0..13 {
                let c = if i % 2 == 0 { red } else { white };
                let y0 = r.top() + r.height() * i as f32 / 13.0;
                let y1 = r.top() + r.height() * (i as f32 + 1.0) / 13.0;
                p.rect_filled(Rect::from_min_max(pos2(r.left(), y0), pos2(r.right(), y1)), 0.0, c);
            }
            let canton = Rect::from_min_max(
                r.min,
                pos2(r.left() + r.width() * 0.42, r.top() + r.height() * 0.54),
            );
            p.rect_filled(canton, 0.0, navy);
        }
        "jp" => {
            p.rect_filled(r, 0.0, white);
            p.circle_filled(r.center(), r.height() * 0.30, calendar::FLAG_JP_RED);
        }
        "gb" => {
            // Union Jack — vike _flag_gb: navy + WHITE diagonals (corner-to-corner) + white cross,
            // then a thinner red cross. The diagonals are what make it read as the UK, not a Nordic flag.
            p.rect_filled(r, 0.0, navy);
            let (cx, cy) = (r.center().x, r.center().y);
            let ws = egui::Stroke::new(stroke::EDGE, white);
            p.line_segment([r.left_top(), r.right_bottom()], ws);
            p.line_segment([r.left_bottom(), r.right_top()], ws);
            p.line_segment([pos2(cx, r.top()), pos2(cx, r.bottom())], ws);
            p.line_segment([pos2(r.left(), cy), pos2(r.right(), cy)], ws);
            let rs = egui::Stroke::new(stroke::HAIRLINE, red);
            p.line_segment([pos2(cx, r.top()), pos2(cx, r.bottom())], rs);
            p.line_segment([pos2(r.left(), cy), pos2(r.right(), cy)], rs);
        }
        "ch" => {
            p.rect_filled(r, 0.0, red);
            let (cx, cy) = (r.center().x, r.center().y);
            p.rect_filled(
                Rect::from_min_max(pos2(cx - 1.4, r.top() + 3.0), pos2(cx + 1.4, r.bottom() - 3.0)),
                0.0,
                white,
            );
            p.rect_filled(
                Rect::from_min_max(pos2(r.left() + 4.0, cy - 1.4), pos2(r.right() - 4.0, cy + 1.4)),
                0.0,
                white,
            );
        }
        _ => {
            p.rect_filled(r, 0.0, calendar::FLAG_UNKNOWN);
        }
    }
}

impl Importance {
    /// The row of the `importance` map this importance is drawn by.
    fn row(self) -> &'static MapRow {
        match self {
            Importance::High => &maps::importance::HIGH,
            Importance::Medium => &maps::importance::MEDIUM,
            Importance::Other => &maps::importance::OTHER,
        }
    }
}

/// The 3-bar importance glyph (heights 5/9/13 in an 18×14 box). The importance's row of the
/// `importance` map says how many bars are lit (`count`) and in what colour (`colour`), and what
/// the rest dim to (`fill`). Ported from calendar_delegate.py.
fn draw_impact(p: &egui::Painter, t: &Tokens, r: egui::Rect, importance: Importance) {
    use egui::{Rect, pos2};
    let row = importance.row();
    let (lit, dim) = (row.colour.resolve(t), row.fill.resolve(t));
    let lit_bars = usize::from(row.count.unwrap_or(0));
    let gx = r.center().x - 9.0;
    let gy = r.center().y - 7.0;
    for (i, bh) in [5.0_f32, 9.0, 13.0].iter().enumerate() {
        let x = gx + 1.0 + i as f32 * 6.0;
        let c = if i < lit_bars { lit } else { dim };
        p.rect_filled(
            Rect::from_min_max(pos2(x, gy + 14.0 - bh), pos2(x + 4.0, gy + 14.0)),
            0.0,
            c,
        );
    }
}

/// "YYYY-MM-DD" → "Monday, June 29" (day-group header).
fn cal_day_label(iso: &str) -> String {
    use chrono::Datelike;
    chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .map(|d| format!("{}, {} {}", d.format("%A"), d.format("%B"), d.day()))
        .unwrap_or_else(|_| iso.to_string())
}

/// Column model for the Calendar equity tables (Earnings/Dividends/IPO), factored out of
/// `cal_equity_table`'s local `let` to satisfy `clippy::type_complexity`: headers, per-column
/// weights, right-align flags, and the rows (each a `(date, cells[(text, color)])`).
type CalEquityTable =
    (Vec<&'static str>, Vec<f32>, Vec<bool>, Vec<(String, Vec<(String, egui::Color32)>)>);

/// The Earnings/Dividends/IPO equity calendar tables (Calendar pages 1-3), grouped by day.
fn cal_equity_table(ui: &mut egui::Ui, td: &tools::ToolData, page: u8) {
    use egui::{Align, Layout, RichText, vec2};
    let t = Tokens::of(ui.ctx());
    let dash = |s: &str| if s.trim().is_empty() { "—".to_string() } else { s.to_string() };
    let full = ui.available_width();

    // (headers, weights, right-align flags, rows[(date, cells[(text,color)])])
    let (headers, weights, rights, mut rows): CalEquityTable = match page {
        1 => {
            let rows = td
                .cal_earnings
                .iter()
                .map(|r| {
                    let sc = if r.surprise.starts_with('+') {
                        Pair::PositiveNegative.text(true, &t)
                    } else if r.surprise.starts_with('-') {
                        Pair::PositiveNegative.text(false, &t)
                    } else {
                        t.theme.text
                    };
                    (
                        r.date.clone(),
                        vec![
                            (r.hour.clone(), t.theme.text2),
                            (r.symbol.clone(), t.theme.text),
                            (dash(&r.eps_est), t.theme.text),
                            (dash(&r.eps_act), t.theme.text),
                            (dash(&r.surprise), sc),
                        ],
                    )
                })
                .collect();
            (
                vec!["Time", "Symbol", "EPS est.", "EPS act.", "Surprise"],
                vec![
                    calendar::EARN_WEIGHT_TIME,
                    calendar::EARN_WEIGHT_SYMBOL,
                    calendar::EARN_WEIGHT_EPS_EST,
                    calendar::EARN_WEIGHT_EPS_ACT,
                    calendar::EARN_WEIGHT_SURPRISE,
                ],
                vec![false, false, true, true, true],
                rows,
            )
        }
        2 => {
            let rows = td
                .cal_dividends
                .iter()
                .map(|r| {
                    (
                        r.date.clone(),
                        vec![
                            (r.symbol.clone(), t.theme.text),
                            (dash(&r.date), t.theme.text),
                            (dash(&r.pay_date), t.theme.text),
                            (dash(&r.amount), t.theme.text),
                            (dash(&r.yield_pct), t.theme.text),
                            (dash(&r.freq), t.theme.text2),
                        ],
                    )
                })
                .collect();
            (
                vec!["Symbol", "Ex-date", "Pay date", "Amount", "Yield", "Freq"],
                vec![
                    calendar::DIV_WEIGHT_SYMBOL,
                    calendar::DIV_WEIGHT_EX_DATE,
                    calendar::DIV_WEIGHT_PAY_DATE,
                    calendar::DIV_WEIGHT_AMOUNT,
                    calendar::DIV_WEIGHT_YIELD,
                    calendar::DIV_WEIGHT_FREQ,
                ],
                vec![false, false, false, true, true, false],
                rows,
            )
        }
        _ => {
            let rows = td
                .cal_ipos
                .iter()
                .map(|r| {
                    (
                        r.date.clone(),
                        vec![
                            (r.symbol.clone(), t.theme.text),
                            (dash(&r.company), t.theme.text),
                            (dash(&r.exchange), t.theme.text2),
                            (dash(&r.price), t.theme.text),
                            (dash(&r.shares), t.theme.text),
                            (dash(&r.status), t.theme.text2),
                        ],
                    )
                })
                .collect();
            (
                vec!["Symbol", "Company", "Exchange", "Price", "Shares", "Status"],
                vec![
                    calendar::IPO_WEIGHT_SYMBOL,
                    calendar::IPO_WEIGHT_COMPANY,
                    calendar::IPO_WEIGHT_EXCHANGE,
                    calendar::IPO_WEIGHT_PRICE,
                    calendar::IPO_WEIGHT_SHARES,
                    calendar::IPO_WEIGHT_STATUS,
                ],
                vec![false, false, false, true, true, false],
                rows,
            )
        }
    };
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let total_w: f32 = weights.iter().sum();
    let unit = (full - calendar::EQUITY_RIGHT_PAD).max(1.0) / total_w;
    let widths: Vec<f32> = weights.iter().map(|w| w * unit).collect();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::NONE;
        for (i, h) in headers.iter().enumerate() {
            let lay = if rights[i] {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            ui.allocate_ui_with_layout(vec2(widths[i], calendar::HEADER_H), lay, |ui| {
                ui.set_min_width(widths[i]);
                if rights[i] {
                    ui.add_space(space::LG); // a right-aligned column keeps a gap from the left-aligned one after it
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(*h)
                            .size(role_px(ui.ctx(), TextRole::Strong))
                            .color(t.theme.text3),
                    )
                    .truncate(),
                );
            });
        }
    });
    ui.painter().hline(
        ui.min_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(stroke::HAIRLINE, t.theme.border),
    );

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("cal_equity").show(ui, |ui| {
        if rows.is_empty() {
            ui.add_space(space::LG);
            ui.weak("Loading…");
            return;
        }
        let mut cur = String::new();
        for (date, cells) in &rows {
            if *date != cur {
                cur = date.clone();
                ui.add_space(space::MD);
                ui.label(
                    font::semibold(cal_day_label(date))
                        .size(role_px(ui.ctx(), TextRole::Title))
                        .color(t.theme.text),
                );
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::NONE;
                for (i, (txt, col)) in cells.iter().enumerate() {
                    let lay = if rights[i] {
                        Layout::right_to_left(Align::Center)
                    } else {
                        Layout::left_to_right(Align::Center)
                    };
                    ui.allocate_ui_with_layout(vec2(widths[i], calendar::ROW_H), lay, |ui| {
                        ui.set_min_width(widths[i]);
                        if rights[i] {
                            ui.add_space(space::LG); // same inset as the header, so the title stays over its numbers
                        }
                        let rt = if rights[i] {
                            RichText::new(txt)
                                .monospace()
                                .size(role_px(ui.ctx(), TextRole::Title))
                                .color(*col)
                        } else {
                            RichText::new(txt).size(role_px(ui.ctx(), TextRole::Title)).color(*col)
                        };
                        ui.add(egui::Label::new(rt).truncate());
                    });
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{Importance, cal_day_label, maps};

    #[test]
    fn day_label_formats_iso_dates() {
        // "YYYY-MM-DD" → "Weekday, Month D" (no zero-padded day)
        assert_eq!(cal_day_label("2026-06-29"), "Monday, June 29");
        assert_eq!(cal_day_label("2026-07-04"), "Saturday, July 4");
    }

    #[test]
    fn day_label_passes_unparsable_input_through() {
        assert_eq!(cal_day_label("not-a-date"), "not-a-date");
        assert_eq!(cal_day_label(""), "");
    }

    /// The importance rows are TODAY's glyph, on every theme: three red bars for High, two amber for
    /// Medium and one grey for the rest, the unlit bars in the theme's border. Changing the table
    /// changes this named test.
    #[test]
    fn the_importance_rows_are_todays_glyph_colours_and_bar_counts() {
        use vike_ui_theme::appearance::Appearance;
        use vike_ui_theme::components::{Status, Tokens};
        use vike_ui_theme::theme::ThemeId;
        for id in ThemeId::ALL {
            let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
            for (importance, lit, bars) in [
                (Importance::High, Status::Error.color(), 3),
                (Importance::Medium, Status::Warning.color(), 2),
                (Importance::Other, t.theme.text3, 1),
            ] {
                let row = importance.row();
                let at = format!("{id:?} {importance:?}");
                assert_eq!(row.colour.resolve(&t), lit, "{at}: the lit bars");
                assert_eq!(row.fill.resolve(&t), t.theme.border, "{at}: the unlit bars");
                assert_eq!(row.count, Some(bars), "{at}: how many bars are lit");
            }
        }
    }

    /// Every importance has a row named for it, no two share one, and every row of the map is an
    /// importance's.
    #[test]
    fn every_importance_has_its_own_row_and_every_row_has_an_importance() {
        let all = [Importance::High, Importance::Medium, Importance::Other];
        for importance in all {
            assert_eq!(importance.row().key, format!("{importance:?}").to_uppercase());
        }
        let mut keys: Vec<&str> = all.iter().map(|i| i.row().key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len(), "two importances share a row");
        assert!(maps::importance::ALL.iter().all(|r| all.iter().any(|i| i.row() == *r)));
    }

    /// The feed's impact word names the importance of that name; anything else is the last.
    #[test]
    fn the_feeds_impact_word_names_its_importance() {
        assert_eq!(Importance::of("High"), Importance::High);
        assert_eq!(Importance::of("Medium"), Importance::Medium);
        for other in ["Low", "Holiday", "", "high"] {
            assert_eq!(Importance::of(other), Importance::Other, "{other:?}");
        }
        assert_eq!(Importance::default(), Importance::Other);
    }
}
