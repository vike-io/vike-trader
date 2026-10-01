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
//! A tool whose body is tabbed ([`crate::ui::workspace::WinKind::carries_title_tabs`]) does not draw a
//! second row of tabs under the bar — it seats its segmented control INSIDE the bar, between the
//! window title and the three window controls. [`tool_title_bar`] does not draw those segments (it
//! cannot: their labels carry live counts the body computes), it RESERVES their rect and hands it
//! back as a [`TitleTabSlot`]. The body then paints into it —
//! `vike_app_core::ui::tool_views::title_bar_tabs`, one frame, same pass, so the counts on the chips
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
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::theme::Theme;
use vike_ui_theme::type_scale::TextRole;

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
/// width a tool window opens at (`crate::ui::window_spawn`'s `vec2(560.0, 400.0)`), so the narrow
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
    // The installed appearance: the window's name and its controls are the Title role, like the
    // chart window's (owner decision 3 of the design system's step-7 plan), in the theme's UI text.
    let look = vike_ui_theme::appearance::current(ui.ctx());
    let title_px = look.text_size.px(TextRole::Title);
    let title_col = Theme::of(look.theme).text_ui;
    let mut a = ToolTitleActions::default();
    let (bar_rect, bar_resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_H), Sense::click_and_drag());
    // The header background: nothing unless the header gradient is on (design system spec §2),
    // then the theme's gradient under the title — `vike_ui_theme::header`'s module doc says why its
    // corners are rounded to the frame's inner radius.
    vike_ui_theme::header::paint_header_background(
        ui.painter(),
        bar_rect,
        &vike_ui_theme::appearance::current(ui.ctx()),
    );
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
                ui.label(kind.icon().rich().size(title_px));
                if show_title {
                    ui.add_space(6.0);
                    ui.label(RichText::new(kind.label()).size(title_px).color(title_col));
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
            let ctrl = |ui: &mut egui::Ui, icon: Icon, tip: &str| -> bool {
                let button = Button::new(icon.rich().size(title_px));
                icons::named(ui.add_sized([CTRL_W, BAR_H - 2.0], button), tip).clicked()
            };
            if ctrl(ui, icons::CLOSE, "Close") {
                a.close = true;
            }
            if ctrl(ui, if is_max { icons::RESTORE } else { icons::MAXIMIZE }, "Maximize / restore")
            {
                a.toggle_max = true;
            }
            if ctrl(ui, icons::MINIMIZE, "Minimize to rail") {
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

#[path = "title_bar_tests.rs"]
#[cfg(test)]
mod title_bar_tests;
