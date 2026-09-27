//! **How a backend is IDENTIFIED on screen — one authority, so no surface can answer differently.**
//!
//! Every place the GUI says which daemon this process is attached to leads with the record's
//! NAME ([`crate::backend_registry::BackendRecord::name`], the operator's own display name,
//! sanitized on save) and treats the dial ADDRESS as secondary detail.
//!
//! # ⚠ The address is not an identity, and this module exists because it was being used as one
//!
//! MEASURED against the live the CI box daemon, 2026-09-15. The Connections foot strip read:
//!
//! ```text
//! Backend ● (--observe) 127.0.0.1:7879 control armed (not in registry)  [Disconnect] [Add backend]
//! ```
//!
//! and `127.0.0.1:7879` is what EVERY thin client shows, whatever backend it is attached to. Both
//! the CI box listeners bind loopback only (`LISTEN 127.0.0.1:7878`, `LISTEN 127.0.0.1:7879`), so an
//! SSH tunnel is the only route in — and through a tunnel the CLIENT-side address is always the
//! tunnel mouth. Two operators on two different production boxes therefore see the identical
//! string, and an operator who cannot tell which box they are attached to is one armed control
//! channel away from sending an order to the wrong daemon.
//!
//! The old headline `(--observe)` made it worse rather than better: it named the FLAG this process
//! was launched with, which is a property of this client and not of the backend at all.
//!
//! # The answer: the DAEMON says which box it is, and this module renders the claim
//!
//! The client cannot discover the far end's address from a tunnelled socket. The daemon can, and it
//! is already sending a frame — so it discovers its own source address from its kernel's routing
//! table and puts it on the wire
//! ([`vike_tradehub_client::wire::WireNodeIdentity::advertise_addr`], produced by
//! `crates/vike-tradehub/src/self_address.rs`). [`SelfReport`] is that value as this module takes
//! it, and [`identify_reported`] is how it composes:
//!
//! * a daemon that REPORTS an address — the report is what the row SHOWS, and the dial address
//!   moves into the hover. The tunnel mouth is not dimmed beside it, it is gone from the row: it
//!   says nothing, it is identical on every box, and leaving it in the row is the whole complaint.
//! * a daemon that reports NOTHING — an older node, or a box with no route to name a source from —
//!   renders EXACTLY as it did before this existed, dial address and all. Blankness is not evidence
//!   about the daemon, so nothing is inferred from it.
//!
//! ⚠ **A self-report is a CLAIM.** Nothing on this side verified it; a daemon could report
//! anything, and what it reports need not be reachable from here (a loopback-bound daemon names a
//! BOX, not an endpoint). [`REPORTED_HOVER`] says so at the point of confusion. Refusing to show it
//! would be the wrong answer to that — an unverified address that distinguishes two production
//! boxes beats a verified one that distinguishes nothing.
//!
//! ⚠ **And it is a different fact from the registry NAME.** The name is the operator's own label,
//! stored on this box; the address is the daemon answering for itself. They are rendered in
//! different positions and carry different hovers, and neither is ever derived from the other.
//!
//! # ⚠ What this module will NOT do: invent a name
//!
//! A record with no name gets [`UNNAMED_HEADLINE`] — the plain statement that it has none — and a
//! NOTE that reads as an invitation to give it one ([`NAME_THIS_BACKEND_NOTE`]), never a host
//! lookup, a reverse-DNS guess, or the address dressed up as a name. Where the operator has not
//! said which box this is, the honest answer is that nobody knows.
//!
//! The two facts are INDEPENDENT and are reported independently: a record can be named and absent
//! from `backends.json` (the registry was edited while the connection stood), and the synthetic
//! `--observe` record ([`crate::backend_conn::cli_observe_record`]) is both unnamed and unlisted.
//!
//! # Consumers
//!
//! `crates/vike-app-core/src/tool_views/connections.rs` (the foot strip, the picker rows and the
//! Backend settings header), `crates/vike-app-core/src/observe_bridge.rs`'s `observing_status`
//! (the status bar's `OBSERVING …` segment) and `crates/vike-desktop/src/main.rs`'s window title.
//! Pure — no egui, no I/O, no environment — so every string below is pinned by a headless test on
//! a box with no GPU.

use crate::backend_registry::BackendRecord;

/// The headline for a record that carries no name. Says what is true and nothing more.
pub const UNNAMED_HEADLINE: &str = "unnamed backend";

/// A record's address when it has none — a half-filled editor row renders too, and an empty gap
/// reads as a rendering bug rather than as a missing field.
pub const NO_ADDRESS: &str = "(no address)";

/// The note for an UNNAMED connection: an INVITATION, not a footnote. It names the button that
/// resolves it, because `(not in registry)` told an operator what was wrong and nothing about what
/// to do next.
pub const NAME_THIS_BACKEND_NOTE: &str = "not in the registry — Add backend to name this box";

/// The note for a NAMED record the registry does not hold (the file was edited while this
/// connection stood). The name already identifies the box, so this stays a footnote.
pub const UNLISTED_NOTE: &str = "not in the registry";

/// Hover text for the ADDRESS, wherever it is rendered: the one sentence that says why it is not
/// the identity.
pub const ADDRESS_HOVER: &str = "the CLIENT side of the link. A daemon that binds loopback is reached through an SSH tunnel, \
     and through a tunnel every backend reads 127.0.0.1 — so this address names no box. The NAME \
     is the identity.";

/// Hover text for an unnamed connection's headline.
pub const UNNAMED_HOVER: &str = "this backend has no name. It was dialled with --observe, which the registry does not record, \
     so nothing here can say which box it is — Add backend stores it under a name you choose.";

/// Hover text for a REPORTED address: what it is, and — the part that matters — what it is not.
///
/// Three claims, in the order a confused reader needs them: the daemon said this about itself,
/// nothing here checked it, and it is not necessarily somewhere you can connect to. The dial
/// address the row no longer shows is appended per-record by
/// [`BackendIdentity::box_address_hover`], because it is the one piece that differs per connection.
pub const REPORTED_HOVER: &str = "the daemon's own report of which box it is running on, sent with every frame. It is a CLAIM: \
     nothing on this side verified it, and a daemon that binds loopback names a box rather than an \
     address you can dial.";

/// **WHICH BOX the daemon says it is running on** — the borrowed, GUI-free view of
/// [`vike_tradehub_client::wire::WireNodeIdentity::advertise_addr`].
///
/// Constructed only through [`SelfReport::of`], which is where an EMPTY wire value is turned into
/// `None`: an empty string means "this daemon reported nothing" (an older node, a box with no route
/// to name a source address from), and the whole point of funnelling it through one constructor is
/// that no surface gets to decide for itself what an empty report means.
///
/// Takes `&str` rather than the wire type so this module keeps its "pure, no I/O, no egui, no wire"
/// property and stays testable without constructing a protocol struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelfReport<'a> {
    /// The address the daemon reported for itself. Never empty — see [`SelfReport::of`].
    pub addr: &'a str,
}

impl<'a> SelfReport<'a> {
    /// The daemon's report, or `None` when it made none.
    ///
    /// ⚠ Blank (empty or whitespace) is `None` and NOT a fact: it is what an old node sends, what a
    /// loopback-only container's failed route lookup produces, and what a daemon that could not
    /// name itself deliberately sends instead of `127.0.0.1`. A surface must render it as "nothing
    /// was said", which is what `None` makes it do.
    #[must_use]
    pub fn of(reported: Option<&'a str>) -> Option<Self> {
        reported.map(str::trim).filter(|a| !a.is_empty()).map(|addr| SelfReport { addr })
    }
}

/// How a backend is presented: the headline the surface LEADS with, the address as secondary
/// detail, and at most one note.
///
/// Borrows the record, so a caller renders without allocating per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendIdentity<'a> {
    /// What to lead with: the record's name, or [`UNNAMED_HEADLINE`].
    pub headline: &'a str,
    /// Whether [`BackendIdentity::headline`] is the operator's own name (`true`) or this module's
    /// stand-in for the absence of one (`false`). Drives emphasis and the hover.
    pub named: bool,
    /// The DIAL address — where this process connected. ⚠ Through a tunnel it is the tunnel mouth
    /// and names no box, which is why it is secondary when the daemon reported one of its own and
    /// leaves the row entirely ([`BackendIdentity::box_address`]).
    pub addr: &'a str,
    /// What the DAEMON said about which box it is, when it said anything.
    pub reported: Option<SelfReport<'a>>,
    /// [`NAME_THIS_BACKEND_NOTE`], [`UNLISTED_NOTE`], or nothing.
    pub note: Option<&'static str>,
    /// The note is an INVITATION (an action the operator should take) rather than a footnote, so a
    /// surface renders it at full weight instead of dimmed. True exactly for
    /// [`NAME_THIS_BACKEND_NOTE`].
    pub invitation: bool,
}

impl BackendIdentity<'_> {
    /// The address as rendered: [`NO_ADDRESS`] for a blank one.
    #[must_use]
    pub fn addr_label(&self) -> &str {
        if self.addr.trim().is_empty() { NO_ADDRESS } else { self.addr }
    }

    /// The hover for the headline: [`UNNAMED_HOVER`] when there is no name, else nothing (a name
    /// explains itself).
    #[must_use]
    pub fn headline_hover(&self) -> Option<&'static str> {
        (!self.named).then_some(UNNAMED_HOVER)
    }

    /// **THE ADDRESS THE ROW SHOWS** — the daemon's own report when it made one, else the dial
    /// address exactly as before.
    ///
    /// ⚠ The report REPLACES the dial address in the row rather than sitting beside it, and that
    /// is the decision this whole change turns on. `127.0.0.1:7879` is not merely less useful than
    /// the real address — it is identical on every box and for every daemon, so a reader who
    /// glances at the strip and sees it learns nothing and believes they learned something. It
    /// stays reachable on hover ([`BackendIdentity::box_address_hover`]), where a reader who wants
    /// to know what this process actually connected to can still find it.
    ///
    /// This is the ONE place that choice is made, so the foot strip, the registry rows and the
    /// status bar cannot answer differently.
    #[must_use]
    pub fn box_address(&self) -> &str {
        match self.reported {
            Some(r) => r.addr,
            None => self.addr_label(),
        }
    }

    /// Whether [`BackendIdentity::box_address`] is the DAEMON's claim (`true`) or this side's dial
    /// address (`false`). A surface uses it to weight the two differently — an address the box
    /// itself named is worth reading, a tunnel mouth is not.
    #[must_use]
    pub fn box_address_is_reported(&self) -> bool {
        self.reported.is_some()
    }

    /// The hover for [`BackendIdentity::box_address`].
    ///
    /// For a REPORTED address: [`REPORTED_HOVER`] plus the dial address the row gave up, so the
    /// fact is moved rather than lost. For the dial address itself: [`ADDRESS_HOVER`] unchanged,
    /// the sentence that says why an address is not an identity.
    #[must_use]
    pub fn box_address_hover(&self) -> String {
        match self.reported {
            Some(_) => format!("{REPORTED_HOVER} This process dialled {}.", self.addr_label()),
            None => ADDRESS_HOVER.to_string(),
        }
    }
}

/// **The address a surface should SHOW for a connection**, given the address this side dialled and
/// whatever the daemon reported — the same decision [`BackendIdentity::box_address`] makes, reached
/// from a status line that holds no [`BackendRecord`].
///
/// Exists so `crate::observe_bridge`'s `observing_status` cannot disagree with the Connections
/// strip about which address names the box. Blank reports are `None` by the same rule
/// [`SelfReport::of`] applies.
#[must_use]
pub fn shown_address<'a>(dial: &'a str, reported: Option<&'a str>) -> &'a str {
    match SelfReport::of(reported) {
        Some(r) => r.addr,
        None => dial,
    }
}

/// The headline for a record: its name, or [`UNNAMED_HEADLINE`] when it has none.
///
/// A name that is whitespace-only counts as absent — [`crate::backend_registry::sanitize_backend_name`]
/// already maps such a name to the empty string on save, and a record that reached the file another
/// way must not render as a blank gap.
#[must_use]
pub fn headline(record: &BackendRecord) -> &str {
    if record.name.trim().is_empty() { UNNAMED_HEADLINE } else { record.name.as_str() }
}

/// Does this record carry a name of the operator's own?
#[must_use]
pub fn is_named(record: &BackendRecord) -> bool {
    !record.name.trim().is_empty()
}

/// The whole presentation for a record NOTHING has reported about — a registry row for a backend
/// this process is not attached to, or a connection whose daemon sent no address.
///
/// [`identify_reported`] is the same call with the daemon's self-report; this one is it with
/// `None`, kept as its own name because most callers genuinely have no report to offer and
/// threading a `None` through them would read as an oversight rather than a fact.
#[must_use]
pub fn identify(record: &BackendRecord, listed: bool) -> BackendIdentity<'_> {
    identify_reported(record, listed, None)
}

/// The whole presentation, INCLUDING what the daemon said about which box it is.
///
/// `listed` is whether `backends.json` holds this record
/// ([`crate::backend_conn::PickerRow::listed`], or `picker.backends.backends.contains(record)` for
/// the live connection).
///
/// ⚠ `reported` belongs to the LIVE CONNECTION, never to a record. A registry row is a stored
/// address and a name; only the backend this process is attached to has a daemon on the other end
/// saying anything. [`report_for`] is the filter that keeps a report from being painted onto a row
/// whose daemon never spoke.
#[must_use]
pub fn identify_reported<'a>(
    record: &'a BackendRecord,
    listed: bool,
    reported: Option<SelfReport<'a>>,
) -> BackendIdentity<'a> {
    let named = is_named(record);
    // The UNNAMED case wins the note: "you cannot tell which box this is" is the failure being
    // fixed, and it outranks "the file does not hold this row". An unnamed record is unlisted by
    // construction anyway (`cli_observe_record` is never persisted), so the arms do not compete in
    // practice — the order is stated so a future named-but-unlisted row cannot silently claim the
    // invitation.
    let (note, invitation) = match (named, listed) {
        (false, _) => (Some(NAME_THIS_BACKEND_NOTE), true),
        (true, false) => (Some(UNLISTED_NOTE), false),
        (true, true) => (None, false),
    };
    BackendIdentity {
        headline: headline(record),
        named,
        addr: record.addr.as_str(),
        reported,
        note,
        invitation,
    }
}

/// The self-report that belongs to `record` — `reported` when this record IS the live connection,
/// `None` otherwise.
///
/// ⚠ The check is the whole function, and it is here rather than at a call site because getting it
/// wrong is invisible: painting the connected daemon's address onto every registry row would look
/// completely plausible and would tell an operator that three different boxes are all at one
/// address. A backend this process is not attached to has nobody speaking for it.
#[must_use]
pub fn report_for<'a>(
    record: &BackendRecord,
    active: Option<&BackendRecord>,
    reported: Option<SelfReport<'a>>,
) -> Option<SelfReport<'a>> {
    if active == Some(record) { reported } else { None }
}

/// The ONE-LINE rendering, for surfaces with no layout of their own — the OS window title, a log
/// line, a status string: `the CI box (127.0.0.1:7879)`, or `unnamed backend (127.0.0.1:7879)`.
///
/// The name still LEADS; the parenthesis is what makes the address read as detail in a medium that
/// has no dimmer colour to say it with.
///
/// ⚠ **This one shows the DIAL address even when the daemon reports its own, and that is a
/// constraint rather than an oversight.** Its only caller is `crates/vike-desktop/src/main.rs`'s
/// window title, which is built into `eframe::NativeOptions` BEFORE the event loop starts — so no
/// connection exists yet, no frame has landed, and there is no report to show. Giving it one would
/// mean a title that updates itself per frame, which is a different change with its own cost. The
/// strip, the picker rows and the status bar all carry the real address the moment the first frame
/// lands, and they are the surfaces an operator reads while working.
#[must_use]
pub fn one_line(record: &BackendRecord) -> String {
    let addr = record.addr.trim();
    let addr = if addr.is_empty() { NO_ADDRESS } else { addr };
    format!("{} ({addr})", headline(record))
}

/// The label a STATUS line leads with, or `None` when the record has no name — a status line has
/// no room for an invitation, and [`UNNAMED_HEADLINE`] in front of an address it already shows
/// would be noise. The strip is where the invitation belongs.
#[must_use]
pub fn status_label(record: &BackendRecord) -> Option<&str> {
    is_named(record).then(|| record.name.trim())
}

#[cfg(test)]
mod tests {
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
}
