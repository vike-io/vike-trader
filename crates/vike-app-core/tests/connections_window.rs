//! **The Connections credential panel, drawn inside the REAL window frame at the SHIPPED window
//! size, and CLICKED.**
//!
//! ⚠⚠ **THIS IS THE SUITE WHOSE ABSENCE LET A DEAD EDIT BUTTON SHIP.** Every existing test of
//! `vike_connections::connections_ui` builds its harness at `egui::vec2(1000.0, 800.0)`
//! (`crates/vike-connections/tests/connections_a11y.rs`'s `harness_grids`,
//! `crates/vike-connections/tests/credential_write_journal.rs`'s `save_binance_live`), and 1000pt
//! is above that panel's `RAIL_DETAIL_MIN_W` — so they only ever render the TWO-COLUMN arm. The
//! window OPENS at [`TOOL_WINDOW_SIZE`], i.e. 560pt, which is the ONE-COLUMN arm. And
//! `crates/vike-app-core/tests/connections_tabs.rs`'s `body_frame`, the only test of the
//! composition, passes a STUB body (`ui.allocate_space(available)`), never the real panel.
//!
//! So the shipped arm, inside the shipped chrome, at the shipped size, had never been rendered by
//! any test — and in it `rail_chips` blew the venue rail up to ~700pt (a `ui.scope` inheriting
//! `horizontal_wrapped`'s `main_wrap`, so every chip was itself a wrapping row; `view.rs`'s own doc
//! carries the mechanism). Six venue chips were squeezed to ~6pt slivers past the clip rect and
//! every `✎` landed 450-530pt below the window floor. `egui-0.36.1/src/hit_test.rs` drops a widget
//! whose `interact_rect` (`clip_rect ∩ rect`) is negative, so **clicking them did nothing** — while
//! the accessibility tree reported every one of them, with its label, exactly as if it were on
//! screen. That is the blind spot `connections_tabs.rs`'s own header warns about in its point 10,
//! and this file is what it looks like to obey it: the assertions here CLICK, and a click is the
//! one question a tree lookup cannot answer.
//!
//! Three gates, in the order the defect happens:
//!
//! 1. [`every_venue_chip_is_clickable_in_the_shipped_window`] — selection actually moves, for
//!    EVERY venue in the roster, at the shipped size. Reddens when a chip is off the clip rect.
//! 2. [`the_rail_and_the_edit_controls_fit_the_shipped_window`] — geometry, read off the tree: the
//!    rail is a rail rather than a 700pt block, and the first `✎` is inside the window it is drawn
//!    in. Reddens on the layout regression even if hit-testing somehow still worked.
//! 3. [`clicking_the_edit_control_opens_the_form_and_save_reaches_the_store`] — the whole act, at
//!    the shipped WIDTH: click `✎`, the masked form opens, type, click **Save**, and the bytes are
//!    on disk. This is the test the brief asks for and the one no harness in the tree performed:
//!    `credential_write_journal.rs` does the same walk but at a width that renders the other arm
//!    and with no window chrome at all.
//!
//! ⚠ **No credential VALUE is rendered, logged or persisted anywhere but the throwaway store.**
//! The typed constants share no six-character run with anything the panel legitimately draws, the
//! same discipline (and the same reason) as `credential_write_journal.rs`'s.

use std::collections::HashMap;

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_app_core::backend_conn::cli_observe_record;
use vike_app_core::backend_editor::EditorState;
use vike_app_core::backend_registry::BackendsFile;
use vike_app_core::tool_views::{
    BackendDigest, BackendPicker, BackendSettingsState, BodyLayout, ConnectionsTab,
    connections_body,
};
use vike_app_core::window_spawn::TOOL_WINDOW_SIZE;
use vike_app_core::workspace::WinKind;
use vike_app_core::workspace::title_bar::tool_title_bar;
use vike_connections::{
    AccountGrids, ConnectionState, CredentialWrite, StoreHealth, connections_ui, credential_summary,
};
use vike_model::scratch::ScratchDir;

/// The glyph `vike_connections::view`'s `tier_row` labels its edit button with.
const PENCIL: &str = "\u{270E}";

/// The two values typed into the LIVE form. ⚠ Chosen to share no six-character run with any venue
/// name, key name, tier token or path this panel renders — so a hit is a leak, never a coincidence.
const TYPED_KEY: &str = "zqxjvw7413mfbphgnd8256wu";
const TYPED_SECRET: &str = "pfgzmwqx9042hvbjntdu6531";

/// The venue whose LIVE form this suite drives. Generic (`{VENUE}_{TIER}_API_*`), three real
/// tiers, and first in `vike_connections::VENUES` — so it is also the venue the panel selects on
/// frame one, which keeps gate 3 independent of a chip click having worked.
const VENUE: &str = "binance";

// ------------------------------------------------------------------------------------------
// Tree helpers — the same shapes the sibling suites use.
// ------------------------------------------------------------------------------------------

fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

fn buttons_labelled<'t, 'h>(h: &'t Harness<'h, ()>, label: &str) -> Vec<Node<'t>> {
    let want = label.to_string();
    nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(want.as_str())
    })
}

fn password_fields<'t, 'h>(h: &'t Harness<'h, ()>) -> Vec<Node<'t>> {
    nodes(h, |n| n.accesskit_node().role() == Role::PasswordInput)
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

/// Every node, anywhere, whose text is exactly `want` — with the rect it was laid out at.
fn rects_of(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    nodes(h, |_| true)
        .iter()
        .filter(|n| node_text(n) == want)
        .filter_map(|n| n.accesskit_node().bounding_box().map(|_| n.rect()))
        .collect()
}

/// **Which venue the rail has selected** — read off the ROLE, which is the invariant
/// `vike_connections::view`'s `rail_chips`/`rail_row` state at themselves: *the SELECTED row is a
/// `Label`, not a `Button`*, so that "which venue am I looking at" is answerable from the tree by
/// role alone and the current selection cannot be re-picked into a no-op frame.
///
/// ⚠ Asked that way rather than by counting occurrences of the name: egui can file one label's
/// text on two nodes, so a count is a property of egui's node shape and not of the panel.
fn shown_venue(h: &Harness<'_, ()>) -> Option<String> {
    let buttons: Vec<String> = nodes(h, |n| n.accesskit_node().role() == Role::Button)
        .iter()
        .map(|n| node_text(n))
        .collect();
    vike_connections::VENUES
        .iter()
        .find(|v| !buttons.iter().any(|b| b == *v))
        .map(|v| (*v).to_string())
}

/// ⚠ **A prerequisite for driving the real title bar, not a workaround** — see
/// `crates/vike-app-core/tests/connections_tabs.rs`'s copy, which carries the whole argument:
/// `vike_ui_theme::font::extralight` only NAMES a `FontFamily` and epaint panics on an unbound one.
fn fonts_ready(ctx: &egui::Context) -> bool {
    let id = egui::Id::new("vike_named_font_families_bound");
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return true;
    }
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .expect("default FontDefinitions define Proportional");
    for name in ["extralight", "semibold", "bold", "light"] {
        fonts.families.insert(egui::FontFamily::Name(name.into()), proportional.clone());
    }
    ctx.set_fonts(fonts);
    ctx.data_mut(|d| d.insert_temp(id, true));
    ctx.request_repaint();
    false
}

// ------------------------------------------------------------------------------------------
// The harness: the REAL title bar + the REAL `connections_body` + the REAL credential panel.
// ------------------------------------------------------------------------------------------

/// A live Connections window of the given size whose Credentials tab body is the REAL
/// `vike_connections::connections_ui` over the REAL venue roster.
///
/// ⚠ `connections_body` rather than `connections_tool_content`, for the reason that seam exists:
/// the tool entry point takes a whole `ToolCtx`, which a headless suite cannot reasonably build,
/// and a chrome whose only entry point needs one is a chrome nothing gates. Everything this suite
/// asserts about — the title-bar reservation, the foot-strip row, the shrink that bounds the body
/// — is `connections_body`'s.
///
/// ⚠ **`Harness::run`'s DEFAULT step budget is kept deliberately**, and a note about why, because
/// raising it was tried first and was the wrong answer. This harness panicked with
/// `exceeded max_steps` when the detail pane gained the rule that makes it occupy its column, and
/// the temptation is to read that as "the harness needs another frame". It did not: the repaint
/// cause named `connections_body`'s strip-overflow line every time, and the strip's measured
/// height was ALTERNATING between two values for ever. Raising the budget to 8 changed nothing,
/// which is what said so.
///
/// A budget that is comfortably larger than the need hides exactly that class of defect, so this
/// harness stays on the default and the oscillation was fixed at its source
/// (`vike_app_core::tool_views::connections`'s `ambient_strip`, whose wrapped row is now allocated
/// at zero height so it cannot inflate to the room around it).
/// [`the_foot_strips_reservation_settles_instead_of_oscillating`] is the gate, in this file,
/// because the property is about a SEQUENCE of frames of the real composition.
fn window(size: egui::Vec2, creds: CredentialWrite<'_>) -> Harness<'_, ()> {
    // The roster the shipped window renders: `AccountGrids::from_vars` over an EMPTY store is what
    // a box with no credentials gets, which is also the QA root's shape. Nothing here is set, so
    // no dot can be a measurement of anything typed below.
    let grids = AccountGrids::from_vars(&HashMap::new());
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let file = BackendsFile::default();
    let rec = cli_observe_record("127.0.0.1:7879");
    let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
        if !fonts_ready(ui.ctx()) {
            return;
        }
        let (_acts, _drag, slot) = tool_title_bar(ui, WinKind::Connections, false);
        let account = vike_connections::shown_account(ui, None);
        let absent;
        let rows: &[vike_connections::VenueCredStatus] = match grids.grid_for(&account) {
            Some(rows) => rows,
            None => {
                absent = grids.absent_grid();
                &absent
            }
        };
        let cred = credential_summary(rows, &StoreHealth::Readable);
        let digest = BackendDigest::of(true, &BackendSettingsState::Idle);
        let picker =
            BackendPicker { backends: &file, active: Some(&rec), available: true, reported: None };
        let mut action = None;
        let mut editor = EditorState::Closed;
        let mut tab = ConnectionsTab::Credentials;
        connections_body(
            ui,
            &slot,
            &mut tab,
            &account,
            &cred,
            &digest,
            &picker,
            &mut action,
            &mut editor,
            |ui, _tab, _action, _editor| {
                connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
            },
        );
    });
    h.run();
    h
}

/// The same window as [`window`], stepped `frames` times, reporting what [`connections_body`]
/// laid out each frame. `run()` is deliberately NOT used: this is for the properties that are
/// about a SEQUENCE of frames, and `run()` would panic on exactly the sequence under test.
fn body_frames(size: egui::Vec2, frames: usize) -> Vec<BodyLayout> {
    let grids = AccountGrids::from_vars(&HashMap::new());
    let live: HashMap<String, ConnectionState> = HashMap::new();
    let file = BackendsFile::default();
    let rec = cli_observe_record("127.0.0.1:7879");
    let store = std::path::PathBuf::from(NEVER_WRITTEN_STORE);
    let seen: std::rc::Rc<std::cell::Cell<Option<BodyLayout>>> = Default::default();
    let inner = seen.clone();
    let mut h = Harness::builder().with_size(size).build_ui(move |ui| {
        if !fonts_ready(ui.ctx()) {
            return;
        }
        let creds = CredentialWrite { store: &store, journal: None, now_ms: 0 };
        let (_acts, _drag, slot) = tool_title_bar(ui, WinKind::Connections, false);
        let account = vike_connections::shown_account(ui, None);
        let absent;
        let rows: &[vike_connections::VenueCredStatus] = match grids.grid_for(&account) {
            Some(rows) => rows,
            None => {
                absent = grids.absent_grid();
                &absent
            }
        };
        let cred = credential_summary(rows, &StoreHealth::Readable);
        let digest = BackendDigest::of(true, &BackendSettingsState::Idle);
        // `reported: None` — this harness drives the reservation loop, not the identity, and a
        // daemon that reports nothing is the arm that renders the dial address (the pre-#1860
        // string). Nothing here depends on which address the strip names, only on how TALL the
        // strip is, so the no-report arm keeps the measurement independent of that feature.
        let picker =
            BackendPicker { backends: &file, active: Some(&rec), available: true, reported: None };
        let mut action = None;
        let mut editor = EditorState::Closed;
        let mut tab = ConnectionsTab::Credentials;
        let layout = connections_body(
            ui,
            &slot,
            &mut tab,
            &account,
            &cred,
            &digest,
            &picker,
            &mut action,
            &mut editor,
            |ui, _tab, _action, _editor| {
                connections_ui(ui, &grids, &live, &StoreHealth::Readable, creds, None);
            },
        );
        inner.set(Some(layout));
    });
    let mut out = Vec::new();
    for _ in 0..frames {
        h.step();
        if let Some(l) = seen.get() {
            out.push(l);
        }
    }
    out
}

/// ⚠⚠ **THE FOOT STRIP'S RESERVATION SETTLES INSTEAD OF OSCILLATING — the regression gate for a
/// window that repainted for ever.**
///
/// `vike_app_core::tool_views::connections`'s `strip_reservation` holds back a row for the strip
/// and is then RAISED to whatever the strip actually measured last frame, and `connections_body`
/// asks for a repaint on any frame the strip overflowed its row. That is a convergent loop only
/// while the strip's height is independent of the room it is drawn in — and it was not:
/// `ui.horizontal_wrapped` is `Layout::left_to_right(Align::Center)` with `main_wrap`, and
/// `egui-0.36.1`'s `Layout::next_frame_ignore_wrap` inflates such a row with
/// `frame_size.y = frame_size.y.max(region.cursor.height())`. So the strip measured its own
/// output, and the two values alternated for ever.
///
/// MEASURED at this exact size before the fix, every frame, never settling:
///
/// ```text
///   reserved=40.50  strip_h=36.47
///   reserved=36.47  strip_h=40.50
/// ```
///
/// …and after it, from the first drawn frame, at this size and at 1200pt tall:
/// `reserved=35.50  strip_h=35.50`.
///
/// ⚠ **This is a SEQUENCE property and needs [`body_frames`] rather than `Harness::run`** —
/// `run()` panics with `exceeded max_steps` on precisely the input under test, which is how the
/// defect surfaced but is not a claim about what went wrong. It read as a harness needing another
/// frame; raising the budget to 8 changed nothing, and the repaint cause named the same line every
/// time. Asserting the numbers is what says the loop closed rather than merely got longer.
///
/// 560x520 is the size that exposes it: the body has to be close enough to the window floor for
/// there to BE leftover room in one arm and none in the other. The pane growing by a single rule
/// was what put it there.
#[test]
fn the_foot_strips_reservation_settles_instead_of_oscillating() {
    let frames = body_frames(egui::vec2(560.0, 520.0), 8);
    // Frame one draws nothing — `fonts_ready` binds the named families and asks for a repaint —
    // so the first LAID-OUT frame is what this reads from.
    assert!(frames.len() >= 4, "the body drew too few frames to judge: {}", frames.len());
    let settled = &frames[1..];
    for (i, l) in settled.iter().enumerate() {
        assert!(
            l.strip.height() <= l.reserved + 0.5,
            "frame {i}: the strip measured {:.2}pt against a {:.2}pt reservation, so \
             `connections_body` asked for another repaint. If every frame says this, the window \
             is repainting for ever: {:?}",
            l.strip.height(),
            l.reserved,
            settled.iter().map(|l| (l.reserved, l.strip.height())).collect::<Vec<_>>()
        );
    }
    let first = settled[0];
    for (i, l) in settled.iter().enumerate() {
        assert!(
            (l.reserved - first.reserved).abs() < 0.5
                && (l.strip.height() - first.strip.height()).abs() < 0.5,
            "frame {i} disagrees with frame 0 — the reservation and the strip are chasing each \
             other rather than settling: {:?}",
            settled.iter().map(|l| (l.reserved, l.strip.height())).collect::<Vec<_>>()
        );
    }
}

/// A store path this suite never writes, and which no walk produced — relative and deliberately
/// absurd, so a stray write is a file review would notice rather than a mutation of whoever's
/// `secrets.env` the working directory happens to sit above. Same idiom (and the same reason) as
/// `connections_a11y.rs`'s.
const NEVER_WRITTEN_STORE: &str = "connections-window-tests-never-save-to-this.env";

fn dry_creds() -> CredentialWrite<'static> {
    CredentialWrite { store: std::path::Path::new(NEVER_WRITTEN_STORE), journal: None, now_ms: 0 }
}

// ------------------------------------------------------------------------------------------
// 1 — the chips
// ------------------------------------------------------------------------------------------

/// ⚠⚠ **EVERY VENUE CHIP IS CLICKABLE IN THE WINDOW THE APP ACTUALLY OPENS.**
///
/// MEASURED before the fix, at the shipped size: `bybit` and `dukascopy` selected; `polymarket`,
/// `ibkr`, `ctrader`, `alpaca`, `aster` and `hyperliquid` were laid out 6.6pt wide at x ≥ 815 —
/// past the body's clip rect at x = 812.1 — and clicking them changed NOTHING. Every one of them
/// was in the accessibility tree with its label, so a suite phrased over the tree could not have
/// seen it; this one clicks, and the selection either moves or it does not.
///
/// Reddens on any layout that pushes a chip outside the window, on a chip that stops being a
/// button, and on a rail whose chips overlap enough that a click lands on a neighbour.
#[test]
fn every_venue_chip_is_clickable_in_the_shipped_window() {
    every_chip_selects_at(TOOL_WINDOW_SIZE);
}

/// ⚠ **THE SAME WALK AT THE NARROWEST SUPPORTED WIDTH.** The approved design must work at 400pt
/// and the narrow arm is the one that shipped broken, so the click gate above is run there too —
/// and 400pt is also where `vike_connections::view`'s `account_strip` takes the arm of its
/// legend right-align that pads NOTHING and lets the row wrap. Padding a row too narrow for the
/// legend run would push its tail past the clip rect, which is precisely the state that makes a
/// widget present in the tree and dead to every click.
///
/// A taller harness than [`TOOL_WINDOW_SIZE`]'s, because at 400pt the chip rail wraps onto more
/// rows and the pane below it is genuinely taller — the WIDTH is the axis this gate is about, and
/// clipping a chip at the BOTTOM would be a different (and honest) failure of a too-short window.
#[test]
fn every_venue_chip_is_clickable_at_the_narrowest_supported_width() {
    every_chip_selects_at(egui::vec2(400.0, 700.0));
}

/// Click every venue chip in turn at `size` and report the ones whose selection did not move.
fn every_chip_selects_at(size: egui::Vec2) {
    let creds = dry_creds();
    let mut h = window(size, creds);

    // Frame one selects the grid's FIRST venue; every OTHER venue is therefore a chip button.
    let first = shown_venue(&h).expect("the panel selects a venue on frame one");
    let mut unreachable: Vec<String> = Vec::new();
    for venue in vike_connections::VENUES {
        if *venue == first {
            continue;
        }
        {
            let chips = buttons_labelled(&h, venue);
            assert_eq!(
                chips.len(),
                1,
                "{size:?}: exactly one rail chip is a button for {venue}: {}",
                tree_text(&h)
            );
            chips[0].click();
        }
        // ⚠ TWO frames. `connections_ui` applies the pick through `EditState::select_venue` AFTER
        // the rail/detail block has already been laid out, so the frame that CONSUMES the click
        // still renders the previous selection; the next one renders the new.
        h.run();
        h.run();
        if shown_venue(&h).as_deref() != Some(*venue) {
            unreachable.push((*venue).to_string());
        }
    }
    assert!(
        unreachable.is_empty(),
        "clicking these rail chips changed nothing — they are rendered outside the window's clip \
         rect, which is what makes `egui::Ui::interact` record an empty `interact_rect` and the \
         hit test drop them: {unreachable:?}"
    );
}

// ------------------------------------------------------------------------------------------
// 2 — the geometry
// ------------------------------------------------------------------------------------------

/// ⚠⚠ **THE RAIL IS A RAIL, AND THE EDIT CONTROLS ARE IN THE WINDOW.**
///
/// The clickability gate above is the property that matters; this one says WHY it failed, so a
/// regression reports the cause rather than a symptom. Both numbers come off the rendered tree:
///
/// * the rail's own extent — the vertical span of the venue-name chips. Before the fix it measured
///   ~700pt in a 400pt window (a chip alone was 143pt tall, because it was a wrapping row inside a
///   wrapping row); afterwards it is a handful of 16pt lines.
/// * the FIRST `✎`'s top. Before the fix the three edit buttons sat at y = 1439 / 1480 / 1521 —
///   450-530pt below a window floor of ~987.
///
/// The bound asserted is the WINDOW, not a pinned pixel count: "the rail fits in the window it is
/// drawn in" is the claim, and a tighter number would be a layout constant restated in a test.
#[test]
fn the_rail_and_the_edit_controls_fit_the_shipped_window() {
    let creds = dry_creds();
    let h = window(TOOL_WINDOW_SIZE, creds);

    let chip_rects: Vec<egui::Rect> =
        vike_connections::VENUES.iter().flat_map(|v| rects_of(&h, v)).collect();
    assert!(!chip_rects.is_empty(), "the rail rendered no venue at all: {}", tree_text(&h));
    let top = chip_rects.iter().map(|r| r.top()).fold(f32::INFINITY, f32::min);
    let bottom = chip_rects.iter().map(|r| r.bottom()).fold(f32::NEG_INFINITY, f32::max);
    let rail_h = bottom - top;
    assert!(
        rail_h < TOOL_WINDOW_SIZE.y,
        "the venue rail is {rail_h:.0}pt tall in a {:.0}pt window — it is not a rail, it is a \
         wrapping row nested inside a wrapping row, and everything after it (the detail pane, its \
         three ✎ buttons, the Save verdict) is pushed past the clip rect where no click reaches \
         it. See `vike_connections::view`'s `rail_chips`.",
        TOOL_WINDOW_SIZE.y
    );

    let pencils = buttons_labelled(&h, PENCIL);
    assert!(!pencils.is_empty(), "the detail pane renders no edit control: {}", tree_text(&h));
    let first = pencils
        .iter()
        .filter_map(|n| n.accesskit_node().bounding_box().map(|_| n.rect()))
        .map(|r| r.top())
        .fold(f32::INFINITY, f32::min);
    assert!(
        first < TOOL_WINDOW_SIZE.y,
        "the first ✎ is laid out at y = {first:.0} in a {:.0}pt window — below the floor, which \
         is where the shipped panel put all three of them",
        TOOL_WINDOW_SIZE.y
    );
}

/// The rail footer's sentence — the LAST thing the credential panel draws, and therefore the
/// cheapest honest measurement of "where does this panel end". Its wording is
/// `vike_connections::view`'s `FOOTNOTE`; this is the phrase
/// `crates/vike-connections/tests/connections_a11y.rs` already reads off the tree.
const PANEL_TAIL: &str = "CREDENTIAL PRESENCE";

/// ⚠⚠ **THE PANEL'S HEIGHT DOES NOT GROW WITH THE WINDOW'S — which is what un-strands the foot
/// strip.**
///
/// ⚠ **It says HEIGHT, and it used to say "does not grow" flat.** That wording was true of both
/// axes when it was written and is now true of one: the panel deliberately FILLS the width it is
/// given (the approved grid makes the detail pane `1fr`, and
/// `crates/vike-connections/tests/connections_layout.rs`'s
/// `the_panel_fills_the_width_it_is_given` is the gate). Nothing about what THIS test proves
/// changed — it only ever varied the height, and the width it drives is fixed at the shipped
/// window's — but a name that claimed both axes would now be read as covering a property it never
/// measured.
///
/// The owner's capture showed a ~1250pt-tall window whose rail ended at y≈600 and whose detail
/// pane ended at y≈400, with the ambient strip pinned to a floor ~600pt below the last thing it
/// described. ⚠ **The strip's own placement was not the defect and must not be "fixed":** it is
/// pinned to the window's foot deliberately (it is process-level state, a status bar), and
/// `scripts/qa_shots.sh`'s `05-connections` pose tells a human judge to REPORT a strip "drawn
/// immediately under a short panel with dead space below it". Un-pinning it would trade one
/// finding for another.
///
/// What was wrong is that the panel ASKED for that window. A tool window auto-sizes to its content
/// on both axes (`egui-0.36.1`'s `Window::show_dyn` → `Resize` with `.with_stroke(false)` then
/// `resizable(false)`, so `Resize::end` reports `size[d] = last_content_size[d]`), and the rail
/// allocated `ui.available_height()` outright with a full-height `ui.separator()` beside it. So
/// the body's height tracked the window's, the window grew, and the loop ended at the arena edge.
///
/// A fixed-size `Harness` cannot show a window shrinking — it has no `Resize` at all. What it CAN
/// show is the input that decides it: the panel's own extent, measured in two windows of different
/// heights. Equal extents mean an auto-sizing window would settle on the content; an extent that
/// tracks the window is the defect, whatever the strip then does.
#[test]
fn the_panels_height_does_not_grow_with_the_window_so_the_strip_is_not_stranded() {
    let tail_bottom = |size: egui::Vec2| -> f32 {
        let creds = dry_creds();
        let h = window(size, creds);
        let bottom = nodes(&h, |_| true)
            .iter()
            .filter(|n| node_text(n).contains(PANEL_TAIL))
            .filter_map(|n| n.accesskit_node().bounding_box().map(|_| n.rect().bottom()))
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            bottom.is_finite(),
            "{size:?}: the panel's tail sentence reached no node: {}",
            tree_text(&h)
        );
        bottom
    };
    let short = tail_bottom(egui::vec2(560.0, 520.0));
    let tall = tail_bottom(egui::vec2(560.0, 1200.0));
    assert!(
        (short - tall).abs() < 1.0,
        "the credential panel ends at y = {short:.0} in a 520pt window and at y = {tall:.0} in a \
         1200pt one — its height is tracking the WINDOW's, which in a window that auto-sizes to \
         its content is an instruction to stay that tall. That is what left the foot strip pinned \
         to a floor hundreds of points below the last thing it described."
    );
}

// ------------------------------------------------------------------------------------------
// 3 — the act
// ------------------------------------------------------------------------------------------

/// The window height gate 3 drives, and why it is not [`TOOL_WINDOW_SIZE`]'s.
///
/// ⚠ The WIDTH is the shipped one — that is what selects the one-column arm this whole file
/// exists for, and it is the axis the defect lived on. The HEIGHT is not, and saying so is more
/// honest than pretending: with a credential form expanded the panel is genuinely taller than
/// 400pt, and in the real window that is reached by SCROLLING (`connections_body` is drawn inside
/// `vike_app_core::workspace::state::BodyBounds::show`'s `ScrollArea`), which a bare `Harness` has
/// no equivalent of. So this gate says "at the shipped width, with the panel fully visible, the
/// act works end to end"; [`the_rail_and_the_edit_controls_fit_the_shipped_window`] is the one
/// that pins the shipped HEIGHT, and it is the one that failed.
const FLOW_SIZE: egui::Vec2 = egui::vec2(TOOL_WINDOW_SIZE.x, 900.0);

/// ⚠⚠ **THE TEST WHOSE ABSENCE LET THIS SHIP: click `✎`, the form opens, type, Save, the bytes
/// are on disk** — all of it inside the real window chrome, at the width the window opens at.
///
/// Every step is its own frame: the harness QUEUES input and `run()` is what delivers it, so
/// clicking and typing in one batch would depend on egui's intra-frame event ordering.
///
/// What each assertion here would catch on its own:
///
/// * the `✎` click producing no password field ⇒ the control is not hit-testable (the shipped
///   defect — the button existed, was `Role::Button`, and its `interact_rect` was empty);
/// * the form still open after **Save** ⇒ `render_edit_form` took its failure arm, which is the
///   state the operator could not see at all while the verdict was rendered past the window floor;
/// * the store not holding the typed key ⇒ Save is wired to nothing, and every assertion above it
///   passed for the wrong reason.
///
/// ⚠ The store is a THROWAWAY `ScratchDir`. Nothing in this workspace may rewrite the user's only
/// copy of live venue keys, which is exactly why `connections_ui` takes the path as a parameter.
#[test]
fn clicking_the_edit_control_opens_the_form_and_save_reaches_the_store() {
    // The system temp directory is legitimate in a test — `crates/vike-ops/tests/system_temp_gate.rs`
    // scopes itself to production code. `ScratchDir` is unique per process and self-deleting.
    let root = ScratchDir::create_in(&std::env::temp_dir(), "vike-connections-window")
        .expect("scratch root");
    let store = root.path().join("secrets.env");
    let creds = CredentialWrite { store: &store, journal: None, now_ms: 0 };
    let mut h = window(FLOW_SIZE, creds);

    assert_eq!(
        shown_venue(&h).as_deref(),
        Some(VENUE),
        "the panel opens on the roster's first venue: {}",
        tree_text(&h)
    );

    // Sim, Demo, Live — the LAST ✎ is the LIVE row's.
    {
        let pencils = buttons_labelled(&h, PENCIL);
        assert_eq!(pencils.len(), 3, "{VENUE} stores keys at all three tiers: {}", tree_text(&h));
        pencils.last().expect("an edit button").click();
    }
    h.run();

    let fields = password_fields(&h);
    let n = fields.len();
    drop(fields);
    assert_eq!(
        n,
        3,
        "⚠ THE DEFECT: clicking ✎ opened NO form. The button is in the tree and the click reached \
         nothing, which is what an `interact_rect` clipped to nothing looks like from here. Tree:\n{}",
        tree_text(&h)
    );
    assert!(
        tree_text(&h).contains(&format!("Edit {VENUE} / live")),
        "…and the form that opened is the LIVE row's: {}",
        tree_text(&h)
    );

    // API Key, API Secret, Passphrase (optional) — the passphrase is left BLANK, so "blank means
    // keep" is exercised by the same walk.
    for (i, value) in [TYPED_KEY, TYPED_SECRET].into_iter().enumerate() {
        {
            let fields = password_fields(&h);
            fields[i].focus();
        }
        h.run();
        {
            let fields = password_fields(&h);
            fields[i].type_text(value);
        }
        h.run();
    }

    {
        let save = buttons_labelled(&h, "Save");
        assert_eq!(save.len(), 1, "exactly one Save button is on screen: {}", tree_text(&h));
        save[0].click();
    }
    h.run();

    assert!(
        password_fields(&h).is_empty(),
        "the form must close on a successful save — it is still open, so Save failed: {}",
        tree_text(&h)
    );

    let saved = std::fs::read_to_string(&store).expect("the credential store was written");
    assert!(saved.contains(&format!("BINANCE_LIVE_API_KEY={TYPED_KEY}")), "{saved}");
    assert!(saved.contains(&format!("BINANCE_LIVE_API_SECRET={TYPED_SECRET}")), "{saved}");
    assert!(
        !saved.contains("BINANCE_LIVE_API_PASSPHRASE"),
        "a blank field means leave unchanged — it must not be written: {saved}"
    );

    // ⚠ And the VERDICT is on screen, in the pane, next to the row that was clicked — not at the
    // foot of a panel taller than the window. It was the latter, which is why a Save that DID fail
    // looked exactly like a button that did nothing.
    let text = tree_text(&h);
    let verdict = format!("saved credentials for {VENUE}/live");
    assert!(text.contains(&verdict), "the Save verdict reaches the screen: {text}");
    let verdict_rects = rects_of(&h, &verdict);
    assert!(!verdict_rects.is_empty(), "…with a rect: {text}");
    assert!(
        verdict_rects.iter().all(|r| r.bottom() <= FLOW_SIZE.y),
        "…inside the window, which is the whole point of moving it beside the row: {verdict_rects:?}"
    );

    // ⚠ NOT ONE CHARACTER of either typed value reached the screen. The masked fields are cleared
    // on close and the verdict names a venue and a tier, never a value.
    for secret in [TYPED_KEY, TYPED_SECRET] {
        assert!(
            !text.contains(secret),
            "a typed credential reached the accessibility tree — that is the one thing this panel \
             may never do: {text}"
        );
    }
}
