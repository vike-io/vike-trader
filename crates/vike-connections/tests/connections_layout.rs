//! **The Connections credential panel's BALANCE, measured off the rendered frame at five widths.**
//!
//! ⚠⚠ **THIS SUITE EXISTS BECAUSE EVERY EXISTING TEST OF THIS PANEL READS TEXT, AND THE DEFECT WAS
//! GEOMETRY.** The owner's report against a live capture was *"i see ui is disbalanced"*, and
//! every word on that frame was correct: a full-width legend SENTENCE running edge to edge across
//! a ~2000pt window, a rail that ended at y≈600 and a detail pane that ended at y≈400 inside a
//! ~1250pt-tall body, and the foot strip pinned — correctly — to a floor 600pt below the last
//! thing it described. A tree-reading assertion cannot see any of that; `rect()` can.
//!
//! The mechanism, because it is what these tests actually pin. A tool window AUTO-SIZES TO ITS
//! CONTENT on both axes: `egui-0.36.1`'s `Window::show_dyn` builds its `Resize` with
//! `.with_stroke(false)` and then `resizable(false)`, and `Resize::end` then reports
//! `size[d] = last_content_size[d]`. So the widest and the tallest thing this panel draws is an
//! INSTRUCTION about how big the window should be, and two of them were wrong:
//!
//! * the ~250-character legend sentence, a wrapping `Label`, which wraps at
//!   `ui.available_width()` and therefore laid out on ONE LINE in a window free to grow;
//! * the rail, allocated `egui::vec2(RAIL_W, ui.available_height())`, beside a vertical
//!   `ui.separator()` which takes `available_size_before_wrap()` — the same height again.
//!
//! # ⚠⚠ THE TWO AXES ARE GATED BY OPPOSITE CLAIMS, AND ONE OF THEM WAS INVERTED
//!
//! This suite first stated ONE property for both axes — the content rect is IDENTICAL at two
//! window sizes — and that was right about the height and wrong about the width. The approved grid
//! is `minmax(190px, 232px) minmax(0, 1fr)`: the RAIL is bounded, the DETAIL pane FILLS. Demanding
//! an identical rect at 900 and 1600 was demanding a panel that refuses the window, which is what
//! a maximized 2560×1600 live capture then showed — an ~866pt column in the top-left corner.
//!
//! * **HEIGHT** — [`the_panels_height_is_independent_of_the_windows`], unchanged in substance. A
//!   panel whose height tracks the window's tells an auto-sizing window to stay that tall and
//!   strands the foot strip below the content.
//! * **WIDTH** — [`the_panel_fills_the_width_it_is_given`], the inverted half, plus
//!   [`the_windows_sizing_loop_converges_with_a_filling_detail_pane`], which answers the runaway
//!   risk that inversion takes on by DRIVING the real `egui::Resize` rather than reasoning about
//!   it. What still may not fill is PROSE (the rail footnote's measure) and the RAIL (its clamp),
//!   and both keep their own gates below.
//!
//! ⚠ **The REAL `connections_ui` is driven, never a stub.** That is the lesson of #1857, where the
//! one composition harness in the tree passed `ui.allocate_space(available)` as its body and the
//! shipped narrow arm had therefore never been rendered by any test at all.
//!
//! Widths: **400, 560, 820, 1300, 2560** — the design's own two (400 and 1300), the width the tool
//! window opens at (560, the one-column arm that shipped broken), the breakpoint itself (820), and
//! the MAXIMIZED size the owner actually runs, which is where the corner-column defect was seen.

use std::collections::HashMap;
use std::path::Path;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_connections::{
    AccountGrids, ConnectionState, CredentialWrite, StoreHealth, VenueCredStatus, connections_ui,
};

/// A store path this suite never writes, and which no walk produced — relative and deliberately
/// absurd, so a stray write is a file review would notice rather than a mutation of whoever's
/// `secrets.env` the working directory happens to sit above. Same idiom (and the same reason) as
/// `connections_a11y.rs`'s.
const NEVER_WRITTEN_STORE: &str = "connections-layout-tests-never-save-to-this.env";

/// The approved design's rail bounds — `grid-template-columns: minmax(190px, 232px)`. Restated
/// here rather than exported from the crate because a test that imported the production constant
/// would be comparing a number to itself; these are the DESIGN's numbers, and the assertion is
/// that the rendered rail sits inside them.
const DESIGN_RAIL_MIN: f32 = 190.0;
const DESIGN_RAIL_MAX: f32 = 232.0;

/// The width below which `connections_ui` collapses to one column — `RAIL_DETAIL_MIN_W`. The two
/// arms are different code and both are driven below.
const BREAKPOINT: f32 = 820.0;

/// The width of the MAXIMIZED window on the owner's own box, where the corner-column defect was
/// captured. It is in every width sweep below because a panel that behaves at 1300 and refuses the
/// window at 2560 is exactly what shipped — the measure that caused it was 866pt, so every width
/// the first version of this suite drove was close enough to it to look correct.
const MAXIMIZED_W: f32 = 2560.0;

/// The glyph `view.rs`'s `tier_row` labels its edit button with.
const PENCIL: &str = "\u{270E}";

/// The per-character width floor a rendered venue name must clear — see
/// [`every_venue_is_rendered_at_every_width`], where the flat-number version of this is argued
/// against. A monospace advance is ~0.6 × the point size, so this is ~75% of the narrow rail's
/// real advance and roughly three times the ~6.6pt a squeezed chip measured in total.
const MIN_PT_PER_CHAR: f32 = 5.0;

/// The rail footer's sentence must stay ON THE TREE — see `rail_footnote`, and
/// `connections_a11y.rs`'s `the_feed_status_is_labelled_and_an_absent_producer_is_not_unknown`,
/// which reads this phrase to prove the panel keeps credential presence and feed state apart.
const FOOTNOTE_NEEDLE: &str = "CREDENTIAL PRESENCE";

// ------------------------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------------------------

/// The REAL panel over the REAL venue roster, at a given window size. An EMPTY store — the shape a
/// box with no credentials has, and the QA root's — so no dot here is a measurement of anything.
fn panel(size: egui::Vec2) -> Harness<'static, ()> {
    let grids = AccountGrids::from_vars(&HashMap::new());
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { store: Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 };
    let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
    });
    h.run();
    h
}

fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

/// One node's text, whatever role carries it.
fn node_text(n: &Node<'_>) -> String {
    let a = n.accesskit_node();
    match (a.label(), a.value()) {
        (Some(l), _) if !l.is_empty() => l.to_string(),
        (_, Some(v)) => v.to_string(),
        _ => String::new(),
    }
}

fn tree_text(h: &Harness<'_, ()>) -> String {
    h.root().children_recursive().map(|n| node_text(&n)).collect::<Vec<_>>().join("\n")
}

/// Every node that carries a bounding box, as (text, rect).
///
/// ⚠ `bounding_box()` is what separates a node that was LAID OUT from one that exists only as a
/// tree entry — and the `rect()` is read off the node rather than recomputed, so this is the
/// panel's own arithmetic and not a second copy of it.
fn placed(h: &Harness<'_, ()>) -> Vec<(String, egui::Rect)> {
    nodes(h, |_| true)
        .iter()
        .filter_map(|n| n.accesskit_node().bounding_box().map(|_| (node_text(n), n.rect())))
        .filter(|(t, _)| !t.is_empty())
        .collect()
}

/// The bounding box of everything the panel laid out.
fn content_rect(h: &Harness<'_, ()>) -> egui::Rect {
    placed(h).iter().fold(egui::Rect::NOTHING, |acc, (_, r)| acc.union(*r))
}

/// The rects of every node whose text is exactly `want`.
fn rects_of(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    placed(h).into_iter().filter(|(t, _)| t == want).map(|(_, r)| r).collect()
}

/// Every node whose text CONTAINS `needle`, with its rect.
fn rects_containing(h: &Harness<'_, ()>, needle: &str) -> Vec<egui::Rect> {
    placed(h).into_iter().filter(|(t, _)| t.contains(needle)).map(|(_, r)| r).collect()
}

/// The five legend entries' rects, read by their exact rendered text.
///
/// ⚠ Spelled `<glyph> = <word>` on purpose — see `view.rs`'s `mark_legend_items`: the `=` is what
/// keeps a legend ENTRY distinguishable from a detail-pane status CELL, which is the distinction
/// `connections_a11y.rs`'s unreadable-store gate is built on. A test that asked for the design's
/// `● configured` here would be asking for that gate to be broken.
fn legend_rects(h: &Harness<'_, ()>) -> Vec<egui::Rect> {
    ["\u{25CF} = configured", "\u{25CB} = not set", "\u{00B7} = no such tier", "? = not measured"]
        .iter()
        .flat_map(|t| rects_of(h, t))
        .collect()
}

// ------------------------------------------------------------------------------------------
// 1 — the legend is a LINE, not a wall
// ------------------------------------------------------------------------------------------

/// ⚠⚠ **THE LEGEND IS ONE COMPACT LINE ON THE ACCOUNT-STRIP ROW, AND NO SENTENCE RUNS ACROSS THE
/// PANEL.**
///
/// Three claims, each reddening on a different half of the reported defect:
///
/// 1. every legend entry shares one row (their rects overlap vertically) — a legend that wrapped
///    into a block is a legend that is back to being a band;
/// 2. the legend sits to the RIGHT of the `Account` label it is aligned opposite, which is the
///    design's placement and the thing that makes it inline rather than a row of its own;
/// 3. the long explanation is NOT laid out at panel width. It is the rail's footer now, wrapped in
///    the rail column, and this asserts the WIDTH of the node rather than its presence — a
///    presence check passed on the shipped frame, where the sentence was 1500pt wide.
#[test]
fn the_legend_is_one_line_and_the_long_note_is_not_a_full_width_band() {
    let h = panel(egui::vec2(1300.0, 900.0));
    let legend = legend_rects(&h);
    assert!(
        legend.len() >= 4,
        "all four mark entries are on the tree, spelled `<glyph> = <word>`:\n{}",
        tree_text(&h)
    );
    let top = legend.iter().map(|r| r.top()).fold(f32::NEG_INFINITY, f32::max);
    let bottom = legend.iter().map(|r| r.bottom()).fold(f32::INFINITY, f32::min);
    assert!(
        bottom >= top,
        "the legend entries must share ONE row — they do not overlap vertically, so the run \
         wrapped into a block: {legend:?}"
    );

    // ⚠ The EXTENT, never the COUNT. egui files one widget's text on the widget's own node AND on
    // an enclosing one, so a count is a property of egui's node shape rather than of this panel —
    // `connections_a11y.rs`'s `rail_marks` says the same thing about the same hazard. MEASURED
    // here: `Account` comes back on two nodes while each legend entry comes back on one.
    let account = rects_of(&h, "Account");
    assert!(!account.is_empty(), "the account strip's own label:\n{}", tree_text(&h));
    let account_right = account.iter().map(|r| r.right()).fold(f32::NEG_INFINITY, f32::max);
    let account_bottom = account.iter().map(|r| r.bottom()).fold(f32::NEG_INFINITY, f32::max);
    let legend_left = legend.iter().map(|r| r.left()).fold(f32::INFINITY, f32::min);
    assert!(
        legend_left > account_right,
        "the legend is right-aligned OPPOSITE the account strip, on its row — it is at x = \
         {legend_left:.0} and the `Account` label ends at x = {account_right:.0}"
    );
    assert!(
        legend.iter().all(|r| r.top() < account_bottom + 4.0),
        "…and on the SAME row, not under it: legend {legend:?} vs Account bottom {account_bottom}"
    );

    // 3 — the sentence. It is in the rail column, so its width is the rail's, not the panel's.
    let note = rects_containing(&h, FOOTNOTE_NEEDLE);
    assert!(!note.is_empty(), "the explanation is still on the TREE: {}", tree_text(&h));
    let widest = note.iter().map(|r| r.width()).fold(0.0_f32, f32::max);
    assert!(
        widest <= DESIGN_RAIL_MAX + 2.0,
        "⚠ THE REPORTED DEFECT: the mark explanation is laid out {widest:.0}pt wide. It belongs in \
         the RAIL's footer (≤ {DESIGN_RAIL_MAX:.0}pt, the design's rail maximum), not as a \
         sentence band across the panel — a wrapping `Label` at panel width is what made the \
         window open at ~2000pt in the first place. See `view.rs`'s `rail_footnote`."
    );
}

// ------------------------------------------------------------------------------------------
// 2 — the two axes, which obey OPPOSITE rules
// ------------------------------------------------------------------------------------------

/// ⚠⚠ **HEIGHT IS INDEPENDENT OF THE WINDOW'S — and that is what keeps the foot strip honest.**
///
/// The same content HEIGHT in a 900×700 window and in a 1600×1300 one. A rail claiming
/// `ui.available_height()` (or a vertical `ui.separator()` beside it, which takes
/// `available_size_before_wrap()`) makes the content 600pt taller in the taller window, and in a
/// window that auto-sizes to its content that is an instruction to BE that tall — which is what
/// put a ~1250pt window around ~600pt of content and left the strip pinned to a floor far below
/// the last thing it described.
///
/// ⚠ **There is deliberately NO vertical analogue of the width gate below.** This panel is a FORM:
/// its natural height is the sum of its rows, and nothing in it should stretch to fill a tall
/// window. A window taller than the form is slack, and the strip's pinning is what the slack is
/// for. See `view.rs`'s module doc, where the asymmetry is the rule rather than an oversight.
///
/// ⚠ Stated as INDEPENDENCE rather than as a bound. A pinned number would need rebaselining every
/// time a venue name or a key name changes length; this reddens on exactly the mistake that was
/// made.
#[test]
fn the_panels_height_is_independent_of_the_windows() {
    let small = content_rect(&panel(egui::vec2(900.0, 700.0)));
    let large = content_rect(&panel(egui::vec2(1600.0, 1300.0)));
    assert!(
        (small.height() - large.height()).abs() < 1.0,
        "the panel's content is {:.0}pt tall in a 700pt window and {:.0}pt tall in a 1300pt one — \
         something in it is claiming `ui.available_height()`, which is what made the window grow \
         to the arena and left the foot strip pinned to a floor far below the content. See \
         `view.rs`'s `connections_ui` two-column arm.",
        small.height(),
        large.height()
    );
}

/// ⚠⚠ **WIDTH IS THE OPPOSITE CLAIM: THE PANEL TAKES THE WHOLE WIDTH IT IS GIVEN.**
///
/// ⚠ **This assertion is the INVERSE of the one that stood here**, and the inversion is the point
/// rather than a weakening. The previous gate demanded the content rect be IDENTICAL at 900 and
/// 1600 and additionally `<= 900pt` — i.e. that the panel have a measure of its own. That was the
/// wrong half of the approved design: the grid is `minmax(190px, 232px) minmax(0, 1fr)`, so the
/// RAIL is the bounded column (still gated, by
/// [`the_rail_column_sits_in_the_designs_range`]) and the DETAIL pane is `1fr`. Capping the panel
/// capped both, and the live capture that found it was a maximized 2560×1600 window holding an
/// ~866pt column in its top-left corner with every rule stopping a third of the way across — the
/// same "disbalanced" report, reached from the other side.
///
/// What is NOT inverted, and is what stops this being a silent weakening: prose keeps its measure
/// ([`the_legend_is_one_line_and_the_long_note_is_not_a_full_width_band`] still holds the
/// explanation inside the rail's width), the rail keeps its clamp, and the HEIGHT gate above is
/// untouched. The runaway risk this trades for is answered by
/// [`the_windows_sizing_loop_converges_with_a_filling_detail_pane`], which runs the real sizer.
#[test]
fn the_panel_fills_the_width_it_is_given() {
    for w in [900.0_f32, 1300.0, 1600.0, 2560.0] {
        let h = panel(egui::vec2(w, 1000.0));
        let content = content_rect(&h);
        // The host container's side margins are the only thing between the panel and the window
        // edge, so "filled" is the window minus a small gutter — never a fraction of it.
        let filled = content.right() >= w - HOST_MARGIN_SLACK;
        assert!(
            filled,
            "{w}pt window: the panel's content ends at x = {:.0}, leaving {:.0}pt of the window \
             unused. The detail pane is the design's `1fr` and must take the width the rail does \
             not — a panel that stops a third of the way across is the column-in-the-corner the \
             live 2560×1600 capture reported.",
            content.right(),
            w - content.right()
        );
    }
}

/// How much of the window's width may be left unused before the panel counts as not filling it —
/// the host container's side margins (egui's own `CentralPanel` uses 8pt a side) plus a point of
/// rounding. Deliberately small: the defect being gated is a pane that used two fifths of a
/// maximized window, not one that is a few points shy of the edge.
const HOST_MARGIN_SLACK: f32 = 20.0;

/// ⚠⚠ **THE SIZING LOOP CONVERGES — VERIFIED BY RUNNING IT, not by arguing about it.**
///
/// A filling child and an auto-sizing window are a feedback loop, and this repo has already been
/// bitten once by a child that grew the window that then grew the child. The argument that filling
/// is safe is that `Resize::begin`'s `desired_size = desired_size.max(last_content_size)` has
/// `x.max(x) == x` as a fixed point when the child reports exactly what it was offered — but the
/// wrapping-`Label` blow-up had the same shape and DID run away, so an argument is not evidence.
///
/// So this drives the REAL `egui::Resize` with the flags a window gives it — `Window::new` builds
/// its `Resize` with `.with_stroke(false)` and `Window::show_dyn` then applies `resizable(false)`,
/// which together are what make `Resize::end` report `size[d] = last_content_size[d]` — and runs
/// frames until the reported size stops moving.
///
/// ⚠ **TWO SEEDS, and the second is the one that is not vacuous.** 560pt is the size the tool
/// window opens at, and it is below every measure this panel has ever had — so a run seeded there
/// converges whether the pane fills or is capped, and on its own it would prove nothing about the
/// change that made this test necessary. The WIDE seed puts the panel in the state the live
/// capture was taken in: handed far more width than its content needs, filling it, and reporting
/// that back to the sizer that handed it over. That is the loop, and it has to close.
///
/// Reddens on a runaway (the size never settles, or it reaches the arena ceiling) and on a
/// collapse (a wide seed that settles back near the narrow one would mean the fill stopped
/// working, which is the corner-column defect returning).
#[test]
fn the_windows_sizing_loop_converges_with_a_filling_detail_pane() {
    // The arena the real window is constrained to — `show_window`'s `constrain_to(bounds)`, which
    // `Window::show_dyn` folds into `resize.max_size`. A runaway ENDS here rather than diverging,
    // so reaching it IS the failure.
    let arena = egui::vec2(MAXIMIZED_W, 1600.0);

    let settle = |seed: egui::Vec2| -> Vec<egui::Vec2> {
        let grids = AccountGrids::from_vars(&HashMap::new());
        let live: HashMap<String, ConnectionState> = HashMap::new();
        let creds =
            CredentialWrite { store: Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 };
        let seen: std::rc::Rc<std::cell::Cell<egui::Vec2>> = Default::default();
        let inner = seen.clone();
        let mut h = Harness::builder().with_size(arena).build_ui(move |ui| {
            let out = egui::Resize::default()
                .id_salt("connections-sizing-loop")
                .with_stroke(false)
                .resizable(false)
                .default_size(seed)
                .max_size(arena)
                .show(ui, |ui| {
                    connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
                    ui.min_rect().size()
                });
            inner.set(out);
        });
        let mut sizes: Vec<egui::Vec2> = Vec::new();
        for _ in 0..12 {
            h.run();
            sizes.push(seen.get());
            if sizes.len() >= 2 && (sizes[sizes.len() - 1] - sizes[sizes.len() - 2]).length() < 0.5
            {
                break;
            }
        }
        sizes
    };

    // The wide seed is deliberately short of the arena, so "it reached the ceiling" stays a
    // distinguishable failure rather than the starting condition.
    let wide_seed = egui::vec2(MAXIMIZED_W - 200.0, 1400.0);
    for (what, seed) in
        [("the opening size", egui::vec2(560.0, 400.0)), ("a wide window", wide_seed)]
    {
        let sizes = settle(seed);
        let settled = *sizes.last().expect("at least one frame ran");
        assert!(
            sizes.len() < 12,
            "{what}: the window's sizing loop did not settle in 12 frames — each frame's content \
             is larger than the last, which is the runaway a filling child is supposed not to \
             cause: {sizes:?}"
        );
        assert!(
            settled.x < arena.x - 1.0,
            "{what}: the loop ran away to the arena ceiling ({:.0}pt of {:.0}pt). Reaching the \
             ceiling is not convergence — it is a runaway that `constrain_to` stopped: {sizes:?}",
            settled.x,
            arena.x
        );
        assert!(
            (settled.x - seed.x).abs() < 1.0,
            "{what}: the panel settled at {:.0}pt in a {:.0}pt container. A filling child reports \
             back exactly what it was offered — anything else means it is either growing the \
             window (a runaway) or refusing it (the corner-column defect): {sizes:?}",
            settled.x,
            seed.x
        );
    }
}

// ------------------------------------------------------------------------------------------
// 3 — the rail is bounded in the design's range
// ------------------------------------------------------------------------------------------

/// ⚠ **THE RAIL IS A BOUNDED COLUMN IN THE DESIGN'S `minmax(190px, 232px)`, NOT A CRAMPED FIXED
/// ONE.**
///
/// It was `const RAIL_W: f32 = 168.0` — below the design's own floor, a venue name plus three dots
/// with nothing left over, which is the "cramped" half of the owner's report.
///
/// Measured as the gap between the rail's left edge and the DETAIL pane's, because that is the
/// column: the detail pane starts one rail plus one gutter in. Read off the selected venue's own
/// heading, which is the detail pane's first widget.
/// ⚠ **The widths here are all comfortably ABOVE [`BREAKPOINT`], and that is not laziness.** The
/// breakpoint is compared against the PANEL's available width, and a harness of width W hands the
/// panel W minus the host container's side margins (16pt for egui's own `CentralPanel`) — so an
/// 820pt harness renders the ONE-column arm, which has no rail column to measure. That arm is
/// driven by [`nothing_is_laid_out_past_the_windows_right_edge`] and
/// [`the_mark_explanation_is_on_the_tree_in_both_arms`], which both include 820.
#[test]
fn the_rail_column_sits_in_the_designs_range() {
    for w in [900.0_f32, 1300.0, 1600.0] {
        let h = panel(egui::vec2(w, 900.0));
        let content = content_rect(&h);
        let pencils = nodes(&h, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some(PENCIL)
        });
        let detail_left = pencils
            .iter()
            .filter_map(|n| n.accesskit_node().bounding_box().map(|_| n.rect().left()))
            .fold(f32::INFINITY, f32::min);
        drop(pencils);
        assert!(
            detail_left.is_finite(),
            "{w}: the detail pane rendered no ✎ to measure from:\n{}",
            tree_text(&h)
        );
        let column = detail_left - content.left();
        assert!(
            column >= DESIGN_RAIL_MIN,
            "{w}: the detail pane starts {column:.0}pt from the panel's left edge, so the rail \
             column is NARROWER than the design's {DESIGN_RAIL_MIN:.0}pt minimum — that is the \
             cramped 168pt rail this fix widened"
        );
        // A gutter and the tier row's own leading cells sit between the rail and the ✎, so the
        // ceiling is the design's maximum plus that lead-in rather than the maximum itself. The
        // claim is BOUNDED — a rail that grew with the window would blow straight past this.
        assert!(
            column <= DESIGN_RAIL_MAX + 220.0,
            "{w}: the rail column is ~{column:.0}pt — it is tracking the window rather than \
             sitting in the design's {DESIGN_RAIL_MIN:.0}–{DESIGN_RAIL_MAX:.0}pt range"
        );
    }
}

// ------------------------------------------------------------------------------------------
// 4 — every width renders, and nothing leaves the window on the narrow ones
// ------------------------------------------------------------------------------------------

/// ⚠⚠ **NOTHING IS LAID OUT PAST THE RIGHT EDGE AT ANY OF THE FOUR WIDTHS.**
///
/// This is the clip-rect property stated as geometry: `egui-0.36.1/src/hit_test.rs` drops a widget
/// whose `interact_rect` (`clip_rect ∩ rect`) is negative, so a control pushed past the window's
/// right edge is present in the accessibility tree, with its label, and dead to every click. Six
/// venue chips shipped in exactly that state (#1857).
///
/// ⚠ 400pt is in the list because the narrow arm is the one that shipped broken, and because the
/// legend's right-align has an arm there: with no room for the run it pads NOTHING and lets the
/// row wrap, and padding a row too narrow for the run would push its tail off this very edge.
#[test]
fn nothing_is_laid_out_past_the_windows_right_edge() {
    for w in [400.0_f32, 560.0, BREAKPOINT, 1300.0, MAXIMIZED_W] {
        let h = panel(egui::vec2(w, 1000.0));
        let over: Vec<(String, egui::Rect)> =
            placed(&h).into_iter().filter(|(_, r)| r.right() > w + 0.5).collect();
        assert!(
            over.is_empty(),
            "{w}pt: these were laid out past the window's right edge, where the hit test drops \
             them: {over:?}"
        );
    }
}

/// ⚠ **THE FOOTNOTE SURVIVES BOTH ARMS.** It is the rail's footer in the two-column shape and a
/// foot note in the narrow one, and in BOTH it must be a tree node — a hover would hide the one
/// sentence that keeps credential presence and live feed state apart, which is the property
/// `connections_a11y.rs`'s `the_feed_status_is_labelled_and_an_absent_producer_is_not_unknown`
/// gates. That suite drives 1000pt only, i.e. the two-column arm; this one drives both.
#[test]
fn the_mark_explanation_is_on_the_tree_in_both_arms() {
    for w in [400.0_f32, 560.0, BREAKPOINT, 1300.0, MAXIMIZED_W] {
        let h = panel(egui::vec2(w, 1200.0));
        let note = rects_containing(&h, FOOTNOTE_NEEDLE);
        assert!(
            !note.is_empty(),
            "{w}pt: the mark explanation reached no node — a hover is not on the tree, and this \
             sentence is what says the marks are credential presence rather than feed state:\n{}",
            tree_text(&h)
        );
        let widest = note.iter().map(|r| r.width()).fold(0.0_f32, f32::max);
        assert!(
            widest <= w,
            "{w}pt: …and it is set in a column rather than across the window: {widest:.0}pt"
        );
    }
}

// ------------------------------------------------------------------------------------------
// 5 — the panel still renders what it renders
// ------------------------------------------------------------------------------------------

/// ⚠ **THE WHOLE VENUE ROSTER IS STILL IN THE RAIL AT EVERY WIDTH.** A layout change that dropped
/// a venue — or squeezed it to a sliver — is the #1857 failure wearing different clothes, and the
/// roster being COMPLETE is also `scripts/qa_shots.sh`'s first judging line for this window.
#[test]
fn every_venue_is_rendered_at_every_width() {
    for w in [400.0_f32, 560.0, BREAKPOINT, 1300.0, MAXIMIZED_W] {
        let h = panel(egui::vec2(w, 1200.0));
        let text = tree_text(&h);
        for venue in vike_connections::VENUES {
            assert!(text.contains(venue), "{w}pt: {venue} is missing from the rail:\n{text}");
        }
        // …and each name was actually LAID OUT, with a rect wide enough to read and to click.
        //
        // ⚠ The floor is PER CHARACTER, not a flat number, and the first cut of this test got that
        // wrong: six chips shipped squeezed to ~6.6pt REGARDLESS of name length, while `okx`
        // legitimately measures ~19.9pt at 11pt monospace — so a flat 20pt floor reddens on a
        // correct frame and says nothing about a sliver. `MIN_PT_PER_CHAR` is comfortably under a
        // monospace advance at either rail's text size (11pt and 12pt) and far above a sliver.
        for venue in vike_connections::VENUES {
            let rects = rects_of(&h, venue);
            assert!(!rects.is_empty(), "{w}pt: {venue} has no rect at all:\n{text}");
            let floor = MIN_PT_PER_CHAR * venue.len() as f32;
            assert!(
                rects.iter().any(|r| r.width() >= floor),
                "{w}pt: {venue} was laid out {:.1}pt wide against a {floor:.1}pt floor — that is \
                 the squeezed-to-a-sliver shape that made six chips unclickable",
                rects.iter().map(|r| r.width()).fold(0.0_f32, f32::max)
            );
        }
    }
}

/// ⚠ **A `NotConfigurable` RAIL MARK IS STILL THE MIDDLE DOT, NOT A FILLED ONE** — restated here
/// at every width because `scripts/qa_shots.sh`'s `05-connections` pose tells a human judge that a
/// single filled `●` on the credential-free QA root means the isolation broke, and this suite is
/// the one that renders the whole roster at the widths that capture is taken at.
///
/// The legend's own `●` is EXCLUDED by the `=`, which is the separator that exists for it.
#[test]
fn no_rail_mark_is_a_filled_dot_on_a_credential_free_store() {
    for w in [400.0_f32, 560.0, BREAKPOINT, 1300.0, MAXIMIZED_W] {
        let h = panel(egui::vec2(w, 1200.0));
        let filled = rects_of(&h, "\u{25CF}");
        assert!(
            filled.is_empty(),
            "{w}pt: this store holds nothing, so no rail mark may be the FILLED dot — {} of them \
             are:\n{}",
            filled.len(),
            tree_text(&h)
        );
        assert!(
            !rects_of(&h, "\u{00B7}").is_empty(),
            "{w}pt: …and the venues with no such tier still draw the middle dot"
        );
    }
}

/// The panel renders a grid with no rows without panicking and without claiming anything — the
/// degenerate input the measure container and the rail-width derivation both have to survive
/// (`name_col_w` folds over the rows, and an empty fold is `0.0`).
#[test]
fn an_empty_grid_renders_the_no_rows_sentence_and_nothing_else_claims_a_measurement() {
    let grids = AccountGrids::single(Vec::<VenueCredStatus>::new());
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let creds = CredentialWrite { store: Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 };
    let mut h = Harness::builder().with_size(egui::vec2(1300.0, 900.0)).build_ui(move |ui| {
        connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
    });
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("no venue rows to show"), "{text}");
    // ⚠ The BUTTON, not the glyph — the legend's own `✎ = edit` entry carries that character as a
    // `Label`, which is exactly the legend-versus-cell confusion the `=` separator exists to keep
    // apart, and a bare `contains` here would be an assertion that can never pass.
    let editors = nodes(&h, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(PENCIL)
    });
    assert!(editors.is_empty(), "an empty grid offers no edit control: {text}");
}
