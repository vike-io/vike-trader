//! The Earnings, Dividends and IPO pages (Calendar pages 1-3): the window's week of equity rows,
//! grouped by day.

use crate::window::{calendar_day_label, empty_note};
use vike_datahub_client::tool_feeds::poll::CalendarEquityWeek;
use vike_ui_theme::components::{ThemeTokens, role_px};
use vike_ui_theme::fonts;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::calendar;

/// Column model for the Calendar equity tables (Earnings/Dividends/IPO), factored out of
/// `calendar_equity_table`'s local `let` to satisfy `clippy::type_complexity`: headers, per-column
/// weights, right-align flags, and the rows (each a `(date, cells[(text, color)])`).
type CalendarEquityTable =
    (Vec<&'static str>, Vec<f32>, Vec<bool>, Vec<(String, Vec<(String, egui::Color32)>)>);

/// The Earnings/Dividends/IPO equity calendar tables (Calendar pages 1-3) of the window's week,
/// grouped by day.
pub(crate) fn calendar_equity_table(
    ui: &mut egui::Ui,
    week: &CalendarEquityWeek,
    page: u8,
    status: &str,
) {
    use egui::{Align, Layout, RichText, vec2};
    let t = ThemeTokens::of(ui.ctx());
    let dash = |s: &str| if s.trim().is_empty() { "—".to_string() } else { s.to_string() };
    let full = ui.available_width();

    // (headers, weights, right-align flags, rows[(date, cells[(text,color)])])
    let (headers, weights, rights, mut rows): CalendarEquityTable = match page {
        1 => {
            let rows = week
                .earnings
                .iter()
                .map(|r| {
                    let sc = if r.surprise.starts_with('+') {
                        ThemeSidePair::PositiveNegative.text(true, &t)
                    } else if r.surprise.starts_with('-') {
                        ThemeSidePair::PositiveNegative.text(false, &t)
                    } else {
                        t.theme.text
                    };
                    (
                        r.date.clone(),
                        vec![
                            (r.hour.clone(), t.theme.text_secondary),
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
                    calendar::EARNINGS_WEIGHT_TIME,
                    calendar::EARNINGS_WEIGHT_SYMBOL,
                    calendar::EARNINGS_WEIGHT_EPS_ESTIMATE,
                    calendar::EARNINGS_WEIGHT_EPS_ACTUAL,
                    calendar::EARNINGS_WEIGHT_SURPRISE,
                ],
                vec![false, false, true, true, true],
                rows,
            )
        }
        2 => {
            let rows = week
                .dividends
                .iter()
                .map(|r| {
                    (
                        r.date.clone(),
                        vec![
                            (r.symbol.clone(), t.theme.text),
                            (dash(&r.date), t.theme.text),
                            (dash(&r.pay_date), t.theme.text),
                            (dash(&r.amount), t.theme.text),
                            (dash(&r.yield_percent), t.theme.text),
                            (dash(&r.freq), t.theme.text_secondary),
                        ],
                    )
                })
                .collect();
            (
                vec!["Symbol", "Ex-date", "Pay date", "Amount", "Yield", "Freq"],
                vec![
                    calendar::DIVIDEND_WEIGHT_SYMBOL,
                    calendar::DIVIDEND_WEIGHT_EX_DATE,
                    calendar::DIVIDEND_WEIGHT_PAY_DATE,
                    calendar::DIVIDEND_WEIGHT_AMOUNT,
                    calendar::DIVIDEND_WEIGHT_YIELD,
                    calendar::DIVIDEND_WEIGHT_FREQUENCY,
                ],
                vec![false, false, false, true, true, false],
                rows,
            )
        }
        _ => {
            let rows = week
                .ipos
                .iter()
                .map(|r| {
                    (
                        r.date.clone(),
                        vec![
                            (r.symbol.clone(), t.theme.text),
                            (dash(&r.company), t.theme.text),
                            (dash(&r.exchange), t.theme.text_secondary),
                            (dash(&r.price), t.theme.text),
                            (dash(&r.shares), t.theme.text),
                            (dash(&r.status), t.theme.text_secondary),
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
    let unit = (full - calendar::EQUITY_RIGHT_PADDING).max(1.0) / total_w;
    let widths: Vec<f32> = weights.iter().map(|w| w * unit).collect();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::NONE;
        for (i, h) in headers.iter().enumerate() {
            let lay = if rights[i] {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            ui.allocate_ui_with_layout(vec2(widths[i], calendar::HEADER_HEIGHT), lay, |ui| {
                ui.set_min_width(widths[i]);
                if rights[i] {
                    ui.add_space(space::LG); // a right-aligned column keeps a gap from the left-aligned one after it
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(*h)
                            .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                            .color(t.theme.text_tertiary),
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
            ui.weak(empty_note(status));
            return;
        }
        let mut current = String::new();
        for (date, cells) in &rows {
            if *date != current {
                current = date.clone();
                ui.add_space(space::MD);
                ui.label(
                    fonts::semibold(calendar_day_label(date))
                        .size(role_px(ui.ctx(), ThemeTextRole::Title))
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
                    ui.allocate_ui_with_layout(vec2(widths[i], calendar::ROW_HEIGHT), lay, |ui| {
                        ui.set_min_width(widths[i]);
                        if rights[i] {
                            ui.add_space(space::LG); // same inset as the header, so the title stays over its numbers
                        }
                        let rt = if rights[i] {
                            RichText::new(txt)
                                .monospace()
                                .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                .color(*col)
                        } else {
                            RichText::new(txt)
                                .size(role_px(ui.ctx(), ThemeTextRole::Title))
                                .color(*col)
                        };
                        ui.add(egui::Label::new(rt).truncate());
                    });
                }
            });
        }
    });
}
