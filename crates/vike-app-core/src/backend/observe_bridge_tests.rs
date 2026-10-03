use super::*;
use vike_exec::OrderStatus;
use vike_tradehub_client::wire::{
    WireBar, WireBarSeries, WireEngineMode, WireHeldOrderView, WireNodeIdentity, WireOrderView,
    WirePositionView, WireSnapshot, WireTradingState, WireVenueBlock,
};

/// A wire identity for the I3 observe-status tests — `name`, `live` and (since the daemon
/// started reporting which box it is) `advertise_addr` render. Empty here so every pre-existing
/// assertion below reads the DIAL address, which is what it did before the field existed;
/// `the_status_line_names_the_box_the_daemon_reported` is the one that fills it.
fn ident(name: &str, live: bool) -> WireNodeIdentity {
    WireNodeIdentity {
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
/// exhaustiveness pin: adding a variant to `vike_exec::OrderStatus` fails to compile right
/// here until it is added to BOTH the list and the arm — which then feeds the round-trip
/// assertions below. This is the audit's ask: a new variant can no longer silently fall
/// through `map_order_status`'s unknown arm and render as terminal `Rejected` in the observer
/// (the forward projection in vike-tradehub `publish.rs` renders `format!("{:?}", status)`,
/// so the Debug spelling IS the wire contract).
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

/// The projection's spelling (`format!("{:?}", status)`, per vike-tradehub `publish.rs`'s
/// `project_order`) round-trips IDENTICALLY for every variant — the Debug-spelling fallback
/// arm of `map_order_status`.
#[test]
fn every_status_round_trips_through_the_debug_spelling() {
    for s in all_order_statuses() {
        assert_eq!(map_order_status(&format!("{s:?}")), s, "Debug spelling of {s:?}");
    }
}

/// The canonical SCREAMING_SNAKE `as_str` spelling (`OrderStatus::parse`'s vocabulary) also
/// decodes for every variant — the first branch of `map_order_status`.
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
fn full_wire_snapshot() -> WireSnapshot {
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
                mode: None,
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
                mode: None,
            },
        ],
        orders: vec![WireOrderView {
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
    assert_eq!(snap.trading_state, vike_exec::TradingState::Reducing);
    assert_eq!(snap.balance.to_bits(), 10_000.0_f64.to_bits());
    assert_eq!(snap.balance_mode, vike_exec::BalanceMode::Delta);
    let recent: Vec<&str> = snap.recent_events.iter().map(|s| &**s).collect();
    assert_eq!(recent, vec!["OrderAccepted c-1"]);
    assert_eq!(snap.fault.as_deref(), Some("boom"));

    // Whole-struct comparisons (derived PartialEq) pin every carried field at once.
    assert_eq!(
        snap.orders,
        vec![vike_core::snapshot::OrderView {
            client_order_id: "c-1".into(),
            venue: "binance".into(),
            account: None,
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: "limit".into(),
            price: Some(59_000.0),
            trigger_price: None,
            status: OrderStatus::Accepted,
            venue_order_id: Some("v-9".into()),
            filled_qty: 0.25,
            avg_fill_px: 59_100.0,
        }]
    );
    assert_eq!(
        snap.positions,
        vec![vike_core::snapshot::PositionView {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            position_side: "BOTH".into(),
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
        vec![vike_core::snapshot::HeldOrderView {
            client_order_id: "c-2".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: "stop".into(),
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
    assert_eq!(snap.recon, vike_core::snapshot::ReconBlock::default());
    assert!(snap.recon_coin_deltas.is_empty());
}

/// `build_portfolio`'s primary-vs-summed derivation: the scalar fields MIRROR `venues[0]`
/// (never summed — exactly as `CoreSnapshot::build` populates them), while
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
    assert!(primary.multipliers.is_empty());
    assert_eq!(primary.multiplier_default.to_bits(), 1.0_f64.to_bits());
    assert_eq!(primary.trading_state, vike_exec::TradingState::Reducing);
    assert_eq!(primary.positions.len(), 1);

    let zero_equity = map_venueblock(&wire.venues[1]);
    assert_eq!(
        zero_equity.margin_ratio.to_bits(),
        0.0_f64.to_bits(),
        "equity <= 0 guards the division to 0.0"
    );
    assert_eq!(zero_equity.trading_state, vike_exec::TradingState::Halted);
}

/// The bridge keeps what the node said about each account: its label, its route key, the symbols
/// it trades and its mode (Trade window spec §4.3). A second account of one venue stays a second
/// account on the desktop.
#[test]
fn map_venueblock_keeps_the_account_its_symbols_and_its_mode() {
    let mut w = full_wire_snapshot().venues.remove(0);
    w.account = Some("SUB".into());
    w.route_key = "binance#SUB".into();
    w.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
    w.mode = Some(WireEngineMode::Live);
    let vb = map_venueblock(&w);
    assert_eq!(vb.account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
    assert_eq!(vb.route_key, "binance#SUB");
    assert_eq!(vb.symbol, "BTCUSDT");
    assert_eq!(vb.extra_symbols, vec!["ETHUSDT".to_string()]);
    assert_eq!(vb.mode, Some(vike_exec::EngineMode::Live));
    assert!(vb.trades("ETHUSDT"));
}

/// An older node says none of it: the block names no account, a bare route key, no symbols (no
/// primary and no extras) and no mode ("not said"), and nothing panics.
#[test]
fn map_venueblock_reads_an_older_node_as_not_said() {
    let mut w = full_wire_snapshot().venues.remove(0);
    w.account = None;
    w.route_key = String::new();
    w.symbols = Vec::new();
    w.mode = None;
    let vb = map_venueblock(&w);
    assert!(vb.account.is_none());
    assert_eq!(vb.route_key, vb.venue, "an empty wire key falls back to the venue");
    assert!(vb.symbol.is_empty() && vb.mode.is_none());
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
        "A".repeat(vike_model::account_keys::MAX_LABEL_LEN + 1),
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
/// is not a labelled account (the ones the block read drops) reads as the default account.
#[test]
fn map_order_keeps_the_account() {
    let mut o = full_wire_snapshot().orders.remove(0);
    o.account = Some("SUB".into());
    assert_eq!(map_order(&o).account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
    o.account = None;
    assert!(map_order(&o).account.is_none());
    for text in unlabelled_wire_spellings() {
        o.account = Some(text.clone());
        assert!(map_order(&o).account.is_none(), "{text:?} must read as the default account");
    }
}

/// ⚠ **`OrderView::account == None` means "the venue's default account", and the type cannot say
/// "unknown".** A node that predates the order's `account` key sends a labelled order with no
/// account, and it maps to `None` exactly as a default-account order does. The only tell is the
/// node's venue blocks: `WireVenueBlock::mode` ships in the same node release as the order's
/// `account`, so a node whose blocks carry a mode is one that names its orders' accounts, and a node
/// whose blocks carry none is not. The tell is the mode's PRESENCE, not its value: `Paper` is what an
/// engine says until something says otherwise, and it counts like the other two.
#[test]
fn map_order_none_means_default_and_a_blocks_mode_is_the_tell() {
    // A node that predates both: its block says no mode, and its order names no account.
    let mut older = full_wire_snapshot();
    older.venues[0].mode = None;
    older.orders[0].account = None;
    let older = wire_to_core(&older);
    assert!(older.portfolio.venues[0].mode.is_none(), "an older node's block says no mode");
    assert!(older.orders[0].account.is_none());

    // A node that names its orders' accounts: its block carries a mode, whichever one, and its
    // order names SUB. Every mode is mapped to itself, so a real-money account is never read as a
    // safer one nor a demo account as a real-money one.
    for (wire, core) in [
        (WireEngineMode::Paper, vike_exec::EngineMode::Paper),
        (WireEngineMode::Demo, vike_exec::EngineMode::Demo),
        (WireEngineMode::Live, vike_exec::EngineMode::Live),
    ] {
        let mut newer = full_wire_snapshot();
        newer.venues[0].mode = Some(wire);
        newer.orders[0].account = Some("SUB".into());
        let newer = wire_to_core(&newer);
        assert_eq!(newer.portfolio.venues[0].mode, Some(core));
        assert_eq!(
            newer.orders[0].account.as_ref().map(ToString::to_string).as_deref(),
            Some("SUB"),
            "an order on a node whose blocks say {wire:?} keeps its account"
        );
    }
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
        vec![vike_model::Bar {
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
        Some(vike_model::Bar {
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

/// Default OFF. `VIKE_TRADEHUB_CONTROL` unset ⇒ read-only observer, whatever key is present —
/// a key in `.env` must never be sufficient on its own.
#[test]
fn control_disabled_never_connects_even_with_a_valid_looking_key() {
    assert!(connect_control(false, NEVER_DIALLED, Some("a-real-looking-key")).is_none());
    assert!(connect_control(false, NEVER_DIALLED, None).is_none());
}

/// Enabled but no key ⇒ read-only. The observer must degrade, not dial with an empty secret.
#[test]
fn control_enabled_without_a_key_stays_read_only() {
    assert!(connect_control(true, NEVER_DIALLED, None).is_none());
}

/// A present-but-blank key (an empty or whitespace-only `.env` line — the realistic
/// mis-configuration) is treated as ABSENT, not as a zero-length secret to authenticate with.
#[test]
fn a_blank_or_whitespace_only_key_counts_as_absent() {
    for blank in ["", " ", "\t", "  \n "] {
        assert!(
            connect_control(true, NEVER_DIALLED, Some(blank)).is_none(),
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
        std::sync::Arc::new(std::sync::Mutex::new(String::new())),
        |_snap| {},
        || {},
    )
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
