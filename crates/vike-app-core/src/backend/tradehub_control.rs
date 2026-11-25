//! `tradehub_control` — the GUI side of the thin-client **Scope::Write** WRITE path (headless two-layer
//! plan, Layer 2): the desktop's observer (every `vike-desktop` launch; a `vike-app --observe` run when this
//! was written) sends a live-core [`vike_execution::Command`] to a remote headless `vike-tradehub`
//! daemon's control server, lifted to the thin-wire [`vike_tradehub_client::wire::CommandWire`] by
//! [`vike_tradehub_client::command_lift::wire_from_command`] (the exact INVERSE of the daemon's
//! `lower_command`, and deliberately PARTIAL: a `Command` with no wire form yields `None`, and
//! [`send_to_backend`] logs it and latches a status-strip line saying it was not sent). The lift lived here
//! until it moved down beside the wire with the observe decode (D56); the send, the routing gate's call and
//! every status-strip word stay.
//!
//! [`vike_tradehub_client::venue_routing::venue_routing_verdict`] is the CLIENT half of the node's
//! routing gate (`vike-cli trade` asks the same one), and it is the only defence against the one
//! direction the node cannot cover: a backend that predates
//! [`vike_tradehub_client::proto::FEATURE_VENUE_ROUTING`] accepts a command naming ANY venue and
//! applies it to its PRIMARY engine — `Ack`, an order in the snapshot, no error — so an operator
//! picking a venue sees success and the order is signed on a different exchange. Against such a
//! backend this client sends only a venue the backend has been seen to publish
//! (`backend_engine_venues`, the roster this desktop's snapshot shows).
//!
//! [`send_to_backend`] is what a shell actually calls — the lift, the routing gate, the write and
//! every log line and status-strip line live here rather than in `crates/vike-desktop`, which is in
//! `EXCLUDE_FROM_CI` and so is compiled by one job and executed by nothing; the shell keeps only the
//! handle and the frame's snapshot. It is applied at `vike-desktop`'s one remote-command choke point
//! (`CommandDispatch::send`), never per button.
//!
//! The master gate for wiring this path at all is the `flags.tradehub_control` row of the PC's own
//! settings database — the same row the daemon reads on its box — which the desktop binary reads
//! after its boot and hands to `crate::backend::connection`'s `connect_backend` /
//! `switch_backend` as `master_control`; this crate reads no environment for it (decision 0111:
//! the `VIKE_TRADEHUB_CONTROL` variable refuses startup). ON is the default since the owner's ruling
//! of 2026-11-12 (decision 0280); a `false` row keeps the observer read-only, byte-identical to
//! before this path existed.

/// **The venues a backend publishes an ENGINE for**, off the snapshot the GUI is already
/// rendering — `vike_execution::Portfolio::venues` is documented as "one block per engine", so this IS
/// the backend's engine roster and not an approximation of it. It is the client-side routing
/// gate's positive evidence, and the twin of `vike-tradehub`'s own
/// `PublisherHandle::engine_venues`, which reads the same projection one hop earlier.
///
/// EMPTY means the observer has not received a frame yet, NOT that the backend runs no engines —
/// [`vike_tradehub_client::venue_routing::venue_routing_verdict`] treats it as "I know nothing", which against a backend that does not
/// check addresses is a refusal.
pub(crate) fn backend_engine_venues(snapshot: &vike_execution::CoreSnapshot) -> Vec<String> {
    snapshot.portfolio.venues.iter().map(|v| v.venue.clone()).collect()
}

/// **The ONE call a GUI shell makes to write a command to a backend**: the lift
/// ([`vike_tradehub_client::command_lift::wire_from_command`]), the routing gate
/// ([`vike_tradehub_client::venue_routing::may_send_to_backend`]), then `write`, the shell's
/// handle (`TradehubControlHandle::try_command`). A command it does NOT send is logged here and its
/// line handed to `latch` — the shell's `TradehubControlHandle::latch_client_refusal` — and it returns
/// `None`, so no caller can drop that line. A command it DID write returns `write`'s ticket, which
/// the shell keeps to read the node's answer to an order (`crate::orders::node_refusals`).
///
/// ⚠ **A command the client does not send must SAY so on screen** (I-1 of the node-bracket
/// pre-flight), and only the LATCH can say it. The sharpest case is the normal one: a desktop built
/// from `main` against a node at the last release, which predates
/// `vike_tradehub_client::proto::FEATURE_BRACKET`. There the TP/SL toggle is enabled
/// ([`vike_tradehub_client::command_lift::bracket_has_wire_form`] describes the client), and `write` refuses
/// the bracket
/// (`ControlRejected::UnsupportedByHub`), which latches nothing by itself: the worker never sees a
/// command it was never handed. With no client gate, the old node would have answered "undecodable
/// request" and latched THAT, so the gate made the failure LESS visible.
///
/// ⚠ **Why not the desktop's own status line (C-1 of the Task 3 review).** The first fix returned
/// the line for the shell to paint into `app.status`. The desktop's frame runs
/// `crate::ui::core_sync::sync_from_core` FIRST, and it ends with an unconditional write of that
/// same line; then the strip paints; then the dispatch runs. A line the dispatch wrote was
/// therefore gone before any strip showed it. The latch is the channel the fold never touches,
/// which the control segment (`access_line`) paints until the operator dismisses it —
/// pinned through that real order by `crates/vike-app-core/src/ui/fold_tests.rs`'s
/// `a_client_side_refusal_survives_the_next_frames_status_rewrite`.
///
/// Every line is short enough to survive that segment's `MAX_ERR_TAIL` clip whole, except the
/// routing refusal's paragraph, which leads with the venue it names so that survives.
///
/// `write` and `latch` are closures so the whole decision is tested here with stubs, rather than in
/// the shell, which no test executes.
pub fn send_to_backend<T>(
    command: &vike_execution::Command,
    snapshot: &vike_execution::CoreSnapshot,
    node_routes_by_venue: bool,
    write: impl FnOnce(
        vike_tradehub_client::wire::CommandWire,
    ) -> Result<T, vike_tradehub_client::ControlRejected>,
    latch: impl FnOnce(String),
) -> Option<T> {
    let Some(wire) = vike_tradehub_client::command_lift::wire_from_command(command) else {
        tracing::warn!("remote control: command has no wire form yet; dropped");
        latch(format!(
            "{} NOT sent: the hub protocol has no form for it",
            unliftable_name(command)
        ));
        return None;
    };
    if let Err(why) = vike_tradehub_client::venue_routing::may_send_to_backend(
        &wire,
        &backend_engine_venues(snapshot),
        node_routes_by_venue,
    ) {
        // One line per refused command: an operator who pressed a button and saw no order needs the
        // reason, and this path is operator-cadence, never a fold.
        let venue = wire.addressed_venue().unwrap_or_default();
        tracing::warn!(%venue, "remote control: command NOT SENT — {why}");
        // The reason runs to a paragraph, and the segment clips it: lead with what it is about.
        let head = match wire.addressed_venue().map(str::trim).filter(|v| !v.is_empty()) {
            Some(venue) => format!("command for `{venue}`"),
            None => "command".to_string(),
        };
        latch(format!("{head} NOT sent: {why}"));
        return None;
    }
    match write(wire.clone()) {
        Ok(ticket) => Some(ticket),
        Err(refused) => {
            tracing::warn!(?refused, "remote control: command NOT SENT, refused before the wire");
            latch(refused_send_status(&wire, refused));
            None
        }
    }
}

/// What a refusal line calls a command [`vike_tradehub_client::command_lift::wire_from_command`] cannot lift.
/// Named for the ones a GUI
/// button produces (m-d of the Task 3 review: an operator who pressed the DOM's cancel-all must
/// learn it was the CANCEL that did not go); anything else is "command".
fn unliftable_name(command: &vike_execution::Command) -> String {
    use vike_execution::{Command, OrderIntent};
    let orders = |n: usize| if n == 1 { "1 order".to_string() } else { format!("{n} orders") };
    match command {
        Command::Order(OrderIntent::CancelBatch(order_client_ids)) => {
            format!("cancel of {}", orders(order_client_ids.len()))
        }
        Command::Order(OrderIntent::SubmitBatch(reqs)) => {
            format!("batch of {}", orders(reqs.len()))
        }
        Command::SetMargin(_) => "leverage change".to_string(),
        _ => "command".to_string(),
    }
}

/// The status-strip line for a command the CLIENT refused before the wire — what
/// [`send_to_backend`] latches for an `Err` from the shell's `write`.
///
/// Only a capability refusal tells the operator to upgrade the node: a full queue and a closed link
/// are facts about this connection, not about the node's version. A refused BRACKET names the word
/// it needs (`vike_tradehub_client::proto::FEATURE_BRACKET`), the case the TP/SL ticket meets
/// against a node at the last release. Any other capability refusal says the same without naming a
/// word: which word a verb needs is `vike_tradehub_client::tradehub_control`'s `required_feature`,
/// and this line does not restate it.
///
/// The full-queue line is in the PAST tense on purpose: it is latched like the rest, so after a
/// retry that went through it must read as what happened, never as the state of the link. Every
/// line fits the control segment's `MAX_ERR_TAIL` whole.
pub fn refused_send_status(
    command: &vike_tradehub_client::wire::CommandWire,
    refused: vike_tradehub_client::ControlRejected,
) -> String {
    use vike_tradehub_client::ControlRejected;
    match (refused, command) {
        (
            ControlRejected::UnsupportedByHub,
            vike_tradehub_client::wire::CommandWire::Bracket(spec),
        ) => {
            // A bracket that NAMES an account owes the account word (`required_feature`'s arm order).
            let word = if spec.account.is_some() {
                vike_tradehub_client::proto::FEATURE_BRACKET_ACCOUNT
            } else {
                vike_tradehub_client::proto::FEATURE_BRACKET
            };
            format!("TP/SL bracket NOT sent: this node predates `{word}` — upgrade the node")
        }
        (ControlRejected::UnsupportedByHub, _) => {
            "command NOT sent: this node predates it — upgrade the node".to_string()
        }
        (ControlRejected::Busy, _) => {
            "a command was NOT sent: the queue to the node was full".to_string()
        }
        (ControlRejected::Gone, _) => {
            "command NOT sent: the control link to the node closed".to_string()
        }
    }
}

/// The longest latched-refusal tail rendered inline in the one-line status summary — a node's
/// `Response::Error` or a refusal the client latched itself ([`send_to_backend`]); either can be
/// verbose, so it is truncated with an ellipsis to keep the status strip a single tidy line (the
/// full text stays in the tracing log).
const MAX_ERR_TAIL: usize = 80;

/// What this desktop may do on the node it is connected to: the role the node's access list holds for this
/// desktop's device (decision 0288: `observe`, `control`, `admin`).
///
/// The node SAYS it: a device's `AuthOk` carries its row's role (`vike_hub_protocol::handshake::Admitted`), read
/// by `control_access`. Without a control channel the connection is read-only, [`BackendAccess::Observe`], whatever
/// the role; a node older than the field says nothing, and a channel it let through is then [`BackendAccess::Control`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendAccess {
    Observe,
    Control,
    Admin,
}

/// Whether this desktop can send orders at all — the ONE value the shell hands the Trade window's
/// glue for it (the owner's decision of 10-03, item 11), read off the control handle with
/// [`BackendControlLink::of`]. The status bar's control dot reads the same states (`Open` / `Lost`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendControlLink {
    /// A connected control channel: orders go to the node.
    Open,
    /// No control channel (the Trade window's `READ_ONLY_WHY`).
    ReadOnly,
    /// A control channel whose link has closed (the Trade window's `NO_CONTROL_LINK_WHY`).
    Lost,
}

impl BackendControlLink {
    /// The link a control handle gives: `None` (no handle) is read-only, a handle reports whether it
    /// is still connected (`TradehubControlHandle::is_connected`).
    pub fn of(handle_connected: Option<bool>) -> BackendControlLink {
        match handle_connected {
            None => BackendControlLink::ReadOnly,
            Some(true) => BackendControlLink::Open,
            Some(false) => BackendControlLink::Lost,
        }
    }
}

/// A control channel's access and WHO, from the node's answer at connect (`TradehubControlHandle::admitted`):
/// the role it said (`admin` → [`BackendAccess::Admin`]) with `person/device`, or, from an older node that says
/// nothing, [`BackendAccess::Control`] and no name (the channel itself proves Write).
pub(crate) fn control_access(
    admitted: Option<&vike_hub_protocol::handshake::Admitted>,
) -> (BackendAccess, Option<String>) {
    match admitted {
        Some(a) => {
            let admin = a.role == vike_hub_protocol::auth::Scope::Account.name();
            let access = if admin { BackendAccess::Admin } else { BackendAccess::Control };
            (access, Some(format!("{}/{}", a.person, a.device)))
        }
        None => (BackendAccess::Control, None),
    }
}

/// THE role words, the one producer every surface reads (the status bar's [`access_line`], the window title,
/// the connect log line): `OBSERVING`, `CONTROL` or `ADMIN`, a writing role followed by who when the node said
/// it — `ADMIN (owner/vike)`.
pub(crate) fn access_label(access: BackendAccess, who: Option<&str>) -> String {
    let word = match access {
        BackendAccess::Observe => return "OBSERVING".to_string(),
        BackendAccess::Control => "CONTROL",
        BackendAccess::Admin => "ADMIN",
    };
    match who.map(str::trim).filter(|w| !w.is_empty()) {
        Some(who) => format!("{word} ({who})"),
        None => word.to_string(),
    }
}

/// The ACCESS half of the status bar's one backend item: the words after the connection line
/// (`crate::backend::observe_bridge::observing_status`), which already names the daemon, so these never repeat it.
/// Kept pure (no handle, no egui) so it is unit-tested in CI-covered `vike-app-core`; the shell reads the handle's
/// `is_connected`, LATCHED `last_error` and `admitted` and paints the returned words.
///
/// Shapes (a writing role is loud: it can drive REAL orders on the daemon, the words say so, the status bar paints
/// them the armed amber and leads them with the warning icon — `crate::ui::status_bar::dot::control_text`):
/// - observe → `"OBSERVING"` (plain: the connection is read-only)
/// - a writing role, connected → [`access_label`] + `": can place REAL orders"`: `"ADMIN (owner/vike): can place
///   REAL orders"`, or `"CONTROL: can place REAL orders"` from a node that does not say the role
/// - disconnected → [`access_label`] + `" disconnected"`
/// - a writing role with a latched error → `… + " · last error: <tail>"`, a long one cut to [`MAX_ERR_TAIL`] chars +
///   `"…"` (the full text is in the log)
///
/// ⚠ `last_error` is the handle's LATCHED banner view — it names no command and is cleared only by
/// [`vike_tradehub_client::TradehubControlHandle::clear_last_error`] (the GUI wires that to a click on
/// these words). It must never be used to report the outcome of a particular command: that is what
/// [`vike_tradehub_client::TradehubControlHandle::await_outcome`] and the `CommandTicket` a send
/// returns are for. Reading the latch per-command is what made every write after one refusal report
/// that refusal, including commands the node executed.
pub(crate) fn access_line(
    access: BackendAccess,
    who: Option<&str>,
    connected: bool,
    last_error: Option<&str>,
) -> String {
    // No warning glyph in the text: an icon cannot live inside a string (`vike_ui_theme::icons`'
    // module doc), so the status bar leads a writing role's words with `icons::WARNING` itself.
    let label = access_label(access, who);
    let mut s = match (access, connected) {
        (BackendAccess::Observe, _) => return label,
        (_, true) => format!("{label}: can place REAL orders"),
        (_, false) => format!("{label} disconnected"),
    };
    if let Some(err) = last_error.map(str::trim).filter(|e| !e.is_empty()) {
        let tail = if err.chars().count() > MAX_ERR_TAIL {
            let cut: String = err.chars().take(MAX_ERR_TAIL).collect();
            format!("{cut}…")
        } else {
            err.to_string()
        };
        s.push_str(" · last error: ");
        s.push_str(&tail);
    }
    s
}

/// The ONE line a control connect logs, at INFO (a writing role on a node whose record asks for control is the
/// normal connect, not a warning): who the node admitted and the role it holds, from its `AuthOk` —
/// `connected as owner/vike, role admin: can place REAL orders`. From a node that does not say it, the device key's
/// registered `person/device` (`registered_as`) and the CONTROL words; `connected with this desktop's device key: …`
/// when the name is not known.
pub(crate) fn connected_line(
    admitted: Option<&vike_hub_protocol::handshake::Admitted>,
    registered_as: Option<&str>,
) -> String {
    if let Some(a) = admitted {
        return format!(
            "connected as {}/{}, role {}: can place REAL orders",
            a.person, a.device, a.role
        );
    }
    let words = access_line(BackendAccess::Control, None, true, None);
    match registered_as {
        Some(who) => format!("connected as {who}: {words}"),
        None => format!("connected with this desktop's device key: {words}"),
    }
}

#[path = "tradehub_control_tests.rs"]
#[cfg(test)]
mod tradehub_control_tests;
