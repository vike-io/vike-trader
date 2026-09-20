//! `tradehub_control` — the GUI-side lowering for the thin-client **Scope::Control** WRITE path
//! (headless two-layer plan, Layer 2): turn a live-core [`vike_exec::Command`] into the thin-wire
//! [`vike_tradehub_client::WireCommand`] a `vike-app --observe` observer sends to a remote headless
//! `vike-tradehub` daemon's control server.
//!
//! [`wire_from_command`] is the exact INVERSE of the daemon's `lower_command`
//! (`vike-tradehub/src/server.rs`, which lowers a received `WireCommand` back into a real
//! `Command`/`OrderIntent` at its edge). It is deliberately **PARTIAL**: the thin wire vocabulary
//! ([`vike_tradehub_client::WireCommand`]) is only the SESSION-relevant subset a remote GUI drives
//! (submit / cancel / modify / mass-cancel / flatten / market-exit / trading-state, plus the
//! strategy-level live-params re-tune since split-plane B4), NOT the full
//! internal `Command`/`OrderIntent` surface (brackets, batches, conditionals, combos, margin,
//! reconcile plumbing, shutdown). A `Command` with no wire form yields `None`; the caller
//! (the GUI's remote-control dispatch arm) logs and drops it — a future PR either extends
//! `WireCommand` or grays out the UI control that produces it.
//!
//! [`venue_routing_verdict`] is the CLIENT half of the node's routing gate, and it is the only
//! defence against the one direction the node cannot cover: a backend that predates
//! [`vike_tradehub_client::proto::FEATURE_VENUE_ROUTING`] accepts a command naming ANY venue and
//! applies it to its PRIMARY engine — `Ack`, an order in the snapshot, no error — so an operator
//! picking a venue sees success and the order is signed on a different exchange. Against such a
//! backend this client sends only a venue the backend has been seen to publish.
//! [`may_send_to_backend`] is what a shell actually calls — the whole decision AND its log line
//! live here rather than in `crates/vike-desktop`, which is in `EXCLUDE_FROM_CI` and so is compiled
//! by one job and executed by nothing; the shell keeps only the handle and the frame's snapshot. It
//! is applied at `vike-desktop`'s one remote-command choke point (`Dispatch::send`), never per
//! button.
//!
//! [`control_enabled`] is the master env gate for wiring this path at all — the same deliberately-
//! unfuzzy exact-`"1"` idiom as [`crate::reconcile_config::reconcile_enabled`], read straight off
//! the REAL process env (see `reconcile_config`'s module doc for why a feature toggle reads process
//! env, not the credentials `.env` map). OFF (unset) means a `--observe` observer stays read-only,
//! byte-identical to before this path existed.

/// Lower a live-core [`vike_exec::Command`] into the thin-wire [`vike_tradehub_client::WireCommand`]
/// a remote Scope::Control client sends — the GUI-side inverse of the daemon's `lower_command`.
///
/// PARTIAL (see the module doc): every mapped variant copies its fields into the standalone wire
/// mirror verbatim (a Submit copies all 9 `OrderRequest` fields the wire carries; an
/// `UpdateParams` copies its target key and re-serializes the typed `StrategyParams` into the
/// core's own serde JSON — the shape the wire deliberately delegates to); everything with
/// no wire form — `OrderIntent::{Bracket, SubmitBatch, CancelBatch, Confirm, ArmConditional,
/// DisarmConditional, Combo}` and `Command::{SetMargin, ApplySnapshot,
/// ReconcileReports, ConfirmRecon, Shutdown}` — returns `None`.
pub fn wire_from_command(cmd: &vike_exec::Command) -> Option<vike_tradehub_client::WireCommand> {
    use vike_exec::{Command, OrderIntent, TradingState};
    use vike_tradehub_client::{WireCommand, WireOrderRequest, WireTradingState};

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
            account: None,
        })),
        Command::Order(OrderIntent::Cancel(coid)) => Some(WireCommand::Cancel(coid.clone())),
        Command::Order(OrderIntent::Modify { client_order_id, new_qty, new_price }) => {
            Some(WireCommand::Modify {
                client_order_id: client_order_id.clone(),
                new_qty: *new_qty,
                new_price: *new_price,
            })
        }
        Command::Order(OrderIntent::MassCancel { venue, symbol }) => {
            Some(WireCommand::MassCancel {
                venue: venue.clone(),
                symbol: symbol.clone(),
                account: None,
            })
        }
        Command::Order(OrderIntent::Flatten { venue, symbol }) => Some(WireCommand::Flatten {
            venue: venue.clone(),
            symbol: symbol.clone(),
            account: None,
        }),
        Command::Order(OrderIntent::MarketExit { venue }) => {
            Some(WireCommand::MarketExit { venue: venue.clone(), account: None })
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
        // No thin-wire form yet (see the module doc) — the caller logs + drops these:
        //   OrderIntent::{Bracket, SubmitBatch, CancelBatch, Confirm, ArmConditional,
        //                 DisarmConditional, Combo}
        //   Command::{SetMargin, ApplySnapshot, ReconcileReports, ConfirmRecon, Shutdown}
        _ => None,
    }
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
///   [`crate::order_dispatch::DispatchRejectReason::NoRoutableMarket`] closes for the snapshot's
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
/// rendering — `vike_core::Portfolio::venues` is documented as "one block per engine", so this IS
/// the backend's engine roster and not an approximation of it. It is the client-side routing
/// gate's positive evidence, and the twin of `vike-tradehub`'s own
/// `PublisherHandle::engine_venues`, which reads the same projection one hop earlier.
///
/// EMPTY means the observer has not received a frame yet, NOT that the backend runs no engines —
/// [`venue_routing_verdict`] treats it as "I know nothing", which against a backend that does not
/// check addresses is a refusal.
pub fn backend_engine_venues(snap: &vike_core::CoreSnapshot) -> Vec<String> {
    snap.portfolio.venues.iter().map(|v| v.venue.clone()).collect()
}

/// **The ONE call a GUI shell makes before writing a command to a backend**: `true` to send, and on
/// `false` the refusal has already been logged with its reason.
///
/// Everything it decides lives here rather than in the shell on purpose — `crates/vike-desktop` is
/// in `xtask/src/ci/tables.rs`'s `EXCLUDE_FROM_CI`, so logic that lands there is compiled by one
/// job and executed by nothing, while this crate's tests run on every PR. The shell keeps the two
/// facts only it holds (the connected handle, the frame's snapshot) and none of the reasoning.
///
/// A command that addresses no venue ([`vike_tradehub_client::wire::WireCommand::addressed_venue`]
/// answers `None` — a cancel by coid, the account-wide kill switch, the UNSCOPED panic button) is
/// always sendable: those name no book, so there is nothing to get wrong, and a panic button with a
/// prerequisite is not one.
pub fn may_send_to_backend(
    cmd: &vike_tradehub_client::WireCommand,
    snap: &vike_core::CoreSnapshot,
    node_routes_by_venue: bool,
) -> bool {
    let Some(venue) = cmd.addressed_venue() else {
        return true;
    };
    match venue_routing_verdict(venue, &backend_engine_venues(snap), node_routes_by_venue) {
        VenueRouting::Send => true,
        VenueRouting::Refuse(why) => {
            // The `tracing` facade, which is all a library crate may use (binaries own
            // `vike_log::init`). One line per refused command: an operator who pressed a button and
            // saw no order needs the reason, and this path is operator-cadence, never a fold.
            tracing::warn!(%venue, "remote control: command NOT SENT — {why}");
            false
        }
    }
}

/// The master gate for wiring a `vike-app --observe` observer's Scope::Control write path: `true`
/// iff `VIKE_TRADEHUB_CONTROL` is the EXACT string `"1"`. Read straight off the REAL process env
/// (like every other `VIKE_*` feature toggle — see [`crate::reconcile_config`]'s module doc for why
/// a feature toggle reads process env, not the credentials `.env` map), with the same deliberately-
/// unfuzzy on-string idiom as [`crate::reconcile_config::reconcile_enabled`] (one unambiguous flag
/// to grep for in an incident); unset or any other value (`"true"`, `"yes"`, `"0"`, ...) stays
/// `false`, so a `--observe` observer stays read-only by default.
pub fn control_enabled() -> bool {
    // The pure exact-`"1"` test is factored into `flag_is_on` so it is unit-testable WITHOUT
    // mutating the process-global env (`std::env::set_var` is unsound from parallel test threads —
    // the same reason `reconcile_config` takes a `&HashMap`; here the value comes from process env).
    flag_is_on(std::env::var("VIKE_TRADEHUB_CONTROL").ok().as_deref())
}

/// The pure exact-`"1"` predicate behind [`control_enabled`] — `true` iff `val` is exactly
/// `Some("1")`. Unit-testable without touching the process env.
fn flag_is_on(val: Option<&str>) -> bool {
    val == Some("1")
}

/// The longest server-error tail rendered inline in the one-line status summary — a rejected
/// command / handshake reason can be verbose, so it is truncated with an ellipsis to keep the
/// status strip a single tidy line (the full text stays in the tracing log).
const MAX_ERR_TAIL: usize = 80;

/// Render the one-line remote **Scope::Control** channel summary the GUI status bar shows when a
/// `vike-app --observe` observer has a control channel mounted (`App.remote_ctrl.is_some()`), from
/// the two async surfaces the handle exposes —
/// [`vike_tradehub_client::RemoteControlHandle::is_connected`] and
/// [`vike_tradehub_client::RemoteControlHandle::last_error`]. Kept pure (no handle, no egui) so it
/// is unit-tested here in CI-covered `vike-app-core`; the GUI shell only reads the two inputs off
/// the handle and paints the returned string. Returns `None` when there is nothing to say — no
/// control channel (`present == false`) — so the status bar renders exactly as before on every
/// non-control path (the default).
///
/// Shapes (⚠ = this observer can drive REAL orders on the daemon, so the live case is loud):
/// - connected, no error   → `"⚠ CONTROL live — this observer can place REAL orders"`
/// - connected, with error → `… + " · last error: <tail>"`
/// - disconnected           → `"CONTROL disconnected"` (+ the same error tail when present)
///
/// A long `last_error` is truncated to [`MAX_ERR_TAIL`] chars + `"…"` (the full text is in the log).
///
/// **`identity` names WHICH daemon the armed channel points at** (split-plane I3 — the spec's
/// named most-dangerous ambiguity). When the latest observe frame carried a
/// [`WireNodeIdentity`](vike_tradehub_client::wire::WireNodeIdentity)
/// (threaded here by the caller from `BridgeHandle::identity` —
/// [`crate::observe_bridge::BridgeHandle`]), the line LEADS with
/// [`crate::observe_bridge::identity_label`]'s tag — `"the build runner [LIVE] · "` (uppercase LIVE,
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
        Some(id) => format!("{} · ", crate::observe_bridge::identity_label(id)),
        None => String::new(),
    };
    s.push_str(if connected {
        "⚠ CONTROL live — this observer can place REAL orders"
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_tradehub_client::{WireCommand, WireOrderRequest, WireTradingState};

    fn req(coid: &str) -> vike_model::OrderRequest {
        vike_model::OrderRequest {
            client_order_id: coid.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: "limit".into(),
            price: Some(59_000.0),
            trigger_price: None,
            reduce_only: true,
            ..Default::default()
        }
    }

    #[test]
    fn submit_copies_all_nine_wire_fields() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Submit(Box::new(req("c-1"))));
        // Compare the WHOLE WireCommand (derived PartialEq) — asserts all 9 carried fields at once
        // AND keeps the float fields off a source-level `==` (which would trip `clippy::float_cmp`).
        assert_eq!(
            wire_from_command(&cmd),
            Some(WireCommand::Submit(WireOrderRequest {
                client_order_id: "c-1".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 0.5,
                order_type: "limit".into(),
                price: Some(59_000.0),
                trigger_price: None,
                reduce_only: true,
                account: None,
            }))
        );
    }

    #[test]
    fn cancel_maps() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Cancel("c-1".into()));
        assert_eq!(wire_from_command(&cmd), Some(WireCommand::Cancel("c-1".into())));
    }

    #[test]
    fn modify_maps() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Modify {
            client_order_id: "c-1".into(),
            new_qty: Some(2.0),
            new_price: None,
        });
        assert_eq!(
            wire_from_command(&cmd),
            Some(WireCommand::Modify {
                client_order_id: "c-1".into(),
                new_qty: Some(2.0),
                new_price: None,
            })
        );
    }

    #[test]
    fn mass_cancel_maps() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::MassCancel {
            venue: Some("binance".into()),
            symbol: None,
        });
        assert_eq!(
            wire_from_command(&cmd),
            Some(WireCommand::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: None
            })
        );
    }

    #[test]
    fn flatten_maps() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
        });
        assert_eq!(
            wire_from_command(&cmd),
            Some(WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                account: None
            })
        );
    }

    #[test]
    fn market_exit_maps() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::MarketExit { venue: None });
        assert_eq!(
            wire_from_command(&cmd),
            Some(WireCommand::MarketExit { venue: None, account: None })
        );
    }

    #[test]
    fn set_trading_state_maps_all_three_arms() {
        use vike_exec::TradingState;
        for (ts, wts) in [
            (TradingState::Active, WireTradingState::Active),
            (TradingState::Reducing, WireTradingState::Reducing),
            (TradingState::Halted, WireTradingState::Halted),
        ] {
            let cmd = vike_exec::Command::SetTradingState(ts);
            assert_eq!(wire_from_command(&cmd), Some(WireCommand::SetTradingState(wts)));
        }
    }

    #[test]
    fn bracket_has_no_wire_form() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Bracket(Box::new(
            vike_model::BracketSpec {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                entry_price: Some(100.0),
                stop_loss: 95.0,
                take_profit: 110.0,
            },
        )));
        assert_eq!(wire_from_command(&cmd), None);
    }

    #[test]
    fn set_margin_and_shutdown_have_no_wire_form() {
        let margin = vike_exec::Command::SetMargin(Box::new(vike_exec::MarginUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            im_requirement: 0.1,
        }));
        assert_eq!(wire_from_command(&margin), None);
        assert_eq!(wire_from_command(&vike_exec::Command::Shutdown), None);
    }

    /// The B4 strategy write verb maps: the target key copies verbatim and the typed
    /// `StrategyParams` re-serializes into the core's OWN serde JSON — byte-for-byte the value
    /// `serde_json::to_value` produces from the core type, which is what the daemon's
    /// `lower_command` deserializes back (the delegated-schema contract).
    #[test]
    fn update_params_maps_onto_the_delegated_core_json() {
        let params = vike_model::StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
            qty: 2.0,
            half_spread: 1.0,
            target_inventory: 0.0,
            max_inventory: 1.0,
            skew: 0.0,
            fill_window_ms: 0,
            net_fill_threshold: 0.0,
            suppress_cooldown_ms: 0,
            style: vike_model::QuoteStyle::Mid,
            depth_levels: 1,
            tick_size: 0.0,
            filter_own: false,
            avellaneda_stoikov: None,
            refresh_tolerance: None,
            ladder: None,
            reward: None,
            toxicity: None,
        });
        let cmd = vike_exec::Command::UpdateParams(Box::new(vike_exec::ParamsUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            params,
        }));
        let Some(WireCommand::UpdateParams { venue, symbol, interval, params: wire_params }) =
            wire_from_command(&cmd)
        else {
            panic!("UpdateParams has a wire form since split-plane B4");
        };
        assert_eq!(
            (venue.as_str(), symbol.as_str(), interval.as_str()),
            ("binance", "BTCUSDT", "1m")
        );
        let vike_exec::Command::UpdateParams(u) = &cmd else { unreachable!() };
        assert_eq!(
            wire_params,
            serde_json::to_value(&u.params).unwrap(),
            "the wire payload IS the core type's own serde JSON, not a lookalike"
        );
    }

    #[test]
    fn control_flag_is_on_only_for_exact_one() {
        assert!(flag_is_on(Some("1")));
        assert!(!flag_is_on(Some("true")));
        assert!(!flag_is_on(Some("0")));
        assert!(!flag_is_on(Some("")));
        assert!(!flag_is_on(None));
    }

    #[test]
    fn status_line_absent_when_no_control_channel() {
        // The default (non-control) path: nothing to render, so the status bar is byte-identical.
        assert_eq!(control_status_line(false, false, None, None), None);
        assert_eq!(control_status_line(false, true, Some("ignored"), None), None);
    }

    #[test]
    fn status_line_connected_no_error_is_the_loud_live_warning() {
        assert_eq!(
            control_status_line(true, true, None, None).as_deref(),
            Some("⚠ CONTROL live — this observer can place REAL orders")
        );
        // A blank/whitespace error is treated as no error (no dangling " · last error: ").
        assert_eq!(
            control_status_line(true, true, Some("   "), None).as_deref(),
            Some("⚠ CONTROL live — this observer can place REAL orders")
        );
    }

    #[test]
    fn status_line_disconnected_shows_disconnected() {
        assert_eq!(
            control_status_line(true, false, None, None).as_deref(),
            Some("CONTROL disconnected")
        );
    }

    #[test]
    fn status_line_appends_error_tail_on_either_connection_state() {
        assert_eq!(
            control_status_line(true, true, Some("order denied"), None).as_deref(),
            Some("⚠ CONTROL live — this observer can place REAL orders · last error: order denied")
        );
        assert_eq!(
            control_status_line(true, false, Some("auth denied"), None).as_deref(),
            Some("CONTROL disconnected · last error: auth denied")
        );
    }

    #[test]
    fn status_line_truncates_a_long_error_tail() {
        let long = "x".repeat(MAX_ERR_TAIL + 40);
        let out = control_status_line(true, false, Some(&long), None).unwrap();
        let expected_tail: String = format!("{}…", "x".repeat(MAX_ERR_TAIL));
        assert!(out.ends_with(&expected_tail), "got: {out}");
        // A tail exactly at the cap is NOT truncated (no ellipsis).
        let exact = "y".repeat(MAX_ERR_TAIL);
        let out2 = control_status_line(true, false, Some(&exact), None).unwrap();
        assert!(out2.ends_with(&exact) && !out2.ends_with('…'), "got: {out2}");
    }

    /// A wire identity for the I3 tests — only `name` and `live` render; the rest rides along.
    fn ident(name: &str, live: bool) -> vike_tradehub_client::wire::WireNodeIdentity {
        vike_tradehub_client::wire::WireNodeIdentity {
            name: name.into(),
            strategy: "spread_maker".into(),
            params: "{}".into(),
            live,
            build: "vike-tradehub 0.1.0 (abc1234)".into(),
            advertise_addr: String::new(),
        }
    }

    /// I3 (split-plane): the control line LEADS with the daemon identity — `name [LIVE]`, the
    /// uppercase tag loud by design, because "which daemon is the armed control channel pointing
    /// at" is the spec's named most-dangerous ambiguity.
    #[test]
    fn status_line_with_live_identity_leads_with_name_and_uppercase_live() {
        let id = ident("the build runner", true);
        let line = control_status_line(true, true, None, Some(&id)).unwrap();
        assert_eq!(line, "the build runner [LIVE] · ⚠ CONTROL live — this observer can place REAL orders");
        assert!(line.starts_with("the build runner [LIVE]"), "got: {line}");
    }

    /// A paper daemon is named too, but with the lowercase `[paper]` tag — and the whole line must
    /// carry no uppercase "LIVE" anywhere, so a glance can never read a paper daemon as live.
    #[test]
    fn status_line_with_paper_identity_names_daemon_without_uppercase_live() {
        let id = ident("sim-box", false);
        let line = control_status_line(true, true, None, Some(&id)).unwrap();
        assert_eq!(line, "sim-box [paper] · ⚠ CONTROL live — this observer can place REAL orders");
        assert!(!line.contains("LIVE"), "a paper daemon must never render LIVE: {line}");
    }

    /// The identity prefix also leads the disconnected shape — while the channel heals, the
    /// operator still needs to know WHICH daemon it was armed at.
    #[test]
    fn status_line_identity_prefixes_the_disconnected_shape() {
        let id = ident("the build runner", true);
        assert_eq!(
            control_status_line(true, false, None, Some(&id)).as_deref(),
            Some("the build runner [LIVE] · CONTROL disconnected")
        );
    }

    /// Backward compat (B3: `identity` is `None` from an older node): every identity-less shape
    /// renders BYTE-IDENTICAL to the pre-I3 strings — the exact strings the tests above pinned
    /// before the parameter existed.
    #[test]
    fn status_line_without_identity_is_byte_identical_to_pre_i3_rendering() {
        assert_eq!(
            control_status_line(true, true, None, None).as_deref(),
            Some("⚠ CONTROL live — this observer can place REAL orders")
        );
        assert_eq!(
            control_status_line(true, false, None, None).as_deref(),
            Some("CONTROL disconnected")
        );
        assert_eq!(
            control_status_line(true, false, Some("auth denied"), None).as_deref(),
            Some("CONTROL disconnected · last error: auth denied")
        );
    }

    // -----------------------------------------------------------------------------------------
    // The client-side ROUTING gate
    // -----------------------------------------------------------------------------------------

    fn venues(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// **The dangerous direction: a NEW client against an OLD backend.** The backend advertises
    /// nothing, so it will take any venue and apply it to its PRIMARY book while answering `Ack`.
    /// The client must therefore refuse locally — and the refusal must name both what was asked
    /// for and what the backend publishes, because "pick a different venue" is useless advice
    /// without the list.
    #[test]
    fn an_old_backend_refuses_a_venue_it_does_not_publish() {
        let VenueRouting::Refuse(why) =
            venue_routing_verdict("okx", &venues(&["binance", "polymarket"]), false)
        else {
            panic!("a backend that does not check the address must not be handed an unseen venue");
        };
        assert!(why.contains("okx"), "{why}");
        assert!(why.contains("binance") && why.contains("polymarket"), "{why}");
    }

    /// ...but a venue that backend PUBLISHES an engine for is positive evidence, and is sent. This
    /// is the only evidence available against an old backend, and it is enough: the core routes a
    /// venue it runs correctly — the historical `unwrap_or(0)` fallback fires only when nothing
    /// matches.
    #[test]
    fn an_old_backend_still_takes_a_venue_it_publishes() {
        assert_eq!(
            venue_routing_verdict("binance", &venues(&["binance", "polymarket"]), false),
            VenueRouting::Send
        );
    }

    /// A backend that ADVERTISES the capability is sent the command even for a venue this client
    /// cannot see. This is the one rule that leans toward sending, deliberately: `node_venues`
    /// comes from a pushed snapshot that lags a runtime `MountStrategy`, and the worst case here is
    /// a clean `Response::Error` from a backend that knows its own engines — never a misroute.
    #[test]
    fn a_backend_that_checks_the_address_is_trusted_to_answer() {
        assert_eq!(
            venue_routing_verdict("okx", &venues(&["binance"]), true),
            VenueRouting::Send,
            "the backend will refuse it by name; this client must not pre-empt a fresher answer"
        );
    }

    /// ⚠ The most conservative case: an observer that has connected but received no frame knows
    /// NOTHING about the backend. Against one that does not advertise the capability that is a
    /// refusal, because "I know nothing" must never read as "anything goes" on a path that signs
    /// orders — and the message says the backend has published none rather than printing an empty
    /// list.
    #[test]
    fn knowing_nothing_about_an_old_backend_refuses_rather_than_guesses() {
        let VenueRouting::Refuse(why) = venue_routing_verdict("binance", &[], false) else {
            panic!("no evidence + no capability must refuse");
        };
        assert!(why.contains("published none"), "{why}");
    }

    /// A blank venue is refused on EVERY backend, advertised or not: an empty string is not a
    /// venue, and a node's routing falls to engine 0 for it on every build. (The observer's
    /// pre-first-frame snapshot placeholder carries exactly this — empty strings.)
    #[test]
    fn a_blank_venue_is_refused_even_by_a_backend_that_checks_addresses() {
        for venue in ["", "   "] {
            assert!(
                matches!(
                    venue_routing_verdict(venue, &venues(&["binance"]), true),
                    VenueRouting::Refuse(_)
                ),
                "a blank venue names no book: {venue:?}"
            );
        }
    }

    /// The comparison is EXACT, matching the node's own engine selection (a string equality on the
    /// route key). A case slip against an old backend is refused rather than silently routed to
    /// its primary.
    #[test]
    fn the_venue_comparison_is_exact() {
        assert!(matches!(
            venue_routing_verdict("BINANCE", &venues(&["binance"]), false),
            VenueRouting::Refuse(_)
        ));
    }

    // -----------------------------------------------------------------------------------------
    // The shell's ONE call
    // -----------------------------------------------------------------------------------------

    fn snap_with(venues: &[&str]) -> vike_core::CoreSnapshot {
        let mut snap = vike_core::CoreSnapshot::empty("", "");
        snap.portfolio.venues = venues
            .iter()
            .map(|v| vike_core::VenueBlock {
                venue: (*v).to_string(),
                account: None,
                route_key: (*v).to_string(),
                balance: 0.0,
                realized_pnl: 0.0,
                fees_paid: 0.0,
                funding_paid: 0.0,
                balance_mode: vike_exec::BalanceMode::Delta,
                multipliers: Default::default(),
                multiplier_default: 1.0,
                equity: 0.0,
                unrealized: 0.0,
                missing_prices: 0,
                margin_used: 0.0,
                free_bp: 0.0,
                margin_ratio: 0.0,
                fee_schedule: None,
                trading_state: vike_exec::TradingState::Active,
                positions: Vec::new(),
            })
            .collect();
        snap
    }

    fn wire_submit(venue: &str) -> WireCommand {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: venue.into(),
            symbol: "SYM".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
        })
    }

    /// The roster comes off the snapshot's per-ENGINE blocks, in their published order.
    #[test]
    fn the_backend_roster_is_read_off_the_published_engine_blocks() {
        assert_eq!(
            backend_engine_venues(&snap_with(&["polymarket", "binance"])),
            vec!["polymarket".to_string(), "binance".to_string()]
        );
        assert!(
            backend_engine_venues(&vike_core::CoreSnapshot::empty("", "")).is_empty(),
            "a snapshot with no engine blocks is the observer's pre-first-frame placeholder"
        );
    }

    /// The composed call, in the two directions that matter: a venue the backend publishes goes out
    /// even against a backend that checks nothing; one it does not publish is held back.
    #[test]
    fn the_shell_call_sends_a_published_venue_and_holds_an_unpublished_one() {
        let snap = snap_with(&["polymarket", "binance"]);
        assert!(may_send_to_backend(&wire_submit("binance"), &snap, false));
        assert!(
            !may_send_to_backend(&wire_submit("okx"), &snap, false),
            "an order for a venue this backend does not publish would land on its PRIMARY book"
        );
        assert!(
            may_send_to_backend(&wire_submit("okx"), &snap, true),
            "...unless the backend checks addresses, in which case it answers by name"
        );
    }

    /// An ADDRESS-LESS command is always sendable, whatever the backend said about itself — the
    /// UNSCOPED panic button above all. A kill switch with a prerequisite is not one.
    #[test]
    fn an_address_less_command_is_always_sendable() {
        let empty = vike_core::CoreSnapshot::empty("", "");
        for cmd in [
            WireCommand::MarketExit { venue: None, account: None },
            WireCommand::Cancel("c-1".into()),
            WireCommand::SetTradingState(WireTradingState::Halted),
        ] {
            assert!(
                may_send_to_backend(&cmd, &empty, false),
                "{cmd:?} names no venue, so there is nothing to misroute"
            );
        }
    }
}
