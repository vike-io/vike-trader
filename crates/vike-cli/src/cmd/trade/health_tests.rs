use std::io;

use super::*;
use vike_client::health::{
    DatahubAnswer, RESTARTED_RECENTLY_MS, Reads, route_key, running_tiers, span, unknown,
};
use vike_client::hubkeys::OBSERVE_KEY_ENV;
use vike_datahub_client::gateway_status::GatewayStatusReply;
use vike_model::TradingState;
use vike_tradehub_client::proto::{FEATURE_HUB_LINKS, FEATURE_RECONCILE_VERBS, starting_error};
use vike_tradehub_client::wire::{
    AccountDirectoryAccountWire, AccountDirectoryWire, ArmingNotRunningViewWire,
    BalanceWithheldWire, CoreSnapshotWire, HubIdentityWire, KEY_CHANGED_WARNING, MountViewWire,
    OrderEmulationStopViewWire, OrderEmulationViewWire, OrderViewWire, PositionViewWire,
    PriceLinkDownWire, ReconcileAlertViewWire, TradeLinkDownWire, VenueBlockWire,
    VenueLinkDownWire,
};

const NODE: &str = "<host>:7879";

fn parsed(args: &[&str]) -> Result<Args, args::Stop> {
    parse(args.iter().map(|s| s.to_string()))
}

// ---- synthetic nodes ----

/// An engine block: `venue` plus an optional label, at `tier`, in `state`.
fn block(venue: &str, label: Option<&str>, tier: &str, state: TradingState) -> VenueBlockWire {
    VenueBlockWire {
        account_code: None,
        order_client_id_shape: None,
        venue: venue.to_string(),
        balance: 1000.0,
        realized_pnl: 0.0,
        fees_paid: 0.0,
        funding_paid: 0.0,
        equity: 1000.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_buying_power: 1000.0,
        trading_state: state,
        positions: Vec::new(),
        account: label.map(str::to_string),
        route_key: route_key(venue, label),
        symbols: vec!["BTCUSDT".to_string()],
        tier: Some(tier.to_string()),
        contract_values: Vec::new(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: String::new(),
        ledger_currency: String::new(),
        currency_assumed: false,
        balance_is_own_book: false,
    }
}

fn position(symbol: &str, size: f64) -> PositionViewWire {
    PositionViewWire {
        venue: "bybit".to_string(),
        symbol: symbol.to_string(),
        position_side: "BOTH".to_string(),
        size,
        average_price: 2414.0,
        unrealized: 12.5,
        leverage: 2.0,
        liquidation_price: 0.0,
    }
}

fn order(order_client_id: &str, status: &str) -> OrderViewWire {
    OrderViewWire {
        order_client_id: order_client_id.to_string(),
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
        average_fill_price: 0.0,
        data_delay: None,
        reason: None,
        age_ms: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

/// A resting order the node has known for `age_ms`.
fn aged(order_client_id: &str, status: &str, age_ms: u64) -> OrderViewWire {
    OrderViewWire { age_ms: Some(age_ms), ..order(order_client_id, status) }
}

fn reconcile_row(id: u64, kind: &str) -> ReconcileAlertViewWire {
    ReconcileAlertViewWire {
        id,
        account: "bybit#DEMO".to_string(),
        kind: kind.to_string(),
        detail: format!("detail of {id}\nsecond line"),
        proposed_event_count: 1,
    }
}

fn not_running(
    venue: &str,
    label: Option<&str>,
    tier: Option<&str>,
    why: &str,
) -> ArmingNotRunningViewWire {
    ArmingNotRunningViewWire {
        venue: venue.to_string(),
        account: label.map(str::to_string),
        route_key: route_key(venue, label),
        tier: tier.map(str::to_string),
        reason: why.to_string(),
        detail: None,
        block_word: None,
    }
}

/// A current node that folded: up a day, no HALT sentinel, two accounts, both Active, and nothing
/// wrong. [`older`] is the same node as a build that predates the frame's newer fields.
fn healthy() -> CoreSnapshotWire {
    let mut s = CoreSnapshotWire::empty();
    s.sequence = 41;
    s.uptime_ms = Some(DAY_MS);
    s.halt_sentinel = Some(false);
    s.identity = Some(HubIdentityWire {
        name: "node-a".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: true,
        build: "vike-tradehub 0.1.72 (abc1234, clean)".to_string(),
        advertise_addr: String::new(),
    });
    s.venues = vec![
        block("binance", None, "demo", TradingState::Active),
        block("bybit", Some("DEMO"), "demo", TradingState::Active),
    ];
    s
}

const HOUR_MS: u64 = 60 * 60 * 1000;
const DAY_MS: u64 = 24 * HOUR_MS;

/// [`healthy`] as a node that predates `uptime_ms`, `halt_sentinel` and `age_ms` sent it.
fn older() -> CoreSnapshotWire {
    CoreSnapshotWire { uptime_ms: None, halt_sentinel: None, ..healthy() }
}

/// The directory that agrees with [`healthy`].
fn directory() -> AccountDirectoryWire {
    let row = |id: i64, venue: &str, label: Option<&str>, tier: &str| AccountDirectoryAccountWire {
        id,
        venue: venue.to_string(),
        label: label.map(str::to_string),
        tier: tier.to_string(),
        venue_account_id: None,
    };
    AccountDirectoryWire {
        venues: Vec::new(),
        accounts: vec![row(1, "binance", None, "demo"), row(2, "bybit", Some("DEMO"), "demo")],
    }
}

/// Every read answered: the capabilities advertise `recon-verbs`, the directory agrees.
fn reads(snap: CoreSnapshotWire) -> Reads {
    Reads {
        snap: Ok(snap),
        features: Some(Ok(vec![FEATURE_RECONCILE_VERBS.to_string()])),
        directory: Some(Ok(directory())),
        mounts: Some(Vec::new()),
        hubs: None,
    }
}

fn judged(snap: CoreSnapshotWire) -> Vec<Check> {
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
    assert_eq!(summary_line(&checks), "HEALTH: OK");
    assert_eq!(exit_number(verdict(&checks)), 0);
    assert_eq!(only(&checks, "door").status, Status::Pass);
    assert!(only(&checks, "build").detail.contains("vike-tradehub 0.1.72"));
    assert!(only(&checks, "build").detail.ends_with("(demo)"));
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

/// The `build` row says what runs, whatever `flags.tradehub_live` says: a node of demo engines is `demo`.
#[test]
fn the_build_row_says_what_runs_whatever_the_switch_says() {
    for switch in [false, true] {
        let mut snap = healthy();
        snap.identity.as_mut().expect("an identity").live = switch;
        let checks = judged(snap);
        let build = only(&checks, "build");
        assert_eq!(build.status, Status::Pass);
        assert!(build.detail.ends_with("daemon \"node-a\" (demo)"), "{switch}: {}", build.detail);
    }
}

/// **The one word, over everything a frame can say.** A stopped account's retained block is not running, a later block
/// of one key is the newer engine, a word no tier spells is not guessed at, and a frame that is no reading (the pre-fold
/// placeholder, no frame at all) leaves the mount rows to answer: never `no account running`, which only a read can say.
#[test]
fn running_tiers_says_what_the_engines_run_and_never_guesses() {
    let stated = |tier: Option<&str>| MountViewWire {
        order_client_id_code: None,
        strategy: "s".to_string(),
        params: String::new(),
        live: true,
        venue: String::new(),
        symbol: String::new(),
        interval: String::new(),
        mount_id: String::new(),
        typed_params: None,
        asset_class: None,
        latched: false,
        route_key: String::new(),
        tier: tier.map(str::to_string),
        not_running: false,
    };
    let at = |tier: &str| block("bybit", Some("A"), tier, TradingState::Active);

    let mut frame = healthy();
    assert_eq!(running_tiers(Some(&frame), &[stated(Some("live"))]), "demo", "the frame outranks");
    frame.venues = vec![at("live"), at("demo")];
    assert_eq!(running_tiers(Some(&frame), &[]), "demo", "the newer block of one key");
    frame.venues = vec![at("demo"), block("okx", None, "paper", TradingState::Active)];
    assert_eq!(running_tiers(Some(&frame), &[]), "paper, demo");
    frame.venues.push(block("binance", None, "live", TradingState::Active));
    assert_eq!(running_tiers(Some(&frame), &[]), "paper, demo, LIVE");
    frame.venues.push(block("ig", None, "sim", TradingState::Active));
    assert_eq!(running_tiers(Some(&frame), &[]), "paper, demo, LIVE, tier not stated");

    frame.venues = vec![at("demo")];
    frame.not_running = vec![not_running("bybit", Some("A"), Some("demo"), "account inactive")];
    assert_eq!(running_tiers(Some(&frame), &[]), "no account running", "a retained block");

    let mut placeholder = healthy();
    placeholder.sequence = 0;
    assert_eq!(running_tiers(Some(&placeholder), &[]), "tier not stated");
    assert_eq!(running_tiers(Some(&placeholder), &[stated(Some("demo"))]), "demo");
    assert_eq!(running_tiers(None, &[stated(Some("demo")), stated(None)]), "demo, tier not stated");
    let stopped = MountViewWire { not_running: true, ..stated(Some("live")) };
    assert_eq!(running_tiers(None, &[stopped]), "tier not stated");
    assert_eq!(running_tiers(None, &[]), "tier not stated");
}

#[test]
fn the_table_has_one_word_per_row_and_ends_with_the_verdict() {
    let checks = judged(healthy());
    let text = human(NODE, &checks);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "vike-cli trade health: <host>:7879");
    assert!(lines[1].starts_with("STATUS"), "{text}");
    for line in &lines[2..lines.len() - 2] {
        let first = line.split_whitespace().next().unwrap_or("");
        assert!(["PASS", "WARN", "FAIL", "UNKNOWN"].contains(&first), "{line:?} in\n{text}");
    }
    assert!(lines[lines.len() - 2].contains("fact(s) are not on the wire"), "{text}");
    assert_eq!(lines[lines.len() - 1], "HEALTH: OK");
}

/// **Pinned on the code from before the link rows: every line a healthy node's table printed stays, in its order.**
/// The hub-link rows and the tally of unknown rows, which counts them, are the only lines allowed to differ.
#[test]
fn pinned_the_healthy_table_keeps_every_line_it_had() {
    let text = human(NODE, &judged(healthy()));
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| !l.contains("gateway-link") && !l.contains("price-link"))
        .filter(|l| !l.contains("fact(s) are not on the wire"))
        .collect();
    assert_eq!(
        kept,
        [
            "vike-cli trade health: <host>:7879",
            "STATUS   CHECK          ACCOUNT     DETAIL",
            "PASS     door                       ready: serving snapshots (seq 41)",
            "PASS     build                      vike-tradehub 0.1.72 (abc1234, clean) — daemon \"node-a\" (demo)",
            "PASS     uptime                     up 1d 0h",
            "PASS     core                       no handler fault",
            "PASS     engine         binance     RUNNING at tier demo (the active row agrees)",
            "PASS     engine         bybit#DEMO  RUNNING at tier demo (the active row agrees)",
            "PASS     trading                    2 engine(s) Active: binance, bybit#DEMO",
            "PASS     duplicates                 no route key is published twice and no stopped engine keeps a block",
            "PASS     keys                       no running account reports a changed key",
            "PASS     recon                      nothing is held for an operator",
            "PASS     orders                     no order is resting",
            "PASS     positions                  flat: no non-flat position",
            "PASS     prices                     every open position has a price source",
            "UNKNOWN  price-feeds                unknown (not on the wire): whether each price-only feed is live now (a priced position can carry a stale quote, and a venue with nothing open proves nothing). Ask: bin/vikectl logs | grep -i -E 'strat|runs no price feed' (system units: journalctl -u vike-datahub -u vike-tradehub)",
            "PASS     halt-sentinel              no HALT sentinel file",
            "UNKNOWN  journal                    unknown (not on the wire): ERROR lines since the node started. Ask: bin/vikectl logs --since <boot> | grep ' ERROR ' (system units: journalctl -u vike-tradehub -u vike-gatewayhub -u vike-datahub)",
            "HEALTH: OK",
        ]
    );
}

// ---- a halted or reducing venue ----

#[test]
fn a_halted_venue_is_a_fail_naming_the_account_and_the_way_out() {
    let mut snap = healthy();
    snap.venues[1].trading_state = TradingState::Halted;
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
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 1");
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
    snap.venues[0].trading_state = TradingState::Reducing;
    let checks = judged(snap);
    assert_eq!(verdict(&checks), Verdict::Warn);
    assert_eq!(summary_line(&checks), "HEALTH: WARN 1");
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
    snap.venues[1].trading_state = TradingState::Halted;
    snap.not_running = vec![not_running("bybit", Some("DEMO"), Some("demo"), "account inactive")];
    let checks = judged(snap);
    assert!(rows(&checks, "trading").iter().all(|c| c.status == Status::Pass), "{checks:#?}");
}

// ---- a held reconcile row ----

#[test]
fn a_held_row_is_a_warn_with_its_kind_its_effect_and_the_confirm_hint() {
    let mut snap = healthy();
    snap.reconcile =
        vec![reconcile_row(12, "OrphanLocalPosition"), reconcile_row(15, "BalanceDrift")];
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

// ---- the operator's armed core-held stops ----

/// A core-held stop as the node lists it: a reduce-only trigger row of the one emulated list, `waiting` while HELD.
fn core_held_stop(arm: &str, held: bool, rearms: u8) -> OrderEmulationViewWire {
    OrderEmulationViewWire {
        id: arm.to_string(),
        venue: "polymarket".to_string(),
        account: None,
        symbol: "TOKEN".to_string(),
        side: -1,
        qty: 5.0,
        kind: "stop".to_string(),
        reduce_only: true,
        trigger: Some(0.4),
        price: None,
        trail: None,
        peg: None,
        display_quantity: None,
        filled: 0.0,
        average_price: 0.0,
        child: None,
        waiting: held
            .then(|| "restored: held until its account's first reconcile pass".to_string()),
        rearms,
        held,
    }
}

/// No stop listed, no row: a node that predates the field also lists none, and a PASS would claim what it cannot
/// know. The older `core_held_stops` field alone adds none either: the row reads the ONE emulated list, so a stop is
/// never counted from two lists; and an emulated row that is no core-held stop (a chase, a stop that may open) adds
/// none.
#[test]
fn no_core_held_stop_adds_no_row() {
    let checks = judged(healthy());
    assert!(rows(&checks, "stops").is_empty(), "{checks:#?}");
    let mut snap = healthy();
    snap.core_held_stops = vec![OrderEmulationStopViewWire {
        arm_id: "s0a1".into(),
        venue: "polymarket".into(),
        symbol: "TOKEN".into(),
        account: "polymarket".into(),
        side: -1,
        qty: 5.0,
        trigger_price: 0.4,
        held: false,
        rearms: 0,
    }];
    snap.emulated_orders =
        vec![OrderEmulationViewWire { reduce_only: false, ..core_held_stop("s0a2", false, 0) }];
    let checks = judged(snap);
    assert!(rows(&checks, "stops").is_empty(), "{checks:#?}");
}

/// A latched mount is a WARN row that names it and the verb that clears it (decision 0239); a mount that is not
/// latched, and a node that listed no mount, add no row (a node from before the flag writes none, so a PASS row
/// would claim what it cannot know).
#[test]
fn a_latched_mount_is_a_warn_row_and_no_latched_mount_adds_none() {
    let mount = |id: &str, latched: bool| vike_tradehub_client::wire::MountViewWire {
        order_client_id_code: None,
        strategy: "grid".into(),
        params: String::new(),
        live: true,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: id.into(),
        typed_params: None,
        asset_class: None,
        latched,
        route_key: String::new(),
        tier: None,
        not_running: false,
    };
    let snap = healthy();
    let mut r = reads(snap.clone());
    r.mounts = Some(vec![mount("free", false)]);
    assert!(rows(&judge(NODE, &r), "mounts").is_empty(), "a free mount adds no row");
    r.mounts = None;
    assert!(rows(&judge(NODE, &r), "mounts").is_empty(), "no mount read adds no row");

    r.mounts = Some(vec![mount("free", false), mount("binance__BTCUSDT__1m", true)]);
    let checks = judge(NODE, &r);
    let row = only(&checks, "mounts");
    assert_eq!(row.status, Status::Warn);
    assert!(row.detail.contains("1 mount(s) latched"), "{}", row.detail);
    assert!(row.detail.contains("trade strategy reset-budget"), "{}", row.detail);
    assert_eq!(row.items, vec!["binance__BTCUSDT__1m binance/BTCUSDT 1m".to_string()]);
    assert_eq!(verdict(&checks), Verdict::Warn);
}

/// An armed stop is PASS and says what it is (held by the node, not the venue) and where to list it; a stop held for
/// the first pass, or re-armed after a failed close, is a WARN that counts it.
#[test]
fn armed_core_held_stops_are_a_pass_and_a_held_or_rearmed_one_is_a_warn() {
    let mut snap = healthy();
    snap.emulated_orders = vec![core_held_stop("s0a1", false, 0)];
    let checks = judged(snap.clone());
    let stops = only(&checks, "stops");
    assert_eq!(stops.status, Status::Pass, "{}", stops.detail);
    assert!(stops.detail.contains("1 core-held reduce-only stop(s) armed"), "{}", stops.detail);
    assert!(stops.detail.contains("NODE's memory"), "{}", stops.detail);
    assert!(stops.detail.contains("vike-cli trade order stops --node <host>:7879"));
    assert!(stops.items.iter().any(|i| i.starts_with("s0a1 SELL 5 TOKEN")), "{:?}", stops.items);

    snap.emulated_orders = vec![core_held_stop("s0a1", true, 0), core_held_stop("s0a2", false, 2)];
    let checks = judged(snap);
    let stops = only(&checks, "stops");
    assert_eq!(stops.status, Status::Warn);
    assert!(stops.detail.contains("2 core-held reduce-only stop(s) armed"), "{}", stops.detail);
    assert!(
        stops.detail.contains("1 HELD until its account's first reconcile pass"),
        "{}",
        stops.detail
    );
    assert!(stops.detail.contains("1 RE-ARMED"), "{}", stops.detail);
    assert_eq!(verdict(&checks), Verdict::Warn);
}

/// An empty list means "nothing held" only on a node that advertises `recon-verbs`.
#[test]
fn an_empty_reconcile_list_is_told_from_an_older_node() {
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
        d.accounts.push(AccountDirectoryAccountWire {
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

/// [`healthy`] with the engine `venue`/`label` at `engine`, and the directory's rows of `venue` replaced
/// by `rows`, in id order.
fn with_rows(
    venue: &str,
    label: Option<&str>,
    engine: &str,
    rows: &[(Option<&str>, &str)],
) -> Reads {
    let mut snap = healthy();
    snap.venues.retain(|b| b.venue != venue);
    snap.venues.push(block(venue, label, engine, TradingState::Active));
    let mut r = reads(snap);
    let Some(Ok(d)) = &mut r.directory else { panic!("the directory was read") };
    d.accounts.retain(|a| a.venue != venue);
    for (i, (label, tier)) in rows.iter().enumerate() {
        d.accounts.push(AccountDirectoryAccountWire {
            id: 10 + i as i64,
            venue: venue.to_string(),
            label: label.map(str::to_string),
            tier: (*tier).to_string(),
            venue_account_id: None,
        });
    }
    r
}

/// **An account's rows fold as the arming rule folds them, whatever order they come in.** Each row of a key
/// used to overwrite the last, so a `paper` row with a higher id read a false WARN beside a `live` engine,
/// and two non-paper tiers read PASS when the last row happened to match.
#[test]
fn an_accounts_rows_fold_by_the_arming_rule() {
    let row = |r: &Reads, key: &str| {
        let checks = judge(NODE, r);
        let found = engine_row(&checks, key);
        (found.status, found.detail.clone())
    };
    for rows in [[(None, "live"), (None, "paper")], [(None, "paper"), (None, "live")]] {
        let (status, detail) = row(&with_rows("binance", None, "live", &rows), "binance");
        assert_eq!(status, Status::Pass, "{rows:?}: {detail}");
        assert!(detail.contains("RUNNING at tier live (the active row agrees)"), "{detail}");
    }

    // Two rows of ONE tier are fine (dukascopy's two demo books).
    let two_demo = [(None, "demo"), (None, "demo")];
    let (status, detail) = row(&with_rows("dukascopy", None, "demo", &two_demo), "dukascopy");
    assert_eq!(status, Status::Pass, "{detail}");

    // Two non-paper tiers, whichever the engine runs at and whichever row comes last: no PASS, and
    // the words say what the node does about it.
    for (engine, rows) in [
        ("demo", [(None, "live"), (None, "demo")]),
        ("live", [(None, "live"), (None, "demo")]),
        ("demo", [(None, "demo"), (None, "live")]),
    ] {
        let (status, detail) = row(&with_rows("binance", None, engine, &rows), "binance");
        assert_eq!(status, Status::Warn, "{engine} {rows:?}: {detail}");
        assert!(detail.contains(&format!("RUNNING at tier {engine}")), "{detail}");
        assert!(detail.contains("two active tiers"), "{detail}");
        assert!(detail.contains("keeps running"), "{detail}");
        assert!(detail.contains("does NOT RUN"), "{detail}");
    }

    // IBKR's tier is the VENUE's (decision 0140): two labels at two tiers run neither.
    let by_label = [(None, "demo"), (Some("LIVE"), "live")];
    let (status, detail) = row(&with_rows("ibkr", None, "demo", &by_label), "ibkr");
    assert_eq!(status, Status::Warn, "{detail}");
    assert!(detail.contains("two active tiers"), "{detail}");
}

/// A stopped account whose rows conflict names that, not whichever row came last.
#[test]
fn a_stopped_account_with_two_tiers_says_so() {
    let mut snap = healthy();
    snap.venues.clear();
    snap.not_running = vec![not_running("binance", None, None, "two active tiers")];
    let mut r = reads(snap);
    let Some(Ok(d)) = &mut r.directory else { panic!("the directory was read") };
    d.accounts = [(1, "demo"), (2, "live")]
        .map(|(id, tier)| AccountDirectoryAccountWire {
            id,
            venue: "binance".to_string(),
            label: None,
            tier: tier.to_string(),
            venue_account_id: None,
        })
        .to_vec();
    let checks = judge(NODE, &r);
    let row = engine_row(&checks, "binance");
    assert_eq!(row.status, Status::Warn);
    assert!(row.detail.starts_with("NOT RUNNING (two active tiers)"), "{}", row.detail);
}

/// **F8 of the twin review: the derived perpetual lane is expected, not a permanent WARN.** The twin
/// `binance#PERP` runs on `BTCUSDT.P` over the default account's row and has no row of its own, so it used to
/// read "RUNNING, but no active row states it" on every health run. It is judged against the row it is
/// derived from: PASS at that row's tier, a WARN only when the tier differs. An operator's own account that
/// merely spells the label (it is mounted on another symbol) is still the old finding.
#[test]
fn a_derived_lane_twin_is_judged_against_the_row_it_is_derived_from() {
    let twin = |symbol: &str, tier: &str| VenueBlockWire {
        symbols: vec![symbol.to_string()],
        ..block("binance", Some("PERP"), tier, TradingState::Active)
    };
    let with = |block: VenueBlockWire| {
        let mut snap = healthy();
        snap.venues.push(block);
        judged(snap)
    };

    let checks = with(twin("BTCUSDT.P", "demo"));
    let lane = engine_row(&checks, "binance#PERP");
    assert_eq!(lane.status, Status::Pass, "{}", lane.detail);
    assert!(lane.detail.contains("RUNNING at tier demo"), "{}", lane.detail);
    assert_eq!(verdict(&checks), Verdict::Ok);

    let checks = with(twin("BTCUSDT.P", "live"));
    let lane = engine_row(&checks, "binance#PERP");
    assert_eq!(lane.status, Status::Warn);
    assert!(lane.detail.contains("RUNNING at tier live, but its active row states demo"));

    let checks = with(twin("ETHUSDT", "demo"));
    let own = engine_row(&checks, "binance#PERP");
    assert_eq!(own.status, Status::Warn, "not mounted on the lane's symbol: not the twin");
    assert!(own.detail.contains("no active row states it"), "{}", own.detail);
}

/// [`healthy`] plus a dukascopy engine (and its active row) whose venue paths `down` names.
fn with_dukascopy(down: Vec<VenueLinkDownWire>, stopped: bool) -> Vec<Check> {
    let mut snap = healthy();
    snap.venues.push(block("dukascopy", None, "demo", TradingState::Active));
    snap.venue_link_down = down;
    if stopped {
        snap.not_running = vec![not_running("dukascopy", None, Some("demo"), "adapter failed")];
    }
    let mut dir = directory();
    dir.accounts.push(AccountDirectoryAccountWire {
        id: 3,
        venue: "dukascopy".to_string(),
        label: None,
        tier: "demo".to_string(),
        venue_account_id: None,
    });
    judge(
        NODE,
        &Reads {
            snap: Ok(snap),
            features: Some(Ok(vec![FEATURE_RECONCILE_VERBS.to_string()])),
            directory: Some(Ok(dir)),
            mounts: None,
            hubs: None,
        },
    )
}

fn sidecar_down() -> VenueLinkDownWire {
    VenueLinkDownWire {
        route_key: "dukascopy".to_string(),
        why: "JForex session disconnected".to_string(),
        failed_restarts: 2,
        retry_in_ms: 20_000,
        effect: String::new(),
        no_adapter: false,
    }
}

/// **A dead venue path never reads as a healthy engine** (the owner's rule after 2026-10-26, when `PASS engine
/// dukascopy RUNNING at tier demo` stood for 2.5 hours over a dead JForex sidecar): the row is WARN, keeps what it
/// already said, and names the reason and what to expect. The other engines are untouched.
#[test]
fn a_running_engine_whose_venue_path_is_down_is_never_a_pass() {
    let checks = with_dukascopy(vec![sidecar_down()], false);
    let row = engine_row(&checks, "dukascopy");
    assert_eq!(row.status, Status::Warn, "{}", row.detail);
    for said in [
        "RUNNING at tier demo (the active row agrees)",
        "the venue path is DOWN: JForex session disconnected",
        "2 restart(s) failed so far",
        "next try in about 20 s",
        "orders are refused until its bridge has it back",
    ] {
        assert!(row.detail.contains(said), "{said:?} missing from {}", row.detail);
    }
    assert_eq!(verdict(&checks), Verdict::Warn, "the verdict is not OK while a path is down");
    assert_eq!(engine_row(&checks, "binance").status, Status::Pass);
    assert_eq!(engine_row(&checks, "bybit#DEMO").status, Status::Pass);
}

/// **The row promises no try nobody stated**: a bridge whose lane ended (OANDA's give-up: failures, no wait) and one that
/// is opening a session (cTrader's: neither) are still WARN, in their own words about the orders, with no "next try".
#[test]
fn a_down_path_with_no_stated_next_try_says_none() {
    let gave_up = VenueLinkDownWire {
        failed_restarts: 4,
        retry_in_ms: 0,
        effect: "orders still go out".to_string(),
        ..sidecar_down()
    };
    let opening = VenueLinkDownWire {
        failed_restarts: 0,
        retry_in_ms: 0,
        effect: "orders wait".to_string(),
        ..sidecar_down()
    };
    for (down, said) in [
        (gave_up, "(4 restart(s) failed so far); orders still go out"),
        (opening, "JForex session disconnected; orders wait"),
    ] {
        let checks = with_dukascopy(vec![down], false);
        let row = engine_row(&checks, "dukascopy");
        assert_eq!(row.status, Status::Warn, "{}", row.detail);
        assert!(row.detail.contains(said), "{said:?} missing from {}", row.detail);
        assert!(!row.detail.contains("next try"), "{}", row.detail);
    }
}

/// Up reads exactly as before the field existed (an empty list is also an older node's frame), and a row that was
/// already a WARN stays one with the outage added to it.
#[test]
fn an_engine_with_no_down_path_reads_as_it_always_did() {
    let checks = with_dukascopy(Vec::new(), false);
    let row = engine_row(&checks, "dukascopy");
    assert_eq!(
        (row.status, row.detail.as_str()),
        (Status::Pass, "RUNNING at tier demo (the active row agrees)")
    );
    assert_eq!(verdict(&checks), Verdict::Ok);

    let mut snap = healthy();
    snap.venues[0] = block("binance", None, "live", TradingState::Active);
    snap.venue_link_down =
        vec![VenueLinkDownWire { route_key: "binance".to_string(), ..sidecar_down() }];
    let checks = judged(snap);
    let row = engine_row(&checks, "binance");
    assert_eq!(row.status, Status::Warn);
    assert!(
        row.detail.contains("RUNNING at tier live, but its active row states demo"),
        "{}",
        row.detail
    );
    assert!(row.detail.contains("the venue path is DOWN"), "{}", row.detail);
}

/// The outage of an account the node lists NOT RUNNING is history: its row says why it does not run, not that its
/// path is down.
#[test]
fn a_down_path_of_an_account_that_does_not_run_is_not_said() {
    let checks = with_dukascopy(vec![sidecar_down()], true);
    let row = engine_row(&checks, "dukascopy");
    assert_eq!(row.detail, "NOT RUNNING (demo): adapter failed");
}

#[test]
fn a_node_that_knows_no_account_is_a_fail() {
    let mut snap = healthy();
    snap.venues.clear();
    let r = Reads { snap: Ok(snap), features: None, directory: None, mounts: None, hubs: None };
    let checks = judge(NODE, &r);
    assert_eq!(only(&checks, "engine").status, Status::Fail);
}

// ---- the links to the other two hubs ----

/// Every read answered, by a node that publishes its hub links.
fn current_reads(snap: CoreSnapshotWire) -> Reads {
    Reads {
        features: Some(Ok(vec![
            FEATURE_RECONCILE_VERBS.to_string(),
            FEATURE_HUB_LINKS.to_string(),
        ])),
        ..reads(snap)
    }
}

fn lost_link(route_key: &str, down_ms: u64) -> TradeLinkDownWire {
    TradeLinkDownWire {
        route_key: route_key.to_string(),
        why: "the gatewayhub is stopping".to_string(),
        down_ms,
        closed: false,
    }
}

fn closed_lease(route_key: &str) -> TradeLinkDownWire {
    TradeLinkDownWire {
        route_key: route_key.to_string(),
        why: "taken over by key-1".to_string(),
        down_ms: 1_000,
        closed: true,
    }
}

fn lost_feed(venue: &str, of: u32) -> PriceLinkDownWire {
    PriceLinkDownWire {
        venue: venue.to_string(),
        why: "connection refused".to_string(),
        down_ms: 65_000,
        of,
    }
}

fn hop_rows<'a>(checks: &'a [Check], name: &str, status: Status) -> Vec<&'a Check> {
    rows(checks, name).into_iter().filter(|c| c.status == status).collect()
}

/// **THE DEFECT: a tradehub that lost a dead gatewayhub read `HEALTH: OK`, because every engine still runs.** Every
/// lease redialling is a FAIL naming the gatewayhub, with how long and why, one line per account, and the exit says so.
#[test]
fn every_trade_link_lost_is_a_fail_naming_the_gatewayhub() {
    let mut snap = healthy();
    snap.trade_link_down = vec![lost_link("binance", 185_000), lost_link("bybit#DEMO", 60_000)];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Fail, "{checks:#?}");
    assert_eq!(exit_number(verdict(&checks)), 2);
    let row = hop_rows(&checks, "gateway-link", Status::Fail);
    assert_eq!(row.len(), 1, "{checks:#?}");
    assert_eq!(
        row[0].detail,
        "GATEWAYHUB DOWN: 2 of 2 trade links lost for 3m 5s (the gatewayhub is stopping), so no order \
         reaches a venue. The tradehub sees only its own links, and the engines still read RUNNING. On its \
         box: bin/vikectl status (system units: systemctl status vike-gatewayhub)"
    );
    assert_eq!(
        row[0].items,
        [
            "binance: down for 3m 5s (the gatewayhub is stopping)",
            "bybit#DEMO: down for 1m 0s (the gatewayhub is stopping)",
        ]
    );
    assert!(hop_rows(&checks, "gateway-link", Status::Pass).is_empty(), "{checks:#?}");
}

/// One lease down while another is up is that lease: the gatewayhub answers, so the headline would be false.
#[test]
fn one_trade_link_lost_beside_a_live_one_is_a_warn_about_that_lease() {
    let mut snap = healthy();
    snap.trade_link_down = vec![lost_link("bybit#DEMO", 12_000)];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Warn, "{checks:#?}");
    let row = hop_rows(&checks, "gateway-link", Status::Warn);
    assert_eq!(row.len(), 1, "{checks:#?}");
    assert_eq!(row[0].scope, "bybit#DEMO");
    assert_eq!(
        row[0].detail,
        "its trade link to the gatewayhub is DOWN for 12s (the gatewayhub is stopping), redialling: no order \
         of this account reaches the venue until it is back. 1 other trade link(s) are up, so the gatewayhub \
         answers: this is this lease"
    );
    assert!(!format!("{checks:?}").contains("GATEWAYHUB DOWN"), "{checks:#?}");
}

/// **A lease closed for good does not read RUNNING**, on the engine row either, and it is a FAIL however many others
/// are up: nothing heals it. Its engine row keeps its account and says where to look; another account's stays a PASS.
#[test]
fn a_lease_closed_for_good_does_not_read_running() {
    let mut snap = healthy();
    snap.trade_link_down = vec![closed_lease("binance")];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Fail, "{checks:#?}");
    let row = hop_rows(&checks, "gateway-link", Status::Fail);
    assert_eq!(row.len(), 1, "{checks:#?}");
    assert_eq!(row[0].scope, "binance");
    assert_eq!(
        row[0].detail,
        "its trade lease to the gatewayhub is CLOSED FOR GOOD (taken over by key-1): no order or cancel of this \
         account reaches the venue and nothing redials. Fix the cause, then set the account's `account.active` \
         off and on to open a new lease"
    );
    let engine = engine_row(&checks, "binance");
    assert_eq!(engine.status, Status::Warn);
    assert!(!engine.detail.contains("RUNNING"), "{}", engine.detail);
    assert_eq!(
        engine.detail,
        "NOT TRADING (demo): its trade lease is closed for good, see `gateway-link`"
    );
    assert_eq!(engine_row(&checks, "bybit#DEMO").status, Status::Pass);
}

/// Every lease closed is no claim that the gatewayhub is down: each was refused or taken over by something that answers.
#[test]
fn every_lease_closed_does_not_say_the_gatewayhub_is_down() {
    let mut snap = healthy();
    snap.trade_link_down = vec![closed_lease("binance"), closed_lease("bybit#DEMO")];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(hop_rows(&checks, "gateway-link", Status::Fail).len(), 2, "{checks:#?}");
    assert!(!format!("{checks:?}").contains("GATEWAYHUB DOWN"), "{checks:#?}");
}

/// **Every link up is a PASS for that hop alone, and an account nothing fresh speaks for says UNKNOWN, never OK**: the
/// trade link stops at the gatewayhub. Only a node that says it publishes the lists gets a PASS for an empty one.
#[test]
fn every_link_up_is_a_pass_and_a_session_nobody_answered_for_stays_unknown() {
    let mut snap = healthy();
    snap.venues.push(block("alpaca", None, "demo", TradingState::Active));
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Warn, "the alpaca engine has no active row here");
    let pass = hop_rows(&checks, "gateway-link", Status::Pass);
    assert_eq!(pass.len(), 1, "{checks:#?}");
    assert!(
        pass[0].detail.contains("3 trade link(s) to the gatewayhub are up"),
        "{}",
        pass[0].detail
    );
    let unknown = hop_rows(&checks, "gateway-link", Status::Unknown);
    assert_eq!(unknown.len(), 3, "one per account: {checks:#?}");
    let of = |key: &str| unknown.iter().find(|c| c.scope == key).expect(key).detail.as_str();
    assert!(of("binance").contains("no fresh answer"), "{}", of("binance"));
    assert!(of("bybit#DEMO").contains("no fresh answer"), "{}", of("bybit#DEMO"));
    assert!(of("alpaca").contains("reports no such path"), "{}", of("alpaca"));
    assert_eq!(only(&checks, "price-link").status, Status::Pass);
}

// ---- the gatewayhub's session with the venue, per account ----

/// The row of `key` on `gateway-link`.
fn session_row<'a>(checks: &'a [Check], key: &str) -> &'a Check {
    let mine =
        rows(checks, "gateway-link").into_iter().filter(|c| c.scope == key).collect::<Vec<_>>();
    assert_eq!(mine.len(), 1, "{key}: {checks:#?}");
    mine[0]
}

/// **THE DEFECT: `gateway-link` read UNKNOWN for every venue's session.** An account whose bridge said up reads up, one
/// that said down reads down in the bridge's words and what it costs, each on its own row.
#[test]
fn each_account_reads_its_own_session_with_the_venue() {
    let mut snap = healthy();
    snap.venue_path_up = vec!["binance".to_string()];
    snap.venue_link_down = vec![VenueLinkDownWire {
        route_key: "bybit#DEMO".to_string(),
        why: "reset by peer".to_string(),
        failed_restarts: 0,
        retry_in_ms: 30_000,
        effect: "orders still go out on their own path".to_string(),
        no_adapter: false,
    }];
    let checks = judge(NODE, &current_reads(snap));
    let binance = session_row(&checks, "binance");
    assert_eq!(binance.status, Status::Pass);
    assert!(binance.detail.contains("its path to the venue is up"), "{}", binance.detail);
    let bybit = session_row(&checks, "bybit#DEMO");
    assert_eq!(bybit.status, Status::Warn);
    assert_eq!(
        bybit.detail,
        "the venue path is DOWN: reset by peer (0 restart(s) failed so far, the next try in about 30 s); orders still \
         go out on their own path"
    );
    assert!(hop_rows(&checks, "gateway-link", Status::Unknown).is_empty(), "{checks:#?}");
}

/// **What the CI box shows now: Dukascopy failing and five FX accounts off on purpose.** A switched-off account reads off,
/// and no fault of this hop; one that failed to start reads why, in the refusal's words.
#[test]
fn a_switched_off_account_reads_off_on_purpose_and_a_failed_one_reads_why() {
    let mut snap = healthy();
    let mut dukascopy = not_running("dukascopy", None, Some("demo"), "adapter failed");
    dukascopy.detail =
        Some("the JForex sidecar would not start: the JNLP could not be fetched".to_string());
    snap.not_running = vec![dukascopy, not_running("ctrader", None, None, "account inactive")];
    let checks = judge(NODE, &current_reads(snap));

    let off = session_row(&checks, "ctrader");
    assert_eq!(off.status, Status::Pass);
    assert_eq!(
        off.detail,
        "no session with the venue, and none is wanted: account inactive, the operator's switch"
    );
    let failed = session_row(&checks, "dukascopy");
    assert_eq!(failed.status, Status::Warn);
    assert_eq!(
        failed.detail,
        "no session with the venue: adapter failed: the JForex sidecar would not start: the JNLP could not be fetched"
    );
}

/// **A switched-off account is read by its block's KEY, never its badge** (A3): a node that rewords the badge still has
/// its operator's switch read as off on purpose, and a key naming another block is not the switch, whatever the words
/// say. An older node sends no key and its badge is read as before (the test above).
#[test]
fn a_switched_off_account_is_read_by_its_block_key_not_its_badge() {
    let mut snap = healthy();
    let mut reworded = not_running("ctrader", None, None, "switched off by the operator");
    reworded.block_word = Some(vike_model::ArmingBlock::AccountInactive);
    let mut failed = not_running("dukascopy", None, Some("demo"), "account inactive");
    failed.block_word = Some(vike_model::ArmingBlock::AdapterFailed);
    snap.not_running = vec![reworded, failed];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(session_row(&checks, "ctrader").status, Status::Pass, "{checks:#?}");
    assert_eq!(session_row(&checks, "dukascopy").status, Status::Warn, "{checks:#?}");
}

/// A box with no lease at all still says why an account it wanted has no session; a paper account is not on the row.
#[test]
fn a_box_with_no_lease_still_reads_its_failed_account_and_paper_is_not_on_the_row() {
    let mut snap = healthy();
    snap.venues = vec![block("binance", None, "paper", TradingState::Active)];
    snap.not_running = vec![not_running("okx", None, Some("demo"), "adapter failed")];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(session_row(&checks, "okx").status, Status::Warn);
    assert!(rows(&checks, "gateway-link").iter().all(|c| c.scope != "binance"), "{checks:#?}");
}

/// **THE OWNER'S OLD COMPLAINT, `OK` at a dead gateway: a bridge that panicked.** The lease stays up, every order is
/// refused and nothing restarts it. That is a FAIL on this row and `NOT TRADING` on the engine's, as a closed lease is,
/// though the bridge's path says nothing and the trade link reads up.
#[test]
fn a_lease_with_no_live_adapter_is_a_fail_and_never_reads_running() {
    let mut snap = healthy();
    snap.venues.push(block("dukascopy", None, "demo", TradingState::Active));
    snap.venue_link_down = vec![VenueLinkDownWire {
        route_key: "dukascopy".to_string(),
        why: "the gatewayhub reports none on this lease".to_string(),
        failed_restarts: 0,
        retry_in_ms: 0,
        effect: "every order and cancel of this account is refused".to_string(),
        no_adapter: true,
    }];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Fail, "{checks:#?}");
    let session = session_row(&checks, "dukascopy");
    assert_eq!(session.status, Status::Fail);
    assert!(session.detail.contains("NO LIVE ADAPTER"), "{}", session.detail);
    let engine = engine_row(&checks, "dukascopy");
    assert_eq!(engine.status, Status::Warn);
    assert_eq!(
        engine.detail,
        "NOT TRADING (demo): the gatewayhub has no live adapter for it, see `gateway-link`"
    );
}

/// **The same verdict on a venue whose bridge reports no session (alpaca), or none yet (fxcm, ig and ibkr while their
/// bridge cannot vouch either way)**: with a live adapter and no answer the row stays UNKNOWN and moves no verdict, and
/// with no adapter it is the FAIL and the `NOT TRADING` of any other venue.
#[test]
fn a_venue_that_reports_no_session_reads_unknown_until_its_adapter_is_gone() {
    for venue in ["alpaca", "fxcm", "ig", "ibkr"] {
        let mut snap = healthy();
        snap.venues.push(block(venue, None, "demo", TradingState::Active));
        let checks = judge(NODE, &current_reads(snap.clone()));
        assert_eq!(session_row(&checks, venue).status, Status::Unknown, "{venue}");

        snap.venue_link_down = vec![VenueLinkDownWire {
            route_key: venue.to_string(),
            why: "the gatewayhub reports none on this lease".to_string(),
            failed_restarts: 0,
            retry_in_ms: 0,
            effect: "every order and cancel of this account is refused".to_string(),
            no_adapter: true,
        }];
        let checks = judge(NODE, &current_reads(snap));
        assert_eq!(verdict(&checks), Verdict::Fail, "{venue}: {checks:#?}");
        assert_eq!(session_row(&checks, venue).status, Status::Fail, "{venue}");
        let engine = engine_row(&checks, venue);
        assert!(engine.detail.starts_with("NOT TRADING"), "{venue}: {}", engine.detail);
    }
}

/// **A lost trade link is judged by its own row**: no second row says the session is unknown behind a link that is down.
#[test]
fn an_account_whose_trade_link_is_lost_has_no_session_row_of_its_own() {
    let mut snap = healthy();
    snap.trade_link_down = vec![lost_link("bybit#DEMO", 12_000)];
    let checks = judge(NODE, &current_reads(snap));
    let on_row = rows(&checks, "gateway-link").into_iter().filter(|c| c.scope == "bybit#DEMO");
    assert_eq!(on_row.count(), 1, "{checks:#?}");
}

// ---- when the hub rows may lean on another row ----

/// **A flat box with no mount: every dial idle, and the datahub dead reads exactly the same.** `price-link` read PASS,
/// `datahub` pointed at it, and the whole read `HEALTH: OK` though no session was read. Nothing is claimed now.
#[test]
fn every_dial_idle_is_no_reading_of_the_datahub_and_the_datahub_row_does_not_lean_on_it() {
    let mut snap = healthy();
    snap.price_dials_up = Some(0);
    let answer = DatahubAnswer::NoAnswer("connection refused".to_string());
    let checks = judge(NODE, &Reads { hubs: leg(answer), ..current_reads(snap.clone()) });
    let price = only(&checks, "price-link");
    assert_eq!(price.status, Status::Unknown, "{}", price.detail);
    assert!(price.detail.contains("no price feed holds a session"), "{}", price.detail);
    let (word, detail) = said_of(&checks, "datahub");
    assert_eq!(word, Status::Unknown);
    assert!(!detail.contains("`price-link`"), "{detail}");

    // One dial up is a reading: the row is the PASS it was and the pointer is back.
    snap.price_dials_up = Some(1);
    let answer = DatahubAnswer::NoAnswer("connection refused".to_string());
    let checks = judge(NODE, &Reads { hubs: leg(answer), ..current_reads(snap.clone()) });
    assert_eq!(only(&checks, "price-link").status, Status::Pass);
    assert!(said_of(&checks, "datahub").1.contains("`price-link`"));

    // A lost lane is a fact whatever the count says, and a node that does not say is read as it always was.
    snap.price_dials_up = Some(0);
    snap.price_link_down = vec![lost_feed("binance", 2)];
    assert_eq!(only(&judge(NODE, &current_reads(snap.clone())), "price-link").status, Status::Warn);
    snap.price_dials_up = None;
    snap.price_link_down.clear();
    assert_eq!(only(&judge(NODE, &current_reads(snap)), "price-link").status, Status::Pass);
}

/// **A dead gatewayhub with no lease row to judge the orders is a FAIL on its own row**: a STARTING door and a pre-fold
/// one read `DEGRADED ... gateway-link judges those` beside no such row, and STARTING added "ask again shortly".
#[test]
fn a_dead_gatewayhub_beside_a_door_with_no_engines_is_a_fail() {
    let text = format!("tradehub snapshot error: {}", starting_error("starting the accounts"));
    let starting = Reads {
        snap: Err(io::Error::new(io::ErrorKind::InvalidData, text)),
        features: None,
        directory: None,
        mounts: None,
        hubs: leg(gateway_down()),
    };
    let checks = judge(NODE, &starting);
    let (word, detail) = said_of(&checks, "gatewayhub");
    assert_eq!(word, Status::Fail, "{detail}");
    assert_eq!(detail, format!("the datahub cannot reach it: {GATEWAY_DOWN}"));
    assert_eq!(verdict(&checks), Verdict::Fail);

    let pre_fold = Reads {
        snap: Ok(CoreSnapshotWire::empty()),
        features: None,
        directory: None,
        mounts: None,
        hubs: leg(gateway_down()),
    };
    let checks = judge(NODE, &pre_fold);
    assert_eq!(only(&checks, "gatewayhub").status, Status::Fail, "{checks:#?}");
}

/// **A box whose only running engines are paper beside an account that failed to start wants the gatewayhub**, and no
/// lease can speak for it: FAIL. A paper-only box, or one whose other accounts are all switched off, keeps today's words.
#[test]
fn a_dead_gatewayhub_is_a_fail_where_an_account_wants_it_and_keeps_its_words_where_none_does() {
    let paper = || {
        let mut snap = healthy();
        snap.venues = vec![block("binance", None, "paper", TradingState::Active)];
        snap
    };
    let mut mixed = paper();
    mixed.not_running = vec![not_running("okx", None, Some("demo"), "adapter failed")];
    let checks = judge(NODE, &Reads { hubs: leg(gateway_down()), ..current_reads(mixed) });
    assert_eq!(only(&checks, "gatewayhub").status, Status::Fail, "{checks:#?}");

    let degraded = |snap: CoreSnapshotWire| {
        let checks = judge(NODE, &Reads { hubs: leg(gateway_down()), ..current_reads(snap) });
        assert!(rows(&checks, "gatewayhub").is_empty(), "{checks:#?}");
        let (word, detail) = said_of(&checks, "datahub");
        assert_eq!(word, Status::Warn);
        assert!(detail.starts_with("DEGRADED: it serves the store"), "{detail}");
    };
    degraded(paper());
    let mut off = paper();
    off.not_running = vec![not_running("okx", None, None, "account inactive")];
    degraded(off);
}

/// **A wedged gatewayhub takes the handshake and gives no report**: that is no proof the daemon answers, so every lease
/// lost reads as the gatewayhub's, never as this tradehub's side of it.
#[test]
fn every_lease_lost_beside_a_gatewayhub_that_gave_no_report_does_not_blame_this_tradehub() {
    let mut snap = healthy();
    snap.trade_link_down = snap.venues.iter().map(|b| lost_link(&b.route_key, 65_000)).collect();
    let wedged = DatahubAnswer::Said(GatewayStatusReply {
        reached: true,
        error: Some("no report in time".to_string()),
        report: None,
    });
    let checks = judge(NODE, &Reads { hubs: leg(wedged), ..current_reads(snap) });
    let row = rows(&checks, "gateway-link").into_iter().find(|c| c.status == Status::Fail);
    let detail = &row.expect("the FAIL row").detail;
    assert!(detail.starts_with("GATEWAYHUB DOWN: 2 of 2 trade links lost"), "{detail}");
    assert!(detail.contains("gave no report"), "{detail}");
    assert!(!detail.contains("this tradehub's side"), "{detail}");
}

/// **The datahub asked is this client's, whatever `--node` says, so a node on another box would be judged against the
/// wrong hubs in silence.** A node that advertises another one is said beside the two rows, and moves no verdict: a tunnel
/// to the right datahub differs too. One that advertises the same address, or none, adds nothing.
#[test]
fn a_node_advertising_another_datahub_is_said_and_moves_no_verdict() {
    let with_advert = |feature: Option<&str>| {
        let mut r = Reads { hubs: leg(up()), ..current_reads(healthy()) };
        if let (Some(feature), Some(Ok(features))) = (feature, r.features.as_mut()) {
            features.push(feature.to_string());
        }
        judge(NODE, &r)
    };
    let checks = with_advert(Some("datahub=<host>:7878"));
    let datahub = rows(&checks, "datahub");
    assert_eq!(datahub.len(), 2, "{checks:#?}");
    assert_eq!(datahub[1].status, Status::Unknown);
    assert!(datahub[1].detail.contains(DATAHUB), "{}", datahub[1].detail);
    assert!(datahub[1].detail.contains("advertises <host>:7878"), "{}", datahub[1].detail);

    assert_eq!(rows(&with_advert(Some(&format!("datahub={DATAHUB}"))), "datahub").len(), 1);
    assert_eq!(rows(&with_advert(None), "datahub").len(), 1);
}

/// A node that predates the lists writes the same nothing: its rows say so and move no verdict, and it is told from a
/// node whose capabilities could not be read. A non-empty list is a fact whatever the capabilities say.
#[test]
fn an_empty_list_from_a_node_that_does_not_say_it_publishes_one_proves_nothing() {
    let checks = judged(healthy());
    assert_eq!(verdict(&checks), Verdict::Ok, "{checks:#?}");
    for name in ["gateway-link", "price-link"] {
        let c = only(&checks, name);
        assert_eq!(c.status, Status::Unknown, "{name}");
        assert!(c.detail.starts_with("unknown (node too old)"), "{name}: {}", c.detail);
    }

    let unread = Reads { features: Some(Err(io::Error::other("down"))), ..reads(healthy()) };
    let checks = judge(NODE, &unread);
    for name in ["gateway-link", "price-link"] {
        assert!(only(&checks, name).detail.contains("capabilities could not be read"), "{name}");
    }

    let mut snap = healthy();
    snap.trade_link_down = vec![lost_link("binance", 1_000), lost_link("bybit#DEMO", 1_000)];
    let older_says_nothing = judge(NODE, &reads(snap));
    assert_eq!(verdict(&older_says_nothing), Verdict::Fail, "a list is a fact");
}

/// **A paper-only box has no link to claim**: no row at all, whatever it advertises.
#[test]
fn a_paper_only_box_has_no_link_rows() {
    let mut snap = healthy();
    snap.venues = vec![block("binance", None, "paper", TradingState::Active)];
    let checks = judge(NODE, &current_reads(snap));
    assert!(rows(&checks, "gateway-link").is_empty(), "{checks:#?}");
    assert!(rows(&checks, "price-link").is_empty(), "{checks:#?}");
}

/// Every price feed lost is the datahub: FAIL, words naming it, strategies blind. One venue's lane beside a live one is
/// that lane: WARN. The `price-feeds` row stays what it was: a connection is not a fresh price.
#[test]
fn every_price_link_lost_is_a_fail_naming_the_datahub_and_one_is_a_warn() {
    let mut snap = healthy();
    snap.price_link_down = vec![lost_feed("binance", 2), lost_feed("bybit", 2)];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Fail, "{checks:#?}");
    let row = only(&checks, "price-link");
    assert_eq!(row.status, Status::Fail);
    assert_eq!(
        row.detail,
        "DATAHUB unreachable from the tradehub: 2 of 2 price feeds lost for 1m 5s (connection refused), so the \
         strategies are blind. On its box: bin/vikectl status (system units: systemctl status vike-datahub)"
    );
    assert_eq!(row.items.len(), 2);
    assert_eq!(only(&checks, "price-feeds").status, Status::Unknown);

    let mut snap = healthy();
    snap.price_link_down = vec![lost_feed("bybit", 3)];
    let checks = judge(NODE, &current_reads(snap));
    assert_eq!(verdict(&checks), Verdict::Warn, "{checks:#?}");
    let row = only(&checks, "price-link");
    assert_eq!(row.status, Status::Warn);
    assert_eq!(row.scope, "bybit");
    assert_eq!(
        row.detail,
        "its price feed's lane to the datahub is DOWN for 1m 5s (connection refused): this venue's strategies \
         hear no quote, tick or bar. 2 other price feed(s) are up, so the datahub answers: this is this lane"
    );
}

/// `adapter failed` says what lies behind it when the node kept that, and a row with no cause reads as it always did.
#[test]
fn a_not_running_row_carries_the_cause_behind_its_word() {
    let mut snap = healthy();
    snap.venues.clear();
    let mut row = not_running("binance", None, Some("demo"), "adapter failed");
    row.detail = Some("the lease is held by key-9".to_string());
    snap.not_running =
        vec![row, not_running("bybit", Some("DEMO"), Some("demo"), "adapter failed")];
    let checks = judged(snap);
    assert_eq!(
        engine_row(&checks, "binance").detail,
        "NOT RUNNING (demo): adapter failed: the lease is held by key-9"
    );
    assert_eq!(engine_row(&checks, "bybit#DEMO").detail, "NOT RUNNING (demo): adapter failed");
}

// ---- a stale duplicate block ----

#[test]
fn one_route_key_published_twice_is_a_fail() {
    let mut snap = healthy();
    snap.venues.insert(1, block("bybit", Some("DEMO"), "demo", TradingState::Active));
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
    let r = Reads { snap: Ok(snap), features: None, directory: None, mounts: None, hubs: None };
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
        mounts: None,
        hubs: None,
    };
    let checks = judge(NODE, &r);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Warn);
    assert!(checks[0].detail.starts_with("STARTING"), "{}", checks[0].detail);
    assert_eq!(
        checks[0].items,
        vec!["starting the accounts; accounts: binance up, dukascopy starting".to_string()]
    );
    assert_eq!(summary_line(&checks), "HEALTH: WARN 1");
}

#[test]
fn an_unreachable_node_and_a_refused_key_are_fails_and_ask_for_nothing_more() {
    let down = Reads {
        snap: Err(io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused")),
        features: None,
        directory: None,
        mounts: None,
        hubs: None,
    };
    let checks = judge(NODE, &down);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Fail);
    assert!(
        checks[0].detail.starts_with("TRADEHUB at <host>:7879 does not answer:"),
        "{}",
        checks[0].detail
    );
    assert!(checks[0].items[0].contains("vike-cli backend status"), "{:?}", checks[0].items);
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 1");

    let denied = Reads {
        snap: Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "tradehub snapshot denied: bad mac",
        )),
        features: None,
        directory: None,
        mounts: None,
        hubs: None,
    };
    let checks = judge(NODE, &denied);
    assert_eq!(checks[0].status, Status::Fail);
    assert!(checks[0].detail.contains("refused the observe handshake"));
    assert!(checks[0].items.iter().any(|i| i.contains(OBSERVE_KEY_ENV)), "{:?}", checks[0].items);
}

// ---- the datahub's word on the gatewayhub ----

const DATAHUB: &str = "<host>:7878";
const GATEWAY_DOWN: &str = "gatewayhub 127.0.0.1:7880: cannot connect: connection refused";

fn up() -> DatahubAnswer {
    DatahubAnswer::Said(GatewayStatusReply {
        reached: true,
        error: None,
        report: Some(vike_datahub_client::gateway_status::GatewayStatusReportWire {
            epoch: 7,
            started_ms: 1_000,
            uptime_ms: 3_600_000,
            connections: 2,
            stream_sessions: 1,
            fetches_running: 0,
            market_data_venues: Vec::new(),
            credentialed_data_venues: Vec::new(),
            budget: None,
        }),
    })
}

fn gateway_down() -> DatahubAnswer {
    DatahubAnswer::Said(GatewayStatusReply {
        reached: false,
        error: Some(GATEWAY_DOWN.to_string()),
        report: None,
    })
}

fn leg(answer: DatahubAnswer) -> Option<HubLeg> {
    Some(HubLeg { addr: DATAHUB.to_string(), answer })
}

/// The whole table of a node whose door does not answer, beside what the datahub answered.
fn door_silent(answer: DatahubAnswer) -> Vec<Check> {
    let snap = Err(io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused"));
    let r = Reads { snap, features: None, directory: None, mounts: None, hubs: leg(answer) };
    judge(NODE, &r)
}

/// A row by name as `(word, detail)`.
fn said_of(checks: &[Check], name: &str) -> (Status, String) {
    let row = only(checks, name);
    (row.status, row.detail.clone())
}

#[test]
fn a_dead_tradehub_with_a_live_gatewayhub_names_each_hop() {
    let checks = door_silent(up());
    assert_eq!(only(&checks, "door").status, Status::Fail);
    assert_eq!(said_of(&checks, "datahub"), (Status::Pass, format!("answers at {DATAHUB}")));
    let (word, detail) = said_of(&checks, "gatewayhub");
    assert_eq!(word, Status::Pass);
    assert!(detail.starts_with("up 1h 0m, per the datahub (2 connection(s)"), "{detail}");
    assert!(detail.contains("says nothing of any venue"), "{detail}");
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 1");
}

#[test]
fn a_dead_tradehub_with_a_dead_gatewayhub_is_two_fails() {
    let checks = door_silent(gateway_down());
    assert_eq!(only(&checks, "datahub").status, Status::Pass);
    assert_eq!(
        said_of(&checks, "gatewayhub"),
        (Status::Fail, format!("the datahub cannot reach it: {GATEWAY_DOWN}"))
    );
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 2");
}

#[test]
fn a_dead_tradehub_and_a_dead_datahub_leave_the_gatewayhub_unknown() {
    let checks = door_silent(DatahubAnswer::NoAnswer("connection refused".to_string()));
    assert_eq!(
        said_of(&checks, "datahub"),
        (Status::Fail, format!("DATAHUB at {DATAHUB} does not answer: connection refused"))
    );
    let (word, detail) = said_of(&checks, "gatewayhub");
    assert_eq!(word, Status::Unknown);
    assert!(detail.contains("no hub can see it"), "{detail}");
    assert!(
        detail.contains(
            "On its box: bin/vikectl status (system units: systemctl status vike-gatewayhub)"
        ),
        "{detail}"
    );
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 2");
}

/// ⚠ An old datahub is `unknown`, never "down", and moves no verdict.
#[test]
fn an_old_datahub_reads_unknown_and_moves_no_verdict() {
    let checks = door_silent(DatahubAnswer::TooOld);
    assert_eq!(only(&checks, "datahub").status, Status::Pass);
    let (word, detail) = said_of(&checks, "gatewayhub");
    assert_eq!(word, Status::Unknown);
    assert!(detail.starts_with("unknown (datahub too old)"), "{detail}");
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 1");
}

#[test]
fn a_datahub_that_refuses_this_box_judges_neither_hub() {
    let checks = door_silent(DatahubAnswer::Refused("bad mac".to_string()));
    let (word, detail) = said_of(&checks, "datahub");
    assert_eq!(word, Status::Unknown);
    assert!(detail.contains("the gatewayhub and datahub are not judged"), "{detail}");
    assert!(rows(&checks, "gatewayhub").is_empty());
    assert_eq!(summary_line(&checks), "HEALTH: FAIL 1");
}

/// A STARTING door answered, so the legs follow it, and a datahub this client cannot dial is no FAIL.
#[test]
fn a_starting_door_keeps_its_warn_and_carries_the_same_legs() {
    let starting = |answer| {
        let text = format!("tradehub snapshot error: {}", starting_error("starting the accounts"));
        let snap = Err(io::Error::new(io::ErrorKind::InvalidData, text));
        Reads { snap, features: None, directory: None, mounts: None, hubs: leg(answer) }
    };
    let checks = judge(NODE, &starting(up()));
    assert_eq!(only(&checks, "gatewayhub").status, Status::Pass);
    assert_eq!(summary_line(&checks), "HEALTH: WARN 1");

    let checks = judge(NODE, &starting(DatahubAnswer::NoAnswer("refused".to_string())));
    assert_eq!(only(&checks, "datahub").status, Status::Unknown);
    assert_eq!(summary_line(&checks), "HEALTH: WARN 1");
}

/// The door answered, every link is up, and the datahub cannot reach the gatewayhub: a degraded datahub, and the row
/// claims nothing about orders.
#[test]
fn a_live_node_beside_a_datahub_that_lost_the_gatewayhub_reads_degraded() {
    let r = Reads { hubs: leg(gateway_down()), ..current_reads(healthy()) };
    let checks = judge(NODE, &r);
    let (word, detail) = said_of(&checks, "datahub");
    assert_eq!(word, Status::Warn);
    assert!(
        detail.starts_with(&format!(
            "DEGRADED: it serves the store, new venue data fails: {GATEWAY_DOWN}"
        )),
        "{detail}"
    );
    assert!(detail.contains("`gateway-link` judges those"), "{detail}");
    assert!(rows(&checks, "gatewayhub").is_empty(), "the links are up: no FAIL for the daemon");
    assert_eq!(rows(&checks, "gateway-link")[0].status, Status::Pass);
}

#[test]
fn a_datahub_this_client_cannot_dial_moves_no_verdict_beside_a_live_node() {
    let answer = DatahubAnswer::NoAnswer("connection refused".to_string());
    let r = Reads { hubs: leg(answer), ..current_reads(healthy()) };
    let checks = judge(NODE, &r);
    let (word, detail) = said_of(&checks, "datahub");
    assert_eq!(word, Status::Unknown);
    assert!(detail.contains("A dead SSH forward reads the same"), "{detail}");
    assert!(detail.contains("`price-link`"), "{detail}");
    assert!(rows(&checks, "gatewayhub").is_empty());
    assert_eq!(count(&checks, Status::Fail), 0, "{checks:?}");
}

/// ⚠ **Every lease lost is `GATEWAYHUB DOWN` only if nothing says otherwise.** The datahub reaching the daemon turns it
/// into this tradehub's side; the datahub not reaching it agrees.
#[test]
fn every_lease_lost_reads_by_what_the_datahub_saw() {
    let mut snap = healthy();
    snap.trade_link_down = snap.venues.iter().map(|b| lost_link(&b.route_key, 65_000)).collect();

    let r = Reads { hubs: leg(up()), ..current_reads(snap.clone()) };
    let checks = judge(NODE, &r);
    let row = rows(&checks, "gateway-link").into_iter().find(|c| c.status == Status::Fail);
    let detail = &row.expect("the FAIL row").detail;
    assert!(
        detail.starts_with("ALL TRADE LINKS LOST: 2 of 2 trade links lost for 1m 5s"),
        "{detail}"
    );
    assert!(detail.contains("this tradehub's side"), "{detail}");
    assert!(!detail.contains("GATEWAYHUB DOWN"), "{detail}");

    let r = Reads { hubs: leg(gateway_down()), ..current_reads(snap) };
    let checks = judge(NODE, &r);
    let row = rows(&checks, "gateway-link").into_iter().find(|c| c.status == Status::Fail);
    let detail = &row.expect("the FAIL row").detail;
    assert!(detail.starts_with("GATEWAYHUB DOWN: 2 of 2 trade links lost for 1m 5s"), "{detail}");
    assert!(detail.contains("The datahub cannot reach it either"), "{detail}");
}

#[test]
fn a_gatewayhub_that_takes_the_handshake_and_gives_no_report_is_a_warn() {
    let answer = DatahubAnswer::Said(GatewayStatusReply {
        reached: true,
        error: Some(
            "gatewayhub at 127.0.0.1:7880 does not advertise \"gateway_status\"".to_string(),
        ),
        report: None,
    });
    let checks = door_silent(answer);
    let (word, detail) = said_of(&checks, "gatewayhub");
    assert_eq!(word, Status::Warn);
    assert!(detail.contains("gave no report"), "{detail}");
    assert!(detail.contains("gateway_status"), "{detail}");
}

#[test]
fn a_pre_fold_frame_is_a_warn_and_nothing_else_is_judged() {
    let mut snap = healthy();
    snap.sequence = 0;
    let r = Reads { snap: Ok(snap), features: None, directory: None, mounts: None, hubs: None };
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
    let judged_with = |orders: Vec<OrderViewWire>| {
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
    assert_eq!(summary_line(&checks), "HEALTH: WARN 1");
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
    assert!(silent.detail.contains("bin/vikectl status"), "{}", silent.detail);
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
    snap.sequence = 0;
    snap.uptime_ms = Some(5_000);
    snap.halt_sentinel = Some(true);
    let r = Reads { snap: Ok(snap), features: None, directory: None, mounts: None, hubs: None };
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

/// **A held balance is a WARN row naming the account and the reason**, one per account; a node that holds nothing
/// back (or predates the field) gets NO row, because a PASS would claim a fact it cannot know.
#[test]
fn a_held_balance_is_a_warn_naming_the_account_and_the_reason_and_none_is_silent() {
    let checks = judged(healthy());
    assert!(rows(&checks, "balance").is_empty(), "nothing held: no row at all");

    let mut snap = healthy();
    snap.balance_withheld = vec![
        BalanceWithheldWire { route_key: "bybit#DEMO".into(), reason: "assumed USD".into() },
        BalanceWithheldWire {
            route_key: "binance".into(),
            reason: "stated EUR not anchored".into(),
        },
    ];
    let checks = judged(snap);
    let held = rows(&checks, "balance");
    assert_eq!(held.len(), 2, "one row per account");
    assert_eq!((held[0].status, held[0].scope.as_str()), (Status::Warn, "bybit#DEMO"));
    assert!(held[0].detail.starts_with("balance held: assumed USD"), "{}", held[0].detail);
    assert!(
        held[1].detail.starts_with("balance held: stated EUR not anchored"),
        "{}",
        held[1].detail
    );
    assert!(held[1].detail.contains("seed + PnL"), "{}", held[1].detail);
}

/// **A stale conversion rate that values an open position is a WARN row naming the account and the rate**
/// (decision 0183: it was invisible, the node valuing the position at the last rate with no word). A stale
/// rate nobody holds anything in, a fresh one that is held, an older node's table (no `held` key) and a
/// stopped account's retained block all get NO row: a PASS would claim what the frame cannot know.
#[test]
fn a_stale_rate_under_a_held_position_is_a_warn_and_nothing_else_is() {
    use vike_tradehub_client::wire::QuoteRateViewWire;
    let rate = |currency_value: &str, stale: bool, held: bool| QuoteRateViewWire {
        currency: currency_value.into(),
        rate: 0.862_069,
        as_of_ms: 1,
        stale,
        held,
    };
    let with_rates = |rates: Vec<QuoteRateViewWire>| {
        let mut snap = healthy();
        snap.venues[1].quote_rates = rates;
        snap.venues[1].quote_rates_in = "EUR".into();
        snap
    };
    for quiet in [
        vec![],
        vec![rate("USD", false, false)],
        vec![rate("USD", false, true)],
        vec![rate("JPY", true, false)],
    ] {
        let checks = judged(with_rates(quiet.clone()));
        assert!(rows(&checks, "rate").is_empty(), "{quiet:?}: {checks:#?}");
    }

    let checks = judged(with_rates(vec![rate("JPY", true, false), rate("USD", true, true)]));
    let warn = only(&checks, "rate");
    assert_eq!((warn.status, warn.scope.as_str()), (Status::Warn, "bybit#DEMO"));
    assert!(warn.detail.contains("quote-rate-stale"), "{}", warn.detail);
    assert_eq!(
        warn.items,
        ["1 USD = 0.862069 EUR STALE (an open position is valued at it)"],
        "only the held rate is named"
    );
    assert_eq!(verdict(&checks), Verdict::Warn);

    // a stopped account's retained block is history: nothing manages it and nothing is valued by a live rate
    let mut snap = with_rates(vec![rate("USD", true, true)]);
    snap.not_running = vec![not_running("bybit", Some("DEMO"), Some("demo"), "no credentials")];
    assert!(rows(&judged(snap), "rate").is_empty());
}

#[test]
fn a_hyperliquid_account_adds_the_leverage_and_approval_rows() {
    let mut snap = healthy();
    snap.venues.push(block("hyperliquid", Some("LIVE"), "live", TradingState::Active));
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

/// **A directory that read and lists nothing is an answer**: no account is active, so an engine that
/// runs has no row stating it, the same WARN as one row missing among others. Only a FAILED read leaves
/// the rows unknown, and that keeps its own row and judges the engines on their own.
#[test]
fn a_directory_that_read_empty_is_no_active_row_not_an_unread_one() {
    let mut r = reads(healthy());
    r.directory = Some(Ok(AccountDirectoryWire { venues: Vec::new(), accounts: Vec::new() }));
    let checks = judge(NODE, &r);
    assert!(rows(&checks, "rows").is_empty(), "the read answered: {checks:#?}");
    let row = engine_row(&checks, "binance");
    assert_eq!(row.status, Status::Warn, "{}", row.detail);
    assert!(row.detail.contains("no active row states it"), "{}", row.detail);
    assert_eq!(verdict(&checks), Verdict::Warn);
}

/// **A failed `StrategyStatus` read is said on the `mounts` row**, where it used to read the same as "no
/// mount is latched". UNKNOWN, so it moves no verdict, and it names the command that answers. A read that
/// answered with nothing latched still adds no row.
#[test]
fn a_failed_strategy_status_read_shows_the_mounts_row_as_unread() {
    let unread = Reads {
        features: Some(Err(io::Error::other("the node hung up"))),
        mounts: None,
        ..reads(healthy())
    };
    let checks = judge(NODE, &unread);
    let row = only(&checks, "mounts");
    assert_eq!(row.status, Status::Unknown, "{}", row.detail);
    assert!(row.detail.contains("the node hung up"), "{}", row.detail);
    assert!(row.detail.contains("vike-cli trade strategy ls"), "{}", row.detail);
    assert_eq!(verdict(&checks), Verdict::Ok, "{checks:#?}");

    assert!(rows(&judged(healthy()), "mounts").is_empty(), "a read that answered says nothing");
}

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
    snap.venues[1].trading_state = TradingState::Halted;
    snap.reconcile = vec![reconcile_row(12, "OrphanLocalPosition")];
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
