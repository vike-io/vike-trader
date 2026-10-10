use super::*;
use crate::cmd::args::HELP_SENTINEL;
// `OBSERVE_KEY_ENV` itself is no longer read by this file's PRODUCTION code — the
// `PermissionDenied` arm delegates to `crate::cmd::nodekeys::observe_permission_denied_lines`,
// which owns that reference now. Only this test still names it, to assert the shared line
// mentions the right variable.
use crate::cmd::nodekeys::OBSERVE_KEY_ENV;
use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity};

fn parsed(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()))
}

/// A two-mount status, one live row and one paper — every render property below is visible in
/// one fixture: identity vs mount params, the LIVE/paper casing, and column alignment across
/// names of different lengths.
fn two_mounts() -> WireStrategyStatus {
    WireStrategyStatus {
        identity: WireNodeIdentity {
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
            WireMountRow {
                strategy: "spread_maker".to_string(),
                params: "venue=polymarket symbol=TOK-A spread=0.01".to_string(),
                live: false,
                venue: String::new(),
                symbol: String::new(),
                interval: String::new(),
                mount_id: String::new(),
                typed_params: None,
                asset_class: None,
            },
            WireMountRow {
                strategy: "np".to_string(),
                params: "venue=polymarket symbol=TOK-B edge=0.02".to_string(),
                live: true,
                venue: String::new(),
                symbol: String::new(),
                interval: String::new(),
                mount_id: String::new(),
                typed_params: None,
                asset_class: None,
            },
        ],
    }
}

/// A snapshot carrying just the two fields this command reads off one.
fn snap(state: WireTradingState, fault: Option<&str>) -> WireSnapshot {
    let mut s = WireSnapshot::empty();
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
    assert_eq!(err, "--json takes no value");
}

#[test]
fn help_short_circuits_even_without_a_node() {
    for spelling in ["-h", "--help"] {
        assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
    }
}

#[test]
fn an_unknown_argument_is_rejected_by_name() {
    let err = parsed(&["--node", "n:1", "--verbose"]).unwrap_err();
    assert!(err.contains("--verbose"), "{err}");
}

/// ⚠ **`state` may not come back as an argument of this verb.** The whole of ruling 17 is that
/// nothing which CHANGES the daemon's mode may be spelled by adding a word to something that
/// does not, so `trade status halted` must be a usage error rather than a halt — and it is,
/// because the grammar takes flags only and a bare word is an unknown argument by name.
#[test]
fn a_bare_mode_word_is_a_usage_error_and_never_a_write() {
    for word in ["halted", "reducing", "active", "state"] {
        let err = parsed(&["--node", "n:1", word]).unwrap_err();
        assert!(err.contains(word), "{word}: {err}");
    }
}

// ---- the mode half ----

#[test]
fn the_mode_line_names_the_state_and_a_fault_is_its_own_line() {
    assert_eq!(mode_lines(WireTradingState::Halted, None), vec!["trading state: Halted"]);
    let faulted = mode_lines(WireTradingState::Active, Some("fold panic in engine 0"));
    assert_eq!(faulted.len(), 2, "{faulted:?}");
    assert!(faulted[1].contains("FAULT"), "{faulted:?}");
    assert!(faulted[1].contains("fold panic in engine 0"), "{faulted:?}");
}

/// The two halves arrive in ONE output, mode first: an operator asking "what is this node
/// doing" reads the kill switch before the roster, not after it.
#[test]
fn the_human_output_carries_the_mode_above_the_registry() {
    let s = snap(WireTradingState::Reducing, None);
    let status = two_mounts();
    let lines = human_lines(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });
    assert_eq!(lines[0], "trading state: Reducing");
    assert_eq!(lines[1], "node hub-a — paper — build vike-tradehub 0.1.0 (abc1234, clean)");
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
    let s = snap(WireTradingState::Halted, None);
    let lines = human_lines(&Answer { snap: Ok(&s), status: Err(&err), knows_class: true });
    assert_eq!(lines[0], "trading state: Halted", "{lines:?}");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].contains("mounted strategies: UNAVAILABLE"), "{lines:?}");
    assert!(lines[1].contains("strategy-verbs"), "{lines:?}");
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
    let lines = registry_lines(&two_mounts(), false);
    assert_eq!(lines[0], "node hub-a — paper — build vike-tradehub 0.1.0 (abc1234, clean)");
    assert_eq!(lines[1], "params: spread=0.01 size=20");
}

/// ⚠ **The `false` is the point: against a node that does not advertise `FEATURE_MOUNT_CLASS`
/// this table is BYTE-IDENTICAL to what it has always been.** The assertions below are the
/// pre-PRODUCT ones, unchanged, so a client talking to an older daemon cannot have its output
/// shifted by a column that daemon knows nothing about.
#[test]
fn one_aligned_row_per_mount_with_live_uppercased() {
    let lines = registry_lines(&two_mounts(), false);
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
    let lines = registry_lines(&status, true);
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
    let lines = registry_lines(&status, true);
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

#[test]
fn a_live_daemon_shouts_on_the_identity_line() {
    let mut status = two_mounts();
    status.identity.live = true;
    assert!(registry_lines(&status, false)[0].contains("— LIVE —"));
}

// ---- the JSON shape ----

/// The `--json` body nests the WIRE payload verbatim: `strategy_status` round-trips back into
/// the wire struct, and the top-level keys beside it are the mode half — so a consumer scripts
/// against the one schema that cannot drift from the node's.
#[test]
fn json_nests_the_wire_payload_verbatim_beside_the_mode() {
    let status = two_mounts();
    let s = snap(WireTradingState::Halted, None);
    let body = json_body(&Answer { snap: Ok(&s), status: Ok(&status), knows_class: true });

    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["trading_state"], "Halted");
    assert_eq!(v["fault"], serde_json::Value::Null);
    let back: WireStrategyStatus = serde_json::from_value(v["strategy_status"].clone()).unwrap();
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
    let s = snap(WireTradingState::Halted, None);
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
    assert!(lines[1].contains("upgrade the node at the CI box:9200"), "{}", lines[1]);
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
    let kinds_that_answered =
        [io::ErrorKind::Unsupported, io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData];
    for kind in kinds_that_answered {
        let err = io::Error::new(kind, "the node said something");
        assert_eq!(failure_exit(&err), Exit::Failed, "{kind:?} is a node that ANSWERED");
    }
    for kind in [
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::TimedOut,
        io::ErrorKind::ConnectionAborted,
    ] {
        let err = io::Error::new(kind, "no answer");
        assert_eq!(failure_exit(&err), Exit::Connect, "{kind:?} never reached a node");
    }
}
