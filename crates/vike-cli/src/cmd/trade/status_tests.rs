use super::*;
// `OBSERVE_KEY_ENV` itself is no longer read by this file's PRODUCTION code — the
// `PermissionDenied` arm delegates to `vike_client::hubkeys::observe_permission_denied_lines`,
// which owns that reference now. Only this test still names it, to assert the shared line
// mentions the right variable.
use vike_client::hubkeys::OBSERVE_KEY_ENV;
use vike_tradehub_client::wire::{
    ArmingNotRunningViewWire, HubIdentityWire, MountViewWire, PriceLinkDownWire, TradeLinkDownWire,
    VenueBlockWire, VenueLinkDownWire,
};

fn parsed(args: &[&str]) -> Result<Args, args::Stop> {
    parse(args.iter().map(|s| s.to_string()))
}

/// A two-mount status, one live row and one paper — every render property below is visible in
/// one fixture: identity vs mount params, the LIVE/paper casing, and column alignment across
/// names of different lengths.
fn two_mounts() -> StrategyStatusWire {
    StrategyStatusWire {
        identity: HubIdentityWire {
            name: "hub-a".to_string(),
            strategy: "spread_maker".to_string(),
            params: "spread=0.01".to_string(),
            live: false,
            build: "vike-tradehub 0.1.0 (abc1234, clean)".to_string(),
            advertise_addr: String::new(),
        },
        effective_params: "spread=0.01 size=20".to_string(),
        mounts: vec![
            // The addressing key + typed params a node advertising `strategy-params` fills in;
            // these fixtures keep them EMPTY, which is what a node predating that capability
            // sends and what this table has always rendered — so the assertions below are
            // unchanged by the fields existing.
            MountViewWire {
                order_client_id_code: None,
                strategy: "spread_maker".to_string(),
                params: "venue=polymarket symbol=TOK-A spread=0.01".to_string(),
                live: false,
                venue: String::new(),
                symbol: String::new(),
                interval: String::new(),
                mount_id: String::new(),
                typed_params: None,
                asset_class: None,
                latched: false,
                route_key: String::new(),
                tier: Some("paper".to_string()),
                not_running: false,
            },
            MountViewWire {
                order_client_id_code: None,
                strategy: "np".to_string(),
                params: "venue=polymarket symbol=TOK-B edge=0.02".to_string(),
                live: true,
                venue: String::new(),
                symbol: String::new(),
                interval: String::new(),
                mount_id: String::new(),
                typed_params: None,
                asset_class: None,
                latched: false,
                route_key: String::new(),
                tier: Some("live".to_string()),
                not_running: false,
            },
        ],
    }
}

/// A built frame (`seq` 1: only a fold moves it) carrying just the two fields this command reads off one.
fn snap(state: TradingState, fault: Option<&str>) -> CoreSnapshotWire {
    let mut s = CoreSnapshotWire::empty();
    s.sequence = 1;
    s.trading_state = state;
    s.fault = fault.map(str::to_string);
    s
}

// ---- the grammar ----

#[test]
fn both_flag_forms_parse_and_json_defaults_off() {
    let a = parsed(&["--node", "<host>:9200"]).unwrap();
    assert_eq!(a, Args { node: "<host>:9200".to_string(), json: false });
    let a = parsed(&["--node=<host>:9200", "--json"]).unwrap();
    assert_eq!(a, Args { node: "<host>:9200".to_string(), json: true });
}

#[test]
fn a_missing_node_errors_naming_the_flag() {
    let err = parsed(&["--json"]).unwrap_err();
    assert!(err.contains("--node"), "{err}");
}

#[test]
fn json_is_a_bare_boolean_and_an_inline_value_is_a_usage_error() {
    let err = parsed(&["--node", "n:1", "--json=1"]).unwrap_err();
    assert!(!err.is_help(), "{err}");
    assert!(err.contains("--json"), "the refusal names the flag: {err}");
}

#[test]
fn help_short_circuits_even_without_a_node() {
    for spelling in ["-h", "--help"] {
        assert!(parsed(&[spelling]).unwrap_err().is_help(), "{spelling}");
    }
}

/// A repeated `--node` is last-wins, as it always was: a runbook line that appends its own
/// `--node` to a wrapper's keeps working.
#[test]
fn a_repeated_node_is_last_wins() {
    let a = parsed(&["--node", "n:1", "--node=n:2"]).unwrap();
    assert_eq!(a, Args { node: "n:2".to_string(), json: false });
}

#[test]
fn an_unknown_argument_is_rejected_by_name() {
    let err = parsed(&["--node", "n:1", "--verbose"]).unwrap_err();
    assert!(err.contains("--verbose"), "{err}");
}

/// ⚠ **`state` may not come back as an argument of this verb.** The whole of ruling 17 is that
/// nothing which CHANGES the daemon's mode may be spelled by adding a word to something that
/// does not, so `trade status halted` must be a usage error rather than a halt — and it is,
/// because the grammar takes flags only and a bare word is an unexpected argument. (`clap` never
/// echoes a positional, so the refusal cannot NAME the word: it says `<hidden>`.)
#[test]
fn a_bare_mode_word_is_a_usage_error_and_never_a_write() {
    for word in ["halted", "reducing", "active", "state"] {
        let err = parsed(&["--node", "n:1", word]).unwrap_err();
        assert!(!err.is_help(), "{word}: {err}");
        assert!(!err.contains(word), "a positional is never echoed: {word}: {err}");
    }
}

// ---- the mode half ----

#[test]
fn the_mode_line_names_the_state_and_a_fault_is_its_own_line() {
    assert_eq!(mode_lines(TradingState::Halted, None), vec!["trading state: Halted"]);
    let faulted = mode_lines(TradingState::Active, Some("fold panic in engine 0"));
    assert_eq!(faulted.len(), 2, "{faulted:?}");
    assert!(faulted[1].contains("FAULT"), "{faulted:?}");
    assert!(faulted[1].contains("fold panic in engine 0"), "{faulted:?}");
}

/// One engine's block with just the fields this command reads (the rest is `serde`'s default).
fn block(route_key: &str, state: TradingState) -> VenueBlockWire {
    let json = serde_json::json!({
        "venue": route_key.split('#').next().unwrap(), "balance": 0.0, "realized_pnl": 0.0,
        "fees_paid": 0.0, "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0,
        "missing_prices": 0, "margin_used": 0.0, "free_buying_power": 0.0, "positions": [],
        "trading_state": format!("{state:?}"), "route_key": route_key,
    });
    serde_json::from_value(json).expect("a venue block")
}

/// ⚠ **Decision 0180's blind spot.** `trading state:` is the PRIMARY engine's; a venue-scoped link
/// dead-man halt halts ONE other venue while the primary reads Active. The per-venue lines name it,
/// and a node whose engines are all Active gets no extra line (byte-identical to before).
#[test]
fn a_halted_or_reducing_venue_is_named_under_the_primarys_state() {
    let mut s = snap(TradingState::Active, None);
    s.venues = vec![
        block("binance", TradingState::Active),
        block("ibkr", TradingState::Halted),
        block("bybit#ALT", TradingState::Halted),
        block("okx", TradingState::Reducing),
    ];
    let status = two_mounts();
    let lines = human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    assert_eq!(lines[0], "trading state: Active", "the top line keeps its wording");
    assert_eq!(lines[1], "halted venues: ibkr, bybit#ALT");
    assert_eq!(lines[2], "reducing venues: okx");
    assert!(lines[3].starts_with("node hub-a"), "{lines:?}");

    s.venues = vec![block("binance", TradingState::Active)];
    let lines = human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    assert!(lines[1].starts_with("node hub-a"), "all Active adds no line: {lines:?}");
}

/// **A STOPPED account is not a halted venue.** A stopped account that was not started again keeps its last
/// block (its position stays valued), and if that was `Halted` it would read as a halt for ever; the frame's
/// `not_running` list (a stop adds the account BEFORE the engine goes) says it is gone. An account that
/// started again has a newer, Active block and no `not_running` entry, so it is unaffected.
#[test]
fn a_stopped_account_is_not_named_halted_or_reducing() {
    let mut s = snap(TradingState::Active, None);
    s.venues = vec![
        block("ibkr", TradingState::Halted),
        block("bybit#ALT", TradingState::Halted),
        block("okx#OLD", TradingState::Reducing),
    ];
    s.not_running = vec![
        ArmingNotRunningViewWire {
            venue: "bybit".into(),
            account: Some("ALT".into()),
            route_key: "bybit#ALT".into(),
            tier: None,
            reason: "account.active = 0".into(),
            detail: None,
            block_word: None,
        },
        ArmingNotRunningViewWire {
            venue: "okx".into(),
            account: Some("OLD".into()),
            route_key: "okx#OLD".into(),
            tier: None,
            reason: "account.active = 0".into(),
            detail: None,
            block_word: None,
        },
    ];
    assert_eq!(venue_state_lines(&s), vec!["halted venues: ibkr"]);
    let status = two_mounts();
    let body = json_body(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["halted_venues"], serde_json::json!(["ibkr"]));
    assert_eq!(v["reducing_venues"], serde_json::json!([]));
}

/// The JSON gains `halted_venues` / `reducing_venues` arrays (always present beside
/// `trading_state`); every existing key is unchanged.
#[test]
fn json_carries_the_per_venue_states_beside_the_primarys() {
    let mut s = snap(TradingState::Halted, None);
    s.venues = vec![block("binance", TradingState::Halted), block("okx", TradingState::Active)];
    let status = two_mounts();
    let body = json_body(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["trading_state"], "Halted");
    assert_eq!(v["halted_venues"], serde_json::json!(["binance"]));
    assert_eq!(v["reducing_venues"], serde_json::json!([]));
}

/// **A held balance is named under the mode, in the shared words, and a node that reports none adds nothing**:
/// the human output gets one `<route key>: balance held: <reason> - …` line per account right after the venue
/// lines (so the registry rows keep their order), `--json` carries the list only when it is not empty.
#[test]
fn a_held_balance_is_named_under_the_mode_and_only_when_the_node_reports_one() {
    let status = two_mounts();
    let mut s = snap(TradingState::Active, None);
    let plain = human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    assert!(plain.iter().all(|l| !l.contains("balance held")), "{plain:?}");
    let body = json_body(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v.get("balance_withheld").is_none(), "nothing reported: no key");

    s.balance_withheld = vec![vike_tradehub_client::wire::BalanceWithheldWire {
        route_key: "oanda".into(),
        reason: "assumed USD".into(),
    }];
    assert_eq!(balance_held_lines(&s).len(), 1);
    let lines = human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    assert_eq!(lines[0], "trading state: Active", "the top line keeps its wording");
    assert!(lines[1].starts_with("oanda: balance held: assumed USD - "), "{lines:?}");
    assert!(lines[2].starts_with("node hub-a"), "the registry follows: {lines:?}");
    let body = json_body(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["balance_withheld"],
        serde_json::json!([{ "route_key": "oanda", "reason": "assumed USD" }])
    );
}

/// **Pinned on the code from before the link lines: a healthy node's answer is what it was**: the same lines, and the
/// same JSON keys (a list with nothing on it is not a key), and `running_tiers`, the one key added since.
#[test]
fn pinned_a_healthy_answer_is_what_it_was() {
    let mut s = snap(TradingState::Active, None);
    s.venues = vec![VenueBlockWire {
        tier: Some("paper".to_string()),
        ..block("polymarket", TradingState::Active)
    }];
    let status = two_mounts();
    let answer = Answer { snap: Ok(&s), status: Ok(&status), knows_class: true };
    assert_eq!(
        human_lines(&answer),
        [
            "trading state: Active",
            "node hub-a — paper — build vike-tradehub 0.1.0 (abc1234, clean)",
            "params: spread=0.01 size=20",
            "",
            "  STRATEGY      MODE   PRODUCT  PARAMS",
            "  spread_maker  paper  —        venue=polymarket symbol=TOK-A spread=0.01",
            "  np            LIVE   —        venue=polymarket symbol=TOK-B edge=0.02",
        ]
    );
    let v: serde_json::Value = serde_json::from_str(&json_body(&answer)).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "fault",
            "halted_venues",
            "reducing_venues",
            "running_tiers",
            "strategy_status",
            "trading_state"
        ]
    );
    assert_eq!(v["running_tiers"], "paper");
    assert_eq!(v["strategy_status"]["identity"]["live"], false, "the wire's field stays as it is");
}

/// **What stands between the node and the venues is printed, beneath the mode and above the registry, and the JSON
/// carries the frame's own lists**: the `not_running` and `venue_link_down` lines this verb used to omit, a lost trade
/// link and a lost price link. A lease closed for good reads as that, never as running.
#[test]
fn what_stands_between_the_node_and_the_venues_is_printed_and_in_the_json() {
    let mut s = snap(TradingState::Active, None);
    s.not_running = vec![ArmingNotRunningViewWire {
        venue: "ibkr".into(),
        account: None,
        route_key: "ibkr".into(),
        tier: Some("demo".into()),
        reason: "adapter failed".into(),
        detail: Some("held by another client".into()),
        block_word: None,
    }];
    s.venue_link_down = vec![VenueLinkDownWire {
        route_key: "dukascopy".into(),
        why: "JForex session disconnected".into(),
        failed_restarts: 0,
        retry_in_ms: 0,
        effect: String::new(),
        no_adapter: false,
    }];
    s.trade_link_down = vec![
        TradeLinkDownWire {
            route_key: "binance".into(),
            why: "the gatewayhub is stopping".into(),
            down_ms: 65_000,
            closed: false,
        },
        TradeLinkDownWire {
            route_key: "bybit#ALT".into(),
            why: "taken over by key-1".into(),
            down_ms: 1_000,
            closed: true,
        },
    ];
    s.price_link_down = vec![PriceLinkDownWire {
        venue: "okx".into(),
        why: "connection refused".into(),
        down_ms: 3_000,
        of: 2,
    }];
    let status = two_mounts();
    let answer = Answer { snap: Ok(&s), status: Ok(&status), knows_class: true };
    let lines = human_lines(&answer);
    assert_eq!(lines[0], "trading state: Active");
    assert_eq!(lines[1], "ibkr: NOT RUNNING (demo): adapter failed: held by another client");
    assert_eq!(
        lines[2],
        "dukascopy: the venue path is DOWN: JForex session disconnected; orders are refused until its bridge \
         has it back"
    );
    assert_eq!(
        lines[3],
        "binance: its trade link to the gatewayhub is DOWN for 1m 5s (the gatewayhub is stopping), redialling: \
         no order of this account reaches the venue until it is back"
    );
    assert!(lines[4].starts_with(
        "bybit#ALT: its trade lease to the gatewayhub is CLOSED FOR GOOD (taken over"
    ));
    assert!(!lines[4].contains("RUNNING"), "a closed lease does not read running: {}", lines[4]);
    assert_eq!(
        lines[5],
        "okx: its price feed's lane to the datahub is DOWN for 3s (connection refused): this venue's strategies \
         hear no quote, tick or bar"
    );
    assert!(lines[6].starts_with("node hub-a"), "the registry follows: {lines:?}");

    let v: serde_json::Value = serde_json::from_str(&json_body(&answer)).unwrap();
    assert_eq!(v["not_running"][0]["detail"], "held by another client");
    assert_eq!(v["venue_link_down"][0]["route_key"], "dukascopy");
    assert_eq!(v["trade_link_down"][1]["closed"], true);
    assert_eq!(v["price_link_down"][0]["of"], 2);
}

/// **The session's `status` and `snapshot` block lists what the one-shot `trade status` lists**, beneath the mode and
/// the `[seq N]` stamp, in the same words.
#[test]
fn the_session_state_block_lists_what_stands_between_the_node_and_the_venues() {
    let mut s = snap(TradingState::Active, None);
    s.trade_link_down = vec![TradeLinkDownWire {
        route_key: "binance".into(),
        why: "the gatewayhub is stopping".into(),
        down_ms: 65_000,
        closed: false,
    }];
    let lines = super::super::describe::state_lines(&s);
    assert!(lines[0].ends_with("trading state: Active"), "{lines:?}");
    assert!(lines[0].starts_with("[seq"), "the session's stamp: {lines:?}");
    assert_eq!(lines[1], link_lines(&s)[0], "{lines:?}");
    assert!(lines[1].starts_with("binance: its trade link to the gatewayhub is DOWN"), "{lines:?}");
    assert_eq!(lines.len(), 2, "{lines:?}");

    s.trade_link_down.clear();
    assert_eq!(super::super::describe::state_lines(&s).len(), 1, "nothing lost: the mode alone");
}

/// The two halves arrive in ONE output, mode first: an operator asking "what is this node
/// doing" reads the kill switch before the roster, not after it.
#[test]
fn the_human_output_carries_the_mode_above_the_registry() {
    let s = snap(TradingState::Reducing, None);
    let status = two_mounts();
    let lines = human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    assert_eq!(lines[0], "trading state: Reducing");
    assert!(lines[1].starts_with("node hub-a"), "{lines:?}");
    assert_eq!(lines[2], "params: spread=0.01 size=20");
}

/// A mode read that faulted AFTER the registry answered says why, and the registry still
/// renders — the alternative is a table whose first line is silently absent.
#[test]
fn an_unreadable_mode_says_so_and_does_not_suppress_the_registry() {
    let err = io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer");
    let status = two_mounts();
    let lines = human_lines(&Answer { snap: Err(&err), status: Ok(&status), knows_class: true });
    assert!(lines[0].contains("UNKNOWN"), "{lines:?}");
    assert!(lines[0].contains("reset by peer"), "{lines:?}");
    assert!(lines[1].starts_with("node hub-a"), "{lines:?}");
}

/// **The degrade against a node too old for `strategy-verbs`.** The registry half is a named
/// absence carrying the node's own reason; the MODE — the half that says whether trading is
/// halted — is printed exactly as it would be on any other node. Before ruling 17's correction
/// this rendering could not be reached at all: `run` returned before the mode was ever read.
#[test]
fn an_unreadable_registry_still_renders_the_mode() {
    let err = io::Error::new(io::ErrorKind::Unsupported, "no \"strategy-verbs\" capability");
    let s = snap(TradingState::Halted, None);
    let lines = human_lines(&Answer { snap: Ok(&s), status: Err(&err), knows_class: true });
    assert_eq!(lines[0], "trading state: Halted", "{lines:?}");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains("mounted strategies: UNAVAILABLE"), "{lines:?}");
    assert!(lines[1].contains("strategy-verbs"), "{lines:?}");
}

/// **A `seq` 0 frame is no reading of the kill switch.** It is the node's placeholder, which says `Active` and no
/// halted venue whatever the node's state is: the verb marks it, states no mode, and still prints what the frame does
/// list (a NOT RUNNING row is the node's own word). The JSON says `pre_fold` and leaves the placeholder's readings out.
#[test]
fn a_frame_with_nothing_built_is_marked_and_states_no_mode() {
    let status = two_mounts();
    let mut s = CoreSnapshotWire::empty();
    s.identity = Some(status.identity.clone());
    s.not_running = vec![ArmingNotRunningViewWire {
        venue: "ibkr".into(),
        account: None,
        route_key: "ibkr".into(),
        tier: Some("demo".into()),
        reason: "adapter failed".into(),
        detail: None,
        block_word: None,
    }];
    let answer = Answer { snap: Ok(&s), status: Ok(&status), knows_class: true };

    let lines = human_lines(&answer);
    assert_eq!(lines[0], crate::cmd::trade::pre_fold_line(&s).expect("seq 0 is marked"));
    assert_eq!(lines[1], "trading state: UNKNOWN — the node has built nothing yet");
    assert_eq!(lines[2], "ibkr: NOT RUNNING (demo): adapter failed");
    assert!(lines[3].starts_with("node hub-a"), "the registry follows: {lines:?}");

    let v: serde_json::Value = serde_json::from_str(&json_body(&answer)).unwrap();
    assert_eq!(v["pre_fold"], true, "{v}");
    for placeholder in ["trading_state", "fault", "halted_venues", "reducing_venues"] {
        assert!(v.get(placeholder).is_none(), "`{placeholder}` is a placeholder here: {v}");
    }
    assert_eq!(v["not_running"][0]["route_key"], "ibkr", "{v}");
    assert!(v.get("strategy_status").is_some(), "{v}");
}

/// The MODE half's stderr diagnostic names the verb, the node and the reason — the twin of
/// [`failure_lines`] for the other half, and the reason a `--json` run cannot fail in silence.
#[test]
fn a_failed_mode_read_has_a_stderr_line_naming_the_verb_and_the_node() {
    let err = io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer");
    let line = mode_failure_line("the CI box:9200", &err);
    assert!(line.contains("vike-cli trade status"), "{line}");
    assert!(line.contains("the CI box:9200"), "{line}");
    assert!(line.contains("reset by peer"), "{line}");
}

/// The two kinds a second connection can still answer under, and the two it cannot. ⚠ This is
/// a strict SUBSET of [`failure_exit`]'s "the node ANSWERED" set: `PermissionDenied` is a node
/// that answered and will answer the same way again, so it is a rung without being a retry.
#[test]
fn the_mode_is_re_asked_only_where_a_second_connection_could_answer() {
    for kind in [io::ErrorKind::Unsupported, io::ErrorKind::InvalidData] {
        let err = io::Error::new(kind, "the node answered");
        assert!(mode_read_worth_attempting(&err), "{kind:?} leaves a usable connection");
    }
    for kind in
        [io::ErrorKind::PermissionDenied, io::ErrorKind::ConnectionRefused, io::ErrorKind::TimedOut]
    {
        let err = io::Error::new(kind, "no");
        assert!(!mode_read_worth_attempting(&err), "{kind:?} would fail identically");
    }
}

// ---- the registry table ----

#[test]
fn the_identity_line_carries_name_mode_and_build() {
    let lines = registry_lines(&two_mounts(), false, None);
    assert_eq!(lines[0], "node hub-a — paper, LIVE — build vike-tradehub 0.1.0 (abc1234, clean)");
    assert_eq!(lines[1], "params: spread=0.01 size=20");
}

/// ⚠ **The `false` is the point: against a node that does not advertise `FEATURE_MOUNT_CLASS`
/// this table is BYTE-IDENTICAL to what it has always been.** The assertions below are the
/// pre-PRODUCT ones, unchanged, so a client talking to an older daemon cannot have its output
/// shifted by a column that daemon knows nothing about.
#[test]
fn one_aligned_row_per_mount_with_live_uppercased() {
    let lines = registry_lines(&two_mounts(), false, None);
    // Header + one row per mount, columns aligned on the longest strategy name.
    assert_eq!(lines[3], "  STRATEGY      MODE   PARAMS");
    assert_eq!(lines[4], "  spread_maker  paper  venue=polymarket symbol=TOK-A spread=0.01");
    assert_eq!(lines[5], "  np            LIVE   venue=polymarket symbol=TOK-B edge=0.02");
    assert_eq!(lines.len(), 6, "{lines:?}");
}

/// The PRODUCT column, and the THREE-WAY distinction that is the whole reason it is gated on a
/// capability rather than on the field.
///
/// One mount names its class and one does not, on a node that DOES advertise the capability —
/// so the em dash here means "not migrated yet", which is a fact about the deployment. The test
/// above covers the third state: an old node, where the column is absent entirely because the
/// client cannot tell that apart from the second.
#[test]
fn the_product_column_shows_a_class_and_says_which_mount_has_none() {
    let mut status = two_mounts();
    status.mounts[0].asset_class = Some("PredictionMarket".to_string());
    let lines = registry_lines(&status, true, None);
    assert_eq!(lines[3], "  STRATEGY      MODE   PRODUCT           PARAMS");
    assert_eq!(
        lines[4],
        "  spread_maker  paper  PredictionMarket  venue=polymarket symbol=TOK-A spread=0.01"
    );
    assert_eq!(
        lines[5],
        "  np            LIVE   —                 venue=polymarket symbol=TOK-B edge=0.02",
        "an unmigrated mount reads as an em dash, not a blank — a blank reads as a rendering \
             slip and this absence is a FACT"
    );
}

/// ⚠ The column is padded by CHARACTER COUNT, not by byte length, and the em dash is the case
/// that tells them apart: it is three bytes and one column. A `String::len()`-based width would
/// shear the params column by two spaces on exactly the rows this feature exists to show.
#[test]
fn the_em_dash_is_padded_as_one_column_not_three_bytes() {
    let mut status = two_mounts();
    status.mounts[0].asset_class = Some("Etf".to_string());
    let lines = registry_lines(&status, true, None);
    // ⚠ CHARACTER offset, not `find`'s BYTE offset — and getting that wrong is the exact trap
    // this test is named for. `"Etf"` is three bytes and three columns; `"—"` is three bytes and
    // ONE. A byte comparison therefore reports a correctly aligned table as sheared, which is
    // what the first draft of this assertion did.
    let params_col = |l: &str| {
        let at = l.find("venue=").expect("every row carries a params cell");
        l[..at].chars().count()
    };
    assert_eq!(
        params_col(&lines[4]),
        params_col(&lines[5]),
        "the params column is sheared between an ASCII class and an em dash: {lines:?}"
    );
}

/// **The identity line says what RUNS, whatever `flags.tradehub_live` says** (decision 0156): that switch is a stop, so
/// a node with only demo accounts read `LIVE` with it on, and one with no paper account read `paper` with it off.
#[test]
fn the_identity_line_says_what_runs_whatever_the_switch_says() {
    type Case<'a> = (&'a [(&'a str, Option<&'a str>)], &'a str);
    let cases: [Case<'_>; 6] = [
        (&[("bybit#DEMO", Some("demo"))], "demo"),
        (&[("bybit#LIVE", Some("live"))], "LIVE"),
        (&[("bybit#PAPER", Some("paper"))], "paper"),
        (
            &[("bybit#LIVE", Some("live")), ("bybit#DEMO", Some("demo")), ("okx", Some("paper"))],
            "paper, demo, LIVE",
        ),
        (&[("bybit#DEMO", Some("demo")), ("okx", None)], "demo, tier not stated"),
        (&[], "no account running"),
    ];
    for switch in [false, true] {
        let mut status = two_mounts();
        status.identity.live = switch;
        for (blocks, word) in cases {
            let mut s = snap(TradingState::Active, None);
            s.venues = blocks
                .iter()
                .map(|(key, tier)| VenueBlockWire {
                    tier: tier.map(str::to_string),
                    ..block(key, TradingState::Active)
                })
                .collect();
            let lines =
                human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
            assert_eq!(
                lines[1],
                format!("node hub-a — {word} — build vike-tradehub 0.1.0 (abc1234, clean)"),
                "the switch at {switch}"
            );
        }
    }
}

// ---- the JSON shape ----

/// The `--json` body nests the WIRE payload verbatim: `strategy_status` round-trips back into
/// the wire struct, and the top-level keys beside it are the mode half — so a consumer scripts
/// against the one schema that cannot drift from the node's.
#[test]
fn json_nests_the_wire_payload_verbatim_beside_the_mode() {
    let status = two_mounts();
    let s = snap(TradingState::Halted, None);
    let body = json_body(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });

    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["trading_state"], "Halted");
    assert_eq!(v["fault"], serde_json::Value::Null);
    let back: StrategyStatusWire = serde_json::from_value(v["strategy_status"].clone()).unwrap();
    assert_eq!(back, status);
    assert_eq!(v["strategy_status"]["identity"]["name"], "hub-a");
    assert_eq!(v["strategy_status"]["effective_params"], "spread=0.01 size=20");
    assert_eq!(v["strategy_status"]["mounts"].as_array().unwrap().len(), 2);
    assert_eq!(v["strategy_status"]["mounts"][1]["live"], true);
}

/// **A mode that could not be read is an `*_error` KEY, never an absence.** No `trading_state`
/// is invented, and `trading_state_error` is what stops the document reading as "this node has
/// no mode" — the shape a `jq .trading_state` (and a human) cannot tell from an omission.
#[test]
fn json_names_the_reason_when_the_mode_could_not_be_read() {
    let status = two_mounts();
    let err = io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer");
    let v: serde_json::Value = serde_json::from_str(&json_body(&Answer {
        snap: Err(&err),
        status: Ok(&status),
        knows_class: true,
    }))
    .unwrap();
    assert!(v.get("trading_state").is_none(), "no mode may be invented: {v}");
    assert!(v.get("fault").is_none(), "{v}");
    assert!(
        v["trading_state_error"].as_str().unwrap_or_default().contains("reset by peer"),
        "the failed read must be VISIBLE and carry its reason: {v}"
    );
    assert!(v.get("strategy_status").is_some(), "{v}");
}

/// …and the twin, for the half a too-old node cannot answer: the registry becomes
/// `strategy_status_error` and the MODE still rides beside it, which is the whole of the
/// honest degrade.
#[test]
fn json_names_the_reason_when_the_registry_could_not_be_read() {
    let err = io::Error::new(io::ErrorKind::Unsupported, "no \"strategy-verbs\" capability");
    let s = snap(TradingState::Halted, None);
    let v: serde_json::Value = serde_json::from_str(&json_body(&Answer {
        snap: Ok(&s),
        status: Err(&err),
        knows_class: true,
    }))
    .unwrap();
    assert_eq!(v["trading_state"], "Halted", "{v}");
    assert!(v.get("strategy_status").is_none(), "no registry may be invented: {v}");
    assert!(
        v["strategy_status_error"].as_str().unwrap_or_default().contains("strategy-verbs"),
        "{v}"
    );
}

// ---- the failure mapping ----

/// The old-node refusal: the client's own sentence (which names the missing capability and
/// that nothing was sent) survives verbatim, and the added line is the ACTION — upgrade that
/// node. Pinned here because provoking it for real needs a downgraded daemon.
#[test]
fn an_unsupported_node_gets_the_upgrade_instruction() {
    let err = io::Error::new(
        io::ErrorKind::Unsupported,
        "this node does not advertise the \"strategy-verbs\" capability (an older \
             vike-tradehub) — StrategyStatus refused client-side, nothing was sent",
    );
    let lines = failure_lines("the CI box:9200", &err);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("strategy-verbs"), "{}", lines[0]);
    assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
    // Whole, because `trade venue` shares the function with its own `serves` words.
    assert_eq!(
        lines[1],
        "upgrade the node at the CI box:9200 to a vike-tradehub build that serves the strategy verbs, \
         then re-run"
    );
}

#[test]
fn an_auth_refusal_points_at_the_observe_key_not_the_verb() {
    let err = io::Error::new(io::ErrorKind::PermissionDenied, "auth denied: bad mac");
    let lines = failure_lines("the CI box:9200", &err);
    assert!(lines[0].contains("refused the observe handshake"), "{}", lines[0]);
    assert!(lines[1].contains(OBSERVE_KEY_ENV), "{}", lines[1]);
}

#[test]
fn a_transport_fault_names_the_address() {
    let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
    let lines = failure_lines("<host>:9200", &err);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("<host>:9200"), "{}", lines[0]);
}

/// Every diagnostic names the verb the operator typed — two words, from the one constant, so a
/// message can never point at the retired top-level `strategy-status` spelling.
#[test]
fn every_failure_names_the_verb_as_the_operator_typed_it() {
    for kind in [io::ErrorKind::Unsupported, io::ErrorKind::PermissionDenied] {
        let err = io::Error::new(kind, "x");
        assert!(failure_lines("n:1", &err)[0].contains("vike-cli trade status"), "{kind:?}");
    }
    let err = io::Error::new(io::ErrorKind::ConnectionRefused, "x");
    assert!(failure_lines("n:1", &err)[0].contains("vike-cli trade status"));
}

/// The RUNG half of the same match, pinned kind by kind: **the node ANSWERED** ⇒ the
/// pre-existing rung, and only a socket that never got an answer is the retry rung.
///
/// ⚠ `InvalidData` is here because it is the arm the catch-all used to swallow.
/// `vike_tradehub_client::strategy_status` returns it for a node-side `Response::Error` (a
/// publisher with no identity block answers exactly that) and for an unexpected response kind —
/// both from a box that connected and passed the handshake. Read as a connect failure, a
/// permanent configuration fact told a wrapper to back off and retry forever.
#[test]
fn the_rung_follows_whether_the_node_answered() {
    crate::cmd::assert_the_rung_follows_whether_the_peer_answered(
        failure_exit,
        &[io::ErrorKind::Unsupported, io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData],
        "the node",
    );
}
