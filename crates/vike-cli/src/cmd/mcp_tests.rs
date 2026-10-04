/// ⚠ **COMPOSED, never spelled.** `crates/vike-config/tests/policy_is_consumed.rs` scans every
/// non-comment line under `src/` for a section-qualified policy key and reads a hit as evidence
/// that the file READS that setting — and unlike its sibling `settings_are_consumed.rs` it does
/// NOT truncate at `#[cfg(test)]`, so a fixture spelling one turns `main` red. It did: #1685
/// lifted these fixtures here from `mcp.rs` and `Policy::max_leverage` is `Consumed::No`.
/// Composing it is the same trick `crates/vike-model/src/credential_keys.rs` uses to keep its
/// near-miss fixtures out of the settings-registry literal harvest.
const POLICY_KEY_FIXTURE: &str = concat!("policy.", "max_leverage");

use super::backtest_tools::search_from_args;
use super::node_reads::{is_node_frame, node_read_answer, node_read_failure};
use super::node_writes::{CHECKED_BY_NODE, CHECKED_BY_NONE, control_rejection};
use super::venue_gate::{
    VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE, VENUE_CHECK_UNVERIFIED, commanded_venue, venue_verdict,
};
use super::*;
use crate::cmd::nodekeys;
use vike_tradehub_client::ControlRejected;
use vike_tradehub_client::wire::WireSnapshot;

/// A minimal DELETE intent — this test only needs *an* intent to hang a token on; what is
/// under test is the EPOCH the token carries, not the intent.
fn any_intent() -> PreviewIntent {
    PreviewIntent::Delete(DeleteIntent {
        kind: "bar".to_string(),
        venue: "binance".to_string(),
        symbol: None,
        group: None,
        interval: None,
        produced_by: "test".to_string(),
    })
}

/// **A preview STAMPS the account epoch it saw**, which is what a confirm compares against. The
/// three checks that already guarded a confirm ask *is this token ours*, *is it still warm*,
/// and *was it issued for this command* — none of them asks whether the NODE is still the node
/// that was previewed.
#[test]
fn a_preview_stamps_the_account_epoch_it_saw() {
    let mut store = PendingPreviews::default();
    let token = store.issue(any_intent(), Some(7));
    let taken = store.take(&token).expect("the token was just issued");
    assert_eq!(
        taken.accounts_epoch,
        Some(7),
        "the epoch at preview time is what a confirm compares"
    );
}

/// ⚠ **The complement, and it is what keeps the refusal off every older node:** a node that
/// reports no epoch stamps `Some(0)`, and `Some(0) == Some(0)` compares EQUAL. A node that
/// cannot report an account set cannot have moved one either, so refusing on that would break
/// every confirm against such a node while proving nothing.
#[test]
fn a_node_that_reports_no_epoch_stamps_zero() {
    let mut store = PendingPreviews::default();
    let token = store.issue(any_intent(), Some(0));
    let taken = store.take(&token).expect("the token was just issued");
    assert_eq!(
        taken.accounts_epoch,
        Some(0),
        "two answered zeroes are the pre-field behaviour exactly"
    );
}

/// ⚠ **A node that could not be ASKED stamps `None`, which is a THIRD state and not the
/// answered zero above.** This is the distinction the guard turns on: an unreadable epoch used
/// to be folded into `0`, so a dropped read connection read as an account set that had moved
/// to zero — and the resulting refusal told the agent an account "may have been mounted,
/// unmounted, or relabelled" when the truth was that the link died.
///
/// The assertion is deliberately `ne` against `Some(0)` rather than `eq` against `None`: what
/// must hold is that the two are DISTINGUISHABLE, which is the property a future `unwrap_or`
/// creeping back would destroy while an `eq` on `None` alone still passed.
#[test]
fn a_node_that_could_not_be_asked_stamps_a_state_of_its_own() {
    let mut store = PendingPreviews::default();
    let unreadable = store.issue(any_intent(), None);
    let answered_zero = store.issue(any_intent(), Some(0));
    assert_ne!(
        store.take(&unreadable).expect("issued").accounts_epoch,
        store.take(&answered_zero).expect("issued").accounts_epoch,
        "\"could not ask\" and \"the node answered 0\" must not be the same value — folding \
             them is what let a lost link be reported as a changed account set"
    );
}

/// Two previews taken across a CHANGED set carry different stamps — the property the refusal
/// reads. Without this the two tests above are satisfied by a field nothing ever varies.
#[test]
fn two_previews_across_a_changed_set_carry_different_stamps() {
    let mut store = PendingPreviews::default();
    let before = store.issue(any_intent(), Some(11));
    let after = store.issue(any_intent(), Some(12));
    assert_ne!(
        store.take(&before).expect("issued").accounts_epoch,
        store.take(&after).expect("issued").accounts_epoch,
        "a moved account set must be visible to the confirm that compares them"
    );
}

// The EXECUTION gate on the shipped templates (see `every_shipped_template_reaches_the_broker`):
// the real `RhaiStrategy` mounted over vike-model's shared `MockBroker` (its `test-support`
// feature, already a dev-dep of this crate for `tests/init_cli.rs`) and driven over real bars.
use std::sync::{Arc, Mutex};

use vike_model::strategy::MockBroker;
use vike_model::{Bar, Strategy};
use vike_script::RhaiStrategy;

/// The same no-node, no-credential server `crates/vike-cli/tests/mcp_transcript.rs` drives,
/// reached through the public constructor rather than re-spelled — a second field list here is
/// a second place to forget a field, and the two would then disagree about what "a fresh
/// server" is.
fn server() -> Server {
    test_server()
}

fn req(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(name: &str, args: Value) -> Value {
    server().handle(&req(1, "tools/call", json!({ "name": name, "arguments": args }))).unwrap()
}

/// Arguments broad enough for EVERY write tool at once — the twin of
/// `crates/vike-cli/tests/mcp_transcript.rs`'s helper of the same name, and it has to stay one:
/// the tools take disjoint argument sets and each ignores what it does not name, so the union
/// is what lets a loop walk [`WRITE_TOOLS`] with no per-tool table to fall out of step with it.
///
/// ⚠ **A NEW write tool whose required arguments are not in here fails the loops rather than
/// skipping them**, which is the behaviour to keep: the failure names the tool, and the fix is
/// one line. (`set_setting` takes only `key` and `value` since `docs/decisions/0086`; the `key` is
/// a config one, though a policy key would take the same path in this ATTENDED server — there is
/// no retype gate left for a policy write to meet.)
fn every_write_tools_arguments() -> Value {
    json!({
        "venue": "sim",
        "symbol": "BTCUSDT",
        "side": 1,
        "qty": 1.0,
        "client_order_id": "c1",
        "new_qty": 2.0,
        "state": "halted",
        "interval": "1m",
        "controller_id": "sim__BTCUSDT__1m",
        "name": "spread_maker",
        // …and `delete_series`' own required set. `kind`/`venue` are its SELECTOR (exact,
        // never a substring) and `produced_by` is REQUIRED on this surface for every call —
        // see that tool's doc for why it is unconditional here and conditional on the CLI.
        "kind": "bar",
        "produced_by": "klines:",
        "key": "config.tradehub_addr",
        "value": "127.0.0.1:7879"
    })
}

#[test]
fn initialize_echoes_protocol_and_names_the_server() {
    let resp =
        server().handle(&req(1, "initialize", json!({ "protocolVersion": "2025-06-18" }))).unwrap();
    assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(resp["result"]["serverInfo"]["name"], "vike-cli");
}

/// A function name NOTHING binds — the specimen the non-vacuity proof below rewrites a template
/// to call. Typo-shaped on purpose: the failure it stands for is a misspelling, and the two
/// assertions at the call site prove it is unbound rather than trusting this comment.
const UNBOUND_WITNESS: &str = "sma_typo";

#[test]
fn the_write_arm_and_the_write_roster_are_the_same_set() {
    // The `call_tool` match arm decides what is GATED; `WRITE_TOOLS` decides what is
    // ADVERTISED as destructive and what the transcript harness exercises. A tool in one and
    // not the other is either an ungated write or an unexercised gate — and until this test
    // existed the arm was held equal to NOTHING: the annotations and the roster were pinned to
    // each other, the routing was a seven-alternative pattern nobody compared to either.
    for name in WRITE_TOOLS {
        assert!(is_write_tool(name), "{name} is in WRITE_TOOLS but not routed as a write");
    }
    let spec = tools_spec();
    for tool in spec.as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let destructive = tool["annotations"]["destructiveHint"].as_bool() == Some(true);
        assert_eq!(
            destructive,
            is_write_tool(name),
            "{name}: destructiveHint and the write routing disagree"
        );
    }
}

// ---- tool scoping (`--profile`, `--deny-tool`) -------------------------------------------

/// EXACTLY what `--profile read-only` serves, spelled out.
///
/// ⚠ A positive list rather than a count, and rather than a filter re-derived here: a count
/// passes when one tool is swapped for another, and a re-derived filter is the implementation
/// agreeing with itself. A NEW tool breaks this test, which is correct — it is the
/// "adding a venue reddens every table" property applied to the tool roster: whoever adds a
/// tool decides which rings serve it, in a review, rather than inheriting a ring by accident.
const READ_ONLY_TOOLS: [&str; 12] = [
    "validate_strategy",
    "discover_params",
    "list_templates",
    "list_indicators",
    "run_backtest",
    "run_sweep",
    "run_walk_forward",
    "list_strategies",
    "list_series",
    "node_snapshot",
    // The two per-call node reads. They belong here on the SAME argument every row above them
    // does: `read-only` is `!is_write_tool`, and neither of these can change a byte on the node
    // — `strategy_status` and `settings_show` authenticate under the OBSERVE scope, which the
    // node will not accept a command on at all.
    "strategy_status",
    "settings_show",
];

/// …and EXACTLY what `--profile offline` serves: the four that open no socket.
const OFFLINE_TOOLS: [&str; 4] =
    ["validate_strategy", "discover_params", "list_templates", "list_indicators"];

fn advertised_names(access: &ToolAccess) -> Vec<String> {
    access
        .advertised()
        .as_array()
        .expect("tools/list is an array")
        .iter()
        .map(|t| t["name"].as_str().expect("every tool is named").to_string())
        .collect()
}

fn server_under(profile: Profile) -> Server {
    Server { access: ToolAccess::new(profile, Vec::new()), ..test_server() }
}

#[test]
fn the_default_profile_serves_the_roster_that_shipped_before_the_flag() {
    // Held equal to `tools_spec` itself rather than to a hand-written list of seventeen names:
    // "identical to today's roster" is the actual claim, and a third copy of that roster (after
    // `server.json`'s) is a third thing to keep in step.
    assert_eq!(
        ToolAccess::full().advertised(),
        tools_spec(),
        "an absent --profile must serve exactly what this server implements"
    );
    for name in served_tool_names() {
        assert!(ToolAccess::full().admits(&name), "{name} must be routed under the default");
    }
}

#[test]
fn the_read_only_profile_serves_exactly_the_reads() {
    let access = ToolAccess::new(Profile::ReadOnly, Vec::new());
    assert_eq!(advertised_names(&access), READ_ONLY_TOOLS);
    for tool in WRITE_TOOLS {
        assert!(!access.admits(tool), "{tool} is a write and must be withheld");
    }
}

#[test]
fn the_offline_profile_serves_exactly_the_tools_that_open_no_socket() {
    let access = ToolAccess::new(Profile::Offline, Vec::new());
    assert_eq!(advertised_names(&access), OFFLINE_TOOLS);
    // The ring is the tool's OWN declaration read back, so this is also a check that every
    // offline tool really does declare it.
    let spec = tools_spec();
    for tool in spec.as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let offline = tool["annotations"]["openWorldHint"] == json!(false);
        assert_eq!(offline, OFFLINE_TOOLS.contains(&name), "{name}: openWorldHint disagrees");
    }
}

#[test]
fn the_rings_nest() {
    let (full, read_only, offline) = (
        ToolAccess::full(),
        ToolAccess::new(Profile::ReadOnly, Vec::new()),
        ToolAccess::new(Profile::Offline, Vec::new()),
    );
    for name in served_tool_names() {
        if offline.admits(&name) {
            assert!(read_only.admits(&name), "{name}: offline ⊄ read-only");
        }
        if read_only.admits(&name) {
            assert!(full.admits(&name), "{name}: read-only ⊄ full");
        }
    }
    assert!(advertised_names(&offline).len() < advertised_names(&read_only).len());
    assert!(advertised_names(&read_only).len() < advertised_names(&full).len());
}

#[test]
fn the_roster_and_the_router_are_one_derivation() {
    // ⚠ THE PROPERTY THE WHOLE FEATURE RESTS ON. An advertised roster and a routing gate
    // computed separately are two lists that can disagree — a tool advertised and then refused
    // wastes an agent's turn, and one WITHHELD but still routed is an ungated write. Checked
    // for every ring AND with a `--deny-tool` layered on, because the subtraction is the case
    // where a second mechanism would most plausibly have been introduced.
    for profile in [Profile::Full, Profile::ReadOnly, Profile::Offline] {
        let access = ToolAccess::new(profile, vec!["list_series".to_string()]);
        let advertised = advertised_names(&access);
        for name in served_tool_names() {
            assert_eq!(
                advertised.contains(&name),
                access.admits(&name),
                "{name} under {profile:?}: tools/list and tools/call disagree"
            );
        }
        assert!(!access.admits("list_series"), "a denied tool is withheld under every ring");
    }
}

#[test]
fn a_write_tool_is_refused_under_read_only_and_the_refusal_names_the_profile() {
    let mut s = server_under(Profile::ReadOnly);
    let err = s
        .call_tool(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
        )
        .unwrap_err();
    assert!(err.refused, "a scope refusal is a GATE saying no, and the transcript reads that");
    assert!(err.message.contains("read-only"), "the profile must be named: {err:?}");
    assert!(err.message.contains("--profile full"), "…and how to change it: {err:?}");
    assert!(err.message.contains("node_snapshot"), "…and what IS served: {err:?}");
    // Nothing was previewed either: the gate runs BEFORE the command is built, so a refused
    // write leaves no token behind to be confirmed later.
    assert!(s.pending.by_token.is_empty(), "a refused write must mint nothing");
}

#[test]
fn a_denied_tool_is_subtracted_from_whatever_the_profile_allows() {
    // ONE mechanism, not a second one: the deny list feeds the same pass the profile does.
    let access = ToolAccess::new(Profile::Full, vec!["market_exit".to_string()]);
    assert!(!access.admits("market_exit"));
    assert!(access.admits("submit_order"), "the rest of the ring is untouched");
    assert!(!advertised_names(&access).contains(&"market_exit".to_string()));
    let mut s = Server { access, ..test_server() };
    let err = s.call_tool("market_exit", &json!({})).unwrap_err();
    assert!(err.refused);
    assert!(err.message.contains("--deny-tool market_exit"), "got: {err:?}");
}

#[test]
fn an_unknown_profile_is_refused_and_never_widens_to_full() {
    // ⚠ THE FAILURE THIS PINS is the one the rival surface gets right and a `_ => Full` arm
    // gets catastrophically wrong: a typo in a launch config that silently serves the write
    // tools to an agent the operator believed was scoped down.
    let err = Profile::parse("readonly").unwrap_err();
    for name in profile_names() {
        assert!(err.contains(name), "the error must name every valid profile: {err}");
    }
    let refused = parse_config(["--profile", "readonly"].map(String::from).into_iter(), None, None)
        .expect_err("an unknown profile must not parse");
    assert!(refused.contains("read-only"), "got: {refused}");
    // …and the whole invocation is refused rather than started, so there is no server to widen.
    let ok = parse_config(["--profile", "read-only"].map(String::from).into_iter(), None, None)
        .expect("a real profile parses");
    assert_eq!(ok.access.profile_name(), "read-only");
}

#[test]
fn an_unknown_deny_tool_is_a_usage_error_naming_the_roster() {
    // A silently-ignored `--deny-tool` is an operator believing a tool is gone when it is not.
    let err =
        parse_config(["--deny-tool", "submit-order"].map(String::from).into_iter(), None, None)
            .expect_err("a name this server does not serve must not parse");
    assert!(err.contains("submit_order"), "the roster must be quoted back: {err}");
    let ok =
        parse_config(["--deny-tool", "submit_order"].map(String::from).into_iter(), None, None)
            .expect("a served name parses");
    assert!(!ok.access.admits("submit_order"));
}

#[test]
fn every_resource_is_a_way_in_to_a_named_tool() {
    // The mapping the resource scope gate reads, held equal to the advertised resources in BOTH
    // directions — a third resource with no row here would silently escape the profile.
    let spec = resources_spec();
    let uris: Vec<&str> =
        spec.as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
    for (uri, tool) in RESOURCE_TOOLS {
        assert!(uris.contains(&uri), "RESOURCE_TOOLS names {uri}, which is not advertised");
        assert!(
            served_tool_names().iter().any(|t| t == tool),
            "{uri} claims to be a way in to {tool}, which this server does not serve"
        );
    }
    for uri in &uris {
        assert!(
            RESOURCE_TOOLS.iter().any(|(u, _)| u == uri),
            "{uri} is advertised with no tool row — it would escape the profile gate"
        );
    }
}

#[test]
fn a_resource_is_withheld_exactly_when_the_tool_it_reaches_is() {
    // ⚠ A resource is a second WAY IN to a tool's implementation. Under `offline`, serving
    // `vike://node/snapshot` would hand back the answer of a tool the profile refused.
    let mut offline = server_under(Profile::Offline);
    let listed = offline.handle(&req(1, "resources/list", json!({}))).unwrap();
    assert_eq!(listed["result"]["resources"], json!([]), "got {listed}");
    let read = offline
        .handle(&req(2, "resources/read", json!({ "uri": "vike://node/snapshot" })))
        .unwrap();
    assert_eq!(read["error"]["code"], -32002, "got {read}");
    assert!(read["error"]["message"].as_str().unwrap().contains("offline"), "got {read}");

    // …and `read-only` withholds NEITHER, because neither resource is a write. That is the
    // non-vacuity half: without it the assertions above would pass on a gate that refused
    // every resource under every profile.
    let mut read_only = server_under(Profile::ReadOnly);
    let listed = read_only.handle(&req(3, "resources/list", json!({}))).unwrap();
    assert_eq!(listed["result"]["resources"].as_array().unwrap().len(), RESOURCE_TOOLS.len());
}

#[test]
fn a_scoped_prompt_says_the_writes_are_absent_instead_of_teaching_them() {
    let mut s = server_under(Profile::ReadOnly);
    let got = s.handle(&req(1, "prompts/get", json!({ "name": "triage_a_stuck_order" }))).unwrap();
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.starts_with("⚠ THIS SESSION SERVES NO ORDER-WRITE TOOL"), "got: {text}");
    assert!(text.contains("read-only"), "the banner names the profile: {text}");
    assert!(
        !text.contains("preview_token"),
        "a sheet must not teach a two-call gate for tools it has just said are absent: {text}"
    );
    // The READ half of the sheet survives — the prompt is still the right procedure, minus the
    // acting.
    assert!(text.contains("node_snapshot"), "got: {text}");
    // …and under the default profile the banner is absent entirely (byte-identical teaching).
    let mut full = server();
    let got =
        full.handle(&req(2, "prompts/get", json!({ "name": "triage_a_stuck_order" }))).unwrap();
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(!text.contains("SERVES NO ORDER-WRITE TOOL"), "got: {text}");
    assert!(text.contains("preview_token"));
}

// ---- the agent transcript (`--trace`) ----------------------------------------------------

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(tag).tempdir().expect("a scratch directory")
}

/// A server that RECORDS, writing into `dir`. Everything else is [`test_server`]'s — no node,
/// no credentials — so nothing here can reach a venue whatever the transcript says.
fn tracing_server(dir: &std::path::Path, profile: Profile) -> Server {
    Server {
        access: ToolAccess::new(profile, Vec::new()),
        trace: Some(McpTrace::new(dir.to_path_buf())),
        ..test_server()
    }
}

/// Every record written under `dir`, parsed. Reads what is ON DISK rather than what the writer
/// returned: the claim is that the transcript survives, not that a function was called.
fn records(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).expect("the transcript directory exists");
    let mut paths: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable record file");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).expect("one JSON object per line"));
        }
    }
    out
}

#[test]
fn a_refused_write_is_recorded_with_the_reason_it_was_refused() {
    // ⚠ THE WHOLE POINT OF THE FILE. A refusal reaches the NODE's audit trail never — it was
    // refused before a byte left this process — so if it is not here it is nowhere.
    let dir = scratch("vike-mcp-refused");
    let mut s = tracing_server(dir.path(), Profile::ReadOnly);
    let resp = s
            .handle(&req(
                1,
                "tools/call",
                json!({ "name": "submit_order", "arguments": { "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 } }),
            ))
            .unwrap();
    assert_eq!(resp["result"]["isError"], true, "got {resp}");
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1, "one call, one record: {recorded:?}");
    assert_eq!(recorded[0]["tool"], json!("submit_order"));
    assert_eq!(recorded[0]["verdict"], json!("refused"));
    assert_eq!(recorded[0]["write"], json!(true));
    assert_eq!(recorded[0]["profile"], json!("read-only"));
    assert!(
        recorded[0]["detail"].as_str().unwrap().contains("read-only"),
        "the reason must be recorded, not just the fact: {recorded:?}"
    );
}

/// **THE DECLARED BOUND on what a `set_setting` transcript can say**, pinned in both
/// directions so a green run means it was measured rather than assumed.
///
/// `crate::cmd::mcp::trace` redacts an argument whose NAME is credential-shaped, and
/// `vike_config::is_secret_key` — the workspace's one authority for that — matches the bare
/// word `KEY`. So the `key` argument is recorded redacted, and no spelling of that concept
/// escapes it (`setting_key`, `dotted_key`: all `_KEY`). The fix is NOT a second table here;
/// what the surface owes instead is to say what the record still carries.
#[test]
fn the_transcript_redacts_the_settings_key_and_keeps_the_rest() {
    let dir = scratch("vike-mcp-setting");
    let mut s = tracing_server(dir.path(), Profile::Full);
    let args = json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" });
    s.handle(&req(1, "tools/call", json!({ "name": "set_setting", "arguments": args }))).unwrap();
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1, "one call, one record: {recorded:?}");
    assert_eq!(recorded[0]["tool"], json!("set_setting"));
    assert_eq!(recorded[0]["write"], json!(true), "a lifecycle verb is a WRITE in the record");
    assert_eq!(recorded[0]["verdict"], json!("preview"), "the first call sends nothing");
    // The bound…
    assert_ne!(
        recorded[0]["args"]["key"],
        json!("config.tradehub_addr"),
        "`key` matches the bare `KEY` shape, so its value is redacted — see this tool's spec \
             comment for why that is not fixed here: {recorded:?}"
    );
    // …and what survives it, which is what makes the record still worth keeping. (The tool's
    // `file` argument used to survive too; it went with the file era, `docs/decisions/0086`.)
    assert_eq!(recorded[0]["args"]["value"], json!("127.0.0.1:7879"), "{recorded:?}");
}

#[test]
fn a_preview_records_the_minted_token_and_the_confirm_records_the_presented_one() {
    let dir = scratch("vike-mcp-token");
    let mut s = tracing_server(dir.path(), Profile::Full);
    let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let preview = s
        .handle(&req(1, "tools/call", json!({ "name": "submit_order", "arguments": args })))
        .unwrap();
    let token = preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .expect("a preview mints a token")
        .to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token.clone());
    // With no node configured this gets PAST the gate and fails at the connection, which is how
    // this suite tells "admitted" from "refused" without ever reaching a venue.
    s.handle(&req(2, "tools/call", json!({ "name": "submit_order", "arguments": confirming })))
        .unwrap();

    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0]["verdict"], json!("preview"), "a preview is not an `ok`");
    assert_eq!(recorded[0]["token"], json!(token));
    assert_eq!(recorded[0]["token_role"], json!(TOKEN_MINTED));
    assert_eq!(recorded[1]["token"], json!(token), "the same identity, on the call that used it");
    assert_eq!(recorded[1]["token_role"], json!(TOKEN_PRESENTED));
    assert_eq!(
        recorded[1]["verdict"],
        json!("error"),
        "an unreachable node is an ERROR, not a gate refusal: {recorded:?}"
    );
    // ⚠ The token is recorded as an IDENTITY in its own field and NOT copied out of the
    // arguments — where the credential-name rule redacts it like any other `_TOKEN`.
    assert_eq!(recorded[1]["args"]["preview_token"], json!(trace::REDACTED));
    // The order's own fields ARE recorded: what the agent attempted is the point.
    assert_eq!(recorded[1]["args"]["qty"], json!(1.0));
}

#[test]
fn a_read_tool_is_recorded_too() {
    // Non-vacuity for the write assertions above: the transcript is a record of the SESSION,
    // not a second copy of the order log.
    let dir = scratch("vike-mcp-read");
    let mut s = tracing_server(dir.path(), Profile::Full);
    s.handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} }))).unwrap();
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["tool"], json!("list_templates"));
    assert_eq!(recorded[0]["verdict"], json!("ok"));
    assert_eq!(recorded[0]["write"], json!(false));
    assert_eq!(recorded[0]["token"], Value::Null, "a read mints and presents no token");
    assert_eq!(recorded[0]["token_role"], Value::Null);
}

#[test]
fn a_planted_secret_in_an_argument_never_reaches_the_transcript() {
    // Two shapes, because the rule has two positions: a credential-shaped KEY, and a credential
    // NAMED inside free text. `crate::cmd::mcp::trace` owns the rule; this proves it is actually
    // wired into the server's own path rather than only unit-tested beside it.
    let dir = scratch("vike-mcp-secret");
    let mut s = tracing_server(dir.path(), Profile::Full);
    s.handle(&req(
        1,
        "tools/call",
        json!({ "name": "submit_order", "arguments": {
            "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0,
            "api_secret": "PLANTED-ARGUMENT-SECRET",
            "reason": "using BINANCE_LIVE_API_SECRET=PLANTED-INLINE-SECRET"
        }}),
    ))
    .unwrap();
    let recorded = records(dir.path());
    let line = recorded[0].to_string();
    assert!(!line.contains("PLANTED-ARGUMENT-SECRET"), "a keyed secret reached disk: {line}");
    assert!(!line.contains("PLANTED-INLINE-SECRET"), "an inline secret reached disk: {line}");
    assert_eq!(recorded[0]["args"]["api_secret"], json!(trace::REDACTED));
    assert_eq!(recorded[0]["args"]["reason"], json!(trace::REDACTED));
}

#[test]
fn without_the_flag_the_transcript_writes_nothing_at_all() {
    // ⚠ Not "writes an empty file" — creates NOTHING. The default must leave no trace on an
    // operator's disk, because the default is what every existing MCP client config runs.
    let dir = scratch("vike-mcp-off");
    let mut s = test_server();
    assert!(s.trace.is_none(), "the default server records nothing");
    s.handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} }))).unwrap();
    s.handle(&req(
            2,
            "tools/call",
            json!({ "name": "submit_order", "arguments": { "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 } }),
        ))
        .unwrap();
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(left.is_empty(), "an untraced session wrote {} entries", left.len());
}

#[test]
fn the_transcript_survives_a_restart_and_a_second_session_appends_to_it() {
    // Two servers over one directory, which is BOTH the restart shape and the two-agents shape.
    let dir = scratch("vike-mcp-restart");
    let mut first = tracing_server(dir.path(), Profile::Full);
    first
        .handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} })))
        .unwrap();
    drop(first);
    let mut second = tracing_server(dir.path(), Profile::ReadOnly);
    second
        .handle(&req(1, "tools/call", json!({ "name": "list_indicators", "arguments": {} })))
        .unwrap();
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 2, "the second session APPENDED: {recorded:?}");
    assert_eq!(recorded[0]["tool"], json!("list_templates"), "the first record survived");
    assert_eq!(recorded[1]["tool"], json!("list_indicators"));
    assert_eq!(recorded[1]["profile"], json!("read-only"), "each record names its own scope");
}

#[test]
fn a_trace_that_cannot_resolve_a_project_refuses_rather_than_recording_nowhere() {
    // ⚠ The asymmetry with `vike_boot::journal_boot_settings`, which records nothing when there
    // is no project: there NOBODY asked, here the operator TYPED `--trace`, and a server that
    // started anyway would be discovered to have recorded nothing after the incident.
    let err = resolve_trace(TraceRequest::ProjectState, None).expect_err("no project, no home");
    assert!(err.contains("--trace-dir"), "the refusal must name a way out: {err}");
    // Spelled without its `VIKE_` head deliberately: `crates/vike-ops/tests/settings_registry.rs`
    // harvests env-shaped string literals out of `src/` and demands a `SETTINGS` row for each,
    // keyed on `(name, krate)` — and this crate has no row for that variable, because it reads
    // it through `vike-boot` rather than itself.
    assert!(err.contains("SETTINGS_DIR"), "…and the other one: {err}");
    // The named-directory form is honourable with no project at all.
    let dir = scratch("vike-mcp-resolve");
    let resolved = resolve_trace(TraceRequest::Dir(dir.path().to_path_buf()), None)
        .expect("an explicit directory needs no project");
    assert_eq!(resolved.expect("a writer").dir(), dir.path());
    // …and no flag is no writer, whatever the project situation is.
    assert!(resolve_trace(TraceRequest::Off, None).expect("off is not a failure").is_none());
}

#[test]
fn the_registry_manifest_lists_every_tool_this_server_serves() {
    // The manifest is what a registry user reads BEFORE installing. A tool added to
    // `tools_spec` and not to the manifest is a promise the listing does not make; one in the
    // manifest and not the server is a promise it cannot keep. Neither is visible to any other
    // gate — the manifest is a hand-written file at the repository root, and nothing else in
    // this workspace reads it.
    //
    // ⚠ It compares `_vike.tools`, a block WE own, rather than anything the upstream registry
    // schema names. The schema has already changed more than once; a gate keyed on an upstream
    // field would silently stop checking the day a key was renamed.
    const MANIFEST: &str = include_str!("../../../../server.json");
    let manifest: Value = serde_json::from_str(MANIFEST).expect("server.json is valid JSON");
    let listed: Vec<&str> = manifest["_vike"]["tools"]
        .as_array()
        .expect("server.json carries _vike.tools")
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    let spec = tools_spec();
    let served: Vec<&str> =
        spec.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(listed, served, "server.json's tool list has drifted from tools_spec");
    assert_eq!(
        manifest["version"].as_str(),
        Some(SERVER_VERSION),
        "server.json's version must match the crate the server reports at initialize"
    );
    // The registry's `description` is capped at 100 characters (the 2025-12-11 schema), and a
    // manifest that overruns it is refused at publish time rather than here — which is the
    // wrong place to find out, since publishing is a deliberate one-off act.
    let description = manifest["description"].as_str().expect("server.json carries description");
    assert!(
        (1..=100).contains(&description.chars().count()),
        "the registry caps `description` at 100 characters; this one is {}",
        description.chars().count()
    );
    // The mirror's FORBID scan aborts the WHOLE publish on a box name or a private path, and
    // this file is the first root-level manifest to ship. Catch it here, where the failure
    // names the manifest, rather than at release time where it names a grep.
    assert!(
        !MANIFEST.contains("the latency box") && !MANIFEST.contains("the CI box"),
        "server.json must name no host — the mirror's FORBID scan aborts the publish"
    );
}

/// **THE DATA DELETER KEEPS EVERY GUARD.** A POSITIVE gate, and it replaces an EXCLUSION.
///
/// The design that proposed `delete_series` opened by forbidding it — a model must not delete
/// data — and its gate would have been an ABSENCE check over `tools_spec` (no advertised tool
/// may contain `delete`/`remove`/`purge`/…). The owner reversed that on 2026-09-07: a user must
/// be able to delete through both the CLI and this surface. **An absence gate cannot be adapted
/// to a present capability**, so the shape is inverted: the tool exists, and this holds every
/// guard that made permitting it defensible.
///
/// §9.1's grant argument survives the reversal and is what sets the bar: an irreversible delete
/// of market history is a larger grant than an order, and in one way larger than a credential
/// write — a credential can be reissued at the venue, and a deleted tape whose venue no longer
/// serves that window cannot be re-fetched at all. Five evidences, because losing any ONE of
/// them is a different failure:
///
/// 1. **`delete_series` is in [`WRITE_TOOLS`].** Removing it would silently drop the mandatory
///    preview, the `destructiveHint`, the `read-only` withholding and the transcript's write
///    classification IN ONE EDIT. That is the failure this evidence exists for, and it is why
///    the roster membership is asserted rather than inferred from the annotations.
/// 2. **A call without `confirm: true` mutates NOTHING** — asserted by driving the real
///    [`Server`] against a real datahub over a RECORDING store, and checking the store saw no
///    delete. NOT by inspecting the returned text: a preview that SAYS it changed nothing while
///    having changed something is exactly what a text assertion cannot catch.
/// 3. **A token bound to a DIFFERENT deletion is refused** — the binding exercised rather than
///    assumed, and over `produced_by` as well as the selector, because the assertion decides
///    what actually goes.
/// 4. **`produced_by` is required by the schema**, and a call omitting it is refused BEFORE any
///    store is opened.
/// 5. **It is absent from `tools/list` under both `read-only` and `offline`**, asserted against
///    the real [`ToolAccess`] rings rather than against a hand-written expectation.
#[test]
fn the_data_deleter_keeps_every_guard() {
    // ---- 1. the roster membership, which is the four other guards in one line ---------------
    assert!(is_write_tool("delete_series"), "delete_series must route as a write");
    assert!(WRITE_TOOLS.contains(&"delete_series"), "…and be ON the roster that says so");
    let spec = tools_spec();
    let tool = spec
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "delete_series")
        .expect("delete_series is advertised");
    assert_eq!(tool["annotations"]["destructiveHint"], json!(true));
    assert_eq!(tool["annotations"]["readOnlyHint"], json!(false));
    assert_eq!(
        tool["annotations"]["idempotentHint"],
        json!(true),
        "the underlying delete IS idempotent, and saying so is honest: a retry after a dropped \
             reply does not double-delete"
    );
    assert_eq!(
        tool["annotations"]["openWorldHint"],
        json!(true),
        "it reaches a datahub over a socket — which is also what withholds it from `offline`"
    );

    // ---- 4. the schema requires the assertion, and the router refuses without it ------------
    let required: Vec<&str> = tool["inputSchema"]["required"]
        .as_array()
        .expect("delete_series declares required properties")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        required.contains(&"produced_by"),
        "produced_by must be REQUIRED by the schema, unconditionally: {required:?}"
    );
    let resp = call("delete_series", json!({ "kind": "bar", "venue": "binance" }));
    assert_eq!(resp["result"]["isError"], true, "a call with no produced_by must be refused");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("REQUIRED"), "the refusal must say so: {text}");
    assert!(
        resp["result"]["structuredContent"].is_null(),
        "a refusal is not a preview and must mint no token: {resp}"
    );

    // ---- 5. the rings withhold it, off the REAL ToolAccess ---------------------------------
    for profile in [Profile::ReadOnly, Profile::Offline] {
        let access = ToolAccess::new(profile, Vec::new());
        assert!(!access.admits("delete_series"), "{profile:?} must withhold the deleter",);
        assert!(
            !advertised_names(&access).iter().any(|n| n == "delete_series"),
            "{profile:?} must not ADVERTISE it either"
        );
    }
}

/// A datahub that ADVERTISES the delete verb and RECORDS every `dry_run` flag it is sent,
/// answering each with a fixed one-series plan.
///
/// ⚠ It speaks the wire rather than wrapping a store, and that is what makes the proof
/// possible at all: the property under test is "a preview SENDS a dry run", which is a fact
/// about the REQUEST — and the store this server would delete from lives in another process on
/// another box, so "the series is still on disk" is not a thing this crate can look at. A
/// `HistStore` double would have proved the same thing one layer further away, through eighteen
/// delegating methods.
///
/// Key-LESS on purpose (no `FEATURE_AUTH`, no nonce), so the plain `DatahubClient::connect`
/// reaches it — this fixture's `Server` is built with `datahub_keys: None`, which is the arm
/// that fallback belongs to.
fn spawn_recording_datahub() -> (std::net::SocketAddr, Arc<Mutex<Vec<bool>>>) {
    use std::net::TcpListener;
    use vike_datahub_client::proto::{
        DeleteDone, PROTO_VERSION, RemovalOutcome, RemovalPlan, Request, Response, SeriesSelector,
        read_frame, write_frame,
    };

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let seen: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let recorded = Arc::clone(&recorded);
            std::thread::spawn(move || {
                while let Ok(request) = read_frame::<_, Request>(&mut s) {
                    let response = match request {
                        Request::Hello { .. } => Response::Welcome {
                            proto_version: PROTO_VERSION,
                            features: vec!["delete_series".to_string()],
                            nonce: None,
                        },
                        Request::DeleteSeries { dry_run, .. } => {
                            recorded.lock().unwrap().push(dry_run);
                            let plan = RemovalPlan {
                                selector: SeriesSelector::new("bar", "binance"),
                                produced_by: Some("klines:".to_string()),
                                series: Vec::new(),
                            };
                            Response::Deleted(DeleteDone {
                                plan,
                                outcome: (!dry_run).then(RemovalOutcome::default),
                            })
                        }
                        other => Response::Error(format!("unexpected {other:?}")),
                    };
                    if write_frame(&mut s, &response).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, seen)
}

/// **Evidences 2 and 3 of [`the_data_deleter_keeps_every_guard`]**, driven end to end against a
/// real server and a real wire.
///
/// 2. a call WITHOUT `confirm` sends `dry_run: true` and NOTHING else — the mutation gate, and
///    it is asserted on what left the process rather than on what the answer says about itself;
/// 3. a token bound to a DIFFERENT deletion is refused — including one that differs only in
///    `produced_by`, which is the argument that decides what actually goes.
#[test]
fn a_delete_preview_sends_only_a_dry_run_and_its_token_is_bound_to_it() {
    let (addr, sent) = spawn_recording_datahub();
    let mut s = Server { datahub_addr: addr.to_string(), ..test_server() };
    let args = json!({
        "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h",
        "produced_by": "klines:"
    });

    // ---- 2. the PREVIEW ---------------------------------------------------------------------
    let resp = s
        .handle(&req(1, "tools/call", json!({ "name": "delete_series", "arguments": args })))
        .unwrap();
    let body = &resp["result"]["structuredContent"];
    assert_eq!(body["will_execute"], json!(false), "{resp}");
    let token = body["preview_token"].as_str().expect("a plan mints a token").to_string();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![true],
        "a preview must send EXACTLY ONE request, and it must be a dry run"
    );

    // ---- 3a. the SAME token against a DIFFERENT SELECTOR ------------------------------------
    let mut other = args.clone();
    other["symbol"] = json!("ETHUSDT");
    other["confirm"] = json!(true);
    other["preview_token"] = json!(token);
    let resp = s
        .handle(&req(2, "tools/call", json!({ "name": "delete_series", "arguments": other })))
        .unwrap();
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("does not match this deletion"), "{text}");
    assert_eq!(
        *sent.lock().unwrap(),
        vec![true],
        "a refused confirm must send NOTHING — the token is consumed before the socket"
    );

    // ⚠ The token is spent by that attempt (single use), so the next case takes a fresh one.
    let resp = s
        .handle(&req(3, "tools/call", json!({ "name": "delete_series", "arguments": args })))
        .unwrap();
    let token = resp["result"]["structuredContent"]["preview_token"].as_str().unwrap().to_string();

    // ---- 3b. the same SELECTOR under a DIFFERENT ASSERTION ----------------------------------
    let mut reasserted = args.clone();
    reasserted["produced_by"] = json!("pmxt:");
    reasserted["confirm"] = json!(true);
    reasserted["preview_token"] = json!(token.clone());
    let resp = s.handle(&req(
        4,
        "tools/call",
        json!({ "name": "delete_series", "arguments": reasserted }),
    ));
    let resp = resp.unwrap();
    assert_eq!(
        resp["result"]["isError"], true,
        "`produced_by` is part of the INTENT: a token previewed under one assertion must not \
             confirm a delete under another: {resp}"
    );

    // ---- ...and the CONFIRMING call, which is the non-vacuity proof for all of the above ----
    let resp = s
        .handle(&req(5, "tools/call", json!({ "name": "delete_series", "arguments": args })))
        .unwrap();
    let token = resp["result"]["structuredContent"]["preview_token"].as_str().unwrap().to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let resp = s.handle(&req(
        6,
        "tools/call",
        json!({ "name": "delete_series", "arguments": confirming }),
    ));
    let resp = resp.unwrap();
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    assert_eq!(resp["result"]["structuredContent"]["will_execute"], json!(true));
    let flags = sent.lock().unwrap().clone();
    assert_eq!(
        flags.last(),
        Some(&false),
        "only a CONFIRMED call may send a non-dry-run request: {flags:?}"
    );
    assert_eq!(
        flags.iter().filter(|d| !**d).count(),
        1,
        "…and exactly one of them did, out of {} requests: {flags:?}",
        flags.len()
    );
}

/// **THE MCP SURFACE ADVERTISES NO CREDENTIAL WRITER, AND CONTAINS NO CALL INTO ONE.**
///
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is the
/// record, and its fourth reason is the one this test holds: `mcp` is a verb of the SAME binary
/// as `secrets`, one dispatch arm away from any CLI writer, so the only way to keep a credential
/// write out of the agent surface for certain is to keep it out of this file. That mattered in
/// the abstract while `vike-cli` had no writer at all; since `vike-cli secrets set` exists it is
/// a live property, and the record's reopen clause names an MCP tool as one of the four things
/// that would re-decide the whole verdict.
///
/// The comparison is against Hummingbot's MCP server, whose `setup_connector` tool takes an
/// AGENT-SUPPLIED credentials dict and writes it, gated only by a `confirm_override` flag for a
/// connector that already exists — so a NEW key is written with no gate at all. A credential
/// write is a larger grant than an order, and this server already makes order preview mandatory.
///
/// ⚠ FOUR evidences, because each alone is defeatable. A tool NAMED `rotate_secret` with an
/// innocuous description passes a description-only check; a tool called `configure` that
/// happened to call the writer passes a name-only one; a free-text `instructions` sentence
/// naming a write verb reaches the model without being a tool at all; and — the one
/// `docs/decisions/0065` §2 measured — a tool that builds an account-administration WIRE frame
/// reaches a credential write on the DAEMON's box while calling no writer in this process, so
/// every function-name needle here stays green. So: no advertised tool may pair a credential
/// word with a write word, AND this file may not call the writer at all, AND every `secrets`
/// subcommand the instructions name must be a READ, AND this file may not name the account
/// wire types.
///
/// ⚠ **And a FIFTH evidence lives in its own test, deliberately:**
/// `the_account_verbs_are_unreachable_through_a_helper_that_names_no_wire_type`. The fourth
/// above keys on this file NAMING a wire type, and argues that a frame cannot be built without
/// naming its request type — true of the frame, false of the CALLER. A client helper taking
/// primitives builds the frame inside ITSELF, exactly as `set_setting` already does for
/// settings, and this file reaches the verb naming nothing. That evidence is separate rather
/// than a fifth leg here so the two can be MEASURED apart: plant such a helper and this test
/// stays green while that one reddens, which is `docs/decisions/0065` §2's finding rather than
/// an assertion about it.
///
/// ⚠ **What it does NOT walk: the PROMPT texts.** The subject/act scan reads `tools_spec` only,
/// and the prompt bodies (`arm_a_venue`) plus a tool error string (`missing_key`) do pair a
/// credential word with a store word — they would trip this test if it were pointed at them,
/// which is why the scope is stated rather than left to be inferred from a green run. It is a
/// deliberate bound and not an oversight: a prompt EXECUTES nothing, and the second evidence
/// below — this file calls no writer — already holds for the whole file, prompts included. The
/// residual is a prompt that TELLS an agent to reach a credential by another route (a shell
/// tool, say), which `0036`'s reopen clause explicitly covers ("including one that merely calls
/// a shell") and which no text scan of this file could catch anyway.
#[test]
fn the_mcp_surface_advertises_no_credential_writer() {
    // The subject words, and the act words. Substring matching on purpose — `secrets`,
    // `credential` and `rotate_secret` all have to be caught, and a tool that talks about a
    // credential without proposing to change one (there are none today) would still be flagged
    // and would then be a deliberate decision rather than a drift.
    const SUBJECT: [&str; 4] = ["secret", "credential", "api key", "api_key"];
    const ACT: [&str; 6] = ["set", "write", "rotate", "store", "save", "configure"];

    let spec = tools_spec();
    for tool in spec.as_array().expect("tools_spec is an array") {
        let name = tool["name"].as_str().expect("every tool is named").to_lowercase();
        let description = tool["description"].as_str().unwrap_or_default().to_lowercase();
        for text in [&name, &description] {
            let subject = SUBJECT.iter().find(|s| text.contains(**s));
            let act = ACT.iter().find(|a| text.contains(**a));
            if let (Some(s), Some(a)) = (subject, act) {
                panic!(
                    "tool `{name}` pairs `{s}` with `{a}`: the MCP surface may advertise no \
                         credential write. See docs/decisions/0036 — an MCP tool is one of the \
                         four things its reopen clause says re-decides the record from the top."
                );
            }
        }
    }

    // …and the ROUTING half: this file calls neither the workspace's one upsert nor its
    // journalled wrapper, so no tool can reach a credential write by any name at all.
    //
    // ⚠ Both needles are `concat!`ed rather than spelled, and that is not decoration: this test
    // reads its OWN file, so a whole spelling written here as test DATA would report itself as a
    // call site and the assertion could never pass. It is the same self-scanning trap
    // `crates/vike-ops/src/scan.rs`'s `find_calls` documents for the gate that walks it.
    //
    // ⚠ "THIS FILE" is the whole MODULE since `cmd/mcp.rs` became a directory (code-layout phase 2,
    // task 9): the parent plus the seven children that took its code. A scan of the parent alone
    // would have stayed green while every moved line left its reach. They are `include_str!`ed
    // rather than walked for the reason the floor below gives — a path that stops existing is a
    // compile error here, which is the right failure direction for a fence.
    const THIS_FILE: &str = concat!(
        include_str!("mcp.rs"),
        include_str!("mcp/backtest_tools.rs"),
        include_str!("mcp/data_tools.rs"),
        include_str!("mcp/node_reads.rs"),
        include_str!("mcp/node_writes.rs"),
        include_str!("mcp/offline_tools.rs"),
        include_str!("mcp/tool_schemas.rs"),
        include_str!("mcp/venue_gate.rs"),
    );
    const UPSERT: &str = concat!("save_", "credentials(");
    const JOURNALLED: &str = concat!("save_", "credentials_journalled(");
    // ⚠ THE THIRD NEEDLE, since `docs/decisions/0054`'s credential half: the Backend-aware
    // upsert that routes a write to the settings DATABASE or to the file. It is the name every
    // production writer in this workspace now calls, so a refusal keyed on the two above alone
    // would have left the MCP surface able to reach a credential write by the only spelling
    // anybody uses. The three are independent matches — `credentials_to_store(` is not
    // `credentials(` — so this adds a needle rather than widening one.
    const ROUTED: &str = concat!("save_", "credentials_to_store(");
    // ⚠ THE FOURTH NEEDLE, and it is not an upsert at all — it is the CREATE.
    // `vike_secrets::migrate` builds the settings database and fills it from both credential
    // files, which is a strictly larger act than replacing a named key, and none of the three
    // needles above matches it. Without this line `crates/vike-cli/src/cmd/mcp.rs` could call
    // the migrator inside a tool handler and this half stayed green, while
    // `docs/decisions/0036`'s FOURTH amendment asserted that this very test held the property
    // "exactly as for the three writers before it". It did not; it does now.
    //
    // The surface was never unguarded — `crates/vike-ops/tests/credential_writer_gate.rs` keys
    // `migrate` tree-wide and `mcp.rs` is not in its `WRITER_CALLERS` — but a record that names
    // a test for a property that test does not hold is the defect, whichever other gate happens
    // to cover it.
    const MIGRATOR: &str = concat!("migr", "ate(");
    for writer in [UPSERT, JOURNALLED, ROUTED, MIGRATOR] {
        assert!(
            !THIS_FILE.contains(writer),
            "cmd/mcp.rs (or a child of it) calls `{writer}` — the MCP surface is read-only about \
             credentials"
        );
    }
    // Non-vacuity: the needle really does find a call when one is present, so a rename of the
    // writer cannot turn this half silently green. `crates/vike-ops/tests/
    // credential_writer_gate.rs` is the tree-wide version of the same question, with a kill
    // proof that plants a real call.
    //
    // ⚠ Keyed on `ROUTED`, and the day it moved is worth recording: `secrets set` called
    // `save_credentials` directly until the store acquired a database home, at which point it
    // moved onto the Backend-aware upsert and `secrets.rs` stopped containing the old needle at
    // all. This floor caught that — it went red while the two refusals above stayed green,
    // which is precisely the "a fix breaks its NEIGHBOUR" shape it exists for. Point it at the
    // writer the CLI actually calls, never at whichever needle happens to still match.
    //
    // ⚠ It names `secrets/set.rs` and not `secrets.rs` since `cmd/secrets.rs` was split by verb
    // (code-layout phase 2, task 10): the writer call lives in the child that holds `secrets set`.
    assert!(
        include_str!("secrets/set.rs").contains(ROUTED),
        "the needle must match the CLI's one writer call site, or this proves nothing"
    );

    // …and the FOURTH half — placed here beside the ROUTING half it extends rather than after
    // the instructions half below, because it asks the same question that one does: can this
    // file REACH a credential write? `docs/decisions/0065-accounts-are-managed-and-the-
    // barrier-is-declared.md` §2 owes it by name: **no tool may CONSTRUCT the account-
    // administration wire verbs.**
    //
    // ⚠ The three halves above are blind to it, and 0065 measured that rather than assuming
    // it: *"All three stay green while an agent gains this capability."* The reason is that
    // this surface is ALREADY a first-class node-wire client — `Server::execute` special-cases
    // `WireCommand::SetSetting` and routes it to `execute_settings_write`, which opens its own
    // connection and calls a CLIENT HELPER. So a new command variant is reachable the moment a
    // tool builds one, and no needle keyed on a WRITER's function name can see it: the
    // credential never passes through `save_credentials_to_store` in THIS process at all — it
    // goes on a wire and the DAEMON writes it. `crates/vike-ops/tests/
    // credential_writer_gate.rs` is equally blind for the same reason, and its `mcp.rs`-shaped
    // hole is this assertion.
    //
    // Three things keep the surfaces apart and this is one of them; the other two are the
    // SCOPE (those verbs require `Scope::Account`, a third key, and `vike-cli` holds no verb
    // that presents one) and the ADVERTISEMENT (`FEATURE_ACCOUNT_VERBS` is withheld by
    // `served_features` unless the daemon's own barrier declaration armed the capability).
    // Neither of those is a property of THIS file, which is why this one is.
    //
    // ⚠ Needles `concat!`ed for the self-scanning reason above, and keyed on the TYPES rather
    // than on a helper name: a helper can be renamed or inlined, but a frame cannot be built
    // without naming the request type that goes in it.
    const ACCOUNT_REQ: &str = concat!("Account", "Request");
    const ACCOUNT_VERB: &str = concat!("Account", "Verb");
    const ACCOUNT_LIST: &str = concat!("WireAccount", "List");
    const ACCOUNT_WRITTEN: &str = concat!("WireAccount", "Written");
    for needle in [ACCOUNT_REQ, ACCOUNT_VERB, ACCOUNT_LIST, ACCOUNT_WRITTEN] {
        assert!(
            !THIS_FILE.contains(needle),
            "cmd/mcp.rs (or a child of it) names `{needle}` — the MCP surface may not reach the account-\
                 administration verbs. They carry a credential VALUE and write the settings \
                 database on the DAEMON's box, which is a grant larger than an order; \
                 docs/decisions/0036 reason 4 is what an agent holding it would have to argue \
                 against, and docs/decisions/0065 §2 names this assertion as the half of that \
                 fence a wire verb needs."
        );
    }
    // ⚠ **Non-vacuity, and the first attempt at it FAILED THIS VERY ASSERTION** — which is
    // worth recording, because it is the same self-scanning trap the routing half above
    // documents, one level further in. That attempt was a
    // `std::any::type_name::<…>()` turbofish per needle: a stronger floor in principle (it
    // asks the COMPILER for the spelling), and impossible in practice, because naming the type
    // in this file is precisely what the loop above forbids. The needles went red against
    // their own proof.
    //
    // The floor reads the DEFINING FILE instead. It puts no forbidden spelling in this file —
    // an `include_str!` path is not a type name — and it still fails loudly if a needle stops
    // naming anything, which is how a scan like this otherwise dies quietly. A MOVE of that
    // file is a compile error here rather than a silent pass, which is the right failure
    // direction for a fence.
    const WIRE_TYPES: &str = include_str!("../../../vike-tradehub-client/src/wire.rs");
    for needle in [ACCOUNT_REQ, ACCOUNT_VERB, ACCOUNT_LIST, ACCOUNT_WRITTEN] {
        assert!(
            WIRE_TYPES.contains(needle),
            "the needle `{needle}` no longer names anything in the wire crate — re-key it, or \
                 this half of the fence is scanning for a name nothing has"
        );
    }

    // …and the THIRD half, added with the `instructions` field: a second free-text channel that
    // reaches the model, which the two scans above cannot see. `instructions` is not a tool, so
    // `0036`'s reopen clause ("an MCP TOOL that writes, or reaches, a credential") does not
    // cover it — which is exactly why it needs its own line here rather than an assumption.
    //
    // ⚠ The SUBJECT×ACT pairing above CANNOT be reused on this text, and the attempt is
    // instructive: `<project>/settings/secrets.env` pairs the subject `secret` with the act
    // `set` — inside the word `settings` — so the crude rule would refuse the surface for
    // naming the store's real path. Substring matching is right for a tool NAME and a one-line
    // description written to a house style; it is wrong for prose. The rule here is the precise
    // one instead: every `secrets` subcommand this text names must be a READ.
    const SECRETS_READ_VERBS: [&str; 2] = ["list", "path"];
    for text in [
        instructions(&ToolAccess::full()),
        instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new())),
    ] {
        for command in backticked_commands(&text) {
            let mut words = command.split_whitespace();
            if words.next() != Some("vike-cli") || words.next() != Some("secrets") {
                continue;
            }
            let sub = words.next().unwrap_or_default();
            assert!(
                SECRETS_READ_VERBS.contains(&sub),
                "the MCP instructions name `vike-cli secrets {sub}`, which is not one of the \
                     READ subcommands {SECRETS_READ_VERBS:?}. The instructions are text an agent \
                     acts on, and this surface directs the operator to READ a credential and never \
                     to write one — see docs/decisions/0036, whose four reasons a credential write \
                     reachable from an agent's context would have to argue against."
            );
        }
        // …and the POSITIVE half, because an omission is what an agent fills in with a guess:
        // the text must SAY this server cannot reach a credential at all.
        assert!(
            text.contains(INSTRUCTIONS_NO_CREDENTIAL),
            "the MCP instructions must state outright that this server reaches no credential \
                 — an omission is what an agent fills in with a guess, and the store's path reads \
                 as an invitation without it"
        );
    }
}

/// A `#[cfg(test)]` helper, not a parser: every command the instructions name is inside
/// backticks, so the spans ARE the commands and nothing has to guess where one ends.
fn backticked_commands(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

/// A `#[cfg(test)]` helper, not a build system: every `.rs` file under each repo-relative
/// root, read OFF DISK so that a file added tomorrow is walked without anybody remembering to
/// list it. Roots resolve from `CARGO_MANIFEST_DIR` rather than the working directory, because
/// `cargo test -p vike-cli` sets the CWD to the crate directory and a workspace run does not.
fn rs_sources_under(roots: &[&str]) -> Vec<(String, String)> {
    let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/vike-cli has a parent")
        .to_path_buf();
    let mut out = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = roots.iter().map(|r| crates_dir.join(r)).collect();
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("this scan must be able to read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("this scan must be able to read {}: {e}", path.display())
                });
                let rel = path.strip_prefix(&crates_dir).unwrap_or(&path).display().to_string();
                out.push((rel.replace('\\', "/"), text));
            }
        }
    }
    out
}

/// **THE FIFTH EVIDENCE: NO HELPER LETS THIS SURFACE REACH THE ACCOUNT VERBS.**
///
/// A SEPARATE `#[test]`, and that is a measurement rather than a style: the four evidences in
/// `the_mcp_surface_advertises_no_credential_writer` have to be able to stay GREEN while this
/// one goes RED. That is the whole finding of `docs/decisions/0065-accounts-are-managed-and-
/// the-barrier-is-declared.md` §2 — plant the helper it warns about and the fence above does
/// not move — and a fifth leg folded into that test would have hidden the finding behind one
/// test name.
///
/// ⚠ **The hole this closes is inside the FOURTH evidence's own argument.** That one keys on
/// the wire TYPE NAMES appearing in this file, and states why: *"a helper can be renamed or
/// inlined, but a frame cannot be built without naming the request type that goes in it."*
/// True of the frame; false of the CALLER, which is the half that decides what an agent can
/// reach. `crates/vike-tradehub-client/src/remote_control.rs`'s `set_setting` is the standing
/// proof, and this file already calls it: a public helper taking `(addr, control_key, file,
/// key, value, confirm, reason)` — every one a primitive — which builds its command INSIDE
/// itself, so `Server::execute_settings_write` reaches a node-side settings write while naming
/// no wire type at all. An account twin written to that same template, say
/// `set_account_credential(addr, admin_key, key, value, confirm)`, is callable from a tool
/// handler with all four needles above still absent from this file — and the credential is
/// written on the DAEMON's box, so `crates/vike-ops/tests/credential_writer_gate.rs` stays
/// green through it as well.
///
/// So this evidence follows the frame to where it MUST be built rather than to where it is
/// called, and two facts make that a scan instead of a call graph: a helper that reaches the
/// node has to DIAL it, and a helper that sends an account frame has to NAME one of its types.
/// **No file may do both.** The partition is total today and needs no exception list —
/// `wire.rs`, `proto.rs` and `lib.rs` name the types and dial nothing, while `handshake.rs`,
/// `remote_control.rs`, `remote_handle.rs` and `liveness.rs` dial and name nothing — so a
/// helper added to any of them lands on both sides at once. The roots are walked ON DISK at
/// run time rather than `include_str!`ed for the one shape an `include_str!` list cannot see:
/// a NEW file, which is exactly where somebody adding an account client would put it.
///
/// ⚠ **Scope, stated rather than left to be inferred from a green run.** Two roots: the client
/// crate — the only crate in this tree holding these types AND the handshake — and
/// `vike-cli`'s own `src/`, the crate this file is composed into, where an in-process helper
/// would live. A helper in a THIRD crate that opened its own socket and spoke the protocol by
/// hand would escape, and nothing here would see it. That is a declared residual, not a claim
/// — and it is
/// `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`'s
/// reopen clause firing somewhere no text scan reaches.
#[test]
fn the_account_verbs_are_unreachable_through_a_helper_that_names_no_wire_type() {
    // Split for the self-scanning reason the fourth evidence documents, one level further in:
    // this test walks its OWN file off disk, so a whole spelling written here as test data
    // would report `mcp.rs` as naming an account type and the assertion could never pass.
    const NAMES: [&str; 5] = [
        concat!("Account", "Request"),
        concat!("Account", "Verb"),
        concat!("WireAccount", "List"),
        concat!("WireAccount", "Written"),
        concat!("Request::", "Account"),
    ];
    // The DIAL needles. The client crate's handshake is `pub(crate)` and is its one door onto
    // a node, so every helper there goes through it; the raw connect is named beside it
    // because a new file could open its own socket instead of borrowing that door.
    const DIALS: [&str; 2] = [concat!("node_", "handshake("), concat!("TcpStream", "::connect")];

    let sources = rs_sources_under(&["vike-tradehub-client/src", "vike-cli/src"]);

    // Non-vacuity, three floors, because a scan like this otherwise dies quietly: the walk
    // found a tree, the DIAL needles match something in it, and the NAME needles match
    // something in it. Without the last two, a rename on either side turns the whole evidence
    // green while the capability it guards is wide open.
    assert!(
        sources.len() > 40,
        "the walk found only {} source files — it is not reading the tree",
        sources.len()
    );
    assert!(
        sources.iter().any(|(_, text)| DIALS.iter().any(|d| text.contains(d))),
        "no walked file dials a node, so the needles {DIALS:?} name nothing and this evidence \
             would pass whatever a helper did"
    );
    assert!(
        sources.iter().any(|(_, text)| NAMES.iter().any(|n| text.contains(n))),
        "no walked file names an account wire type — re-key the needles, or this evidence is \
             scanning for names nothing has"
    );

    for (path, text) in &sources {
        let Some(dial) = DIALS.iter().find(|d| text.contains(**d)) else { continue };
        let Some(name) = NAMES.iter().find(|n| text.contains(**n)) else { continue };
        panic!(
            "{path} both dials a node (`{dial}`) and names `{name}`, so it holds — or can \
                 hold — a client helper that reaches the account-administration verbs. The MCP \
                 surface is a first-class client of this same wire from this same binary and can \
                 call such a helper with PRIMITIVES, naming no wire type: \
                 `the_mcp_surface_advertises_no_credential_writer` stays green through that, and \
                 so does crates/vike-ops/tests/credential_writer_gate.rs, because the credential \
                 is written on the DAEMON's box and passes no writer in this process. Those verbs \
                 carry a credential VALUE, which docs/decisions/0036 reason 4 says re-decides that \
                 record from the top, and docs/decisions/0065 §2 is the measurement that this \
                 shape defeats every needle above it. If the helper is genuinely wanted, the fence \
                 has to move onto something the MCP surface cannot call at all — a decision, \
                 argued, and never a needle re-keyed until it is green again."
        );
    }
}

/// **AND THE FLOOR UNDER THE SCOPE CLAIM, which until now was held by nothing at all.**
///
/// `the_mcp_surface_advertises_no_credential_writer`'s fourth evidence names three things that
/// keep this surface off the account plane, and says outright that two of them are "not a
/// property of THIS file": the SCOPE — those verbs want `Scope::Account`, and `vike-cli` holds
/// no verb that presents one — and the ADVERTISEMENT. The advertisement has a test on the
/// daemon's side. The scope had none: it was true by ABSENCE, and adding an `admin` accessor
/// beside `observe` and `control` on `crates/vike-cli/src/cmd/nodekeys.rs`'s `NodeKeyring`
/// would have reddened nothing in this tree. A fence with a second leg that can be removed
/// without a test moving is a fence with one leg, so this is that leg.
#[test]
fn the_cli_keyring_cannot_present_the_admin_scope() {
    // The keyring this binary hands every node-dialling verb, the MCP server included. Two
    // accessors, and the third scope deliberately has none.
    const KEYRING: &str = include_str!("nodekeys.rs");
    const ADMIN_ACCESSOR: &str = concat!("fn ", "admin");
    assert!(
        !KEYRING.contains(ADMIN_ACCESSOR),
        "cmd/nodekeys.rs declares `{ADMIN_ACCESSOR}` — the CLI's keyring may not present the \
             admin scope. It is one of the two things the fourth evidence of \
             `the_mcp_surface_advertises_no_credential_writer` names as keeping the agent surface \
             off the account-administration verbs, and the MCP server is handed this same keyring."
    );
    // Non-vacuity: the shape really does match the accessors that ARE there, so a refactor of
    // the keyring cannot turn the refusal above silently green.
    for present in [concat!("fn ", "observe"), concat!("fn ", "control")] {
        assert!(
            KEYRING.contains(present),
            "the needle shape no longer matches this keyring's real accessors — re-key it, or \
                 the refusal above is scanning for a spelling nothing uses"
        );
    }

    // …and the CONSTRUCTOR half, because an accessor is not the only door: a `NodeKeys` gains
    // an admin key from exactly one function, whose one caller in this workspace is the DAEMON
    // reading its own store. No file of this crate may call it.
    const ADMIN_KEYS: &str = concat!("from_vars_", "with_admin(");
    for (path, text) in rs_sources_under(&["vike-cli/src"]) {
        assert!(
            !text.contains(ADMIN_KEYS),
            "{path} calls `{ADMIN_KEYS}` — this crate composes the MCP surface, and an admin \
                 key held in THIS process is what the account-administration verbs authenticate \
                 against. docs/decisions/0065 §2's scope split is the claim this refusal holds."
        );
    }
    assert!(
        include_str!("../../../vike-tradehub-client/src/auth.rs").contains(ADMIN_KEYS),
        "the needle `{ADMIN_KEYS}` no longer names the admin-key constructor — re-key it, or \
             this half is scanning for a name nothing has"
    );
}

/// Rust CODE with `//` comments removed — a doc mention is prose, not a reach.
///
/// ⚠ **Every other evidence in this family scans whole files, comments included, and that is a
/// measured defect rather than a nuance.** An adversarial review of the fifth evidence went red
/// against a DOC COMMENT the reviewer had just written, and `crates/vike-cli/src/cmd/mcp.rs`
/// sits on the dialing side of that test's partition today ONLY because its module doc mentions
/// `TcpStream::connect`. A fence that a sentence can trip is a fence that teaches authors to
/// stop writing sentences. Quote-aware so a `//` inside a string literal is not mistaken for a
/// comment.
fn rust_code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut prev_slash = false;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'"' => {
                in_string = !in_string;
                prev_slash = false;
            }
            b'/' if !in_string => {
                if prev_slash {
                    return &line[..i - 1];
                }
                prev_slash = true;
            }
            _ => prev_slash = false,
        }
    }
    line
}

/// The sites that legitimately name the admin scope, each with the reason it is not a reach.
///
/// ⚠ **There are none in these two crates now, and that is the intended end state.** The two
/// sites that used to be pinned here — the ping report's `Scope -> &str` and the client crate's
/// twin of it beside the handshake — were exhaustive `match` arms turning a scope into a WORD
/// (a label authorises nothing). They were the same body twice, and it now lives once, as
/// `Scope::name` in `vike-node-proto`, which this scan does not read: neither crate names
/// `Scope::Account` in code any more.
///
/// A site is not absorbed by adding a row. The refusal below says what to do instead, and says
/// it because an exception list that grows on contact is the shape of gate this tree has watched
/// rot elsewhere.
const SCOPE_LABEL_SITES: [&str; 0] = [];

/// **The sixth evidence, and it follows the AUTHORIZATION rather than a type name.**
///
/// ⚠ **This exists because an adversarial review defeated the fifth twice, with compiling
/// code, while every other evidence here stayed green.** Both evasions are recorded because the
/// shape of each is the argument:
///
/// * **A JSON literal.** This wire is length-prefixed `serde_json`, so a frame is a STRING —
///   `serde_json::from_value(json!({"Account": {"verb": {"SetCredential": …}}}))` builds a
///   `Request` naming no account type at all, and was proved byte-equal to the typed frame.
///   The fourth evidence's *"a frame cannot be built without naming the request type that goes
///   in it"* is true of the FRAME and false of the CALLER.
/// * **A split across the partition's own poles.** The fifth evidence reasons that no file both
///   dials and names. True — and *"no file may do both"* is not *"no pair of files may"*. A
///   constructor in `proto.rs` (names, never dials) plus a primitives-only sender in
///   `remote_control.rs` (dials, names nothing) is idiomatic, compiles, and passes everything.
///
/// **What both must do, and cannot delegate, is PRESENT THE SCOPE.** An account verb is refused
/// by `crates/vike-tradehub/src/server/accounts.rs`'s `account_admission` under anything but
/// `Scope::Account`, so a reach that works has `Scope::Account` in the file that authenticates. That
/// is a property of the authorization rather than of a spelling, which is why this evidence
/// keys on it.
///
/// ⚠ **It still does not hold the CLASS, and saying so is the point.** `Scope::Account` is itself
/// a name: `Scope` derives `Deserialize`, so a JSON literal or an integer tag reaches the same
/// variant naming nothing. **No text scan can hold this property, because the wire is data and
/// a scan reads code.** What would hold it is structural and is a decision rather than a test —
/// `docs/decisions/0036`'s *What would reopen this* clause is where it belongs, and the
/// candidates measured during that review were: make the account plane unreachable from this
/// binary at the CRATE graph; seal the frame type so a literal cannot be deserialized into it
/// from outside; or move the barrier onto the DAEMON, which is the only side that sees what it
/// is actually being asked to do. Until one is taken, this family is ADVISORY — it raises the
/// cost of an accident and stops none of the three evasions above on purpose.
#[test]
fn a_dialing_file_does_not_present_the_admin_scope() {
    const ADMIN_SCOPE: &str = concat!("Scope", "::Account");
    let mut offenders = Vec::new();
    let mut seen_labels: Vec<&str> = Vec::new();
    for (path, text) in rs_sources_under(&["vike-cli/src", "vike-tradehub-client/src"]) {
        let reaches: Vec<&str> =
            text.lines().map(rust_code_of).filter(|code| code.contains(ADMIN_SCOPE)).collect();
        if reaches.is_empty() {
            continue;
        }
        if let Some(site) = SCOPE_LABEL_SITES.iter().find(|s| path.ends_with(*s)) {
            seen_labels.push(site);
            continue;
        }
        offenders.push(format!("  {path}: {}", reaches.join(" | ")));
    }
    for site in SCOPE_LABEL_SITES {
        assert!(
            seen_labels.contains(&site),
            "the pinned label site {site} no longer names `{ADMIN_SCOPE}` in CODE — either it \
                 moved, or `rust_code_of` has started eating real lines. Either way this test is \
                 scanning for something nothing has, which is the failure mode it exists to avoid \
                 in others."
        );
    }
    assert!(
        offenders.is_empty(),
        "these files present `{ADMIN_SCOPE}` in code, and an account verb is refused under \
             every other scope — so a reach that WORKS has it here:\n{}\n\nIf this is a legitimate \
             new client, it is not this test that decides: docs/decisions/0036 fences the MCP \
             surface from credential writes and `docs/decisions/0065` §2 owes the scope split. Add \
             a pinned site with the argument, or take one of the structural answers this test's \
             own doc names — do not widen the needle.",
        offenders.join("\n")
    );
}

#[test]
fn initialize_carries_instructions_that_name_the_surface_beyond_this_one() {
    let mut s = server();
    let init = s.handle(&req(1, "initialize", json!({}))).unwrap();
    let text = init["result"]["instructions"].as_str().expect(
        "initialize must carry an `instructions` string — it is the ONE channel that can say \
             what this surface is NOT, and a client shows it to the model",
    );
    // The four capabilities that live outside this server, each measured as MISSING by the
    // 2026-09-06 model run. Named here rather than derived because the LIST is the editorial
    // decision this text exists to make; that each one is REAL is what the next test derives.
    for needle in [
        "vike-backend datahub --record",
        "vike-cli data hist fetch",
        // ⚠ `backtest run --local`, not `backtest --local`: decision 11 of the
        // backtest-CLI-surface design made the sub-verb mandatory, so the older spelling is
        // now an exit-2. A needle that stopped at `vike-cli backtest` would have kept passing
        // over the broken invocation, which is why this one carries the sub-verb.
        "vike-cli backtest run --local",
        // ⚠ …and the datahub it reads through. The bullet said "backtest with no server" until
        // decision 0084's 2026-09-25 amendment, and a needle on `--local` alone kept passing
        // over that false promise — the half that makes the command WORK on a box with files
        // and no server is this one.
        "VIKE_DATAHUB_STORE=DIR vike-backend datahub",
    ] {
        assert!(text.contains(needle), "the instructions must name `{needle}`");
    }
    assert!(
        !text.contains("no server"),
        "the instructions promise a run with no server, and since decision 0084's 2026-09-25 \
             amendment every history read goes through a datahub — `--local` moves the engine, \
             not the history"
    );
    assert!(text.contains("settings/secrets.env"), "the instructions must name the ONE store");
}

#[test]
fn the_instructions_are_scoped_by_the_same_access_the_roster_is() {
    // The write clause and the `list_series` clause are ADVERTISEMENTS of tools. A profile that
    // withholds the tool must withhold the sentence, or the prose is advertising what
    // `tools/list` refuses — the exact drift [`ToolAccess`] exists to make impossible.
    let full = instructions(&ToolAccess::full());
    assert!(full.contains("PREVIEW-GATED"), "`full` serves the write tools and must say so");
    assert!(full.contains("list_series"), "`full` serves the datahub reads and may say so");

    let read_only = instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new()));
    assert!(
        !read_only.contains("PREVIEW-GATED"),
        "`read-only` serves no write tool, so the preview gate is a door the agent cannot reach"
    );
    assert!(read_only.contains("list_series"), "`read-only` still serves the datahub reads");

    let offline = instructions(&ToolAccess::new(Profile::Offline, Vec::new()));
    assert!(!offline.contains("PREVIEW-GATED"), "`offline` serves no write tool");
    assert!(
        !offline.contains("list_series"),
        "`offline` withholds every network tool, `list_series` included — telling an agent to \
             confirm a fetch with it is advertising a withheld tool in prose"
    );

    // What NO profile may drop: the operator-side commands. They are things a HUMAN types, and
    // no scoping of THIS server changes what the operator can run on the box.
    for (name, text) in [("full", &full), ("read-only", &read_only), ("offline", &offline)] {
        assert!(
            text.contains("vike-backend datahub --record") && text.contains("settings/secrets.env"),
            "the `{name}` profile dropped an OPERATOR-side command; the profile scopes this \
                 server's tools, not the operator's terminal"
        );
    }
}

#[test]
fn the_instructions_name_only_real_commands() {
    // The rot the whole repository is organised against: prose that names a command the binary
    // does not have. It is worse than naming none — it sends an operator to a terminal to type
    // something that fails, on the word of the surface itself.
    //
    // Each `vike-cli` verb is checked against `crate::COMMANDS`, the dispatcher's own roster,
    // and each SUBCOMMAND and FLAG against the owning module's own `USAGE`. Neither is a copy.
    //
    // ⚠ Two bounds, declared rather than implied. Arguments are NOT checked (`run.toml`,
    // `binance:BTCUSDT:1h` and `180` are values an operator supplies, not names this workspace
    // owns), and a NON-`vike-cli` binary is not checked here at all — this crate cannot see
    // another crate's manifest, so `crates/vike-ops/tests/mcp_instructions_gate.rs` holds
    // `vike-backend` to a real `[[bin]]` from outside.
    fn usage_for(verb: &str) -> Option<&'static str> {
        match verb {
            "data" => Some(crate::cmd::data::USAGE),
            "secrets" => Some(crate::cmd::secrets::USAGE),
            "backtest" => Some(crate::cmd::backtest::USAGE),
            _ => None,
        }
    }

    let verbs: Vec<&str> = crate::COMMANDS.iter().map(|(name, _)| *name).collect();
    let mut checked = 0usize;
    for access in [ToolAccess::full(), ToolAccess::new(Profile::Offline, Vec::new())] {
        let text = instructions(&access);
        for command in backticked_commands(&text) {
            let mut words = command.split_whitespace();
            if words.next() != Some("vike-cli") {
                continue;
            }
            // A span that is JUST the binary name (`vike-cli`, naming the product rather than
            // invoking it) carries no verb to check. It is not unchecked: the BINARY half is
            // `crates/vike-ops/tests/mcp_instructions_gate.rs`'s, from where a manifest is
            // visible.
            let Some(verb) = words.next() else { continue };
            assert!(
                verbs.contains(&verb),
                "the MCP instructions name `vike-cli {verb}`, which is not a registered \
                     subcommand. The dispatcher's roster is {verbs:?}."
            );
            let usage = usage_for(verb).unwrap_or_else(|| {
                panic!(
                    "the MCP instructions name `vike-cli {verb}` and this test has no `USAGE` \
                         to hold its subcommands and flags to — add the arm, do not delete the \
                         check"
                )
            });
            for word in words {
                // Values are the operator's, not ours (see the bound above).
                if !word.starts_with("--") && (word.contains(['.', ':']) || word.starts_with('$')) {
                    continue;
                }
                if word.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                assert!(
                    usage.contains(word),
                    "the MCP instructions name `{word}` under `vike-cli {verb}`, which that \
                         command's own USAGE does not offer"
                );
                checked += 1;
            }
        }
    }
    // Non-vacuity: a text whose backtick spans stopped parsing would pass every assertion above
    // by checking nothing at all.
    assert!(checked >= 4, "only {checked} subcommand/flag(s) were checked — the parse broke");
}

#[test]
fn the_server_serves_prompts_and_each_one_renders() {
    let mut s = server();
    let init = s.handle(&req(0, "initialize", json!({}))).unwrap();
    assert!(
        init["result"]["capabilities"]["prompts"].is_object(),
        "initialize must declare the prompts capability once prompts/list answers"
    );
    let listed = s.handle(&req(1, "prompts/list", json!({}))).unwrap();
    let names: Vec<&str> = listed["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["backtest_a_strategy", "arm_a_venue", "triage_a_stuck_order"]);
    for name in names {
        // With NO arguments — the shape a human browsing a client's prompt menu gets. Every
        // argument this server declares is optional, so this must render rather than error.
        let got = s.handle(&req(2, "prompts/get", json!({ "name": name }))).unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(!text.is_empty(), "{name} rendered an empty prompt");
        assert_eq!(got["result"]["messages"][0]["role"], "user", "{name}");
        assert_eq!(
            got["result"]["description"],
            prompt_description(name),
            "{name}: prompts/get must echo the description prompts/list advertised"
        );
    }
    let bogus = s.handle(&req(3, "prompts/get", json!({ "name": "nope" }))).unwrap();
    assert_eq!(bogus["error"]["code"], -32602, "got {bogus}");
}

#[test]
fn every_write_touching_prompt_teaches_the_token_not_just_confirm() {
    // ⚠ THE FAILURE THIS PINS. The published pages describing this surface said `confirm: true`
    // was the whole gate — true before the token binding landed, false since. A prompt written
    // from one of those pages teaches an agent a flow that is REFUSED on its second call, with
    // a message about a token the prompt never mentioned. So every prompt that touches a write
    // tool must name the token, its single use and its binding.
    let mut s = server();
    for name in ["arm_a_venue", "triage_a_stuck_order"] {
        let got = s.handle(&req(1, "prompts/get", json!({ "name": name }))).unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        for needle in ["preview_token", "confirm: true", "at most ONCE", "BOUND to the command"] {
            assert!(text.contains(needle), "{name}'s prompt never says {needle:?}");
        }
        assert!(
            text.contains("verified_by_node"),
            "{name}'s prompt must tell the agent to read the node's verdict, not the estimate"
        );
        // ...and the ROSTER, name by name. `two_call_gate` renders it from the write tools this
        // session ADMITS — which under the default `full` profile is `WRITE_TOOLS` entire; this
        // is what keeps it rendered. Spelled out by hand it was a FOURTH copy of the roster
        // held equal to nothing — an eighth write tool joins the routing, the `destructiveHint`
        // annotations and the transcript harness by construction, and would be missing from
        // exactly the sheet an agent reads before calling it ONCE and believing it executed.
        for tool in WRITE_TOOLS {
            assert!(text.contains(tool), "{name}'s prompt never names the write tool {tool:?}");
        }
        // The window is derived too, for the same reason one notch smaller: retuning
        // `PREVIEW_WINDOW` must not leave the teaching sheet quoting the old number.
        let window = format!("expires {} seconds", PREVIEW_WINDOW.as_secs());
        assert!(text.contains(&window), "{name}'s prompt must say {window:?}");
    }
    // The backtest flow reaches no write tool at all, and must not pretend otherwise — an
    // agent told to confirm something during a backtest looks for a gate that is not there.
    let bt = s.handle(&req(2, "prompts/get", json!({ "name": "backtest_a_strategy" }))).unwrap();
    let text = bt["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(!text.contains("preview_token"), "the backtest flow places no orders");
}

#[test]
fn a_prompt_argument_reaches_the_rendered_text() {
    // The arguments are advertised, so they must do something — an advertised knob that
    // changes nothing is the same defect class as a settings key nothing reads.
    let mut s = server();
    let got = s
        .handle(&req(
            1,
            "prompts/get",
            json!({ "name": "triage_a_stuck_order", "arguments": { "client_order_id": "c-42" } }),
        ))
        .unwrap();
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("c-42"), "the coid argument must reach the text: {text}");
}

#[test]
fn the_server_advertises_resources_and_lists_them() {
    let mut s = server();
    let init = s.handle(&req(1, "initialize", json!({}))).unwrap();
    assert!(
        init["result"]["capabilities"]["resources"].is_object(),
        "initialize must declare the resources capability once resources/list answers — a \
             client that does not see it never calls the method"
    );
    let listed = s.handle(&req(2, "resources/list", json!({}))).unwrap();
    let uris: Vec<&str> = listed["result"]["resources"]
        .as_array()
        .expect("resources/list returns an array")
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"vike://node/snapshot"), "got {uris:?}");
    assert!(uris.contains(&"vike://backtest/last"), "got {uris:?}");
    // Every advertised resource must carry the two fields a client renders it by. A URI with
    // no name is a row a human picks blind.
    for r in listed["result"]["resources"].as_array().unwrap() {
        assert!(r["name"].as_str().is_some_and(|n| !n.is_empty()), "{r} has no name");
        assert_eq!(r["mimeType"], "application/json", "{r}");
    }
}

#[test]
fn an_unread_resource_is_an_error_rather_than_an_empty_document() {
    // ⚠ The distinction this pins. An agent handed `{}` for `vike://backtest/last` would
    // summarise it as a backtest that produced nothing — a different claim from "no backtest
    // has run". Same for an unknown URI: a silent empty read is a lie an agent cannot detect.
    let mut s = server();
    let miss =
        s.handle(&req(1, "resources/read", json!({ "uri": "vike://backtest/last" }))).unwrap();
    assert_eq!(miss["error"]["code"], -32002, "got {miss}");
    assert!(
        miss["error"]["message"].as_str().unwrap().contains("no backtest has run"),
        "the error must say WHICH absence it is: {miss}"
    );

    let bogus = s.handle(&req(2, "resources/read", json!({ "uri": "vike://nope" }))).unwrap();
    assert_eq!(bogus["error"]["code"], -32002, "got {bogus}");

    // The node snapshot with no `--node` is the same shape — an error naming the missing
    // configuration, never an empty snapshot an agent would read as a flat account.
    let no_node =
        s.handle(&req(3, "resources/read", json!({ "uri": "vike://node/snapshot" }))).unwrap();
    assert_eq!(no_node["error"]["code"], -32002, "got {no_node}");
    assert!(no_node["error"]["message"].as_str().unwrap().contains("--node"), "got {no_node}");
}

#[test]
fn the_last_backtest_resource_serves_what_the_tool_returned() {
    // The resource is a second WAY IN to the tool's answer, not a second source of it — so
    // what it serves must be BYTE-IDENTICAL to the tool's own rendering. There is no datahub
    // here, so drive the recording seam directly rather than pretending a run happened.
    let mut s = server();
    let report = json!({ "report": { "sharpe": 1.25, "trades": 42 } });
    s.last_backtest = Some(render(&report));
    let got =
        s.handle(&req(1, "resources/read", json!({ "uri": "vike://backtest/last" }))).unwrap();
    let contents = &got["result"]["contents"][0];
    assert_eq!(contents["uri"], "vike://backtest/last");
    assert_eq!(contents["mimeType"], "application/json");
    assert_eq!(
        contents["text"].as_str().unwrap(),
        tool_ok(report.clone())["content"][0]["text"].as_str().unwrap(),
        "the resource and the tool must render the same report identically"
    );
}

/// THE §15.1 ONE-ROSTER GATE'S AGENT LEG. A model can only ask for a method the SCHEMA names,
/// so a roster that does not reach `tools_spec` leaves every agent a grid user — which is the
/// exact consequence §12 of the backtest-CLI-surface design says this stage exists to end.
#[test]
fn the_run_sweep_schema_advertises_the_whole_method_roster() {
    let spec = tools_spec();
    let tool = spec
        .as_array()
        .expect("tools_spec is an array")
        .iter()
        .find(|t| t["name"] == "run_sweep")
        .expect("run_sweep is served");
    let enumerated =
        tool["inputSchema"]["properties"]["optimizer"]["enum"].as_array().expect("an enum");
    let names: Vec<&str> = enumerated.iter().map(|v| v.as_str().expect("a string")).collect();
    assert_eq!(
        names,
        vike_datahub_client::SEARCH_METHODS.to_vec(),
        "the tool schema and the protocol must name one roster"
    );
    for knob in ["trials", "seed", "euler_depth"] {
        assert!(
            tool["inputSchema"]["properties"][knob].is_object(),
            "the agent must be able to pass {knob}"
        );
    }
}

/// A numeric argument arrives from a model as a JSON NUMBER, and the wire carries TOKENS — so
/// the boundary renders it. A string is accepted too (a model that quoted it is not wrong), and
/// anything else is refused BY NAME rather than silently dropped, which is what
/// `Value::as_str` alone would have done.
#[test]
fn a_numeric_tool_argument_is_rendered_and_a_bad_one_is_named() {
    let search = search_from_args(&json!({ "trials": 128, "optimizer": "tpe" }))
        .expect("a number is accepted")
        .expect("a selector was built");
    assert_eq!(search.trials.as_deref(), Some("128"));
    assert_eq!(search.optimizer.as_deref(), Some("tpe"));

    let quoted = search_from_args(&json!({ "seed": "7" })).expect("a string is accepted too");
    assert_eq!(quoted.expect("a selector").seed.as_deref(), Some("7"));

    let err = search_from_args(&json!({ "trials": [1, 2] })).expect_err("a list is refused");
    assert!(err.contains("trials"), "the refusal names the argument: {err}");
}

/// No search argument at all builds NO selector, so an ordinary agent grid search ships the
/// frame it always shipped — and reaches a daemon that predates the capability unchanged.
#[test]
fn a_sweep_with_no_method_argument_builds_no_selector() {
    assert!(
        search_from_args(&json!({ "profile": "[data]\n" })).expect("ok").is_none(),
        "an ordinary grid search must not start negotiating a capability"
    );
}
#[test]
fn tools_list_has_read_and_write_tools_with_correct_hints() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"run_backtest") && names.contains(&"submit_order"));
    // The absorbed vike-mcp read surface + the closed write-verb drift are all advertised.
    for n in [
        "validate_strategy",
        "discover_params",
        "list_templates",
        "list_series",
        "run_sweep",
        "run_walk_forward",
        "modify",
        "mass_cancel",
    ] {
        assert!(names.contains(&n), "{n} must be advertised in tools/list");
    }
    for t in tools {
        let n = t["name"].as_str().unwrap();
        if WRITE_TOOLS.contains(&n) {
            assert_eq!(t["annotations"]["destructiveHint"], true, "{n} must be destructive");
            // The mandatory-preview gate is part of each write tool's advertised contract.
            let desc = t["description"].as_str().unwrap();
            assert!(desc.contains("confirm"), "{n} must document the preview gate: {desc}");
        } else {
            assert_eq!(t["annotations"]["readOnlyHint"], true, "{n} must be read-only");
        }
    }
}

#[test]
fn discover_params_tool_returns_the_declared_knobs() {
    let resp = call(
        "discover_params",
        json!({ "script": "let qty = param(\"qty\", 2.5);\nfn on_bar() {}" }),
    );
    assert_eq!(resp["result"]["isError"], false);
    let params = resp["result"]["structuredContent"]["params"].as_array().unwrap();
    assert_eq!(params[0]["name"], "qty");
    assert_eq!(params[0]["default"], 2.5);
}

#[test]
fn list_indicators_returns_exactly_the_host_bound_set() {
    // The tool must advertise ONLY what the Rhai host binds (vike_script::RHAI_INDICATORS),
    // never the whole `vike_indicators::registry()` — an agent acts on this list, and any
    // unbound name it is handed produces a script that silently self-disables. Both sides read
    // the SAME derived list, so this cannot pass while the tool under-reports either.
    let resp = call("list_indicators", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    let inds = resp["result"]["structuredContent"]["indicators"].as_array().unwrap();
    let mut names: Vec<&str> = inds.iter().map(|i| i["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    // The union, for the reason spelled out on `is_callable`: an indicator reachable only
    // through `bollinger_mid(20)` is still an indicator this tool must offer.
    let mut expected: Vec<&str> = vike_indicators::registry()
        .iter()
        .map(|m| m.name)
        .filter(|n| vike_script::is_callable(n))
        .collect();
    expected.sort_unstable();
    assert_eq!(names, expected, "list_indicators must be the Rhai host-bound set");
    assert_eq!(resp["result"]["structuredContent"]["count"], names.len());
    // ⚠ The DEFAULT response stays compact: name + category, and no per-indicator detail. The
    // roster is the whole catalog now, so a default that carried every parameter and output
    // line would spend an agent's context on indicators it never asked about.
    for i in inds {
        assert!(i["category"].as_str().is_some_and(|s| !s.is_empty()), "{i}");
        assert!(i.get("params").is_none(), "the default roster must stay compact: {i}");
    }
}

/// Detail is PULLED, per name or per family — the other half of the tiering above.
#[test]
fn list_indicators_narrows_by_name_and_by_category() {
    // Derived: whatever the host binds first, never a hard-coded name.
    let first = vike_script::RHAI_INDICATORS.first().expect("the host binds something");
    let one = call("list_indicators", json!({ "name": first }));
    assert_eq!(one["result"]["isError"], false);
    let rows = one["result"]["structuredContent"]["indicators"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], *first);
    // The two fields a compact roster CANNOT carry, and the reason detail exists at all: a
    // multi-parameter, multi-output indicator is indistinguishable from a simple one without
    // them.
    assert!(rows[0]["params"].is_array(), "a detail row must carry its parameters");
    assert!(rows[0]["outputs"].as_array().is_some_and(|o| !o.is_empty()));
    let category = rows[0]["category"].as_str().unwrap().to_string();

    let family = call("list_indicators", json!({ "category": category.to_lowercase() }));
    assert_eq!(family["result"]["isError"], false, "the category match is case-insensitive");
    let fam = family["result"]["structuredContent"]["indicators"].as_array().unwrap();
    assert!(fam.iter().any(|r| r["name"] == *first));
    assert!(fam.iter().all(|r| r["category"] == category.as_str()));

    // A name nobody can call is an ERROR, not an empty list: an empty answer reads as "that
    // family is empty in this build", which sends somebody hunting through a config.
    let miss = call("list_indicators", json!({ "name": "sma_typo" }));
    assert_eq!(miss["result"]["isError"], true);
    let bad = call("list_indicators", json!({ "category": "not-a-category" }));
    assert_eq!(bad["result"]["isError"], true);
    assert!(bad["result"]["content"][0]["text"].as_str().unwrap().contains(&category));
}

/// A registry indicator the host HOLDS BACK answers with the host's own REASON. An agent that
/// reached for one — they are real indicator names, and a model has seen them — otherwise gets
/// "unknown", re-checks its spelling, and tries again with the same name.
///
/// Derived on every axis: which name is held back, and what the reason says, both come from
/// `vike-script`. A build binding the entire registry has nothing to explain and skips — stated
/// rather than silent, because a vacuous pass should be readable in the test, not inferred.
#[test]
fn list_indicators_says_why_a_registry_name_is_not_callable() {
    // ⚠ NOT `!RHAI_INDICATORS.contains(..)` any more. That set is the BARE names, and since
    // per-line accessors landed a name can be absent from it and still perfectly callable —
    // `bollinger` is. A genuinely held-back indicator is one no spelling reaches, which is
    // exactly what `is_callable` answers.
    let held = vike_indicators::registry().iter().find(|m| !vike_script::is_callable(m.name));
    let Some(held) = held else {
        return; // nothing is held back in this build
    };
    let resp = call("list_indicators", json!({ "name": held.name }));
    assert_eq!(resp["result"]["isError"], true, "a name a script cannot call is not an answer");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let why = vike_script::unbound_reason(held.name)
        .expect("a held-back registry indicator must carry a reason");
    assert!(text.contains(why), "the tool must quote the host's own reason: {text}");
}

/// ⚠ The `tools/list` description ships in EVERY session, whether the tool is called or not, so
/// it must NOT enumerate the roster. It used to — `RHAI_INDICATORS.join(", ")` was spliced in,
/// which cost 13 characters while the host bound three names and would cost kilobytes of every
/// agent's context now that it binds the catalog.
#[test]
fn list_indicators_description_does_not_enumerate_the_set() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let tool = tools.iter().find(|t| t["name"] == "list_indicators").unwrap();
    let desc = tool["description"].as_str().unwrap();
    assert!(desc.contains("HOST-BOUND"), "{desc}");
    assert!(
        !desc.contains(&vike_script::RHAI_INDICATORS.join(", ")),
        "the description must not carry the roster: {desc}"
    );
    // A FIXED ceiling, deliberately independent of the set: the point is that this text cannot
    // grow when an indicator is added.
    const MAX_DESCRIPTION: usize = 700;
    assert!(
        desc.len() <= MAX_DESCRIPTION,
        "the tools/list description is {} bytes — it ships in every session and must stay \
             bounded; describe the tool, do not list its answer",
        desc.len()
    );
    // ...and it must tell the agent how to GET the set, or bounding it just hid the answer.
    assert!(desc.contains("category") && desc.contains("name"), "{desc}");
}

#[test]
fn a_panicking_resource_read_is_an_rpc_error_not_a_dead_session() {
    // The twin of the tool pin below, for the second way in. A resource has no tool-result
    // envelope, so the caught panic surfaces as a JSON-RPC error — and the session goes on:
    // the SAME server answers the next request.
    let mut s = server();
    let resp =
        s.handle(&req(1, "resources/read", json!({ "uri": "vike://__test_panic" }))).unwrap();
    assert_eq!(resp["error"]["code"], -32603, "{resp}");
    assert!(resp["error"]["message"].as_str().unwrap().contains("panicked"), "{resp}");
    let next = s.handle(&req(2, "ping", json!({}))).unwrap();
    assert!(next.get("result").is_some(), "the session must survive a caught panic: {next}");
}

#[test]
fn a_panicking_prompt_render_is_an_rpc_error_not_a_dead_session() {
    let mut s = server();
    let resp = s.handle(&req(1, "prompts/get", json!({ "name": "__test_panic" }))).unwrap();
    assert_eq!(resp["error"]["code"], -32603, "{resp}");
    assert!(resp["error"]["message"].as_str().unwrap().contains("panicked"), "{resp}");
    let next = s.handle(&req(2, "ping", json!({}))).unwrap();
    assert!(next.get("result").is_some(), "the session must survive a caught panic: {next}");
}

#[test]
fn a_panicking_tool_becomes_iserror_not_a_dead_session() {
    // Mirrors vike-mcp's rpc.rs `tools_call_panic_becomes_iserror` pin: the catch_unwind in
    // `handle` turns a panicking tool into an `isError` tool RESULT — never a JSON-RPC
    // protocol error, never a crashed stdio loop.
    let resp = call("__test_panic", json!({}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("panicked"));
    assert!(resp.get("error").is_none(), "a caught tool panic is NOT a protocol error");
}

#[test]
fn submit_order_without_confirm_only_previews_and_touches_no_network() {
    // node_addr is None; a preview must NOT try to connect (it would error). It returns the
    // resolved wire command with will_execute:false.
    let resp = call(
        "submit_order",
        json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5, "order_type": "limit", "price": 100.0 }),
    );
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["Submit"]["symbol"], "BTCUSDT");
    assert_eq!(sc["wire_command"]["Submit"]["qty"], 0.5);
    assert_eq!(sc["guardrail"]["notional"], 50.0); // 0.5 * 100
}

/// Every WRITE tool advertises the optional `reason` (node proto v4) — and never requires it, so
/// an agent that ignores it keeps working exactly as before.
#[test]
fn every_write_tool_advertises_an_optional_reason() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    for name in WRITE_TOOLS {
        let tool = tools.iter().find(|t| t["name"] == name).expect("advertised");
        let props = &tool["inputSchema"]["properties"];
        assert_eq!(props["reason"]["type"], "string", "{name} must offer a `reason` string");
        assert!(
            props["reason"]["description"].as_str().unwrap().contains("audit"),
            "{name}: the reason description must say where it lands"
        );
        let required = tool["inputSchema"]["required"].as_array();
        assert!(
            required.is_none_or(|r| !r.iter().any(|v| v == "reason")),
            "{name}: `reason` must stay OPTIONAL"
        );
    }
    // A READ tool has no rationale to record.
    let snap = tools.iter().find(|t| t["name"] == "node_snapshot").unwrap();
    assert!(snap["inputSchema"]["properties"]["reason"].is_null());
}

/// The preview ECHOES the rationale (so the agent sees what will be filed against the command)
/// while leaving the resolved command untouched — the reason rides beside it, never inside it.
#[test]
fn a_write_preview_echoes_the_reason_without_changing_the_command() {
    let bare = call("cancel_order", json!({ "client_order_id": "c-1" }));
    assert_eq!(bare["result"]["structuredContent"]["reason"], Value::Null);

    let withr = call(
        "cancel_order",
        json!({ "client_order_id": "c-1", "reason": "  stale quote after the feed gap  " }),
    );
    let sc = &withr["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["reason"], "stale quote after the feed gap", "trimmed, and echoed back");
    assert_eq!(
        sc["wire_command"], bare["result"]["structuredContent"]["wire_command"],
        "the wire command is byte-identical with and without a reason"
    );
    // A blank rationale is no rationale (never an empty string in the preview).
    let blank = call("cancel_order", json!({ "client_order_id": "c-1", "reason": "   " }));
    assert_eq!(blank["result"]["structuredContent"]["reason"], Value::Null);
}

#[test]
fn submit_order_missing_required_field_is_a_tool_error() {
    let resp = call("submit_order", json!({ "venue": "sim", "side": 1, "qty": 1.0 })); // no symbol
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn confirm_alone_no_longer_executes_it_returns_a_preview() {
    // ⚠ THE REGRESSION THIS PINS. `confirm: true` used to be the whole gate, so a FIRST-and-only
    // call executed — while this module's doc promised "only a second call ... sends it". It now
    // fails CLOSED: without a `preview_token` the call is a preview, whatever `confirm` says.
    let resp = call(
        "submit_order",
        json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0, "confirm": true }),
    );
    assert_eq!(
        resp["result"]["isError"], false,
        "confirm without a token is a PREVIEW, not an error"
    );
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false, "nothing may be sent without a preview_token");
    assert!(sc["preview_token"].is_string(), "a preview must hand back the token that confirms it");
}

#[test]
fn a_preview_with_no_node_says_it_was_not_verified() {
    // An ABSENT node verdict must never read as an approving one — the client-side guardrail
    // cannot price a market order at all, and market is the default order type.
    let resp =
        call("submit_order", json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }));
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["verified_by_node"], false);
    assert_eq!(sc["node_verdict"], Value::Null);
    assert!(
        sc["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
        "the note must SAY the verdict is unverified: {}",
        sc["note"]
    );
}

#[test]
fn a_node_that_could_not_be_asked_is_not_a_verified_preview() {
    // ⚠ THE BUG THIS PINS. `verified` used to be `node.is_some()`, and `Server::node_preview`
    // returns `Some` on its FAILURE path too — a `checked_by: "none"` row carrying the
    // transport error, so the preview can name the fault. So a node that was dialled and
    // refused (unreachable, `AuthDenied`, `Response::Error`) came back `verified_by_node: true`
    // wearing the note that calls the verdict "the one that counts" — while `node_verdict`
    // right beside it said the node was never asked. The prompts this server serves teach an
    // agent to read that flag and confirm on it; for a MARKET order the client-side guardrail
    // is vacuous too, so nothing at all would have checked the order.
    let cmd = verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let unreachable = json!({
        "checked_by": CHECKED_BY_NONE,
        "accepted": Value::Null,
        "reason": "the node could not be asked: connection refused",
    });
    let pv = preview_of(
        "submit_order",
        &cmd,
        None,
        verbs::GuardrailCaps::default(),
        "pv-1",
        Some(unreachable),
        VENUE_CHECK_UNVERIFIED,
        // Not a settings write, so no `change` rides the preview.
        None,
    );
    assert_eq!(pv["verified_by_node"], false, "a fault is not a verdict: {pv}");
    assert!(pv.get("change").is_none(), "only a settings write's preview carries `change`: {pv}");
    assert!(
        pv["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
        "the note must match the flag: {}",
        pv["note"]
    );
    // ...and the answering case still reads as verified, or the fix would have disarmed the
    // distinction rather than corrected it.
    let answered = json!({
        "checked_by": CHECKED_BY_NODE,
        "accepted": true,
        "reason": Value::Null,
    });
    let ok = preview_of(
        "submit_order",
        &cmd,
        None,
        verbs::GuardrailCaps::default(),
        "pv-2",
        Some(answered),
        VENUE_CHECK_MOUNTED,
        None,
    );
    assert_eq!(ok["verified_by_node"], true, "an answered dry-run IS a verdict: {ok}");
}

#[test]
fn a_preview_whose_node_could_not_be_asked_is_not_verified() {
    // A node IS configured and a control key IS present, but the dry-run cannot reach it
    // (`127.0.0.1:1` refuses immediately). `node_preview` answers with a `checked_by: "none"`
    // object rather than `None`, and `preview_of` used to label THAT `verified_by_node: true`
    // with the "it is the verdict that counts" note — a dropped tunnel mid-incident read as
    // the node's own approval. A fault is not a verdict.
    let env: std::collections::HashMap<String, String> =
        [(nodekeys::CONTROL_KEY_ENV.to_string(), "ctl-key".to_string())].into_iter().collect();
    // Over `test_server()` rather than a second field list — the same argument the `server()`
    // helper's own doc makes: a hand-written list here is a second place to forget a field, and
    // the two would then disagree about what "a fresh server" is.
    let mut s = Server {
        node_addr: Some("127.0.0.1:1".to_string()),
        keys: nodekeys::resolve(&env, &std::collections::HashMap::new(), None),
        ..test_server()
    };
    let resp = s
        .handle(&req(
            1,
            "tools/call",
            json!({ "name": "cancel_order", "arguments": { "client_order_id": "c-1" } }),
        ))
        .unwrap();
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even unreachable");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["node_verdict"]["checked_by"], "none", "{}", sc["node_verdict"]);
    assert_eq!(sc["node_verdict"]["accepted"], Value::Null);
    assert_eq!(sc["verified_by_node"], false, "a transport fault is not a verdict");
    assert!(
        sc["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
        "the note must SAY the verdict is unverified: {}",
        sc["note"]
    );
}

#[test]
fn a_preview_token_fires_at_most_once() {
    let mut s = server();
    let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let pv = s.call_tool("submit_order", &args).unwrap();
    let token = pv["preview_token"].as_str().unwrap().to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token.clone());
    // First confirm consumes the token; with no node configured it fails at the CONNECTION,
    // which is proof it got PAST the gate.
    let first = s.call_tool("submit_order", &confirming).unwrap_err();
    assert!(first.message.contains("node"), "should reach the node step, got: {first:?}");
    assert!(!first.refused, "reaching the node is a FAILURE, not a gate refusal: {first:?}");
    // Second gets nothing — the token was removed before the caller executed.
    let second = s.call_tool("submit_order", &confirming).unwrap_err();
    assert!(
        second.message.contains("unknown or already used"),
        "a token must fire at most once, got: {second:?}"
    );
    assert!(second.refused, "a spent token is a GATE refusal, and the transcript reads that bit");
}

#[test]
fn intent_ignores_the_minted_id_but_nothing_else() {
    // ⚠ THE BUG THIS PINS. An exact compare rejected EVERY confirm, because
    // `fill_client_order_id` mints a fresh id on every call — so a preview and its confirm can
    // never carry the same one, and submit_order was unusable through this surface. CI caught
    // it; this keeps it caught.
    let base = verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let mut a = base.clone();
    let mut b = base.clone();
    if let (WireCommand::Submit(x), WireCommand::Submit(y)) = (&mut a, &mut b) {
        x.client_order_id = "c-1".into();
        y.client_order_id = "c-2".into();
    }
    let node = |c: &WireCommand| PreviewIntent::Node(Box::new(c.clone()));
    assert!(same_intent(&node(&a), &node(&b)), "a differing minted id must NOT break the binding");

    // …but every other field still binds.
    let other = verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0 }),
    )
    .unwrap();
    assert!(!same_intent(&node(&a), &node(&other)), "a different qty MUST break the binding");
    // ⚠ …and two DIFFERENT KINDS of intent never match, whatever they carry: the discriminant
    // is part of the compare, which is what stops a node token confirming a deletion.
    assert!(!same_intent(
        &node(&a),
        &PreviewIntent::Delete(DeleteIntent {
            kind: "bar".into(),
            venue: "sim".into(),
            symbol: None,
            group: None,
            interval: None,
            produced_by: "klines:".into(),
        })
    ));
}

#[test]
fn a_preview_token_is_bound_to_the_command_it_previewed() {
    // ⚠ Preview `qty: 0.5`, confirm `qty: 50` was ACCEPTED before this gate existed.
    let mut s = server();
    let pv = s
        .call_tool(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5 }),
        )
        .unwrap();
    let token = pv["preview_token"].as_str().unwrap().to_string();
    let err = s
        .call_tool(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0,
                         "confirm": true, "preview_token": token }),
        )
        .unwrap_err();
    assert!(
        err.message.contains("does not match this command"),
        "a token must not confirm a DIFFERENT command, got: {err:?}"
    );
}

#[test]
fn set_trading_state_preview_maps_the_wire_command() {
    let resp = call("set_trading_state", json!({ "state": "halted" }));
    assert_eq!(resp["result"]["structuredContent"]["wire_command"]["SetTradingState"], "Halted");
}

#[test]
fn market_exit_preview_allows_an_omitted_venue() {
    let resp = call("market_exit", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    assert!(resp["result"]["structuredContent"]["wire_command"]["MarketExit"].is_object());
}

/// The venue a write NAMES, per wire variant — the input half of the gate.
///
/// ⚠ The two OPTIONAL rows are the ones worth having a test for. `market_exit` and
/// `mass_cancel` mean "every engine" when the argument is omitted, so an omission must read as
/// "nothing to check" and never as "an empty venue to compare" — a gate that refused there
/// would take out the panic button, which is a worse failure than the one being closed.
#[test]
fn the_venue_a_write_names_is_read_per_variant() {
    let of = |tool: &str, args: Value| {
        let cmd = verbs::wire_command_for(tool, &args).expect("a valid write");
        commanded_venue(&cmd).map(str::to_string)
    };
    assert_eq!(
        of("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
        Some("sim".to_string())
    );
    assert_eq!(of("flatten", json!({ "venue": "sim", "symbol": "B" })), Some("sim".to_string()));
    assert_eq!(of("market_exit", json!({ "venue": "sim" })), Some("sim".to_string()));
    assert_eq!(of("mass_cancel", json!({ "venue": "sim" })), Some("sim".to_string()));
    // ...and the venue-less shapes, each of which must be checked against nothing.
    assert_eq!(of("market_exit", json!({})), None, "an omitted venue means EVERY engine");
    assert_eq!(of("mass_cancel", json!({})), None, "an omitted venue means EVERY venue");
    assert_eq!(of("cancel_order", json!({ "client_order_id": "c1" })), None);
    assert_eq!(of("modify", json!({ "client_order_id": "c1", "new_qty": 2.0 })), None);
    assert_eq!(of("set_trading_state", json!({ "state": "halted" })), None);
}

/// The decision, in every direction it has.
///
/// ⚠ THE POSITION THIS PINS is the 2026-09-06 model run's: an agent invented `venue: "node"`
/// from the operator's wording, and every layer under it accepted the string — the node's
/// dry-run vets only the notional cap, and `vike_core`'s `apply_intent_routed` resolves an
/// unroutable venue with `unwrap_or(0)`, i.e. onto the node's FIRST engine with the capability
/// preflight skipped. A real order rested in the book under a venue that names nothing.
#[test]
fn a_venue_the_node_does_not_mount_is_refused_and_the_refusal_names_what_is_mounted() {
    let mounted = ["polymarket".to_string(), "binance".to_string()];
    let err = venue_verdict(Some("node"), None, Some(&mounted[..])).expect_err("must refuse");
    // The offending value, so the agent knows WHICH argument to change...
    assert!(err.contains("\"node\""), "the refusal must name the offending value: {err}");
    // ...what the node actually has, so it can fix it without a guess...
    assert!(err.contains("polymarket"), "the refusal must name the mounted venues: {err}");
    assert!(err.contains("binance"), "the refusal must name EVERY mounted venue: {err}");
    // ...and the tool that reports it, which is the step the failing run had skipped.
    assert!(err.contains("node_snapshot"), "the refusal must name the read tool: {err}");
}

/// The three passing dispositions, and the one that must NOT be a refusal.
#[test]
fn the_venue_check_reports_which_of_the_three_answers_it_gave() {
    let mounted = ["polymarket".to_string()];
    assert_eq!(
        venue_verdict(Some("polymarket"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
    assert_eq!(
        venue_verdict(None, None, Some(&mounted[..])),
        Ok(VENUE_CHECK_NONE),
        "nothing to check"
    );
    // ⚠ NO EVIDENCE IS NOT A REFUSAL. A node that cannot be read is a node that cannot be
    // written to either (`Server::execute` fails there), so refusing here would buy nothing and
    // would break every preview taken against an unreachable node — which is the shape
    // `mcp_transcript.rs` drives over a node-less server. What it must NOT do is present the
    // unmade check as a passed one, which is why this is its own value rather than `mounted`.
    assert_eq!(venue_verdict(Some("anything"), None, None), Ok(VENUE_CHECK_UNVERIFIED));
}

/// **AMBIGUITY REFUSES, and it is a DIFFERENT fact from absent evidence.** A node that answered
/// with two candidates told us more than a node we could not read, and a gate that collapsed
/// the two would discard the more informative answer.
#[test]
fn a_venue_carried_by_two_accounts_is_refused_rather_than_guessed() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string(), "okx".to_string()];
    let err = venue_verdict(Some("binance"), None, Some(&mounted[..]))
        .expect_err("a venue two accounts carry may not be guessed");
    assert!(err.contains("AMBIGUOUS"), "{err}");
    assert!(err.contains("binance#ALT"), "the refusal must NAME the candidates: {err}");
    assert!(
        err.contains("no preview_token was issued"),
        "the refusal must say the two-call gate was not half given away: {err}"
    );
    assert!(
        !err.contains("default"),
        "it must NEVER launder the guess as a reassurance — 0041's own charge: {err}"
    );
}

/// Naming the ROUTE KEY outright resolves it — that is what the refusal above tells the caller
/// to do, so it has to work.
#[test]
fn naming_the_route_key_resolves_the_ambiguity() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    assert_eq!(
        venue_verdict(Some("binance#ALT"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
}

/// …and so does naming it in the ACCOUNT FIELD, which is the spelling the wire actually offers
/// now that `MountStrategy` carries one. Without this arm the gate would refuse a command that
/// says exactly which book it means — the ambiguity refusal firing on the one caller who
/// removed the ambiguity.
#[test]
fn naming_the_account_field_resolves_the_ambiguity() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    assert_eq!(
        venue_verdict(Some("binance"), Some("ALT"), Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
}

/// ⚠ **`DEFAULT` IS THE ROW THIS GATE WOULD HAVE LOST, and it is the whole reason the named-
/// account arm runs BEFORE the carrier count.** Its route key is the BARE VENUE, so the count
/// for it is two on this node — identical to the account-less caller's. Judged by the count
/// alone, the caller who said "the unlabelled account, deliberately" is refused as ambiguous
/// and told to name an account, which is what they just did.
#[test]
fn naming_default_is_never_ambiguous_even_though_its_route_key_is_the_bare_venue() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    assert_eq!(
        venue_verdict(Some("binance"), Some("DEFAULT"), Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
    // …and the account-less caller on the SAME node is still refused, so this cannot be passed
    // by an arm that merely stopped counting.
    assert!(
        venue_verdict(Some("binance"), None, Some(&mounted[..])).is_err(),
        "absence is a different row and must still refuse"
    );
}

/// An account this node does not run is refused BY NAME, and the refusal shows the route key it
/// looked for — so the caller can compare it against `venues[].route_key` directly instead of
/// re-deriving the `#` grammar themselves.
#[test]
fn an_unmounted_account_is_refused_by_name_and_shows_the_key_it_sought() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    let err = venue_verdict(Some("binance"), Some("HEDGE"), Some(&mounted[..]))
        .expect_err("an unarmed account resolves no engine");
    assert!(err.contains("HEDGE"), "names the account: {err}");
    assert!(err.contains("binance#HEDGE"), "…and the route key it sought: {err}");
    assert!(
        err.contains("no preview_token was issued"),
        "…and that the two-call gate was not half given away: {err}"
    );
}

/// An account string that is not a legal account NAME is refused as such, before any lookup —
/// the grammar is `parse_wire_account`'s, so the gate has no second set of rules to drift from.
#[test]
fn an_illegal_account_string_is_refused_before_the_lookup() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    let err = venue_verdict(Some("binance"), Some("alt"), Some(&mounted[..]))
        .expect_err("a lowercase label is not a legal account name");
    assert!(err.contains("not a legal account name"), "{err}");
    assert!(err.contains("no preview_token was issued"), "{err}");
}

/// ⚠ The complement, and it is what keeps the refusal from being a capability regression: a
/// venue exactly ONE account carries still passes, which is every node in production today.
#[test]
fn a_venue_one_account_carries_still_passes() {
    let mounted = ["binance".to_string(), "okx".to_string()];
    assert_eq!(venue_verdict(Some("binance"), None, Some(&mounted[..])), Ok(VENUE_CHECK_MOUNTED));
    assert_eq!(venue_verdict(Some("okx"), None, Some(&mounted[..])), Ok(VENUE_CHECK_MOUNTED));
}

/// ⚠ A venue whose name is a PREFIX of another's must not be read as that other's account.
/// `binanceus` is a roster venue, not `binance`'s account — the separator is what decides, and
/// `label_of_route_key` is what knows it.
#[test]
fn a_prefix_venue_is_not_mistaken_for_an_account_of_another() {
    let mounted = ["binance".to_string(), "binanceus".to_string()];
    assert_eq!(
        venue_verdict(Some("binance"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED),
        "binanceus is a different venue, so binance is carried by exactly one account"
    );
}
/// EXACT, never case-folded.
///
/// ⚠ Routing compares the payload's venue to an engine's `route_key` with `==`
/// (`vike_core`'s `engine_idx_for_route_key`), so `"Polymarket"` routes to nothing on a node
/// mounting `"polymarket"` and falls back to the first engine exactly as `"node"` did. A
/// case-insensitive refusal would therefore ADMIT a string the routing silently redirects —
/// the hole reopened, wearing a friendlier spelling.
#[test]
fn the_venue_comparison_is_exact_because_the_routing_is() {
    let mounted = ["polymarket".to_string()];
    assert!(venue_verdict(Some("Polymarket"), None, Some(&mounted[..])).is_err(), "case matters");
    assert!(
        venue_verdict(Some("polymarket "), None, Some(&mounted[..])).is_err(),
        "whitespace matters"
    );
    assert_eq!(
        venue_verdict(Some("polymarket"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
}

/// A preview against a node-less server is UNVERIFIED, not refused — and it SAYS so.
///
/// The end-to-end half of the disposition above, over the shipped router rather than the pure
/// function: `test_server()` has no node, so the mounted set cannot be read, and every write
/// tool must still preview exactly as it did before this gate existed.
#[test]
fn a_preview_with_no_node_reports_the_venue_as_unverified_rather_than_refusing_it() {
    let resp =
        call("submit_order", json!({ "venue": "node", "symbol": "B", "side": 1, "qty": 1.0 }));
    assert_eq!(resp["result"]["isError"], false, "no node = no evidence = no refusal: {resp}");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["venue_check"], VENUE_CHECK_UNVERIFIED, "{sc}");
    assert!(sc["preview_token"].is_string(), "an unverified preview still mints a token: {sc}");
}

/// Every write tool's preview carries the field, so an agent never has to infer from its
/// absence whether the venue was looked at.
#[test]
fn every_write_tool_preview_reports_its_venue_check() {
    for tool in WRITE_TOOLS {
        let resp = call(tool, every_write_tools_arguments());
        let sc = &resp["result"]["structuredContent"];
        let got = sc["venue_check"].as_str().unwrap_or_default();
        assert!(
            [VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE, VENUE_CHECK_UNVERIFIED].contains(&got),
            "{tool}: every preview reports one of the three dispositions, got {sc}"
        );
    }
}

// ---- the node-lifecycle write tools + the two per-call node reads ------------------------

/// [`LIFECYCLE_TOOLS`] is a SUBSET of [`WRITE_TOOLS`], never a second roster.
///
/// ⚠ The direction that matters is this one: a lifecycle tool missing from `WRITE_TOOLS` loses
/// the mandatory preview gate, the `destructiveHint` and the `read-only` withholding while
/// still LOOKING like a gated tool — it would be routed by `call_tool`'s catch-all as an
/// unknown tool, which is at least loud, but the annotations pin would go quiet the moment
/// somebody "fixed" that by adding an arm.
#[test]
fn the_lifecycle_tools_are_a_subset_of_the_write_roster() {
    for tool in LIFECYCLE_TOOLS {
        assert!(
            WRITE_TOOLS.contains(&tool),
            "{tool} is a lifecycle tool but not on WRITE_TOOLS — it would carry no preview gate"
        );
        assert!(is_write_tool(tool), "{tool} must route as a write");
    }
    // …and each one is really BUILDABLE by the SHARED construction site, so this is not three
    // names agreeing with three names while `verbs::wire_command_for` answers `no wire command
    // for …` — the state this file's own lifecycle builder existed to avoid, and which the
    // lift into `verbs` would silently restore if one name were left behind there.
    for tool in LIFECYCLE_TOOLS {
        assert!(
            verbs::wire_command_for(tool, &every_write_tools_arguments()).is_ok(),
            "{tool}: the shared site must build a command from the shared argument union"
        );
    }
}

/// Each lifecycle tool inherits the MANDATORY PREVIEW: a first call sends nothing and hands
/// back the token that would confirm it. The roster loops elsewhere prove this for
/// `WRITE_TOOLS` entire; this is the same claim aimed at the three that are new, so a
/// regression names them rather than "some write tool".
#[test]
fn a_lifecycle_write_previews_before_it_executes() {
    for tool in LIFECYCLE_TOOLS {
        let mut args = every_write_tools_arguments();
        args["confirm"] = json!(true);
        let sc = &call(tool, args)["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], json!(false), "{tool}: confirm alone must not execute");
        assert!(sc["preview_token"].is_string(), "{tool}: a preview carries its token: {sc}");
        assert_eq!(sc["verified_by_node"], json!(false), "{tool}: no node, so no verdict");
    }
}

/// **A settings write PREVIEWS with no `policy_confirm`, a policy key included, and the preview
/// shows the change.** The argument is gone with the retype it carried (`docs/decisions/0086` point
/// 7: no retype confirm, for any key, on any surface), and what the preview carries instead is
/// `change` — the key, the node's CURRENT value and the new one, `old → new`.
///
/// ⚠ With no node configured the old value cannot be READ, and the preview says so rather than
/// inventing one: `old` is null and `old_unread` names why. A fabricated old value is the one field
/// an agent would relay to the owner as fact.
///
/// The calls are the ones the deleted gate treated differently — two policy keys and a non-policy
/// one — and all three now take the same path. Each names only its key: the tool's `file` argument
/// went with the file era (the wire's is derived from the key, `crate::cmd::verbs`' job).
#[test]
fn a_settings_write_previews_without_a_typed_confirm_and_shows_old_to_new() {
    for (key, value) in [
        ("policy.max_notional_per_order", "250"),
        (POLICY_KEY_FIXTURE, "3"),
        ("config.tradehub_addr", "127.0.0.1:7879"),
    ] {
        let resp = call("set_setting", json!({ "key": key, "value": value }));
        assert_eq!(resp["result"]["isError"], false, "{key}: previewed, never refused: {resp}");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], json!(false), "{key}: still only a preview: {sc}");
        assert!(sc["preview_token"].is_string(), "{key}: …and it mints its token: {sc}");
        assert_eq!(
            sc["wire_command"]["SetSetting"]["confirm"],
            Value::Null,
            "{key}: nothing rides in the wire's ignored confirm field: {sc}"
        );
        let change = &sc["change"];
        assert_eq!(change["key"], json!(key), "{sc}");
        assert_eq!(change["new"], json!(value), "{sc}");
        assert_eq!(change["old"], Value::Null, "{key}: no node was asked, so no old value: {sc}");
        assert!(
            change["old_unread"]
                .as_str()
                .is_some_and(|why| why.contains("no vike-tradehub node configured")),
            "{key}: …and the preview says WHY it has none: {sc}"
        );
        let line = change["line"].as_str().unwrap_or_default();
        assert!(
            line.starts_with(key) && line.ends_with(&format!(" → {value}")),
            "{key}: the one-line `key: old → new`: {line:?}"
        );
    }
}

/// **The preview gate is not the retype, and it outlives it.** On a POLICY write — the call whose
/// extra gate was just deleted, so the one place the shared gate could have gone with it:
/// `confirm: true` alone still only previews; an unknown token is refused; a token minted for one
/// value does not confirm another; and a fresh token reaches the node step, which is the positive
/// half — without it the refusals above would all pass on a gate that refused everything.
#[test]
fn a_policy_write_still_executes_only_against_its_own_preview_token() {
    let mut s = server();
    let args = json!({ "key": "policy.max_notional_per_order", "value": "250" });
    let mut bare = args.clone();
    bare["confirm"] = json!(true);

    let pv = s.call_tool("set_setting", &bare).expect("`confirm: true` alone is a preview");
    assert_eq!(pv["will_execute"], json!(false), "no token, nothing sent: {pv}");
    let token = pv["preview_token"].as_str().expect("a preview mints its token").to_string();

    let mut unknown = bare.clone();
    unknown["preview_token"] = json!("not-a-token-this-server-issued");
    let err = s.call_tool("set_setting", &unknown).expect_err("an unknown token executes nothing");
    assert!(err.refused, "a GATE refusal, which the transcript classifies: {err:?}");
    assert!(err.message.contains("unknown or already used"), "{err:?}");

    let mut other_value = bare.clone();
    other_value["value"] = json!("999");
    other_value["preview_token"] = json!(token);
    let err = s
        .call_tool("set_setting", &other_value)
        .expect_err("a token confirms only the value it previewed");
    assert!(err.message.contains("does not match this command"), "{err:?}");

    let fresh = s.call_tool("set_setting", &args).expect("a fresh preview");
    let mut confirming = bare.clone();
    confirming["preview_token"] = fresh["preview_token"].clone();
    let err = s.call_tool("set_setting", &confirming).expect_err("no node is configured");
    assert!(!err.refused, "the right token is ADMITTED — this is the node step failing: {err:?}");
    assert!(err.message.contains("no vike-tradehub node configured"), "{err:?}");
}

/// A CONFIRMED settings write leaves by [`Server::execute_settings_write`] rather than the
/// fire-and-forget worker, and with no node configured that surfaces as the connection error —
/// which is the positive evidence it got PAST the preview gate, the same shape
/// `crates/vike-cli/tests/mcp_transcript.rs` uses for `submit_order`.
#[test]
fn a_confirmed_settings_write_reaches_the_node_step() {
    let mut s = server();
    let args = json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" });
    let preview = s
        .handle(&req(1, "tools/call", json!({ "name": "set_setting", "arguments": args })))
        .unwrap();
    let token = preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .expect("the preview mints a token")
        .to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let sent = s
        .handle(&req(2, "tools/call", json!({ "name": "set_setting", "arguments": confirming })))
        .unwrap();
    assert_eq!(sent["result"]["isError"], true, "no node is configured: {sent}");
    let text = sent["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("no vike-tradehub node configured"),
        "a matching token must reach the node step — that is what proves the gate ADMITS: \
             {text}"
    );
}

/// **The UNATTENDED gate refuses EVERY settings write** (decision 0040: the owner's rulings of
/// 2026-09-28 for `policy.*` and 2026-09-29 for every other key) — whatever the key's class or
/// spelling — and nothing that is not a settings write.
#[test]
fn the_unattended_gate_refuses_every_settings_write_and_nothing_else() {
    let setting = |key: &str| WireCommand::SetSetting {
        file: verbs::section_of_key(key).to_string(),
        key: key.to_string(),
        value: "1".into(),
        confirm: None,
    };
    for key in [
        "policy.max_notional_per_order",
        POLICY_KEY_FIXTURE,
        "policy.venues.binance",
        "policy.accounts.binance.ALT",
        " policy.max_notional_per_order",
        "flags.tradehub_live",
        "flags.reconcile_off",
        "config.tradehub_addr",
        "preferences.log_level",
        "policies.max_notional_per_order",
    ] {
        let why = unattended_refusal(&setting(key)).expect_err(key);
        assert!(why.contains("vike-cli config set"), "{key:?}: name the CLI route: {why}");
        assert!(why.contains("GUI"), "{key:?}: …and the GUI route: {why}");
    }
    assert_eq!(unattended_refusal(&WireCommand::Cancel("c-1".into())), Ok(()), "not a setting");
}

/// **An unattended SERVER refuses a settings write before any token exists** — the gate sits in the
/// router ahead of the preview, so the command is unconfirmable rather than merely unconfirmed, and
/// the refusal carries the `refused` bit the transcript classifies. The same key previews in an
/// ATTENDED server, so what refuses is the session, not the key.
#[test]
fn an_unattended_server_refuses_a_settings_write_before_minting_a_token() {
    let mut s = Server { unattended: true, ..server() };
    for key in ["policy.max_notional_per_order", "config.tradehub_addr"] {
        let err = s
            .call_tool("set_setting", &json!({ "key": key, "value": "1" }))
            .expect_err("an unattended session changes no setting");
        assert!(err.refused, "`{key}`: a GATE refusal: {err:?}");
    }
    assert_eq!(s.pending.next, 0, "no token may exist for a command nobody can confirm");
    let mut attended = server();
    let pv = attended
        .call_tool("set_setting", &json!({ "key": "config.tradehub_addr", "value": "1:2" }))
        .expect("an attended session previews it");
    assert!(pv["preview_token"].is_string(), "{pv}");
}

/// `--unattended` is a BOOLEAN and takes no value — `--unattended=false` would be a way to write
/// the flag and mean its opposite — and an absent flag is an ATTENDED session.
#[test]
fn the_unattended_flag_parses_and_refuses_a_value() {
    let parsed = |argv: &[&str]| parse_config(argv.iter().map(|s| s.to_string()), None, None);
    assert!(parsed(&["--unattended"]).expect("the bare flag parses").unattended);
    assert!(!parsed(&[]).expect("no flag parses").unattended, "attended is the default");
    assert!(parsed(&["--unattended=false"]).is_err(), "a value is refused, never read");
}

// ⚠ Four PURE-BUILDER tests stood here — the mount XOR, the mount's `params` table,
// `unmount_strategy`'s required id, and `set_setting`'s value rendering. They MOVED to
// `crate::cmd::verbs`'s tests with the arms they exercise, unchanged in intent, and they are
// now three of the ten the roster loops there cover rather than three of three. What stays in
// this file is everything with a `Server` in front of it.

/// The two per-call node reads answer with the CONNECTION problem when there is no node — never
/// with an empty payload, which an agent would summarise as "the node runs nothing" and
/// "the node is configured with nothing".
#[test]
fn a_node_read_without_a_node_is_an_error_and_never_an_empty_answer() {
    for tool in ["strategy_status", "settings_show"] {
        let resp = call(tool, json!({}));
        assert_eq!(resp["result"]["isError"], true, "{tool}: got {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no vike-tradehub node configured"),
            "{tool}: the error must name what is missing: {text}"
        );
    }
}

/// The three per-call READ failure diagnoses, which an agent must be able to tell apart: two of
/// them are permanent facts about a reachable box and retrying either forever is the behaviour
/// the split exists to prevent.
#[test]
fn a_node_read_failure_says_whether_retrying_could_ever_help() {
    let unsupported = node_read_failure(
        "strategy_status",
        "1.2.3.4:7777",
        &io::Error::new(io::ErrorKind::Unsupported, "no strategy-verbs capability"),
    );
    assert!(unsupported.contains("upgrade"), "{unsupported}");
    assert!(unsupported.contains("cannot succeed"), "{unsupported}");
    assert!(
        unsupported.contains("no strategy-verbs capability"),
        "the client's own sentence \
             names the capability string and must survive: {unsupported}"
    );

    let denied = node_read_failure(
        "settings_show",
        "1.2.3.4:7777",
        &io::Error::new(io::ErrorKind::PermissionDenied, "bad signature"),
    );
    assert!(denied.contains(nodekeys::OBSERVE_KEY_ENV), "points at the key: {denied}");

    let down = node_read_failure(
        "settings_show",
        "1.2.3.4:7777",
        &io::Error::new(io::ErrorKind::ConnectionRefused, "refused"),
    );
    assert!(down.contains("cannot query"), "{down}");
    assert!(!down.contains("upgrade"), "a refused socket is not an old node: {down}");
}

/// An enqueue refused for want of a NODE CAPABILITY must say so in words. It used to print the
/// bare enum name, which the lifecycle verbs made a live path: they are the first tools on this
/// surface that a current-protocol node can decline to accept at all.
#[test]
fn a_capability_refusal_tells_the_agent_not_to_retry() {
    let text = control_rejection(ControlRejected::UnsupportedByNode);
    assert!(text.contains("REFUSED"), "{text}");
    assert!(text.contains("cannot succeed"), "…and that retrying is pointless: {text}");
    assert!(text.contains("upgrade"), "…and where the fix is: {text}");
    assert!(
        control_rejection(ControlRejected::Busy).contains("try again"),
        "a full queue IS transient and must read differently"
    );
}

/// The two new instruction clauses are scoped by the SAME [`ToolAccess`] the roster is — the
/// identical property `the_instructions_are_scoped_by_the_same_access_the_roster_is` holds for
/// the clauses that shipped before them.
#[test]
fn the_new_instruction_clauses_are_scoped_with_the_tools_they_describe() {
    let full = instructions(&ToolAccess::full());
    let read_only = instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new()));
    let offline = instructions(&ToolAccess::new(Profile::Offline, Vec::new()));

    assert!(full.contains("strategy_status"), "`full` serves the node reads and may say so");
    assert!(
        read_only.contains("strategy_status"),
        "`read-only` still serves them — they authenticate under the observe scope"
    );
    assert!(
        !offline.contains("strategy_status"),
        "`offline` withholds every network tool, so naming one is advertising a withheld tool"
    );

    // The lifecycle clause carries the OWNER's rule for a settings write (`docs/decisions/0086`
    // point 6: an agent changes a live setting only after the owner said yes in chat) — and it is
    // scoped with the tools it governs, like every other clause here.
    assert!(full.contains("yes in chat"), "`full` serves set_setting and must carry the rule");
    for (name, text) in [("read-only", &read_only), ("offline", &offline)] {
        assert!(
            !text.contains("yes in chat"),
            "`{name}` serves no lifecycle write, so the rule is a door it cannot reach"
        );
    }
    // …and nothing of the retype the rule replaced (point 7), under any profile: an instruction
    // naming `policy_confirm` would send an agent after an argument the schema no longer has.
    for (name, text) in [("full", &full), ("read-only", &read_only), ("offline", &offline)] {
        assert!(!text.contains("policy_confirm"), "`{name}` names a removed argument: {text}");
        assert!(
            !text.to_ascii_lowercase().contains("retyp"),
            "`{name}` describes a removed ceremony: {text}"
        );
    }
}

/// **`set_setting` takes no retyped key, and says what does guard it.** The `policy_confirm`
/// argument is gone from the schema (`docs/decisions/0086` point 7), no tool text on this surface
/// asks an agent to obtain a retyping, and the tool's own description carries the owner's rule and
/// the `old → new` its preview shows — the two things an agent needs before it confirms one.
#[test]
fn set_setting_takes_no_retyped_key_and_states_the_owners_rule() {
    let spec = tools_spec();
    let tool = spec
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "set_setting")
        .expect("set_setting is served under `full`");
    let props = tool["inputSchema"]["properties"].as_object().expect("an object schema");
    assert!(!props.contains_key("policy_confirm"), "the retype argument must be gone: {props:?}");
    let desc = tool["description"].as_str().expect("a description");
    assert!(desc.contains("yes in chat"), "the owner's rule must travel with the tool: {desc}");
    assert!(desc.contains("old → new"), "…and what the preview shows: {desc}");
    let whole = spec.to_string();
    assert!(!whole.contains("policy_confirm"), "no tool may name the removed argument");
    assert!(!whole.to_ascii_lowercase().contains("retyp"), "no tool may describe the ceremony");
}

#[test]
fn list_strategies_is_advertised_read_only() {
    let resp = server().handle(&req(3, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|t| t["name"] == "list_strategies")
        .expect("list_strategies must be advertised in tools/list");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
}

#[test]
fn list_strategies_without_a_reachable_datahub_is_a_clean_error() {
    // Point at a port nothing is listening on: the connect must fail into a clean tool error,
    // never a panic. (`127.0.0.1:1` refuses immediately.)
    let mut s = Server { datahub_addr: "127.0.0.1:1".to_string(), ..test_server() };
    let resp = s
        .handle(&req(1, "tools/call", json!({ "name": "list_strategies", "arguments": {} })))
        .unwrap();
    assert_eq!(resp["result"]["isError"], true, "an unreachable datahub must error, not panic");
}

#[test]
fn node_snapshot_without_node_is_a_clean_error() {
    let resp = call("node_snapshot", json!({}));
    assert_eq!(resp["result"]["isError"], true);
}

/// The condition `node_snapshot`'s first-frame wait ends on, row by row: the client's own
/// placeholder never satisfies it, and EITHER piece of node evidence alone does — a stamped
/// `identity` (an idle node at `seq: 0`, the row the old `seq > 0` condition sat out) or a fold
/// (`seq > 0` from a node that stamps nothing).
#[test]
fn only_a_frame_the_node_pushed_ends_the_first_frame_wait() {
    let placeholder = WireSnapshot::empty();
    assert!(!is_node_frame(&placeholder), "the client's placeholder is never the node's answer");

    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: "idle".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let idle_stamped = WireSnapshot { identity: Some(identity), ..WireSnapshot::empty() };
    assert!(is_node_frame(&idle_stamped), "an idle node's stamped seq-0 frame is a real answer");

    let folded_unstamped = WireSnapshot { seq: 1, ..WireSnapshot::empty() };
    assert!(is_node_frame(&folded_unstamped), "a fold is node evidence without any identity");
}

/// A read tool MARKS every placeholder frame — the client's own and the node's stamped pre-fold one
/// alike, since both carry `venues: []` and epoch `0` that are not readings — and marks nothing
/// else. `pre_fold` is present on every answer, so `false` can be relied on too.
#[test]
fn a_read_marks_a_frame_with_nothing_built_as_pre_fold_and_only_that() {
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: "pre-fold".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let node_placeholder = WireSnapshot { identity: Some(identity), ..WireSnapshot::empty() };
    for (what, snap) in [("client", WireSnapshot::empty()), ("node", node_placeholder)] {
        let answer = node_read_answer(&snap).expect("serializes");
        assert_eq!(answer["pre_fold"], true, "the {what}'s placeholder is marked: {answer}");
        let note = answer["pre_fold_note"].as_str().expect("…with the note");
        assert!(note.contains("does NOT mean nothing is mounted"), "{note}");
        assert_eq!(answer["seq"], 0, "the frame itself is served unchanged: {answer}");
    }

    let built = WireSnapshot { seq: 7, ..WireSnapshot::empty() };
    let answer = node_read_answer(&built).expect("serializes");
    assert_eq!(answer["pre_fold"], false, "a built frame is not marked: {answer}");
    assert!(answer.get("pre_fold_note").is_none(), "…and carries no note: {answer}");
    assert_eq!(answer["seq"], 7, "{answer}");
}

#[test]
fn unknown_method_is_a_jsonrpc_error() {
    let resp = server().handle(&req(9, "no/such", json!({}))).unwrap();
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn a_notification_gets_no_response() {
    assert!(
        server()
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none()
    );
}

// ---- the absorbed vike-mcp tool surface (Phase A) ----------------------------------------

#[test]
fn validate_strategy_answers_ok_and_compile_errors_offline() {
    // A good script is an `ok:true` ANSWER; a bad script is an `ok:false` ANSWER carrying the
    // compile error — NOT an isError tool failure (the agent reads the error and fixes the
    // script). Mirrors vike-mcp's validate_strategy semantics; the argument is `script` here
    // (vike-mcp said `code`), aligned with this file's discover_params.
    let good = call(
        "validate_strategy",
        json!({ "script": "let fast = param(\"fast\", 5.0);\nfn on_bar() {}" }),
    );
    assert_eq!(good["result"]["isError"], false);
    assert_eq!(good["result"]["structuredContent"]["ok"], true);

    let bad = call("validate_strategy", json!({ "script": "fn on_bar( {" }));
    assert_eq!(bad["result"]["isError"], false, "a compile error is an ANSWER, not a failure");
    let sc = &bad["result"]["structuredContent"];
    assert_eq!(sc["ok"], false);
    assert!(!sc["error"].as_str().unwrap().is_empty());
}

#[test]
fn validate_strategy_missing_script_arg_is_a_tool_error() {
    let resp = call("validate_strategy", json!({}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("script"));
}

#[test]
fn list_templates_returns_named_parameterized_sources_that_compile() {
    let resp = call("list_templates", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    let ts = resp["result"]["structuredContent"]["templates"].as_array().unwrap();
    assert!(ts.iter().any(|t| t["name"] == "SMA cross"));
    // Every advertised template must actually compile against THIS build's Rhai host and
    // expose param() knobs (so it drops straight into run_sweep) — the same pin
    // vike-studio-core keeps on its own copy of these sources.
    //
    // ⚠ Compiling is NOT the property that matters most, and this test cannot see it:
    // `discover_params` runs the script's TOP LEVEL only, and every template's real work sits
    // inside `fn on_bar()`. `every_shipped_template_reaches_the_broker` below is the gate that
    // covers what this one structurally cannot.
    for t in ts {
        let (name, code) = (t["name"].as_str().unwrap(), t["code"].as_str().unwrap());
        let params = vike_script::discover_params(code)
            .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
        assert!(!params.is_empty(), "template {name} must expose param()s");
        assert!(code.contains("on_bar"), "template {name} must define on_bar");
    }
}

/// A deterministic strictly-RISING bar series. Monotone closes are enough to drive every
/// shipped template past its warm-up and into a decision: `sma(5)` rises above `sma(20)`,
/// `rsi(14)` climbs past the reversion band's `hi`, and `high()` clears the breakout channel.
fn rising_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let c = 100.0 + i as f64;
            Bar {
                ts: i as i64 * 60_000,
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect()
}

/// ⚠ **The gate behind "an agent can copy a template and it will actually trade."**
///
/// `list_templates` is the surface an AGENT copies from, and until this test it was the one
/// template surface with no behavioral gate at all: the compile pin above is the whole of what
/// this crate checked. (`crates/vike-studio-core/tests/templates_execute.rs` is the twin over
/// that crate's own copy of these sources; it runs them through the real Run pipeline instead.)
///
/// Parsing is the wrong bar. Rhai resolves a REGISTERED function when its line RUNS, not at
/// compile time, and `discover_params` runs only the top level — so every call a template makes
/// (all of them inside `fn on_bar()`) is unchecked by a compile gate. Three mistakes land in
/// that blind spot identically: an unbound NAME (a typo, or any name outside
/// `vike_script::RHAI_INDICATORS` — the set the host actually registers), a wrong
/// ARITY, and a wrong ARGUMENT TYPE (`rhai`'s `resolve_fn` hashes each argument's `TypeId` and
/// performs no INT->FLOAT coercion for a registered function, so `market(1, 1)` or `sma(5.0)`
/// misses just as hard as a typo). Each raises `ErrorFunctionNotFound` on every bar;
/// `RhaiStrategy`'s hook runner swallows it (fail-safe: zero orders that bar) and self-disables
/// after 10 consecutive errors. The strategy looks mounted and silently never trades. Only
/// EXECUTION distinguishes that from a strategy that simply saw no signal.
///
/// Non-vacuity is demonstrated rather than argued — see
/// `an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing` below, which drives the
/// same series and shows this assertion genuinely fails for such a script.
#[test]
fn every_shipped_template_reaches_the_broker() {
    for (name, src) in TEMPLATES {
        let mut strat = RhaiStrategy::<MockBroker>::compile(src)
            .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
        let mut broker = MockBroker::default();
        for bar in rising_bars(80) {
            broker.px = bar.close;
            strat.on_bar(&mut broker, &bar);
        }
        assert!(
            !broker.markets.is_empty(),
            "template {name} placed no order over 80 rising bars — a template an agent COPIES \
                 must actually trade. Check that every function it calls is host-bound \
                 (`vike_script::RHAI_INDICATORS`, plus `crates/vike-script/src/engine.rs`'s \
                 `register_reads`/`register_verbs`/`build_engine`) and that each call's arity and \
                 ARGUMENT TYPES match the registration exactly — rhai coerces neither."
        );
        for (symbol, side, qty) in &broker.markets {
            assert_eq!(symbol, "BTCUSDT", "template {name} must route to the bar's own symbol");
            assert!(*side == 1 || *side == -1, "template {name} sent side {side}, not ±1");
            assert!(*qty > 0.0, "template {name} sent a non-positive qty {qty}");
        }
    }
}

/// The proof that the gate above is not vacuous, and a live specimen of the failure it exists
/// to catch.
///
/// The SMA-cross template with its `sma(` calls rewritten to [`UNBOUND_WITNESS`] still COMPILES
/// and still reports its `param()` knobs — both compile-only gates stay green on it — while
/// reaching the broker exactly never.
///
/// ⚠ The witness used to be `wma`, a REAL registry indicator the host did not bind. It stopped
/// being a witness when the host widened to the registry, so it is now a name outside the
/// registry ENTIRELY — asserted, not assumed, on both axes below. Do not restore `wma`: a
/// bound name here makes this test pass for the wrong reason and quietly turns
/// `every_shipped_template_reaches_the_broker` into a claim about nothing.
#[test]
fn an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing() {
    let sma_cross = TEMPLATES[0].1; // the one starter this rewrite has a call to break
    let unbound = sma_cross.replace("sma(", &format!("{UNBOUND_WITNESS}("));
    assert!(
        unbound.contains(&format!("{UNBOUND_WITNESS}(")),
        "test premise: the rewrite must have applied"
    );
    assert!(
        vike_script::discover_params(&unbound).is_ok(),
        "test premise: a call to an unbound function still COMPILES — that is the whole hazard"
    );
    assert!(
        !vike_script::RHAI_INDICATORS.contains(&UNBOUND_WITNESS),
        "test premise: the witness must stay outside the host-bound set"
    );
    assert!(
        vike_indicators::get(UNBOUND_WITNESS).is_none(),
        "test premise: the witness must not be a registry indicator either"
    );

    let mut strat = RhaiStrategy::<MockBroker>::compile(&unbound).expect("still compiles");
    let mut broker = MockBroker::default();
    for bar in rising_bars(80) {
        broker.px = bar.close;
        strat.on_bar(&mut broker, &bar);
    }
    assert!(
        broker.markets.is_empty(),
        "a script calling an unbound host function must reach the broker with NOTHING — \
             otherwise `every_shipped_template_reaches_the_broker` could not detect one"
    );
}

/// A minimal, VALID sweep profile (shape errors must never be what a connect test trips on).
const PARAMSCAN_PROFILE: &str = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\nfrom = \"0\"\nto = \"100000\"\n[strategy]\nname = \"buy_hold\"\n[sweep]\nfast = [5, 10]\n";

/// The tool's OWN argument checks still fire before any connect. Profile-SHAPE errors (no
/// `[sweep]`/`[walkforward]` table, bad range, unknown strategy) deliberately moved SERVER-side
/// when these tools started shipping the TOML verbatim — one profile parser in the workspace,
/// one error source; the server's `run_paramscan_profile` arm answers them (pinned by
/// `vike-datahub`'s `run_sweep_walkforward_profile_roundtrip` test).
#[test]
fn run_tools_reject_a_missing_profile_arg_before_any_connect() {
    for tool in ["run_sweep", "run_walk_forward"] {
        let resp = call(tool, json!({}));
        assert_eq!(resp["result"]["isError"], true, "{tool}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("profile"), "{tool}: {text}");
    }
    // An unparsable `script` injection is likewise a local error (it rewrites the TOML here).
    let resp = call("run_sweep", json!({ "profile": "this is [not valid", "script": "x" }));
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn remote_run_tools_without_a_reachable_datahub_are_clean_errors() {
    // Point at a port nothing is listening on (`127.0.0.1:1` refuses immediately): each remote
    // tool's connect failure must be a clean tool error, never a panic — same pin as
    // `list_strategies_without_a_reachable_datahub_is_a_clean_error`.
    let mut s = Server { datahub_addr: "127.0.0.1:1".to_string(), ..test_server() };
    let wf_profile = format!("{PARAMSCAN_PROFILE}[walkforward]\nn_splits = 4\n");
    for (tool, args) in [
        ("run_sweep", json!({ "profile": PARAMSCAN_PROFILE })),
        ("run_walk_forward", json!({ "profile": wf_profile })),
        ("list_series", json!({})),
    ] {
        let resp =
            s.handle(&req(1, "tools/call", json!({ "name": tool, "arguments": args }))).unwrap();
        assert_eq!(resp["result"]["isError"], true, "{tool} must error, not panic");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("cannot connect"), "{tool}: {text}");
    }
}

#[test]
fn modify_without_confirm_only_previews_the_shared_wire_command() {
    // The verbs-module construction site: the SAME WireCommand::Modify the trade REPL's
    // `modify c-1 --qty 2` builds, preview-gated like every other write tool.
    let resp = call("modify", json!({ "client_order_id": "c-1", "new_qty": 2.0 }));
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["Modify"]["client_order_id"], "c-1");
    assert_eq!(sc["wire_command"]["Modify"]["new_qty"], 2.0);
    assert_eq!(sc["wire_command"]["Modify"]["new_price"], Value::Null);
    assert_eq!(sc["guardrail"]["within_limits"], true, "no order size to check");
}

#[test]
fn modify_with_nothing_to_change_is_a_tool_error() {
    let resp = call("modify", json!({ "client_order_id": "c-1" }));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("new_qty"));
}

#[test]
fn mass_cancel_preview_allows_an_omitted_scope() {
    let resp = call("mass_cancel", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["MassCancel"]["venue"], Value::Null);
    assert_eq!(sc["wire_command"]["MassCancel"]["symbol"], Value::Null);
}

#[test]
fn mass_cancel_confirm_without_a_token_is_a_preview_not_a_send() {
    let resp = call("mass_cancel", json!({ "venue": "sim", "confirm": true }));
    assert_eq!(resp["result"]["isError"], false, "no token ⇒ preview, never a send");
    assert_eq!(resp["result"]["structuredContent"]["will_execute"], false);
}

/// Every `skills/<name>/SKILL.md` teaches a procedure over THIS server's tools, and nothing but
/// the file itself says which tools those are — so this is the one place a tool change can
/// redden the skill that teaches it. The failure it guards is the M1 one: the docs described
/// the preview gate for a full day after the gate had changed and nobody noticed, because a
/// page that names a tool is never compiled against the tool. A skill is worse than a page
/// here — it is INSTALLED (`npx skills add` symlinks the file into an agent's skill
/// directory), so a stale step is not read by a human who might doubt it; it is executed by an
/// agent that will not.
///
/// What is pinned, per skill, against the Agent Skills spec and against `tools_spec`:
///   * `name` equals the directory (the spec's identity rule) and matches its pattern;
///   * `description` is 30–1024 chars — it is the TRIGGER (the spec has no separate field),
///     so an empty one is a skill that never fires and an overlong one is refused at install;
///   * every read field is a scalar a YAML parser would accept — no `: ` or ` #` inside an
///     unquoted value, no leading indicator — because this scan is not a parser and once
///     passed a description js-yaml refused (the file is the product, and it did not install);
///   * under 500 lines, the spec's ceiling;
///   * every `metadata.tools` entry is a tool this server serves — a renamed tool reddens here;
///   * every served tool the body names in backticks IS declared, so `metadata.tools` cannot
///     under-report what the skill drives (the convention this buys: backticks mean "a tool
///     this procedure calls", and a tool merely referred to is written bare);
///   * a declared WRITE tool comes with `preview_token` in the body — a skill that teaches a
///     write without the two-call gate teaches an agent to read a preview as a send;
///   * the text names no operator file the public mirror withholds (`CLAUDE.md`, `justfile`,
///     `scripts/`, `.github/`, the decision and plan trees) and pins no `.rs:NNN` line — both
///     rot silently, and neither can be followed from an install.
///
/// Read from disk at test time, deliberately not `include_str!`: the mirror ships `skills/`
/// beside `crates/`, and a compile-time embed would make every one of those markdown files a
/// build input of this crate.
///
/// ⚠ **The floor that keeps this from passing over a mis-resolved directory is DERIVED, and it
/// used to be the number ten in an `assert!` and in this doc.** That is the failure the whole
/// `skills/` tree is now generated to avoid one level down: a count written beside a set drifts
/// from it, and this one would have been wrong the moment a skill was added — while still
/// reading green, because it was a `>=`. So the property asserted instead is a POSITIVE one
/// that says the same thing without naming a number: every SUBDIRECTORY of `skills/` carries a
/// `SKILL.md`, and there is at least one. A mis-resolved path has no subdirectories and fails
/// on the second half; a directory that lost its page fails on the first.
#[test]
fn every_skill_names_only_tools_this_server_serves() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let served: Vec<String> = tools_spec()
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("skills directory {}: {e}", dir.display()))
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let mut seen = 0;
    for entry in entries {
        if !entry.path().is_dir() {
            // `skills/README.md` — the generated index — sits beside the directories.
            continue;
        }
        let path = entry.path().join("SKILL.md");
        let skill = entry.file_name().to_string_lossy().to_string();
        assert!(
            path.is_file(),
            "skills/{skill} is a directory with no SKILL.md — the Agent Skills spec identifies \
                 a skill by that file, so this one installs as nothing"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        seen += 1;
        // Frontmatter is the block between the opening `---` and the next; the body follows.
        let rest = text.strip_prefix("---\n").unwrap_or_else(|| panic!("{skill}: no frontmatter"));
        let end =
            rest.find("\n---\n").unwrap_or_else(|| panic!("{skill}: unterminated frontmatter"));
        let (front, body) = (&rest[..end], &rest[end + 5..]);
        // A top-level or `metadata:`-nested `key: value` line, as written. No YAML
        // dependency: the two shapes the spec allows here are both one line.
        let raw_field = |key: &str| -> Option<&str> {
            front
                .lines()
                .find_map(|l| l.trim_start().strip_prefix(key)?.strip_prefix(':'))
                .map(str::trim)
        };
        // ...and the same value with its quotes stripped, for the content assertions below.
        let field = |key: &str| raw_field(key).map(|v| v.trim_matches('"'));
        // ⚠ A line scan is not a YAML parser, and the gap bit once: a description quoting the
        // error text `control command not sent: Gone` passed here — it is 30–1024 characters —
        // while js-yaml (what `npx skills add` and gray-matter wrap) refused the file with
        // `bad indentation of a mapping entry`, because `: ` inside an UNQUOTED scalar ends
        // the scalar. So every field the scan reads is held to the plain-scalar rules a YAML
        // parser would apply, unless the whole value is one quoted scalar: no `: ` or ` #`
        // inside it, no trailing `:`, and no leading indicator character. Read from disk by a
        // parser this crate does not carry, the failure was an uninstallable skill behind a
        // green gate; here it is a named substring.
        let plain_scalar_hazard = |raw: &str| -> Option<String> {
            let quoted = |q: char| raw.len() >= 2 && raw.starts_with(q) && raw.ends_with(q);
            if raw.is_empty() || quoted('"') || quoted('\'') {
                return None;
            }
            for needle in [": ", " #"] {
                if raw.contains(needle) {
                    return Some(format!("contains {needle:?}"));
                }
            }
            if raw.ends_with(':') {
                return Some("ends with ':'".to_string());
            }
            let first = raw.chars().next().unwrap_or(' ');
            if "-?:,[]{}#&*!|>'\"%@`".contains(first) {
                return Some(format!("starts with the indicator {first:?}"));
            }
            None
        };
        for key in ["name", "description", "tools", "source"] {
            if let Some(raw) = raw_field(key)
                && let Some(why) = plain_scalar_hazard(raw)
            {
                panic!(
                    "{skill}: frontmatter `{key}` is an unquoted scalar that {why} — a YAML \
                         parser ends the value there and refuses the file, so the skill cannot \
                         be installed. Reword it, or quote the whole value"
                );
            }
        }
        let name = field("name").unwrap_or_else(|| panic!("{skill}: no `name`"));
        assert_eq!(name, skill, "{skill}: `name` must equal the directory name");
        let pattern_ok = !name.is_empty()
            && name.len() <= 64
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--");
        assert!(pattern_ok, "{skill}: `name` {name:?} breaks the spec's pattern");
        let desc_chars = field("description").unwrap_or_default().chars().count();
        assert!(
            (30..=1024).contains(&desc_chars),
            "{skill}: description is {desc_chars} chars, must be 30–1024 — it is the trigger"
        );
        let lines = text.lines().count();
        assert!(lines < 500, "{skill}: {lines} lines; the spec's ceiling is 500");
        let declared: Vec<&str> = field("tools").unwrap_or_default().split_whitespace().collect();
        for t in &declared {
            assert!(
                served.iter().any(|s| s == *t),
                "{skill}: metadata.tools names `{t}`, which this server does not serve \
                     (tools_spec serves {served:?})"
            );
        }
        for s in &served {
            if body.contains(&format!("`{s}`")) {
                assert!(
                    declared.contains(&s.as_str()),
                    "{skill}: the body names `{s}` but metadata.tools does not declare it — \
                         the declaration under-reports what the skill drives"
                );
            }
        }
        for t in &declared {
            if WRITE_TOOLS.contains(t) {
                assert!(
                    body.contains("preview_token"),
                    "{skill}: teaches the write tool `{t}` without `preview_token` — without \
                         the two-call gate every call is a preview, and an agent taught to read \
                         one as a send has been taught a refusal"
                );
            }
        }
        for needle in
            ["CLAUDE.md", "justfile", "scripts/", ".github/", "docs/decisions", "docs/superpowers"]
        {
            assert!(!text.contains(needle), "{skill}: names {needle}, which the mirror withholds");
        }
        for (i, line) in text.lines().enumerate() {
            let mut tail = line;
            while let Some(at) = tail.find(".rs:") {
                tail = &tail[at + 4..];
                assert!(
                    !tail.starts_with(|c: char| c.is_ascii_digit()),
                    "{skill} line {}: {:?} pins a line number — cite by symbol",
                    i + 1,
                    line.trim()
                );
            }
        }
    }
    assert!(
        seen > 0,
        "no skills under {} — an empty answer here is a wrong path, not a smaller package",
        dir.display()
    );
}
