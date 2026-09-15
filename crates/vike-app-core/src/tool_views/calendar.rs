//! The Calendar tool body — the 7-day economic/earnings day strip, the ForexFactory economic
//! table (drawn flags + impact glyphs) and the Nasdaq equity tables (Earnings/Dividends/IPO),
//! with its private helpers (`draw_flag`/`draw_impact`/`cal_day_label`/`cal_equity_table`).
//! Moved verbatim from `vike-app`'s `main.rs` (tool-view extraction batch 1).

use super::ToolCtx;
use crate::tools;
use vike_ui_theme::{font, palette as theme};

/// The Calendar tool body: the 7-day economic/earnings day strip + the per-page tables (flag
/// PNGs from `flags`). Split out of `tool_content`'s match arm verbatim (dedup-app refactor).
pub fn calendar_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    use egui::RichText;
    use egui::{Align, Color32, Layout, vec2};
    const C_SURF: Color32 = theme::SURFACE;
    const C_HOV: Color32 = theme::HOVER;
    const C_BORD: Color32 = theme::BORDER;
    const C_TEXT: Color32 = theme::TEXT;
    const C_T2: Color32 = theme::TEXT2;
    const C_T3: Color32 = theme::TEXT3;
    const C_UP: Color32 = theme::UP;
    const C_DOWN: Color32 = theme::DOWN;
    let (td, flags) = (ctx.td, ctx.flags);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let num = |s: &str| -> Option<f64> {
        let t: String =
            s.trim().chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
        t.parse::<f64>().ok()
    };
    const C_ACC: Color32 = theme::ACCENT; // FIX: was (62,224,137), a 1-bit drift off ACCENT
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
        ui.spacing_mut().button_padding = vec2(11.0, 7.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        let _ = ui.button("Today");
        let _ = ui.button("‹");
        let _ = ui.button("›");
        ui.add_space(6.0);
        ui.label(font::semibold(&td.cal_range).size(16.0).color(C_TEXT)); // Py 16px/600
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let _ = ui.add(egui::Button::new("Local").min_size(vec2(96.0, 0.0)));
            let _ = ui.add(egui::Button::new("Countries").min_size(vec2(110.0, 0.0)));
            ui.add_space(4.0);
            ui.checkbox(&mut tv.cal_high_only, RichText::new("High only").color(C_T2));
        });
    });
    ui.add_space(6.0);

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
                .filter(|e| e.date_iso == iso && (!high_only || e.importance >= 2))
                .count();
            let (earn, div, ipo) = td.cal_equity.get(&iso).copied().unwrap_or((0, 0, 0));
            week.push((format!("{} {}", WD3[i as usize], d.day()), [econ, earn, div, ipo]));
        }
        let gap = 8.0;
        let cw = ((full - gap * 6.0) / 7.0).clamp(90.0, 320.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, (label, counts)) in week.iter().enumerate() {
                let on = i as i8 == sel_day; // selected card → HOVER fill + ACCENT border/title
                let inner = egui::Frame::new()
                    .fill(if on { C_HOV } else { C_SURF })
                    .stroke(egui::Stroke::new(1.0, if on { C_ACC } else { C_BORD }))
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        // force VERTICAL — the Frame inherits the cards-row left_to_right
                        // layout otherwise, so title + rows flow sideways and overlap.
                        ui.vertical(|ui| {
                            ui.set_width(cw - 24.0);
                            ui.set_min_height(64.0);
                            let tc = if on { C_ACC } else { C_TEXT };
                            ui.label(font::semibold(label).size(13.0).color(tc)); // Py 13/600
                            ui.add_space(3.0);
                            for (ci, cnt) in counts.iter().enumerate() {
                                if *cnt == 0 {
                                    continue; // hide zero rows (vike _DayCard.set_counts)
                                }
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(CATS[ci]).size(12.0).color(C_T2)); // 12/400
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(
                                            font::semibold(cnt.to_string())
                                                .size(12.0)
                                                .color(C_TEXT),
                                        ); // 12/600
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
    ui.add_space(6.0);

    // ---- category pills + All categories ----
    ui.horizontal(|ui| {
        for (i, name) in PAGES.iter().enumerate() {
            let active = i == page as usize;
            if ui
                .selectable_label(
                    active,
                    RichText::new(*name).size(12.0).color(if active { C_TEXT } else { C_T3 }),
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
    ui.add_space(2.0);

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
        let (w_time, w_ctry, w_imp) = (52.0, 150.0, 24.0);
        // vike _NUM_COL_W = 210 fixed when the panel is wide enough; scale down on narrow ones.
        let w_num = if full >= 980.0 { 210.0 } else { (full * 0.14).clamp(90.0, 210.0) };
        let (w_act, w_fc, w_pr) = (w_num, w_num, w_num);
        // right margin = vike _RIGHT_PAD(40) + scrollbar clearance, so Prior doesn't collide
        // with the window edge / vertical scrollbar.
        let w_evt = (full - (w_time + w_ctry + w_imp + w_act + w_fc + w_pr) - 52.0).max(140.0);
        let hcell = |ui: &mut egui::Ui, w: f32, t: &str, right: bool| {
            let lay = if right {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            ui.allocate_ui_with_layout(vec2(w, 20.0), lay, |ui| {
                ui.set_min_width(w); // reserve the FULL column width (else it collapses to text)
                ui.add(egui::Label::new(RichText::new(t).size(12.0).color(C_T3)).truncate());
                // Py header 12px TEXT3
            });
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0; // columns land exactly on computed widths
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
            egui::Stroke::new(1.0, C_BORD),
        );

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if td.calendar.is_empty() {
                ui.weak("loading…");
            }
            let mut cur_day = String::new();
            let (mut prev_time, mut prev_ctry) = (String::new(), String::new());
            let mut marker_done = false;
            for e in td.calendar.iter().filter(|e| {
                (!high_only || e.importance >= 2)
                    && sel_iso.as_ref().is_none_or(|s| s == &e.date_iso)
            }) {
                if e.day != cur_day {
                    cur_day = e.day.clone();
                    prev_time.clear();
                    prev_ctry.clear();
                    ui.add_space(5.0);
                    ui.label(font::bold(&cur_day).size(13.0).color(C_TEXT));
                    // Py day-group bold/13
                }
                // red "● now HH:MM" marker before the first future event
                if !marker_done && e.ts_ms > now_ms {
                    marker_done = true;
                    let hhmm = chrono::DateTime::from_timestamp_millis(now_ms)
                        .map(|d| d.format("%H:%M").to_string())
                        .unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.colored_label(C_DOWN, RichText::new("●").size(12.0));
                        ui.label(font::bold(format!("now  {hhmm}")).color(C_DOWN));
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
                    (format!("Coming in {}:{:02}:{:02}", d / 3600, (d / 60) % 60, d % 60), C_DOWN)
                } else if e.actual.is_empty() {
                    ("—".to_string(), C_TEXT)
                } else {
                    let col = match (num(&e.actual), num(&e.forecast)) {
                        (Some(a), Some(f)) if a > f => C_UP,
                        (Some(a), Some(f)) if a < f => C_DOWN,
                        _ => C_TEXT,
                    };
                    (e.actual.clone(), col)
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0; // align rows to the header columns
                    // TIME (mono)
                    ui.allocate_ui_with_layout(
                        vec2(w_time, 19.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_time);
                            if show_time {
                                ui.label(RichText::new(&e.time).monospace().size(13.0).color(C_T3));
                            }
                        },
                    );
                    // COUNTRY: flag + name + impact glyph
                    ui.allocate_ui_with_layout(
                        vec2(w_ctry, 19.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_ctry);
                            if show_ctry {
                                let (fr, _) =
                                    ui.allocate_exact_size(vec2(20.0, 14.0), egui::Sense::hover());
                                if let Some(tex) = flags.get(&e.iso2) {
                                    // real flag PNG texture (accurate); fall back to a drawn flag
                                    ui.painter().image(
                                        tex.id(),
                                        fr,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                } else if !e.iso2.is_empty() {
                                    draw_flag(ui.painter(), fr, &e.iso2);
                                }
                                ui.add_space(5.0);
                                let nm = if e.country_name.is_empty() {
                                    &e.country
                                } else {
                                    &e.country_name
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(nm).color(C_TEXT)).truncate(),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        vec2(w_imp, 19.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_imp);
                            let (ir, _) =
                                ui.allocate_exact_size(vec2(18.0, 14.0), egui::Sense::hover());
                            draw_impact(ui.painter(), ir, e.importance);
                        },
                    );
                    // EVENT
                    ui.allocate_ui_with_layout(
                        vec2(w_evt, 19.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(w_evt);
                            ui.add(
                                egui::Label::new(RichText::new(&e.title).color(C_TEXT)).truncate(),
                            );
                        },
                    );
                    // ACTUAL / FORECAST / PRIOR (mono, right-aligned)
                    let rcell = |ui: &mut egui::Ui, w: f32, t: String, c: Color32| {
                        ui.allocate_ui_with_layout(
                            vec2(w, 19.0),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.set_min_width(w);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(t).monospace().size(13.0).color(c),
                                    )
                                    .truncate(),
                                );
                            },
                        );
                    };
                    rcell(ui, w_act, act_txt, act_col);
                    rcell(ui, w_fc, dash(&e.forecast), C_TEXT); // Py: Forecast/Prior = default TEXT
                    rcell(ui, w_pr, dash(&e.previous), C_TEXT);
                });
            }
        });
    } else {
        // Earnings / Dividends / IPO equity tables (built below in a follow-on edit)
        cal_equity_table(ui, td, page);
    }
    ui.add_space(3.0);
    if !td.cal_status.is_empty() {
        ui.label(RichText::new(&td.cal_status).size(11.0).color(C_T3));
    }
}

/// Draw a small recognizable flag for `iso` (lowercase alpha-2) into `r`. Ported from
/// economic_calendar.py `_FLAG_BANDS` + `_FLAG_SPECIAL` (drawn, no PNG assets).
fn draw_flag(p: &egui::Painter, r: egui::Rect, iso: &str) {
    use egui::{Color32 as C, Rect, pos2};
    let white = C::from_rgb(0xff, 0xff, 0xff);
    let navy = C::from_rgb(1, 33, 105);
    let red = C::from_rgb(0xcf, 0x14, 0x2b);
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
        "de" => hb(&[C::BLACK, C::from_rgb(0xdd, 0, 0), C::from_rgb(0xff, 0xce, 0)]),
        "fr" => vb(&[C::from_rgb(0, 0x23, 0x95), white, C::from_rgb(0xed, 0x29, 0x39)]),
        "it" => vb(&[C::from_rgb(0, 0x92, 0x46), white, C::from_rgb(0xce, 0x2b, 0x37)]),
        "in" => hb(&[C::from_rgb(0xff, 0x99, 0x33), white, C::from_rgb(0x13, 0x88, 0x08)]),
        "ru" => hb(&[white, C::from_rgb(0, 0x39, 0xa6), C::from_rgb(0xd5, 0x2b, 0x1e)]),
        "mx" => vb(&[C::from_rgb(0, 0x68, 0x47), white, C::from_rgb(0xce, 0x11, 0x26)]),
        "ca" => vb(&[red, white, red]),
        "id" => hb(&[C::from_rgb(0xe7, 0, 0x11), white]),
        "sg" => hb(&[C::from_rgb(0xef, 0x33, 0x40), white]),
        "za" => hb(&[C::from_rgb(0, 0x7a, 0x4d), white, C::from_rgb(0xde, 0x38, 0x31)]),
        "cn" => {
            p.rect_filled(r, 0.0, C::from_rgb(0xde, 0x29, 0x10));
        }
        "sa" => {
            p.rect_filled(r, 0.0, C::from_rgb(0, 0x6c, 0x35));
        }
        "hk" => {
            p.rect_filled(r, 0.0, C::from_rgb(0xde, 0x29, 0x10));
        }
        "tr" => {
            p.rect_filled(r, 0.0, C::from_rgb(0xe3, 0x0a, 0x17));
        }
        "au" | "nz" => {
            p.rect_filled(r, 0.0, navy);
        }
        "eu" => {
            p.rect_filled(r, 0.0, C::from_rgb(0, 0x33, 0x99));
            p.circle_filled(r.center(), 2.0, C::from_rgb(0xff, 0xcc, 0)); // vike _flag_eu dot
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
            p.circle_filled(r.center(), r.height() * 0.30, C::from_rgb(0xbc, 0, 0x2d));
        }
        "gb" => {
            // Union Jack — vike _flag_gb: navy + WHITE diagonals (corner-to-corner) + white cross,
            // then a thinner red cross. The diagonals are what make it read as the UK, not a Nordic flag.
            p.rect_filled(r, 0.0, navy);
            let (cx, cy) = (r.center().x, r.center().y);
            let ws = egui::Stroke::new(2.4, white);
            p.line_segment([r.left_top(), r.right_bottom()], ws);
            p.line_segment([r.left_bottom(), r.right_top()], ws);
            p.line_segment([pos2(cx, r.top()), pos2(cx, r.bottom())], ws);
            p.line_segment([pos2(r.left(), cy), pos2(r.right(), cy)], ws);
            let rs = egui::Stroke::new(1.1, red);
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
            p.rect_filled(r, 0.0, C::from_rgb(60, 66, 74));
        }
    }
}

/// The 3-bar importance glyph (heights 5/9/13 in an 18×14 box). Bars `i <= importance`
/// are lit (TEXT3/WARN/DOWN by level); the rest dim BORDER. Ported from calendar_delegate.py.
fn draw_impact(p: &egui::Painter, r: egui::Rect, importance: u8) {
    use egui::{Rect, pos2};
    let lit = match importance {
        2 => theme::DOWN,  // red
        1 => theme::WARN,  // amber
        _ => theme::TEXT3, // grey
    };
    let dim = theme::BORDER;
    let gx = r.center().x - 9.0;
    let gy = r.center().y - 7.0;
    for (i, bh) in [5.0_f32, 9.0, 13.0].iter().enumerate() {
        let x = gx + 1.0 + i as f32 * 6.0;
        let c = if i as u8 <= importance { lit } else { dim };
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
    use egui::{Align, Color32, Layout, RichText, vec2};
    const TEXT: Color32 = theme::TEXT;
    const T2: Color32 = theme::TEXT2;
    const T3: Color32 = theme::TEXT3;
    const BORD: Color32 = theme::BORDER;
    const UP: Color32 = theme::UP;
    const DOWN: Color32 = theme::DOWN;
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
                        UP
                    } else if r.surprise.starts_with('-') {
                        DOWN
                    } else {
                        TEXT
                    };
                    (
                        r.date.clone(),
                        vec![
                            (r.hour.clone(), T2),
                            (r.symbol.clone(), TEXT),
                            (dash(&r.eps_est), TEXT),
                            (dash(&r.eps_act), TEXT),
                            (dash(&r.surprise), sc),
                        ],
                    )
                })
                .collect();
            (
                vec!["Time", "Symbol", "EPS est.", "EPS act.", "Surprise"],
                vec![1.0, 1.3, 1.0, 1.0, 1.0],
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
                            (r.symbol.clone(), TEXT),
                            (dash(&r.date), TEXT),
                            (dash(&r.pay_date), TEXT),
                            (dash(&r.amount), TEXT),
                            (dash(&r.yield_pct), TEXT),
                            (dash(&r.freq), T2),
                        ],
                    )
                })
                .collect();
            (
                vec!["Symbol", "Ex-date", "Pay date", "Amount", "Yield", "Freq"],
                vec![1.0, 1.1, 1.1, 1.0, 0.9, 0.9],
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
                            (r.symbol.clone(), TEXT),
                            (dash(&r.company), TEXT),
                            (dash(&r.exchange), T2),
                            (dash(&r.price), TEXT),
                            (dash(&r.shares), TEXT),
                            (dash(&r.status), T2),
                        ],
                    )
                })
                .collect();
            (
                vec!["Symbol", "Company", "Exchange", "Price", "Shares", "Status"],
                vec![1.0, 2.2, 1.0, 0.9, 1.1, 0.9],
                vec![false, false, false, true, true, false],
                rows,
            )
        }
    };
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let total_w: f32 = weights.iter().sum();
    let unit = (full - 16.0).max(1.0) / total_w;
    let widths: Vec<f32> = weights.iter().map(|w| w * unit).collect();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, h) in headers.iter().enumerate() {
            let lay = if rights[i] {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            ui.allocate_ui_with_layout(vec2(widths[i], 20.0), lay, |ui| {
                ui.set_min_width(widths[i]);
                ui.add(egui::Label::new(RichText::new(*h).size(12.0).color(T3)).truncate());
            });
        }
    });
    ui.painter().hline(ui.min_rect().x_range(), ui.cursor().top(), egui::Stroke::new(1.0, BORD));

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("cal_equity").show(ui, |ui| {
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.weak("Loading…");
            return;
        }
        let mut cur = String::new();
        for (date, cells) in &rows {
            if *date != cur {
                cur = date.clone();
                ui.add_space(5.0);
                ui.label(font::bold(cal_day_label(date)).size(13.0).color(TEXT));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (i, (txt, col)) in cells.iter().enumerate() {
                    let lay = if rights[i] {
                        Layout::right_to_left(Align::Center)
                    } else {
                        Layout::left_to_right(Align::Center)
                    };
                    ui.allocate_ui_with_layout(vec2(widths[i], 19.0), lay, |ui| {
                        ui.set_min_width(widths[i]);
                        let rt = if rights[i] {
                            RichText::new(txt).monospace().size(13.0).color(*col)
                        } else {
                            RichText::new(txt).size(13.0).color(*col)
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
    use super::cal_day_label;

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
}
