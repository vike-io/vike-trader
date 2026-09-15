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
            Some(WireCommand::MassCancel { venue: venue.clone(), symbol: symbol.clone() })
        }
        Command::Order(OrderIntent::Flatten { venue, symbol }) => {
            Some(WireCommand::Flatten { venue: venue.clone(), symbol: symbol.clone() })
        }
        Command::Order(OrderIntent::MarketExit { venue }) => {
            Some(WireCommand::MarketExit { venue: venue.clone() })
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
            Some(WireCommand::MassCancel { venue: Some("binance".into()), symbol: None })
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
            Some(WireCommand::Flatten { venue: "binance".into(), symbol: "BTCUSDT".into() })
        );
    }

    #[test]
    fn market_exit_maps() {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::MarketExit { venue: None });
        assert_eq!(wire_from_command(&cmd), Some(WireCommand::MarketExit { venue: None }));
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
}
