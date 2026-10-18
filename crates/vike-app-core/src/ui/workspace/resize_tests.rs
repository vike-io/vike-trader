use super::*;
use egui::{Context, Event, PointerButton, RawInput, pos2, vec2};
use std::cell::Cell;
use vike_ui_theme::value::status_bar;

/// The headless viewport, and the DEFAULT workspace bounds `show_window` clamps into (the app
/// passes its desktop rect). Big enough that no fixture window is ever clamped or constrained
/// — which is why the arena CEILING needs [`arena`] to be seen at all.
fn screen() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))
}

/// The app's real window ARENA, shaped like the one `vike-desktop`'s `draw_chrome` computes:
/// the viewport minus the caption/menu strip at the top and minus the fixed-height status bar
/// at the bottom (`crate::ui::status_bar`'s `egui::Panel::bottom("statusbar")`, which `app_ui.rs`
/// adds BEFORE the `CentralPanel`, so `app.desktop = ui.max_rect()` already excludes it — MEASURED in
/// `egui-0.36.1/src/containers/panel.rs`, whose `show_inside_dyn` sets
/// `cursor.max[axis] = visible_outer_rect.min[axis]` for a `PanelSide::Bottom` before
/// `CentralPanel` reads `available_rect_before_wrap`).
///
/// ⚠ It is deliberately SMALLER than [`screen`] on both axes: the viewport stays 1600×1000 so
/// a window can be painted outside the arena without leaving the context's own screen rect,
/// which is exactly the shape the reported bug has.
fn arena() -> Rect {
    Rect::from_min_max(pos2(0.0, 40.0), pos2(1200.0, 1000.0 - status_bar::STATUS_BAR_H))
}

/// The title strip a real tool window draws before its body
/// (`crates/vike-app-core/src/ui/workspace/title_bar.rs`'s `tool_title_bar`, which moved down out
/// of vike-desktop when it started reserving the Connections window's tab slot), modelled as a
/// plain full-width allocation. Its only load-bearing property is
/// that it clears `window_resize_handles`' `workspace::TITLE_CLEAR` — which is what keeps the side bands
/// off the real title bar's move-drag and its control buttons.
const TITLE_H: f32 = 24.0;

/// The `item_spacing` the shipped app sets on every style — READ from
/// `vike_ui_theme::appearance::install`'s own result rather than copied from it (`(6, 4)` at the
/// default density; egui's own default is `(8, 3)`). The `y` component is what a LADDER body
/// over-allocates by, once per gap between its siblings; see [`Site::ToolFillsSpaced`] and
/// [`LADDER_SIBLINGS`].
fn shipped_item_spacing() -> Vec2 {
    let ctx = Context::default();
    vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
    ctx.global_style().spacing.item_spacing
}

/// The sibling counts of the shipped LADDER bodies, each with the kind it belongs to, and
/// the OVERRUN each therefore produces past the rect its body was offered:
/// `(siblings - 1) * shipped_item_spacing().y`.
///
/// * `vike_panels::cockpit::ladder::draw` — `ladder_header` (the density's header height, 24 at Normal) /
///   the region, two allocations
///   summing the same way, so 1 gap = **4pt**. (The cockpit's rail, PTB header and ticket are
///   drawn BEFORE the ladder and consume only their own heights; the ladder reads
///   `available_rect_before_wrap` after them, so their gaps are already in the cursor and
///   contribute nothing.)
///
/// The Trade window is not a ladder shape in this sense: `vike_panels::trade::draw` allocates its
/// whole rect ONCE and draws its regions into clipped children, so it has no sibling gap and no
/// overrun. The DOM's 12 pt overflow (finding F7) went with the DOM.
const LADDER_SIBLINGS: [(&str, usize); 1] = [("Polymarket cockpit", 2)];

/// The width of a FLOATING scrollbar's interact strip: `egui-0.36.1/src/style.rs`'s
/// `ScrollStyle::floating()` sets `bar_width: 10.0`, and `scroll_area.rs` senses the bar over
/// `max_bar_rect` — `outer_rect.with_min_x(max_cross - full_width)`, i.e. the outermost
/// `bar_width` points of the scroll area, whatever width the bar is currently ANIMATED to.
const BAR_W: f32 = 10.0;

/// Which vike-desktop CALL SITE a fixture models. There are two of them and the split is not
/// by kind: `app_ui.rs`'s `if w.kind == workspace::WinKind::Chart` branch draws ONE of the
/// three `fills` kinds, and the other two — Trade and Polymarket — are dispatched from the TOOL
/// site alongside the nine intrinsic-height kinds. A harness that modelled only the chart site
/// for `fills` was therefore modelling the wrong site for two of the three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Site {
    /// The CHART call site: a `fills` body covering `ui.max_rect()` exactly, no margin frame,
    /// no wrapper at all.
    Chart,
    /// The TOOL call site for a NON-`fills` kind: title strip, [`TOOL_BODY_MARGIN`], then a
    /// fixed-size body handed through [`BodyBounds::show`].
    Tool,
    /// The TOOL call site for a `fills` kind (Trade, Polymarket): the same margin frame, a body
    /// that covers the frame's available rect EXACTLY, handed through [`BodyBounds::show`] —
    /// which must hand a `fills` body straight through.
    ///
    /// ⚠ It is the ID-CHAIN and WRAPPER reference, and a faithful model of the Trade window's
    /// geometry (one allocation of the whole rect), but not of the Polymarket ladder's. That
    /// ladder reads `available_rect_before_wrap` once and then lays out siblings summing to it,
    /// so it ends up PAST that rect by its own item-spacing gaps — this fixture lands on it
    /// exactly. Use [`Site::ToolFillsSpaced`] for anything that turns on where the body ends.
    ToolFills,
    /// The SAME site with [`BodyBounds::show`] BYPASSED — origin/main's spelling, and the
    /// reference `ToolFills`'s id chain, bands and painted rect are compared against.
    ToolFillsBare,
    /// The TOOL site with a LADDER-shaped `fills` body: `N` sibling allocations SUMMING to the
    /// rect the body read once at the top, which is the shape the Polymarket cockpit's ladder
    /// has (`vike_panels::cockpit::ladder::draw`; the DOM's was the other, and is gone). The
    /// `item_spacing.y` gaps BETWEEN those siblings were never subtracted, so the body
    /// over-allocates past its available rect by `(N - 1)` of them.
    ///
    /// ⚠ [`Site::ToolFills`] cannot express that and never could: its body allocates exactly
    /// the rect it was offered, so its `min_rect == max_rect` is a property of the FIXTURE.
    /// The doc's "Chart / Trade / Polymarket are byte-identical" claim was gated by that
    /// fixture, and so held for a reason no shipped ladder has. The claim itself SURVIVES —
    /// `Region::expand_to_include_rect` unions the overrun into `max_rect` as well, so both
    /// anchors end on it — and this fixture is what makes that falsifiable instead of
    /// tautological. See
    /// `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`.
    ToolFillsSpaced(usize),
}

/// One scripted window: a persistent `Context`, a pointer script, and a body whose size the
/// test chooses. The draw closure MIRRORS one of the two vike-desktop call sites — see
/// [`Site`].
struct Harness {
    ctx: Context,
    time: f64,
    ptr: Option<Pos2>,
    queued: Vec<Event>,
    /// Last frame's `BodyBounds::scrolls`, exactly as `show_window` computed it.
    scrolled: Cell<bool>,
    /// The size the window BODY lays out at, below the title strip. Ignored by the `fills`
    /// sites, whose bodies cover whatever rect they are offered.
    body: Vec2,
    /// Which call site this fixture draws as.
    site: Site,
    /// `ui.min_rect()` / `ui.max_rect()` of the window's content `Ui` at the moment
    /// `window_resize_handles` reads one, so a test can prove WHICH rect the bands anchor on.
    ///
    /// ⚠ BOTH are captured where that function reads them — AFTER the body — and `content_max`
    /// captured them at the top of the closure until 2026-09-13, which is a different rect for
    /// any body that OVERFLOWS: `egui-0.36.1/src/layout.rs`'s `Region::expand_to_include_rect`
    /// unions the child's rect into `min_rect` AND `max_rect` alike, so `max_rect` absorbs the
    /// overflow before the bands are placed. Reading the top-of-closure value made the two
    /// anchors look as though they diverged for the ladder bodies when the production function
    /// sees them EQUAL — and a claim was written on it. [`Self::offered_max`] is that
    /// top-of-closure rect, kept under a name that says what it is.
    content_min: Cell<Rect>,
    content_max: Cell<Rect>,
    /// The rect the window's content `Ui` was OFFERED, read before a single child is added —
    /// `Resize::begin`'s `inner_rect`. A body that over-allocates ends past it; see
    /// [`Site::ToolFillsSpaced`]. It is NOT what the bands anchor on under either spelling.
    offered_max: Cell<Rect>,
    /// The BODY `Ui`'s `max_rect` — the `ScrollArea`'s `inner_rect`, which is where its
    /// floating scrollbars sense.
    body_max: Cell<Rect>,
    /// The BODY `Ui`'s `Ui::id` — the id every auto-id widget inside the body is keyed on.
    body_id: Cell<Option<Id>>,
    /// The workspace bounds handed to `show_window` — the app's `desktop` rect. Defaults to
    /// [`screen`] (so no fixture is ever constrained); [`Harness::in_arena`] narrows it to
    /// [`arena`] for the tests that turn on a window meeting the arena's edge.
    bounds: Rect,
}

impl Harness {
    fn tool(body: Vec2) -> Self {
        Self::with(body, Site::Tool)
    }

    fn fill() -> Self {
        Self::with(Vec2::ZERO, Site::Chart)
    }

    /// A `fills` kind drawn at the TOOL call site (Trade / Polymarket). `wrapped` picks the
    /// production spelling ([`BodyBounds::show`]) or origin/main's bare one.
    fn tool_fill(wrapped: bool) -> Self {
        Self::with(Vec2::ZERO, if wrapped { Site::ToolFills } else { Site::ToolFillsBare })
    }

    /// A LADDER-shaped `fills` body at the TOOL call site: `siblings` allocations summing to
    /// the rect it read at the top — see [`Site::ToolFillsSpaced`].
    fn ladder(siblings: usize) -> Self {
        Self::with(Vec2::ZERO, Site::ToolFillsSpaced(siblings))
    }

    fn with(body: Vec2, site: Site) -> Self {
        let ctx = Context::default();
        // The SHIPPED style, installed the way the app installs it rather than copied value by
        // value. Two of its values are geometry every band here depends on: the app zeroes the
        // window margin (the chart-title flush fix; egui's default is 6), without which every
        // assertion below would measure a different frame inset than production and
        // `fixed_size` (an INNER content size) would disagree with the OUTER rect `show_window`
        // reads back by twice that margin; and `item_spacing` is `(6, 4)`, not egui's `(8, 3)`,
        // which a ladder body over-allocates by once per sibling gap — a harness on the default
        // would measure a shift the app never produces. See [`Site::ToolFillsSpaced`].
        vike_ui_theme::appearance::install(&ctx, &vike_ui_theme::appearance::Appearance::default());
        Self {
            ctx,
            time: 0.0,
            ptr: None,
            queued: Vec::new(),
            scrolled: Cell::new(false),
            body,
            site,
            content_min: Cell::new(Rect::NOTHING),
            content_max: Cell::new(Rect::NOTHING),
            offered_max: Cell::new(Rect::NOTHING),
            body_max: Cell::new(Rect::NOTHING),
            body_id: Cell::new(None),
            bounds: screen(),
        }
    }

    /// Narrow the workspace bounds this fixture's window lives in to the app-shaped
    /// [`arena`] — the viewport minus the caption strip and the status bar.
    fn in_arena(mut self) -> Self {
        self.bounds = arena();
        self
    }

    /// Draw one frame of this window.
    fn frame(&mut self, w: &mut WinState) {
        let mut raw =
            RawInput { screen_rect: Some(screen()), time: Some(self.time), ..Default::default() };
        self.time += 1.0 / 60.0;
        if let Some(p) = self.ptr {
            raw.events.push(Event::PointerMoved(p));
        }
        raw.events.append(&mut self.queued);
        self.ctx.begin_pass(raw);
        let scrolled = &self.scrolled;
        let content_min = &self.content_min;
        let content_max = &self.content_max;
        let offered_max = &self.offered_max;
        let body_max = &self.body_max;
        let body_id = &self.body_id;
        let body = self.body;
        let site = self.site;
        show_window(&self.ctx, w, self.bounds, |ui, bounds| {
            scrolled.set(bounds.scrolls());
            offered_max.set(ui.max_rect());
            let _ = ui.allocate_space(vec2(ui.max_rect().width(), TITLE_H));
            // A `fills` body covers whatever rect it is offered — the chart's `leftover` fill;
            // the Trade window and the Polymarket ladder sized to `available_rect_before_wrap`.
            let fill_body = |ui: &mut egui::Ui| {
                body_max.set(ui.max_rect());
                body_id.set(Some(ui.id()));
                let full = ui.max_rect();
                let _ = ui.allocate_rect(full, egui::Sense::hover());
            };
            match site {
                // The CHART call site: no margin frame, and no `BodyBounds` wrapper at all.
                Site::Chart => fill_body(ui),
                Site::Tool => {
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                        bounds.show(ui, |ui| {
                            body_max.set(ui.max_rect());
                            body_id.set(Some(ui.id()));
                            let _ = ui.allocate_space(body);
                        });
                    });
                }
                // The TOOL call site with a `fills` kind, through the production wrapper...
                Site::ToolFills => {
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                        bounds.show(ui, fill_body);
                    });
                }
                // ...and the same site with the wrapper bypassed (origin/main).
                Site::ToolFillsBare => {
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, fill_body);
                }
                // The LADDER shape: read the available rect ONCE, then lay out `n` siblings
                // whose heights SUM to it — exactly `vike_panels::cockpit::ladder::draw`'s
                // `ladder_header`/`region`. It does not subtract the `item_spacing.y` egui
                // inserts BETWEEN them, so the body ends `(n - 1)` gaps past the rect it read.
                Site::ToolFillsSpaced(n) => {
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                        bounds.show(ui, |ui| {
                            body_max.set(ui.max_rect());
                            body_id.set(Some(ui.id()));
                            let full = ui.available_rect_before_wrap();
                            let each = full.height() / n as f32;
                            for _ in 0..n {
                                let _ = ui.allocate_exact_size(
                                    vec2(full.width(), each),
                                    egui::Sense::hover(),
                                );
                            }
                        });
                    });
                }
            }
            // `window_resize_handles` runs immediately after this closure returns and nothing
            // between the two touches the placer, so THESE are the two rects it chooses
            // between — `min_rect` for the anchor, `max_rect` for the fallback and for the
            // spelling origin/main used. ⚠ `max_rect` is read HERE rather than at the top,
            // because it is not the same rect: every child the cursor advances past is unioned
            // into it (`Region::expand_to_include_rect`), so an overflowing body moves it.
            content_min.set(ui.min_rect());
            content_max.set(ui.max_rect());
            Vec2::ZERO // this fixture's title strip is inert — it never move-drags
        });
        // A headless pass harvests no textures; say "deliberately not rendered" rather than
        // letting `TexturesDelta`'s drop panic on unapplied deltas (egui 0.36).
        self.ctx.end_pass().drop_without_applying_deltas();
    }

    fn frames(&mut self, w: &mut WinState, n: usize) {
        for _ in 0..n {
            self.frame(w);
        }
    }

    /// The rect `window_resize_handles` actually allocated for `mask` — read back out of the
    /// context rather than recomputed here, so the test grabs the PRODUCTION band and cannot
    /// drift from its `BAND`/`workspace::TITLE_CLEAR` arithmetic. `None` when no band was allocated.
    fn band(&self, w: &WinState, mask: u8) -> Option<Rect> {
        self.ctx.read_response(w.id.with(("winresize", mask))).map(|r| r.rect)
    }

    /// The rect the window was actually PAINTED at (egui's own area geometry) — what a user
    /// sees, and NOT the same thing as `w.size` once the latch has taken hold.
    ///
    /// ⚠ It is the window FRAME's rect, so it is [`Self::window_stroke`] points LARGER than
    /// the content rect the bands anchor on, on every side.
    fn painted(&self, w: &WinState) -> Rect {
        self.ctx.memory(|m| m.area_rect(w.id)).expect("the window has been shown")
    }

    /// The window frame's stroke width. `egui-0.36.1/src/containers/frame.rs` counts it as
    /// part of the frame's total margin (`content_rect + inner_margin +
    /// MarginF32::from(self.stroke.width) + outer_margin`), and `Frame::window` takes it from
    /// `visuals.window_stroke()` — so an area rect sits exactly this far outside the content
    /// rect `ui.min_rect()` reports, on every side. Read from the live style rather than
    /// written down, because it is a theme value.
    fn window_stroke(&self) -> f32 {
        self.ctx.style_of(self.ctx.theme()).visuals.window_stroke.width
    }

    fn move_to(&mut self, at: Pos2) {
        self.ptr = Some(at);
    }

    fn button(&mut self, at: Pos2, pressed: bool) {
        self.ptr = Some(at);
        self.queued.push(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }

    /// Press at `from` and move to `to` inside ONE frame's `RawInput` — what a high-polling
    /// mouse routinely delivers. `to` must stay inside the band: egui hit-tests with
    /// `interact_pos`, which every pointer event in the frame updates
    /// (`egui-0.36.1/src/input_state/mod.rs`), so the widget picked up is the one under the
    /// FINAL position.
    fn press_and_move(&mut self, from: Pos2, to: Pos2) {
        self.ptr = Some(from);
        self.queued.push(Event::PointerButton {
            pos: from,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        self.queued.push(Event::PointerMoved(to));
    }
}

/// Grab the `mask` band and drag it by `by`, one mouse step per frame: hover, press, move,
/// release. `Sense::drag()` alone reports `dragged()` from the PRESS frame on
/// (`egui-0.36.1/src/interaction.rs`: "This widget is just sensitive to drags, so we can mark
/// it as dragged right away"), and `Response::drag_delta` is that frame's pointer delta — so
/// the press frame hands `show_window` a ZERO delta and the move frame hands it exactly `by`.
fn drag_band(h: &mut Harness, w: &mut WinState, mask: u8, by: Vec2) {
    let at = h
        .band(w, mask)
        .expect("the band must have been allocated before it can be grabbed")
        .center();
    h.move_to(at);
    h.frame(w); // hover
    h.button(at, true);
    h.frame(w); // press — `dragged()` but a ZERO delta, so it must NOT latch
    h.move_to(at + by);
    h.frame(w); // the drag itself
    h.button(at + by, false);
    h.frame(w); // release
}

/// A tool window spawned at `size`, settled past `OPEN_PIN_FRAMES` so `force_frames` has
/// drained and the bands are allocated.
fn settled_tool(h: &mut Harness, kind: WinKind, at: Pos2, size: Vec2) -> WinState {
    let mut w = WinState::tool("fixture", kind, Rect::from_min_size(at, size));
    h.frames(&mut w, 14);
    w
}

#[path = "resize_tests/geometry.rs"]
#[cfg(test)]
mod geometry;

#[path = "resize_tests/latch.rs"]
#[cfg(test)]
mod latch;
