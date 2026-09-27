//! **The tool window's title bar — and the slot inside it a tabbed tool seats its segments in.**
//!
//! # Why this lives here rather than in the shell
//!
//! It was `vike-desktop`'s `main.rs` until the Connections tabs moved into the bar. That crate is
//! in `xtask::ci::tables`' `EXCLUDE_FROM_CI` and its whole coverage is the `app-check` job — a
//! `cargo check`, clippy, and a nextest pass that reaches `crates/vike-desktop/src/chart_gpu.rs`
//! and nothing else — so the title bar's GEOMETRY was gated by nothing at all. Every decision it
//! makes is CPU work (`egui::Context::run_ui` computes layout with no GPU), so moving the whole
//! function one crate down puts it where `crates/vike-app-core/tests/connections_tabs.rs`'s
//! `the_tab_slot_lives_inside_the_title_bar_and_ends_on_its_hairline` runs it on
//! the GPU-less CI runners. What is left at the call site is the two-line mapping into the shell's
//! own `TitleActions`.
//!
//! ⚠ That is arm 1 of `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs`'s own
//! `GROWTH_GUIDANCE` applied rather than argued around: the tabs needed room in the shell, and the
//! shell's ceiling had six code lines of slack.
//!
//! # The tab slot, and what it is FOR
//!
//! A tool whose body is tabbed ([`crate::workspace::WinKind::carries_title_tabs`]) does not draw a
//! second row of tabs under the bar — it seats its segmented control INSIDE the bar, between the
//! window title and the three window controls. [`tool_title_bar`] does not draw those segments (it
//! cannot: their labels carry live counts the body computes), it RESERVES their rect and hands it
//! back as a [`TitleTabSlot`]. The body then paints into it —
//! `vike_app_core::tool_views::title_bar_tabs`, one frame, same pass, so the counts on the chips
//! are this frame's rather than last frame's.
//!
//! Two properties that are the whole design and are gated rather than described:
//!
//! * **The slot's BOTTOM is the bar's bottom.** The selected chip's fourth edge is therefore the
//!   gap it makes in the hairline the tool paints at [`TitleTabSlot::hairline_y`] — the chip reads
//!   as continuous with the panel below it, which is what a tab MEANS, and no marker rule is
//!   needed. ⚠ That line lives one pixel row BELOW the bar, so it must be painted under
//!   [`TitleTabSlot::hairline_clip`] and NEVER under `bar`: the renderer's scissor bound is
//!   exclusive and clipping to the bar deletes the whole stroke. That doc carries the arithmetic
//!   and the gate.
//! * **Below [`TITLE_DROP_W`] the window TITLE drops** and the segments tighten
//!   ([`TitleTabSlot::pad`]), which is what makes two segments fit in a title bar on a ~400pt
//!   window. The icon stays: it is the only thing left that says which window this is.
//!
//! A kind that carries no tabs keeps its title at every width and gets a slot it never reads.

use super::state::WinKind;
use egui::{Align, Button, Layout, Pos2, Rect, RichText, Sense, UiBuilder, Vec2};
use vike_ui_theme::palette;

/// The title bar's height. Unchanged from the spelling this moved out of.
pub const BAR_H: f32 = 30.0;

/// One window control's width (`─ □ ✕`), and the trailing pad after them.
const CTRL_W: f32 = 30.0;
const CTRL_PAD: f32 = 6.0;

/// The tab chips' height, seated on the bar's BOTTOM edge so the selected one's missing fourth
/// edge is the break in the hairline under the bar.
pub const TAB_H: f32 = 24.0;

/// The gap between the window title (or its icon, once the title has dropped) and the first chip.
const TAB_GAP: f32 = 10.0;

/// Below this bar width a tabbed window's TITLE drops and its segments tighten. It is also the
/// width a tool window opens at (`crate::window_spawn`'s `vec2(560.0, 400.0)`), so the narrow
/// shape is the ordinary one rather than an edge case.
pub const TITLE_DROP_W: f32 = 560.0;

/// The chip's horizontal padding at each width — wide, then tightened.
const PAD_WIDE: i8 = 9;
const PAD_TIGHT: i8 = 5;

/// What the three window controls decided this frame. The shell maps it into its own richer
/// `TitleActions` (which carries the chart bar's symbol/venue/indicator fields as well, none of
/// which a TOOL title bar can produce).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolTitleActions {
    pub close: bool,
    pub minimize: bool,
    pub toggle_max: bool,
}

/// Where a tabbed tool's segmented control goes, handed back by [`tool_title_bar`] so the BODY can
/// paint into it later in the same pass.
///
/// ⚠ Every field is absolute screen geometry, not a size — the body paints into it through a
/// detached `egui::Ui`, so it must not depend on wherever the body's own cursor happens to be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleTabSlot {
    /// The whole title-bar rect — the span the hairline runs across.
    pub bar: Rect,
    /// The chips' rect: from just after the title to just before the window controls, and BOTTOM-
    /// seated on the bar so the selected chip breaks the hairline.
    pub tabs: Rect,
    /// The chips' horizontal padding at this width — wide, or tightened below
    /// [`TITLE_DROP_W`]. [`title_bar_plan`] is where the two values live.
    pub pad: i8,
    /// Whether this window's kind seats tabs here at all. A body that reads the slot on a kind
    /// that answers `false` is drawing a control nothing reserved room for.
    pub carries_tabs: bool,
}

impl TitleTabSlot {
    /// The y of the hairline the selected chip breaks: the bar's own bottom edge, on the pixel
    /// grid. ⚠ vike's title bar deliberately draws NO bottom divider of its own, so this line is
    /// the tool's and is painted by the tool — see the module doc.
    ///
    /// ⚠ The `.round() + 0.5` puts the line's CENTRE half a point into the row BELOW the bar: a
    /// 1pt stroke centred there covers `bar.bottom().round() ..= bar.bottom().round() + 1`, which
    /// is physical row `round(bar.bottom())` — the first row the bar does NOT own. That is the
    /// right place for it (the chip's bottom edge is `bar.bottom()`, so the rule sits flush under
    /// the chip rather than through it), and it is also why the clip may not be the bar — see
    /// [`Self::hairline_clip`].
    #[must_use]
    pub fn hairline_y(&self) -> f32 {
        self.bar.bottom().round() + 0.5
    }

    /// **The clip rect the hairline must be painted under — the bar, extended DOWN to cover the
    /// line's own pixel row.**
    ///
    /// ⚠ Clipping the hairline to [`Self::bar`] renders NOTHING, and nothing about that is
    /// visible from here: it is the renderer's scissor arithmetic.
    /// `egui-wgpu-0.36.1/src/renderer.rs`'s `ScissorRect::new` rounds each clip edge to a whole
    /// physical pixel (`clip_max_y = round(pixels_per_point * clip_rect.max.y)`) and the pass
    /// renders rows `[clip_min_y, clip_max_y)` — EXCLUSIVE at the bottom. With the bar as the clip
    /// that bound is `round(ppp * bar.bottom())`, and [`Self::hairline_y`]'s line sits in the row
    /// at exactly that index, so the whole stroke is scissored away. The selected chip's break is
    /// the ONLY thing marking the selection (the chip's fill is the panel's own `BG`), so a
    /// hairline that never lands leaves the control with no marker at all rather than merely
    /// missing a rule.
    ///
    /// Extending `max.y` to `hairline_y() + 0.5` — the stroke's own bottom edge — makes the bound
    /// `round(ppp * (round(bar.bottom()) + 1))`, which is strictly above the line's row at every
    /// `pixels_per_point >= 1`. `the_hairline_row_is_scissored_away_by_the_bar_and_survives_its_own_clip`
    /// restates that arithmetic over the real `ScissorRect` rounding and asserts BOTH directions,
    /// so the bug is a regression witness rather than a memory.
    #[must_use]
    pub fn hairline_clip(&self) -> Rect {
        Rect::from_min_max(
            Pos2::new(self.bar.left(), self.bar.top()),
            Pos2::new(self.bar.right(), self.hairline_y() + 0.5),
        )
    }
}

/// What the bar shows at this width. Pure, so the responsive rule is gated without a harness.
#[must_use]
pub fn title_bar_plan(kind: WinKind, bar_width: f32) -> (bool, i8) {
    let wide = bar_width >= TITLE_DROP_W;
    // A kind with no tabs has nothing competing for the row, so its title never drops.
    (!kind.carries_title_tabs() || wide, if wide { PAD_WIDE } else { PAD_TIGHT })
}

/// The chips' rect inside the bar. Pure — `title_right` is where the title (or its icon alone)
/// ended and `controls_left` where `─ □ ✕` begins.
///
/// ⚠ Clamped so a bar too narrow for both never produces an inverted rect: the chips lose to the
/// controls, never the other way round, because a window whose close button has been pushed off
/// its own title bar cannot be closed.
#[must_use]
pub fn tab_slot_rect(bar: Rect, title_right: f32, controls_left: f32) -> Rect {
    let left = (title_right + TAB_GAP).min(controls_left);
    Rect::from_min_max(
        Pos2::new(left, bar.bottom() - TAB_H),
        Pos2::new(controls_left.max(left), bar.bottom()),
    )
}

/// Simple title bar for tool windows: `[icon] name … ─ □ ✕` (draggable). Returns the frame's
/// actions, the drag delta, and the [`TitleTabSlot`] a tabbed body paints its segments into.
///
/// ⚠ The bar is allocated with `Sense::click_and_drag()` FIRST and the controls are drawn on top
/// of it, which is what lets the buttons win the click against the move-drag. A body painting into
/// [`TitleTabSlot::tabs`] later in the same pass wins the same way, for the same reason — egui
/// resolves a hit to the LAST widget registered at that position.
pub fn tool_title_bar(
    ui: &mut egui::Ui,
    kind: WinKind,
    is_max: bool,
) -> (ToolTitleActions, Vec2, TitleTabSlot) {
    let mut a = ToolTitleActions::default();
    let (bar_rect, bar_resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_H), Sense::click_and_drag());
    // (no bottom divider — vike's title bar has none; a tabbed tool paints its own, so that the
    // selected chip has something to break)
    if bar_resp.double_clicked() {
        a.toggle_max = true;
    }
    let drag = if bar_resp.dragged() { bar_resp.drag_delta() } else { Vec2::ZERO };
    let ctrl_w = 3.0 * CTRL_W + CTRL_PAD;
    let (show_title, pad) = title_bar_plan(kind, bar_rect.width());
    let title = ui
        .scope_builder(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    bar_rect.min + egui::vec2(8.0, 1.0),
                    Pos2::new(bar_rect.max.x - ctrl_w, bar_rect.max.y - 1.0),
                ))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.label(RichText::new(kind.icon()).size(15.0));
                if show_title {
                    ui.add_space(6.0);
                    ui.label(
                        vike_ui_theme::font::extralight(kind.label()) // vike title font-weight:200
                            .size(15.0)
                            .color(palette::TEXT_UI),
                    );
                }
            },
        )
        .response
        .rect;
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(bar_rect.shrink2(egui::vec2(0.0, 1.0)))
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.visuals_mut().button_frame = false;
            let ctrl = |ui: &mut egui::Ui, g: &str, tip: &str| -> bool {
                ui.add_sized([CTRL_W, BAR_H - 2.0], Button::new(RichText::new(g).size(15.0)))
                    .on_hover_text(tip)
                    .clicked()
            };
            if ctrl(ui, "✕", "Close") {
                a.close = true;
            }
            if ctrl(ui, if is_max { "❐" } else { "□" }, "Maximize / restore") {
                a.toggle_max = true;
            }
            if ctrl(ui, "─", "Minimize to rail") {
                a.minimize = true;
            }
        },
    );
    let slot = TitleTabSlot {
        bar: bar_rect,
        tabs: tab_slot_rect(bar_rect, title.right(), bar_rect.max.x - ctrl_w),
        pad,
        carries_tabs: kind.carries_title_tabs(),
    };
    (a, drag, slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(width: f32) -> Rect {
        Rect::from_min_size(Pos2::new(100.0, 50.0), egui::vec2(width, BAR_H))
    }

    /// ⚠ **The chips' BOTTOM is the bar's bottom**, at every width and whatever the title did.
    /// That is the whole continuity effect: the selected chip's fourth edge is the gap it makes in
    /// the hairline at [`TitleTabSlot::hairline_y`], which is that same edge. Reddens on a slot
    /// centred in the bar, or one seated on the bar's TOP — both of which look plausible and both
    /// of which put the chip's bottom edge somewhere the hairline is not.
    #[test]
    fn the_tab_slot_is_seated_on_the_bars_bottom_edge() {
        for w in [400.0_f32, 560.0, 1200.0] {
            let b = bar(w);
            let slot = tab_slot_rect(b, b.left() + 60.0, b.right() - 96.0);
            assert_eq!(slot.bottom(), b.bottom(), "width {w}: the chips sit ON the hairline");
            assert!(slot.top() >= b.top(), "width {w}: …and inside the bar: {slot:?}");
        }
        let b = bar(1200.0);
        let slot = TitleTabSlot {
            bar: b,
            tabs: tab_slot_rect(b, b.left() + 60.0, b.right() - 96.0),
            pad: PAD_WIDE,
            carries_tabs: true,
        };
        // The hairline is the chips' own bottom edge, on the pixel grid.
        assert!((slot.hairline_y() - (b.bottom().round() + 0.5)).abs() < f32::EPSILON);
        assert!((slot.hairline_y() - slot.tabs.bottom()).abs() <= 1.0, "{slot:?}");
    }

    /// ⚠ **The chips lose to the window controls, never the other way round.** A bar too narrow
    /// for both must not push `✕` off its own title bar — a window that cannot be closed is a
    /// worse failure than a clipped tab. Reddens on an unclamped `from_min_max`, which yields an
    /// INVERTED rect that egui then paints backwards.
    #[test]
    fn a_bar_too_narrow_for_both_clamps_rather_than_inverting() {
        let b = bar(120.0);
        // The controls take 96pt of a 120pt bar; the title has already eaten what is left.
        let slot = tab_slot_rect(b, b.left() + 80.0, b.right() - 96.0);
        assert!(slot.width() >= 0.0, "never inverted: {slot:?}");
        assert!(slot.right() <= b.right() - 96.0 + 0.001, "the controls keep their room: {slot:?}");
    }

    /// **`egui-wgpu-0.36.1/src/renderer.rs`'s `ScissorRect::new`, restated as the physical ROW
    /// RANGE a render pass will actually touch.** Rounded per edge, `[min, max)` — the exclusive
    /// upper bound is the whole of finding 1.
    fn scissor_rows(clip: Rect, ppp: f32) -> std::ops::Range<i64> {
        let min = (ppp * clip.min.y).round() as i64;
        let max = ((ppp * clip.max.y).round() as i64).max(min);
        min..max
    }

    /// The physical row a 1pt stroke centred on `y` lands in — its centre's row, which is the row
    /// that has to survive for ANY of the stroke to be seen.
    fn stroke_row(y: f32, ppp: f32) -> i64 {
        (ppp * y).floor() as i64
    }

    /// ⚠⚠ **THE HAIRLINE IS THE WHOLE DESIGN, AND CLIPPED TO THE BAR IT RENDERS NOTHING.**
    ///
    /// The selected chip is filled with `palette::BG` — the same ground the title bar stands on —
    /// and draws only its left, top and right edges. Its missing FOURTH edge is the gap it makes
    /// in this line, and that gap is the only thing marking the selection. So a hairline that is
    /// scissored away does not cost a divider: it costs the selection marker, and the frame reads
    /// as two inert words.
    ///
    /// This is arithmetic rather than a picture because it has to be: no CI runner has a GPU, and
    /// a clipped line reaches no accessibility tree. The model is `ScissorRect::new`'s own
    /// rounding ([`scissor_rows`]), driven over fractional bar bottoms (a window sits wherever the
    /// user dragged it) and over the `pixels_per_point` values a real display hands egui.
    ///
    /// Both directions are asserted on purpose. The witness half is what stops the survival half
    /// from passing against a clip that never needed widening.
    ///
    /// ⚠ **The witness is stated at two strengths, because only one of them is universal and the
    /// first draft of this test claimed the stronger one everywhere and went red.** At
    /// `pixels_per_point == 1` — the ordinary case, and the one the reviewers argued — the bar's
    /// clip excludes the stroke's row OUTRIGHT, at every bar bottom: the bound is `round(bottom)`
    /// and the row is `floor(round(bottom) + 0.5)`, which is the same integer, and the range is
    /// half-open. At fractional scales the rounding sometimes admits that row by luck (bottom
    /// `80.4` at `1.25x`: bound `round(100.5) = 101`, row `100`), so "nothing renders" is not true
    /// there — what IS true at every scale is that the bar's bound falls strictly INSIDE the
    /// stroke, so part of the line is always cut. The defect is therefore "invisible at 1x and
    /// unreliable everywhere else", which is worse than a clean always-broken, not better: it is
    /// the shape that survives a glance at one machine.
    #[test]
    fn the_hairline_row_is_scissored_away_by_the_bar_and_survives_its_own_clip() {
        for offset in [0.0_f32, 0.1, 0.25, 0.4, 0.5, 0.6, 0.75, 0.9] {
            let b = Rect::from_min_size(Pos2::new(100.0, 50.0 + offset), egui::vec2(800.0, BAR_H));
            let slot = TitleTabSlot {
                bar: b,
                tabs: tab_slot_rect(b, b.left() + 60.0, b.right() - 96.0),
                pad: PAD_WIDE,
                carries_tabs: true,
            };
            // The stroke's own bottom edge, in points — where the pass has to reach for the whole
            // 1pt line to be drawn.
            let stroke_bottom = slot.hairline_y() + 0.5;
            for ppp in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
                let row = stroke_row(slot.hairline_y(), ppp);
                // The UNIVERSAL witness: the bar's scissor bound cuts into the stroke at every
                // scale, so the hairline is never drawn whole under it.
                assert!(
                    (scissor_rows(slot.bar, ppp).end as f32) < ppp * stroke_bottom,
                    "bar bottom {} @ {ppp}x: clipping to the BAR must CUT the stroke — its bound \
                     {:?} has to fall inside the line's own extent (…{}). An assertion that \
                     stopped failing here would mean the survival check below proves nothing",
                    slot.bar.bottom(),
                    scissor_rows(slot.bar, ppp),
                    ppp * stroke_bottom,
                );
                // …and at 1x it takes the whole thing: the centre row is outside the range.
                if ppp == 1.0 {
                    assert!(
                        !scissor_rows(slot.bar, ppp).contains(&row),
                        "bar bottom {} @ 1x: clipping to the BAR scissors row {row} away entirely \
                         (rows {:?}) — nothing of the hairline reaches a pixel, which is the \
                         shipped bug",
                        slot.bar.bottom(),
                        scissor_rows(slot.bar, ppp),
                    );
                }
                assert!(
                    scissor_rows(slot.hairline_clip(), ppp).contains(&row),
                    "bar bottom {} @ {ppp}x: the hairline's own clip must RENDER row {row} \
                     (rows {:?}, clip {:?})",
                    slot.bar.bottom(),
                    scissor_rows(slot.hairline_clip(), ppp),
                    slot.hairline_clip(),
                );
            }
            // …and the widening is exactly one stroke, not a clip that spills into the panel: it
            // reaches the line's bottom edge and stops.
            assert!(
                (slot.hairline_clip().max.y - (slot.hairline_y() + 0.5)).abs() < f32::EPSILON,
                "{slot:?}"
            );
            assert_eq!(slot.hairline_clip().min, slot.bar.min, "the bar's own top-left: {slot:?}");
            assert!(
                slot.hairline_clip().max.y - slot.bar.max.y <= 1.5,
                "at most the stroke's own row below the bar: {slot:?}"
            );
        }
    }

    /// The responsive rule: a tabbed window drops its TITLE below the breakpoint and tightens its
    /// chips; a window with no tabs keeps its title at every width, because nothing is competing
    /// for the row.
    #[test]
    fn the_title_drops_only_for_a_tabbed_kind_and_only_below_the_breakpoint() {
        assert_eq!(title_bar_plan(WinKind::Connections, TITLE_DROP_W), (true, PAD_WIDE));
        assert_eq!(title_bar_plan(WinKind::Connections, TITLE_DROP_W - 1.0), (false, PAD_TIGHT));
        assert_eq!(title_bar_plan(WinKind::Connections, 400.0), (false, PAD_TIGHT));
        // …and a kind that seats no tabs is unaffected at the same widths.
        assert!(title_bar_plan(WinKind::News, 400.0).0);
        assert!(title_bar_plan(WinKind::Chart, 120.0).0);
    }
}
