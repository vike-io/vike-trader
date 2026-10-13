//! **The backend-connection row Data Manager renders directly above its Credentials/Backend
//! destinations** — `connections.rs`'s `strip_row`, called from `data.rs`'s `data_body` since
//! 2026-10-05 (Connections-merges-into-Data-Manager).
//!
//! Replaces most of `connections_tabs.rs` (deleted this task), adapted for the new call shape:
//! `strip_row` no longer sits inside `ambient_strip`'s zero-height-measuring wrapper or
//! `connections_body`'s foot-strip reservation feedback loop — neither exists any more, because
//! Data Manager draws this row in-flow, like any other widget, with nothing downstream measuring
//! it back into a layout decision the way the standalone window's foot strip had to be protected
//! from the window's own floor. Several clusters of the old file's coverage did NOT port, each for
//! a stated reason rather than by omission:
//!
//! * The whole title-bar-tab-chrome suite (`the_selected_tab_is_a_label_and_the_other_is_the_only_
//!   button`, every `tab_harness`/`tab_frame`/`slot_at` test, `hairline_segments`, `status_line`,
//!   `credentials_line`) — guarded `title_bar_tabs`/`tab_bar`/`status_line`/`credentials_line`,
//!   which are deleted outright: Data Manager picks between destinations with its own rail
//!   (`data_rail::rail`), not a segmented control seated in the title bar, and `strip_row` itself
//!   carries no status-line digest.
//! * `the_strips_measured_height_does_not_depend_on_the_room_below_it` and the foot-strip
//!   reservation/geometry suite (`the_foot_strip_survives_a_body_that_eats_every_pixel`,
//!   `..._fits_the_row_that_was_reserved_for_it`, `..._is_a_hittable_row_when_the_body_overflows_
//!   the_window`) — guarded `connections_body`'s `set_max_height`/floor-row fallback and the
//!   measured-height feedback loop it fed. Data Manager has no equivalent: `strip_row` draws in
//!   the ordinary flow, and nothing reads its height back into how much room anything else gets.
//! * `an_unreadable_credential_store_renders_no_count_on_either_surface` — guarded the Credentials/
//!   Backend title-bar SEGMENT's `Option<usize>` badge. Data Manager's rail has no analogous risk
//!   to protect against: `data_rail::RailCounts::count` returns a hardcoded `None` for both
//!   destinations regardless of store health, so there is no badge that could ever print an
//!   invented zero.
//! * The chrome-driven half of `the_backend_digest_tells_not_asked_yet_apart_from_asking` (the
//!   `tab_harness`/status-line string checks) — guarded `status_line`'s rendering of
//!   `BackendDigest::line()`. The pure half (that `Idle`/`Pending` fold to different
//!   `BackendDigest` variants and neither invents a count) ported as a new inline test in
//!   `connections.rs`'s own `#[cfg(test)] mod tests`, since `BackendDigest::line()` already
//!   carries the exact wording ("not read yet" / "reading…") with no chrome needed to see it.
//!
//! What DID port, because the thing it guards is unchanged or still real:
//! * `strip_row`'s own content (identity, address, control state, Disconnect, Add backend) —
//!   byte-for-byte the same function; only the parameter "which destination does Add backend
//!   switch to" changed type, from `ConnectionsTab` to `DataDest`.
//! * The de-duplication property between `strip_row` and `backend_tab` — the ONE real regression
//!   risk this whole plan amendment exists for: `backend_tab`'s `registry_rows` must never restate
//!   what `strip_row` already drew, now that both are drawn in the same frame by `data_body`
//!   rather than by two mutually-exclusive tab bodies.

use egui::accesskit::Role;
use egui_kittest::kittest::NodeT;
use egui_kittest::{Harness, Node};
use std::assert_matches;
use vike_app_core::backend::backend_conn::{BackendAction, cli_observe_record};
use vike_app_core::backend::backend_editor::EditorState;
use vike_app_core::backend::backend_registry::{BackendRecord, BackendsFile};
use vike_app_core::ui::tool_views::{
    BackendPicker, BackendSettingsState, DataDest, SettingsEditState, SettingsFilter,
    SettingsWriteRequest, backend_tab, strip_row,
};

// ------------------------------------------------------------------------------------------
// Tree helpers — the same shapes `connections_tabs.rs` used, ported verbatim.
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

fn node_text(n: &Node<'_>) -> String {
    let a = n.accesskit_node();
    match (a.label(), a.value()) {
        (Some(l), _) if !l.is_empty() => l.to_string(),
        (_, Some(v)) => v.to_string(),
        _ => String::new(),
    }
}

fn node_rect(n: &Node<'_>) -> Option<egui::Rect> {
    n.accesskit_node().bounding_box().map(|_| n.rect())
}

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

fn text_rects(h: &Harness<'_, ()>, want: &str) -> Vec<egui::Rect> {
    nodes(h, |_| true).iter().filter(|n| node_text(n) == want).filter_map(node_rect).collect()
}

/// Every DISTINCT position a node carrying exactly `want` was laid out at, rounded to the point —
/// see `connections_tabs.rs`'s original for why this counts positions rather than nodes.
fn distinct_positions(h: &Harness<'_, ()>, want: &str) -> Vec<(i32, i32)> {
    let mut out: Vec<(i32, i32)> = text_rects(h, want)
        .iter()
        .map(|r| (r.left().round() as i32, r.top().round() as i32))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn click(h: &mut Harness<'_, ()>, want: &str) {
    let wanted = want.to_string();
    let found = nodes(h, move |n| {
        let a = n.accesskit_node();
        a.role() == Role::Button && a.label().as_deref() == Some(wanted.as_str())
    });
    assert_eq!(found.len(), 1, "exactly one button labelled {want:?} must be on screen");
    found[0].click();
    h.run();
}

// ------------------------------------------------------------------------------------------
// Fixtures
// ------------------------------------------------------------------------------------------

fn registry() -> BackendsFile {
    BackendsFile::default()
}

fn record() -> BackendRecord {
    cli_observe_record("127.0.0.1:7879")
}

// ------------------------------------------------------------------------------------------
// strip_row's own content
// ------------------------------------------------------------------------------------------

struct Strip {
    harness: Harness<'static, ()>,
    state: std::rc::Rc<std::cell::RefCell<(DataDest, EditorState)>>,
}

fn strip_harness(dest: DataDest, active: bool) -> Strip {
    strip_harness_reporting(dest, active, None)
}

/// [`strip_harness`] with the daemon's self-report supplied — what the far end said about which
/// box it is running on. `None` is the old-node case and is what [`strip_harness`] passes.
fn strip_harness_reporting(dest: DataDest, active: bool, reported: Option<String>) -> Strip {
    let file = registry();
    let rec = record();
    let shared: std::rc::Rc<std::cell::RefCell<(DataDest, EditorState)>> =
        std::rc::Rc::new(std::cell::RefCell::new((dest, EditorState::Closed)));
    let inner = shared.clone();
    let harness = Harness::builder().with_size(egui::vec2(700.0, 200.0)).build_ui(move |ui| {
        let picker = BackendPicker {
            backends: &file,
            active: if active { Some(&rec) } else { None },
            reported: vike_app_core::backend::backend_identity::SelfReport::of(reported.as_deref()),
        };
        let mut action = None;
        let mut state = inner.borrow_mut();
        let (dest, editor) = &mut *state;
        strip_row(ui, &picker, &mut action, dest, editor);
    });
    Strip { harness, state: shared }
}

/// **The row is IDENTICAL whichever destination asked for it.** It is process-level state you
/// glance at, and it does not read the destination it was called from — the destination is only
/// an OUT slot ("Add backend" writes `DataDest::Backend` into it), never an input to what gets
/// drawn. Reddens if a future edit makes the row's content depend on `*dest`'s starting value.
#[test]
fn the_strip_is_byte_identical_whichever_destination_it_is_drawn_from() {
    let mut creds = strip_harness(DataDest::Credentials, true);
    creds.harness.run();
    let mut backend = strip_harness(DataDest::Backend, true);
    backend.harness.run();
    assert_eq!(
        tree_text(&creds.harness),
        tree_text(&backend.harness),
        "the row is process-level state and does not read which of the two destinations asked \
         for it"
    );
}

/// ⚠ **Add backend takes the operator to where the form lands.** The add/edit form is
/// `backend_tab`'s — so the click must switch `*dest` to `DataDest::Backend` as well as opening
/// the form. Reddens on a click that opens a form on a destination the operator is not looking at.
#[test]
fn add_backend_from_the_strip_switches_to_the_destination_that_renders_the_form() {
    let mut s = strip_harness(DataDest::Credentials, true);
    s.harness.run();
    click(&mut s.harness, "Add backend");
    let state = s.state.borrow();
    assert_eq!(state.0, DataDest::Backend, "the click switches destinations");
    assert_matches!(state.1, EditorState::Add { .. }, "…and opens the add form: {:?}", state.1);
}

/// The strip names the live connection: its identity, its address, its control state and the fact
/// that the registry does not list it.
///
/// ⚠ **The HEADLINE is the identity, and it used to be `(--observe)`** — the name of the FLAG this
/// client was launched with, which is a property of the client and not of the backend at all. The
/// address beside it identified nothing either: both the CI box listeners bind loopback, so through the
/// SSH tunnel that is the only route in, every thin client on every box read `127.0.0.1`.
/// `vike_app_core::backend::backend_identity` carries the measurement and owns both spellings.
#[test]
fn the_strip_names_the_live_connection_its_address_and_its_control_state() {
    let mut s = strip_harness(DataDest::Credentials, true);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(
        text.contains(vike_app_core::backend::backend_identity::UNNAMED_HEADLINE),
        "an unnamed backend says so plainly — it never renders the launch flag as a name: {text}"
    );
    assert!(!text.contains("(--observe)"), "the launch flag is not the backend's identity: {text}");
    assert!(text.contains("127.0.0.1:7879"), "{text}");
    // `cli_observe_record` arms control (the process-level `flags.tradehub_control` gate is the
    // only one on the CLI path), so this is the mockup's own example strip verbatim.
    assert!(text.contains("control armed"), "{text}");
    assert!(
        text.contains(vike_app_core::backend::backend_identity::NAME_THIS_BACKEND_NOTE),
        "…and the registry note is an INVITATION naming the button that answers it, not a \
         footnote stating a fault: {text}"
    );
    let buttons = button_labels(&s.harness);
    assert!(buttons.iter().any(|b| b == "Disconnect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == "Add backend"), "{buttons:?}");
}

/// ⚠⚠ **THE OWNER'S CASE, END TO END: an unnamed `--observe` connection to a daemon that reports
/// which box it is puts THAT ADDRESS IN THE STRIP ROW.**
///
/// This drives the REAL [`strip_row`] with the same `cli_observe_record` a `--observe` session
/// builds and an EMPTY registry. Two assertions and both are the point: the daemon's address IS in
/// the row, and the tunnel mouth is NOT (it is on the hover, which the accessibility tree does not
/// carry — that is the move, not a loss).
///
/// Reddens on a daemon self-report that does not reach the row, and on a change that puts the
/// tunnel mouth back beside it. ⚠ RFC 5737 documentation address: a real box's address must never
/// reach a tracked file.
#[test]
fn the_strip_shows_the_address_the_daemon_reported_and_not_the_tunnel_mouth() {
    let mut s =
        strip_harness_reporting(DataDest::Credentials, true, Some("203.0.113.7:7879".to_string()));
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(
        text.contains("203.0.113.7:7879"),
        "the daemon's own report of which box it is must be IN THE ROW: {text}"
    );
    assert!(
        !text.contains("127.0.0.1"),
        "…and the tunnel mouth is not: it is the same string on every box, which is the whole \
         complaint. It moves to the address hover, not beside the real one: {text}"
    );
    assert!(
        text.contains(vike_app_core::backend::backend_identity::UNNAMED_HEADLINE),
        "a reported address is never promoted to a name: {text}"
    );
    assert!(
        text.contains(vike_app_core::backend::backend_identity::NAME_THIS_BACKEND_NOTE),
        "{text}"
    );
    assert!(text.contains("control armed"), "{text}");
}

/// ⚠ **AN OLD NODE REPORTS NOTHING, AND THE STRIP IS UNCHANGED — END TO END, FROM ITS WIRE BYTES.**
///
/// The frame is DESERIALIZED from an identity block that carries none of the new key, which is
/// literally what a daemon built before this change sends, and its `advertise_addr` is then driven
/// through the REAL [`strip_row`]. That chain is the point: `crates/vike-tradehub-client`'s own
/// suite proves the JSON parses as empty, this proves empty renders as it always did, and nothing
/// in between gets to decide that a blank report means something.
#[test]
fn a_daemon_that_reports_nothing_renders_the_strip_exactly_as_before() {
    let old: vike_tradehub_client::wire::WireNodeIdentity = serde_json::from_str(
        r#"{"name":"the build runner","strategy":"spread_maker","params":"{}","live":true,
            "build":"vike-tradehub 0.1.0 (abc1234)"}"#,
    )
    .expect("an old node's frame still parses");
    assert_eq!(old.advertise_addr, "", "an absent key is empty, not an error and not an address");

    let mut none = strip_harness(DataDest::Credentials, true);
    none.harness.run();
    for blank in [old.advertise_addr.clone(), String::new(), "   ".to_string()] {
        let mut s = strip_harness_reporting(DataDest::Credentials, true, Some(blank.clone()));
        s.harness.run();
        assert_eq!(
            tree_text(&s.harness),
            tree_text(&none.harness),
            "a blank report is the same as no report: {blank:?}"
        );
    }
    assert!(
        tree_text(&none.harness).contains("127.0.0.1:7879"),
        "…and with nothing reported the dial address is still all the strip has: {}",
        tree_text(&none.harness)
    );
}

/// A NAMED record leads with its NAME on the strip, and the address follows as detail.
#[test]
fn a_named_backend_leads_with_its_name_on_the_strip() {
    let named = BackendRecord {
        name: "the CI box".to_string(),
        addr: "127.0.0.1:7879".to_string(),
        observe_key: "PROD2_OBSERVE_KEY".to_string(),
        control_key: None,
        control: false,
        datahub_observe_key: String::new(),
    };
    let file = BackendsFile { backends: vec![named.clone()], active: Some("the CI box".to_string()) };
    let state: std::rc::Rc<std::cell::RefCell<(DataDest, EditorState)>> =
        std::rc::Rc::new(std::cell::RefCell::new((DataDest::Backend, EditorState::Closed)));
    let inner = state.clone();
    let mut harness = Harness::builder().with_size(egui::vec2(900.0, 200.0)).build_ui(move |ui| {
        let picker = BackendPicker { backends: &file, active: Some(&named), reported: None };
        let mut action = None;
        let mut st = inner.borrow_mut();
        let (dest, editor) = &mut *st;
        strip_row(ui, &picker, &mut action, dest, editor);
    });
    harness.run();
    let text = tree_text(&harness);
    assert!(text.contains("the CI box"), "the operator's own name for the box is the headline: {text}");
    assert!(text.contains("127.0.0.1:7879"), "…and the address is still there as detail: {text}");
    assert!(
        !text.contains(vike_app_core::backend::backend_identity::UNLISTED_NOTE),
        "a listed record is not annotated: {text}"
    );
    assert!(
        !text.contains(vike_app_core::backend::backend_identity::UNNAMED_HEADLINE),
        "a named record never renders the stand-in: {text}"
    );
}

/// A named record and a reporting daemon are TWO facts and the strip carries both.
#[test]
fn a_named_record_carries_its_name_and_its_daemons_reported_address_side_by_side() {
    let named = BackendRecord {
        name: "the CI box".to_string(),
        addr: "127.0.0.1:7879".to_string(),
        observe_key: "PROD2_OBSERVE_KEY".to_string(),
        control_key: None,
        control: false,
        datahub_observe_key: String::new(),
    };
    let file = BackendsFile { backends: vec![named.clone()], active: Some("the CI box".to_string()) };
    let state: std::rc::Rc<std::cell::RefCell<(DataDest, EditorState)>> =
        std::rc::Rc::new(std::cell::RefCell::new((DataDest::Backend, EditorState::Closed)));
    let inner = state.clone();
    let mut harness = Harness::builder().with_size(egui::vec2(900.0, 200.0)).build_ui(move |ui| {
        let picker = BackendPicker {
            backends: &file,
            active: Some(&named),
            reported: vike_app_core::backend::backend_identity::SelfReport::of(Some(
                "203.0.113.7:7879",
            )),
        };
        let mut action = None;
        let mut st = inner.borrow_mut();
        let (dest, editor) = &mut *st;
        strip_row(ui, &picker, &mut action, dest, editor);
    });
    harness.run();
    let text = tree_text(&harness);
    assert!(text.contains("the CI box"), "the operator's own name for the box: {text}");
    assert!(
        text.contains("203.0.113.7:7879"),
        "…and the daemon's own report of where it is: {text}"
    );
    assert!(!text.contains("127.0.0.1"), "…and not the tunnel mouth, which names no box: {text}");
}

/// With nothing connected the strip says so and offers no Disconnect — never an empty address or
/// a dot that could be read as connected.
#[test]
fn a_disconnected_strip_says_so_and_offers_no_disconnect() {
    let mut s = strip_harness(DataDest::Credentials, false);
    s.harness.run();
    let text = tree_text(&s.harness);
    assert!(text.contains("not connected"), "{text}");
    let buttons = button_labels(&s.harness);
    assert!(!buttons.iter().any(|b| b == "Disconnect"), "{buttons:?}");
    assert!(buttons.iter().any(|b| b == "Add backend"), "adding one is still offered: {buttons:?}");
}

// ------------------------------------------------------------------------------------------
// The de-duplication property — strip_row and backend_tab, drawn together as data_body does
// ------------------------------------------------------------------------------------------

/// One frame of `strip_row` immediately followed by the real `backend_tab` — the exact sequence
/// `data_body` draws for `DataDest::Backend`, minus the settings-table plumbing neither gate below
/// reads.
fn backend_frame(file: BackendsFile, active: Option<BackendRecord>) -> Harness<'static, ()> {
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui(move |ui| {
        let picker = BackendPicker { backends: &file, active: active.as_ref(), reported: None };
        let mut action: Option<BackendAction> = None;
        let mut dest = DataDest::Backend;
        let mut editor = EditorState::Closed;
        strip_row(ui, &picker, &mut action, &mut dest, &mut editor);

        let vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut registry_update = None;
        let settings = BackendSettingsState::Idle;
        let mut refresh = false;
        let mut edit = SettingsEditState::Idle;
        let mut write: Option<SettingsWriteRequest> = None;
        let mut filter = SettingsFilter::Set;
        backend_tab(
            ui,
            &vars,
            &picker,
            &mut action,
            &mut editor,
            &mut registry_update,
            &settings,
            &mut refresh,
            &mut edit,
            &mut write,
            &mut filter,
        );
    });
    h.run();
    h
}

/// ⚠⚠ **THE LIVE CONNECTION IS RENDERED ONCE ACROSS THE WHOLE FRAME — including once `strip_row`
/// and `backend_tab` are drawn together rather than exclusively, as the deleted tab model had it.**
///
/// `backend_tab`'s own doc has claimed since the redesign that the live connection "is not
/// rendered again: it is [`strip_row`]'s, once" — true by construction (`backend_tab` reads
/// `registry_rows`, never the unlisted-live-connection row `picker_rows` would add), but this is
/// the gate that actually drives both functions in the SAME frame and measures it, which is the
/// property a stub body cannot prove.
#[test]
fn the_live_connection_is_rendered_once_across_the_strip_and_the_backend_destination() {
    let h = backend_frame(registry(), Some(record()));
    let addr = distinct_positions(&h, "127.0.0.1:7879");
    assert_eq!(
        addr.len(),
        1,
        "the live connection's ADDRESS is laid out at {} distinct places — the strip is the \
         rendering that stays, and backend_tab may not restate it:\n{}",
        addr.len(),
        tree_text(&h)
    );
    let disconnects: Vec<String> =
        button_labels(&h).into_iter().filter(|b| b == "Disconnect").collect();
    assert_eq!(
        disconnects.len(),
        1,
        "exactly one Disconnect in the whole frame — the strip's: {disconnects:?}"
    );
}

/// The Backend destination still SAYS what it stopped rendering: an unlisted live connection gets
/// one sentence pointing at where it IS rendered, which is information the strip itself can't
/// carry (it has no idea it is unlisted).
#[test]
fn an_unlisted_live_connection_is_named_in_words_on_the_backend_destination() {
    let h = backend_frame(registry(), Some(record()));
    let text = tree_text(&h);
    assert!(
        text.contains("not a backends.json record"),
        "the destination says why there is no row for the live connection: {text}"
    );
    assert!(
        text.contains("backend-connection row above this screen"),
        "…and points at where it IS rendered: {text}"
    );
}

/// A LISTED active record keeps its registry row and loses only the verb the strip owns.
#[test]
fn a_listed_active_record_keeps_its_row_and_the_strip_keeps_the_only_disconnect() {
    let rec_a = BackendRecord { name: "the latency box".into(), addr: "<host>:7879".into(), ..record() };
    let rec_b = BackendRecord { name: "the CI box".into(), addr: "<host>:7879".into(), ..record() };
    let file = BackendsFile { backends: vec![rec_a, rec_b.clone()], active: None };
    let h = backend_frame(file, Some(rec_b));

    let text = tree_text(&h);
    assert!(text.contains("the latency box") && text.contains("the CI box"), "both records have rows: {text}");
    assert!(
        !text.contains("not a backends.json record"),
        "…and the live connection IS a record here, so no such sentence: {text}"
    );
    let buttons = button_labels(&h);
    assert_eq!(
        buttons.iter().filter(|b| *b == "Disconnect").count(),
        1,
        "exactly one Disconnect in the window — the strip's, never the active row's: {buttons:?}"
    );
    assert_eq!(
        buttons.iter().filter(|b| *b == "Connect").count(),
        1,
        "…and Connect on the ONE non-active record, which is how switching happens: {buttons:?}"
    );
    assert_eq!(
        buttons.iter().filter(|b| *b == "Edit").count(),
        2,
        "both records stay editable, active included — that is what a registry row is for: \
         {buttons:?}"
    );
}
