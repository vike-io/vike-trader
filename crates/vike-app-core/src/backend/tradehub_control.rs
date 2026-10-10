//! `tradehub_control` — the GUI-side lowering for the thin-client **Scope::Write** WRITE path
//! (headless two-layer plan, Layer 2): turn a live-core [`vike_exec::Command`] into the thin-wire
//! [`vike_tradehub_client::wire::WireCommand`] the desktop's observer (every `vike-desktop` launch; a
//! `vike-app --observe` run when this was written) sends to a remote headless `vike-tradehub`
//! daemon's control server.
//!
//! [`wire_from_command`] is the exact INVERSE of the daemon's `lower_command`
//! (`vike-tradehub/src/server.rs`, which lowers a received `WireCommand` back into a real
//! `Command`/`OrderIntent` at its edge). It is deliberately **PARTIAL**: the thin wire vocabulary
//! ([`vike_tradehub_client::wire::WireCommand`]) is only the SESSION-relevant subset a remote GUI drives
//! (submit / cancel / modify / mass-cancel / flatten / market-exit / trading-state, the TP/SL
//! bracket since the node-side bracket command, plus the strategy-level live-params re-tune since
//! split-plane B4), NOT the full internal `Command`/`OrderIntent` surface (batches, conditionals,
//! combos, margin, reconcile plumbing, shutdown). A `Command` with no wire form yields `None`; the
//! caller ([`send_to_backend`]) logs it and latches a status-strip line saying it was not sent — a
//! future PR either extends `WireCommand` or grays out the UI control that produces it.
//!
//! [`venue_routing_verdict`] is the CLIENT half of the node's routing gate, and it is the only
//! defence against the one direction the node cannot cover: a backend that predates
//! [`vike_tradehub_client::proto::FEATURE_VENUE_ROUTING`] accepts a command naming ANY venue and
//! applies it to its PRIMARY engine — `Ack`, an order in the snapshot, no error — so an operator
//! picking a venue sees success and the order is signed on a different exchange. Against such a
//! backend this client sends only a venue the backend has been seen to publish
//! ([`may_send_to_backend`]).
//!
//! [`send_to_backend`] is what a shell actually calls — the lift, the routing gate, the write and
//! every log line and status-strip line live here rather than in `crates/vike-desktop`, which is in
//! `EXCLUDE_FROM_CI` and so is compiled by one job and executed by nothing; the shell keeps only the
//! handle and the frame's snapshot. It is applied at `vike-desktop`'s one remote-command choke point
//! (`Dispatch::send`), never per button.
//!
//! The master gate for wiring this path at all is the `flags.tradehub_control` row of the PC's own
//! settings database — the same row the daemon reads on its box — which the desktop binary reads
//! after its boot and hands to `crate::backend::backend_conn`'s `connect_backend` /
//! `switch_backend` as `master_control`; this crate reads no environment for it (decision 0111:
//! the `VIKE_TRADEHUB_CONTROL` variable refuses startup). Off — the default — means the observer,
//! every desktop launch, stays read-only, byte-identical to before this path existed.

/// Lower a live-core [`vike_exec::Command`] into the thin-wire [`vike_tradehub_client::wire::WireCommand`]
/// a remote Scope::Write client sends — the GUI-side inverse of the daemon's `lower_command`.
///
/// PARTIAL (see the module doc): every mapped variant copies its fields into the standalone wire
/// mirror verbatim (a Submit copies every `OrderRequest` field the wire carries, its account in the
/// wire spelling; an
/// `UpdateParams` copies its target key and mount id and re-serializes the typed `StrategyParams`
/// into the core's own serde JSON — the shape the wire deliberately delegates to); everything with
/// no wire form — `OrderIntent::{SubmitBatch, CancelBatch, Confirm, ArmConditional,
/// DisarmConditional, Combo}` and `Command::{SetMargin, ApplySnapshot,
/// ReconcileReports, ConfirmRecon, Shutdown}` — returns `None`.
///
/// ⚠ An `UpdateParams` keeps its `mount_id` verbatim, and WHETHER to name one is the producer's call:
/// send it whenever the node advertises [`vike_tradehub_client::proto::FEATURE_PARAMS_BY_MOUNT`]
/// (its status rows then carry the id to send), because two mounts can share a series and an
/// unaddressed update on one is refused by the core. Against a node WITHOUT the capability the
/// client write path refuses an addressed update locally (`ControlRejected::UnsupportedByNode`,
/// nothing sent) rather than let that node retune the first mount on the series.
///
/// ⚠ The gate covers ADDRESSED updates only. An UNADDRESSED update from a new client to an older
/// node (one without `strategy-params-mount`) is sent as before, and on a series two mounts share
/// that node still retunes the FIRST mount - it predates the core's refusal of an ambiguous update.
/// A producer that can name a mount should do so whenever the node advertises the capability, and
/// should not rely on the unaddressed shape against a node that does not.
pub fn wire_from_command(
    cmd: &vike_exec::Command,
) -> Option<vike_tradehub_client::wire::WireCommand> {
    use vike_exec::{Command, OrderIntent, TradingState};
    use vike_tradehub_client::wire::WireBracketSpec;
    use vike_tradehub_client::wire::{WireCommand, WireOrderRequest, WireTradingState};

    match cmd {
        Command::Order(OrderIntent::Submit(req)) => Some(WireCommand::Submit(WireOrderRequest {
            client_order_id: req.client_order_id.clone(),
            venue: req.venue.clone(),
            symbol: req.symbol.clone(),
            side: req.side,
            qty: req.qty,
            order_type: req.order_type.clone(),
            price: req.price,
            trigger_price: req.trigger_price,
            reduce_only: req.reduce_only,
            // ⚠ The account rides across in the wire spelling (`Display`: `DEFAULT` for the unlabelled
            // book, as the `MountStrategy` arm below argues). Hard-coding `None` here dropped it, and
            // an account-less submit on a venue that runs two engines is refused as ambiguous. A node
            // older than account-scoped submits is refused client-side, never misrouted
            // (`vike_tradehub_client::remote_control`'s `required_feature`).
            account: req.account.as_ref().map(|a| a.to_string()),
        })),
        // The TP/SL bracket: `BracketSpec` field for field. It carries NO account, and that is the
        // cut rather than an omission: `BracketSpec` has no account field, so the node takes a
        // bracket only on a venue whose one account is the default one, and refuses every other
        // shape before its Ack (`vike_tradehub`'s `account_refusal`).
        //
        // ⚠ A non-finite `entry_price` serializes as `null`, which the node reads as a MARKET entry,
        // so the only guard is the sender's: the one producer of this intent in the GUI is
        // `crates/vike-app-core/src/orders/order_dispatch.rs`'s `admit_bracket`, which validates
        // every leg first and refuses a non-finite price. This arm copies; it does not judge.
        Command::Order(OrderIntent::Bracket(spec)) => Some(WireCommand::Bracket(WireBracketSpec {
            venue: spec.venue.clone(),
            symbol: spec.symbol.clone(),
            side: spec.side,
            qty: spec.qty,
            entry_price: spec.entry_price,
            stop_loss: spec.stop_loss,
            take_profit: spec.take_profit,
        })),
        Command::Order(OrderIntent::Cancel(coid)) => Some(WireCommand::Cancel(coid.clone())),
        Command::Order(OrderIntent::Modify { client_order_id, new_qty, new_price }) => {
            Some(WireCommand::Modify {
                client_order_id: client_order_id.clone(),
                new_qty: *new_qty,
                new_price: *new_price,
            })
        }
        // The three risk-REDUCING verbs carry their account across the lift, in the wire spelling
        // (`Display` — `DEFAULT` for the unlabelled book, as the `MountStrategy` arm below argues).
        // ⚠ Dropping it here would WIDEN the verb on the far side: an account-less reduce fans out
        // over every account of the venue, so a lift to `None` turns "cancel ALT's book" into
        // "cancel the exchange". A node that does not honour the field refuses nothing — the client
        // refuses the send against it (`vike_tradehub_client::remote_control`'s `required_feature`).
        Command::Order(OrderIntent::MassCancel { venue, symbol, account }) => {
            Some(WireCommand::MassCancel {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.as_ref().map(|a| a.to_string()),
            })
        }
        Command::Order(OrderIntent::Flatten { venue, symbol, account }) => {
            Some(WireCommand::Flatten {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: account.as_ref().map(|a| a.to_string()),
            })
        }
        Command::Order(OrderIntent::MarketExit { venue, account }) => {
            Some(WireCommand::MarketExit {
                venue: venue.clone(),
                account: account.as_ref().map(|a| a.to_string()),
            })
        }
        Command::SetTradingState(ts) => Some(WireCommand::SetTradingState(match ts {
            TradingState::Active => WireTradingState::Active,
            TradingState::Reducing => WireTradingState::Reducing,
            TradingState::Halted => WireTradingState::Halted,
        })),
        // The strategy-level live-params re-tune (split-plane B4): the target key copies verbatim;
        // the typed `StrategyParams` re-serializes into the core's OWN serde JSON, which is exactly
        // what the wire variant carries (delegated, not mirrored — see its doc) and what the
        // daemon's `lower_command` deserializes back. `to_value` on these serde-derive params
        // structs cannot fail in practice; `.ok()?` keeps the function total rather than panicking
        // a GUI thread on a hypothetical unserializable future variant.
        Command::UpdateParams(u) => Some(WireCommand::UpdateParams {
            venue: u.venue.clone(),
            symbol: u.symbol.clone(),
            interval: u.interval.clone(),
            mount_id: u.mount_id.clone(),
            params: serde_json::to_value(&u.params).ok()?,
        }),
        // The runtime mount verbs (split-plane B5): the spec is fully serde, so the lift is a
        // field-for-field copy — the wire variant mirrors `vike_exec::MountSpec` exactly.
        Command::MountStrategy(spec) => Some(WireCommand::MountStrategy {
            venue: spec.venue.clone(),
            // ⚠ **`Display`, not `text()`** — and the difference is a whole wire state. `text()`
            // answers `None` for the DEFAULT account, which would flatten "named the unlabelled
            // account" into "named nothing"; `Display` renders it `DEFAULT`, the spelling
            // `parse_wire_account` reads back on the far side. The two are different rows of the
            // routing table at `N >= 2`, so collapsing them here would lose the operator's choice
            // between the lift and the frame.
            account: spec.account.as_ref().map(|a| a.to_string()),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            controller_id: spec.controller_id.clone(),
            name: spec.name.clone(),
            rhai: spec.rhai.clone(),
            params: spec.params.clone(),
        }),
        Command::UnmountStrategy { controller_id } => {
            Some(WireCommand::UnmountStrategy { controller_id: controller_id.clone() })
        }
        // No thin-wire form yet (see the module doc) — the caller logs these and reports them
        // as not sent:
        //   OrderIntent::{SubmitBatch, CancelBatch, Confirm, ArmConditional,
        //                 DisarmConditional, Combo}
        //   Command::{SetMargin, ApplySnapshot, ReconcileReports, ConfirmRecon, Shutdown}
        _ => None,
    }
}

/// Whether a TP/SL bracket has a wire form — DERIVED from [`wire_from_command`] itself (it lifts a
/// sample bracket and asks), so the Trade window's ticket enables its TP/SL toggle the day a wire
/// form lands and nothing here needs flipping (Ruling R8 of the Trade window plan).
///
/// The answer is a property of the BUILD, so it is computed once and cached: the Trade window reads
/// it every frame, and the lift allocates two strings per call.
///
/// ⚠ It says what THIS client can lift, never what the connected node can take: against a node that
/// predates `vike_tradehub_client::proto::FEATURE_BRACKET` the client refuses the send, and
/// [`send_to_backend`] latches the status-strip line that says so.
pub fn bracket_has_wire_form() -> bool {
    static LIFTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LIFTS.get_or_init(|| {
        use vike_exec::{Command, OrderIntent};
        wire_from_command(&Command::Order(OrderIntent::Bracket(Box::new(
            vike_model::BracketSpec {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                entry_price: None,
                stop_loss: 1.0,
                take_profit: 2.0,
            },
        ))))
        .is_some()
    })
}

/// What a client should do with a command it is about to send, given WHICH VENUE that command
/// names and what the connected node has said about itself — the answer to
/// [`venue_routing_verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueRouting {
    /// Send it. Either this node publishes an engine for that venue, or it advertises
    /// [`vike_tradehub_client::proto::FEATURE_VENUE_ROUTING`] and will answer a `Response::Error`
    /// naming its own roster — both of which are honest outcomes.
    Send,
    /// Do NOT send it; show the operator this reason instead. The node would accept the frame and
    /// apply it to a book the operator did not name.
    Refuse(String),
}

/// **May this client send a command addressed to `venue`?** — the client half of the routing fix,
/// and the half that covers the direction the server half cannot.
///
/// # The direction this exists for
///
/// A node that advertises `FEATURE_VENUE_ROUTING` refuses a venue it runs no engine for, so a
/// client talking to one can offer the operator anything and let the node answer. A node that does
/// NOT advertise it takes `vike_core`'s historical `route_of(..).unwrap_or(0)`: it accepts the
/// command, applies it to its PRIMARY engine, answers `Ack`, and shows the order in the snapshot.
/// The operator picks a venue, sees no error, and the order is signed on a different exchange.
/// Nothing on the wire distinguishes that from success, which is why the check has to happen before
/// the frame is written and why it cannot be a check on the reply.
///
/// # The rules, and which way each one leans
///
/// * a BLANK venue is refused — an empty string is not a venue, and the node's routing would fall
///   to engine 0 on every build, advertised or not (this is the same hole
///   [`crate::orders::order_dispatch::DispatchRejectReason::NoRoutableMarket`] closes for the snapshot's
///   own empty placeholder, restated here because this function is reachable from paths that never
///   pass through the planner);
/// * a venue the node PUBLISHES an engine for is sent — that is positive evidence, and it is the
///   only evidence available against an old node;
/// * otherwise, if the node advertises the capability, it is sent — the node can say no, and it
///   knows more than this client does: `node_venues` comes from a pushed snapshot that lags a
///   runtime `MountStrategy`, so a client that refused here would block a venue the node had since
///   acquired. **This is the one rule that leans toward sending**, and it leans there because the
///   worst case is a clean refusal from the node rather than a misroute;
/// * otherwise it is REFUSED locally, naming both the venue asked for and what the node publishes.
///
/// ⚠ `node_venues` EMPTY plus no capability is the most conservative case and is refused: an
/// observer that has not yet received a frame knows nothing about the node, and "I know nothing"
/// must never read as "anything goes" on a path that signs orders.
pub fn venue_routing_verdict(
    venue: &str,
    node_venues: &[String],
    node_routes_by_venue: bool,
) -> VenueRouting {
    let venue = venue.trim();
    if venue.is_empty() {
        return VenueRouting::Refuse(
            "this command names no venue, so the backend would apply it to whichever book it \
             happens to run first"
                .to_string(),
        );
    }
    if node_venues.iter().any(|v| v == venue) {
        return VenueRouting::Send;
    }
    if node_routes_by_venue {
        return VenueRouting::Send;
    }
    let known = if node_venues.is_empty() {
        "it has published none yet".to_string()
    } else {
        format!("it publishes: {}", node_venues.join(", "))
    };
    VenueRouting::Refuse(format!(
        "this backend does not check which venue a command names — it would apply an order for \
         `{venue}` to its PRIMARY book without saying so, and {known}. Upgrade the backend, or \
         trade a venue it publishes"
    ))
}

/// **The venues a backend publishes an ENGINE for**, off the snapshot the GUI is already
/// rendering — `vike_exec::Portfolio::venues` is documented as "one block per engine", so this IS
/// the backend's engine roster and not an approximation of it. It is the client-side routing
/// gate's positive evidence, and the twin of `vike-tradehub`'s own
/// `PublisherHandle::engine_venues`, which reads the same projection one hop earlier.
///
/// EMPTY means the observer has not received a frame yet, NOT that the backend runs no engines —
/// [`venue_routing_verdict`] treats it as "I know nothing", which against a backend that does not
/// check addresses is a refusal.
pub fn backend_engine_venues(snap: &vike_exec::CoreSnapshot) -> Vec<String> {
    snap.portfolio.venues.iter().map(|v| v.venue.clone()).collect()
}

/// **The ROUTING gate a GUI shell's write passes before it reaches a backend** ([`send_to_backend`]
/// applies it): `Ok` to send, and on `Err` the refusal's reason, already logged, for the status
/// strip.
///
/// Everything it decides lives here rather than in the shell on purpose — `crates/vike-desktop` is
/// in `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI`, so logic that lands there is compiled by one
/// job and executed by nothing, while this crate's tests run on every PR. The shell keeps the two
/// facts only it holds (the connected handle, the frame's snapshot) and none of the reasoning.
///
/// A command that addresses no venue ([`vike_tradehub_client::wire::WireCommand::addressed_venue`]
/// answers `None` — a cancel by coid, the account-wide kill switch, the UNSCOPED panic button) is
/// always sendable: those name no book, so there is nothing to get wrong, and a panic button with a
/// prerequisite is not one.
pub fn may_send_to_backend(
    cmd: &vike_tradehub_client::wire::WireCommand,
    snap: &vike_exec::CoreSnapshot,
    node_routes_by_venue: bool,
) -> Result<(), String> {
    let Some(venue) = cmd.addressed_venue() else {
        return Ok(());
    };
    match venue_routing_verdict(venue, &backend_engine_venues(snap), node_routes_by_venue) {
        VenueRouting::Send => Ok(()),
        VenueRouting::Refuse(why) => {
            // The `tracing` facade, which is all a library crate may use (binaries own
            // `vike_log::init`). One line per refused command: an operator who pressed a button and
            // saw no order needs the reason, and this path is operator-cadence, never a fold.
            tracing::warn!(%venue, "remote control: command NOT SENT — {why}");
            Err(why)
        }
    }
}

/// **The ONE call a GUI shell makes to write a command to a backend**: the lift
/// ([`wire_from_command`]), the routing gate ([`may_send_to_backend`]), then `write`, the shell's
/// handle (`RemoteControlHandle::try_command`). A command it does NOT send is logged here and its
/// line handed to `latch` — the shell's `RemoteControlHandle::latch_client_refusal` — and it returns
/// nothing, so no caller can drop that line.
///
/// ⚠ **A command the client does not send must SAY so on screen** (I-1 of the node-bracket
/// pre-flight), and only the LATCH can say it. The sharpest case is the normal one: a desktop built
/// from `main` against a node at the last release, which predates
/// `vike_tradehub_client::proto::FEATURE_BRACKET`. There the TP/SL toggle is enabled
/// ([`bracket_has_wire_form`] describes the client), and `write` refuses the bracket
/// (`ControlRejected::UnsupportedByNode`), which latches nothing by itself: the worker never sees a
/// command it was never handed. With no client gate, the old node would have answered "undecodable
/// request" and latched THAT, so the gate made the failure LESS visible.
///
/// ⚠ **Why not the desktop's own status line (C-1 of the Task 3 review).** The first fix returned
/// the line for the shell to paint into `app.status`. The desktop's frame runs
/// `crate::ui::core_sync::sync_from_core` FIRST, and it ends with an unconditional write of that
/// same line; then the strip paints; then the dispatch runs. A line the dispatch wrote was
/// therefore gone before any strip showed it. The latch is the channel the fold never touches,
/// which the control segment ([`control_status_line`]) paints until the operator dismisses it —
/// pinned through that real order by `crates/vike-app-core/src/ui/fold_tests.rs`'s
/// `a_client_side_refusal_survives_the_next_frames_status_rewrite`.
///
/// Every line is short enough to survive that segment's [`MAX_ERR_TAIL`] clip whole, except the
/// routing refusal's paragraph, which leads with the venue it names so that survives.
///
/// `write` and `latch` are closures so the whole decision is tested here with stubs, rather than in
/// the shell, which no test executes.
pub fn send_to_backend<T>(
    cmd: &vike_exec::Command,
    snap: &vike_exec::CoreSnapshot,
    node_routes_by_venue: bool,
    write: impl FnOnce(
        vike_tradehub_client::wire::WireCommand,
    ) -> Result<T, vike_tradehub_client::ControlRejected>,
    latch: impl FnOnce(String),
) {
    let Some(wire) = wire_from_command(cmd) else {
        tracing::warn!("remote control: command has no wire form yet; dropped");
        latch(format!("{} NOT sent: the node protocol has no form for it", unliftable_name(cmd)));
        return;
    };
    if let Err(why) = may_send_to_backend(&wire, snap, node_routes_by_venue) {
        // The reason runs to a paragraph, and the segment clips it: lead with what it is about.
        let head = match wire.addressed_venue().map(str::trim).filter(|v| !v.is_empty()) {
            Some(venue) => format!("command for `{venue}`"),
            None => "command".to_string(),
        };
        latch(format!("{head} NOT sent: {why}"));
        return;
    }
    if let Err(refused) = write(wire.clone()) {
        tracing::warn!(?refused, "remote control: command NOT SENT, refused before the wire");
        latch(refused_send_status(&wire, refused));
    }
}

/// What a refusal line calls a command [`wire_from_command`] cannot lift. Named for the ones a GUI
/// button produces (m-d of the Task 3 review: an operator who pressed the DOM's cancel-all must
/// learn it was the CANCEL that did not go); anything else is "command".
fn unliftable_name(cmd: &vike_exec::Command) -> String {
    use vike_exec::{Command, OrderIntent};
    let orders = |n: usize| if n == 1 { "1 order".to_string() } else { format!("{n} orders") };
    match cmd {
        Command::Order(OrderIntent::CancelBatch(coids)) => {
            format!("cancel of {}", orders(coids.len()))
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
/// word: which word a verb needs is `vike_tradehub_client::remote_control`'s `required_feature`,
/// and this line does not restate it.
///
/// The full-queue line is in the PAST tense on purpose: it is latched like the rest, so after a
/// retry that went through it must read as what happened, never as the state of the link. Every
/// line fits the control segment's [`MAX_ERR_TAIL`] whole.
pub fn refused_send_status(
    cmd: &vike_tradehub_client::wire::WireCommand,
    refused: vike_tradehub_client::ControlRejected,
) -> String {
    use vike_tradehub_client::ControlRejected;
    match (refused, cmd) {
        (
            ControlRejected::UnsupportedByNode,
            vike_tradehub_client::wire::WireCommand::Bracket(_),
        ) => {
            format!(
                "TP/SL bracket NOT sent: this node predates `{}` — upgrade the node",
                vike_tradehub_client::proto::FEATURE_BRACKET
            )
        }
        (ControlRejected::UnsupportedByNode, _) => {
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

/// Render the one-line remote **Scope::Write** channel summary the GUI status bar shows when the
/// desktop observer has a control channel mounted (`App::remote_ctrl` is `Some`; this said
/// `vike-app --observe` and `App.remote_ctrl.is_some()` until 2026-09-28), from
/// the two async surfaces the handle exposes —
/// [`vike_tradehub_client::RemoteControlHandle::is_connected`] and
/// [`vike_tradehub_client::RemoteControlHandle::last_error`]. Kept pure (no handle, no egui) so it
/// is unit-tested here in CI-covered `vike-app-core`; the GUI shell only reads the two inputs off
/// the handle and paints the returned string. Returns `None` when there is nothing to say — no
/// control channel (`present == false`) — so the status bar renders exactly as before on every
/// non-control path (the default).
///
/// Shapes (the live case is loud: this observer can drive REAL orders on the daemon, the words
/// say so, the segment is painted the armed amber, and the status bar leads it with the warning
/// icon — `crate::ui::status_dot::control_text`):
/// - connected, no error   → `"CONTROL live — this observer can place REAL orders"`
/// - connected, with error → `… + " · last error: <tail>"`
/// - disconnected           → `"CONTROL disconnected"` (+ the same error tail when present)
///
/// A long `last_error` is truncated to [`MAX_ERR_TAIL`] chars + `"…"` (the full text is in the log).
///
/// **`identity` names WHICH daemon the armed channel points at** (split-plane I3 — the spec's
/// named most-dangerous ambiguity). When the latest observe frame carried a
/// [`WireNodeIdentity`](vike_tradehub_client::wire::WireNodeIdentity)
/// (threaded here by the caller from `BridgeHandle::identity` —
/// [`crate::backend::observe_bridge::BridgeHandle`]), the line LEADS with
/// [`crate::backend::observe_bridge::identity_label`]'s tag — `"the build runner [LIVE] · "` (uppercase LIVE,
/// impossible to miss) or `"sim-box [paper] · "`. `None` (an older pre-B3 node, or no frame yet)
/// renders BYTE-IDENTICAL to the identity-less shapes above.
///
/// ⚠ `last_error` is the handle's LATCHED banner view — it names no command and is cleared only by
/// [`vike_tradehub_client::RemoteControlHandle::clear_last_error`] (the GUI wires that to a click on
/// this segment). It must never be used to report the outcome of a particular command: that is what
/// [`vike_tradehub_client::RemoteControlHandle::await_outcome`] and the `CommandTicket` a send
/// returns are for. Reading the latch per-command is what made every write after one refusal report
/// that refusal, including commands the node executed.
pub fn control_status_line(
    present: bool,
    connected: bool,
    last_error: Option<&str>,
    identity: Option<&vike_tradehub_client::wire::WireNodeIdentity>,
) -> Option<String> {
    if !present {
        return None;
    }
    let mut s = match identity {
        Some(id) => format!("{} · ", crate::backend::observe_bridge::identity_label(id)),
        None => String::new(),
    };
    // No warning glyph in the text: an icon cannot live inside a string (`vike_ui_theme::icons`'
    // module doc), so the status bar leads a LIVE line with `icons::WARNING` itself
    // (`crate::ui::status_dot::control_text`).
    s.push_str(if connected {
        "CONTROL live — this observer can place REAL orders"
    } else {
        "CONTROL disconnected"
    });
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
    Some(s)
}

#[path = "tradehub_control_tests.rs"]
#[cfg(test)]
mod tradehub_control_tests;
