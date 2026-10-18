use super::*;
use crate::cmd::hubkeys::OBSERVE_KEY_ENV;
use vike_tradehub_client::proto::starting_error;
use vike_tradehub_client::wire::{
    WireDirectoryAccount, WireHubIdentity, WirePositionView, WireReconAlert,
};

const NODE: &str = "<host>:7879";

fn parsed(args: &[&str]) -> Result<Args, args::Stop> {
    parse(args.iter().map(|s| s.to_string()))
}

// ---- synthetic nodes ----

/// An engine block: `venue` plus an optional label, at `tier`, in `state`.
fn block(venue: &str, label: Option<&str>, tier: &str, state: WireTradingState) -> WireVenueBlock {
    WireVenueBlock {
        venue: venue.to_string(),
        balance: 1000.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        equity: 1000.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_bp: 1000.0,
        trading_state: state,
        positions: Vec::new(),
        account: label.map(str::to_string),
        route_key: route_key(venue, label),
        symbols: vec!["BTCUSDT".to_string()],
        tier: Some(tier.to_string()),
        contract_values: Vec::new(),
        admits_symbols: false,
    }
}

fn position(symbol: &str, size: f64) -> WirePositionView {
    WirePositionView {
        venue: "bybit".to_string(),
        symbol: symbol.to_string(),
        position_side: "BOTH".to_string(),
        size,
        avg_px: 2414.0,
        unrealized: 12.5,
        leverage: 2.0,
        liq_price: 0.0,
    }
}

fn order(coid: &str, status: &str) -> WireOrderView {
    WireOrderView {
        client_order_id: coid.to_string(),
        venue: "bybit".to_string(),
        account: Some("DEMO".to_string()),
        symbol: "ETHUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(2000.0),
        trigger_price: None,
        status: status.to_string(),
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        data_delay: None,
        reason: None,
        age_ms: None,
    }
}

/// A resting order the node has known for `age_ms`.
fn aged(coid: &str, status: &str, age_ms: u64) -> WireOrderView {
    WireOrderView { age_ms: Some(age_ms), ..order(coid, status) }
}

fn recon_row(id: u64, kind: &str) -> WireReconAlert {
    WireReconAlert {
        id,
        account: "bybit#DEMO".to_string(),
        kind: kind.to_string(),
        detail: format!("detail of {id}\nsecond line"),
        proposed_event_count: 1,
    }
}

fn not_running(venue: &str, label: Option<&str>, tier: Option<&str>, why: &str) -> WireNotRunning {
    WireNotRunning {
        venue: venue.to_string(),
        account: label.map(str::to_string),
        route_key: route_key(venue, label),
        tier: tier.map(str::to_string),
        reason: why.to_string(),
    }
}

/// A current node that folded: up a day, no HALT sentinel, two accounts, both Active, and nothing
/// wrong. [`older`] is the same node as a build that predates the frame's newer fields.
fn healthy() -> WireSnapshot {
    let mut s = WireSnapshot::empty();
    s.seq = 41;
    s.uptime_ms = Some(DAY_MS);
    s.halt_sentinel = Some(false);
    s.identity = Some(WireHubIdentity {
        name: "node-a".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: true,
        build: "vike-tradehub 0.1.72 (abc1234, clean)".to_string(),
        advertise_addr: String::new(),
    });
    s.venues = vec![
        block("binance", None, "demo", WireTradingState::Active),
        block("bybit", Some("DEMO"), "demo", WireTradingState::Active),
    ];
    s
}

const HOUR_MS: u64 = 60 * 60 * 1000;
const DAY_MS: u64 = 24 * HOUR_MS;

/// [`healthy`] as a node that predates `uptime_ms`, `halt_sentinel` and `age_ms` sent it.
fn older() -> WireSnapshot {
    WireSnapshot { uptime_ms: None, halt_sentinel: None, ..healthy() }
}

/// The directory that agrees with [`healthy`].
fn directory() -> WireDirectory {
    let row = |id: i64, venue: &str, label: Option<&str>, tier: &str| WireDirectoryAccount {
        id,
        venue: venue.to_string(),
        label: label.map(str::to_string),
        tier: tier.to_string(),
        venue_account_id: None,
    };
    WireDirectory {
        venues: Vec::new(),
        accounts: vec![row(1, "binance", None, "demo"), row(2, "bybit", Some("DEMO"), "demo")],
    }
}

/// Every read answered: the capabilities advertise `recon-verbs`, the directory agrees.
fn reads(snap: WireSnapshot) -> Reads {
    Reads {
        snap: Ok(snap),
        features: Some(Ok(vec![FEATURE_RECON_VERBS.to_string()])),
        directory: Some(Ok(directory())),
    }
}

fn judged(snap: WireSnapshot) -> Vec<Check> {
    judge(NODE, &reads(snap))
}

fn rows<'a>(checks: &'a [Check], name: &str) -> Vec<&'a Check> {
    checks.iter().filter(|c| c.name == name).collect()
}

/// The ONE row of `name`.
fn only<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    let found = rows(checks, name);
    assert_eq!(found.len(), 1, "exactly one `{name}` row expected in {checks:#?}");
    found[0]
}

fn words(checks: &[Check]) -> Vec<&'static str> {
    checks.iter().map(|c| c.status.word()).collect()
}

// ---- the grammar ----

#[test]
fn both_flag_forms_parse_and_json_defaults_off() {
    assert_eq!(parsed(&["--node", NODE]).unwrap(), Args { node: NODE.to_string(), json: false });
    assert_eq!(
        parsed(&["--json", "--node=<host>:7879"]).unwrap(),
        Args { node: NODE.to_string(), json: true }
    );
}

#[test]
fn a_missing_node_errors_naming_the_flag() {
    let err = parsed(&["--json"]).unwrap_err();
    assert!(err.contains("--node"), "{err}");
}

#[test]
fn help_short_circuits_even_without_a_node_and_a_bare_word_is_refused() {
    for spelling in ["-h", "--help"] {
        assert!(parsed(&[spelling]).unwrap_err().is_help(), "{spelling}");
    }
    let err = parsed(&["--node", NODE, "halted"]).unwrap_err();
    assert!(!err.is_help(), "{err}");
}

// ---- OK ----

#[test]
fn a_healthy_node_is_ok_and_names_what_the_wire_cannot_say() {
    let checks = judged(healthy());
    assert_eq!(verdict(&checks), Verdict::Ok, "{checks:#?}");
    assert_eq!(summary_line(&checks), "DOCTOR: OK");
    assert_eq!(exit_number(verdict(&checks)), 0);
    assert_eq!(only(&checks, "door").status, Status::Pass);
    assert!(only(&checks, "build").detail.contains("vike-tradehub 0.1.72"));
    assert!(only(&checks, "build").detail.contains("LIVE"));
    // One engine row per account, with the tier it runs at and the row's agreement.
    let engines = rows(&checks, "engine");
    assert_eq!(engines.len(), 2);
    assert!(engines.iter().all(|c| c.status == Status::Pass && c.detail.contains("demo")));
    assert!(only(&checks, "trading").detail.contains("2 engine(s) Active: binance, bybit#DEMO"));
    // What the current frame carries is judged: a day of uptime and no sentinel file are PASS rows.
    assert_eq!(only(&checks, "uptime").status, Status::Pass);
    assert_eq!(only(&checks, "uptime").detail, "up 1d 0h");
    assert_eq!(only(&checks, "halt-sentinel").status, Status::Pass);
    // The facts only the box holds are rows of their own, each with its command, none a verdict.
    for name in ["journal", "price-feeds"] {
        let c = only(&checks, name);
        assert_eq!(c.status, Status::Unknown, "{name}");
        assert!(c.detail.contains("unknown (not on the wire)"), "{name}: {}", c.detail);
        assert!(c.detail.contains("Ask: "), "{name}: {}", c.detail);
    }
    // No Hyperliquid account, so no Hyperliquid-only rows.
    assert!(rows(&checks, "leverage").is_empty() && rows(&checks, "hl-approval").is_empty());
}

#[test]
fn the_table_has_one_word_per_row_and_ends_with_the_verdict() {
    let checks = judged(healthy());
    let text = human(NODE, &checks);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "vike-cli trade doctor: <host>:7879");
    assert!(lines[1].starts_with("STATUS"), "{text}");
    for line in &lines[2..lines.len() - 2] {
        let first = line.split_whitespace().next().unwrap_or("");
        assert!(["PASS", "WARN", "FAIL", "UNKNOWN"].contains(&first), "{line:?} in\n{text}");
    }
    assert!(lines[lines.len() - 2].contains("fact(s) are not on the wire"), "{text}");
    assert_eq!(lines[lines.len() - 1], "DOCTOR: OK");
}

// ---- a halted or reducing venue ----

#[test]
fn a_halted_venue_is_a_fail_naming_the_account_and_the_way_out() {
    let mut snap = healthy();
    snap.venues[1].trading_state = WireTradingState::Halted;
    let checks = judged(snap);
    let halted: Vec<_> =
        rows(&checks, "trading").into_iter().filter(|c| c.status == Status::Fail).collect();
    assert_eq!(halted.len(), 1);
    assert_eq!(halted[0].scope, "bybit#DEMO");
    assert!(halted[0].detail.starts_with("HALTED"), "{}", halted[0].detail);
    assert!(halted[0].items.iter().any(|i| i.contains("trade resume")), "{:?}", halted[0].items);
    // The Active engine is still reported Active, beside it.
    assert!(only_pass(&checks, "trading").detail.contains("1 engine(s) Active: binance"));
    assert_eq!(verdict(&checks), Verdict::Fail);
    assert_eq!(summary_line(&checks), "DOCTOR: FAIL 1");
    assert_eq!(exit_number(verdict(&checks)), 2);
    assert!(human(NODE, &checks).contains("HALTED"));
}

fn only_pass<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.name == name && c.status == Status::Pass)
        .unwrap_or_else(|| panic!("no PASS `{name}` row in {checks:#?}"))
}

#[test]
fn a_reducing_venue_is_a_warn() {
    let mut snap = healthy();
    snap.venues[0].trading_state = WireTradingState::Reducing;
    let checks = judged(snap);
    assert_eq!(verdict(&checks), Verdict::Warn);
    assert_eq!(summary_line(&checks), "DOCTOR: WARN 1");
    assert_eq!(exit_number(verdict(&checks)), 1);
    let reducing =
        rows(&checks, "trading").into_iter().find(|c| c.status == Status::Warn).expect("a WARN");
    assert_eq!(reducing.scope, "binance");
}

/// A stopped engine's last block may still say Halted; the account is NOT RUNNING and nothing
/// halts it, so it must not be counted as a halted venue.
#[test]
fn a_stopped_engines_retained_state_is_not_a_halted_venue() {
    let mut snap = healthy();
    snap.venues[1].trading_state = WireTradingState::Halted;
    snap.not_running = vec![not_running("bybit", Some("DEMO"), Some("demo"), "account inactive")];
    let checks = judged(snap);
    assert!(rows(&checks, "trading").iter().all(|c| c.status == Status::Pass), "{checks:#?}");
}

// ---- a held reconcile row ----

#[test]
fn a_held_row_is_a_warn_with_its_kind_its_effect_and_the_confirm_hint() {
    let mut snap = healthy();
    snap.recon = vec![recon_row(12, "OrphanLocalPosition"), recon_row(15, "BalanceDrift")];
    let checks = judged(snap);
    let recon = only(&checks, "recon");
    assert_eq!(recon.status, Status::Warn);
    assert!(recon.detail.contains("2 held for an operator"), "{}", recon.detail);
    assert!(recon.detail.contains("(BalanceDrift x1, OrphanLocalPosition x1)"), "{}", recon.detail);
    assert!(recon.detail.contains("vike-cli trade recon ls --node <host>:7879"));
    assert!(recon.detail.contains("trade recon confirm <id> --node <control addr>"));
    assert!(
        recon
            .items
            .iter()
            .any(|i| i.starts_with("#12 bybit#DEMO OrphanLocalPosition: detail of 12 second line"))
    );
    assert!(recon.items.iter().any(|i| i.contains("drop the LOCAL position")), "{:?}", recon.items);
    assert_eq!(verdict(&checks), Verdict::Warn);
}

/// An empty list means "nothing held" only on a node that advertises `recon-verbs`.
#[test]
fn an_empty_recon_list_is_told_from_an_older_node() {
    let mut r = reads(healthy());
    assert_eq!(only(&judge(NODE, &r), "recon").status, Status::Pass);
    r.features = Some(Ok(vec!["strategy-verbs".to_string()]));
    let older = judge(NODE, &r);
    assert_eq!(only(&older, "recon").status, Status::Unknown);
    assert!(only(&older, "recon").detail.contains("proves nothing"));
    r.features = None;
    assert_eq!(only(&judge(NODE, &r), "recon").status, Status::Unknown);
}

// ---- a NOT RUNNING account ----

#[test]
fn a_not_running_account_is_a_warn_with_the_nodes_reason() {
    let mut snap = healthy();
    snap.venues.truncate(1);
    snap.not_running = vec![not_running("bybit", Some("DEMO"), Some("demo"), "adapter failed")];
    let checks = judged(snap);
    let down = engine_row(&checks, "bybit#DEMO");
    assert_eq!(down.status, Status::Warn);
    assert_eq!(down.detail, "NOT RUNNING (demo): adapter failed");
    assert_eq!(engine_row(&checks, "binance").status, Status::Pass);
    assert_eq!(verdict(&checks), Verdict::Warn);
}

fn engine_row<'a>(checks: &'a [Check], key: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.name == "engine" && c.scope == key)
        .unwrap_or_else(|| panic!("no engine row for {key} in {checks:#?}"))
}

#[test]
fn an_active_row_with_no_engine_and_no_reason_is_a_fail() {
    let mut r = reads(healthy());
    if let Some(Ok(d)) = &mut r.directory {
        d.accounts.push(WireDirectoryAccount {
            id: 3,
            venue: "oanda".to_string(),
            label: None,
            tier: "demo".to_string(),
            venue_account_id: None,
        });
    }
    let checks = judge(NODE, &r);
    let missing = engine_row(&checks, "oanda");
    assert_eq!(missing.status, Status::Fail);
    assert!(missing.detail.contains("NO engine and NO NOT RUNNING reason"), "{}", missing.detail);
    assert_eq!(verdict(&checks), Verdict::Fail);
}

#[test]
fn a_tier_the_row_does_not_state_is_a_warn() {
    let mut snap = healthy();
    snap.venues[0].tier = Some("live".to_string());
    snap.venues[1].tier = None;
    let checks = judged(snap);
    let wrong = engine_row(&checks, "binance");
    assert_eq!(wrong.status, Status::Warn);
    assert!(wrong.detail.contains("RUNNING at tier live, but its active row states demo"));
    let unpublished = engine_row(&checks, "bybit#DEMO");
    assert_eq!(unpublished.status, Status::Warn);
    assert!(unpublished.detail.contains("tier not published"), "{}", unpublished.detail);
}

#[test]
fn a_node_that_knows_no_account_is_a_fail() {
    let mut snap = healthy();
    snap.venues.clear();
    let r = Reads { snap: Ok(snap), features: None, directory: None };
    let checks = judge(NODE, &r);
    assert_eq!(only(&checks, "engine").status, Status::Fail);
}

// ---- a stale duplicate block ----

#[test]
fn one_route_key_published_twice_is_a_fail() {
    let mut snap = healthy();
    snap.venues.insert(1, block("bybit", Some("DEMO"), "demo", WireTradingState::Active));
    let checks = judged(snap);
    let dup = only(&checks, "duplicates");
    assert_eq!(dup.status, Status::Fail);
    assert_eq!(dup.scope, "bybit#DEMO");
    assert!(dup.detail.starts_with("2 blocks share this route key"), "{}", dup.detail);
    assert_eq!(verdict(&checks), Verdict::Fail);
}

#[test]
fn a_stopped_account_that_keeps_its_block_is_a_warn_and_its_position_is_unmanaged() {
    let mut snap = healthy();
    snap.venues[1].positions = vec![position("ETHUSDT", 8.34)];
    snap.not_running = vec![not_running(
        "bybit",
        Some("DEMO"),
        Some("demo"),
        "account inactive; position open at venue: ETHUSDT 8.34",
    )];
    let checks = judged(snap);
    let stale = only(&checks, "duplicates");
    assert_eq!((stale.status, stale.scope.as_str()), (Status::Warn, "bybit#DEMO"));
    assert!(stale.detail.contains("stopped engine's block is still published"));
    let positions = only(&checks, "positions");
    assert_eq!(positions.status, Status::Warn);
    assert!(
        positions.detail.contains("1 on an account that is NOT RUNNING"),
        "{}",
        positions.detail
    );
    assert!(positions.items[0].contains("nothing manages it"), "{:?}", positions.items);
    // The account is NOT RUNNING, not "RUNNING" because a block exists for it.
    assert!(engine_row(&checks, "bybit#DEMO").detail.starts_with("NOT RUNNING"));
}

#[test]
fn blocks_without_route_keys_leave_duplicates_unknown() {
    let mut snap = healthy();
    for b in &mut snap.venues {
        b.route_key.clear();
    }
    let r = Reads { snap: Ok(snap), features: None, directory: None };
    let checks = judge(NODE, &r);
    let dup = only(&checks, "duplicates");
    assert_eq!(dup.status, Status::Unknown);
    assert!(dup.detail.contains("2 block(s) carry no route key"), "{}", dup.detail);
}

// ---- the door ----

#[test]
fn the_starting_door_is_a_warn_carrying_the_progress() {
    let text = format!(
        "tradehub snapshot error: {}",
        starting_error("starting the accounts; accounts: binance up, dukascopy starting")
    );
    let r = Reads {
        snap: Err(io::Error::new(io::ErrorKind::InvalidData, text)),
        features: None,
        directory: None,
    };
    let checks = judge(NODE, &r);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Warn);
    assert!(checks[0].detail.starts_with("STARTING"), "{}", checks[0].detail);
    assert_eq!(
        checks[0].items,
        vec!["starting the accounts; accounts: binance up, dukascopy starting".to_string()]
    );
    assert_eq!(summary_line(&checks), "DOCTOR: WARN 1");
}

#[test]
fn an_unreachable_node_and_a_refused_key_are_fails_and_ask_for_nothing_more() {
    let down = Reads {
        snap: Err(io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused")),
        features: None,
        directory: None,
    };
    let checks = judge(NODE, &down);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Fail);
    assert!(checks[0].detail.contains("cannot read a snapshot from <host>:7879"));
    assert_eq!(summary_line(&checks), "DOCTOR: FAIL 1");

    let denied = Reads {
        snap: Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "tradehub snapshot denied: bad mac",
        )),
        features: None,
        directory: None,
    };
    let checks = judge(NODE, &denied);
    assert_eq!(checks[0].status, Status::Fail);
    assert!(checks[0].detail.contains("refused the observe handshake"));
    assert!(checks[0].items.iter().any(|i| i.contains(OBSERVE_KEY_ENV)), "{:?}", checks[0].items);
}

#[test]
fn a_pre_fold_frame_is_a_warn_and_nothing_else_is_judged() {
    let mut snap = healthy();
    snap.seq = 0;
    let r = Reads { snap: Ok(snap), features: None, directory: None };
    let checks = judge(NODE, &r);
    assert_eq!(only(&checks, "door").status, Status::Warn);
    assert!(rows(&checks, "engine").is_empty() && rows(&checks, "trading").is_empty());
    assert_eq!(verdict(&checks), Verdict::Warn);
}

#[test]
fn a_handler_fault_is_a_fail() {
    let mut snap = healthy();
    snap.fault = Some("handler panicked: index out of bounds".to_string());
    let checks = judged(snap);
    let core = only(&checks, "core");
    assert_eq!(core.status, Status::Fail);
    assert!(core.detail.contains("index out of bounds"));
}

// ---- orders, positions, prices, keys ----

#[test]
fn resting_orders_are_counted_the_terminal_ones_are_not_and_the_age_is_not_guessed() {
    let mut snap = healthy();
    snap.orders = vec![
        order("c-1", "FILLED"),
        order("c-2", "ACCEPTED"),
        order("c-3", "PartiallyFilled"),
        order("c-4", "CANCELED"),
        order("c-5", "Rejected"),
    ];
    let checks = judged(snap);
    let orders = only(&checks, "orders");
    assert_eq!(orders.status, Status::Pass);
    assert!(
        orders.detail.starts_with("2 resting (ACCEPTED x1, PARTIALLYFILLED x1)"),
        "{}",
        orders.detail
    );
    assert!(
        orders.detail.contains("first in registry order: c-2 bybit ETHUSDT"),
        "{}",
        orders.detail
    );
    // None of them carries an age (a node that predates the field): said as such, never guessed.
    let age = only(&checks, "order-age");
    assert_eq!(age.status, Status::Unknown);
    assert!(
        age.detail.contains("unknown (node too old)") && age.detail.contains("grep -F 'c-2'"),
        "{}",
        age.detail
    );
}

/// The oldest resting order is judged by the node's own count: under a day is a PASS naming it, over
/// a day is a WARN, a terminal order's age is ignored, and one order without an age does not hide
/// the ages of the others.
#[test]
fn the_oldest_resting_orders_age_is_judged_against_a_day() {
    let judged_with = |orders: Vec<WireOrderView>| {
        let mut snap = healthy();
        snap.orders = orders;
        let checks = judged(snap);
        only(&checks, "order-age").clone()
    };

    let young = judged_with(vec![
        aged("c-1", "ACCEPTED", 5 * 60 * 1000),
        aged("c-2", "ACCEPTED", 3 * HOUR_MS),
        order("c-3", "ACCEPTED"), // no age: ignored, not zero
    ]);
    assert_eq!(young.status, Status::Pass);
    assert_eq!(young.detail, "the oldest resting order is 3h 0m old: c-2 bybit ETHUSDT");

    // Exactly a day is not yet "older than a day"; one second past it is.
    assert_eq!(judged_with(vec![aged("c-1", "ACCEPTED", DAY_MS)]).status, Status::Pass);
    let stale =
        judged_with(vec![aged("c-1", "ACCEPTED", DAY_MS + 1000), aged("c-2", "NEW", 60_000)]);
    assert_eq!(stale.status, Status::Warn);
    assert!(stale.detail.contains("1d 0h old (over 24h): c-1 bybit ETHUSDT"), "{}", stale.detail);
    assert!(stale.detail.contains("a restart resets the clock"), "{}", stale.detail);

    // A FILLED order's age is not a resting order's: only the resting one is judged.
    let ended = judged_with(vec![aged("c-1", "FILLED", 3 * DAY_MS), aged("c-2", "ACCEPTED", 1000)]);
    assert_eq!(ended.status, Status::Pass);
    assert!(ended.detail.contains("1s old: c-2"), "{}", ended.detail);
}

/// A WARN anywhere moves the verdict; the stale order is one.
#[test]
fn a_stale_order_makes_the_verdict_a_warn() {
    let mut snap = healthy();
    snap.orders = vec![aged("c-1", "ACCEPTED", 2 * DAY_MS)];
    let checks = judged(snap);
    assert_eq!(verdict(&checks), Verdict::Warn);
    assert_eq!(summary_line(&checks), "DOCTOR: WARN 1");
}

// ---- uptime ----

#[test]
fn a_node_up_under_two_minutes_is_a_warn_and_one_up_longer_is_a_pass() {
    let up = |ms: Option<u64>| {
        let mut snap = healthy();
        snap.uptime_ms = ms;
        only(&judged(snap), "uptime").clone()
    };
    let fresh = up(Some(45_000));
    assert_eq!(fresh.status, Status::Warn);
    assert_eq!(fresh.detail, "up 45s: restarted recently, so the rest of this table is mid-start");
    assert_eq!(up(Some(RESTARTED_RECENTLY_MS - 1)).status, Status::Warn);
    assert_eq!(up(Some(RESTARTED_RECENTLY_MS)).status, Status::Pass);
    assert_eq!(up(Some(3 * DAY_MS + 5 * HOUR_MS)).detail, "up 3d 5h");

    // A node that predates the field says nothing: not zero seconds, not a restart.
    let silent = up(None);
    assert_eq!(silent.status, Status::Unknown);
    assert!(silent.detail.starts_with("unknown (node too old)"), "{}", silent.detail);
    assert!(silent.detail.contains("systemctl show vike-tradehub"), "{}", silent.detail);
}

#[test]
fn a_span_names_its_two_coarsest_units() {
    assert_eq!(span(0), "0s");
    assert_eq!(span(59_999), "59s");
    assert_eq!(span(61_000), "1m 1s");
    assert_eq!(span(HOUR_MS + 60_000), "1h 1m");
    assert_eq!(span(DAY_MS + 3 * HOUR_MS + 59_000), "1d 3h");
}

/// Uptime and the sentinel are the publisher's own readings, true before the core has folded.
#[test]
fn a_pre_fold_frame_still_reports_the_uptime_and_the_sentinel() {
    let mut snap = healthy();
    snap.seq = 0;
    snap.uptime_ms = Some(5_000);
    snap.halt_sentinel = Some(true);
    let r = Reads { snap: Ok(snap), features: None, directory: None };
    let checks = judge(NODE, &r);
    assert_eq!(only(&checks, "uptime").status, Status::Warn);
    assert_eq!(only(&checks, "halt-sentinel").status, Status::Fail);
    assert!(rows(&checks, "engine").is_empty(), "nothing else is judged");
    assert_eq!(verdict(&checks), Verdict::Fail);
}

// ---- the HALT sentinel ----

#[test]
fn an_engaged_sentinel_is_a_fail_and_a_clear_one_a_pass() {
    let sentinel = |flag: Option<bool>| {
        let mut snap = healthy();
        snap.halt_sentinel = flag;
        let checks = judged(snap);
        (only(&checks, "halt-sentinel").clone(), verdict(&checks))
    };

    let (engaged, verdict_engaged) = sentinel(Some(true));
    assert_eq!(engaged.status, Status::Fail);
    assert!(engaged.detail.contains("HALT sentinel file EXISTS"), "{}", engaged.detail);
    assert!(engaged.items.iter().any(|i| i.contains("remove the file")), "{:?}", engaged.items);
    assert_eq!(verdict_engaged, Verdict::Fail);

    let (clear, verdict_clear) = sentinel(Some(false));
    assert_eq!((clear.status, clear.detail.as_str()), (Status::Pass, "no HALT sentinel file"));
    assert_eq!(verdict_clear, Verdict::Ok);

    // A node that predates the field is NOT "no sentinel": it is unknown, and it names the way to ask.
    let (silent, verdict_silent) = sentinel(None);
    assert_eq!(silent.status, Status::Unknown);
    assert!(silent.detail.starts_with("unknown (node too old)"), "{}", silent.detail);
    assert!(silent.detail.contains("HALT sentinel path resolved"), "{}", silent.detail);
    assert_eq!(verdict_silent, Verdict::Ok, "an unknown row never moves the verdict");
}

// ---- a node that predates all of it ----

/// The same node without the three newer fields: every row they feed is `unknown (node too old)`,
/// none is a guess, and the verdict is what it was.
#[test]
fn an_older_node_reads_unknown_node_too_old_on_each_row_it_cannot_feed() {
    let mut snap = older();
    snap.orders = vec![order("c-1", "ACCEPTED")];
    let checks = judged(snap);
    for name in ["uptime", "halt-sentinel", "order-age"] {
        let c = only(&checks, name);
        assert_eq!(c.status, Status::Unknown, "{name}");
        assert!(c.detail.starts_with("unknown (node too old)"), "{name}: {}", c.detail);
        assert!(c.detail.contains("Ask: "), "{name}: {}", c.detail);
    }
    // The rows no node can feed keep their own wording.
    for name in ["journal", "price-feeds"] {
        assert!(only(&checks, name).detail.starts_with("unknown (not on the wire)"), "{name}");
    }
    assert_eq!(verdict(&checks), Verdict::Ok);
    let table = human(NODE, &checks);
    assert!(table.contains("unknown (node too old)"), "{table}");
    assert!(table.contains("or the node is too old to publish them"), "{table}");
}

#[test]
fn no_resting_order_is_a_pass_with_no_age_row() {
    let mut snap = healthy();
    snap.orders = vec![order("c-1", "FILLED")];
    let checks = judged(snap);
    assert_eq!(only(&checks, "orders").detail, "no order is resting");
    assert!(rows(&checks, "order-age").is_empty());
}

#[test]
fn non_flat_positions_are_listed_and_flat_ones_are_not() {
    let mut snap = healthy();
    snap.venues[1].positions = vec![position("ETHUSDT", 8.34), position("BTCUSDT", 0.0)];
    let checks = judged(snap);
    let p = only(&checks, "positions");
    assert_eq!(p.status, Status::Pass);
    assert_eq!(p.detail, "1 non-flat");
    assert_eq!(p.items.len(), 1);
    assert!(p.items[0].starts_with("bybit#DEMO ETHUSDT BOTH 8.34 @ 2414"), "{:?}", p.items);
    assert!(p.items[0].contains("leverage 2.0x"), "{:?}", p.items);
    let flat = judged(healthy());
    assert_eq!(only(&flat, "positions").detail, "flat: no non-flat position");
}

#[test]
fn a_position_with_no_price_source_is_a_fail() {
    let mut snap = healthy();
    snap.venues[0].missing_prices = 1;
    let checks = judged(snap);
    let fail =
        rows(&checks, "prices").into_iter().find(|c| c.status == Status::Fail).expect("a FAIL");
    assert_eq!(fail.scope, "binance");
    assert!(fail.detail.contains("no priceable source"));
    assert_eq!(rows(&checks, "price-feeds").len(), 1, "the feed question stays unknown");
}

#[test]
fn a_changed_key_is_a_warn_naming_the_account() {
    let mut snap = healthy();
    snap.key_changed = vec!["bybit#DEMO".to_string()];
    let checks = judged(snap);
    let k = only(&checks, "keys");
    assert_eq!((k.status, k.scope.as_str()), (Status::Warn, "bybit#DEMO"));
    assert_eq!(k.detail, KEY_CHANGED_WARNING);
}

#[test]
fn a_hyperliquid_account_adds_the_leverage_and_approval_rows() {
    let mut snap = healthy();
    snap.venues.push(block("hyperliquid", Some("LIVE"), "live", WireTradingState::Active));
    let checks = judged(snap);
    assert!(only(&checks, "leverage").detail.contains("vike-cli secrets accounts"));
    assert!(only(&checks, "hl-approval").detail.contains("TRADE-AGENT:"));
    assert!(
        [only(&checks, "leverage"), only(&checks, "hl-approval")]
            .iter()
            .all(|c| c.status == Status::Unknown)
    );
}

// ---- the output ----

#[test]
fn a_directory_that_failed_is_said_and_the_rest_is_still_judged() {
    let mut r = reads(healthy());
    r.directory =
        Some(Err(io::Error::new(io::ErrorKind::Unsupported, "no \"directory\" capability")));
    let checks = judge(NODE, &r);
    assert_eq!(only(&checks, "rows").status, Status::Unknown);
    assert!(only(&checks, "rows").detail.contains("vike-cli secrets accounts"));
    // Without rows there is nothing to disagree with: the engines are judged on their own.
    assert!(rows(&checks, "engine").iter().all(|c| c.status == Status::Pass), "{checks:#?}");
}

#[test]
fn the_exit_numbers_are_the_verdict_and_are_pinned() {
    assert_eq!(exit_number(Verdict::Ok), 0);
    assert_eq!(exit_number(Verdict::Warn), 1);
    assert_eq!(exit_number(Verdict::Fail), 2);
}

#[test]
fn unknown_rows_never_move_the_verdict() {
    let checks = vec![
        Check::new(Status::Pass, "door", "ready"),
        unknown("uptime", "how long", "systemctl show vike-tradehub"),
    ];
    assert_eq!(verdict(&checks), Verdict::Ok);
    assert_eq!(words(&checks), vec!["PASS", "UNKNOWN"]);
}

#[test]
fn json_carries_the_verdict_the_counts_and_every_row() {
    let mut snap = healthy();
    snap.venues[1].trading_state = WireTradingState::Halted;
    snap.recon = vec![recon_row(12, "OrphanLocalPosition")];
    let checks = judged(snap);
    let doc: serde_json::Value = serde_json::from_str(&json_body(NODE, &checks)).expect("JSON");
    assert_eq!(doc["node"], NODE);
    assert_eq!(doc["verdict"], "FAIL");
    assert_eq!(doc["fail"], 1);
    assert_eq!(doc["warn"], 1);
    assert_eq!(doc["unknown"], count(&checks, Status::Unknown));
    let rows = doc["checks"].as_array().expect("an array of rows");
    assert_eq!(rows.len(), checks.len());
    let halted = rows.iter().find(|r| r["status"] == "FAIL").expect("the FAIL row");
    assert_eq!(
        (halted["check"].as_str(), halted["account"].as_str()),
        (Some("trading"), Some("bybit#DEMO"))
    );
    let door = &rows[0];
    assert_eq!((door["check"].as_str(), door["account"].is_null()), (Some("door"), true));
}
