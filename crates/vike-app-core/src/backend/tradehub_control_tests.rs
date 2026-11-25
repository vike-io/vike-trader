use super::*;
use std::assert_matches;
use vike_tradehub_client::{
    ControlRejected,
    wire::{CommandWire, OrderRequestWire},
};

fn req(order_client_id: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.5,
        order_type: vike_model::OrderType::Limit,
        price: Some(59_000.0),
        trigger_price: None,
        reduce_only: true,
        ..Default::default()
    }
}

/// The three roles, each in its own words: read-only says the plain `OBSERVING`; a writing role says what it can do.
#[test]
fn access_line_says_each_role_in_its_own_words() {
    assert_eq!(access_line(BackendAccess::Observe, None, true, None), "OBSERVING");
    assert_eq!(
        access_line(BackendAccess::Control, None, true, None),
        "CONTROL: can place REAL orders"
    );
    assert_eq!(access_line(BackendAccess::Admin, None, true, None), "ADMIN: can place REAL orders");
    // The node said who: the writing role names the person and the device.
    assert_eq!(
        access_line(BackendAccess::Admin, Some("owner/vike"), true, None),
        "ADMIN (owner/vike): can place REAL orders"
    );
    assert_eq!(
        access_line(BackendAccess::Control, Some("alice/pc"), true, None),
        "CONTROL (alice/pc): can place REAL orders"
    );
    assert_eq!(access_label(BackendAccess::Observe, Some("alice/pc")), "OBSERVING");
    // A blank/whitespace error is treated as no error (no dangling " · last error: ").
    assert_eq!(
        access_line(BackendAccess::Control, None, true, Some("   ")),
        "CONTROL: can place REAL orders"
    );
}

/// The words never name the daemon: the connection line before them already does, once.
#[test]
fn access_line_never_repeats_the_daemon() {
    for access in [BackendAccess::Observe, BackendAccess::Control, BackendAccess::Admin] {
        for connected in [true, false] {
            let line = access_line(access, None, connected, Some("order denied"));
            assert!(!line.contains('[') && !line.contains("tradehub"), "{line}");
        }
    }
}

fn admitted(role: &str) -> vike_hub_protocol::handshake::Admitted {
    vike_hub_protocol::handshake::Admitted {
        person: "owner".into(),
        device: "vike".into(),
        role: role.into(),
    }
}

/// The node's own answer decides the role: `admin` is ADMIN, `control` CONTROL, each with who.
#[test]
fn the_nodes_answer_decides_the_role_and_names_who() {
    assert_eq!(
        control_access(Some(&admitted("admin"))),
        (BackendAccess::Admin, Some("owner/vike".to_string()))
    );
    assert_eq!(
        control_access(Some(&admitted("control"))),
        (BackendAccess::Control, Some("owner/vike".to_string()))
    );
}

/// An OLD node's answer carries no role: the channel it let through is Write, so CONTROL, as before.
#[test]
fn an_old_nodes_answer_falls_back_to_control() {
    assert_eq!(control_access(None), (BackendAccess::Control, None));
    let (access, who) = control_access(None);
    assert_eq!(access_line(access, who.as_deref(), true, None), "CONTROL: can place REAL orders");
}

/// The connect's ONE log line names who the node admitted and its role; no line of it is the old WARN's
/// "this observer can place/cancel REAL orders on the remote daemon".
#[test]
fn the_connect_line_names_the_device_and_its_role() {
    assert_eq!(
        connected_line(Some(&admitted("admin")), None),
        "connected as owner/vike, role admin: can place REAL orders"
    );
    assert_eq!(
        connected_line(None, Some("owner/vike")),
        "connected as owner/vike: CONTROL: can place REAL orders"
    );
    assert_eq!(
        connected_line(None, None),
        "connected with this desktop's device key: CONTROL: can place REAL orders"
    );
}

#[test]
fn access_line_disconnected_shows_disconnected() {
    assert_eq!(access_line(BackendAccess::Control, None, false, None), "CONTROL disconnected");
    assert_eq!(access_line(BackendAccess::Admin, None, false, None), "ADMIN disconnected");
}

#[test]
fn access_line_appends_error_tail_on_either_connection_state() {
    assert_eq!(
        access_line(BackendAccess::Control, None, true, Some("order denied")),
        "CONTROL: can place REAL orders · last error: order denied"
    );
    assert_eq!(
        access_line(BackendAccess::Control, None, false, Some("auth denied")),
        "CONTROL disconnected · last error: auth denied"
    );
}

#[test]
fn access_line_truncates_a_long_error_tail() {
    let long = "x".repeat(MAX_ERR_TAIL + 40);
    let out = access_line(BackendAccess::Control, None, false, Some(&long));
    let expected_tail: String = format!("{}…", "x".repeat(MAX_ERR_TAIL));
    assert!(out.ends_with(&expected_tail), "got: {out}");
    // A tail exactly at the cap is NOT truncated (no ellipsis).
    let exact = "y".repeat(MAX_ERR_TAIL);
    let out2 = access_line(BackendAccess::Control, None, false, Some(&exact));
    assert!(out2.ends_with(&exact) && !out2.ends_with('…'), "got: {out2}");
}

// -----------------------------------------------------------------------------------------
// The shell's ONE call
// -----------------------------------------------------------------------------------------

fn snapshot_with(venues: &[&str]) -> vike_execution::CoreSnapshot {
    let mut snapshot = vike_execution::CoreSnapshot::empty("", "");
    snapshot.portfolio.venues = venues
        .iter()
        .map(|v| vike_execution::VenueBlock {
            account_code: None,
            order_client_id_shape: None,
            venue: (*v).to_string(),
            account: None,
            route_key: (*v).to_string(),
            symbol: String::new(),
            extra_symbols: Default::default(),
            tier: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: vike_execution::BalanceMode::Delta,
            contract_values: Default::default(),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Default::default(),
            ledger_currency: Default::default(),
            currency_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_buying_power: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: vike_model::TradingState::Active,
            positions: Vec::new(),
        })
        .collect();
    snapshot
}

fn wire_submit(venue: &str) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: venue.into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// The roster comes off the snapshot's per-ENGINE blocks, in their published order.
#[test]
fn the_backend_roster_is_read_off_the_published_engine_blocks() {
    assert_eq!(
        backend_engine_venues(&snapshot_with(&["polymarket", "binance"])),
        vec!["polymarket".to_string(), "binance".to_string()]
    );
    assert!(
        backend_engine_venues(&vike_execution::CoreSnapshot::empty("", "")).is_empty(),
        "a snapshot with no engine blocks is the observer's pre-first-frame placeholder"
    );
}

// -----------------------------------------------------------------------------------------
// The remote write, end to end: a command the client does NOT send is LATCHED for the strip
// -----------------------------------------------------------------------------------------

/// A TP/SL bracket on binance, for the end-to-end send tests.
fn bracket_command() -> vike_execution::Command {
    vike_execution::Command::Order(vike_execution::OrderIntent::Bracket(Box::new(
        vike_model::OrderBracket {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
            order_client_id: None,
        },
    )))
}

/// Drive [`send_to_backend`] with a stub `write` answering `answer`, and return what was WRITTEN
/// and what was LATCHED. The latch stands in for `TradehubControlHandle::latch_client_refusal`, whose
/// own persistence is pinned in `vike-tradehub-client`'s `tests/strategy_verbs.rs`
/// (`a_client_side_refusal_latches_until_dismissed_and_a_node_refusal_replaces_it`).
fn send_capturing(
    command: &vike_execution::Command,
    snapshot: &vike_execution::CoreSnapshot,
    node_routes_by_venue: bool,
    answer: Result<(), ControlRejected>,
) -> (Vec<CommandWire>, Option<String>) {
    let mut written = Vec::new();
    let mut latched = None;
    send_to_backend(
        command,
        snapshot,
        node_routes_by_venue,
        |wire| {
            written.push(wire);
            answer
        },
        |line| latched = Some(line),
    );
    (written, latched)
}

/// A command that was WRITTEN returns the write's ticket (the shell keeps it to read the node's
/// answer to an order); one the client held back or the queue refused returns none, and latched
/// its line instead.
#[test]
fn a_written_command_returns_its_ticket_and_a_refused_one_returns_none() {
    let snapshot = snapshot_with(&["binance"]);
    let sent = send_to_backend(
        &bracket_command(),
        &snapshot,
        true,
        |_| Ok::<u8, ControlRejected>(7),
        |_| {},
    );
    assert_eq!(sent, Some(7));
    let mut latched = None;
    let busy = send_to_backend(
        &bracket_command(),
        &snapshot,
        true,
        |_| Err::<u8, _>(ControlRejected::Busy),
        |line| latched = Some(line),
    );
    assert_eq!(busy, None);
    assert!(latched.is_some(), "the refusal is still latched");
}

/// What the strip's access words paint for a latched `line`: the REAL [`access_line`], with its
/// real [`MAX_ERR_TAIL`] clip.
fn strip_paints(line: &str) -> String {
    access_line(BackendAccess::Control, None, true, Some(line))
}

/// ⚠ **I-1 of the node-bracket pre-flight: the refusal against an OLDER node reaches the screen.**
/// The desktop is built from `main` and the node runs the last release, so a node that predates
/// `bracket` is the NORMAL state the day this merges. The client refuses the verb before the queue
/// (`ControlRejected::UnsupportedByHub`), which latches nothing by itself; the send latches the
/// line, and the line reaches the strip WHOLE — the actionable part survives the strip's clip.
#[test]
fn a_bracket_an_older_node_cannot_take_latches_its_refusal_whole() {
    let (written, latched) = send_capturing(
        &bracket_command(),
        &snapshot_with(&["binance"]),
        true,
        Err(ControlRejected::UnsupportedByHub),
    );
    assert_eq!(written.len(), 1, "the lifted bracket reached the write: {written:?}");
    assert_matches!(written[0], CommandWire::Bracket(_), "{written:?}");
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
        send_capturing(&bracket_command(), &snapshot_with(&["binance"]), false, Ok(()));
    assert_eq!(latched, None);
    assert_eq!(written.len(), 1, "{written:?}");
}

/// The ROUTING gate's refusal is a client-side refusal too. Its reason runs to a paragraph, so the
/// line LEADS with the venue it held back: that is what survives the strip's clip. Nothing is
/// written.
#[test]
fn a_venue_the_routing_gate_holds_back_is_latched_venue_first() {
    let command = vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(
        vike_model::OrderRequest { venue: "okx".into(), ..req("c-1") },
    )));
    let (written, latched) = send_capturing(&command, &snapshot_with(&["binance"]), false, Ok(()));
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
    let margin = vike_execution::Command::SetMargin(Box::new(vike_execution::MarginUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        initial_margin_requirement: 0.1,
    }));
    let cancels = vike_execution::Command::Order(vike_execution::OrderIntent::CancelBatch(vec![
        "a".into(),
        "b".into(),
    ]));
    let one =
        vike_execution::Command::Order(vike_execution::OrderIntent::CancelBatch(vec!["a".into()]));
    for (command, said) in [
        (margin, "leverage change NOT sent"),
        (cancels, "cancel of 2 orders NOT sent"),
        (one, "cancel of 1 order NOT sent"),
    ] {
        let (written, latched) =
            send_capturing(&command, &snapshot_with(&["binance"]), true, Ok(()));
        assert!(written.is_empty(), "{written:?}");
        let line = latched.expect("a command with no wire form must be latched");
        assert!(line.starts_with(said), "{line}");
        assert!(strip_paints(&line).ends_with(&line), "{line}");
    }
}

/// Every client refusal has its own line, each survives the strip's clip whole, and none of them
/// blames the node for a full queue or a closed link: only `UnsupportedByHub` says "upgrade". The
/// full-queue line is in the PAST tense because it is latched: after a retry that went through it
/// must read as history, not as the state of the link.
#[test]
fn every_client_refusal_has_its_own_line() {
    let submit = wire_submit("binance");
    let lines = [
        refused_send_status(&submit, ControlRejected::UnsupportedByHub),
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
