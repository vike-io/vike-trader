use super::*;
use crate::ui::status_bar::STATUS_BAR_H;
use egui::{Context, Event, PointerButton, RawInput, pos2, vec2};
use std::cell::Cell;

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
    Rect::from_min_max(pos2(0.0, 40.0), pos2(1200.0, 1000.0 - STATUS_BAR_H))
}

/// The title strip a real tool window draws before its body
/// (`crates/vike-app-core/src/ui/workspace/title_bar.rs`'s `tool_title_bar`, which moved down out
/// of vike-desktop when it started reserving the Connections window's tab slot), modelled as a
/// plain full-width allocation. Its only load-bearing property is
/// that it clears `window_resize_handles`' `TITLE_CLEAR` — which is what keeps the side bands
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

/// The sibling counts of the two shipped LADDER bodies, each with the kind it belongs to, and
/// the OVERRUN each therefore produces past the rect its body was offered:
/// `(siblings - 1) * shipped_item_spacing().y`.
///
/// * `vike_panels::dom::draw` — `header` (26) / `toolbar` (26) / the ladder region /
///   `footer` (28), four allocations whose heights sum to the `full` it read at the top, so
///   3 gaps = **12pt**.
/// * `vike_cockpit::ladder::draw` — `ladder_header` (the density's header height, 24 at Normal) /
///   the region, two allocations
///   summing the same way, so 1 gap = **4pt**. (The cockpit's rail, PTB header and ticket are
///   drawn BEFORE the ladder and consume only their own heights; the ladder reads
///   `available_rect_before_wrap` after them, so their gaps are already in the cursor and
///   contribute nothing.)
const LADDER_SIBLINGS: [(&str, usize); 2] = [("DOM", 4), ("Polymarket cockpit", 2)];

/// The width of a FLOATING scrollbar's interact strip: `egui-0.36.1/src/style.rs`'s
/// `ScrollStyle::floating()` sets `bar_width: 10.0`, and `scroll_area.rs` senses the bar over
/// `max_bar_rect` — `outer_rect.with_min_x(max_cross - full_width)`, i.e. the outermost
/// `bar_width` points of the scroll area, whatever width the bar is currently ANIMATED to.
const BAR_W: f32 = 10.0;

/// Which vike-desktop CALL SITE a fixture models. There are two of them and the split is not
/// by kind: `app_ui.rs`'s `if w.kind == workspace::WinKind::Chart` branch draws ONE of the
/// three `fills` kinds, and the other two — DOM and Polymarket — are dispatched from the TOOL
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
    /// The TOOL call site for a `fills` kind (DOM, Polymarket): the same margin frame, a body
    /// that covers the frame's available rect EXACTLY, handed through [`BodyBounds::show`] —
    /// which must hand a `fills` body straight through.
    ///
    /// ⚠ It is the ID-CHAIN and WRAPPER reference, not a model of either shipped ladder's
    /// GEOMETRY. Both ladders read `available_rect_before_wrap` once and then lay out siblings
    /// summing to it, so they end up PAST that rect by their own item-spacing gaps — this
    /// fixture lands on it exactly. Use [`Site::ToolFillsSpaced`] for anything that turns on
    /// where the body ends.
    ToolFills,
    /// The SAME site with [`BodyBounds::show`] BYPASSED — origin/main's spelling, and the
    /// reference `ToolFills`'s id chain, bands and painted rect are compared against.
    ToolFillsBare,
    /// The TOOL site with a LADDER-shaped `fills` body: `N` sibling allocations SUMMING to the
    /// rect the body read once at the top, which is the shape both shipped ladders have
    /// (`vike_panels::dom::draw`, `vike_cockpit::ladder::draw`). The `item_spacing.y` gaps
    /// BETWEEN those siblings were never subtracted, so the body over-allocates past its
    /// available rect by `(N - 1)` of them.
    ///
    /// ⚠ [`Site::ToolFills`] cannot express that and never could: its body allocates exactly
    /// the rect it was offered, so its `min_rect == max_rect` is a property of the FIXTURE.
    /// The doc's "Chart / DOM / Polymarket are byte-identical" claim was gated by that
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

    /// A `fills` kind drawn at the TOOL call site (DOM / Polymarket). `wrapped` picks the
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
            // the DOM and Polymarket ladders sized to `available_rect_before_wrap`.
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
                // whose heights SUM to it — exactly `vike_panels::dom::draw`'s
                // `header`/`toolbar`/`region`/`footer` and `vike_cockpit::ladder::draw`'s
                // `ladder_header`/`region`. Neither subtracts the `item_spacing.y` egui
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
    /// drift from its `BAND`/`TITLE_CLEAR` arithmetic. `None` when no band was allocated.
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

#[test]
fn a_left_drag_on_a_tool_window_moves_the_left_edge_and_pins_the_right() {
    // THE REPORTED BUG. A tool window's LEFT edge could not be dragged at all: egui refuses it
    // structurally — `PossibleInteractions::new` reads `resize_left: resizable.x && (movable
    // || pivot.x() != Align::LEFT)` and these windows are `.movable(false)` at the default
    // LEFT_TOP pivot — so the app's own bands are the only thing that can ever serve it.
    //
    // MUTATION that must redden this: drop `|| w.user_sized` from `show_window`'s
    // `pinning_now`. The band arithmetic still runs, but the end-of-frame read-back
    // (`if !pinning_now`) hands the geometry straight back to egui's stale rect and the left
    // edge snaps home.
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Greeks, pos2(240.0, 140.0), vec2(600.0, 420.0));
    assert!(!w.user_sized, "an untouched window has not latched");

    let before = w.pos.x + w.size.x; // the RIGHT edge — the invariant a left drag must pin
    let x0 = w.pos.x;
    let width0 = w.size.x;

    drag_band(&mut h, &mut w, 1, vec2(100.0, 0.0));

    assert!(w.user_sized, "moving a band latches the window as app-owned");
    assert!(
        (w.pos.x - (x0 + 100.0)).abs() < 2.0,
        "left edge must follow the drag: {x0} -> {} (wanted {})",
        w.pos.x,
        x0 + 100.0
    );
    assert!(
        (w.size.x - (width0 - 100.0)).abs() < 2.0,
        "width must shrink by the drag: {width0} -> {}",
        w.size.x
    );
    assert!(
        (w.pos.x + w.size.x - before).abs() < 2.0,
        "the RIGHT edge is pinned: {before} -> {}",
        w.pos.x + w.size.x
    );

    // ...and it STAYS moved. This half is what catches egui handing the geometry back a frame
    // later, which is exactly what the un-latched code did.
    h.frames(&mut w, 4);
    assert!(
        (w.pos.x - (x0 + 100.0)).abs() < 2.0,
        "the left edge must not snap home on a later frame: {}",
        w.pos.x
    );
    assert!(
        (w.pos.x + w.size.x - before).abs() < 2.0,
        "the right edge must stay pinned on later frames: {}",
        w.pos.x + w.size.x
    );

    // ⚠ ...and finally on what a USER sees. Every assertion above reads a value `show_window`
    // itself just wrote, so all of them would pass with the window painted somewhere else
    // entirely. The painted rect is egui's own answer.
    let painted = h.painted(&w);
    assert!(
        (painted.min.x - w.pos.x).abs() < 2.0,
        "the PAINTED left edge must be where `w.pos.x` says: {} vs {}",
        painted.min.x,
        w.pos.x
    );
    assert!(
        (painted.width() - w.size.x).abs() < 2.0,
        "the PAINTED width must be `w.size.x`: {} vs {}",
        painted.width(),
        w.size.x
    );
}

#[test]
fn a_resized_tool_window_stops_growing_to_its_content() {
    // THE OTHER REPORTED BUG: a tool window could not be dragged SMALLER, because
    // `Resize::begin` does `desired_size = desired_size.max(last_content_size)` on every
    // non-dragging frame — the window's floor is its content's minimum size.
    //
    // ⚠ The assertion is on the PAINTED rect, never on `w.size`: after the latch that value is
    // written by the band arithmetic itself, so asserting on it would pass with the whole fix
    // deleted.
    //
    // ⚠ It drags BOTH WAYS, and that is not thoroughness — it is what makes each half of the
    // fix load-bearing. The two mutations redden different halves, and a shrink-only test was
    // MEASURED green under the second one:
    //
    //   * `BodyBounds::show` -> call `body(ui)` directly (no container at all) and phase A
    //     reddens: the body reports its CONTENT size again, which re-arms `Resize`'s floor.
    //   * change only `auto_shrink(!scrolls)` to `auto_shrink(true)` and phase A stays GREEN —
    //     a scroll area is capped at the space available either way once its content
    //     OVERFLOWS, so shrinking alone cannot tell the two apart. Phase B is what reddens:
    //     with auto-shrink ON, a window dragged TALLER than its content collapses back to the
    //     content (`scroll_area.rs`'s `(true, true) => inner_size[d].min(content_size[d])`),
    //     so the painted window stops matching the size the user dragged and the bands end up
    //     outside it.
    let body = vec2(560.0, 400.0);
    let spawn = vec2(600.0, 400.0);
    let mut h = Harness::tool(body);
    let mut w = settled_tool(&mut h, WinKind::Studio, pos2(200.0, 120.0), spawn);
    // ⚠ Compared against the SPAWN height, not against `body.y`: the latter is true of any
    // window taller than its body and would hold with nothing grown at all. Against the spawn
    // it says the thing that matters — egui grew the window PAST what the app asked for.
    assert!(
        h.painted(&w).height() > spawn.y,
        "before the latch egui grows the window past its spawn size onto its content floor — \
             that is the bug ({} <= {})",
        h.painted(&w).height(),
        spawn.y
    );

    // PHASE A — drag the bottom edge UP, through the content floor.
    drag_band(&mut h, &mut w, 8, vec2(0.0, -180.0));
    h.frames(&mut w, 4);

    assert!(w.user_sized, "the bottom band latches the window too");
    assert!(h.scrolled.get(), "a latched tool body is told to bound itself");
    assert!(
        h.painted(&w).height() < body.y,
        "the PAINTED window must now be shorter than its content ({} >= {})",
        h.painted(&w).height(),
        body.y
    );

    // PHASE B — and back DOWN, past the content: the painted window must follow the drag
    // rather than collapsing onto the body it contains.
    let tall = w.size.y + 400.0;
    drag_band(&mut h, &mut w, 8, vec2(0.0, 400.0));
    h.frames(&mut w, 4);
    assert!(
        (h.painted(&w).height() - tall).abs() < 4.0,
        "the PAINTED window must match the dragged size, not the body's ({} vs {tall})",
        h.painted(&w).height()
    );
}

#[test]
fn the_bottom_band_stays_inside_a_window_shorter_than_its_box() {
    // THE ANCHOR. `window_resize_handles` read `ui.max_rect()`, which for an UNLATCHED tool
    // window is `Resize::begin`'s `desired_size` — a MONOTONE HIGH-WATER MARK — while the
    // window is PAINTED at `last_content_size` (`Resize::end` takes `else { size[d] =
    // state.last_content_size[d]; }`, since `Window::show_dyn` forces `resizable(false)` and
    // builds its `Resize` `.with_stroke(false)`). A body SHORTER than the spawn box makes the
    // two diverge, and the bottom band plus both bottom corners were then allocated BELOW the
    // visible window: the bottom edge could not be grabbed at all, and an invisible 8pt
    // `Sense::drag` strip floated in dead space showing a resize cursor and swallowing clicks
    // on whatever sat underneath. A REGRESSION — that edge worked through egui's native
    // resize until `.resizable(false)`.
    //
    // MUTATION that must redden this: in `window_resize_handles`, replace the whole
    // `let r = if fits(painted) { … }` selection with `let r = ui.max_rect();`.
    let mut h = Harness::tool(vec2(520.0, 100.0)); // a body FAR shorter than the box
    let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

    // NON-VACUITY: the two rects really do diverge here, so "inside the painted rect" is a
    // claim about the fix rather than about a degenerate fixture.
    let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
    assert!(
        max_r.height() > min_r.height() + 100.0,
        "the fixture must actually diverge: max_rect {} vs min_rect {}",
        max_r.height(),
        min_r.height()
    );

    let painted = h.painted(&w);
    for mask in [8u8, 1 | 8, 2 | 8] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            painted.contains_rect(band),
            "band {mask} at {band:?} must lie INSIDE the painted window {painted:?}"
        );
    }

    // ...and the edge is not merely inside, it WORKS: grabbing it resizes the window.
    let h0 = w.size.y;
    drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 + 60.0)).abs() < 2.0,
        "the bottom edge must drag: {h0} -> {}",
        w.size.y
    );
}

#[test]
fn a_body_taller_than_the_arena_keeps_the_window_and_its_bands_inside_it() {
    // THE REPORTED BUG, in the owner's words: "WHY I CANT RESIZE IT AT BOTTOM?? IT SEEMS LIKE
    // WINDOW IS OVERLAYED BY BOTTOM STATUS BAR?" — on the Connections tool window.
    //
    // The arena is NOT the problem and the theory it suggests is wrong: `app_ui.rs` adds
    // `egui::Panel::bottom("statusbar")` BEFORE the `CentralPanel`, and a bottom panel sets
    // `cursor.max[axis] = visible_outer_rect.min[axis]` on its parent
    // (`egui-0.36.1/src/containers/panel.rs`), so `app.desktop` genuinely excludes the strip —
    // see [`arena`], which is shaped the same way.
    //
    // What is true is the WINDOW. Until it is app-owned a tool window is painted at its
    // CONTENT, whatever its declared size says: `Window::show_dyn` forces
    // `resize.resizable(false)` and `.with_stroke(false)`, so `Resize::end` always takes
    // `else { size[d] = state.last_content_size[d]; }`. A body taller than the arena therefore
    // paints a window whose bottom edge is past `bounds` — under the status bar, exactly as
    // reported — and `window_resize_handles` then allocates the bottom band and both bottom
    // corners out there, where `Ui::interact`'s `interact_rect: self.clip_rect().intersect(
    // rect)` is NEGATIVE (the area's clip rect is its `constrain_rect`) and
    // `egui-0.36.1/src/hit_test.rs` drops the widget before measuring anything. The band
    // exists, `read_response` answers for it, and it can never be hit.
    //
    // MUTATION that must redden this: delete the `w.arena_bounded = true;` arm from
    // `show_window`'s read-back block (the ARENA CEILING latch). PERFORMED — it reddens here
    // and on the width twin, and leaves `a_window_that_fits_its_arena_never_takes_the_ceiling`
    // green. ⚠ MEASURED under it, by stripping this test's earlier assertions one layer at a
    // time so each later claim was actually reached — because a test that only ever reddens on
    // its first assertion has proved nothing about the rest:
    //
    //   * painted rect  `[..1476]` against an arena ending at `978` — the window is drawn
    //     498pt past the status bar's top edge, which is the report verbatim;
    //   * band 8        `[[129 1467] - [671 1475]]` against a clip rect of
    //     `[[0 40] - [1200 978]]` — allocated, answering `read_response`, 489pt outside;
    //   * the DRAG      a full press-move-release on it moves the window `1436 -> 1436`.
    //
    // That last line is the whole bug in one number, and it is why this test drags rather than
    // stopping at geometry.
    //
    // ⚠ The assertion ORDER is deliberate, because every band is still ALLOCATED under that
    // mutation and `h.band(..)` still answers for it. The geometric claims come FIRST so the
    // mutation's first red is the symptom a user sees — a window painted past the arena — and
    // only then the bookkeeping. Asserting `w.arena_bounded` first would have reddened on a
    // flag while proving nothing about the window.
    const BODY_H: f32 = 1400.0;
    let mut h = Harness::tool(vec2(520.0, BODY_H)).in_arena();
    let arena = arena();
    let mut w = settled_tool(&mut h, WinKind::Connections, pos2(120.0, 60.0), vec2(560.0, 400.0));

    // NON-VACUITY, from the fixture's own arithmetic rather than from anything the fix wrote:
    // this body genuinely cannot fit, so the containment below is a claim about the ceiling.
    assert!(
        BODY_H > arena.height(),
        "the fixture must overflow: body {BODY_H} vs arena {}",
        arena.height()
    );

    assert!(!w.user_sized, "nothing was grabbed, so the USER latch must stay clear");

    // THE SYMPTOM: the window is no longer painted past the arena, i.e. under the status bar.
    let painted = h.painted(&w);
    assert!(
        painted.max.y <= arena.max.y + 1.0,
        "the painted window must not reach past the arena bottom ({}) into the status bar: {}",
        arena.max.y,
        painted.max.y
    );
    assert!(
        arena.contains_rect(painted.shrink(1.0)),
        "the whole window must sit inside the arena {arena:?}: {painted:?}"
    );

    // THE MECHANISM: every band is inside the clip rect, which for this `Area` IS the arena.
    for mask in [8u8, 1 | 8, 2 | 8, 1, 2] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            arena.intersect(band).is_positive(),
            "band {mask} at {band:?} must intersect the clip rect {arena:?} — a band outside \
                 it is registered, answers `read_response`, and can never be hit"
        );
        assert!(
            painted.contains_rect(band),
            "band {mask} at {band:?} must lie inside the painted window {painted:?}"
        );
    }

    // ...and the bottom edge WORKS. It can only be dragged UP from here — the window is at the
    // ceiling — which is the half the owner could not reach at all.
    let h0 = w.size.y;
    assert!(
        (h0 - arena.height()).abs() < 1.0,
        "the ceiling must cap the window at the arena height {}: {h0}",
        arena.height()
    );
    drag_band(&mut h, &mut w, 8, vec2(0.0, -120.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 - 120.0)).abs() < 2.0,
        "the bottom edge must drag the window SMALLER: {h0} -> {}",
        w.size.y
    );
    let painted = h.painted(&w);
    assert!(
        (painted.height() - (h0 - 120.0)).abs() < 2.0,
        "...and the PAINTED window must follow it: {}",
        painted.height()
    );

    // ...and back DOWN again, so the ceiling is a cap rather than a one-way freeze.
    drag_band(&mut h, &mut w, 8, vec2(0.0, 60.0));
    h.frames(&mut w, 4);
    assert!(
        (w.size.y - (h0 - 60.0)).abs() < 2.0,
        "the bottom edge must drag back down too: {}",
        w.size.y
    );

    // ...and only now the bookkeeping, which is the MEANS rather than the end.
    assert!(w.arena_bounded, "a body the arena cannot hold must latch the ceiling");
    assert!(h.scrolled.get(), "...and the body must be told to bound itself");
    assert!(
        h.body_max.get().height() < BODY_H,
        "...which is what BOUNDS it: offered {} of {BODY_H}",
        h.body_max.get().height()
    );
}

#[test]
fn a_body_wider_than_the_arena_keeps_both_side_bands_inside_it() {
    // The SAME mechanism on the other axis, and WHICH band it costs depends on where the
    // window sits. `Context::constrain_window_rect_to_area` clamps an oversized window's
    // position into `[area.left - margin, area.left]`, where
    // `margin = window.width() - area.width()` — so the overflow hangs off whichever side the
    // pivot did not pin. At this fixture's position the left edge lands exactly ON the arena's
    // and the RIGHT band is the one outside the clip; a window whose pivot had been dragged
    // further left loses the LEFT band instead. Either way at least one side band is gone,
    // which is how a wide body can reproduce the ORIGINAL report's symptom ("resize does not
    // work from the left edge") on a build where #1773 already fixed its cause — a different
    // defect wearing the same complaint.
    //
    // MUTATION that must redden this: as above, delete the `w.arena_bounded = true;` arm —
    // PERFORMED, and it reddens here first on the painted WIDTH, not on a flag (same
    // assertion-order argument as the height twin).
    const BODY_W: f32 = 1400.0;
    let mut h = Harness::tool(vec2(BODY_W, 300.0)).in_arena();
    let arena = arena();
    let mut w = settled_tool(&mut h, WinKind::Studio, pos2(60.0, 80.0), vec2(560.0, 400.0));

    assert!(
        BODY_W > arena.width(),
        "the fixture must overflow: body {BODY_W} vs arena {}",
        arena.width()
    );

    let painted = h.painted(&w);
    assert!(
        painted.width() <= arena.width() + 1.0,
        "the window must be capped at the arena width {}: {}",
        arena.width(),
        painted.width()
    );
    for mask in [1u8, 2, 1 | 8, 2 | 8] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            arena.intersect(band).is_positive(),
            "band {mask} at {band:?} must intersect the clip rect {arena:?}"
        );
    }

    // ...and the LEFT edge drags, pinning the right — the original report's exact claim, now
    // asserted on a window that could not previously offer that band at all.
    let right0 = w.pos.x + w.size.x;
    let x0 = w.pos.x;
    drag_band(&mut h, &mut w, 1, vec2(90.0, 0.0));
    h.frames(&mut w, 4);
    assert!(
        (w.pos.x - (x0 + 90.0)).abs() < 2.0,
        "the left edge must follow the drag: {x0} -> {}",
        w.pos.x
    );
    assert!(
        (w.pos.x + w.size.x - right0).abs() < 2.0,
        "...with the RIGHT edge pinned: {right0} -> {}",
        w.pos.x + w.size.x
    );

    // ...and only now the bookkeeping.
    assert!(w.arena_bounded, "a body the arena cannot hold must latch the ceiling");
    assert!(
        h.body_max.get().width() < BODY_W,
        "...which is what BOUNDS it: offered {} of {BODY_W}",
        h.body_max.get().width()
    );
}

#[test]
fn a_window_that_fits_its_arena_never_takes_the_ceiling() {
    // The ceiling's NO-OP half, and the reason it is a latch on OVERFLOW rather than a clamp
    // on every window. A tool window that fits must stay exactly what it was before this
    // change: unpinned, sized by egui, growing to its content — nine tool kinds open at
    // 560×400 and several bodies are naturally bigger, so bounding one that fits would open it
    // into scrollbars for nothing.
    //
    // ⚠ It runs in the NARROW [`arena`], not in [`screen`]: run against bounds nothing can
    // reach, this test would pass with the ceiling wired to fire unconditionally.
    //
    // MUTATION that must redden this: drop the `rect.width() > … || rect.height() > …`
    // condition from the ARENA CEILING latch (latch every window), or drop `CEILING_SLACK`
    // from both comparisons and hand the fixture a body that lands exactly on the arena.
    let mut h = Harness::tool(vec2(620.0, 500.0)).in_arena();
    let arena = arena();
    let w = settled_tool(&mut h, WinKind::News, pos2(100.0, 120.0), vec2(560.0, 400.0));

    assert!(!w.arena_bounded, "a window that fits must NOT take the ceiling");
    assert!(!h.scrolled.get(), "...and its body is never told to bound itself");
    assert!(!w.user_sized, "...nor does the user latch fire on its own");
    let painted = h.painted(&w);
    assert!(
        painted.height() < arena.height(),
        "non-vacuity: the fixture must actually fit ({} vs {})",
        painted.height(),
        arena.height()
    );
    // The pre-existing promise this must not break: egui still owns the geometry, so the
    // window has GROWN to its taller and wider content rather than staying at the 560×400 it
    // opened with.
    assert!(w.size.y > 400.0, "an unbounded window must still grow to its content: {}", w.size.y);
    assert!(w.size.x > 560.0, "...on the other axis too: {}", w.size.x);
}

/// `window_resize_handles`' own `BAND` / `TITLE_CLEAR`, restated here so
/// [`expected_bands`] can rebuild the band set from OUTSIDE the function under test. They are
/// private to it, so this is a deliberate second copy: a production change to either constant
/// moves every band and must redden the byte-identity tests below, which is exactly what a
/// shared constant would hide.
const BAND: f32 = 8.0;
const TITLE_CLEAR: f32 = 36.0;

/// The five band rects `window_resize_handles` places on an anchor rect, keyed by mask, spelled
/// as origin/main spelled them (`bl`, `br`, `left`, `right`, `bottom`).
fn expected_bands(r: Rect) -> [(u8, Rect); 5] {
    [
        (1 | 8, Rect::from_min_max(pos2(r.min.x, r.max.y - BAND), pos2(r.min.x + BAND, r.max.y))),
        (2 | 8, Rect::from_min_max(pos2(r.max.x - BAND, r.max.y - BAND), pos2(r.max.x, r.max.y))),
        (
            1,
            Rect::from_min_max(
                pos2(r.min.x, r.min.y + TITLE_CLEAR),
                pos2(r.min.x + BAND, r.max.y - BAND),
            ),
        ),
        (
            2,
            Rect::from_min_max(
                pos2(r.max.x - BAND, r.min.y + TITLE_CLEAR),
                pos2(r.max.x, r.max.y - BAND),
            ),
        ),
        (
            8,
            Rect::from_min_max(pos2(r.min.x + BAND, r.max.y - BAND), pos2(r.max.x - BAND, r.max.y)),
        ),
    ]
}

#[test]
fn a_chart_windows_band_anchor_is_byte_identical_to_the_old_max_rect_one() {
    // THE CHART must not move by ONE POINT, and it reaches that by FILLING rather than by
    // overrunning: `vike_chart::chart::draw` ends with
    // `add_space(min(clip.bottom, max_rect.bottom) - ui.cursor().top())`, and the cursor is
    // already PAST every gap it laid — so it lands exactly on `max_rect.bottom` however many
    // siblings it drew, and `min_rect == max_rect` with nothing unioned in.
    //
    // ⚠ This test ran over `[Site::Chart, Site::ToolFills]` and claimed the property for
    // Chart, DOM and Polymarket at once. The claim holds for all three, but that fixture could
    // not show it: its body allocates exactly the rect it is offered, so its
    // `min_rect == max_rect` is a property of the FIXTURE rather than of any shipped body, and
    // both ladders in fact OVERRUN. They keep the property for a different reason —
    // `Region::expand_to_include_rect` grows `max_rect` with them — which is why they have
    // their own test, `a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical`,
    // rather than riding this one.
    //
    // ⚠ It asserted `min_rect == max_rect` and NOTHING else at first, and that gated nothing:
    // every change to where the bands actually land left it green. What is asserted now is
    // each of the five BAND RECTS, in absolute coordinates, against the arithmetic origin/main
    // performed on `ui.max_rect()`. The equality survives as the PREMISE.
    //
    // MUTATION that must redden this: `const TITLE_CLEAR: f32 = 40.0;` in
    // `window_resize_handles` (or any change to `BAND` or to a band rect's arithmetic) — the
    // chart's bands move, which is precisely what "must not move by one point" forbids.
    // ⚠ The anchor swap itself CANNOT redden THIS test and that is the claim, not a gap: the
    // chart's body reaches `ui.max_rect()`'s bottom, so `content_ui.min_rect()` IS that rect
    // (`egui-0.36.1/src/containers/resize.rs`, `Resize::begin`'s `inner_rect =
    // Rect::from_min_size(position, state.desired_size)`). The anchor is gated where the two
    // rects CAN differ, which is where a body UNDER-fills its box —
    // `the_bottom_band_stays_inside_a_window_shorter_than_its_box`. No `fills` kind can reach
    // that state, so no `fills` test gates the anchor and none pretends to.
    let mut h = Harness::fill();
    let mut w = WinState::new(
        "chartfix",
        "BTCUSDT",
        "1m",
        WinKind::Chart,
        Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
    );
    h.frames(&mut w, 14);

    let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
    assert_eq!(
        min_r, max_r,
        "PREMISE: the chart body fills to `max_rect`'s bottom, so the bands anchor on the \
             same rect either way"
    );
    // THE CLAIM: every band is exactly where the old `max_rect` spelling put it.
    for (mask, want) in expected_bands(max_r) {
        let got = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert_eq!(got, want, "band {mask} must not move by one point");
    }
    // ...and that rect is the window a user sees, which is the property the doc claims.
    let painted = h.painted(&w);
    assert!(
        painted.contains_rect(min_r),
        "the anchor rect {min_r:?} must lie inside the painted window {painted:?}"
    );
}

#[test]
fn a_ladder_windows_bands_survive_its_item_spacing_overflow_byte_identical() {
    // THE LADDER SHAPE, and the two wrong stories told about it.
    //
    // The DOM and the Polymarket cockpit each read `ui.available_rect_before_wrap()` ONCE and
    // then lay out siblings SUMMING to that height, so the `item_spacing.y` egui inserts
    // BETWEEN them was never subtracted and is pure overflow: the body ends
    // `(siblings - 1) * shipped_item_spacing().y` past the rect it was OFFERED — 12pt for the DOM's four
    // allocations, 4pt for the cockpit ladder's two ([`LADDER_SIBLINGS`]). That much is real
    // and is asserted below against [`Harness::offered_max`].
    //
    // ⚠ STORY ONE, the doc's original: "a `fills` body covers `max_rect` exactly, so
    // `min_rect == max_rect` and the bands are byte-identical". The CONCLUSION is right and
    // the REASON is not — these bodies overrun `max_rect` rather than covering it — and it was
    // gated by a fixture (`Site::ToolFills`) whose body allocates exactly the rect it is
    // offered, so the claim was unfalsifiable by construction.
    //
    // ⚠ STORY TWO, a review round's correction: "the bands therefore MOVE DOWN by the
    // overflow, and on the `max_rect` spelling the DOM's bottom band sat 12pt INBOARD of the
    // painted edge". Also false, and in the more dangerous direction — it reads as a bug fixed
    // when nothing moved. `egui-0.36.1/src/layout.rs`'s `Region::expand_to_include_rect`
    // unions every child's rect into `min_rect` AND `max_rect` alike ("`max_rect` will always
    // be at least the size of `min_rect`", `Region::max_rect`'s own doc), so by the time
    // `window_resize_handles` runs the overflow is in BOTH rects and the anchors AGREE. A
    // `max_rect` band can only ever sit at-or-OUTSIDE the painted edge, never inboard.
    // MEASURED: under the anchor mutation named below this test stays GREEN, band rects and
    // all, while `the_bottom_band_stays_inside_a_window_shorter_than_its_box` reddens.
    //
    // What this test therefore gates is the CONCLUSION with its real mechanism: a body that
    // overruns its offered rect by a measured amount still gets byte-identical bands, because
    // `max_rect` grew with it.
    //
    // MUTATION that must redden this: `const TITLE_CLEAR: f32 = 40.0;` in
    // `window_resize_handles` (or any change to `BAND` or to a band rect's arithmetic) — the
    // ladders' bands move, which is what "byte-identical" forbids. RUN: it reddens here with
    // "DOM: band 1 must not move by one point" (and the chart's twin above with the same
    // wording). ⚠ The ANCHOR SWAP (replace the whole `let r = if fits(painted) { … }`
    // selection with `let r = ui.max_rect();`) deliberately does NOT redden it: that is the
    // claim, and it was RUN too — 16 tests, one failure, and it is the under-fill one.
    for (label, siblings) in LADDER_SIBLINGS {
        let mut h = Harness::ladder(siblings);
        let mut w = WinState::new(
            "ladderfix",
            "BTCUSDT",
            "1m",
            WinKind::Dom,
            Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
        );
        h.frames(&mut w, 14);

        let (min_r, max_r, offered) =
            (h.content_min.get(), h.content_max.get(), h.offered_max.get());
        let shift = (siblings - 1) as f32 * shipped_item_spacing().y;
        assert!(shift > 0.0, "{label}: a ladder has at least two siblings");

        // NON-VACUITY: the fixture really does model an OVERRUNNING body, by exactly the gaps
        // the shipped one leaves. Without this the byte-identity below is a claim about a body
        // that fits, which is the hole `Site::ToolFills` left.
        assert!(
            (min_r.max.y - offered.max.y - shift).abs() < 0.01,
            "{label}: the body must over-allocate by its {} item-spacing gaps — content \
                 bottom {} vs the {} it was offered (wanted a {shift}pt overrun)",
            siblings - 1,
            min_r.max.y,
            offered.max.y
        );

        // THE MECHANISM: `max_rect` absorbed that overrun, so the two anchors AGREE and the
        // `max_rect` spelling could not have placed a band inboard of the painted edge.
        assert_eq!(
            min_r, max_r,
            "{label}: `Region::expand_to_include_rect` unions the overrun into `max_rect` too, \
                 so the anchor the bands use and the one origin/main used are the same rect"
        );

        // ...and that rect IS the window a user sees. ⚠ Offset by the window frame's stroke,
        // which egui counts as part of the frame's margin, so the area rect sits that far
        // outside the content rect on every side.
        let painted = h.painted(&w);
        let edge = h.window_stroke();
        assert!(
            (painted.max.y - min_r.max.y - edge).abs() < 0.01,
            "{label}: the painted bottom edge IS the anchor's, plus the {edge}pt frame stroke \
                 ({} vs {})",
            painted.max.y,
            min_r.max.y
        );

        // THE CLAIM: every band, byte-identical to the arithmetic origin/main performed.
        for (mask, want) in expected_bands(min_r) {
            let got = h
                .band(&w, mask)
                .unwrap_or_else(|| panic!("{label}: band {mask} must be allocated"));
            assert_eq!(got, want, "{label}: band {mask} must not move by one point");
        }
    }
}

#[test]
fn the_tool_sites_fills_body_is_handed_through_unwrapped() {
    // G1's regression, and the one the layout assertions above can NEVER see. DOM and
    // Polymarket are `fills` kinds dispatched from the TOOL call site, so they go through
    // `BodyBounds::show` — and a `ScrollArea` there is LAYOUT-identical (`[false, false]` +
    // `auto_shrink(true)` reserves no space and follows the content) while putting an extra
    // `Ui` level into the body's ID CHAIN. egui keys widget state on `Ui::id`, so that is one
    // silent discard of every egui-persisted state inside the DOM ladder and the Polymarket
    // cockpit — scroll offsets, collapsing headers, text cursors — on upgrade.
    //
    // The reference is the SAME fixture with the wrapper bypassed, which is origin/main's
    // spelling: same window id, same kind, same geometry, same body.
    //
    // MUTATION that must redden this: delete `if self.fills { return body(ui); }` from
    // `BodyBounds::show`.
    let rect = Rect::from_min_size(pos2(180.0, 120.0), vec2(640.0, 460.0));
    let (mut hw, mut hb) = (Harness::tool_fill(true), Harness::tool_fill(false));
    let mut ww = WinState::tool("domfix", WinKind::Dom, rect);
    let mut wb = WinState::tool("domfix", WinKind::Dom, rect);
    hw.frames(&mut ww, 14);
    hb.frames(&mut wb, 14);

    assert!(!hw.scrolled.get(), "a `fills` kind is never told to bound its body");
    let wrapped_id = hw.body_id.get().expect("the wrapped body was drawn");
    assert_eq!(
        wrapped_id,
        hb.body_id.get().expect("the bare body was drawn"),
        "the tool site's `fills` body must get the SAME `Ui::id` it had unwrapped — every \
             auto-id widget inside the DOM ladder and the Polymarket cockpit is keyed on it"
    );

    // ...and the GEOMETRY is unchanged too: same painted window, same bands, same body rect.
    assert_eq!(hw.painted(&ww), hb.painted(&wb), "the painted window must be unchanged");
    assert_eq!(hw.body_max.get(), hb.body_max.get(), "the body's own rect must be unchanged");
    for (mask, _) in expected_bands(hw.content_min.get()) {
        let got = hw.band(&ww, mask).unwrap_or_else(|| panic!("band {mask} must exist"));
        let want = hb.band(&wb, mask).expect("the bare fixture's band");
        assert_eq!(got, want, "band {mask} must be unchanged by the wrapper");
    }
}

#[test]
fn a_window_too_short_for_its_bands_is_still_resizable() {
    // The bands are the ONLY resize mechanism any window has — `show_window` builds every
    // `Window` `.resizable(false)`, and egui structurally refuses the left edge for a
    // non-movable LEFT_TOP-pivot window regardless. So `window_resize_handles`' too-small
    // guard is not "skip a nicety": keyed on the PAINTED rect alone it made a tool window
    // whose body paints shorter than `TITLE_CLEAR + 2*BAND` unresizable BY ANY MEANS, for the
    // life of the window — where before `.resizable(false)` egui's own right and bottom edges
    // worked. A short body is not exotic: an empty list or a collapsed panel is one.
    //
    // MUTATION that must redden this: drop the `else if fits(ui.max_rect())` arm from
    // `window_resize_handles` (i.e. `return None` as soon as the painted rect is too small).
    let mut h = Harness::tool(vec2(520.0, 6.0)); // a body only a few points tall
    let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

    // NON-VACUITY: the painted content really is too short to carry a band set, and the
    // monotone high-water rect really is not — so this fixture takes the fallback arm.
    let (min_r, max_r) = (h.content_min.get(), h.content_max.get());
    assert!(
        min_r.height() < TITLE_CLEAR + 2.0 * BAND,
        "the fixture must actually be too short: painted height {}",
        min_r.height()
    );
    assert!(
        max_r.height() >= TITLE_CLEAR + 2.0 * BAND,
        "...and the fallback rect must be able to carry the bands: {}",
        max_r.height()
    );

    for mask in [1u8, 2, 8] {
        assert!(
            h.band(&w, mask).is_some(),
            "band {mask} must be allocated — without it this window can never be resized again"
        );
    }

    // ...and the bottom edge WORKS: grabbing it resizes the window.
    let h0 = w.size.y;
    drag_band(&mut h, &mut w, 8, vec2(0.0, 200.0));
    h.frames(&mut w, 4);
    assert!(w.user_sized, "the drag must latch the window as app-owned");
    assert!(w.size.y > h0 + 100.0, "the bottom edge must have dragged: {h0} -> {}", w.size.y);

    // SELF-HEALING, which is what makes the fallback's out-of-window bands acceptable: the
    // latched window is pinned to `w.size` and its body is bounded to that, so the painted
    // rect now carries the bands itself and the fallback arm is never taken again.
    let painted = h.painted(&w);
    assert!(
        (painted.height() - w.size.y).abs() < 4.0,
        "the latched window must paint at the dragged size: {} vs {}",
        painted.height(),
        w.size.y
    );
    for mask in [8u8, 1 | 8, 2 | 8] {
        let band = h.band(&w, mask).unwrap_or_else(|| panic!("band {mask} must be allocated"));
        assert!(
            painted.contains_rect(band),
            "band {mask} at {band:?} must now lie INSIDE the painted window {painted:?}"
        );
    }
}

#[test]
fn a_side_band_latch_floors_the_axis_its_band_never_touched() {
    // A SIDE-band drag latches the window, and the two resize arms above the latch floor only
    // the axis the pressed band consumes — `MINW` under `mask & (1 | 2)`, `MINH` under
    // `mask & 8`. So without the both-axes floor a side drag pinned the window with `w.size.y`
    // left at whatever the pre-latch read-back wrote: the PAINTED content height, which for a
    // short body (an empty list, a collapsed panel) is far under `MINH`.
    //
    // That value is then the window's DECLARED geometry (`pinning_now` builds it
    // `.fixed_size(w.size)`) while the window is PAINTED somewhere else entirely, because
    // `Resize::end` takes `size[d] = state.last_content_size[d]` for a `Window` and a latched
    // body is a `ScrollArea` whose `inner_size` is floored at `min_scrolled_size` — 64pt, BOTH
    // axes, unconditional for an enabled direction. `w.size` stops describing the window a
    // user sees, which is precisely what the read-back's own comment ("for pinned windows
    // `w.size`/`w.pos` are authoritative") promises it does not do, and the visible symptom is
    // the NEXT drag on that axis not tracking the pointer: the arm's own `.max(MIN*)` snaps
    // the stale value up first, so the edge moves by something other than the delta.
    //
    // ⚠ THIS IS NOT A STRANDING TEST, and a review round asked for it as one — the claim being
    // that `fixed_size` collapses `ui.max_rect()` onto `w.size` (true) and so puts BOTH of
    // `window_resize_handles`' anchor rects under its `fits` guard, leaving the window
    // unresizable by any means. It cannot: the same 64pt `min_scrolled_size` floor holds the
    // PAINTED rect — and therefore `ui.min_rect()` — well clear of that guard whatever
    // `w.size` says. The band-allocation assertions below are kept as the REFUTATION of that
    // claim and are GREEN under the mutation named here; the DRAG assertion is the gate.
    //
    // MUTATION that must redden this: delete `w.size = w.size.max(egui::vec2(MINW, MINH));`
    // from `show_window`'s latch block — the two `.max(MIN*)` calls in the arms above are the
    // per-axis floors, so deleting the line restores the per-axis-only spelling. RUN, and it
    // reddens HERE and nowhere else: "the bottom edge must follow the pointer after a
    // side-band latch: painted height 100 -> 250 for a 90pt drag (w.size.y = 250)". The jump
    // is the bottom drag's PRESS frame — a zero delta that still runs the `mask & 8` arm,
    // whose `.max(MINH)` snaps the stale value up before the move frame adds its 90.
    let mut h = Harness::tool(vec2(520.0, 6.0)); // the same short body as the test above
    let mut w = settled_tool(&mut h, WinKind::News, pos2(300.0, 160.0), vec2(600.0, 420.0));

    // NON-VACUITY: the pre-latch read-back really does leave `w.size.y` under the `MINH` the
    // bottom arm would have applied, so the floor deleted by the mutation has work to do.
    assert!(
        w.size.y < 160.0,
        "the fixture must actually spawn a window shorter than MINH: w.size.y = {}",
        w.size.y
    );

    // THE SIDE BAND — mask 1, which consumes only `d.x`.
    drag_band(&mut h, &mut w, 1, vec2(60.0, 0.0));
    h.frames(&mut w, 4);
    assert!(w.user_sized, "a side-band drag latches the window");

    // THE REFUTATION (green under the mutation, and that is the point): a latched short window
    // is NOT stranded — `min_scrolled_size` keeps the painted rect band-sized either way.
    for mask in [1u8, 2, 8, 1 | 8, 2 | 8] {
        assert!(
            h.band(&w, mask).is_some(),
            "band {mask} must be allocated after a SIDE-band latch (w.size = {:?}, \
                 painted = {:?})",
            w.size,
            h.painted(&w)
        );
    }

    // THE GATE: `w.size` describes the window a user sees, so the bottom edge TRACKS the
    // pointer. Asserted on the PAINTED rect, never on `w.size` — after the latch that value is
    // written by the band arithmetic itself, so a `w.size`-only assertion passes with the
    // floor deleted and the window painted somewhere else.
    let before = h.painted(&w).height();
    drag_band(&mut h, &mut w, 8, vec2(0.0, 90.0));
    h.frames(&mut w, 4);
    let after = h.painted(&w).height();
    assert!(
        (after - before - 90.0).abs() < 2.0,
        "the bottom edge must follow the pointer after a side-band latch: painted height \
             {before} -> {after} for a 90pt drag (w.size.y = {})",
        w.size.y
    );
}

#[test]
fn cross_axis_jitter_on_a_band_does_not_latch_the_window() {
    // The latch guard tested the WHOLE delta while the resize arithmetic above it consumes
    // only the component the mask names: the side bands read `d.x`, the bottom band `d.y`. So
    // a press on the bottom band that jittered purely HORIZONTALLY (or on a side band, purely
    // vertically) resized NOTHING and latched the window anyway — the same permanent,
    // irreversible conversion to `fixed_size` + a self-bounding body that the zero-delta guard
    // exists to prevent, one axis over. A pointer that moves on exactly one axis for a frame
    // is the common case, not the exotic one.
    //
    // MUTATION that must redden this: restore `if !fills && d != egui::Vec2::ZERO` in place of
    // the `moved_the_edge` guard in `show_window`.
    // (band, the CROSS-axis jitter it consumes none of, an IN-axis drag it must still take)
    let rounds =
        [(8u8, vec2(20.0, 0.0), vec2(0.0, -40.0)), (1u8, vec2(0.0, 20.0), vec2(-40.0, 0.0))];
    for (mask, jitter, along) in rounds {
        let mut h = Harness::tool(vec2(560.0, 520.0));
        let mut w = settled_tool(&mut h, WinKind::Trade, pos2(240.0, 140.0), vec2(600.0, 420.0));
        let from = h.band(&w, mask).expect("the band must be allocated").center();
        let to = from + jitter;
        let (x0, y0) = (w.size.x, w.size.y);

        h.move_to(from);
        h.frame(&mut w); // hover
        h.button(from, true);
        h.frame(&mut w); // press — zero delta
        h.move_to(to);
        h.frame(&mut w); // the CROSS-AXIS jitter: this band consumes none of it
        h.button(to, false);
        h.frame(&mut w); // release
        h.frames(&mut w, 4);

        assert!(
            !w.user_sized,
            "band {mask} consumes none of {jitter:?}, so it must not latch the window"
        );
        assert!(!h.scrolled.get(), "...and the body must not be told to bound itself");
        assert!(
            (w.size.x - x0).abs() < 2.0 && (w.size.y - y0).abs() < 2.0,
            "...and nothing was resized: {x0}x{y0} -> {}x{}",
            w.size.x,
            w.size.y
        );

        // NON-VACUITY: the SAME band on the SAME window latches on an in-axis move, so the
        // negative above is about the axis rather than about a band that never fired.
        drag_band(&mut h, &mut w, mask, along);
        h.frames(&mut w, 4);
        assert!(w.user_sized, "band {mask} must still latch on an in-axis drag of {along:?}");
    }
}

#[test]
fn an_untouched_tool_window_still_takes_its_size_from_egui() {
    // The latch's NO-OP half, and the reason it is a latch. Nine tool kinds open at 560x400
    // and several bodies are naturally bigger, so a window pinned from frame one would open
    // into scrollbars. Until a band is grabbed the window must behave exactly as it did before
    // this change: unpinned, sized by egui, growing to fit.
    //
    // MUTATION that must redden this: make `pinning`/`pinning_now` true for every tool window
    // from frame one (`|| !fills` in place of `|| w.user_sized`). `w.size` is then never read
    // back and stays at the spawn size.
    let mut h = Harness::tool(vec2(620.0, 440.0));
    let spawn = vec2(400.0, 300.0);
    let w = settled_tool(&mut h, WinKind::News, pos2(300.0, 200.0), spawn);

    assert!(!w.user_sized, "nothing has been dragged, so nothing has latched");
    assert!(!h.scrolled.get(), "an unlatched body is never told to bound itself");
    assert!(
        w.size.y > spawn.y,
        "egui must still grow the window to its taller content: {} <= {}",
        w.size.y,
        spawn.y
    );
    assert!(w.size.x > spawn.x, "...and to its wider content: {} <= {}", w.size.x, spawn.x);
}

#[test]
fn a_bare_click_on_a_band_does_not_latch_the_window() {
    // A `Sense::drag()` widget is `dragged()` from the PRESS frame, where `drag_delta()` is
    // ZERO (`egui-0.36.1/src/interaction.rs`: "This widget is just sensitive to drags, so we
    // can mark it as dragged right away"). Latching on `Some(..)` alone therefore turned ONE
    // accidental click on a tool window's 8pt edge into a permanent conversion to `fixed_size`
    // + scrolling for the rest of the session, with no way back. Every other test here DRAGS,
    // so none of them can tell a press from a press-and-move.
    //
    // MUTATION that must redden this: drop the per-axis NON-ZERO tests from the latch guard —
    // `let moved_the_edge = mask & (1 | 2 | 8) != 0;` — so the press frame's ZERO delta
    // latches. ⚠ This said "drop `&& d != egui::Vec2::ZERO` from the latch guard", which was
    // a leftover: that guard was rewritten to `moved_the_edge` and the string appears nowhere
    // in production, so the only instruction telling the next author how to prove this gate
    // was UNPERFORMABLE. Its sibling
    // (`cross_axis_jitter_on_a_band_does_not_latch_the_window`) names the same text as a
    // RESTORE, which is what it is — the old code. The re-worded mutation was RUN: it reddens
    // here with "a bare click must NOT latch the window" (and, expectedly, reddens that
    // sibling and `the_bottom_band_stays_inside_a_window_shorter_than_its_box` too).
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Trade, pos2(240.0, 140.0), vec2(600.0, 420.0));
    let at = h.band(&w, 8).expect("the bottom band must be allocated").center();

    h.move_to(at);
    h.frame(&mut w); // hover
    h.button(at, true);
    h.frame(&mut w); // press — `dragged()`, zero delta
    h.button(at, false);
    h.frame(&mut w); // release, pointer never moved
    h.frames(&mut w, 4);

    assert!(!w.user_sized, "a bare click must NOT latch the window");
    assert!(!h.scrolled.get(), "...and the body must not be told to bound itself");
}

#[test]
fn a_press_and_a_move_coalesced_into_one_frame_keeps_the_delta_they_carried() {
    // `pinning` was captured BEFORE the resize block, so on the very frame a window latched
    // the end-of-frame read-back still ran and overwrote the delta just applied with egui's
    // pre-drag rect. With the latch moved onto MOVEMENT that frame ALWAYS carries a non-zero
    // delta, so this stopped being theoretical: a press and a move coalescing into one frame's
    // `RawInput` is routine with a high-polling mouse.
    //
    // ⚠ The move stays INSIDE the 8pt band on purpose: egui hit-tests with `interact_pos`,
    // which every pointer event in the frame updates, so a coalesced move that left the band
    // would never press the band at all.
    //
    // MUTATION that must redden this: use the pre-frame `pinning` for the read-back again
    // (`if !pinning` in place of `if !pinning_now`).
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Options, pos2(240.0, 140.0), vec2(600.0, 420.0));
    let band = h.band(&w, 8).expect("the bottom band must be allocated");
    let from = pos2(band.center().x, band.min.y + 1.0);
    let to = pos2(from.x, from.y + 6.0);

    h.move_to(from);
    h.frame(&mut w); // hover, so the press frame is the FIRST frame with a button down
    let h0 = w.size.y;
    h.press_and_move(from, to);
    h.frame(&mut w); // press AND move, one `RawInput`
    h.button(to, false);
    h.frame(&mut w); // release
    h.frames(&mut w, 4);

    assert!(w.user_sized, "a coalesced press+move is a movement, so it latches");
    assert!(
        (w.size.y - (h0 + 6.0)).abs() < 1.0,
        "the coalesced frame's delta must survive: {h0} -> {} (wanted {})",
        w.size.y,
        h0 + 6.0
    );
    assert!(
        (h.painted(&w).height() - w.size.y).abs() < 2.0,
        "...and the PAINTED window must agree: {} vs {}",
        h.painted(&w).height(),
        w.size.y
    );
}

#[test]
fn the_bodys_id_is_stable_across_the_latch() {
    // egui keys widget state on `Ui::id`, so a `ScrollArea` INSERTED when the latch flips
    // changes the id of the whole tool body and discards every auto-id-keyed piece of state
    // inside it the first time a user drags an edge: the Studio code editor's cursor,
    // selection and UNDO STACK, every inner scroll offset, every `CollapsingState`, a
    // half-typed field in the Connections editor. `BodyBounds::show` therefore builds the
    // container UNCONDITIONALLY and toggles only its sizing.
    //
    // MUTATION that must redden this: spell `BodyBounds::show` as
    // `if self.scrolls { ScrollArea::both().auto_shrink(false).show(ui, body).inner } else {
    // body(ui) }` — the pre-latch id is then the Frame's child, not the scroll area's.
    let mut h = Harness::tool(vec2(560.0, 520.0));
    let mut w = settled_tool(&mut h, WinKind::Studio, pos2(240.0, 140.0), vec2(600.0, 420.0));
    let before = h.body_id.get().expect("the body was drawn");
    assert!(!h.scrolled.get(), "the fixture must start UNLATCHED for this to mean anything");

    drag_band(&mut h, &mut w, 8, vec2(0.0, -120.0));
    h.frames(&mut w, 4);

    assert!(h.scrolled.get(), "the fixture must have latched for this to mean anything");
    assert_eq!(
        h.body_id.get().expect("the body was drawn"),
        before,
        "the body's `Ui::id` must not change when the window latches — every auto-id widget \
             inside it (text cursors, undo stacks, collapsing headers, inner scroll offsets) is \
             keyed on it"
    );
}

#[test]
fn the_side_bands_clear_the_scrollbars_and_the_bottom_band_overlaps_by_the_measured_2pt() {
    // The band-vs-scrollbar geometry, on a LATCHED window whose body overflows BOTH ways so
    // both floating bars are live. This is also the only test that drags the RIGHT band
    // (mask 2).
    //
    // ⚠ It models the PRODUCTION inset (`TOOL_BODY_MARGIN`), because a harness with no margin
    // is exactly the configuration this geometry says must never ship.
    //
    // MUTATION that must redden this: set `TOOL_BODY_MARGIN`'s `right` to 0 — the vertical
    // bar's strip then lands ON the right band.
    let mut h = Harness::tool(vec2(900.0, 700.0));
    let mut w = settled_tool(&mut h, WinKind::Connections, pos2(120.0, 100.0), vec2(600.0, 420.0));

    // Latch and shrink in BOTH axes, so the 900x700 body overflows the window both ways.
    drag_band(&mut h, &mut w, 8, vec2(0.0, -260.0));
    h.frames(&mut w, 4);
    drag_band(&mut h, &mut w, 1, vec2(200.0, 0.0));
    h.frames(&mut w, 4);
    assert!(h.scrolled.get(), "the window must be latched for the bars to exist");

    // THE RIGHT BAND. It must serve the drag even with the vertical bar alongside it.
    let left0 = w.pos.x;
    let right0 = w.pos.x + w.size.x;
    drag_band(&mut h, &mut w, 2, vec2(-90.0, 0.0));
    h.frames(&mut w, 4);
    assert!(
        (w.pos.x + w.size.x - (right0 - 90.0)).abs() < 2.0,
        "the right edge must follow the drag: {right0} -> {}",
        w.pos.x + w.size.x
    );
    assert!((w.pos.x - left0).abs() < 2.0, "a right drag pins the LEFT edge: {}", w.pos.x);

    // THE GEOMETRY. `body_max` is the `ScrollArea`'s `inner_rect`; a floating bar senses over
    // `max_bar_rect`, the outermost `BAR_W` points of it.
    let inner = h.body_max.get();
    let right_band = h.band(&w, 2).expect("the right band must be allocated");
    let bottom_band = h.band(&w, 8).expect("the bottom band must be allocated");

    assert!(
        inner.max.x <= right_band.min.x + 0.01,
        "the vertical bar's strip (out to x={}) must stay INBOARD of the right band (from \
             x={}) — that is what the 8pt side inset buys",
        inner.max.x,
        right_band.min.x
    );
    assert!(
        inner.max.x - BAR_W < right_band.min.x,
        "non-vacuous: the bar strip must actually sit against the band, not off in the middle \
             of the window ({} vs {})",
        inner.max.x - BAR_W,
        right_band.min.x
    );

    // ...and the ACCEPTED 2pt overlap at the bottom, where the inset is 6 rather than 8. The
    // bands are allocated LAST and `hit_test.rs`'s `find_closest_within` breaks a distance tie
    // by taking "the last one = the one on top", so the BAND wins those points and the
    // horizontal bar's grab area is 2pt thinner — never the other way round.
    let overlap = inner.max.y - bottom_band.min.y;
    assert!(
        (overlap - 2.0).abs() < 0.5,
        "the bottom band must overlap the horizontal bar's strip by the measured 2pt (inset 6 \
             vs band 8), not {overlap}"
    );
}

#[test]
fn a_fill_window_never_takes_the_latch() {
    // The safety half. A `fills` kind (Chart/DOM/Polymarket) is app-owned from frame one and
    // its body sizes itself to the available rect — handing it `scrolls == true` would wrap a
    // self-filling body in a ScrollArea and break it. Its band drags must therefore keep
    // working while the latch stays untaken.
    //
    // MUTATIONS that must redden this: drop the `!fills` guard around `w.user_sized = true`
    // (the first half), or drop the `!fills` from `BodyBounds { scrolls: !fills && … }` (the
    // second half — which no latch guard can catch, because `user_sized` is set BY HAND there).
    let mut h = Harness::fill();
    let mut w = WinState::new(
        "chartfix",
        "BTCUSDT",
        "1m",
        WinKind::Chart,
        Rect::from_min_size(pos2(200.0, 150.0), vec2(700.0, 500.0)),
    );
    h.frames(&mut w, 14);

    let width0 = w.size.x;
    drag_band(&mut h, &mut w, 1, vec2(120.0, 0.0));

    // Non-vacuous: prove the band actually fired before believing what it did NOT set.
    assert!(
        w.size.x < width0 - 50.0,
        "the fill window must still resize from its left band: {width0} -> {}",
        w.size.x
    );
    assert!(!w.user_sized, "a fills kind must NEVER take the latch");
    assert!(!h.scrolled.get(), "...and must never be told to scroll its body");

    // ⚠ The assertion above could not fail on its own: `scrolls` is `!fills && user_sized` and
    // the latch guard already holds `user_sized` false, so it restates the line before it. Set
    // the flag BY HAND and the `scrolls` expression is falsified independently of the latch.
    w.user_sized = true;
    h.frame(&mut w);
    assert!(
        !h.scrolled.get(),
        "a `fills` kind must not scroll its body even with `user_sized` set by hand — the \
             `!fills` in `scrolls` is a second, independent guard"
    );
}

#[test]
fn bands_do_not_register_while_maximized_or_pinned() {
    // A band must not fire while the window is maximized (its geometry is the full bounds
    // every frame) nor during the arrange/restore settle (`force_frames > 0`), where the app
    // is still committing a forced rect.
    //
    // MUTATIONS that must redden this: drop `!w.maximized` from `resizable_now` (the first
    // arm), or drop `w.force_frames == 0` (the second).
    //
    // ⚠ Each arm gets its OWN context, reads after several frames in the condition, and then
    // CLEARS the condition and asserts the band comes BACK. Without that second half,
    // `read_response(..).is_none()` is also the answer for a window that was never shown, an
    // id that changed, or a fixture too small for `window_resize_handles`' own size guard —
    // none of which is the thing under test. (`Context::read_response` also falls back to an
    // earlier pass, so a single frame could answer with a band allocated before the condition
    // was set.)
    fn left(h: &Harness, w: &WinState) -> Option<Rect> {
        h.band(w, 1u8)
    }
    fn fixture(id: &str) -> WinState {
        WinState::tool(
            id,
            WinKind::Data,
            Rect::from_min_size(pos2(200.0, 150.0), vec2(600.0, 420.0)),
        )
    }

    // CONTROL — the same fixture with neither guard active DOES allocate a band, so the two
    // negatives below cannot pass vacuously.
    let mut h = Harness::tool(vec2(560.0, 460.0));
    let w = settled_tool(&mut h, WinKind::Data, pos2(200.0, 150.0), vec2(600.0, 420.0));
    assert!(left(&h, &w).is_some(), "control: a settled tool window allocates its left band");

    // MAXIMIZED — and then un-maximized, which must bring the band back.
    let mut h = Harness::tool(vec2(560.0, 460.0));
    let mut w = fixture("maxed");
    w.maximized = true;
    h.frames(&mut w, 14);
    assert!(left(&h, &w).is_none(), "no band may be allocated while maximized");
    w.maximized = false;
    h.frames(&mut w, 14);
    assert!(left(&h, &w).is_some(), "...and the band must return once it is restored");

    // FORCE-PINNED: a freshly-opened window carries `OPEN_PIN_FRAMES`, so three frames in is
    // still mid-settle — and once the pin drains the band must appear.
    let mut h = Harness::tool(vec2(560.0, 460.0));
    let mut w = fixture("pinned");
    h.frames(&mut w, 3);
    assert!(
        w.force_frames > 0,
        "the fixture must still be mid-settle for this arm to mean anything"
    );
    assert!(left(&h, &w).is_none(), "no band may be allocated during a forced-geometry pin");
    h.frames(&mut w, 14);
    assert_eq!(w.force_frames, 0, "the pin must have drained");
    assert!(left(&h, &w).is_some(), "...and the band must appear once it has");
}
