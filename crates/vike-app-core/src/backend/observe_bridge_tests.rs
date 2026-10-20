use super::*;
use vike_model::OrderStatus;
use vike_tradehub_client::wire::{
    WireBar, WireBarSeries, WireHeldOrderView, WireHubIdentity, WireNotRunning, WireOrderView,
    WirePositionView, WireSnapshot, WireTradingState, WireVenueBlock,
};

/// A wire identity for the I3 observe-status tests — `name`, `live` and (since the daemon
/// started reporting which box it is) `advertise_addr` render. Empty here so every pre-existing
/// assertion below reads the DIAL address, which is what it did before the field existed;
/// `the_status_line_names_the_box_the_daemon_reported` is the one that fills it.
fn ident(name: &str, live: bool) -> WireHubIdentity {
    WireHubIdentity {
        name: name.into(),
        strategy: "spread_maker".into(),
        params: "{}".into(),
        live,
        build: "vike-tradehub 0.1.0 (abc1234)".into(),
        advertise_addr: String::new(),
    }
}

/// B3 (`identity` is `None` from an older node, or before the first frame): the identity-less
/// observe status.
///
/// ⚠ It is no longer BYTE-IDENTICAL to the pre-I3 string, and this test used to assert that it
/// was. The `(connected)` tail is what makes a healthy observe link classify — see
/// [`observing_status`]'s doc — and byte-identity with a string that classified as `Unknown` was
/// never the property worth keeping; the LEADING `OBSERVING <addr>`, which is what an operator
/// reads, is unchanged.
#[test]
fn observing_status_without_identity_names_the_addr_and_carries_the_token() {
    assert_eq!(
        observing_status(None, "127.0.0.1:9301", None),
        "OBSERVING 127.0.0.1:9301 (connected)"
    );
}

/// ⚠ **BEFORE THE FIRST FRAME the registry NAME is the only identity this side has**, and the
/// status line used to throw it away and print an address instead. Both the CI box listeners bind
/// loopback, so every thin client on every box read `OBSERVING 127.0.0.1:7879` — the same
/// string whichever production daemon it was attached to
/// (`crates/vike-app-core/src/backend/backend_identity.rs` carries the measurement).
#[test]
fn observing_status_leads_with_the_backend_name_before_a_frame_carries_one() {
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:7879", None),
        "the CI box — OBSERVING 127.0.0.1:7879 (connected)"
    );
}

/// …and the daemon's OWN self-report outranks the registry's label for it: it carries the
/// arming tag as well as a name, and it is the backend answering for itself.
#[test]
fn the_daemons_own_identity_outranks_the_registry_name() {
    let id = ident("the build runner", true);
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:7879", Some(&id)),
        "the build runner [LIVE] — OBSERVING 127.0.0.1:7879 (connected)"
    );
}

/// ⚠ **…and when the daemon reports WHICH BOX it is, that is the address the line names.**
/// `OBSERVING 127.0.0.1:7879` is the same string on every box for every daemon — the tunnel
/// mouth, not the far end — so a status bar printing it tells an operator nothing while
/// looking like it told them something. The choice is `backend_identity::shown_address`'s, so
/// this line and the Connections foot strip cannot name two different boxes at once.
///
/// (RFC 5737 documentation address: a real box's address must never reach a tracked file.)
#[test]
fn the_status_line_names_the_box_the_daemon_reported() {
    let mut id = ident("the build runner", true);
    id.advertise_addr = "203.0.113.7:7879".into();
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:7879", Some(&id)),
        "the build runner [LIVE] — OBSERVING 203.0.113.7:7879 (connected)"
    );
    // …and an old node, which reports nothing, keeps the pre-existing line exactly.
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:7879", Some(&ident("the build runner", true))),
        "the build runner [LIVE] — OBSERVING 127.0.0.1:7879 (connected)"
    );
}

/// An UNNAMED record adds nothing to the status line — no invented name, and no stand-in
/// headline in front of an address the line already shows. The strip is where the invitation
/// to name it belongs.
#[test]
fn an_unnamed_backend_adds_nothing_to_the_status_line() {
    assert_eq!(
        observing_status(None, "127.0.0.1:7879", None),
        "OBSERVING 127.0.0.1:7879 (connected)"
    );
    assert_eq!(
        observing_status(Some("   "), "127.0.0.1:7879", None),
        "OBSERVING 127.0.0.1:7879 (connected)",
        "a whitespace-only name is an absent one"
    );
}

/// ⚠⚠ **A NAME IS OPERATOR TEXT GOING INTO A STRING A CLASSIFIER READS.** `parse_feed_status`
/// substring-matches over the whole line with `disconnected`/`error`/`failed` winning outright,
/// so a backend the operator called `error-box` would paint a HEALTHY link's dot red — the dot
/// reporting the NAME instead of the link. [`led_by`] drops the lead rather than the truth.
///
/// ⚠ This covers the DAEMON's self-reported name too, which had the same hole before this
/// change and no test: a paper daemon calling itself `failed-over` classified `Error` on a
/// working connection.
#[test]
fn a_name_that_would_repaint_the_dot_is_dropped_rather_than_the_dot_being_wrong() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};
    for hostile in ["error-box", "disconnected-1", "failed-over", "fault-2"] {
        let line = observing_status(Some(hostile), "127.0.0.1:7879", None);
        assert_eq!(
            line, "OBSERVING 127.0.0.1:7879 (connected)",
            "a name that would change the classification is dropped from the line"
        );
        assert_eq!(parse_feed_status(&line), ConnectionState::Connected, "`{line}`");
        // …and the DAEMON-supplied half of the same hazard.
        let id = ident(hostile, false);
        let from_wire = observing_status(None, "127.0.0.1:7879", Some(&id));
        assert_eq!(parse_feed_status(&from_wire), ConnectionState::Connected, "`{from_wire}`");
    }
    // The dialling line is the same shape and the same rule: a hostile name must not make a
    // dial look like an error.
    let dialling = led_by(Some("error-box"), "connecting to 127.0.0.1:7879…");
    assert_eq!(parse_feed_status(&dialling), ConnectionState::Connecting, "`{dialling}`");
    // …while an ORDINARY name survives on that line, so the guard is not a blanket refusal.
    assert_eq!(
        led_by(Some("the CI box"), "connecting to 127.0.0.1:7879…"),
        "the CI box — connecting to 127.0.0.1:7879…"
    );
}

/// I3 (split-plane): once a frame carries identity, the observe status LEADS with the daemon
/// name and the loud uppercase `[LIVE]` tag — the read-only twin of the control line's rule.
#[test]
fn observing_status_with_live_identity_leads_with_name_and_uppercase_live() {
    let id = ident("the build runner", true);
    assert_eq!(
        observing_status(None, "127.0.0.1:9301", Some(&id)),
        "the build runner [LIVE] — OBSERVING 127.0.0.1:9301 (connected)"
    );
}

/// A paper daemon renders the lowercase `[paper]` tag and the line carries no uppercase
/// "LIVE" anywhere, so a glance can never read a paper daemon as live.
#[test]
fn observing_status_with_paper_identity_has_no_uppercase_live() {
    let id = ident("sim-box", false);
    let line = observing_status(None, "127.0.0.1:9301", Some(&id));
    assert_eq!(line, "sim-box [paper] — OBSERVING 127.0.0.1:9301 (connected)");
    assert!(!line.contains("LIVE"), "a paper daemon must never render LIVE: {line}");
}

/// ⚠ THE PAPERCUT, and the accident hiding underneath it.
///
/// A healthy observe link used to classify `Unknown` (grey dot, the word "Unknown" in the
/// Connections tool) — EXCEPT when the observed daemon was live, because `identity_label`'s
/// `[LIVE]` tag happens to contain a token in the shared parser's connected family. So the dot
/// answered "is the daemon armed", not "is the link up". All three spellings now classify
/// `Connected`, and the paper one is the case that proves the token rather than the coincidence
/// is doing the work.
#[test]
fn every_observe_connected_spelling_now_classifies_as_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};
    for line in [
        observing_status(None, "127.0.0.1:9301", None),
        observing_status(None, "127.0.0.1:9301", Some(&ident("the build runner", true))),
        observing_status(None, "127.0.0.1:9301", Some(&ident("sim-box", false))),
    ] {
        assert_eq!(parse_feed_status(&line), ConnectionState::Connected, "`{line}`");
    }
    // The coincidence, demonstrated on the strings as they were: the live one classified and
    // the paper one did not, from the same healthy link.
    assert_eq!(
        parse_feed_status("the build runner [LIVE] — OBSERVING 127.0.0.1:9301"),
        ConnectionState::Connected,
        "the pre-fix live spelling classified — on the word LIVE, not on any observe token"
    );
    assert_eq!(
        parse_feed_status("sim-box [paper] — OBSERVING 127.0.0.1:9301"),
        ConnectionState::Unknown,
        "…and the pre-fix paper spelling did not: same link, different colour"
    );
}

/// The other three lines [`spawn_bridge`]'s loop sets, classified through the same parser — so
/// the observe lane is now fully covered rather than covered in its healthy state only. These
/// are spelled from the loop's own `format!`s; a change there that broke one would leave this
/// red.
#[test]
fn the_other_observe_lines_still_classify_as_they_did() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};
    assert_eq!(parse_feed_status("connecting to 127.0.0.1:9301…"), ConnectionState::Connecting);
    assert_eq!(
        parse_feed_status("disconnected from 127.0.0.1:9301, reconnecting…"),
        ConnectionState::Disconnected
    );
    assert_eq!(
        parse_feed_status(
            "observe connect to 127.0.0.1:9301 failed (connection refused); retrying…"
        ),
        ConnectionState::Error
    );
}

/// Every `OrderStatus` variant. The no-wildcard `match` below is the COMPILE-TIME
/// exhaustiveness pin: adding a variant to `vike_model::OrderStatus` fails to compile right
/// here until it is added to BOTH the list and the arm — which then feeds the round-trip
/// assertions below. This is the audit's ask: a new variant can no longer silently fall
/// through `map_order_status`'s unknown arm and render as terminal `Rejected` in the observer
/// (the forward projection in vike-tradehub `publish.rs` renders `status.as_str()`; nodes built
/// before it rendered `format!("{:?}", status)`, so BOTH spellings are the wire contract).
fn all_order_statuses() -> Vec<OrderStatus> {
    use OrderStatus as S;
    let all = vec![
        S::Initialized,
        S::Submitted,
        S::Accepted,
        S::Triggered,
        S::PartiallyFilled,
        S::Filled,
        S::Canceled,
        S::Rejected,
        S::Denied,
        S::Expired,
        S::PendingCancel,
        S::Liquidated,
        S::Emulated,
        S::Released,
    ];
    for s in &all {
        match s {
            S::Initialized
            | S::Submitted
            | S::Accepted
            | S::Triggered
            | S::PartiallyFilled
            | S::Filled
            | S::Canceled
            | S::Rejected
            | S::Denied
            | S::Expired
            | S::PendingCancel
            | S::Liquidated
            | S::Emulated
            | S::Released => {}
        }
    }
    all
}

/// The OLDER nodes' spelling (`format!("{:?}", status)`, what vike-tradehub `publish.rs`'s
/// `project_order` sent before it sent `as_str`) round-trips IDENTICALLY for every variant — the
/// Debug-spelling fallback arm of `map_order_status`, kept for a desktop observing an older node.
#[test]
fn every_status_round_trips_through_the_debug_spelling() {
    for s in all_order_statuses() {
        assert_eq!(map_order_status(&format!("{s:?}")), s, "Debug spelling of {s:?}");
    }
}

/// The canonical SCREAMING_SNAKE `as_str` spelling (`OrderStatus::parse`'s vocabulary, what the
/// node sends) decodes for every variant — the first branch of `map_order_status`.
#[test]
fn every_status_round_trips_through_the_screaming_snake_spelling() {
    for s in all_order_statuses() {
        assert_eq!(map_order_status(s.as_str()), s, "as_str spelling of {s:?}");
    }
}

/// An unknown wire status decodes to terminal `Rejected` — never a live/actionable state.
/// Wrong-case spellings of real variants are unknown too (both real vocabularies are
/// exact-match).
#[test]
fn unknown_status_falls_back_to_rejected() {
    assert_eq!(map_order_status("BOGUS_STATUS"), OrderStatus::Rejected);
    assert_eq!(map_order_status(""), OrderStatus::Rejected);
    assert_eq!(map_order_status("accepted"), OrderStatus::Rejected);
    assert_eq!(map_order_status("FILLED "), OrderStatus::Rejected);
}

/// A fully-populated two-venue wire snapshot: venue "binance" is the PRIMARY (scalar mirror),
/// venue "bybit" exists to prove the summed-vs-mirrored split and the `equity <= 0` ratio arm.
pub(crate) fn full_wire_snapshot() -> WireSnapshot {
    let pos = WirePositionView {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        position_side: "BOTH".into(),
        size: 0.5,
        avg_px: 60_000.0,
        unrealized: 2_345.67,
        leverage: 5.0,
        liq_price: 48_000.0,
    };
    WireSnapshot {
        seq: 42,
        accounts_epoch: 0,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trading_state: WireTradingState::Reducing,
        balance: 10_000.0,
        equity_total: 12_400.0,
        venues: vec![
            WireVenueBlock {
                venue: "binance".into(),
                balance: 10_000.0,
                realized_pnl: 250.5,
                fees_paid: 3.25,
                funding_paid: -1.5,
                equity: 12_345.67,
                unrealized: 2_345.67,
                missing_prices: 1,
                margin_used: 500.0,
                free_bp: 11_845.67,
                trading_state: WireTradingState::Reducing,
                positions: vec![pos.clone()],
                account: None,
                route_key: "binance".into(),
                symbols: Vec::new(),
                tier: None,
                contract_values: Vec::new(),
                admits_symbols: false,
                quote_rates: Vec::new(),
                quote_rates_in: String::new(),
                ledger_ccy: String::new(),
            },
            WireVenueBlock {
                venue: "bybit".into(),
                balance: 100.0,
                realized_pnl: -20.0,
                fees_paid: 1.0,
                funding_paid: 0.5,
                equity: 0.0, // exercises the `equity <= 0 -> margin_ratio 0.0` arm
                unrealized: 0.0,
                missing_prices: 2,
                margin_used: 400.0,
                free_bp: 0.0,
                trading_state: WireTradingState::Halted,
                positions: Vec::new(),
                account: None,
                route_key: "bybit".into(),
                symbols: Vec::new(),
                tier: None,
                contract_values: Vec::new(),
                admits_symbols: false,
                quote_rates: Vec::new(),
                quote_rates_in: String::new(),
                ledger_ccy: String::new(),
            },
        ],
        orders: vec![WireOrderView {
            data_delay: None,
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: "limit".into(),
            price: Some(59_000.0),
            trigger_price: None,
            status: "Accepted".into(),
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.25,
            avg_fill_px: 59_100.0,
            reason: None,
            age_ms: None,
        }],
        positions: vec![pos],
        held_exits: vec![WireHeldOrderView {
            client_order_id: "c-2".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: "stop".into(),
            price: None,
            trigger_price: Some(55_000.0),
            parent_order_id: Some("c-1".into()),
        }],
        recent_events: vec!["OrderAccepted c-1".into()],
        fault: Some("boom".into()),
        bars: vec![WireBarSeries {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            closed: vec![WireBar {
                ts: 1_000,
                o: 60_000.0,
                h: 60_100.0,
                l: 59_900.0,
                c: 60_050.0,
                v: 12.5,
            }],
            forming: Some(WireBar {
                ts: 61_000,
                o: 60_050.0,
                h: 60_080.0,
                l: 60_020.0,
                c: 60_070.0,
                v: 3.2,
            }),
        }],
        identity: None,
        not_running: Vec::new(),
        key_changed: Vec::new(),
        recon: Vec::new(),
        uptime_ms: None,
        halt_sentinel: None,
        primary_ccy: String::new(),
    }
}

/// The top-level scalars, order fields, position fields (with the documented off-path
/// defaults), and held-exit fields all map through `wire_to_core`; the observer-only fields
/// (`marks`/`mounts`/`recon`/counters) stay empty/zero.
#[test]
fn wire_to_core_maps_orders_positions_and_held_exits() {
    let snap = wire_to_core(&full_wire_snapshot());

    assert_eq!(snap.seq, 42);
    assert_eq!(snap.venue, "binance");
    assert_eq!(snap.symbol, "BTCUSDT");
    assert_eq!(snap.trading_state, vike_model::TradingState::Reducing);
    assert_eq!(snap.balance.to_bits(), 10_000.0_f64.to_bits());
    assert_eq!(snap.balance_mode, vike_exec::BalanceMode::Delta);
    let recent: Vec<&str> = snap.recent_events.iter().map(|s| &**s).collect();
    assert_eq!(recent, vec!["OrderAccepted c-1"]);
    assert_eq!(snap.fault.as_deref(), Some("boom"));

    // Whole-struct comparisons (derived PartialEq) pin every carried field at once.
    assert_eq!(
        snap.orders,
        vec![vike_exec::OrderView {
            data_delay: None,
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: vike_model::OrderType::Limit,
            price: Some(59_000.0),
            trigger_price: None,
            status: OrderStatus::Accepted,
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.25,
            avg_fill_px: 59_100.0,
            reason: None,
        }]
    );
    assert_eq!(
        snap.positions,
        vec![vike_exec::PositionView {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: vike_model::events::PositionSide::Both,
            size: 0.5,
            avg_px: 60_000.0,
            unrealized: 2_345.67,
            mark_source: None, // off-path default (resolver-only)
            leverage: 5.0,
            liq_price: 48_000.0,
            margin_mode: vike_model::MarginMode::Cross, // off-path default
            isolated_margin: None,                      // off-path default
        }]
    );
    assert_eq!(
        snap.held_exits,
        vec![vike_exec::HeldOrderView {
            client_order_id: "c-2".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: vike_model::OrderType::Stop,
            price: None,
            trigger_price: Some(55_000.0),
            parent_order_id: Some("c-1".into()),
        }]
    );

    // Observer-only voids: no local core state.
    assert!(snap.marks.is_empty());
    assert!(snap.mounts.is_empty());
    assert_eq!(snap.conflated_market_drops, 0);
    assert_eq!(snap.rejected_commands, 0);
    assert_eq!(snap.recon, vike_exec::ReconBlock::default());
    assert!(snap.recon_coin_deltas.is_empty());
}

/// `build_portfolio`'s primary-vs-summed derivation: the scalar fields MIRROR `venues[0]`
/// (never summed — exactly as `vike_core::snapshot::build` populates them), while
/// `margin_used_total`/`missing_prices_total` ARE cross-venue sums and `equity_total` is
/// carried from the wire aggregate.
#[test]
fn build_portfolio_mirrors_primary_and_sums_the_totals() {
    let wire = full_wire_snapshot();
    let p = build_portfolio(&wire);

    assert_eq!(p.equity.to_bits(), 12_345.67_f64.to_bits(), "mirrors venues[0], not summed");
    assert_eq!(p.equity_total.to_bits(), 12_400.0_f64.to_bits(), "the wire aggregate");
    assert_eq!(p.realized_pnl.to_bits(), 250.5_f64.to_bits());
    assert_eq!(p.fees_paid.to_bits(), 3.25_f64.to_bits());
    assert_eq!(p.funding_paid.to_bits(), (-1.5_f64).to_bits());
    assert_eq!(p.margin_used_total.to_bits(), 900.0_f64.to_bits(), "500 + 400 summed");
    assert_eq!(p.missing_prices_total, 3, "1 + 2 summed");
    assert!(p.balances_by_asset.is_empty(), "not on the wire");
    assert_eq!(p.venues.len(), 2);
}

/// An empty wire snapshot (no venues) yields the 0.0 primary-mirror fields — the `unwrap_or`
/// arms, never a panic. The `to_bits` comparisons pin POSITIVE zero: `margin_used_total`
/// must come from the naive fold (std's float `Sum` empty identity is `-0.0`, which would
/// diverge bitwise from `CoreSnapshot::empty`'s `Portfolio::default()` placeholder).
#[test]
fn build_portfolio_with_no_venues_zeroes_the_primary_mirror() {
    let p = build_portfolio(&WireSnapshot::empty());
    assert_eq!(p.equity.to_bits(), 0.0_f64.to_bits());
    assert_eq!(p.realized_pnl.to_bits(), 0.0_f64.to_bits());
    assert_eq!(p.margin_used_total.to_bits(), 0.0_f64.to_bits());
    assert_eq!(p.missing_prices_total, 0);
    assert!(p.venues.is_empty());
}

/// ⚠ **The remote GUI adds one currency only** (decision 0183, item 13): a wire frame whose second block
/// is in EUR gives a portfolio whose `margin_used_total` is the PRIMARY group's, whose blocks carry
/// their group, and whose `ledger_totals` keeps the EUR figure on its own row.
#[test]
fn build_portfolio_adds_the_primary_currency_group_only() {
    let mut wire = full_wire_snapshot();
    wire.venues[1].ledger_ccy = "EUR".into();
    let p = build_portfolio(&wire);

    assert_eq!(
        p.margin_used_total.to_bits(),
        500.0_f64.to_bits(),
        "the dollar block's 500, not +400"
    );
    assert_eq!(p.venues[0].ledger_ccy, "");
    assert_eq!(p.venues[1].ledger_ccy, "EUR");
    assert!(p.is_multi_currency());
    let rows = p.ledger_totals();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].margin_used.to_bits(), p.margin_used_total.to_bits());
    assert_eq!(rows[1].margin_used.to_bits(), 400.0_f64.to_bits(), "the EUR block's own 400");
    // The count of unpriced positions is not money: it is the whole node's.
    assert_eq!(p.missing_prices_total, 3);
}

/// ⚠ **The observer's primary is the node's, not the frame's first block** (the restart drill: a
/// `[binance USDT, oanda EUR, bybit USDT]` node whose binance stopped and started again publishes
/// `[oanda, bybit, binance']`). `equity_total` is the dollar group's, so the margin sum, the panel's headline,
/// the strip and the listed groups must all be the DOLLAR group's too, with the EUR block listed beside them.
#[test]
fn the_observer_takes_the_primary_group_the_node_states_not_the_first_blocks() {
    let mut wire = full_wire_snapshot();
    wire.venues.swap(0, 1); // [bybit (400), binance (500)]: the EUR block leads
    wire.venues[0].ledger_ccy = "EUR".into();
    wire.primary_ccy = String::new(); // the node's primary is the dollar group

    let p = build_portfolio(&wire);
    assert_eq!(p.primary_ccy(), "", "the stated key, not the first block's EUR");
    assert_eq!(p.margin_used_total.to_bits(), 500.0_f64.to_bits(), "the dollar block alone");
    assert_eq!(p.ledger_ccys().iter().map(|c| c.to_string()).collect::<Vec<_>>(), ["", "EUR"]);
    let rows = p.ledger_totals();
    assert_eq!(rows[0].margin_used.to_bits(), p.margin_used_total.to_bits());

    let snap = wire_to_core(&wire);
    let panel = crate::orders::equity_panel::EquityPanelModel::from_snapshot(&snap);
    assert_eq!(panel.ccy, "USD");
    assert_eq!(panel.primary, "");
    assert_eq!(panel.other_ledgers.len(), 1);
    assert_eq!(panel.other_ledgers[0].ccy, "EUR");
    assert_eq!(panel.other_ledgers[0].margin_used.to_bits(), 400.0_f64.to_bits());
    let strip = crate::ui::tool_views::hero_totals(&panel.rows, panel.primary);
    assert_eq!(strip.realized.to_bits(), panel.rows[1].realized.to_bits(), "the dollar row alone");

    // A EUR primary is the EUR block, wherever it sits in the frame.
    wire.primary_ccy = "EUR".into();
    let p = build_portfolio(&wire);
    assert_eq!(p.primary_ccy(), "EUR");
    assert_eq!(p.margin_used_total.to_bits(), 400.0_f64.to_bits(), "the EUR block alone");
    let panel = crate::orders::equity_panel::EquityPanelModel::from_snapshot(&wire_to_core(&wire));
    assert_eq!((panel.ccy, panel.other_ledgers[0].ccy), ("EUR", "USD"));
}

/// ⚠ **An old peer still renders.** A frame from a node that predates `ledger_ccy` has no such key in any
/// block: it decodes (the key defaults empty), every block is the dollar group, and the panel and the
/// portfolio read as they did before the field existed, with no second headline.
#[test]
fn a_frame_without_ledger_ccy_decodes_and_renders_as_one_group() {
    let mut json = serde_json::to_value(full_wire_snapshot()).expect("serializes");
    for block in json["venues"].as_array_mut().expect("venues") {
        assert!(
            block.as_object_mut().expect("block").remove("ledger_ccy").is_none(),
            "none written"
        );
    }
    let wire: WireSnapshot = serde_json::from_value(json).expect("an older node's frame decodes");
    assert!(wire.venues.iter().all(|v| v.ledger_ccy.is_empty()));

    let snap = wire_to_core(&wire);
    assert!(!snap.portfolio.is_multi_currency());
    assert_eq!(
        snap.portfolio.margin_used_total.to_bits(),
        900.0_f64.to_bits(),
        "both blocks, as before"
    );
    let panel = crate::orders::equity_panel::EquityPanelModel::from_snapshot(&snap);
    assert!(panel.other_ledgers.is_empty());
    assert_eq!(panel.ccy, "USD");
    assert_eq!(panel.rows.len(), 2);
}

/// `map_venueblock` recomputes `margin_ratio` from the wired `margin_used`/`equity` (the wire
/// does not carry it), guards the `equity <= 0` arm to 0.0, and defaults the GUI-irrelevant
/// core internals (`balance_mode` Delta / `fee_schedule` None / empty 1.0-default multiplier
/// grid).
#[test]
fn map_venueblock_recomputes_margin_ratio_and_defaults_the_internals() {
    let wire = full_wire_snapshot();
    let primary = map_venueblock(&wire.venues[0]);
    assert_eq!(
        primary.margin_ratio.to_bits(),
        (500.0_f64 / 12_345.67_f64).to_bits(),
        "recomputed margin_used / equity"
    );
    assert_eq!(primary.balance_mode, vike_exec::BalanceMode::Delta);
    assert_eq!(primary.fee_schedule, None);
    assert!(primary.contract_values.is_empty());
    assert_eq!(primary.contract_value_default, vike_model::ContractValue::default());
    assert_eq!(primary.trading_state, vike_model::TradingState::Reducing);
    assert_eq!(primary.positions.len(), 1);

    let zero_equity = map_venueblock(&wire.venues[1]);
    assert_eq!(
        zero_equity.margin_ratio.to_bits(),
        0.0_f64.to_bits(),
        "equity <= 0 guards the division to 0.0"
    );
    assert_eq!(zero_equity.trading_state, vike_model::TradingState::Halted);
}

/// The bridge keeps what the node said about each account: its label, its route key, the symbols
/// it trades and its tier (Trade window spec §4.3). A second account of one venue stays a second
/// account on the desktop.
#[test]
fn map_venueblock_keeps_the_account_its_symbols_and_its_tier() {
    let mut w = full_wire_snapshot().venues.remove(0);
    w.account = Some("SUB".into());
    w.route_key = "binance#SUB".into();
    w.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    w.tier = Some("live".into());
    let vb = map_venueblock(&w);
    assert_eq!(vb.account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
    assert_eq!(vb.route_key, "binance#SUB");
    assert_eq!(vb.symbol, "BTCUSDT");
    assert_eq!(vb.extra_symbols, vec!["ETHUSDT".to_string()]);
    assert_eq!(vb.tier, Some(vike_model::AccountTier::Live));
    assert!(vb.trades("ETHUSDT"));
}

/// An older node says none of it: the block names no account, a bare route key, no symbols (no
/// primary and no extras) and no tier ("not said"), and nothing panics.
#[test]
fn map_venueblock_reads_an_older_node_as_not_said() {
    let mut w = full_wire_snapshot().venues.remove(0);
    w.account = None;
    w.route_key = String::new();
    w.symbols = Vec::new();
    w.tier = None;
    let vb = map_venueblock(&w);
    assert!(vb.account.is_none());
    assert_eq!(vb.route_key, vb.venue, "an empty wire key falls back to the venue");
    assert!(vb.symbol.is_empty() && vb.tier.is_none());
    assert!(vb.extra_symbols.is_empty());
    assert!(!vb.trades("BTCUSDT"));
}

/// The wire spellings of an account that must NOT come out as a labelled one, shared by the block
/// read and the order read so the two cannot drift apart:
/// - `DEFAULT`, the reserved word for the unlabelled account. `parse_wire_account` accepts it and
///   answers `AccountLabel::Default`; the bridge's `.filter(|l| !l.is_default())` is what turns that
///   into `None`, the convention `VenueBlock::account` and `OrderView::account` both state. Without
///   it a `Some(Default)` is a second spelling of the unlabelled account that no reader expects.
/// - the empty label, and a label one past `MAX_LABEL_LEN`, which `parse_wire_account` refuses.
/// - a label with characters no label has.
fn unlabelled_wire_spellings() -> Vec<String> {
    vec![
        "DEFAULT".to_string(),
        String::new(),
        "A".repeat(vike_model::accounts::account_keys::MAX_LABEL_LEN + 1),
        "not a label!".to_string(),
    ]
}

/// A spelling that is not a labelled account cannot become one: a malformed label and the reserved
/// word for the default account both read as the default (unlabelled) account, `None`.
#[test]
fn map_venueblock_drops_a_malformed_label() {
    let mut w = full_wire_snapshot().venues.remove(0);
    for text in unlabelled_wire_spellings() {
        w.account = Some(text.clone());
        assert!(map_venueblock(&w).account.is_none(), "{text:?} must read as the default account");
    }
}

/// An order keeps the account it rests in, an unlabelled one stays unlabelled, and a spelling that
/// is not a labelled account (the ones the block read drops) maps to `None`, the only spelling this
/// type has for it. That `None` is never ATTRIBUTED as the default account's: see
/// `an_order_whose_label_this_client_cannot_read_is_attributed_to_no_window`.
#[test]
fn map_order_keeps_the_account() {
    let mut o = full_wire_snapshot().orders.remove(0);
    o.account = Some("SUB".into());
    assert_eq!(map_order(&o).account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
    o.account = None;
    assert!(map_order(&o).account.is_none());
    for text in unlabelled_wire_spellings() {
        o.account = Some(text.clone());
        assert!(map_order(&o).account.is_none(), "{text:?} names no labelled account");
    }
}

/// Decision 0140: the node's delay word reads back as the label the order row shows; no word, and a
/// word this client cannot name (a newer node's), read as no label — never a guess. And the round
/// trip through the wire's JSON keeps it.
#[test]
fn map_order_reads_the_delay_word_back_and_an_unknown_one_as_no_label() {
    use vike_model::DataDelay;
    let mut o = full_wire_snapshot().orders.remove(0);
    assert_eq!(map_order(&o).data_delay, None, "an unlabelled order (an old node) has no label");
    for (word, label) in [
        ("delayed", DataDelay::DELAYED),
        ("frozen", DataDelay::Frozen),
        ("real-time", DataDelay::RealTime),
    ] {
        o.data_delay = Some(word.into());
        let back: WireOrderView =
            serde_json::from_str(&serde_json::to_string(&o).expect("serializes")).expect("parses");
        assert_eq!(map_order(&back).data_delay, Some(label), "{word}");
    }
    o.data_delay = Some("stale-ish".into());
    assert_eq!(map_order(&o).data_delay, None);
}

/// ⚠ **`OrderView::account == None` means "the venue's default account", and the type cannot say
/// "unknown".** A node that predates the order's `account` key sends a labelled order with no
/// account, and it maps to `None` exactly as a default-account order does. The only tell is the
/// node's venue blocks: `WireVenueBlock::tier` ships only in node releases that also send the
/// order's `account`, so a node whose blocks carry a tier is one that names its orders' accounts,
/// and a node whose blocks carry none is not. The tell is the tier's PRESENCE, not its value:
/// `paper` counts like the other two.
#[test]
fn map_order_none_means_default_and_a_blocks_tier_is_the_tell() {
    // A node that predates both: its block says no tier, and its order names no account.
    let mut older = full_wire_snapshot();
    older.venues[0].tier = None;
    older.orders[0].account = None;
    let older = wire_to_core(&older);
    assert!(older.portfolio.venues[0].tier.is_none(), "an older node's block says no tier");
    assert!(older.orders[0].account.is_none());

    // A node that names its orders' accounts: its block carries a tier, whichever one, and its
    // order names SUB. Every tier word is read as itself, so a real-money account is never read as
    // a safer one nor a demo account as a real-money one.
    for core in vike_model::AccountTier::ALL {
        let mut newer = full_wire_snapshot();
        newer.venues[0].tier = Some(core.as_str().into());
        newer.orders[0].account = Some("SUB".into());
        let newer = wire_to_core(&newer);
        assert_eq!(newer.portfolio.venues[0].tier, Some(core));
        assert_eq!(
            newer.orders[0].account.as_ref().map(ToString::to_string).as_deref(),
            Some("SUB"),
            "an order on a node whose blocks say {core:?} keeps its account"
        );
    }
}

/// A tier word this build does not know is "not said" (`MODE ?`), never PAPER; the strict parser
/// is the one edge where the word becomes a tier (decision 0129).
#[test]
fn a_tier_word_this_build_does_not_know_is_not_said() {
    assert_eq!(map_tier(Some("live")), Some(vike_model::AccountTier::Live));
    assert_eq!(map_tier(None), None);
    for word in ["staging", "", "sim", "mainnet"] {
        assert_eq!(map_tier(Some(word)), None, "{word:?}");
    }
}

/// Decision 0156: the accounts the node says it does NOT run cross the bridge with their route
/// key, their row's tier and the node's reason word, so the Trade window can say why one is not
/// running. An older node sends none: the list stays empty.
#[test]
fn the_accounts_the_node_does_not_run_cross_the_bridge_with_their_reason() {
    let mut w = full_wire_snapshot();
    assert!(wire_to_core(&w).not_running.is_empty(), "CONTROL: a node that names none");
    w.not_running = vec![
        WireNotRunning {
            venue: "okx".into(),
            account: None,
            route_key: "okx".into(),
            tier: Some("live".into()),
            reason: "no credentials".into(),
        },
        WireNotRunning {
            venue: "binance".into(),
            account: Some("SUB".into()),
            route_key: "binance#SUB".into(),
            tier: Some("demo".into()),
            reason: "flags.tradehub_live off".into(),
        },
    ];
    let snap = wire_to_core(&w);
    let seen: Vec<_> = snap
        .not_running
        .iter()
        .map(|n| {
            (
                n.venue.as_str(),
                n.account.as_ref().map(ToString::to_string),
                n.route_key.as_str(),
                n.tier,
                n.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("okx", None, "okx", Some(vike_model::AccountTier::Live), "no credentials"),
            (
                "binance",
                Some("SUB".to_string()),
                "binance#SUB",
                Some(vike_model::AccountTier::Demo),
                "flags.tradehub_live off",
            ),
        ]
    );
}

/// M-9 of the final review (slice B): an order whose wire account label this client cannot parse
/// (a label-grammar skew between node and client) is attributed to NO window. `OrderView::account`
/// cannot spell "unknown", so the bridge maps it to `None` like a default-account order; what keeps
/// it off the default window's markers and Cancel all is the tell it leaves on its venue: the block
/// of the engine it rests in carries the same unparseable label in its route key, so no order of
/// that venue is attributed at all (`order_dispatch::orders_name_their_account`). The order itself
/// stays in the snapshot, so the Account window still lists it.
#[test]
fn an_order_whose_label_this_client_cannot_read_is_attributed_to_no_window() {
    use crate::orders::order_dispatch::{
        TradeAddress, orders_name_their_account, owns_order, window_orders,
    };
    let mut w = full_wire_snapshot();
    w.venues.truncate(1);
    w.venues[0].tier = Some("live".into());
    w.venues[0].symbols = vec!["BTCUSDT".into()];
    let mut skewed = w.venues[0].clone();
    skewed.account = Some("sub-1".into());
    skewed.route_key = "binance#sub-1".into();
    skewed.tier = Some("demo".into());
    w.venues.push(skewed);
    w.orders[0].account = Some("sub-1".into());
    let snap = wire_to_core(&w);
    assert_eq!(
        snap.orders.len(),
        1,
        "the order stays in the snapshot: the Account window lists it"
    );
    let default = TradeAddress::parse("binance", None, "BTCUSDT").expect("an address");
    assert!(
        !orders_name_their_account(&snap, "binance"),
        "the venue's orders cannot be attributed"
    );
    assert!(!owns_order(&snap, &default)(&snap.orders[0]), "never the default account's order");
    assert_eq!(window_orders(&snap, &default).count(), 0, "no marker, no Cancel all");

    // CONTROL: the same node with a label this client reads attributes the order to its account.
    let mut readable = w.clone();
    readable.venues[1].account = Some("SUB".into());
    readable.venues[1].route_key = "binance#SUB".into();
    readable.orders[0].account = Some("SUB".into());
    let snap = wire_to_core(&readable);
    let sub = TradeAddress::parse("binance", Some("SUB"), "BTCUSDT").expect("an address");
    assert!(orders_name_their_account(&snap, "binance"));
    assert_eq!(window_orders(&snap, &sub).count(), 1, "SUB's window marks its order");
    assert_eq!(window_orders(&snap, &default).count(), 0);
}

/// The bounded wire bar tail is rebuilt into the core's `(venue, symbol, interval)` bar cache:
/// closed bars + the forming candle, with the symbol stamped and the wire-absent `Bar` fields
/// (`funding`/`bid`/`ask`) `None`.
#[test]
fn wire_to_core_rebuilds_the_bar_cache() {
    let snap = wire_to_core(&full_wire_snapshot());
    let key = ("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string());
    let series = snap.bars.get(&key).expect("wire series lands under its (v, s, i) key");
    assert_eq!(
        *series.closed,
        vec![vike_marketdata::Bar {
            ts: 1_000,
            open: 60_000.0,
            high: 60_100.0,
            low: 59_900.0,
            close: 60_050.0,
            volume: 12.5,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".into()),
        }]
    );
    assert_eq!(
        series.forming,
        Some(vike_marketdata::Bar {
            ts: 61_000,
            open: 60_050.0,
            high: 60_080.0,
            low: 60_020.0,
            close: 60_070.0,
            volume: 3.2,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("BTCUSDT".into()),
        })
    );
}

// ── connect_control: the refuse-paths of a gate that guards REAL order placement ─────────
//
// Every case below must return `None` WITHOUT dialling, so the address is a deliberately
// unroutable placeholder: if a refactor ever lets one of these reach `RemoteControlHandle::
// connect`, the test hangs/fails instead of silently arming a write path. The connecting
// path itself needs a live daemon and stays uncovered here.
const NEVER_DIALLED: &str = "127.0.0.1:0";

/// Default OFF. `flags.tradehub_control` off ⇒ read-only observer, whatever key is present —
/// a key must never be sufficient on its own.
#[test]
fn control_disabled_never_connects_even_with_a_valid_looking_key() {
    assert!(
        connect_control(false, NEVER_DIALLED, Some("a-real-looking-key"), patience()).is_none()
    );
    assert!(connect_control(false, NEVER_DIALLED, None, patience()).is_none());
}

/// Enabled but no key ⇒ read-only. The observer must degrade, not dial with an empty secret.
#[test]
fn control_enabled_without_a_key_stays_read_only() {
    assert!(connect_control(true, NEVER_DIALLED, None, patience()).is_none());
}

/// A present-but-blank key (an empty or whitespace-only `.env` line — the realistic
/// mis-configuration) is treated as ABSENT, not as a zero-length secret to authenticate with.
#[test]
fn a_blank_or_whitespace_only_key_counts_as_absent() {
    for blank in ["", " ", "\t", "  \n "] {
        assert!(
            connect_control(true, NEVER_DIALLED, Some(blank), patience()).is_none(),
            "{blank:?} must not arm the control channel"
        );
    }
}

// ── spawn_bridge: the stop handle (split-plane B6) ───────────────────────────────────────
//
// Both tests dial a DEAD loopback address (bind an ephemeral port, then drop the listener),
// so every connect attempt is refused instantly and the thread spends its life in the dial
// backoff — exactly where an unsliced 2s sleep would park a `stop()` for the full window.

/// Bind-then-drop an ephemeral loopback port: nothing listens there afterwards, so
/// `RemoteCoreHandle::connect` fails FAST (refused) instead of hanging the test on a dial.
fn dead_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("ephemeral local_addr").to_string();
    drop(listener);
    addr
}

fn spawn_dead_bridge() -> BridgeHandle {
    spawn_bridge(
        dead_addr(),
        Some("test-backend".into()),
        "test-key".into(),
        // The row's default poll. A dead address never reaches the poll loop, so this value is
        // not what these tests are about — `nap_unless_stopped`'s own tests below are.
        std::time::Duration::from_millis(50),
        patience(),
        std::sync::Arc::new(std::sync::Mutex::new(String::new())),
        |_snap| {},
        || {},
    )
}

// ── nap_unless_stopped: the one sliced wait (split-plane B6, and the observe-poll row) ─────────

/// A stop raised BEFORE the wait skips it entirely: even a 30-second total returns at once, so a
/// bridge told to stop never sleeps a poll it no longer owes.
#[test]
fn a_nap_that_starts_stopped_returns_at_once() {
    let t0 = std::time::Instant::now();
    nap_unless_stopped(std::time::Duration::from_secs(30), &|| true);
    let elapsed = t0.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(5), "slept {elapsed:?} despite a stop");
}

/// **A stop raised mid-nap lands within a slice, however long the poll is.** This is the property
/// that lets `preferences.observe_poll_ms` run to a full second without making a backend switch —
/// which joins the bridge thread on the GUI's frame thread — wait out a whole poll: the wait is
/// naps of at most `STOP_SLICE` with the flag re-read between them.
#[test]
fn a_stop_raised_mid_nap_ends_a_long_poll_within_a_slice() {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let raiser = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        })
    };
    let t0 = std::time::Instant::now();
    nap_unless_stopped(std::time::Duration::from_secs(30), &|| {
        flag.load(std::sync::atomic::Ordering::Relaxed)
    });
    let elapsed = t0.elapsed();
    raiser.join().expect("the raiser thread");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a 30s nap took {elapsed:?} to notice a stop — it is not sliced"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(50),
        "returned after {elapsed:?}, before the stop was raised"
    );
}

/// With nothing raising a stop, the nap sleeps AT LEAST the whole total — in slices, the last one
/// shortened to fit — so a poll of 120ms (not a multiple of the slice) is not rounded down to two.
#[test]
fn an_unstopped_nap_sleeps_the_whole_total_even_when_it_is_not_a_slice_multiple() {
    let total = std::time::Duration::from_millis(120);
    let t0 = std::time::Instant::now();
    nap_unless_stopped(total, &|| false);
    let elapsed = t0.elapsed();
    assert!(elapsed >= total, "slept {elapsed:?}, less than the {total:?} it owed");
}

/// The default poll is exactly one slice, so at the default the nap is the single 50ms sleep the
/// connected loop always took, and the slice is the same constant the dial backoff is walked in.
#[test]
fn the_default_poll_is_one_slice() {
    assert_eq!(
        std::time::Duration::from_millis(u64::from(
            vike_config::preferences::DEFAULT_OBSERVE_POLL_MS
        )),
        STOP_SLICE
    );
}

/// `stop()` returns well under the 2s dial backoff: the backoff sleep is SLICED with the
/// flag re-checked between slices, so a mid-backoff stop parks for ~one slice, never the
/// whole window. The 1s bound is 20x the slice and half the backoff — a regression to one
/// unsliced sleep fails it deterministically.
#[test]
fn stop_returns_well_under_the_dial_backoff() {
    let mut handle = spawn_dead_bridge();
    // Let the thread through its first (refused) dial and into the backoff sleep.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let t0 = std::time::Instant::now();
    handle.stop();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "stop() took {elapsed:?} — the backoff sleep is not sliced"
    );
    handle.stop(); // idempotent: the join handle is already taken; returns immediately
}

/// Dropping the handle stops the thread too (`Drop` = `stop()`), so a caller that simply
/// lets it fall out of scope — the B1 runtime backend switch — leaks no dialling thread.
#[test]
fn drop_stops_the_bridge_thread() {
    let handle = spawn_dead_bridge();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let stop_flag = handle.stop.clone(); // same-module test: reach the private flag
    let t0 = std::time::Instant::now();
    drop(handle);
    let elapsed = t0.elapsed();
    assert!(stop_flag.load(std::sync::atomic::Ordering::Relaxed), "drop must raise the stop flag");
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "drop took {elapsed:?} — Drop must stop-and-join promptly"
    );
}

// ── the operator's link patience reaches the dial ────────────────────────────────────────
//
// `preferences.observe_stall_timeout_ms` and `preferences.control_reply_timeout_ms` are read by the
// BINARY and handed down as a `LinkPatience`; these tests hold that the two windows reach the
// right socket, against a scripted node that goes silent with its socket held open (no FIN, no
// RST — the silent death the windows exist for). At the default 45 s / 30 s every one of them
// would run past its own deadline, so each is its own kill proof.

use vike_tradehub_client::liveness::LinkPatience;

fn patience() -> LinkPatience {
    LinkPatience::default()
}

/// The scripted node's observe/control split, and how long it holds the silent socket open: far
/// past every assertion, so a test cannot pass because the node finally hung up.
const SILENT_HOLD: std::time::Duration = std::time::Duration::from_secs(20);

/// A one-connection node on loopback speaking the REAL handshake and advertising the observe
/// heartbeat (the window is armed only against such a node). `observe`: after `AuthOk` it reads
/// the `Subscribe`, pushes ONE snapshot and goes silent; otherwise it reads ONE command and goes
/// silent. The socket is HELD in both cases.
fn silent_node(observe: bool) -> String {
    use vike_tradehub_client::proto::{
        FEATURE_OBSERVE_HEARTBEAT, NODE_PROTO_VERSION, Request, Response, read_frame, write_frame,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let nonce = [29u8; 32];
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        let features = vec![FEATURE_OBSERVE_HEARTBEAT.to_string()];
        write_frame(
            &mut stream,
            &Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce, features },
        )
        .expect("welcome");
        let Ok(Request::Auth { scope, .. }) = read_frame::<_, Request>(&mut stream) else {
            return;
        };
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("authok");
        if observe {
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Subscribe { .. }) => {}
                other => panic!("expected Subscribe, got {other:?}"),
            }
            let mut snap = vike_tradehub_client::wire::WireSnapshot::empty();
            snap.seq = 3;
            write_frame(&mut stream, &Response::SnapshotFrame(Box::new(snap))).expect("one frame");
        } else {
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Command { .. }) => {}
                other => panic!("expected the one Command, got {other:?}"),
            }
        }
        std::thread::sleep(SILENT_HOLD);
    });
    addr
}

/// The bridge dials with the observe window it was GIVEN: a stream that goes silent is ended at
/// `patience.observe_stall` (here 300 ms) and the status line says the link dropped. ⚠ KILL PROOF:
/// dial with the constant instead of `patience.observe_stall` and the status never changes inside
/// the 5 s wait (the default is 45 s).
#[test]
fn the_bridge_ends_a_silent_stream_at_the_observe_window_it_was_given() {
    let addr = silent_node(true);
    let status = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut handle = spawn_bridge(
        addr.clone(),
        None,
        "bridge-patience-key".into(),
        std::time::Duration::from_millis(50),
        LinkPatience::from_millis(300, 30_000),
        status.clone(),
        |_snap| {},
        || {},
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = String::new();
    while std::time::Instant::now() < deadline {
        seen = status.lock().unwrap().clone();
        if seen.contains("disconnected from") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    handle.stop();
    assert!(
        seen.contains(&format!("disconnected from {addr}")),
        "the silent stream must be ended at the 300 ms window the bridge was given; status: {seen:?}"
    );
}

/// The control channel waits for a reply the window it was GIVEN: a command the node read and
/// never answered resolves as outcome-UNKNOWN at `patience.control_reply` (here 300 ms), not at
/// the 30 s default. ⚠ KILL PROOF: dial with the constant and the ticket resolves to nothing inside
/// the 5 s wait.
#[test]
fn the_control_channel_gives_a_command_up_at_the_reply_window_it_was_given() {
    use vike_tradehub_client::wire::{WireCommand, WireTradingState};
    let addr = silent_node(false);
    let handle = connect_control(
        true,
        &addr,
        Some("control-patience-key"),
        LinkPatience::from_millis(45_000, 300),
    )
    .expect("the scripted node completes the real handshake");
    let ticket = handle
        .try_command(WireCommand::SetTradingState(WireTradingState::Active))
        .expect("the queue is open");
    assert_eq!(
        handle.await_outcome(ticket, std::time::Duration::from_secs(5)),
        Some(vike_tradehub_client::CommandOutcome::Disconnected),
        "the node read the command and went silent: UNKNOWN at the 300 ms window"
    );
}

/// `vike-config` restates the two built-in windows and their ceilings (it is layer 20 and cannot
/// name this layer's constants), so THIS test is the pin: the defaults ARE the liveness constants,
/// the floor keeps the observe window at two heartbeats or more, the control ceiling is the one the
/// one-shot replies' own assertion puts on a reply wait, and the whole accepted range keeps every
/// relation the liveness assertions hold. A floor changed on either side reddens here, by name.
#[test]
fn the_link_patience_bounds_restate_the_liveness_constants() {
    use std::time::Duration;
    use vike_config::preferences::{
        DEFAULT_CONTROL_REPLY_TIMEOUT_MS, DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
        MAX_CONTROL_REPLY_TIMEOUT_MS, MAX_OBSERVE_STALL_TIMEOUT_MS,
    };
    use vike_tradehub_client::liveness::{
        CONTROL_REPLY_TIMEOUT, OBSERVE_HEARTBEAT, OBSERVE_READ_TIMEOUT,
    };
    let ms = |v: u32| Duration::from_millis(u64::from(v));
    assert_eq!(ms(DEFAULT_OBSERVE_STALL_TIMEOUT_MS), OBSERVE_READ_TIMEOUT);
    assert_eq!(ms(DEFAULT_CONTROL_REPLY_TIMEOUT_MS), CONTROL_REPLY_TIMEOUT);
    assert_eq!(
        LinkPatience::from_millis(
            DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
            DEFAULT_CONTROL_REPLY_TIMEOUT_MS
        ),
        LinkPatience::default(),
        "a box with neither row dials with today's windows"
    );
    // The control ceiling is the one-shot replies' own upper bound (ten minutes).
    assert_eq!(ms(MAX_CONTROL_REPLY_TIMEOUT_MS), Duration::from_secs(600));
    // The observe ceiling is five minutes: past it a dead link outlives a person's attention.
    assert_eq!(ms(MAX_OBSERVE_STALL_TIMEOUT_MS), Duration::from_secs(300));
    for observe in [DEFAULT_OBSERVE_STALL_TIMEOUT_MS, MAX_OBSERVE_STALL_TIMEOUT_MS] {
        assert!(ms(observe) >= 2 * OBSERVE_HEARTBEAT, "at least two missed heartbeats");
    }
}

/// **Every production dial in this crate passes the operator's patience.** `connect` is the
/// default-window door for a caller with no setting to read; a library site that dials through it
/// would silently ignore both rows. Scans this crate's non-test sources, comments excluded.
#[test]
fn no_production_dial_in_this_crate_bypasses_the_link_patience() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read a source directory").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !name.ends_with("_tests") && name != "tests" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(
        files.len() > 20,
        "the walk found {} files — it is looking in the wrong place",
        files.len()
    );
    // Assembled, because this file's own text is a test module the walk skips and a literal would
    // still read as data in a reviewer's search.
    let needles =
        [concat!("RemoteCoreHandle", "::connect("), concat!("RemoteControlHandle", "::connect(")];
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in needles {
                assert!(
                    !line.contains(needle),
                    "{}:{} dials through `connect`, which ignores the operator's link patience; \
                     pass `LinkPatience` through connect_with_read_timeout / \
                     connect_with_reply_timeout",
                    file.display(),
                    n + 1
                );
            }
        }
    }
}

/// The node's refusal reason reads back onto the core `OrderView` (the desktop's order rows), through
/// the wire's JSON; a node that sent none reads as none.
#[test]
fn map_order_reads_the_refusal_reason_back() {
    let mut o = full_wire_snapshot().orders.remove(0);
    assert_eq!(map_order(&o).reason, None, "an order a node did not refuse has no reason");
    o.reason = Some("INVALID_REQUEST: more digits than symbol allows".into());
    let back: WireOrderView =
        serde_json::from_str(&serde_json::to_string(&o).expect("serializes")).expect("parses");
    assert_eq!(
        map_order(&back).reason.as_deref(),
        Some("INVALID_REQUEST: more digits than symbol allows")
    );
}

/// Decision 0170: the node's non-default contract values become the observer's grid, so the order
/// ticket sizes an inverse contract by its quantity; a kind this build does not know is dropped, not
/// guessed, and a node that sends none leaves every symbol at the default.
#[test]
fn the_nodes_contract_values_become_the_observers_grid() {
    use vike_tradehub_client::wire::WireContractValue;
    let mut block = full_wire_snapshot().venues.remove(0);
    block.contract_values = vec![
        WireContractValue { symbol: "BTC-PERPETUAL".into(), kind: "inverse".into(), per_unit: 1.0 },
        WireContractValue { symbol: "ESZ6".into(), kind: "linear".into(), per_unit: 50.0 },
        WireContractValue { symbol: "X".into(), kind: "quanto".into(), per_unit: 3.0 },
        WireContractValue { symbol: "Z".into(), kind: "inverse".into(), per_unit: 0.0 },
    ];
    let mapped = map_venueblock(&block);
    assert_eq!(mapped.contract_value_of("BTC-PERPETUAL"), vike_model::ContractValue::inverse(1.0));
    assert_eq!(mapped.contract_value_of("ESZ6"), vike_model::ContractValue::linear(50.0));
    assert_eq!(mapped.contract_value_of("X"), vike_model::ContractValue::default(), "unknown kind");
    assert_eq!(mapped.contract_value_of("BTCUSDT"), vike_model::ContractValue::default());
    assert_eq!(
        mapped.contract_value_of("Z"),
        vike_model::ContractValue::inverse(1.0),
        "a zero unit is folded to 1.0, the kind kept"
    );
}

/// **The node's conversion-rate table becomes the observer's** (decision 0183, phase 1): the rows, their age and
/// the currency they are in are read verbatim, a stale flag the node set stays set, and a rate that is not a
/// positive number is shown STALE rather than hidden (the observer folds a degenerate contract value to the
/// default; a rate it cannot trust must say so). A node that sends none leaves the table empty.
#[test]
fn the_nodes_rate_table_becomes_the_observers_and_a_bad_rate_reads_stale() {
    use vike_tradehub_client::wire::WireQuoteRate;
    let mut block = full_wire_snapshot().venues.remove(0);
    let none = map_venueblock(&block);
    assert!(none.quote_rates.is_empty() && none.quote_rates_in.is_empty());
    assert!(!none.quote_rates_stale());

    block.quote_rates_in = "USD".into();
    block.quote_rates = vec![
        WireQuoteRate { ccy: "GBP".into(), rate: 1.35, as_of_ms: 100, stale: false },
        WireQuoteRate { ccy: "JPY".into(), rate: 1.0 / 150.0, as_of_ms: 50, stale: true },
        WireQuoteRate { ccy: "CHF".into(), rate: 0.0, as_of_ms: 100, stale: false },
        WireQuoteRate { ccy: "CAD".into(), rate: f64::NAN, as_of_ms: 100, stale: false },
    ];
    let mapped = map_venueblock(&block);
    assert_eq!(mapped.quote_rates_in, "USD");
    let got: Vec<(&str, bool)> =
        mapped.quote_rates.iter().map(|r| (r.ccy.as_str(), r.stale)).collect();
    assert_eq!(got, [("GBP", false), ("JPY", true), ("CHF", true), ("CAD", true)]);
    assert_eq!(mapped.quote_rates[0].rate, 1.35, "a good rate is read as sent");
    assert!(mapped.quote_rates_stale());
}

/// **The "trade any symbol" capability, as the desktop reads it off the wire** (the node x client
/// matrix, client half): a block is built exactly as the node writes it, serialized, decoded and
/// mapped, then asked whether the desktop may send a symbol its engine was not started on.
///
/// * NEW node: binance, bybit, aster and okx publish `admits_symbols`, so the window and the dispatcher
///   take the picked symbol; deribit publishes nothing and keeps one symbol (okx on a node that predates its row does the same).
/// * OLD node (v0.1.63): the key is not in the frame at all, so every venue, binance included, reads
///   one symbol only. Such a node would Ack a foreign symbol and sign it on the mounted one.
#[test]
fn the_desktop_sends_a_foreign_symbol_only_to_a_node_that_says_it_admits_symbols() {
    use crate::orders::order_dispatch::{TradeAddress, tradable};
    use crate::orders::symbol_follow::{OneSymbol, one_symbol};

    // The block as the NODE sends it for a demo account mounted on `mounted`.
    let decoded = |venue: &str, mounted: &str, admits: bool| {
        let mut block = full_wire_snapshot().venues.remove(0);
        block.venue = venue.into();
        block.route_key = venue.into();
        block.symbols = vec![mounted.into()];
        block.tier = Some("demo".into());
        block.admits_symbols = admits;
        let json = serde_json::to_string(&block).expect("serializes");
        assert_eq!(
            json.contains("admits_symbols"),
            admits,
            "key written only when offered: {json}"
        );
        let wire: WireVenueBlock = serde_json::from_str(&json).expect("a node's block decodes");
        let mut snap = vike_exec::CoreSnapshot::empty(venue, mounted);
        snap.portfolio.venues = vec![map_venueblock(&wire)];
        snap
    };
    let foreign = |venue: &str, symbol: &str| TradeAddress {
        venue: venue.into(),
        account: None,
        symbol: symbol.into(),
    };

    for (venue, mounted, other) in [
        ("bybit", "BTCUSDT", "ETHUSDT"),
        ("binance", "BTCUSDT", "ETHUSDT"),
        ("aster", "BTCUSDT.P", "ETHUSDT.P"),
    ] {
        let new_node = decoded(venue, mounted, true);
        assert_eq!(one_symbol(&new_node.portfolio.venues[0]), None, "{venue}: new node");
        assert!(tradable(&new_node, &foreign(venue, other)), "{venue}: new node, foreign symbol");
        assert!(tradable(&new_node, &foreign(venue, mounted)), "{venue}: the mounted one too");

        let old_node = decoded(venue, mounted, false);
        assert_eq!(
            one_symbol(&old_node.portfolio.venues[0]),
            Some(OneSymbol::NodeDoesNotOffer),
            "{venue}: old node"
        );
        assert!(!tradable(&old_node, &foreign(venue, other)), "{venue}: old node, foreign symbol");
        assert!(tradable(&old_node, &foreign(venue, mounted)), "{venue}: the mounted one still");
    }
    for (venue, mounted, why) in [
        ("okx", "BTC-USDT-SWAP", OneSymbol::EnginePending),
        ("deribit", "BTC-PERPETUAL", OneSymbol::Deribit),
    ] {
        let node = decoded(venue, mounted, false);
        assert_eq!(one_symbol(&node.portfolio.venues[0]), Some(why), "{venue}");
        assert!(!tradable(&node, &foreign(venue, "ETH-USDT-SWAP")), "{venue}: foreign symbol");
    }
}
