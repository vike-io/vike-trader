//! **The tool window's title bar — and the slot inside it a tabbed tool seats its segments in.**
//!
//! # Why this lives here rather than in the shell
//!
//! It was `vike-desktop`'s `main.rs` until the Connections tabs moved into the bar. That crate is
//! in `xtask::ci::tables`' `EXCLUDE_FROM_CI` and its whole coverage is the `app-check` job — a
//! `cargo check`, clippy, and a nextest pass that reaches `crates/vike-desktop/src/chart_gpu.rs`
//! and nothing else — so the title bar's GEOMETRY was gated by nothing at all. Every decision it
//! makes is CPU work (`egui::Context::run_ui` computes layout with no GPU), so moving the whole
//! function one crate down puts it on the GPU-less CI runners instead of a shell nothing gated.
//! What is left at the call site is the two-line mapping into the shell's own `TitleActions`.
//!
//! ⚠ That is arm 1 of `crates/vike-ops/tests/gui/ci_excluded_gui_shell_ratchet/ratchet.rs`'s own
//! `GROWTH_GUIDANCE` applied rather than argued around: the tabs needed room in the shell, and the
//! shell's ceiling had six code lines of slack.
//!
//! # The tab slot, and what it is FOR
//!
//! A tool whose body is tabbed ([`crate::ui::workspace::WinKind::carries_title_tabs`]) does not draw a
//! second row of tabs under the bar — it seats its segmented control INSIDE the bar, between the
//! window title and the three window controls. [`tool_title_bar`] does not draw those segments (it
//! cannot: their labels carry live counts the body computes), it RESERVES their rect and hands it
//! back as a [`TitleTabSlot`]. The body then paints into it, one frame, same pass, so the counts
//! on the chips are that frame's rather than last frame's — `vike_app_core::ui::tool_views::
//! connections`'s `title_bar_tabs` did exactly this until Connections-merges-into-Data-Manager
//! deleted it (2026-10-05) along with the only window whose tabs these were. The slot exists for
//! the next tabbed kind to paint into, not because one does today.
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
//! * **Below `title_bar::TITLE_DROP_W` the window TITLE drops** and the segments tighten
//!   ([`TitleTabSlot::pad`]), which is what makes two segments fit in a title bar on a ~400pt
//!   window. The icon stays: it is the only thing left that says which window this is. A kind
//!   whose segments the bar can MEASURE (the Trade window's three view-control icons) keeps its
//!   title below the breakpoint for as long as title and segments both fit, which is its 320pt
//!   layout too.
//!
//! A kind that carries no tabs keeps its title at every width and gets a slot it never reads.

use super::state::WinKind;
use egui::{Align, Button, Layout, Pos2, Rect, RichText, Sense, UiBuilder, Vec2};
use vike_panels::trade::layout::VIEW_CONTROLS_W;
use vike_ui_theme::chrome;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::theme::Theme;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::title_bar;

/// The gap between the window title (or its icon, once the title has dropped) and the first chip.
const TAB_GAP: f32 = space::XL;

/// The gap between a tool window's title bar and its body: the density's `gap` for every kind but
/// the Trade window, whose bar ends where its body begins (the design's instrument band carries its
/// own padding and sits directly under the title). Every OTHER window's body has no band of its own,
/// so it keeps the gap.
#[must_use]
pub fn body_gap(kind: WinKind, density_gap: f32) -> f32 {
    if flush(kind) { 0.0 } else { density_gap }
}

/// Whether `kind`'s body starts at the bar's bottom edge.
fn flush(kind: WinKind) -> bool {
    matches!(kind, WinKind::Trade)
}

/// What the bar's right-hand cluster takes: the three window controls and, for a kind that seats
/// segments between its title and them, the room for the hairline that parts the two.
fn cluster_w(kind: WinKind) -> f32 {
    chrome::CONTROLS_W + if kind.carries_title_tabs() { chrome::SLOT_TO_CONTROLS } else { 0.0 }
}

/// The chip's horizontal padding at each width — wide, then tightened.
const PAD_WIDE: i8 = space::LG as i8;
const PAD_TIGHT: i8 = space::MD as i8;

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
    /// `title_bar::TITLE_DROP_W`. [`title_bar_plan`] is where the two values live.
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
    let wide = bar_width >= title_bar::TITLE_DROP_W;
    // A kind with no tabs has nothing competing for the row, so its title never drops.
    (!kind.carries_title_tabs() || wide, if wide { PAD_WIDE } else { PAD_TIGHT })
}

/// How wide the segments a kind seats in its bar are, when the BAR can know it: the Trade window's
/// three view-control icons (`vike_panels::trade::layout::VIEW_CONTROLS_W`, the width its glue right-
/// aligns them in). `None` for every other kind — a kind that carries no tabs seats no segments at
/// all, and no surviving tabbed kind besides Trade carries chips whose width the bar can know ahead
/// of painting them (the old Connections tabs did not; that kind is deleted, 2026-10-05). Exhaustive,
/// so a new kind has to say.
fn segments_w(kind: WinKind) -> Option<f32> {
    match kind {
        WinKind::Trade => Some(VIEW_CONTROLS_W),
        WinKind::Chart
        | WinKind::Account
        | WinKind::Options
        | WinKind::Greeks
        | WinKind::News
        | WinKind::Calendar
        | WinKind::Data
        | WinKind::Studio
        | WinKind::Tearsheet
        | WinKind::Polymarket
        | WinKind::Settings => None,
    }
}

/// Whether a title ending at `title_right` still leaves `segments_w` of slot before
/// `controls_left`. Pure: [`tool_title_bar`] keeps a narrow window's title when this holds (minor 34
/// of the Trade window plan: the Trade window's three icons fit beside "Trade" at its 320pt layout,
/// where [`title_bar_plan`]'s breakpoint would drop it).
fn title_fits_beside(title_right: f32, segments_w: f32, controls_left: f32) -> bool {
    title_right + TAB_GAP + segments_w <= controls_left
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
        Pos2::new(left, bar.bottom() - title_bar::TAB_H),
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
    // The installed appearance: every window's name and controls are the Body role (the design's
    // 11 px at Standard), in the theme's UI text — the v3 design's bar is small and the same for
    // every window.
    let look = vike_ui_theme::appearance::current(ui.ctx());
    let px = look.text_size.px(TextRole::Body);
    let theme = Theme::of(look.theme);
    let mut a = ToolTitleActions::default();
    let (bar_rect, bar_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), chrome::TITLE_BAR_H),
        Sense::click_and_drag(),
    );
    // The header background: nothing unless the header gradient is on (design system spec §2),
    // then the theme's gradient under the title — `vike_ui_theme::header`'s module doc says why its
    // corners are rounded to the frame's inner radius.
    vike_ui_theme::header::paint_header_background(ui.painter(), bar_rect, &look);
    // (no bottom divider — vike's title bar has none; a tabbed tool paints its own, so that the
    // selected chip has something to break)
    if bar_resp.double_clicked() {
        a.toggle_max = true;
    }
    let drag = if bar_resp.dragged() { bar_resp.drag_delta() } else { Vec2::ZERO };
    let ctrl_w = cluster_w(kind);
    let (wide, pad) = title_bar_plan(kind, bar_rect.width());
    // Below the breakpoint a kind whose segments the bar can measure keeps its title while both
    // fit: where the title would END is measured with the label's own layout (inset, name), never
    // estimated.
    let show_title = wide
        || segments_w(kind).is_some_and(|need| {
            // `FontSelection::Default` is the fallback `ui.label` lays a `RichText` out with.
            let wrap = Some(egui::TextWrapMode::Extend);
            let name = egui::WidgetText::from(RichText::new(kind.label()).size(px)).into_galley(
                ui,
                wrap,
                f32::INFINITY,
                egui::FontSelection::Default,
            );
            let title_right = bar_rect.min.x + chrome::TITLE_INSET + name.size().x;
            title_fits_beside(title_right, need, bar_rect.max.x - ctrl_w)
        });
    let title = ui
        .scope_builder(
            UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    bar_rect.min + egui::vec2(chrome::TITLE_INSET, space::HAIR),
                    Pos2::new(bar_rect.max.x - ctrl_w, bar_rect.max.y - space::HAIR),
                ))
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                // The name; the kind's icon only where the name has dropped, so that a window still
                // says what it is (the design has no icon beside the name).
                if show_title {
                    ui.label(RichText::new(kind.label()).size(px).color(theme.text_ui));
                } else {
                    ui.label(kind.icon().rich().size(px));
                }
            },
        )
        .response
        .rect;
    if kind.carries_title_tabs() {
        // The hairline parting the slot's segments from the window's own controls, centred in the bar.
        let h = chrome::SEPARATOR_H;
        let sep = Rect::from_min_size(
            Pos2::new(
                bar_rect.max.x - ctrl_w + chrome::SLOT_TO_SEPARATOR,
                bar_rect.center().y - h / 2.0,
            ),
            egui::vec2(stroke::HAIRLINE, h),
        );
        ui.painter().rect_filled(sep, 0.0, theme.border);
    }
    let controls = Rect::from_min_max(
        bar_rect.min,
        Pos2::new(bar_rect.max.x - chrome::WINDOW_CONTROLS_PAD, bar_rect.max.y),
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(controls.shrink2(egui::vec2(space::NONE, space::HAIR)))
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = chrome::WINDOW_CONTROL_GAP;
            ui.visuals_mut().button_frame = false;
            let ctrl = |ui: &mut egui::Ui, icon: Icon, tip: &str| -> bool {
                let button = Button::new(icon.rich().size(px));
                icons::named(ui.add_sized(Vec2::splat(chrome::WINDOW_CONTROL), button), tip)
                    .clicked()
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
    if flush(kind) {
        // The cursor is wherever the scopes above left it, an item gap under the bar: put it ON the
        // bar's bottom edge, where this kind's body starts.
        ui.add_space(bar_rect.max.y - ui.cursor().min.y);
    }
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
