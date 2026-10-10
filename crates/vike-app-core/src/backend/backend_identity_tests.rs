use super::*;

fn record(name: &str, addr: &str) -> BackendRecord {
    BackendRecord {
        name: name.to_string(),
        addr: addr.to_string(),
        observe_key: "K".to_string(),
        control_key: None,
        control: false,
        datahub_observe_key: String::new(),
    }
}

/// The headline is the NAME, and the address is a separate, secondary field.
#[test]
fn a_named_listed_record_leads_with_its_name_and_says_nothing_else() {
    let r = record("the CI box", "127.0.0.1:7879");
    let id = identify(&r, true);
    assert_eq!(id.headline, "the CI box");
    assert!(id.named);
    assert_eq!(id.addr, "127.0.0.1:7879");
    assert_eq!(id.note, None);
    assert!(!id.invitation);
    assert_eq!(id.headline_hover(), None);
}

/// ⚠ The defect, pinned: the `--observe` record used to headline as `(--observe)`, which names
/// the FLAG rather than the backend. It now says it has no name, and the note is the action.
#[test]
fn an_unnamed_record_says_so_and_invites_a_name_rather_than_inventing_one() {
    let r = record("", "127.0.0.1:7879");
    let id = identify(&r, false);
    assert_eq!(id.headline, UNNAMED_HEADLINE);
    assert!(!id.named);
    assert_eq!(id.note, Some(NAME_THIS_BACKEND_NOTE));
    assert!(id.invitation, "an operator who cannot tell which box this is needs an action");
    assert_eq!(id.headline_hover(), Some(UNNAMED_HOVER));
    // …and nothing in the rendering is derived from the address.
    assert!(!id.headline.contains("127.0.0.1"), "the address is never dressed up as a name");
    assert!(!id.headline.contains("observe"), "the launch flag is not the backend's identity");
}

/// The two facts are independent: a NAMED record the file no longer holds keeps its name as
/// the headline and gets the quieter footnote.
#[test]
fn a_named_but_unlisted_record_keeps_its_name_and_gets_the_footnote() {
    let r = record("the CI box", "127.0.0.1:7879");
    let id = identify(&r, false);
    assert_eq!(id.headline, "the CI box");
    assert_eq!(id.note, Some(UNLISTED_NOTE));
    assert!(!id.invitation, "the name already says which box this is");
}

/// A whitespace-only name is an absent one, not a blank headline.
#[test]
fn a_whitespace_name_is_an_absent_name() {
    let r = record("   ", "127.0.0.1:7879");
    assert_eq!(headline(&r), UNNAMED_HEADLINE);
    assert!(!is_named(&r));
    assert_eq!(status_label(&r), None);
}

#[test]
fn one_line_leads_with_the_name_and_parenthesises_the_address() {
    assert_eq!(one_line(&record("the CI box", "127.0.0.1:7879")), "the CI box (127.0.0.1:7879)");
    assert_eq!(
        one_line(&record("", "127.0.0.1:7879")),
        "unnamed backend (127.0.0.1:7879)",
        "an unnamed backend says so in the window title too"
    );
    assert_eq!(one_line(&record("the CI box", "  ")), format!("the CI box ({NO_ADDRESS})"));
}

#[test]
fn a_blank_address_renders_as_a_stated_absence() {
    let r = record("the CI box", "");
    assert_eq!(identify(&r, true).addr_label(), NO_ADDRESS);
}

/// The note an operator reads has to name the button that resolves it — the whole complaint
/// against `(not in registry)` was that it stated a fault and no remedy.
#[test]
fn the_invitation_names_the_button_that_answers_it() {
    assert!(NAME_THIS_BACKEND_NOTE.contains("Add backend"), "{NAME_THIS_BACKEND_NOTE}");
}

/// The hover carries the MEASURED reason the address identifies nothing, so the claim is
/// readable at the point of confusion rather than only in this module's doc.
#[test]
fn the_address_hover_states_why_an_address_is_not_an_identity() {
    assert!(ADDRESS_HOVER.contains("127.0.0.1"), "{ADDRESS_HOVER}");
    assert!(ADDRESS_HOVER.contains("tunnel"), "{ADDRESS_HOVER}");
}

// ----------------------------------------------------------------------------------------
// The daemon's self-report. ⚠ RFC 5737 documentation addresses throughout — a real box's
// address must never reach a tracked file.
// ----------------------------------------------------------------------------------------

/// **THE WHOLE POINT: a reporting daemon's address is what the row shows, and the tunnel mouth
/// is not in it.**
///
/// Reddens on any change that puts the dial address back in the row beside the real one, or
/// that drops the real one. Both have been the shipped behaviour, and the second is the one an
/// operator cannot work around.
#[test]
fn a_reporting_daemon_puts_its_own_address_in_the_row_and_the_dial_address_in_the_hover() {
    let r = record("", "127.0.0.1:7879");
    let id = identify_reported(&r, false, SelfReport::of(Some("203.0.113.7:7879")));
    assert_eq!(id.box_address(), "203.0.113.7:7879", "the box, not the tunnel");
    assert!(id.box_address_is_reported(), "…and the surface is told it is the daemon's claim");
    let hover = id.box_address_hover();
    assert!(hover.contains("127.0.0.1:7879"), "the dial address MOVES to the hover: {hover}");
    assert!(hover.contains("CLAIM"), "…and the hover says nothing verified it: {hover}");
    // The two facts stay separate: the report never becomes the headline.
    assert_eq!(id.headline, UNNAMED_HEADLINE);
    assert!(!id.headline.contains("203.0.113"), "an address is never dressed up as a name");
    // …and the invitation to NAME the box survives — an address is not a name.
    assert_eq!(id.note, Some(NAME_THIS_BACKEND_NOTE));
}

/// **AN OLD NODE REPORTS NOTHING, AND THAT PATH IS UNCHANGED.** Blankness is not evidence about
/// the daemon, so nothing is inferred from it: the row falls back to exactly what it rendered
/// before this field existed, hover included.
#[test]
fn a_daemon_that_reports_nothing_renders_exactly_as_it_did_before() {
    let r = record("the CI box", "127.0.0.1:7879");
    let before = identify(&r, true);
    for blank in [None, Some(""), Some("   ")] {
        let id = identify_reported(&r, true, SelfReport::of(blank));
        assert_eq!(id, before, "a blank report is byte-identical to no report: {blank:?}");
        assert_eq!(id.box_address(), "127.0.0.1:7879", "the dial address is all there is");
        assert!(!id.box_address_is_reported());
        assert_eq!(id.box_address_hover(), ADDRESS_HOVER);
    }
}

/// ⚠ **A report belongs to the LIVE CONNECTION, not to a record.** Painting the connected
/// daemon's address onto every registry row would read as completely plausible and would tell
/// an operator that three different boxes are all at one address.
#[test]
fn only_the_active_record_carries_the_daemons_report() {
    let active = record("the CI box", "127.0.0.1:7879");
    let other = record("staging", "127.0.0.1:7880");
    let report = SelfReport::of(Some("203.0.113.7:7879"));
    assert_eq!(report_for(&active, Some(&active), report), report, "the attached one does");
    assert_eq!(report_for(&other, Some(&active), report), None, "…and no other row does");
    assert_eq!(report_for(&active, None, report), None, "nor does any row with no connection");
}

/// The status line and the strip reach the same decision through one function, so they cannot
/// name two different boxes for one connection.
#[test]
fn the_status_line_and_the_strip_choose_the_same_address() {
    let r = record("the CI box", "127.0.0.1:7879");
    let reported = Some("203.0.113.7:7879");
    assert_eq!(shown_address("127.0.0.1:7879", reported), "203.0.113.7:7879");
    assert_eq!(
        identify_reported(&r, true, SelfReport::of(reported)).box_address(),
        shown_address(&r.addr, reported),
        "one decision, two surfaces"
    );
    for blank in [None, Some(""), Some("  ")] {
        assert_eq!(shown_address("127.0.0.1:7879", blank), "127.0.0.1:7879", "{blank:?}");
    }
}

/// A record with NO dial address and a reporting daemon still renders the daemon's address —
/// the two fields are independent, and [`NO_ADDRESS`] is only the fallback's fallback.
#[test]
fn a_report_answers_even_when_this_side_has_no_address_to_show() {
    let r = record("the CI box", "");
    assert_eq!(
        identify_reported(&r, true, SelfReport::of(Some("203.0.113.7:7879"))).box_address(),
        "203.0.113.7:7879"
    );
    assert_eq!(identify(&r, true).box_address(), NO_ADDRESS);
}

#[test]
fn the_window_title_leads_with_the_product_name() {
    let name = vike_ui_theme::brand::APP_NAME;
    assert_eq!(
        window_title(Some(&record("lab", "127.0.0.1:7879"))),
        format!("{name} — OBSERVING lab (127.0.0.1:7879)")
    );
    assert_eq!(window_title(None), format!("{name} — live"));
}
