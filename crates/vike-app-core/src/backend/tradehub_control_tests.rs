use super::*;
use vike_tradehub_client::wire::WireBracketSpec;
use vike_tradehub_client::{
    ControlRejected,
    wire::{WireCommand, WireOrderRequest, WireTradingState},
};

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

/// A Submit's account crosses the lift in the wire spelling (final review I-1). The test above
/// builds its request with `account: None`, so it passes whether the lift copies the account or
/// drops it; this one does not.
#[test]
fn a_submit_carries_its_account_in_the_wire_spelling() {
    use vike_model::accounts::account_keys::AccountLabel;
    let mut request = req("c-1");
    for (account, want) in [
        (None, None),
        (Some(AccountLabel::Default), Some("DEFAULT")),
        (AccountLabel::parse("SUB").ok(), Some("SUB")),
    ] {
        request.account = account;
        let cmd =
            vike_exec::Command::Order(vike_exec::OrderIntent::Submit(Box::new(request.clone())));
        match wire_from_command(&cmd) {
            Some(WireCommand::Submit(w)) => assert_eq!(w.account.as_deref(), want),
            other => panic!("a submit lifts, got {other:?}"),
        }
    }
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
        account: None,
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
        account: None,
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
    let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::MarketExit {
        venue: None,
        account: None,
    });
    assert_eq!(
        wire_from_command(&cmd),
        Some(WireCommand::MarketExit { venue: None, account: None })
    );
}

/// A reducing intent that NAMES an account keeps it across the lift — in the wire's spelling,
/// `DEFAULT` included, exactly as `MountStrategy`'s arm renders it. Lifting it to `None` would
/// WIDEN the verb on the far side: an account-less reduce fans out over every account of the
/// venue, so a thin client that dropped the field would flatten books its operator did not
/// name.
#[test]
fn a_labelled_reducing_intent_lifts_its_account_in_the_wire_spelling() {
    use vike_exec::{Command, OrderIntent};
    use vike_model::accounts::account_keys::AccountLabel;
    let alt = || Some(AccountLabel::Named("ALT".into()));
    assert_eq!(
        wire_from_command(&Command::Order(OrderIntent::MassCancel {
            venue: Some("binance".into()),
            symbol: None,
            account: alt(),
        })),
        Some(WireCommand::MassCancel {
            venue: Some("binance".into()),
            symbol: None,
            account: Some("ALT".into()),
        })
    );
    assert_eq!(
        wire_from_command(&Command::Order(OrderIntent::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: alt(),
        })),
        Some(WireCommand::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: Some("ALT".into()),
        })
    );
    assert_eq!(
        wire_from_command(&Command::Order(OrderIntent::MarketExit {
            venue: Some("binance".into()),
            account: Some(AccountLabel::Default),
        })),
        Some(WireCommand::MarketExit {
            venue: Some("binance".into()),
            account: Some("DEFAULT".into()),
        }),
        "`DEFAULT` is a NAMED account on the wire, not an absent one"
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

/// A bracket lifts field for field. This was `bracket_has_no_wire_form`, which asserted `None` for
/// the same command while the wire had no bracket and the shell dropped the TP/SL ticket with a log
/// line. It is inverted rather than deleted, so the history stays attached to the pin.
#[test]
fn a_bracket_lifts_field_for_field() {
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
    assert_eq!(
        wire_from_command(&cmd),
        Some(WireCommand::Bracket(WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
        }))
    );
    // …and a MARKET entry lifts as `entry_price: None`, the wire's spelling of "enter at market"
    // (m-b of the Task 3 review): the lift must not invent a price for it.
    let market = vike_exec::Command::Order(vike_exec::OrderIntent::Bracket(Box::new(
        vike_model::BracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            entry_price: None,
            stop_loss: 105.0,
            take_profit: 90.0,
        },
    )));
    let Some(WireCommand::Bracket(lifted)) = wire_from_command(&market) else {
        panic!("a market-entry bracket lifts too");
    };
    assert_eq!((lifted.entry_price, lifted.side), (None, -1));
}

/// The Trade window's TP/SL toggle reads this, and it is DERIVED from the lift, so it answers true
/// the moment the lift above does. Ruling R8 of the Trade window plan. Asked twice because the
/// answer is cached (`OnceLock`): the second read is the cached one, and must agree.
#[test]
fn a_bracket_has_a_wire_form_so_the_ticket_can_offer_tpsl() {
    assert!(bracket_has_wire_form());
    assert!(bracket_has_wire_form(), "the cached answer is the lift's answer");
}

/// `bracket_has_wire_form` is DERIVED from the lift, never a constant: whatever the lift answers
/// for a bracket, it answers too. Deliberately pins no VALUE — the lift's own bracket pin above is
/// the one pin, so this holds on both sides of the node-side bracket command (Ruling R8, I6).
#[test]
fn bracket_has_wire_form_answers_what_the_lift_answers() {
    for entry_price in [None, Some(100.0)] {
        let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Bracket(Box::new(
            vike_model::BracketSpec {
                venue: "okx".into(),
                symbol: "ETH-USDT".into(),
                side: -1,
                qty: 3.0,
                entry_price,
                stop_loss: 110.0,
                take_profit: 90.0,
            },
        )));
        assert_eq!(bracket_has_wire_form(), wire_from_command(&cmd).is_some(), "{cmd:?}");
    }
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
    assert_eq!((venue.as_str(), symbol.as_str(), interval.as_str()), ("binance", "BTCUSDT", "1m"));
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
        Some("CONTROL live — this observer can place REAL orders")
    );
    // A blank/whitespace error is treated as no error (no dangling " · last error: ").
    assert_eq!(
        control_status_line(true, true, Some("   "), None).as_deref(),
        Some("CONTROL live — this observer can place REAL orders")
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
        Some("CONTROL live — this observer can place REAL orders · last error: order denied")
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
    assert_eq!(line, "the build runner [LIVE] · CONTROL live — this observer can place REAL orders");
    assert!(line.starts_with("the build runner [LIVE]"), "got: {line}");
}

/// A paper daemon is named too, but with the lowercase `[paper]` tag — and the whole line must
/// carry no uppercase "LIVE" anywhere, so a glance can never read a paper daemon as live.
#[test]
fn status_line_with_paper_identity_names_daemon_without_uppercase_live() {
    let id = ident("sim-box", false);
    let line = control_status_line(true, true, None, Some(&id)).unwrap();
    assert_eq!(line, "sim-box [paper] · CONTROL live — this observer can place REAL orders");
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
        Some("CONTROL live — this observer can place REAL orders")
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
            symbol: String::new(),
            extra_symbols: Vec::new(),
            mode: None,
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
/// even against a backend that checks nothing; one it does not publish is held back, and the
/// refusal NAMES the venue it held back (it is the line the status strip paints).
#[test]
fn the_shell_call_sends_a_published_venue_and_holds_an_unpublished_one() {
    let snap = snap_with(&["polymarket", "binance"]);
    assert_eq!(may_send_to_backend(&wire_submit("binance"), &snap, false), Ok(()));
    let held = may_send_to_backend(&wire_submit("okx"), &snap, false);
    assert!(
        held.as_ref().is_err_and(|why| why.contains("okx")),
        "an order for a venue this backend does not publish lands on its PRIMARY book: {held:?}"
    );
    assert_eq!(
        may_send_to_backend(&wire_submit("okx"), &snap, true),
        Ok(()),
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
        assert_eq!(
            may_send_to_backend(&cmd, &empty, false),
            Ok(()),
            "{cmd:?} names no venue, so there is nothing to misroute"
        );
    }
}

// -----------------------------------------------------------------------------------------
// The remote write, end to end: a command the client does NOT send is LATCHED for the strip
// -----------------------------------------------------------------------------------------

/// A TP/SL bracket on binance, for the end-to-end send tests.
fn bracket_cmd() -> vike_exec::Command {
    vike_exec::Command::Order(vike_exec::OrderIntent::Bracket(Box::new(vike_model::BracketSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    })))
}

/// Drive [`send_to_backend`] with a stub `write` answering `answer`, and return what was WRITTEN
/// and what was LATCHED. The latch stands in for `RemoteControlHandle::latch_client_refusal`, whose
/// own persistence is pinned in `vike-tradehub-client`'s `tests/strategy_verbs.rs`
/// (`a_client_side_refusal_latches_until_dismissed_and_a_node_refusal_replaces_it`).
fn send_capturing(
    cmd: &vike_exec::Command,
    snap: &vike_core::CoreSnapshot,
    node_routes_by_venue: bool,
    answer: Result<(), ControlRejected>,
) -> (Vec<WireCommand>, Option<String>) {
    let mut written = Vec::new();
    let mut latched = None;
    send_to_backend(
        cmd,
        snap,
        node_routes_by_venue,
        |wire| {
            written.push(wire);
            answer
        },
        |line| latched = Some(line),
    );
    (written, latched)
}

/// What the strip's control segment paints for a latched `line`: the REAL
/// [`control_status_line`], with its real [`MAX_ERR_TAIL`] clip.
fn strip_paints(line: &str) -> String {
    control_status_line(true, true, Some(line), None).expect("a control channel is present")
}

/// ⚠ **I-1 of the node-bracket pre-flight: the refusal against an OLDER node reaches the screen.**
/// The desktop is built from `main` and the node runs the last release, so a node that predates
/// `bracket` is the NORMAL state the day this merges. The client refuses the verb before the queue
/// (`ControlRejected::UnsupportedByNode`), which latches nothing by itself; the send latches the
/// line, and the line reaches the strip WHOLE — the actionable part survives the strip's clip.
#[test]
fn a_bracket_an_older_node_cannot_take_latches_its_refusal_whole() {
    let (written, latched) = send_capturing(
        &bracket_cmd(),
        &snap_with(&["binance"]),
        true,
        Err(ControlRejected::UnsupportedByNode),
    );
    assert_eq!(written.len(), 1, "the lifted bracket reached the write: {written:?}");
    assert!(matches!(written[0], WireCommand::Bracket(_)), "{written:?}");
    let line = latched.expect("a command the client refused must be latched, never vanish");
    let word = format!("`{}`", vike_tradehub_client::proto::FEATURE_BRACKET);
    assert!(line.contains(&word), "the line names the missing capability {word}: {line}");
    assert!(line.contains("upgrade the node") && line.contains("NOT sent"), "{line}");
    assert!(strip_paints(&line).ends_with(&line), "the strip clips nothing of it: {line}");
}

/// A command the client ENQUEUES latches nothing: what the node then does with it arrives the
/// usual ways (the pushed snapshot, and the latch the node's own `Response::Error` writes).
#[test]
fn a_command_the_client_enqueues_latches_nothing() {
    let (written, latched) =
        send_capturing(&bracket_cmd(), &snap_with(&["binance"]), false, Ok(()));
    assert_eq!(latched, None);
    assert_eq!(written.len(), 1, "{written:?}");
}

/// The ROUTING gate's refusal is a client-side refusal too. Its reason runs to a paragraph, so the
/// line LEADS with the venue it held back: that is what survives the strip's clip. Nothing is
/// written.
#[test]
fn a_venue_the_routing_gate_holds_back_is_latched_venue_first() {
    let cmd = vike_exec::Command::Order(vike_exec::OrderIntent::Submit(Box::new(
        vike_model::OrderRequest { venue: "okx".into(), ..req("c-1") },
    )));
    let (written, latched) = send_capturing(&cmd, &snap_with(&["binance"]), false, Ok(()));
    assert!(written.is_empty(), "nothing goes on the wire: {written:?}");
    let line = latched.expect("a held-back venue must be latched");
    let painted = strip_paints(&line);
    assert!(painted.ends_with('…'), "the reason is clipped on the strip: {painted}");
    assert!(painted.contains("`okx`") && painted.contains("NOT sent"), "{painted}");
}

/// A command with no thin-wire form is not sent either, and is latched NAMING the command (m-d of
/// the Task 3 review): an operator who pressed the DOM's cancel-all must learn it was the CANCEL
/// that did not go. Nothing is written, and each line survives the strip's clip whole. A cancel of
/// ONE order says "1 order", not "1 orders" (the DOM's cancel-side on a one-order side).
#[test]
fn a_command_with_no_wire_form_is_latched_by_name_and_writes_nothing() {
    let margin = vike_exec::Command::SetMargin(Box::new(vike_exec::MarginUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        im_requirement: 0.1,
    }));
    let cancels = vike_exec::Command::Order(vike_exec::OrderIntent::CancelBatch(vec![
        "a".into(),
        "b".into(),
    ]));
    let one = vike_exec::Command::Order(vike_exec::OrderIntent::CancelBatch(vec!["a".into()]));
    for (cmd, said) in [
        (margin, "leverage change NOT sent"),
        (cancels, "cancel of 2 orders NOT sent"),
        (one, "cancel of 1 order NOT sent"),
    ] {
        let (written, latched) = send_capturing(&cmd, &snap_with(&["binance"]), true, Ok(()));
        assert!(written.is_empty(), "{written:?}");
        let line = latched.expect("a command with no wire form must be latched");
        assert!(line.starts_with(said), "{line}");
        assert!(strip_paints(&line).ends_with(&line), "{line}");
    }
}

/// Every client refusal has its own line, each survives the strip's clip whole, and none of them
/// blames the node for a full queue or a closed link: only `UnsupportedByNode` says "upgrade". The
/// full-queue line is in the PAST tense because it is latched: after a retry that went through it
/// must read as history, not as the state of the link.
#[test]
fn every_client_refusal_has_its_own_line() {
    let submit = wire_submit("binance");
    let lines = [
        refused_send_status(&submit, ControlRejected::UnsupportedByNode),
        refused_send_status(&submit, ControlRejected::Busy),
        refused_send_status(&submit, ControlRejected::Gone),
    ];
    for line in &lines {
        assert!(line.contains("NOT sent"), "{line}");
        assert!(strip_paints(line).ends_with(line.as_str()), "clipped on the strip: {line}");
    }
    assert!(lines[0].contains("upgrade the node"), "{}", lines[0]);
    assert!(!lines[1].contains("upgrade") && !lines[2].contains("upgrade"), "{lines:?}");
    assert!(lines[1].contains("was full"), "{}", lines[1]);
    assert_ne!(lines[1], lines[2]);
}
