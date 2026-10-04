use super::*;

fn probe(features: &[&str]) -> Probe {
    Probe {
        addr: "127.0.0.1:7880".to_string(),
        proto_version: vike_datahub_client::PROTO_VERSION,
        features: features.iter().map(|s| (*s).to_string()).collect(),
        requires_auth: features.contains(&vike_datahub_client::FEATURE_AUTH),
        authenticated_as: None,
        ping_error: None,
    }
}

fn json_of(probe: &Probe) -> serde_json::Value {
    serde_json::from_str(&probe_json(probe)).expect("the document is valid JSON")
}

/// The two sentinels classify the two planes, a pre-split daemon is NAMED rather than
/// collapsed, and a daemon advertising neither is `unknown` rather than guessed at.
#[test]
fn the_plane_is_read_off_each_daemons_unconditional_capability() {
    assert_eq!(probe(&["backtest", "list_strategies"]).plane(), "compute");
    assert_eq!(probe(&["load_bars", "inventory"]).plane(), "data");
    assert_eq!(probe(&["backtest", "load_bars"]).plane(), "compute+data (a pre-split daemon)");
    assert_eq!(probe(&[]).plane(), "unknown");
    assert_eq!(probe(&["something_else"]).plane(), "unknown");
}

/// The Studio answer is the MOUNT sentinel and nothing else — a compute daemon with no runner
/// table must read "not mounted" even though it serves every profile-shaped verb.
#[test]
fn the_studio_table_is_mounted_only_when_its_own_capability_is_advertised() {
    assert!(!probe(&["backtest", "run_walkforward_profile"]).studio_mounted());
    assert!(probe(&["backtest", "run_slice", "run_sweep", "run_walkforward"]).studio_mounted());
}

/// ⚠ Capability matching is WHOLE-STRING, and this is the assertion that keeps it so: a
/// `contains`-shaped check would let `run_slice_v2` satisfy `run_slice`, and the per-venue
/// `md_venue=` entries exist only because every check in this protocol family is equality.
#[test]
fn a_capability_is_matched_whole_and_never_as_a_substring() {
    assert!(!probe(&["run_slice_v2"]).studio_mounted());
    assert!(!probe(&["md_venue=backtest"]).serves(COMPUTE_SENTINEL));
    assert!(probe(&["run_slice"]).studio_mounted());
}

/// **`requires_auth` and `authenticated_as` are different facts**, and the report may not
/// collapse them: keys held locally against a key-less server is the configuration an operator
/// most needs to SEE, because every verb is being served to whoever reaches the socket.
#[test]
fn the_auth_line_separates_what_the_server_demands_from_what_this_connection_got() {
    let keyless = probe(&["backtest"]);
    assert!(keyless.auth_line().contains("none"), "{}", keyless.auth_line());
    assert!(
        keyless.auth_line().contains("DeleteSeries"),
        "a key-less posture must say what it MEANS: {}",
        keyless.auth_line()
    );

    let mut keyed = probe(&["backtest", vike_datahub_client::FEATURE_AUTH]);
    keyed.authenticated_as = Some(Scope::Read);
    assert!(keyed.auth_line().contains("REQUIRED"), "{}", keyed.auth_line());
    assert!(keyed.auth_line().contains("observe"), "{}", keyed.auth_line());

    // …and the pair that should not happen is RENDERED rather than panicked on.
    let mut odd = probe(&["backtest"]);
    odd.authenticated_as = Some(Scope::Write);
    assert!(odd.auth_line().contains("control"), "{}", odd.auth_line());
}

/// Every question renders a row, in the table's own order, and each row names the capability it
/// was read from — so a reader can check a label against the verbatim `features` line below it.
#[test]
fn every_question_renders_a_row_naming_the_capability_it_read() {
    let text = report_lines(&probe(&["backtest", "run_slice"])).join("\n");
    for &(question, capability) in QUESTIONS {
        assert!(text.contains(question), "the report omits {question}:\n{text}");
        assert!(text.contains(capability), "the report omits {capability}:\n{text}");
    }
    assert!(text.contains("features:"), "the verbatim list is the check on every row: {text}");
}

/// The UNANSWERED question is named in both renderings, and the JSON carries `store_root` as an
/// explicit `null` BESIDE the name — see [`probe_json`] for why an absent key and a bare null
/// are both wrong.
#[test]
fn the_store_root_is_reported_as_unanswered_and_never_guessed() {
    let p = probe(&["backtest"]);
    let text = report_lines(&p).join("\n");
    assert!(text.contains("unanswered: store_root"), "{text}");

    let doc = json_of(&p);
    assert_eq!(doc["store_root"], serde_json::Value::Null);
    assert_eq!(doc["unanswered"], serde_json::json!(["store_root"]));
}

/// The document carries the address it dialled and the negotiated version — "which daemon
/// answered" is half the answer once more than one exists, and the ladder's default rung is
/// invisible in the argv.
#[test]
fn the_json_document_names_the_address_the_version_and_the_plane() {
    let mut p = probe(&["backtest", "run_slice", vike_datahub_client::FEATURE_AUTH]);
    p.authenticated_as = Some(Scope::Write);
    let doc = json_of(&p);
    assert_eq!(doc["addr"], serde_json::json!("127.0.0.1:7880"));
    assert_eq!(doc["proto_version"], serde_json::json!(vike_datahub_client::PROTO_VERSION));
    assert_eq!(doc["plane"], serde_json::json!("compute"));
    assert_eq!(doc["studio_runners"], serde_json::json!(true));
    assert_eq!(doc["auth_required"], serde_json::json!(true));
    assert_eq!(doc["authenticated_as"], serde_json::json!("control"));
    assert_eq!(doc["ping"], serde_json::json!("ok"));
    assert_eq!(doc["features"].as_array().map(Vec::len), Some(3));
}

/// A key-less server's document says `null` for the scope rather than inventing one, and a
/// failed Ping travels as its MESSAGE rather than as a boolean.
#[test]
fn a_keyless_server_and_a_failed_ping_both_travel_honestly() {
    let mut p = probe(&["load_bars"]);
    p.ping_error = Some("timed out".to_string());
    let doc = json_of(&p);
    assert_eq!(doc["authenticated_as"], serde_json::Value::Null);
    assert_eq!(doc["auth_required"], serde_json::json!(false));
    assert_eq!(doc["ping"], serde_json::json!("timed out"));
    assert!(report_lines(&p).iter().any(|l| l.contains("Ping did NOT answer")), "{p:?}");
}

/// The per-venue market-data entries are DECODED through the protocol's own helper rather than
/// by a second prefix parser here, and they are omitted entirely when none was advertised — an
/// empty row would read as "this daemon serves market data for nothing".
#[test]
fn advertised_market_data_venues_are_decoded_by_the_protocols_own_helper() {
    let p = probe(&[
        "load_bars",
        vike_datahub_client::FEATURE_MARKET_DATA,
        "md_venue=binance",
        "md_venue=bybit",
    ]);
    let text = report_lines(&p).join("\n");
    assert!(text.contains("md venues:") && text.contains("binance"), "{text}");
    assert_eq!(json_of(&p)["md_venues"], serde_json::json!(["binance", "bybit"]));

    let none = probe(&["load_bars"]);
    assert!(!report_lines(&none).iter().any(|l| l.contains("md venues")), "{none:?}");
    assert_eq!(json_of(&none)["md_venues"], serde_json::json!([]));
}

/// **A REFUSED-BY-POLICY answer and an UNREACHABLE socket never share a rung**, which is the
/// one property a wrapper branches on: rung 3 promises "the same invocation may well work
/// later" and a keyed daemon this box has no key for will not.
#[test]
fn a_keyed_refusal_and_an_unreachable_socket_land_on_different_rungs() {
    use crate::exit::Exit;
    use std::io::{Error, ErrorKind};

    let keyed = Error::new(ErrorKind::PermissionDenied, "keyed");
    let refused = classify_dial_failure("1.2.3.4:9", &keyed);
    assert_eq!(refused.exit, Exit::Failed);
    let msg = refused.msg;
    assert!(msg.contains("ANSWERED"), "{msg}");
    assert!(msg.contains("vike-cli datahub setup"), "it names the minter: {msg}");
    assert!(msg.contains("firewall"), "the residual is declared where it bites: {msg}");

    let dead = Error::new(ErrorKind::ConnectionRefused, "nothing there");
    let unreachable = classify_dial_failure("1.2.3.4:9", &dead);
    assert_eq!(unreachable.exit, Exit::Connect);
    let msg = unreachable.msg;
    assert!(msg.contains("1.2.3.4:9"), "{msg}");
    assert!(msg.contains("vike-backend"), "it names what to start: {msg}");
}
