//! The Economic page's two drawn glyphs: a country's flag when no bundled PNG answers its code
//! ([`draw_flag`]) and the 3-bar importance glyph ([`draw_impact`]); plus the bundled flag PNGs
//! themselves ([`FLAG_PNGS`]), which the desktop decodes once with its egui context.

use super::CalendarEconomicImportance;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::calendar;

/// Draw a small recognizable flag for `iso` (lowercase alpha-2) into `r`. Ported from
/// economic_calendar.py `_FLAG_BANDS` + `_FLAG_SPECIAL` (drawn, no PNG assets).
pub(crate) fn draw_flag(p: &egui::Painter, r: egui::Rect, iso: &str) {
    use egui::{Color32, Rect, pos2};
    let white = calendar::FLAG_WHITE;
    let navy = calendar::FLAG_NAVY;
    let red = calendar::FLAG_RED;
    let horizontal_bands = |cols: &[Color32]| {
        let n = cols.len() as f32;
        for (i, c) in cols.iter().enumerate() {
            let y0 = r.top() + r.height() * i as f32 / n;
            let y1 = r.top() + r.height() * (i as f32 + 1.0) / n;
            p.rect_filled(Rect::from_min_max(pos2(r.left(), y0), pos2(r.right(), y1)), 0.0, *c);
        }
    };
    let vb = |cols: &[Color32]| {
        let n = cols.len() as f32;
        for (i, c) in cols.iter().enumerate() {
            let x0 = r.left() + r.width() * i as f32 / n;
            let x1 = r.left() + r.width() * (i as f32 + 1.0) / n;
            p.rect_filled(Rect::from_min_max(pos2(x0, r.top()), pos2(x1, r.bottom())), 0.0, *c);
        }
    };
    match iso {
        "de" => horizontal_bands(&[
            calendar::FLAG_DE_BLACK,
            calendar::FLAG_DE_RED,
            calendar::FLAG_DE_GOLD,
        ]),
        "fr" => vb(&[calendar::FLAG_FR_BLUE, white, calendar::FLAG_FR_RED]),
        "it" => vb(&[calendar::FLAG_IT_GREEN, white, calendar::FLAG_IT_RED]),
        "in" => horizontal_bands(&[calendar::FLAG_IN_SAFFRON, white, calendar::FLAG_IN_GREEN]),
        "ru" => horizontal_bands(&[white, calendar::FLAG_RU_BLUE, calendar::FLAG_RU_RED]),
        "mx" => vb(&[calendar::FLAG_MX_GREEN, white, calendar::FLAG_MX_RED]),
        "ca" => vb(&[red, white, red]),
        "id" => horizontal_bands(&[calendar::FLAG_ID_RED, white]),
        "sg" => horizontal_bands(&[calendar::FLAG_SG_RED, white]),
        "za" => horizontal_bands(&[calendar::FLAG_ZA_GREEN, white, calendar::FLAG_ZA_RED]),
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
            p.circle_filled(r.center(), calendar::FLAG_EU_DOT_RADIUS, calendar::FLAG_EU_GOLD); // vike _flag_eu dot
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

impl CalendarEconomicImportance {
    /// The row of the `importance` map this importance is drawn by.
    fn row(self) -> &'static ThemeMapRow {
        match self {
            CalendarEconomicImportance::High => &maps::importance::HIGH,
            CalendarEconomicImportance::Medium => &maps::importance::MEDIUM,
            CalendarEconomicImportance::Other => &maps::importance::OTHER,
        }
    }
}

/// The 3-bar importance glyph (heights 5/9/13 in an 18×14 box). The importance's row of the
/// `importance` map says how many bars are lit (`count`) and in what colour (`colour`), and what
/// the rest dim to (`fill`). Ported from calendar_delegate.py.
pub(crate) fn draw_impact(
    p: &egui::Painter,
    t: &ThemeTokens,
    r: egui::Rect,
    importance: CalendarEconomicImportance,
) {
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

/// The country flags the Calendar's currency column can show, by lowercase iso2: every code
/// `crate::economic::currency_country` can yield plus de/fr/it/es. 40 px wide PNGs bundled from
/// `assets/flags/` (source and date: its README), so the desktop downloads nothing for them. The
/// desktop decodes them once with its egui context (`App::load_flag_images`).
pub const FLAG_PNGS: &[(&str, &[u8])] = &[
    ("us", include_bytes!("../../assets/flags/us.png")),
    ("eu", include_bytes!("../../assets/flags/eu.png")),
    ("gb", include_bytes!("../../assets/flags/gb.png")),
    ("jp", include_bytes!("../../assets/flags/jp.png")),
    ("au", include_bytes!("../../assets/flags/au.png")),
    ("nz", include_bytes!("../../assets/flags/nz.png")),
    ("ca", include_bytes!("../../assets/flags/ca.png")),
    ("ch", include_bytes!("../../assets/flags/ch.png")),
    ("cn", include_bytes!("../../assets/flags/cn.png")),
    ("in", include_bytes!("../../assets/flags/in.png")),
    ("br", include_bytes!("../../assets/flags/br.png")),
    ("za", include_bytes!("../../assets/flags/za.png")),
    ("kr", include_bytes!("../../assets/flags/kr.png")),
    ("mx", include_bytes!("../../assets/flags/mx.png")),
    ("ru", include_bytes!("../../assets/flags/ru.png")),
    ("tr", include_bytes!("../../assets/flags/tr.png")),
    ("id", include_bytes!("../../assets/flags/id.png")),
    ("sa", include_bytes!("../../assets/flags/sa.png")),
    ("sg", include_bytes!("../../assets/flags/sg.png")),
    ("hk", include_bytes!("../../assets/flags/hk.png")),
    ("se", include_bytes!("../../assets/flags/se.png")),
    ("no", include_bytes!("../../assets/flags/no.png")),
    ("de", include_bytes!("../../assets/flags/de.png")),
    ("fr", include_bytes!("../../assets/flags/fr.png")),
    ("it", include_bytes!("../../assets/flags/it.png")),
    ("es", include_bytes!("../../assets/flags/es.png")),
];

#[cfg(test)]
mod tests {
    use super::{CalendarEconomicImportance, maps};

    /// The importance rows are TODAY's glyph, on every theme: three red bars for High, two amber for
    /// Medium and one grey for the rest, the unlit bars in the theme's border. Changing the table
    /// changes this named test.
    #[test]
    fn the_importance_rows_are_todays_glyph_colours_and_bar_counts() {
        use vike_ui_theme::appearance::ThemeAppearance;
        use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
        use vike_ui_theme::theme::ThemePaletteId;
        for id in ThemePaletteId::ALL {
            let t = ThemeTokens::from_appearance(&ThemeAppearance {
                theme: id,
                ..ThemeAppearance::default()
            });
            for (importance, lit, bars) in [
                (CalendarEconomicImportance::High, ThemeStatus::Error.color(), 3),
                (CalendarEconomicImportance::Medium, ThemeStatus::Warning.color(), 2),
                (CalendarEconomicImportance::Other, t.theme.text_tertiary, 1),
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
        let all = [
            CalendarEconomicImportance::High,
            CalendarEconomicImportance::Medium,
            CalendarEconomicImportance::Other,
        ];
        for importance in all {
            assert_eq!(importance.row().key, format!("{importance:?}").to_uppercase());
        }
        let mut keys: Vec<&str> = all.iter().map(|i| i.row().key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len(), "two importances share a row");
        assert!(maps::importance::ALL.iter().all(|r| all.iter().any(|i| i.row() == *r)));
    }
}
