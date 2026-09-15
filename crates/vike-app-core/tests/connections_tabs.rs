//! **The Connections tool's two-tab chrome, its status line, its ambient strip and the Backend
//! tab's settings editor — gated end to end, headlessly, off the accessibility tree.**
//!
//! `vike-desktop` is not in the derived CI roster and
//! `crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs` exists to keep it that way, so every
//! decision this window makes lives in `vike-app-core` and is gated here. Layout and geometry are
//! CPU work — `egui::Context::run_ui` computes them with no GPU — so this suite runs on the
//! GPU-less CI runners like every other headless UI suite in the tree.
//!
//! Five properties, each with the defect it exists for:
//!
//! 1. **The selected tab is answerable from the tree by ROLE.** The selected segment is a
//!    `Label` and the other a `Button`, the idiom `vike_connections`' account chips already use.
//!    A pair of `SelectableLabel`s would make "which tab am I on" unreadable here and would let
//!    the current tab be re-picked into a no-op frame.
//! 2. **No count is invented.** The Backend segment renders NO number in every state where the
//!    node has not answered. A `0` there is the exact shape of fabrication this window is not
//!    allowed to produce — the mockup's own notes record an earlier draft inventing one.
//! 3. **Neither tab's state is ever hidden.** One status line carries the ACTIVE tab spelled out
//!    and the INACTIVE one digested; switching swaps which is which.
//! 4. **The ambient strip is IDENTICAL on both tabs**, driven twice and compared. It exists to
//!    remove a duplicated address — two renderings of one fact that a reconnect between frames
//!    could separate — so a strip that differed per tab would have reintroduced it one level up.
//! 5. **⚠ The settings editor is HONEST about a write that cannot take effect.** When the origin
//!    is `env:VAR` the file write is inert until that variable is unset on the daemon's box, and
//!    the Save button says so in its own label. Nothing on the wire, in `apply_set_setting` or in
//!    this panel said so before; a pure test of `env_shadow` stays green with the renderer wired
//!    to nothing, which is why the button's LABEL is read off the real tree.
//! 6. **⚠ Property 2 applies to the CREDENTIALS segment too**, and it did not when the window
//!    shipped. The credential loader is infallible: a store that exists and cannot be opened
//!    returns an EMPTY map, so the chrome printed a measured `Credentials 0` / `0 set` about a file
//!    nothing read. Both surfaces now render no number and say what happened.
//! 7. **⚠ The INACTIVE half must describe something that is actually happening.** The Backend
//!    fetch was driven from inside the section body, which only the Backend tab draws, while its
//!    digest is drawn on both — so the default tab's status line said `reading…` for ever about a
//!    fetch nothing had requested. The driver moved to the tool; the digest learned to say `not
//!    read yet`; both are pinned here.
//! 8. **⚠ The segments are IN the window's title bar, and there are TWO of them.** Every harness
//!    below drives the REAL `vike_app_core::workspace::tool_title_bar` and paints through the real
//!    `title_bar_tabs`, so "which row is this drawn in" is a property the suite can see: the chips
//!    land in the rect the bar reserved, the window's own name is the bar's and never a third
//!    chip, and the title DROPS below the breakpoint so two chips still fit at ~400pt.
//! 9. **⚠ The ambient strip survives a body that eats every pixel it is offered.** It did not:
//!    `vike_connections::connections_ui` allocates `ui.available_height()` outright, so the strip
//!    was appended below the window's own bottom and reached no pixel on the captured frame.
//!    Every earlier test of the strip drove `ambient_strip` DIRECTLY, which is why a strip nothing
//!    could see stayed green — [`the_foot_strip_survives_a_body_that_eats_every_pixel`] drives the
//!    composition instead.
//! 10. **⚠⚠ THE TREE CANNOT SEE PIXELS, AND THREE OF THIS WINDOW'S DEFECTS LIVED EXACTLY THERE.**
//!     An accessibility node is reported whether or not a pixel of it reached the screen, and a
//!     PAINTED line (the hairline) registers no node at all — so a suite phrased entirely over the
//!     tree is blind to clipping, to scissoring, and to anything drawn rather than laid out. Three
//!     findings in this file were that shape: the hairline clipped to the bar rendered NOTHING (its
//!     clip's scissor bound is exclusive at exactly the stroke's own row), the chips at ~400pt were
//!     claimed off the tree while their `Ui`'s clip rect IS the slot, and the foot strip overflowed
//!     a row a third too short for it. So the gates here read GEOMETRY —
//!     `title_bar_tabs`/`connections_body` return the rects they drew — and, for the hairline, the
//!     CLIP RECTS off `Harness::output().shapes` run through `egui-wgpu`'s own scissor rounding.
//!     Prefer that shape to a tree lookup whenever the claim is about what somebody can SEE.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use vike_app_core::backend_conn::cli_observe_record;
use vike_app_core::backend_editor::EditorState;
use vike_app_core::backend_registry::{BackendRecord, BackendsFile};
use vike_app_core::tool_views::{
    BackendDigest, BackendPicker, BackendSettingsState, BodyLayout, ConnectionsTab,
    SettingsEditState, SettingsFilter, SettingsWriteRequest, TabRow, ambient_strip,
    backend_settings_section, connections_body, credentials_line, hairline_segments, status_line,
    title_bar_tabs,
};
use vike_app_core::workspace::WinKind;
use vike_app_core::workspace::title_bar::{TitleTabSlot, tool_title_bar};
use vike_connections::CredentialSummary;
use vike_model::account_keys::AccountLabel;
use vike_tradehub_client::wire::{WireSettingsRow, WireSettingsShow};

// ------------------------------------------------------------------------------------------
// Tree helpers — the same shape the other headless suites in this tree use.
// ------------------------------------------------------------------------------------------

fn nodes<'t, 'h>(h: &'t Harness<'h, ()>, pred: impl Fn(&Node<'t>) -> bool) -> Vec<Node<'t>> {
    h.root().children_recursive().filter(|n| pred(n)).collect()
}

fn button_labels(h: &Harness<'_, ()>) -> Vec<String> {
    nodes(h, |n| n.accesskit_node().role() == Role::Button)
        .iter()
        .map(|n| n.accesskit_node().label().unwrap_or_default().to_string())
        .collect()
}

/// One node's text, whatever ROLE carries it: egui files a `Role::Label`'s text under `value` and
/// a `Role::Button`'s under `label`.
///
/// ⚠ Role-agnostic ON PURPOSE. The inline window title this redesign deleted was a `ui.label`, so
/// every assertion phrased over `button_labels` was blind to the exact thing it claimed to
/// forbid — see [`the_window_name_is_the_title_bars_and_never_a_third_segment`].
fn node_text(n: &Node<'_>) -> String {
    let a = n.accesskit_node();
    match (a.label(), a.value()) {
        (Some(l), _) if !l.is_empty() => l.to_string(),
        (_, Some(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// One node's rect in logical points, or `None` for a node accesskit gave no bounding box (the
/// tree's own root, among others — `Node::rect` PANICS on those).
fn node_rect(n: &Node<'_>) -> Option<egui::Rect> {
    n.accesskit_node().bounding_box().map(|_| n.rect())
}

/// Every DISTINCT non-empty node text whose rect's CENTRE falls inside `area` — "what is rendered
/// HERE", which is the question the accessibility tree can answer once it is asked with geometry.
///
/// ⚠ **Distinct, because one widget is not one node.** MEASURED: a chip row holding two segments
/// and one amber dot reports `["Credentials 15", "Credentials 15", "Backend 4", "●", "●"]` — egui
/// files a `Label` as a node carrying the text plus an enclosing one carrying it again, while a
/// `Button` appears once. Counting raw nodes would therefore make "how many segments are in this
/// row" depend on which of them is SELECTED, which is not a fact about the row.
fn texts_within(h: &Harness<'_, ()>, area: egui::Rect) -> Vec<String> {
    let mut out: Vec<String> = nodes(h, |_| true)
        .iter()
        .filter(|n| node_rect(n).is_some_and(|r| area.contains(r.center())))
        .map(node_text)
        .filter(|t| !t.trim().is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every node, anywhere in the tree, whose text is exactly `want` — with its rect.
fn text_rects(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    nodes(h, |_| true).iter().filter(|n| node_text(n) == want).filter_map(node_rect).collect()
}

/// **The HAIRLINE as the renderer received it** — every horizontal `Shape::LineSegment` this frame
/// painted at `y`, each with the CLIP RECT it was painted under.
///
/// ⚠ This reads `Harness::output().shapes`, not the accessibility tree, and it has to: a painted
/// line registers no node, so the tree cannot tell a drawn hairline from one that was scissored
/// away — which is exactly how a hairline that rendered NOTHING passed review.
fn hairlines_at(h: &Harness<'_, ()>, y: f32) -> Vec<(egui::Rect, f32, f32)> {
    h.output()
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::LineSegment { points: [a, b], .. }
                if (a.y - b.y).abs() < 0.01 && (a.y - y).abs() < 0.01 =>
            {
                Some((c.clip_rect, a.x.min(b.x), a.x.max(b.x)))
            }
            _ => None,
        })
        .collect()
}

/// `egui-wgpu-0.36.1/src/renderer.rs`'s `ScissorRect::new`, restated as the physical row range a
/// render pass actually touches: each edge rounded, the range `[min, max)`.
///
/// ⚠ The EXCLUSIVE upper bound is the whole of the hairline defect, and it is invisible from egui:
/// a shape whose clip rect ends exactly at the shape's own top edge is emitted, tessellated, and
/// then scissored away by the GPU with nothing anywhere reporting it.
/// `crates/vike-app-core/src/workspace/title_bar.rs`'s
/// `the_hairline_row_is_scissored_away_by_the_bar_and_survives_its_own_clip` drives the same model
/// over the pure geometry; this copy drives it over the clip rect production actually emitted.
fn scissor_rows(clip: egui::Rect, ppp: f32) -> std::ops::Range<i64> {
    let min = (ppp * clip.min.y).round() as i64;
    let max = ((ppp * clip.max.y).round() as i64).max(min);
    min..max
}

/// The physical row a 1pt stroke centred on `y` lands in.
fn stroke_row(y: f32, ppp: f32) -> i64 {
    (ppp * y).floor() as i64
}

/// Everything the tree says, one node per line — label and value both, because egui files a
/// `Role::Label`'s text under `value` rather than `label`.
fn tree_text(h: &Harness<'_, ()>) -> String {
    h.root()
        .children_recursive()
        .map(|n| {
            let a = n.accesskit_node();
            let mut s = String::new();
            if let Some(l) = a.label() {
                s.push_str(&l);
                s.push(' ');
            }
            if let Some(v) = a.value() {
                s.push_str(&v);
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// ⚠ **A prerequisite for driving the real title bar, not a workaround.**
///
/// `vike_ui_theme::font::extralight` only NAMES a `FontFamily`; registering it is the BINARY's job
/// (`vike_app_core::fonts`, called from the shell's `install_fonts`). A bare `egui::Context` has no
/// such family and epaint PANICS — `FontFamily::Name("extralight") is not bound to any fonts` — the
/// first time one is laid out. `crates/vike-chart/examples/export_png.rs`'s
/// `bind_chart_font_families` is the identical prerequisite one crate over.
///
/// `Context::set_fonts` takes effect on the NEXT pass, so this binds on the first frame and
/// answers `false`; the caller draws nothing that frame. Every harness below is built and then
/// `run()`, which steps again because the bind requests a repaint — so the frame the assertions
/// read is always a bound one.
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

fn click(h: &mut Harness<'_, ()>, want: &str) {
    {
        let wanted = want.to_string();
        let found = nodes(h, move |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some(wanted.as_str())
        });
        assert_eq!(found.len(), 1, "exactly one button labelled {want:?} must be on screen");
        found[0].click();
    }
    h.run();
}

// ------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------

fn summary() -> CredentialSummary {
    CredentialSummary {
        venues: 14,
        configured: Some(15),
        configurable: 34,
        health: vike_connections::StoreHealth::Readable,
    }
}

/// The same grid, counted from a store that would not OPEN — an empty map with a fault behind it,
/// which the loader cannot distinguish from an unconfigured box and this tool therefore must.
fn unreadable_summary() -> CredentialSummary {
    CredentialSummary {
        venues: 14,
        configured: None,
        configurable: 34,
        health: vike_connections::StoreHealth::Unreadable(
            "credential store /srv/vike/settings/secrets.env could not be read: permission denied"
                .to_string(),
        ),
    }
}

fn row(key: &str, section: &str, origin: &str, read_by: &str) -> WireSettingsRow {
    WireSettingsRow {
        section: section.into(),
        key: key.into(),
        value: "127.0.0.1:7879".into(),
        origin: origin.into(),
        read_by: read_by.into(),
    }
}

/// A node answer spanning every ORIGIN kind and every READ value, plus one amber finding.
fn show() -> WireSettingsShow {
    WireSettingsShow {
        settings_dir: Some("/srv/vike-<unit>/settings".into()),
        rows: vec![
            row("config.tradehub_addr", "config.toml", "config.toml", "tradehub"),
            row("config.node_addr", "config.toml", "env:VIKE_NODE_ADDR", "cli"),
            row("config.log_dir", "config.toml", "config.toml", "NO"),
            row("preferences.chart_style", "preferences.toml", "default", "yes"),
            row("policy.max_notional_per_order", "policy.toml", "policy.toml", "tradehub"),
        ],
    }
}

fn registry() -> BackendsFile {
    BackendsFile::default()
}

fn record() -> BackendRecord {
    cli_observe_record("127.0.0.1:7879")
}

// ------------------------------------------------------------------------------------------
// 1 + 2 — the segmented control
// ------------------------------------------------------------------------------------------

/// A harness over the real title bar and the real [`title_bar_tabs`], carrying the tab out through
/// a cell the closure owns.
///
/// ⚠ It drives the PRODUCTION [`tool_title_bar`] rather than modelling a strip of the right
/// height, and that is the point of the rewrite: the chips are painted into the rect that function
/// reserves, so "the segments are in the title bar" and "the title drops at 400pt" are properties
/// this suite can actually see. A harness that allocated its own bar would have re-asserted the
/// harness.
fn tab_harness(start: ConnectionsTab, digest: BackendDigest, width: f32) -> Harness<'static, ()> {
    tab_frame(start, digest, width, summary()).h
}

/// The same, over a caller-supplied credential summary — the door the unreadable-store test uses.
fn tab_harness_cred(
    start: ConnectionsTab,
    digest: BackendDigest,
    width: f32,
    cred: CredentialSummary,
) -> Harness<'static, ()> {
    tab_frame(start, digest, width, cred).h
}

/// **One rendered frame of the real title bar, plus the two facts the accessibility tree cannot
/// carry.**
///
/// ⚠ `slot` and `row` are RETURNED rather than re-derived, and that is the correction this suite
/// needed twice over. The chips' detached `Ui` takes `slot.tabs` as its CLIP RECT
/// (`egui-0.36.1/src/ui.rs`'s `Ui::new` sets `clip_rect = max_rect`), so a chip wider than the slot
/// is invisible on screen and fully present in the tree — a test that reads a label back has
/// learned nothing about whether anybody can see it. Same for the hairline the chips break: it is
/// painted, so it reaches no node at all.
struct TabFrame {
    h: Harness<'static, ()>,
    /// The slot `tool_title_bar` reserved on the frame the assertions read.
    slot: TitleTabSlot,
    /// What `title_bar_tabs` drew into it — every chip's rect, and the selected one.
    row: TabRow,
}

fn tab_frame(
    start: ConnectionsTab,
    digest: BackendDigest,
    width: f32,
    cred: CredentialSummary,
) -> TabFrame {
    let cell = std::rc::Rc::new(std::cell::Cell::new(start));
    let seen: std::rc::Rc<std::cell::RefCell<Option<(TitleTabSlot, TabRow)>>> = Default::default();
    let inner = seen.clone();
    let mut h = Harness::builder().with_size(egui::vec2(width, 300.0)).build_ui(move |ui| {
        if !fonts_ready(ui.ctx()) {
            return;
        }
        let (_acts, _drag, slot) = tool_title_bar(ui, WinKind::Connections, false);
        let mut tab = cell.get();
        let row = title_bar_tabs(ui, &slot, &mut tab, &cred, &digest);
        cell.set(tab);
        *inner.borrow_mut() = Some((slot, row));
        status_line(ui, tab, &AccountLabel::Default, &cred, &digest);
    });
    h.run();
    let (slot, row) = seen.borrow().clone().expect("the bar always reserves a slot");
    TabFrame { h, slot, row }
}

/// The slot the shipped bar reserves at this width, off a REAL rendered frame — the same frame the
/// chips were drawn into, so a geometry assertion and a tree assertion cannot be describing two
/// different layouts.
fn slot_at(width: f32) -> TitleTabSlot {
    tab_frame(ConnectionsTab::Credentials, BackendDigest::NotFetched, width, summary()).slot
}

/// ⚠ The SELECTED segment is a `Label`, the other a `Button` — so "which tab am I on" is
/// answerable by ROLE, and the current tab cannot be re-picked. Reddens on a pair of
/// `SelectableLabel`s, which is the obvious spelling and the unreadable one.
#[test]
fn the_selected_tab_is_a_label_and_the_other_is_the_only_button() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let mut h = tab_harness(ConnectionsTab::Credentials, loaded.clone(), 1000.0);
    h.run();

    let buttons = button_labels(&h);
    assert!(
        buttons.iter().any(|b| b.starts_with("Backend")),
        "the INACTIVE tab is the clickable one: {buttons:?}"
    );
    assert!(
        !buttons.iter().any(|b| b.starts_with("Credentials")),
        "the ACTIVE tab must not be a button: {buttons:?}"
    );

    // The other way round.
    let mut h = tab_harness(ConnectionsTab::Backend, loaded, 1000.0);
    h.run();
    let buttons = button_labels(&h);
    assert!(buttons.iter().any(|b| b.starts_with("Credentials")), "{buttons:?}");
    assert!(!buttons.iter().any(|b| b.starts_with("Backend")), "{buttons:?}");
}

/// Both segments carry their live count, and the Backend one carries the amber dot when the node
/// reports a key that is set and read by nothing.
#[test]
fn each_segment_carries_its_count_and_the_backend_one_its_amber_dot() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    assert!(loaded.has_finding(), "the fixture must contain one, or this test proves nothing");
    let mut h = tab_harness(ConnectionsTab::Credentials, loaded, 1000.0);
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("Credentials 15"), "the configured-cell count is on the chip: {text}");
    // FOUR of the five fixture rows are named by a layer (three files + one env); the fifth
    // stands at its compiled-in default.
    assert!(text.contains("Backend 4"), "the set-row count is on the chip: {text}");
}

/// ⚠ **NO NUMBER is rendered where none is known**, in every one of the four non-`Loaded` states.
/// Reddens on a `0` standing in for "the node has not answered" — the mockup's own notes record an
/// earlier draft inventing "14 of 68 loaded", and this is the test that stops its return.
#[test]
fn the_backend_segment_renders_no_number_until_the_node_answers() {
    for (name, digest) in [
        ("no backend", BackendDigest::of(false, &BackendSettingsState::Idle)),
        ("idle", BackendDigest::of(true, &BackendSettingsState::Idle)),
        ("pending", BackendDigest::of(true, &BackendSettingsState::Pending)),
        ("unsupported", BackendDigest::of(true, &BackendSettingsState::Unsupported)),
        ("error", BackendDigest::of(true, &BackendSettingsState::Error("bad mac".into()))),
    ] {
        let mut h = tab_harness(ConnectionsTab::Credentials, digest, 1000.0);
        h.run();
        let buttons = button_labels(&h);
        assert!(
            buttons.iter().any(|b| b == "Backend"),
            "{name}: the segment must read `Backend` with no number: {buttons:?}"
        );
        assert!(
            !buttons.iter().any(|b| b.starts_with("Backend 0")),
            "{name}: a zero is a number this tool does not have: {buttons:?}"
        );
    }
}

/// Clicking the inactive segment switches the tab — the real click, on the real button.
#[test]
fn clicking_the_inactive_segment_switches_the_tab() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let mut h = tab_harness(ConnectionsTab::Credentials, loaded, 1000.0);
    h.run();
    click(&mut h, "Backend 4");
    let buttons = button_labels(&h);
    assert!(
        buttons.iter().any(|b| b.starts_with("Credentials")),
        "after the click Credentials is the INACTIVE (clickable) one: {buttons:?}"
    );
}

/// The window's title drops below the breakpoint; the segments survive at ~400pt.
///
/// ⚠ The title is now the TITLE BAR's — `tool_title_bar` draws it, and drops it for a kind that
/// seats tabs there. So this test reads it off the real bar rather than off an inline label the
/// tool used to draw, which is the whole of divergence 1: that label sat in the segments' row and
/// a judge counted it as a third tab.
///
/// ⚠ **"The segments survive" is asserted as GEOMETRY here, not as a tree lookup, and the earlier
/// spelling of this test could not have caught the failure it claimed to.** It read the two chip
/// labels back out of the accessibility tree — which reports a node whether or not a pixel of it
/// reached the screen. The chips are drawn through a DETACHED `Ui` whose clip rect IS `slot.tabs`
/// (`egui-0.36.1/src/ui.rs`'s `Ui::new`: `clip_rect = max_rect`), so a row wider than the slot is
/// silently cut off and the tree says nothing changed. The narrow width is the one that can fail
/// that way — it is also the width a tool window OPENS at (`crate::window_spawn`'s
/// `vec2(560.0, 400.0)`), so it is the ordinary case rather than an edge — and
/// [`the_chips_fit_the_slot_the_bar_reserved_at_the_narrow_width`] is where the fit is argued in
/// full.
#[test]
fn the_window_title_drops_below_the_breakpoint_and_the_segments_do_not() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let wide = tab_frame(ConnectionsTab::Credentials, loaded.clone(), 1000.0, summary());
    assert!(tree_text(&wide.h).contains("Connections"), "the title is on a wide window");

    let narrow = tab_frame(ConnectionsTab::Credentials, loaded, 400.0, summary());
    let text = tree_text(&narrow.h);
    assert!(!text.contains("Connections"), "the title drops at 400pt: {text}");
    assert!(text.contains("Credentials 15"), "…and both segments survive: {text}");
    assert!(text.contains("Backend 4"), "{text}");
    // …and "survive" means DRAWN, not merely present in the tree.
    let bounds = narrow.row.bounds().expect("two chips were drawn");
    assert!(
        narrow.slot.tabs.contains_rect(bounds.shrink(0.01)),
        "at 400pt the chips must FIT the slot the bar reserved, or the clip cuts them off with \
         the tree none the wiser: chips {bounds:?} vs slot {:?}",
        narrow.slot.tabs
    );
}

/// ⚠⚠ **THERE ARE EXACTLY TWO SEGMENTS, AND THE WINDOW'S NAME IS NOT ONE OF THEM.**
///
/// The captured frame read `Connections | Credentials 0 | Backend` and was judged as three tabs.
/// There was no third tab: the first item was an inline title label the TOOL drew, in the
/// segments' own row, at a size and colour that made it look like a chip. The window's name is the
/// title bar's job and always was, so the cure was to move the chips INTO the bar and delete the
/// label.
///
/// ⚠ **The first version of this test could not fail for that reason**, and the miss is worth
/// recording because it is the exact shape this tree keeps finding. It asserted over
/// `button_labels` — nodes of `Role::Button` — while the thing it forbade was a `ui.label`, a
/// `Role::Label`. Re-introducing the deleted line verbatim would have left it green. So the gate
/// is now ROLE-AGNOSTIC and GEOMETRIC: it asks what is rendered inside the chip slot, whatever
/// role carries it, and separately that the name IS rendered once, by the BAR, to the LEFT of that
/// slot.
///
/// Reddens on: a name-shaped item re-added to the chip row as a label OR a button; a second
/// rendering of the window's name anywhere in the frame; the name drifting into the slot.
#[test]
fn the_window_name_is_the_title_bars_and_never_a_third_segment() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let f = tab_frame(ConnectionsTab::Credentials, loaded, 1000.0, summary());
    let name = WinKind::Connections.label();

    // 1. Nothing in the chip slot is the window's name — whatever role draws it.
    let in_slot = texts_within(&f.h, f.slot.tabs);
    assert!(
        !in_slot.iter().any(|t| t.contains(name)),
        "the window's NAME is never a segment, and a `ui.label` is as much of one as a `Button`: \
         {in_slot:?}"
    );

    // 2. …and what IS in the slot is the two chips and nothing else carrying text. (The amber dot
    //    rides a chip and is allowed; it is not a segment.)
    let chips: Vec<&String> = in_slot
        .iter()
        .filter(|t| t.starts_with("Credentials") || t.starts_with("Backend"))
        .collect();
    assert_eq!(chips.len(), 2, "exactly two segments: {in_slot:?}");
    let strays: Vec<&String> = in_slot
        .iter()
        .filter(|t| !t.starts_with("Credentials") && !t.starts_with("Backend") && *t != "\u{25CF}")
        .collect();
    assert!(strays.is_empty(), "nothing else renders text in the chip slot: {strays:?}");

    // 3. The name IS rendered — and EVERY rendering of it is the BAR's: inside the bar, left of
    //    the slot. Phrased over every occurrence rather than as a count, because one `ui.label` is
    //    more than one accesskit node (see [`texts_within`]) — and it is the stronger claim
    //    anyway: a second rendering ANYWHERE, in the chip row or in the body below, fails it.
    let name_rects = text_rects(&f.h, name);
    assert!(!name_rects.is_empty(), "the window's name is drawn at this width");
    for title in &name_rects {
        assert!(
            f.slot.bar.contains_rect(*title),
            "every rendering of the window's name is the TITLE BAR's: {title:?} in {:?}",
            f.slot.bar
        );
        assert!(
            title.right() <= f.slot.tabs.left() + 0.01,
            "…and to the LEFT of the chips, never among them: {title:?} vs {:?}",
            f.slot.tabs
        );
    }
}

/// ⚠ **THE CHIPS ARE PAINTED INSIDE THE TITLE BAR, NOT IN A ROW UNDER IT.** That row is the whole
/// of divergence 2 — the space the design recovers was still being spent, and the chip had no
/// hairline to break. Read as geometry rather than as a picture: the slot the shipped bar reserves
/// is inside the bar, and its bottom IS the hairline the chip breaks.
#[test]
fn the_tab_slot_lives_inside_the_title_bar_and_ends_on_its_hairline() {
    for w in [400.0_f32, 1000.0] {
        let slot = slot_at(w);
        assert!(slot.carries_tabs, "width {w}: the Connections kind seats tabs in its bar");
        assert!(
            slot.bar.contains_rect(slot.tabs),
            "width {w}: the chips are INSIDE the bar, not below it: {slot:?}"
        );
        assert_eq!(slot.tabs.bottom(), slot.bar.bottom(), "width {w}: seated on the hairline");
        assert!(
            (slot.hairline_y() - slot.bar.bottom()).abs() <= 1.0,
            "width {w}: the hairline is the bar's own bottom edge: {slot:?}"
        );
        assert!(slot.tabs.width() > 0.0, "width {w}: there is room for the chips: {slot:?}");
    }
}

/// ⚠⚠ **THE CHIPS FIT THE SLOT AT ~400pt — THE WIDTH A TOOL WINDOW OPENS AT.**
///
/// The narrow geometry was CLAIMED by reading two labels back out of the accessibility tree, and
/// that tree cannot see clipping: the chips are drawn through the detached `Ui`
/// [`title_bar_tabs`] builds, whose clip rect is `slot.tabs` itself, so a row one point too wide
/// is cut off on screen and reported complete in the tree. So the narrow width had NO coverage at
/// all, which is the state this test ends.
///
/// Asserted on rects, in both directions: every chip lands inside the slot the bar reserved, and
/// the selected chip's BOTTOM edge is the bar's own bottom — the edge the hairline breaks at, and
/// the one a `Layout` cross-align of `Center` (the spelling `ui.horizontal` would have forced)
/// silently lifts off it.
#[test]
fn the_chips_fit_the_slot_the_bar_reserved_at_the_narrow_width() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    for w in [400.0_f32, 560.0, 1000.0] {
        for start in [ConnectionsTab::Credentials, ConnectionsTab::Backend] {
            let f = tab_frame(start, loaded.clone(), w, summary());
            assert_eq!(f.row.chips.len(), 2, "width {w}: two chips were drawn: {:?}", f.row);
            for chip in &f.row.chips {
                assert!(
                    f.slot.tabs.contains_rect(chip.shrink(0.01)),
                    "width {w}: chip {chip:?} is outside the reserved slot {:?} — the detached \
                     Ui's clip rect IS that slot, so the overflow is invisible on screen and \
                     fully present in the accessibility tree",
                    f.slot.tabs
                );
            }
            let selected = f.row.selected.expect("one segment is always selected");
            assert!(
                (selected.bottom() - f.slot.bar.bottom()).abs() <= 0.5,
                "width {w}: the selected chip is SEATED on the bar's bottom edge — that edge is \
                 the hairline it breaks: chip {selected:?} vs bar {:?}",
                f.slot.bar
            );
        }
    }
}

/// ⚠⚠ **THE SELECTED CHIP BREAKS THE HAIRLINE — the only thing that marks it as selected — AND
/// THE HAIRLINE HAS TO REACH A PIXEL.** Its fill is `palette::BG`, which is also what the title
/// bar stands on, and it draws only three edges; the gap in the line under the bar IS its fourth.
/// So a hairline that renders nothing does not cost a divider: it costs the selection marker, and
/// the frame reads as two inert words.
///
/// Three layers, because the first version of this test had only the first and the shipped code
/// failed the second:
///
/// 1. the pure arithmetic of the break ([`hairline_segments`]);
/// 2. **the clip rect production actually emitted**, read off `Harness::output().shapes` and run
///    through `egui-wgpu`'s own scissor rounding ([`scissor_rows`]) — the line was painted under
///    `slot.bar`, whose scissor bound is EXCLUSIVE at exactly the line's own row, so every stroke
///    was thrown away by the GPU with nothing reporting it;
/// 3. the spans actually painted, against what the chip's rect says they should be.
#[test]
fn the_selected_chip_breaks_the_hairline_and_nothing_else_does() {
    // 1 — the pure break. Nothing selected (not a production state, but the fallback the renderer
    // keeps): one line.
    assert_eq!(hairline_segments(0.0, 100.0, None), vec![(0.0, 100.0)]);

    // An interior chip: two runs, and the gap between them is EXACTLY the chip.
    let segs = hairline_segments(0.0, 100.0, Some((30.0, 55.0)));
    assert_eq!(segs, vec![(0.0, 30.0), (55.0, 100.0)], "the gap is the chip's own span");
    let painted: f32 = segs.iter().map(|(a, b)| b - a).sum();
    assert!((painted - 75.0).abs() < f32::EPSILON, "only the chip's width is unpainted: {segs:?}");

    // Flush against either end: one run, no zero-width ghost segment.
    assert_eq!(hairline_segments(0.0, 100.0, Some((0.0, 40.0))), vec![(40.0, 100.0)]);
    assert_eq!(hairline_segments(0.0, 100.0, Some((60.0, 100.0))), vec![(0.0, 60.0)]);

    // 2 + 3 — the real frame.
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let f = tab_frame(ConnectionsTab::Credentials, loaded, 1000.0, summary());
    let chip = f.row.selected.expect("the SELECTED chip must report a rect to break the line with");
    assert!(chip.width() > 0.0, "a real chip: {chip:?}");
    assert!(
        f.slot.tabs.contains_rect(chip.shrink(0.01)),
        "…inside the slot the bar reserved: {chip:?} vs {:?}",
        f.slot.tabs
    );

    let y = f.slot.hairline_y();
    let painted = hairlines_at(&f.h, y);
    assert_eq!(painted.len(), 2, "the hairline is TWO runs, broken by the chip: {painted:?}");

    // 2 — every run must actually survive the scissor, at every scale a display hands egui. The
    // model is `ScissorRect::new`'s own rounding; the clip rect is the one production emitted.
    for (clip, a, b) in &painted {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0] {
            let row = stroke_row(y, ppp);
            assert!(
                scissor_rows(*clip, ppp).contains(&row),
                "the hairline run {a}..{b} is SCISSORED AWAY at {ppp}x: its row {row} is outside \
                 the pass's {:?} for the clip {clip:?} this frame emitted. The line is the \
                 selected chip's only marker, so this renders a control with no selection at all.",
                scissor_rows(*clip, ppp)
            );
        }
        assert!(
            clip.min.x <= f.slot.bar.left() + 0.01 && clip.max.x >= f.slot.bar.right() - 0.01,
            "…and the clip still spans the whole bar horizontally: {clip:?} vs {:?}",
            f.slot.bar
        );
    }

    // 3 — and the gap is the chip's own footprint, not somewhere else.
    let mut runs: Vec<(f32, f32)> = painted.iter().map(|(_, a, b)| (*a, *b)).collect();
    runs.sort_by(|p, q| p.0.total_cmp(&q.0));
    assert_eq!(
        runs,
        hairline_segments(f.slot.bar.left(), f.slot.bar.right(), Some((chip.left(), chip.right()))),
        "the painted runs are exactly the ones the break arithmetic asks for, around chip {chip:?}"
    );
}

// ------------------------------------------------------------------------------------------
// 3 — the one status line
// ------------------------------------------------------------------------------------------

/// ⚠ The ACTIVE tab is spelled out and the INACTIVE one digested, and SWITCHING SWAPS THEM — so
/// neither tab's state is ever hidden behind the other. Reddens on a status line that follows only
/// one tab, which is the shape that made the old window's backend state invisible while the
/// credential grid was on screen.
#[test]
fn the_status_line_carries_both_tabs_and_swaps_which_is_spelled_out() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));

    let mut h = tab_harness(ConnectionsTab::Credentials, loaded.clone(), 1000.0);
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("this machine · 14 venues"), "credentials spelled out: {text}");
    assert!(
        text.contains("Backend settings  5 keys · 4 set"),
        "…and the backend half DIGESTED, labelled with the tab it belongs to: {text}"
    );

    let mut h = tab_harness(ConnectionsTab::Backend, loaded, 1000.0);
    h.run();
    let text = tree_text(&h);
    assert!(
        text.contains("5 keys · 4 set") && !text.contains("Backend settings  5 keys"),
        "the backend half is now spelled out and unlabelled: {text}"
    );
    assert!(
        text.contains("Credentials  this machine · 14 venues"),
        "…and the credentials half is DIGESTED and labelled: {text}"
    );
}

/// The amber finding is NAMED in the status line, not merely counted — one key's name is more use
/// than a number, and the table's filter is how the rest are found.
#[test]
fn the_status_line_names_the_key_that_is_set_and_read_by_nothing() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let mut h = tab_harness(ConnectionsTab::Backend, loaded, 1000.0);
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("⚠ 1 read by nothing: config.log_dir"), "{text}");
}

/// The digested credentials line names the account when one is selected — a count with no subject
/// would describe the wrong store as soon as an operator switched account.
#[test]
fn the_credentials_line_names_a_labelled_account() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let line = credentials_line(&alt, &summary(), true);
    assert!(line.contains("this machine"), "{line}");
    assert!(line.contains("account ALT"), "{line}");
}

// ------------------------------------------------------------------------------------------
// 4 — the ambient strip
// ------------------------------------------------------------------------------------------

struct Strip {
    harness: Harness<'static, ()>,
    state: std::rc::Rc<std::cell::RefCell<(ConnectionsTab, EditorState)>>,
}

fn strip_harness(tab: ConnectionsTab, active: bool) -> Strip {
    let file = registry();
    let rec = record();
    let shared: std::rc::Rc<std::cell::RefCell<(ConnectionsTab, EditorState)>> =
        std::rc::Rc::new(std::cell::RefCell::new((tab, EditorState::Closed)));
    let inner = shared.clone();
    let harness = Harness::builder().with_size(egui::vec2(700.0, 200.0)).build_ui(move |ui| {
        let picker = BackendPicker {
            backends: &file,
            active: if active { Some(&rec) } else { None },
            available: true,
        };
        let mut action = None;
        let mut state = inner.borrow_mut();
        let (tab, editor) = &mut *state;
        ambient_strip(ui, &picker, &mut action, tab, editor);
    });
    Strip { harness, state: shared }
}

/// ⚠ **The strip is IDENTICAL on both tabs.** It exists to remove a duplicated address — the
/// Backend settings header used to restate the active backend beside a picker row that a
/// reconnect could have moved — so a strip that rendered differently per tab would have
/// reintroduced the same class of disagreement one level up.
#[test]
fn the_ambient_strip_is_byte_identical_on_both_tabs() {
    let mut creds = strip_harness(ConnectionsTab::Credentials, true);
    creds.harness.run();
    let mut backend = strip_harness(ConnectionsTab::Backend, true);
    backend.harness.run();
    assert_eq!(
        tree_text(&creds.harness),
        tree_text(&backend.harness),
        "the strip is process-level state and belongs to neither tab"
    );
}

/// ⚠ **Add backend takes the operator to where the form lands.** The add/edit form is the Backend
/// tab's — an expanding editor inside a status strip is what a status strip is not — so the click
/// must switch the tab as well as opening the form. Reddens on a click that opens a form on a tab
/// the operator is not looking at, which is indistinguishable from a click that did nothing.
#[test]
fn add_backend_from_the_strip_switches_to_the_tab_that_renders_the_form() {
    let mut s = strip_harness(ConnectionsTab::Credentials, true);
    s.harness.run();
    click(&mut s.harness, "Add backend");
    let state = s.state.borrow();
    assert_eq!(state.0, ConnectionsTab::Backend, "the click switches tabs");
    assert!(matches!(state.1, EditorState::Add { .. }), "…and opens the add form: {:?}", state.1);
}

/// The strip names the live connection the way the mockup does: the synthetic `--observe` record,
/// its address, its control state and the fact that the registry does not list it.
#[test]
fn the_strip_names_the_live_connection_its_address_and_its_control_state() {
    let mut s = strip_harness(ConnectionsTab::Credentials, true);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("(--observe)"), "{text}");
    assert!(text.contains("127.0.0.1:7879"), "{text}");
    // `cli_observe_record` arms control (the process-level `VIKE_TRADEHUB_CONTROL` gate is the
    // only one on the CLI path), so this is the mockup's own example strip verbatim.
    assert!(text.contains("control armed"), "{text}");
    assert!(text.contains("(not in registry)"), "the synthetic record is not a file entry: {text}");
    let buttons = button_labels(&s.harness);
    assert!(buttons.iter().any(|b| b == "Disconnect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == "Add backend"), "{buttons:?}");
}

/// One frame of the REAL [`connections_body`] over a greedy body, at a given window width and
/// starting tab: the tree it rendered, the layout it reported, and the floor the body was offered.
///
/// The body is `allocate_space(available)` — the shape `vike_connections::connections_ui`'s
/// two-column arm has, since its rail takes `egui::vec2(RAIL_W, ui.available_height())` outright.
/// That is what makes this a test of the COMPOSITION rather than of a piece: every earlier test of
/// the strip drove [`ambient_strip`] directly, which is why a strip nobody could see stayed green.
fn body_frame(tab: ConnectionsTab, width: f32, height: f32) -> (String, BodyLayout, f32) {
    let file = registry();
    let rec = record();
    let cred = summary();
    let digest = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let seen: std::rc::Rc<std::cell::Cell<Option<(BodyLayout, f32)>>> = Default::default();
    let inner = seen.clone();
    let mut h = Harness::builder().with_size(egui::vec2(width, height)).build_ui(move |ui| {
        if !fonts_ready(ui.ctx()) {
            return;
        }
        let (_a, _d, slot) = tool_title_bar(ui, WinKind::Connections, false);
        let picker = BackendPicker { backends: &file, active: Some(&rec), available: true };
        let mut action = None;
        let mut editor = EditorState::Closed;
        let mut tab = tab;
        let top = ui.cursor().top();
        let body_floor: std::rc::Rc<std::cell::Cell<f32>> = Default::default();
        let floor = body_floor.clone();
        let layout = connections_body(
            ui,
            &slot,
            &mut tab,
            &AccountLabel::Default,
            &cred,
            &digest,
            &picker,
            &mut action,
            &mut editor,
            |ui, _tab, _action, _editor| {
                floor.set(ui.max_rect().bottom());
                // ⚠ And the body must still start BELOW the title bar. The first cut of the
                // reservation moved the cursor UP over it (`Ui::set_max_height` snaps the cursor
                // to `max_rect.min.y`, which it has just unioned back to the `Ui`'s own top),
                // which no floor comparison would have caught.
                assert!(
                    ui.cursor().top() >= top,
                    "the body starts below the title bar, not over it ({} vs {top})",
                    ui.cursor().top()
                );
                ui.allocate_space(egui::vec2(ui.available_width(), ui.available_height()));
            },
        );
        inner.set(Some((layout, body_floor.get())));
    });
    h.run();
    let (layout, body_floor) = seen.get().expect("the body renders every frame");
    (tree_text(&h), layout, body_floor)
}

/// ⚠⚠ **THE STRIP SURVIVES A BODY THAT EATS EVERY PIXEL IT IS OFFERED — on BOTH tabs.**
///
/// This is the one the shipped window failed, and it failed invisibly because every other test of
/// the strip drove [`ambient_strip`] DIRECTLY. `vike_connections::connections_ui`'s two-column arm
/// allocates its rail `egui::vec2(RAIL_W, ui.available_height())` — all of it — so the shipped
/// pad-to-the-foot filler had nothing left to pad with and the strip was appended one strip-height
/// BELOW the window's own bottom. What was OBSERVED is the outcome: no strip on the captured
/// frame at all. (`crates/vike-app-core/src/tool_views/connections.rs`'s module doc carries the
/// rest of the account and marks which half of it is derived rather than read off the code.)
///
/// Reddens on the reservation being dropped, and on a body that is handed the whole height again.
#[test]
fn the_foot_strip_survives_a_body_that_eats_every_pixel() {
    let (creds_text, layout, body_floor) = body_frame(ConnectionsTab::Credentials, 900.0, 420.0);
    assert!(
        layout.window_floor - body_floor >= layout.reserved - 0.01,
        "the body's floor sits at least the RESERVED row above the window's — the row is \
         reserved, not merely padded to ({body_floor} vs {}, reserved {})",
        layout.window_floor,
        layout.reserved
    );
    for needle in ["(--observe)", "127.0.0.1:7879", "control armed"] {
        assert!(
            creds_text.contains(needle),
            "the strip reached the screen on the Credentials tab ({needle}): {creds_text}"
        );
    }

    // …and IDENTICALLY on the Backend tab, which is the property the strip exists for: one
    // rendering of the live connection, belonging to neither tab.
    let (backend_text, _, _) = body_frame(ConnectionsTab::Backend, 900.0, 420.0);
    let strip_of = |t: &str| -> Vec<String> {
        t.lines()
            .filter(|l| l.contains("--observe") || l.contains("127.0.0.1:7879"))
            .map(String::from)
            .collect()
    };
    assert_eq!(
        strip_of(&creds_text),
        strip_of(&backend_text),
        "the strip is process-level state and renders the same on both tabs"
    );
    assert!(!strip_of(&creds_text).is_empty(), "…and it is not empty on either: {creds_text}");
}

/// ⚠⚠ **THE DRAWN STRIP FITS THE ROW THAT WAS RESERVED FOR IT — the check whose absence let the
/// foot strip go missing a SECOND time.**
///
/// The first fix reserved a row and bounded the body, and the strip still crossed the window
/// floor: the reservation was a `const STRIP_H: f32 = 22.0` and nothing compared it against what
/// [`ambient_strip`] draws. That is a separator (6pt) plus one `item_spacing.y` plus a wrapped row
/// whose tallest child is a `Button` — floored at `spacing.interact_size.y`, which is 18pt in
/// egui's own style before a point of `button_padding` is added. The constant was short by about a
/// third, and the test that existed asserted the constant against ITSELF.
///
/// So this one measures. Both numbers come off the real frame: `reserved` is what
/// `strip_reservation` held back before the body drew anything, `strip` is where `ambient_strip`
/// actually landed. Reddens on a reservation that stops tracking the strip — a widget added to the
/// row, a bigger text size, a style with more `button_padding` — none of which any constant could
/// have followed.
///
/// ⚠ The NARROW case is the one no arithmetic could have predicted and is therefore the one worth
/// having: `horizontal_wrapped` puts the Disconnect/Add buttons on a second line, so the strip is a
/// whole row taller. That is what the measurement fed back through `egui::Context` memory is for,
/// and running the harness to convergence is what proves the loop closes rather than oscillating.
#[test]
fn the_foot_strip_fits_the_row_that_was_reserved_for_it() {
    for (w, h) in [(900.0_f32, 420.0_f32), (560.0, 400.0), (300.0, 380.0)] {
        for tab in [ConnectionsTab::Credentials, ConnectionsTab::Backend] {
            let (_text, layout, _floor) = body_frame(tab, w, h);
            assert!(
                layout.strip.height() > 0.0,
                "{w}x{h}: the strip was drawn at all: {:?}",
                layout.strip
            );
            assert!(
                layout.strip.height() <= layout.reserved + 0.01,
                "{w}x{h}: the DRAWN strip ({:.2}pt) must fit the row reserved for it \
                 ({:.2}pt) — this is the comparison a `const STRIP_H` could not make, and the \
                 overflow is what crosses the window floor: {:?}",
                layout.strip.height(),
                layout.reserved,
                layout.strip
            );
            assert!(
                layout.strip.bottom() <= layout.window_floor + 0.01,
                "{w}x{h}: …and the whole strip is INSIDE the window, which is what the fit above \
                 buys: strip {:?} vs floor {}",
                layout.strip,
                layout.window_floor
            );
            assert!(
                layout.reserved <= layout.strip.height() + 8.0,
                "{w}x{h}: the reservation is a MEASUREMENT of the strip, not a pad big enough to \
                 hide a mistake — reserved {:.2}pt for a {:.2}pt strip",
                layout.reserved,
                layout.strip.height()
            );
        }
    }
}

/// With nothing connected the strip says so and offers no Disconnect — never an empty address or
/// a dot that could be read as connected.
#[test]
fn a_disconnected_strip_says_so_and_offers_no_disconnect() {
    let mut s = strip_harness(ConnectionsTab::Credentials, false);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("not connected"), "{text}");
    let buttons = button_labels(&s.harness);
    assert!(!buttons.iter().any(|b| b == "Disconnect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == "Add backend"), "adding one is still offered: {buttons:?}");
}

// ------------------------------------------------------------------------------------------
// 5 — the Backend tab's settings table + its honest editor
// ------------------------------------------------------------------------------------------

struct Settings {
    harness: Harness<'static, ()>,
    state: std::rc::Rc<std::cell::RefCell<(SettingsEditState, SettingsFilter)>>,
}

fn settings_harness(state: BackendSettingsState, filter: SettingsFilter) -> Settings {
    settings_harness_armed(state, filter, true, 1000.0)
}

fn settings_harness_armed(
    state: BackendSettingsState,
    filter: SettingsFilter,
    control_armed: bool,
    width: f32,
) -> Settings {
    let shared: std::rc::Rc<std::cell::RefCell<(SettingsEditState, SettingsFilter)>> =
        std::rc::Rc::new(std::cell::RefCell::new((SettingsEditState::Idle, filter)));
    let inner = shared.clone();
    let harness = Harness::builder().with_size(egui::vec2(width, 800.0)).build_ui(move |ui| {
        let mut refresh = false;
        let mut write: Option<SettingsWriteRequest> = None;
        let mut s = inner.borrow_mut();
        let (edit, filt) = &mut *s;
        backend_settings_section(
            ui,
            Some("prod"),
            control_armed,
            &state,
            &mut refresh,
            edit,
            &mut write,
            filt,
        );
    });
    Settings { harness, state: shared }
}

/// Click the `edit` affordance on row `i` of the settings table, then settle a frame.
fn open_row(s: &mut Settings, i: usize) {
    {
        let edits = nodes(&s.harness, |n| {
            let a = n.accesskit_node();
            a.role() == Role::Button && a.label().as_deref() == Some("edit")
        });
        assert!(i < edits.len(), "row {i} has no edit affordance (found {})", edits.len());
        edits[i].click();
    }
    s.harness.run();
}

/// Every accessibility node's VALUE, one entry each — egui files a `Role::Label`'s text there, so
/// this is how a cell's exact content is asserted rather than searched for as a substring.
fn values(h: &Harness<'_, ()>) -> Vec<String> {
    h.root()
        .children_recursive()
        .filter_map(|n| n.accesskit_node().value().map(|v| v.to_string()))
        .collect()
}

/// The table's summary is ONE dense paragraph carrying every fact the mockup asks for, and the
/// numbers are folds over the rows the node actually sent.
#[test]
fn the_backend_summary_is_one_paragraph_of_measured_numbers() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("5 keys"), "{text}");
    assert!(text.contains("4 set from env or file"), "{text}");
    assert!(text.contains("1 at their compiled-in default"), "{text}");
    assert!(text.contains("⚠ set and read by nothing: config.log_dir"), "{text}");
    assert!(text.contains("restart-to-apply"), "{text}");
    assert!(text.contains("/srv/vike-<unit>/settings"), "the settings dir is named: {text}");
}

/// ORIGIN keeps the wire's own distinctions — `env:VAR`, a file name, and `default` — and READ
/// keeps all THREE of its values. Reddens on a panel that reduces either to a boolean.
#[test]
fn origin_and_read_reach_the_screen_with_every_value_the_wire_carries() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    let cells = values(&s.harness);
    // ORIGIN: three DIFFERENT answers, each rendered as itself. Collapsing `env:VAR` and a file
    // name into one "set" word is what would hide the env-outranks-file hazard entirely.
    for origin in ["env:VIKE_NODE_ADDR", "config.toml", "policy.toml", "default"] {
        assert!(
            cells.iter().any(|c| c == origin),
            "the ORIGIN cell {origin:?} must reach the screen verbatim: {cells:?}"
        );
    }
    // READ: all THREE values — a binary's short name, `yes` (a library reads it), and `NO`.
    for read in ["tradehub", "cli", "yes", "NO"] {
        assert!(
            cells.iter().any(|c| c == read),
            "the READ cell {read:?} must reach the screen verbatim: {cells:?}"
        );
    }
}

/// The `set · N` / `all · N` filter counts what it says and hides what it says.
#[test]
fn the_filter_counts_and_hides_the_rows_it_names() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::Set);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("set · 4"), "{text}");
    assert!(text.contains("all · 5"), "{text}");
    assert!(
        !text.contains("preferences.chart_style"),
        "the `set` filter hides a defaulted row: {text}"
    );

    click(&mut s.harness, "all · 5");
    let text = tree_text(&s.harness);
    assert!(text.contains("preferences.chart_style"), "`all` shows it again: {text}");
}

/// ⚠⚠ **THE POINT OF THE WHOLE THING.** A row the ENVIRONMENT sets gets a prominent amber warning
/// naming the variable, and the Save button relabels itself `Save to file anyway` — because the
/// write is accepted, answers `restart_required: true`, and changes nothing the daemon will read.
/// A pure test of `env_shadow` stays green with this renderer wired to nothing, which is exactly
/// why the button's LABEL is read off the real tree.
#[test]
fn an_env_shadowed_row_warns_and_its_save_button_says_anyway() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();

    // Every row carries an `edit` button; the env-shadowed one is the SECOND row.
    open_row(&mut s, 1);
    let text = tree_text(&s.harness);
    assert!(
        text.contains("the ENVIRONMENT sets this key"),
        "the hazard must be stated, not implied: {text}"
    );
    assert!(text.contains("VIKE_NODE_ADDR"), "the VARIABLE is named: {text}");
    assert!(
        text.contains("unset on the daemon's box and it restarts"),
        "…and so is the remedy: {text}"
    );
    let buttons = button_labels(&s.harness);
    assert!(
        buttons.iter().any(|b| b == "Save to file anyway"),
        "the button must be honest about what it does: {buttons:?}"
    );
    assert!(!buttons.iter().any(|b| b == "Save to file"), "{buttons:?}");
}

/// A row the FILE sets gets no such warning, and its button is a plain `Save to file` — so the
/// amber is a signal rather than decoration.
#[test]
fn a_file_set_row_gets_no_env_warning_and_a_plain_save_button() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    open_row(&mut s, 0);
    let text = tree_text(&s.harness);
    assert!(!text.contains("the ENVIRONMENT sets this key"), "{text}");
    let buttons = button_labels(&s.harness);
    assert!(buttons.iter().any(|b| b == "Save to file"), "{buttons:?}");
    assert!(!buttons.iter().any(|b| b == "Save to file anyway"), "{buttons:?}");
}

/// The editor states exactly what will be written and WHERE, shows the effective value with its
/// origin, and carries the sandbox residual beside Save on every row.
#[test]
fn the_editor_states_the_write_target_the_effective_value_and_the_sandbox_residual() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    open_row(&mut s, 0);
    let text = tree_text(&s.harness);
    assert!(text.contains("Effective now"), "{text}");
    assert!(text.contains("from config.toml"), "the origin travels with the value: {text}");
    assert!(
        text.contains("/srv/vike-<unit>/settings/config.toml"),
        "the exact FILE is named before the click: {text}"
    );
    assert!(text.contains("takes effect on restart"), "{text}");
    assert!(
        text.contains("ProtectSystem=strict") && text.contains("EROFS"),
        "⚠ the deployed-box residual must sit beside the button: {text}"
    );
    assert!(text.contains("Do not widen"), "…and must refuse the repair: {text}");
}

/// A `policy.toml` row still demands the typed confirm, and the button stays DISABLED until the
/// exact key is typed. The redesign moved the editor; it may not have relaxed the ceremony.
#[test]
fn a_policy_row_still_demands_the_typed_confirm() {
    let mut s = settings_harness(BackendSettingsState::Loaded(show()), SettingsFilter::All);
    s.harness.run();
    open_row(&mut s, 4);
    let text = tree_text(&s.harness);
    assert!(text.contains("policy ceiling"), "{text}");

    // The confirm buffer starts EMPTY — never pre-filled. (A hint is not a pre-fill: the wire
    // write's own typed-confirm contract demands the operator types the key.)
    let empty_confirm = matches!(
        &s.state.borrow().0,
        SettingsEditState::Editing { confirm, .. } if confirm.is_empty()
    );
    assert!(empty_confirm, "the typed confirm is never pre-filled: {:?}", s.state.borrow().0);

    // ⚠ The gate is read through BEHAVIOUR rather than through an `is_disabled` flag: clicking
    // Save must not advance the flow. `add_enabled(false, ..)` makes egui swallow the click, so a
    // flow still in `Editing` afterwards is the ceremony holding.
    click(&mut s.harness, "Save to file");
    assert!(
        matches!(&s.state.borrow().0, SettingsEditState::Editing { .. }),
        "an unconfirmed policy Save must take nothing: {:?}",
        s.state.borrow().0
    );
}

/// ⚠ **An UNARMED backend's write channel is named as the blocker BEFORE the click**, and the
/// sentence also names the node-side gate this process cannot see. Reddens on an editor that
/// offers Save against an observe-only record and lets a transport refusal be the first word on
/// the subject.
#[test]
fn an_unarmed_backend_says_its_write_channel_cannot_sign_the_write() {
    let mut armed = settings_harness_armed(
        BackendSettingsState::Loaded(show()),
        SettingsFilter::All,
        true,
        1000.0,
    );
    armed.harness.run();
    open_row(&mut armed, 0);
    assert!(
        !tree_text(&armed.harness).contains("write channel is NOT armed"),
        "an armed record gets no such line"
    );

    let mut unarmed = settings_harness_armed(
        BackendSettingsState::Loaded(show()),
        SettingsFilter::All,
        false,
        1000.0,
    );
    unarmed.harness.run();
    open_row(&mut unarmed, 0);
    let text = tree_text(&unarmed.harness);
    assert!(text.contains("write channel is NOT armed"), "{text}");
    assert!(text.contains("observe key cannot sign it"), "{text}");
    assert!(
        text.contains("flags.tradehub_control"),
        "…and the gate this side cannot see is named rather than assumed away: {text}"
    );
}

/// ⚠ The table survives ~400pt: the sticky header and its rows shrink TOGETHER, so the labels
/// stay registered with their columns, and every key still reaches the tree. Reddens on a header
/// computed independently of the rows — the failure mode a sticky header drawn outside the scroll
/// area invites.
#[test]
fn the_settings_table_survives_a_four_hundred_point_window() {
    let mut s = settings_harness_armed(
        BackendSettingsState::Loaded(show()),
        SettingsFilter::All,
        true,
        400.0,
    );
    s.harness.run();
    let text = tree_text(&s.harness);
    for header in ["KEY", "VALUE", "ORIGIN", "READ"] {
        assert!(text.contains(header), "{header} must survive a narrow window: {text}");
    }
    let edits = nodes(&s.harness, |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some("edit")
    });
    assert_eq!(edits.len(), 5, "every row is still editable at 400pt");
}

/// Every non-`Loaded` state renders its own sentence rather than an ellipsis that outlives the
/// fault. ⚠ Five states, not three: `Unsupported` and `Error` are reachable on day one.
#[test]
fn every_fetch_state_renders_its_own_sentence() {
    for (state, needle) in [
        // ⚠ Idle and Pending render DIFFERENT sentences — "not asked yet" is not "asking".
        // Collapsing them is what let a tab-conditional auto-fetch pass for a working one.
        (BackendSettingsState::Idle, "have not been read yet"),
        (BackendSettingsState::Pending, "reading the node's settings"),
        (BackendSettingsState::Unsupported, "server predates settings-show"),
        (BackendSettingsState::Error("bad mac".into()), "bad mac"),
    ] {
        let mut s = settings_harness(state.clone(), SettingsFilter::Set);
        s.harness.run();
        let text = tree_text(&s.harness);
        assert!(text.contains(needle), "{state:?} must render {needle:?}: {text}");
        // ⚠ And the FILTER is absent, because it would carry two counts of a row set that has not
        // arrived. `set · 0 / all · 0` is two invented numbers where two measured ones go.
        assert!(!text.contains("set · "), "{state:?} must render no filter counts: {text}");
        assert!(!text.contains("all · "), "{state:?} must render no filter counts: {text}");
    }
}

// ------------------------------------------------------------------------------------------
// 6 — the two counts this window is not allowed to invent
// ------------------------------------------------------------------------------------------

/// ⚠ **THE CREDENTIALS SEGMENT OBEYS THE SAME RULE THE BACKEND ONE DOES.**
///
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env` is documented INFALLIBLE: a
/// store that EXISTS and cannot be opened logs `tracing::error!` and returns an EMPTY map,
/// byte-identical to an absent store. Folded blind, this chrome renders a confident `Credentials
/// 0` and `0 set of 34 configurable` — measured numbers about a file nothing read, which the root
/// `CLAUDE.md` names as the one thing these two states may never do ("a permissions bug wearing
/// the 'not configured' answer looks exactly like a correct fresh install").
///
/// Reddens on `CredentialSummary::configured` going back to a plain `usize`, and on `tab_bar`
/// passing `Some(..)` unconditionally.
#[test]
fn an_unreadable_credential_store_renders_no_count_on_either_surface() {
    let loaded = BackendDigest::of(true, &BackendSettingsState::Loaded(show()));
    let mut h =
        tab_harness_cred(ConnectionsTab::Backend, loaded.clone(), 1000.0, unreadable_summary());
    h.run();

    // The segment is the INACTIVE one here, so it is a button and its label is the whole chip.
    let buttons = button_labels(&h);
    assert!(
        buttons.iter().any(|b| b == "Credentials"),
        "the segment must read `Credentials` with no number: {buttons:?}"
    );
    assert!(
        !buttons.iter().any(|b| b.starts_with("Credentials 0")),
        "a zero is a number this tool does not have: {buttons:?}"
    );

    let text = tree_text(&h);
    assert!(!text.contains("0 set of"), "…and neither does the status line: {text}");
    assert!(text.contains("store unreadable"), "it says what happened instead: {text}");
    assert!(text.contains("34 configurable"), "the denominator is still known: {text}");

    // …and a store that ANSWERED still prints its count, including a measured zero.
    let measured = CredentialSummary { configured: Some(0), ..summary() };
    let mut h = tab_harness_cred(ConnectionsTab::Backend, loaded, 1000.0, measured);
    h.run();
    let buttons = button_labels(&h);
    assert!(
        buttons.iter().any(|b| b == "Credentials 0"),
        "a MEASURED zero is a number, and it prints: {buttons:?}"
    );
}

/// ⚠ **THE DIGEST CAN SAY `not read yet`, WHICH IS NOT `reading…`.**
///
/// `BackendDigest::of(true, Idle)` used to fold to `Loading`, so the status line's digested
/// Backend half claimed a fetch was in flight in the one state where nothing had asked for one.
/// That mattered because the auto-fetch lived inside `backend_settings_section`, which the tab
/// split draws on only one of two tabs — so on the default Credentials tab the state stayed `Idle`
/// for ever and the sentence was permanently false.
///
/// The fix is the relocated driver (`connections_tool_content` calls `should_fetch_settings`
/// before the tab branch); this is the belt that makes a re-hidden driver VISIBLE instead of
/// plausible. Reddens on the two states folding back together.
#[test]
fn the_backend_digest_tells_not_asked_yet_apart_from_asking() {
    let idle = BackendDigest::of(true, &BackendSettingsState::Idle);
    let pending = BackendDigest::of(true, &BackendSettingsState::Pending);
    assert_ne!(idle, pending, "two different facts");
    assert_eq!(idle, BackendDigest::NotFetched);
    assert_eq!(pending, BackendDigest::Loading);
    assert_eq!(idle.badge_count(), None, "neither invents a number");
    assert_eq!(pending.badge_count(), None);

    let mut h = tab_harness(ConnectionsTab::Credentials, idle, 1000.0);
    h.run();
    let text = tree_text(&h);
    assert!(text.contains("Backend settings  not read yet"), "{text}");
    assert!(!text.contains("reading…"), "nothing may claim a fetch is in flight: {text}");

    let mut h = tab_harness(ConnectionsTab::Credentials, pending, 1000.0);
    h.run();
    assert!(tree_text(&h).contains("Backend settings  reading…"));
}

/// ⚠ **THE SECTION NO LONGER DRIVES THE FETCH, AND THE TOOL DOES.**
///
/// `should_fetch_settings` was called from inside `backend_settings_section` and nowhere else.
/// That body is drawn only on the Backend tab, and `ConnectionsTab::default()` is `Credentials` —
/// so a connected backend left on the default tab never fetched anything, while the chrome on that
/// same tab described the fetch. The decision itself is unchanged and still pure; what moved is
/// WHO calls it.
///
/// This pins both halves: the predicate still answers `true` for an idle active backend (so the
/// relocation did not silently disarm it), and the section body no longer sets the out-slot for it
/// — only the explicit Refresh click does. A section that set it again would restore the coupling
/// this window's design removed.
#[test]
fn the_settings_section_sets_no_auto_fetch_and_only_refresh_does() {
    assert!(
        vike_app_core::tool_views::should_fetch_settings(true, &BackendSettingsState::Idle),
        "the predicate itself is unchanged — an idle ACTIVE backend is still a fetch"
    );

    let refreshed = std::rc::Rc::new(std::cell::Cell::new(false));
    let inner = refreshed.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui(move |ui| {
        let mut refresh = false;
        let mut edit = SettingsEditState::Idle;
        let mut write: Option<SettingsWriteRequest> = None;
        let mut filter = SettingsFilter::Set;
        backend_settings_section(
            ui,
            Some("prod"),
            true,
            &BackendSettingsState::Idle,
            &mut refresh,
            &mut edit,
            &mut write,
            &mut filter,
        );
        if refresh {
            inner.set(true);
        }
    });
    h.run();
    assert!(
        !refreshed.get(),
        "drawing the section must NOT request a fetch — the tool does that, before the tab branch, \
         so the Credentials tab gets one too"
    );

    click(&mut h, "Refresh");
    assert!(refreshed.get(), "…and the explicit Refresh click still does, in any state");
}
