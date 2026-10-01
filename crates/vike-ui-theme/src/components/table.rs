//! The data table (spec §4.1 "table", §4.2 "data table"):
//! - a header that stays put while the rows scroll;
//! - columns that share the width by weight and never go under their minimum;
//! - cells that truncate and show their whole text on hover;
//! - zebra rows and the hover fill;
//! - the selected row's accent edge — the same selected-row marker as the rail.

use std::fmt::Debug;
use std::hash::Hash;

use egui::{
    Align, Align2, Color32, Layout, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, WidgetInfo, WidgetType,
};

use super::Tokens;
use crate::type_scale::TextRole;

/// One column.
#[derive(Clone, Copy, Debug)]
pub struct Column<'a> {
    pub title: &'a str,
    /// Its share of the width, against the other columns'.
    pub weight: f32,
    /// It never gets narrower; a table whose minimums exceed its pane is wider than the pane.
    pub min_w: f32,
    /// A number: monospace and right-aligned (spec §3.3).
    pub numeric: bool,
}

/// What the table saw this frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableResponse {
    pub clicked: Option<usize>,
    pub hovered: Option<usize>,
    /// `(row, rect)` of every row drawn this frame — where an editor that opens under its row goes
    /// (spec §4.2's "flows").
    pub rows: Vec<(usize, Rect)>,
}

/// Each column's width for `available`: shares by weight, never under a column's minimum. A column
/// that would fall under its minimum is held at it, and the others re-share what is left.
pub fn column_widths(available: f32, columns: &[Column<'_>]) -> Vec<f32> {
    let mut pinned = vec![false; columns.len()];
    for _ in 0..=columns.len() {
        let held: f32 =
            columns.iter().zip(&pinned).filter(|(_, p)| **p).map(|(c, _)| c.min_w).sum();
        let weight: f32 =
            columns.iter().zip(&pinned).filter(|(_, p)| !**p).map(|(c, _)| c.weight).sum();
        let widths: Vec<f32> =
            columns
                .iter()
                .zip(&pinned)
                .map(|(c, p)| {
                    if *p || weight <= 0.0 {
                        c.min_w
                    } else {
                        (available - held) * c.weight / weight
                    }
                })
                .collect();
        let under: Vec<usize> =
            (0..columns.len()).filter(|&i| !pinned[i] && widths[i] < columns[i].min_w).collect();
        if under.is_empty() {
            return widths;
        }
        for i in under {
            pinned[i] = true;
        }
    }
    columns.iter().map(|c| c.min_w).collect()
}

/// A data table of `rows` rows; `cell(row, column)` is a cell's text. The header is drawn OUTSIDE
/// the scroll area, which is what keeps it in place; `id` keys the scroll position.
pub fn data_table(
    ui: &mut Ui,
    id: impl Hash + Debug,
    columns: &[Column<'_>],
    rows: usize,
    selected: Option<usize>,
    mut cell: impl FnMut(usize, usize) -> String,
) -> TableResponse {
    let t = Tokens::of(ui.ctx());
    let widths = column_widths(ui.available_width(), columns);
    let full: f32 = widths.iter().sum();
    let (head, _) = ui.allocate_exact_size(egui::vec2(full, t.metrics.row_h), Sense::hover());
    {
        let p = ui.painter();
        let mut x = head.left();
        for (c, w) in columns.iter().zip(&widths) {
            let r = Rect::from_min_size(egui::pos2(x, head.top()), egui::vec2(*w, head.height()));
            let (pos, anchor) = anchor(r, c.numeric, t.metrics.pad);
            p.with_clip_rect(r).text(
                pos,
                anchor,
                c.title,
                t.font(TextRole::Caption),
                t.theme.text3,
            );
            x += w;
        }
        p.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, t.theme.border));
    }
    let mut out = TableResponse::default();
    ScrollArea::vertical().id_salt(id).auto_shrink([false, true]).show_rows(
        ui,
        t.metrics.row_h,
        rows,
        |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let on = selected == Some(i);
                // The selected row is a LABEL, as the rail's is: it senses no click, so it cannot
                // be picked again, and the tree reports it by role (spec §4.2).
                let sense = if on { Sense::hover() } else { Sense::click() };
                let (row, resp) = ui.allocate_exact_size(egui::vec2(full, t.metrics.row_h), sense);
                let hovered = ui.rect_contains_pointer(row);
                let fill = if on || hovered {
                    t.theme.hover
                } else if i % 2 == 1 {
                    t.theme.surface
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(row, 0.0, fill);
                if on {
                    let edge = Rect::from_min_size(row.min, egui::vec2(2.0, row.height()));
                    ui.painter().rect_filled(edge, 0.0, t.theme.accent);
                }
                let mut x = row.left();
                let mut spoken = Vec::with_capacity(columns.len());
                for (col, (c, w)) in columns.iter().zip(&widths).enumerate() {
                    let r =
                        Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(*w, row.height()));
                    let layout = if c.numeric {
                        Layout::right_to_left(Align::Center)
                    } else {
                        Layout::left_to_right(Align::Center)
                    };
                    let inner = r.shrink2(egui::vec2(t.metrics.pad, 0.0));
                    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(layout));
                    let font =
                        if c.numeric { t.mono(TextRole::Body) } else { t.font(TextRole::Body) };
                    let words = cell(i, col);
                    let text = RichText::new(words.as_str()).font(font).color(t.theme.text);
                    child.add(egui::Label::new(text).truncate().selectable(false));
                    spoken.push(words);
                    x += w;
                }
                // Focus rings a row from INSIDE: the next row paints over anything outside it.
                if resp.has_focus() {
                    let ring = Stroke::new(1.0, t.theme.accent);
                    ui.painter().rect_stroke(row, 0.0, ring, StrokeKind::Inside);
                }
                // Named by its cells, whole — a truncated cell's text included.
                let name = spoken.join(" ");
                let typ = if on { WidgetType::Label } else { WidgetType::Button };
                resp.widget_info(|| WidgetInfo::labeled(typ, true, &name));
                if hovered {
                    out.hovered = Some(i);
                }
                if resp.clicked() {
                    out.clicked = Some(i);
                }
                out.rows.push((i, row));
            }
        },
    );
    out
}

/// Where a header's text sits: left for words, right for numbers, inside the padding.
fn anchor(r: Rect, numeric: bool, pad: f32) -> (egui::Pos2, Align2) {
    if numeric {
        (egui::pos2(r.right() - pad, r.center().y), Align2::RIGHT_CENTER)
    } else {
        (egui::pos2(r.left() + pad, r.center().y), Align2::LEFT_CENTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::Tokens;
    use crate::components::testing::{ctx_with, fills, harness, named, paint};
    use crate::theme::ThemeId;
    use egui::accesskit::Role;

    fn cols() -> [Column<'static>; 3] {
        [
            Column { title: "Symbol", weight: 2.0, min_w: 60.0, numeric: false },
            Column { title: "Venue", weight: 1.0, min_w: 40.0, numeric: false },
            Column { title: "Bars", weight: 1.0, min_w: 120.0, numeric: true },
        ]
    }

    #[test]
    fn widths_share_by_weight_and_hold_a_minimum() {
        let even = [Column { title: "a", weight: 1.0, min_w: 0.0, numeric: false }; 3];
        assert_eq!(column_widths(300.0, &even), [100.0, 100.0, 100.0]);
        // "Bars" would get a quarter of 400 = 100, holds its 120, and the other two share 280 2:1.
        let w = column_widths(400.0, &cols());
        assert_eq!(w[2], 120.0);
        assert!(
            (w[0] - 280.0 * 2.0 / 3.0).abs() < 1e-3 && (w[1] - 280.0 / 3.0).abs() < 1e-3,
            "{w:?}"
        );
        // Minimums wider than the pane are kept: the table grows past its pane, never squeezed.
        assert_eq!(column_widths(100.0, &cols()), [60.0, 40.0, 120.0]);
    }

    /// Zebra rows, and the selected row filled with the accent edge — in EACH theme's accent, the
    /// check a compile-time colour fails on three themes of four.
    #[test]
    fn zebra_rows_and_the_selected_rows_accent_edge() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let t = Tokens::of(&ctx);
            let shapes = paint(&ctx, |ui| {
                data_table(ui, "t", &cols(), 4, Some(2), |r, c| format!("{r}:{c}"));
            });
            let f = fills(&shapes);
            assert!(f.contains(&t.theme.surface), "{id:?}: odd rows are zebra: {f:?}");
            assert!(f.contains(&t.theme.hover), "{id:?}: the selected row is filled");
            assert!(f.contains(&t.theme.accent), "{id:?}: the selected row's accent edge");
        }
    }

    /// The selected row is a LABEL and every other row a button, each named by its cells: the
    /// accessibility tree answers "which row" by role, as it does for the rail (spec §4.2), and the
    /// selected row cannot be picked again.
    #[test]
    fn the_selected_row_is_a_label_and_the_others_are_buttons() {
        let h = harness(Appearance::default(), |ui| {
            data_table(ui, "t", &cols(), 3, Some(1), |r, c| format!("r{r}c{c}"));
        });
        let labels = named(&h, Role::Label);
        let buttons = named(&h, Role::Button);
        assert!(labels.contains(&"r1c0 r1c1 r1c2".to_string()), "{labels:?}");
        for other in ["r0c0 r0c1 r0c2", "r2c0 r2c1 r2c2"] {
            assert!(buttons.contains(&other.to_string()), "{other}: {buttons:?}");
        }
        assert!(!buttons.iter().any(|b| b.starts_with("r1")), "{buttons:?}");
    }

    /// A truncated cell keeps its WHOLE text for the hover and for the accessibility tree.
    #[test]
    fn a_truncated_cell_keeps_its_whole_text() {
        let long = "a symbol name far too long for its column";
        let h = harness(Appearance::default(), move |ui| {
            ui.set_width(300.0);
            data_table(ui, "t", &cols(), 1, None, |_, c| {
                if c == 0 { long.to_string() } else { "x".into() }
            });
        });
        let labels = named(&h, Role::Label);
        assert!(labels.iter().any(|l| l == long), "{labels:?}");
    }
}
