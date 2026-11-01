//! The order lane: the `Dispatch` command choke point and `dispatch_orders`.

use super::*;

/// Where one order/session command from the GUI is routed: a remote headless daemon's Scope::Write
/// channel (an observer with control enabled) — the ONLY route an order out of this process has.
/// Built once at the command choke point in `App::ui` (its `eframe::App` impl) from
/// `self.remote_ctrl()` and borrows that handle for the block, so EVERY command site funnels
/// through one [`Dispatch::send`]. The path is
/// `vike_app_core::backend::tradehub_control::send_to_backend`, which lowers each command through
/// `wire_from_command` and reports anything it does not send in the log and on the status strip's
/// control segment (the handle's latch).
///
/// One variant on purpose: the in-process `Local` arm left with the local core (rulings 1 and 2),
/// and `crates/vike-desktop/tests/app_local_core_gate.rs` pins the `Dispatch::Remote(` spelling.
enum Dispatch<'a> {
    /// The remote channel, plus THIS FRAME'S SNAPSHOT — the second field is the client-side
    /// ROUTING gate's evidence about the backend (which venues it publishes an engine for), and it
    /// rides here rather than being re-read inside [`Dispatch::send`] because the snapshot is
    /// already borrowed for this block. The gate itself is
    /// `vike_app_core::backend::tradehub_control::may_send_to_backend`, one crate down, where CI runs it.
    /// The first field is the connection that owns the handle: an order's ticket is kept on it
    /// (`BackendConn::track_sent`) so the node's refusal of that order reaches its window's strip.
    Remote(
        &'a vike_app_core::backend::backend_conn::BackendConn,
        &'a vike_tradehub_client::RemoteControlHandle,
        &'a vike_exec::CoreSnapshot,
    ),
}

impl Dispatch<'_> {
    /// Fire one command — fire-and-forget. A command that was NOT sent (no thin-wire form, held back by the routing gate, or
    /// refused by the client before the wire: a bracket against a node older than `bracket`) is
    /// LATCHED on the handle, where the status strip's control segment shows it until dismissed.
    /// ⚠ Never into `app.status`: the next frame's `sync_from_core` rewrites that line before the
    /// strip paints it (C-1 of the node-bracket Task 3 review).
    ///
    /// The remote arm DISCARDS the `CommandTicket` deliberately: a GUI button has no synchronous
    /// place to report an outcome (blocking the repaint on a network reply is the one thing this
    /// path must never do), so the observer learns results the same way it always has — the pushed
    /// snapshot for what happened, and the status strip's latched `last_error` for what was
    /// refused. A surface that PRINTS a per-command verdict (`vike-cli trade`/`mcp`) must instead
    /// await the ticket; reading the latch for that is the stale-refusal bug.
    fn send(&self, cmd: vike_exec::Command) {
        match self {
            // ⚠ **The client-side gates fire here**, at the one choke point rather than at each
            // button, and before the write: a backend that does not advertise
            // `vike_tradehub_client::proto::FEATURE_VENUE_ROUTING` accepts a command naming ANY
            // venue and applies it to its PRIMARY engine — `Ack`, an order in the snapshot, no
            // error anywhere — so there is no reply to check afterwards. The DECISIONS, their log
            // lines and the refusal lines are `send_to_backend`, one crate down, where CI runs
            // them; what stays here is the two facts only this binary holds, the write and the latch.
            Dispatch::Remote(backend, ctrl, snap) => {
                let sent = vike_app_core::backend::tradehub_control::send_to_backend(
                    &cmd,
                    snap,
                    ctrl.routes_by_venue(),
                    |wire| ctrl.try_command(wire),
                    |line| ctrl.latch_client_refusal(line),
                );
                if let Some(ticket) = sent {
                    backend.track_sent(ticket, &cmd);
                }
            }
        }
    }
}

/// Tail phase — the order lane: every window's drained intents become commands (or refusals on
/// their window's strip), sent through the local core or the remote Scope::Write channel.
pub(super) fn dispatch_orders(
    app: &mut App,
    fr: &WindowFrame,
    trade_frame: tool_views::TradeFrame,
    account_cancels: Vec<String>,
    cockpit_cmds: Vec<CockpitCmd>,
    opt_orders: Vec<tools::OptOrderTicket>,
    opt_cancels: Vec<String>,
) {
    // The order lane: every window's intents become commands (or refusals on their window's strip).
    // Route order/session commands to the remote Scope::Write channel (an observer with control
    // enabled), the only route there is. No channel — a read-only observer, or one still
    // connecting — drops the whole block, a read-only no-op.
    // Field-path spelling of `app.remote_ctrl()` (disjoint borrows): the whole-`app` borrow
    // a method call takes would pin `app.next_win_n`'s assignment below for `dispatch`'s
    // lifetime.
    let backend = app.active_backend.as_ref();
    let remote_ctrl = backend.and_then(|b| b.ctrl.as_ref());
    // ⚠ The remote arm now carries THIS FRAME'S SNAPSHOT beside the handle, and it is not
    // decoration: it is the client-side ROUTING gate's evidence about the backend (which venues it
    // publishes an engine for — `Portfolio::venues`, one block per engine). Against a backend that
    // does not advertise `FEATURE_VENUE_ROUTING`, a venue in that list is the ONLY venue an order
    // may name, because every other one silently lands on that backend's primary book. The gate is
    // `vike_app_core::backend::tradehub_control::may_send_to_backend`, applied inside
    // `send_to_backend`, which `Dispatch::send` calls; a command it does not send is latched on the
    // handle, and the status strip's control segment shows it.
    let dispatch =
        backend.zip(remote_ctrl).map(|(b, ctrl)| Dispatch::Remote(b, ctrl, &fr.core_snap));
    if let (Some(dispatch), Some(backend)) = (dispatch, backend) {
        // What the node answered to the orders sent in earlier frames: its refusal of one is on the
        // strip of the window that sent it, in the node's own words (the control segment keeps only
        // a short tail of the latest refusal and names no order).
        tool_views::route_node_refusals(&app.wins, &mut app.tool_views, &backend.node_refusals());
        // Local preview caps for the order-entry safety layer, from the POLICY ceiling
        // (the `policy.max_notional_per_order` row, resolved ONCE in `main` —
        // see `POLICY_ORDER_LIMITS`), else permissive. A LOCAL guard in front of the venue
        // RiskGate — an order that fails it is dropped with a warn instead of round-tripping
        // to a venue that would reject it.
        //
        // ⚠ This was a PER-FRAME `env::var("VIKE_MAX_ORDER_NOTIONAL")`. Phase 5 of the
        // settings-unification design removed that variable outright: a ceiling any exported
        // variable can raise is not a ceiling — and re-reading it every frame meant the limit
        // could also move under a running process. It is resolved once at startup now, and a
        // process that still finds the old variable set REFUSES TO BOOT (see `main`).
        let order_limits = *POLICY_ORDER_LIMITS.get_or_init(order_entry::OrderLimits::default);
        // The DECISION — which drained intent becomes which `vike_exec::Command`, and which the
        // local preview refuses — is `vike_app_core::orders::order_dispatch::plan_dispatch`, a PURE
        // function; this file keeps only the I/O (drain in, `Dispatch::send` out, warn the
        // refusals).
        //
        // WHY IT MOVED. The block that used to sit here validated only THREE of its five submit
        // paths (the DOM ladder `Place`, the Polymarket cockpit `Submit`, the deribit options
        // confirm ticket). The **Trade window** — the manual order-entry panel a human types
        // into — called neither `validate` nor `validate_with_value`, and neither did its
        // TP+SL bracket sub-path nor the DOM Close/Reverse exit, so the one MANUAL path had no
        // local notional cap at all. Fixing that here would have been exactly as unverified as
        // the bug: nothing TESTS this file (`justfile`'s `ci_crates` omits this crate — `vike-app`
        // then, `vike-desktop` now — so `just windows-check` skips it too, and
        // `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI` lists it; the `app-check` job compiles and
        // clippy-gates it and runs none of this logic) — the same reason `order_entry`'s
        // notional-MULTIPLIER bug shipped. So the
        // decision now lives in a crate CI compiles and tests, behind a structural chokepoint
        // plus an exhaustive `SubmitSource` roster; see that module's doc for the gate.
        //
        // Every Trade window intent arrives with its window's OWN venue, account and symbol (the
        // address it drew, `TradeFrame::drain`), never the snapshot's primary market.
        let plan = vike_app_core::orders::order_dispatch::plan_dispatch(
            vike_app_core::orders::order_dispatch::DispatchInputs {
                trade_actions: trade_frame.orders,
                account_cancels,
                cockpit_cmds,
                opt_orders,
                opt_cancels,
            },
            &order_limits,
            &fr.core_snap,
            app.next_win_n,
            app.shot_n,
            // A VIKE_SHOT run keeps the deterministic ids; a live session mints its own.
            app.env.shot.is_none().then_some(&mut app.coids),
        );
        app.next_win_n = plan.next_win_n;
        for reject in &plan.rejects {
            tracing::warn!(
                submit_source = reject.source.label(),
                venue = %reject.venue,
                symbol = %reject.symbol,
                account = ?reject.account,
                "order rejected by local preview: {}",
                reject.reason
            );
        }
        // ...and each Trade window's own refusals on its strip (venue + symbol + account).
        tool_views::route_rejects(&app.wins, &mut app.tool_views, &plan.rejects);
        for cmd in plan.commands {
            dispatch.send(cmd);
        }
    } else {
        // No control channel: nothing leaves, and every Trade window that tried says so.
        tool_views::refuse_unsent(&app.wins, &mut app.tool_views, &trade_frame.orders);
    }
}
