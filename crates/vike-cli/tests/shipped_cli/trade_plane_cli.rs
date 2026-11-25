//! Shipped-binary behaviour for the `trade` plane's GROUP LAYER: a bare group, an unknown verb
//! inside a built group, an UNBUILT group refused by name, a RETIRED REPL word naming its one-shot
//! replacement, `--help` at group level, an unreachable node on a read, and a `--json` read's
//! stdout shape — plus, since stage 5 of the order-payload design deleted the CLI's own
//! labelled-book write refusal, where each labelled WRITE is refused now (the node's edge for
//! `submit` on a node advertising `account-scoped-submit`, and the client's capability gate for a
//! `submit` to a node that does not and for the three risk-reducing verbs), measured against a real
//! node — or, for the released node that discards the account, a scripted one advertising exactly
//! what those releases advertise.
//!
//! `crate::cmd::trade::plane::claim_group` and the three group routers
//! (`crate::cmd::trade::{order,position,strategy}`) already carry pure unit tests over these exact
//! classifications — this file exists because a unit test of a `Result<_, CliError>` cannot see the
//! real process EXIT CODE, the real stdout/stderr STREAMS, or the argv parsing that reaches those
//! functions in the first place. Task 10 of the trade-CLI-plane is that shipped-binary proof.
//!
//! ⚠ **Two of this task's asks were already live before this file existed, and are deliberately NOT
//! duplicated here — a duplicated assertion is not coverage, it is two things to keep in step:**
//! - [`Exit::Refused`]'s READ producer (a labelled book selector the read wire cannot address) and
//!   [`Exit::Venue`]'s `venue_refusal` case (a real paper node refusing a venue it does not run) are
//!   already asserted over the shipped binary by `tests/shipped_cli/exit_codes.rs`'s
//!   `an_unaddressable_book_is_four` and `a_node_rejection_is_five`. (This called each the rung's
//!   ONLY producer, which stopped being true for both: the client's capability refusal and the
//!   node's account refusal are asserted further down this file.) The second already spins up a
//!   genuine paper `vike-tradehub` node over the exact `StrategyCorePaper`/`server::serve` harness this
//!   file's own fixture below duplicates for its OWN cases, per that file's own note on why the
//!   fixture stays out of the shared `tests/common/mod.rs` both files declare.
//! - The PLANE-level `trade --help` (success, stdout, no leaked sentinel) is already asserted by
//!   `tests/cli_verbs/help_cli.rs`'s `the_already_correct_surfaces_stay_correct`. What that file does NOT
//!   cover is the GROUP level (`trade order --help` / `trade position --help` / `trade strategy
//!   --help`): those routes never reach `crate::cmd::trade::run`'s own `cfg.help` arm at all — they
//!   go through each group's OWN `parse` plus `crate::cmd::args::exit_for_parse_error`, a SECOND
//!   mechanism this file is the first shipped-binary proof of.
//!
//! ⚠ Every invocation redirects the settings directory ON THE CHILD (`Command::env`, never
//! `std::env::set_var`, which is unsafe under threads and leaks across this binary's parallel
//! cases), pointed at a fresh, empty temp directory — the same isolation property
//! `tests/shipped_cli/exit_codes.rs` and `tests/cli_verbs/help_cli.rs` both state: without it a run on a developer box
//! resolves the real project settings and reads a real credential store into these assertions.

use std::process::Output;

use crate::common;
use common::run;

use std::time::Duration;
use vike_cli::exit::Exit;
use vike_log::poll::wait_until;

/// The internal short-circuit token `crate::cmd::args::help_requested` carries — control flow, not
/// a diagnostic, so it must never reach a user's terminal. Redeclared here rather than imported
/// (it is `pub(crate)` inside the library): `tests/cli_verbs/help_cli.rs` redeclares the same literal for the
/// same reason.
const HELP_SENTINEL: &str = "help requested";

// -------------------------------------------------------------------------------------------
// The group layer: bare group, unknown verb inside a group, an unbuilt group, a retired word.
// -------------------------------------------------------------------------------------------

/// A bare group (no verb at all) is a usage error naming the group's OWN verb roster — never the
/// generic "unknown group" wording, because the group itself claimed fine; what is missing is a
/// VERB.
///
/// Catches: the "needs a verb" arm collapsing into the "unknown verb" arm (the answer would then be
/// an error line with no `Commands:` table, and the assertion on that table below would fail), and
/// the verb roster going stale (in which case `"submit"` would vanish from it).
#[test]
fn a_bare_group_is_a_usage_error_naming_its_verbs() {
    let out = run(&["trade", "order"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Usage as i32), "stderr: {err}");
    assert!(err.contains("submit"), "the bare-group refusal must name its verbs: {err}");
    assert!(
        err.contains("Usage: vike-cli trade order <COMMAND>"),
        "and print the group's own page: {err}"
    );
    assert!(err.contains("Commands:"), "a bare group is not an unknown one: its page: {err}");
}

/// An unknown verb INSIDE a built group is the mirror case: it names the bad word rather than
/// reporting the group as missing a verb entirely — the two "the group claimed, and then..."
/// failures must stay distinguishable from stderr alone, with no exit-code difference to lean on.
#[test]
fn an_unknown_verb_in_a_built_group_is_a_usage_error_naming_it() {
    let out = run(&["trade", "order", "not-a-real-verb"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Usage as i32), "stderr: {err}");
    assert!(err.contains("unrecognized subcommand"), "must say the verb is unknown: {err}");
    assert!(err.contains("not-a-real-verb"), "must name the offending word: {err}");
    assert!(
        err.contains("For more information, try '--help'."),
        "a bad verb is an error line, not a MISSING one's page: {err}"
    );
}

/// An UNBUILT group (`watch`) is refused BY NAME and says it is designed — never
/// reported as unknown, which is the one distinction an operator who typed it deserves: they read
/// the design table, they did not mistype a word.
#[test]
fn an_unbuilt_group_says_it_is_designed_and_does_not_read_as_unknown() {
    let out = run(&["trade", "watch", "ls"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Usage as i32), "{err}");
    assert!(err.contains("designed"), "{err}");
    assert!(
        err.contains(
            "`trade watch` is designed but not built yet - every other group `trade --help` \
             names is."
        ),
        "the shared sentence, not an unknown group: an operator who typed `watch` has read the \
         design: {err}"
    );
    assert!(
        err.contains("trade watch"),
        "must name the group itself, not a generic sentence: {err}"
    );
}

/// A retired word is read like any other word that is no group: the old spelling refuses, on the usage
/// rung, and does nothing.
#[test]
fn a_retired_verb_is_no_longer_read() {
    for word in ["doctor", "account-state", "unadmit", "mount-unlatch"] {
        let out = run(&["trade", word, "binance/ALT", "--node", "127.0.0.1:1"], &[]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(Exit::Usage as i32), "{word}: {err}");
        assert!(err.contains("unknown `trade` group"), "{word}: {err}");
    }
}

/// A RETIRED REPL spelling is refused BY NAME, with the one-shot replacement in the SAME sentence —
/// never a bare "unknown group", which would send an operator who typed a word that shipped for
/// months hunting for a typo that is not there.
#[test]
fn a_repl_word_names_its_one_shot_replacement() {
    for (retired, replacement) in
        [("submit", "trade order submit"), ("positions", "trade position ls")]
    {
        let out = run(&["trade", retired], &[]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(Exit::Usage as i32), "{retired}: {err}");
        assert!(err.contains(replacement), "{retired}: must name its replacement: {err}");
        assert!(
            err.contains(&format!(
                "`trade {retired}` is a REPL word, not a command. One-shot it is `{replacement}`."
            )),
            "{retired}: a retired word is not an unknown group: {err}"
        );
    }
}

/// A RENAMED leading word (stage 5) is refused by its id on the usage rung, naming the command that
/// replaced it — never an alias, never an unknown group.
#[test]
fn a_renamed_word_names_the_new_spelling() {
    for (was, now) in [
        ("recon", "trade reconcile"),
        ("resume-venue", "trade venue resume"),
        ("market-exit", "trade position close-all"),
        ("panic", "trade position close-all"),
        ("pos", "trade position ls"),
    ] {
        let out = run(&["trade", was, "bybit", "--node", "127.0.0.1:1"], &[]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(Exit::Usage as i32), "{was}: {err}");
        assert!(
            err.contains(&format!(
                "vike-cli trade: error [cli.trade.renamed]: `trade {was}` is now `{now}`: use the \
                 new spelling\n"
            )),
            "{was}: {err}"
        );
        assert!(out.stdout.is_empty(), "{was}: a refusal prints nothing on stdout: {out:?}");
    }
}

// -------------------------------------------------------------------------------------------
// `--help` at GROUP level — the group router's own mechanism
// (`crate::cmd::args::exit_for_parse_error`), never exercised by `tests/cli_verbs/help_cli.rs`'s
// PLANE-level case (see this file's module doc).
// -------------------------------------------------------------------------------------------

/// Each built group's `--help`/`-h` must be a success, on stdout, with no internal token leaked to
/// either stream. Catches the exact historical defect `tests/cli_verbs/help_cli.rs`'s module doc describes
/// for four OTHER commands — the group router routing its own help short-circuit into the error arm
/// instead of `exit_for_parse_error`'s success arm — for the one surface that defect's original
/// sweep never reached, because these groups did not exist yet.
#[test]
fn group_help_is_a_success_on_stdout_and_leaks_no_internal_token() {
    for group in ["order", "position", "strategy"] {
        for flag in ["--help", "-h"] {
            let out = run(&["trade", group, flag], &[]);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "trade {group} {flag}: must exit 0; stderr: {stderr}");
            assert!(
                stdout.to_lowercase().contains("usage:"),
                "trade {group} {flag}: usage must be on stdout: {stdout}"
            );
            assert!(
                !stdout.contains(HELP_SENTINEL) && !stderr.contains(HELP_SENTINEL),
                "trade {group} {flag}: leaked the internal sentinel: stdout={stdout:?} \
                 stderr={stderr:?}"
            );
        }
    }
}

// -------------------------------------------------------------------------------------------
// The CONNECT rung on a read against an unreachable node.
// -------------------------------------------------------------------------------------------

/// A READ against an unreachable node is the CONNECT rung, not a usage error: the command line
/// parsed fine (a valid bare-venue book, a syntactically valid address) and the very same
/// invocation may well work later against a live node. Port 1 on loopback refuses immediately on
/// every platform this ships to — the same fact `tests/shipped_cli/exit_codes.rs`'s own connect cases rely on.
///
/// ⚠ The child's settings database holds a DEVICE key. Without one, `order ls`
/// refuses BEFORE it ever tries to open a socket (`keys.observe_absent_message`, plain
/// `ExitCode::FAILURE` — a DIFFERENT rung), so asserting the CONNECT classification requires
/// getting a key to resolve first — exactly what `tests/shipped_cli/exit_codes.rs`'s
/// `a_node_connect_failure_is_three` does for `trade status`.
#[test]
fn a_read_with_no_node_exits_on_the_connect_rung() {
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let out = run(
        &["trade", "order", "ls", "--node", "127.0.0.1:1", "--json"],
        &[("VIKE_SETTINGS_DIR", settings.path().to_str().expect("utf-8 temp path"))],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Connect as i32), "stderr: {err}");
    assert!(
        err.contains("cannot open observe connection"),
        "must classify the SOCKET, not merely fail: {err}"
    );
    assert!(out.stdout.is_empty(), "a connect failure must print nothing to stdout: {out:?}");
}

/// **`order ls` takes the plane's book spelling and every row names its account**, over the shipped
/// binary: `bybit/DEMO` used to exit 4, and a bare `bybit` mixed DEMO and LIVE with `account: null` on
/// each row. A bare venue stays every account of it; a book naming one, `DEFAULT` included, narrows.
#[test]
fn order_ls_narrows_to_the_account_a_book_names_and_every_row_names_its_own() {
    let mut frame = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    frame.sequence = 3;
    frame.venues = serde_json::from_value(serde_json::json!([{
        "venue": "bybit", "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
        "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
        "margin_used": 0.0, "free_buying_power": 0.0, "trading_state": "Active", "positions": [],
        "route_key": "bybit#DEMO", "account": "DEMO", "tier": "demo",
    }]))
    .expect("one block");
    frame.orders = vec![
        common::wire_order("c-demo", "bybit", Some("DEMO")),
        common::wire_order("c-live", "bybit", Some("LIVE")),
        common::wire_order("c-default", "bybit", None),
        common::wire_order("c-other", "binance", None),
    ];
    let node = common::spawn_frame_node(frame).to_string();
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let dir = settings.path().to_str().expect("utf-8 temp path");
    let read = |book: &str, extra: &[&str]| {
        let mut line = vec!["trade", "order", "ls", book, "--node", node.as_str()];
        line.extend_from_slice(extra);
        let out = run(&line, &[("VIKE_SETTINGS_DIR", dir)]);
        assert_eq!(out.status.code(), Some(0), "{book}: {out:?}");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let order_client_ids_and_accounts = |book: &str| -> Vec<(String, Option<String>)> {
        let rows: serde_json::Value =
            serde_json::from_str(&read(book, &["--json"])).expect("one JSON array");
        let rows = rows.as_array().expect("an array").iter();
        rows.map(|r| {
            assert_eq!(r["account_known"], true, "{r}");
            let account = r["account"].as_str().map(str::to_string);
            (r["order_client_id"].as_str().expect("an order client id").to_string(), account)
        })
        .collect()
    };
    let row = |order_client_id: &str, account: Option<&str>| {
        (order_client_id.to_string(), account.map(str::to_string))
    };

    assert_eq!(order_client_ids_and_accounts("bybit/DEMO"), [row("c-demo", Some("DEMO"))]);
    assert_eq!(order_client_ids_and_accounts("bybit/DEFAULT"), [row("c-default", None)]);
    assert!(
        order_client_ids_and_accounts("bybit/ALT").is_empty(),
        "no order of it is an empty answer"
    );
    assert_eq!(
        order_client_ids_and_accounts("bybit"),
        [row("c-demo", Some("DEMO")), row("c-live", Some("LIVE")), row("c-default", None),]
    );

    let table = read("bybit/DEMO", &[]);
    assert!(table.starts_with("account: bybit/DEMO\n"), "{table}");
    let line = table.lines().find(|l| l.starts_with("c-demo")).expect("the row");
    assert_eq!(line.split_whitespace().nth(2), Some("DEMO"), "{table}");
    assert!(read("bybit/DEFAULT", &[]).starts_with("account: bybit/DEFAULT\n"));
    assert!(read("bybit", &[]).starts_with("account: bybit\n"));
}

/// **`position ls` of a bare venue reads every account of it, the same as `order ls`.** It read the
/// default account alone, so on a box where bybit is DEMO and LIVE it listed nothing beside an open live
/// position. A book naming one, `DEFAULT` included, narrows. The rows and their keys are unchanged.
#[test]
fn position_ls_of_a_bare_venue_reads_every_account_and_a_book_narrows() {
    let block = |venue: &str, account: Option<&str>, symbol: &str| {
        serde_json::json!({
            "venue": venue, "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
            "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
            "margin_used": 0.0, "free_buying_power": 0.0, "trading_state": "Active",
            "route_key": account.map_or(venue.to_string(), |a| format!("{venue}#{a}")),
            "account": account, "tier": "demo",
            "positions": [{
                "venue": venue, "symbol": symbol, "position_side": "BOTH", "size": 1.0,
                "average_price": 100.0, "unrealized": 0.0, "leverage": 0.0, "liquidation_price": 0.0,
            }],
        })
    };
    let mut frame = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    frame.sequence = 3;
    frame.venues = serde_json::from_value(serde_json::json!([
        block("bybit", Some("DEMO"), "ETHUSDT"),
        block("bybit", Some("LIVE"), "BTCUSDT"),
        block("bybit", None, "SOLUSDT"),
        block("binance", None, "XRPUSDT"),
    ]))
    .expect("the blocks");
    let node = common::spawn_frame_node(frame).to_string();
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let dir = settings.path().to_str().expect("utf-8 temp path");
    let read = |book: &str, extra: &[&str]| {
        let mut line = vec!["trade", "position", "ls", book, "--node", node.as_str()];
        line.extend_from_slice(extra);
        let out = run(&line, &[("VIKE_SETTINGS_DIR", dir)]);
        assert_eq!(out.status.code(), Some(0), "{book}: {out:?}");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let symbols_and_accounts = |book: &str| -> Vec<(String, Option<String>)> {
        let rows: serde_json::Value =
            serde_json::from_str(&read(book, &["--json"])).expect("one JSON array");
        let rows = rows.as_array().expect("an array").iter();
        rows.map(|r| {
            let account = r["account"].as_str().map(str::to_string);
            (r["symbol"].as_str().expect("a symbol").to_string(), account)
        })
        .collect()
    };
    let row =
        |symbol: &str, account: Option<&str>| (symbol.to_string(), account.map(str::to_string));

    assert_eq!(
        symbols_and_accounts("bybit"),
        [row("ETHUSDT", Some("DEMO")), row("BTCUSDT", Some("LIVE")), row("SOLUSDT", None)]
    );
    assert_eq!(symbols_and_accounts("bybit/LIVE"), [row("BTCUSDT", Some("LIVE"))]);
    assert_eq!(symbols_and_accounts("bybit/DEFAULT"), [row("SOLUSDT", None)]);
    assert!(symbols_and_accounts("bybit/ALT").is_empty(), "no position of it is an empty answer");

    let table = read("bybit", &[]);
    assert!(table.starts_with("account: bybit\n"), "{table}");
    let line = table.lines().find(|l| l.contains("BTCUSDT")).expect("the live position's row");
    assert_eq!(line.split_whitespace().nth(1), Some("LIVE"), "{table}");
    assert!(read("bybit/DEFAULT", &[]).starts_with("account: bybit/DEFAULT\n"));
    assert!(read("bybit/LIVE", &[]).starts_with("account: bybit/LIVE\n"));
}

// -------------------------------------------------------------------------------------------
// A book on a WRITE verb, over the SHIPPED BINARY and a REAL node — after stage 5.
//
// ⚠ This section held ONE test, `order_submit_refuses_a_labelled_book_before_anything_is_sent`, and
// it pinned the CLI's own local refusal of a labelled book (a selector function,
// `refuse_an_unroutable_account`, which refused before a key resolved or a socket opened, naming
// the node's then-missing `OrderRequest::account` as the cause). Stage 5 of
// `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` DELETED that
// refusal on 2026-09-26, once this branch carried main's node half. Deleting a refusal is only safe
// if every case it covered is still refused SOMEWHERE, and "the refusal moved" is a claim about a
// process boundary — so each case below is measured against a genuine paper `vike-tradehub`, never
// a mock that would have to invent the refusal it asserts.
// -------------------------------------------------------------------------------------------

/// Run one one-shot write against the fixture node, with the device key the fixture's settings
/// directory holds. `--yes` skips the confirm prompt (a child process has no terminal).
fn write_against(addr: &str, settings: &str, verb: &[&str]) -> Output {
    let mut args = vec!["trade"];
    args.extend_from_slice(verb);
    args.extend_from_slice(&["--node", addr, "--yes"]);
    run(&args, &[("VIKE_SETTINGS_DIR", settings)])
}

/// One order on the fixture's book, seen on a fresh observe connection: the node has now published its accounts,
/// which is what a door counting them needs.
fn warm_up(addr: std::net::SocketAddr, address_str: &str, settings_str: &str) {
    let observer = vike_tradehub_client::TradehubCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");
    let warm = write_against(
        address_str,
        settings_str,
        &["order", "submit", json_read_fixture::VENUE, json_read_fixture::TOKEN, "buy", "1"],
    );
    assert_eq!(
        warm.status.code(),
        Some(0),
        "the warm-up order must be accepted, or the fixture is wrong; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(10), || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's snapshot"
    );
}

/// A labelled `submit` naming an account the node does NOT hold is refused by the ONE ORDER GATE on this side
/// (`vike_client::order_gate::judge`, D5 of 2026-11-24: the MCP's rule, and the CLI's since), once the node's frame
/// shows the accounts it runs: [`Exit::Refused`], `order.edge.account_not_mounted`, and nothing reaches the node's
/// book. Until that day the CLI sent it and the node's edge refused it (`crates/vike-tradehub/src/server/refusal.rs`'s
/// `account_refusal`, still the backstop for a frame this side could not read). The fixture node, built from this
/// tree, advertises `account-scoped-submit`; the test after this one is the node that does not.
///
/// ⚠ **The warm-up is load-bearing, not tidiness.** That edge gate treats an EMPTY engine roster
/// as UNKNOWN rather than refusing (a core publishes only when its state goes dirty, and a gate that
/// refused on an empty roster would refuse everything a feed-less node ever received). So the case
/// runs only after a real order on the DEFAULT book has been seen on a fresh observe connection —
/// the same discipline `tests/shipped_cli/exit_codes.rs`'s `a_node_rejection_is_five` measured the need for.
///
/// Replaces half of `order_submit_refuses_a_labelled_book_before_anything_is_sent` (this section's
/// header). A HELD labelled account is the other half of stage 3's promise — routed rather than
/// refused — and a single-account fixture has no second book to prove it on; that is pinned
/// node-side, over a real two-engine core, by
/// `crates/vike-core/src/runtime/tests/mount_account/account_field.rs`'s
/// `a_labelled_order_reaches_the_named_engine_and_only_it`.
#[test]
fn a_labelled_submit_for_an_unheld_account_is_refused_by_the_one_order_gate() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let observer = vike_tradehub_client::TradehubCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");

    let warm = write_against(
        &address_str,
        settings_str,
        &["order", "submit", json_read_fixture::VENUE, json_read_fixture::TOKEN, "buy", "1"],
    );
    assert_eq!(
        warm.status.code(),
        Some(0),
        "the warm-up order on the DEFAULT book must be accepted, or the fixture is wrong rather \
         than the case under test; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(10), || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's snapshot — the roster may still be empty, so \
         the case below would test the empty-roster window rather than the edge refusal"
    );

    let labelled = format!("{}/ALT", json_read_fixture::VENUE);
    let out = write_against(
        &address_str,
        settings_str,
        &["order", "submit", labelled.as_str(), json_read_fixture::TOKEN, "buy", "1"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert_eq!(out.status.code(), Some(Exit::Refused as i32), "refused on this side\n{transcript}");
    assert!(stdout.contains("connected: CONTROL"), "{transcript}");
    assert!(
        !stdout.contains("accepted by the node"),
        "a refused command must never print acceptance\n{transcript}"
    );
    assert!(
        stderr.contains("error [order.edge.account_not_mounted]: ") && stderr.contains("`ALT`"),
        "the one order gate's refusal, naming the account the node does not run\n{transcript}"
    );
    assert_eq!(
        observer.snapshot().orders.len(),
        1,
        "only the warm-up reached the node\n{transcript}"
    );
}

/// ⚠ **A labelled `submit` to a node RELEASED before its `OrderRequest` could carry an account is
/// refused CLIENT-side — the case the stage-5 deletion first left with no refusal anywhere.** The
/// test above proves the node's edge gate over a node built from THIS tree. A review of the deletion
/// measured the fleet instead: every `vike-tradehub` from `v0.1.27` through `v0.1.32` advertises
/// `account-routing`, which the client gate then trusted, while its `lower_command` discarded the
/// account and routed the order by venue alone — so a labelled submit reached whichever book that
/// venue's one engine was and printed `accepted by the node`. Before the deletion the CLI refused
/// every labelled submit locally, against any node.
///
/// The real server cannot be built to LACK a capability it serves, so this one needs a double —
/// `tests/shipped_cli/trade_status_cli.rs`'s reason for its own scripted node. It advertises exactly what those
/// releases advertised and nothing else, and the assertions are the capability refusal's usual
/// three: the CLI connected (`connected: CONTROL`), it refused on the rung that means LOCALLY
/// ([`Exit::Refused`]), naming the capability it needs, and the node received NO command frame.
#[test]
fn a_labelled_submit_to_a_released_node_that_drops_the_account_is_refused_client_side() {
    let (addr, frames) = account_dropping_node::spawn();
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let labelled = format!("{}/ALT", json_read_fixture::VENUE);

    let out = write_against(
        &addr.to_string(),
        settings_str,
        &["order", "submit", labelled.as_str(), json_read_fixture::TOKEN, "buy", "1"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert_eq!(
        out.status.code(),
        Some(Exit::Refused as i32),
        "refused LOCALLY: this node would discard the account\n{transcript}"
    );
    assert!(stdout.contains("connected: CONTROL"), "{transcript}");
    assert!(!stdout.contains("accepted by the node"), "{transcript}");
    assert!(stderr.contains("refused CLIENT-side"), "{transcript}");
    assert!(
        stderr.contains("account-scoped-submit"),
        "must name the capability the node would have to advertise\n{transcript}"
    );
    let seen = frames
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("the scripted node never reported ({e})\n{transcript}"));
    assert!(
        !seen.iter().any(|f| f.starts_with("Command")),
        "NOTHING may be sent to a node that would drop the account, saw: {seen:?}\n{transcript}"
    );
}

/// **An order over this box's `policy.max_notional_per_order` is NOT SENT** (the owner's ruling of 2026-11-17: the CLI
/// refuses it, as the desktop does; it used to print OVER LIMIT and send it), over the SHIPPED binary and a REAL paper
/// node: the guardrail measures the order on the node's frame, the order over the cap exits on the rung that means
/// refused LOCALLY and never reaches the node, and the same order under the cap goes out. The warm-up puts the venue's
/// block on the frame: without it the order is not priced, so not checked.
#[test]
fn an_order_over_this_boxs_notional_cap_is_refused_before_the_wire() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let cap = run(
        &["config", "set", "policy.max_notional_per_order", "1"],
        &[("VIKE_SETTINGS_DIR", settings_str)],
    );
    assert!(cap.status.success(), "the cap row: {}", String::from_utf8_lossy(&cap.stderr));
    warm_up(addr, &address_str, settings_str);

    let submit = |qty: &str| {
        let argv = ["order", "submit", json_read_fixture::VENUE, json_read_fixture::TOKEN, "buy"];
        let mut argv = argv.to_vec();
        argv.extend([qty, "@0.5", "--no-wait"]);
        let out = write_against(&address_str, settings_str, &argv);
        let transcript = format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out, transcript)
    };
    let (over, transcript) = submit("10");
    assert_eq!(over.status.code(), Some(Exit::Refused as i32), "{transcript}");
    assert!(
        transcript.contains("error [order.edge.over_max_notional]: not sent: notional 5 is over")
            && transcript.contains("policy.max_notional_per_order"),
        "the refusal names the cap it is over\n{transcript}"
    );
    assert!(!transcript.contains("accepted by the node"), "{transcript}");

    let (under, transcript) = submit("1");
    assert_eq!(under.status.code(), Some(0), "an order under the cap is sent\n{transcript}");
    let observer = vike_tradehub_client::TradehubCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");
    // The warm-up is a market order (no price), so the one priced order is the order under the cap.
    assert!(
        wait_until(Duration::from_secs(10), || {
            observer.snapshot().orders.iter().any(|o| o.price.is_some())
        }),
        "the order under the cap never reached the node's snapshot"
    );
    assert!(
        !observer.snapshot().orders.iter().any(|o| o.qty > 5.0),
        "the order over the cap reached the node: {:?}",
        observer.snapshot().orders
    );
}

/// The node's NOTE on an order it accepted and sent (placed into a closed market, decision 0236) is printed
/// next to the acceptance by `trade order submit`, and in the JSON document under `notes`; an acceptance
/// with no note prints no note line and no `notes` key. The order is not held back by the CLI either way.
#[test]
fn the_nodes_note_on_an_accepted_order_is_printed_beside_the_acceptance() {
    let note = "EURUSD: the FX market is closed until Sunday 22:00 UTC: the venue may hold, refuse or queue this order";
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let submit = [
        "order",
        "submit",
        json_read_fixture::VENUE,
        json_read_fixture::TOKEN,
        "buy",
        "1",
        "--no-wait",
    ];

    let (addr, _frames) = account_dropping_node::spawn_answering(vec![note.to_string()]);
    let out = write_against(&addr.to_string(), settings_str, &submit);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let transcript = format!(
        "--- stdout ---\n{stdout}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{transcript}");
    assert!(stdout.contains("accepted by the node"), "{transcript}");
    assert!(stdout.contains(&format!("note: {note}")), "{transcript}");

    let (addr, _frames) = account_dropping_node::spawn_answering(vec![note.to_string()]);
    let mut json = submit.to_vec();
    json.push("--json");
    let out = write_against(&addr.to_string(), settings_str, &json);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout is one JSON document: {e}\n{}", String::from_utf8_lossy(&out.stdout))
    });
    assert_eq!(doc["outcome"], "accepted");
    assert_eq!(doc["notes"], serde_json::json!([note]));

    let (addr, _frames) = account_dropping_node::spawn_answering(Vec::new());
    let out = write_against(&addr.to_string(), settings_str, &submit);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("accepted by the node"), "{stdout}");
    assert!(
        !stdout.contains("note:"),
        "an acceptance with nothing to say prints no note: {stdout}"
    );
}

/// **A `--json --yes` write waits at most `MINT_FRAME_WAIT` for the frame it mints from**
/// (`crates/vike-cli/src/cmd/trade/oneshot.rs`'s `frame_read`). This double serves ONE connection, the control one, so
/// the observe handshake is never answered: the order still goes out, under a full-shape CLI id with account `00`, and
/// stderr says why, well inside the handshake's own 10 s deadline the unbounded read sat out.
#[test]
fn a_json_yes_write_mints_without_a_frame_when_the_node_does_not_answer_observe() {
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let submit = [
        "order",
        "submit",
        json_read_fixture::VENUE,
        json_read_fixture::TOKEN,
        "buy",
        "1",
        "--no-wait",
        "--json",
    ];
    let (addr, frames) = account_dropping_node::spawn_answering(Vec::new());
    let started = std::time::Instant::now();
    let out = write_against(&addr.to_string(), settings_str, &submit);
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("full shape and account 00"), "{stderr}");
    assert!(took < Duration::from_secs(8), "the mint's read is bounded: {took:?}\n{stderr}");
    let seen = frames.recv_timeout(Duration::from_secs(10)).expect("the report");
    let sent = seen.iter().find(|f| f.starts_with("Command")).expect("the order was sent");
    assert!(sent.contains("order_client_id: \"C000000"), "{sent}");
}

/// **`--reason` takes the rest of the line, so a book typed after it names nothing**:
/// `close-all --node H --yes --reason why binance` is the panic button over the whole node. It is sent,
/// never refused, and the preview says the venue word is only rationale.
#[test]
fn a_venue_in_the_reason_of_a_bookless_panic_verb_is_noted_and_never_refused() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    for verb in ["position close-all", "order mass-cancel"] {
        let mut args = vec!["trade"];
        args.extend(verb.split(' '));
        args.extend(["--node", address_str.as_str(), "--yes", "--no-wait"]);
        args.extend(["--reason", "why", "binance", "BTCUSDT"]);
        let out = run(&args, &[("VIKE_SETTINGS_DIR", settings_str)]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let transcript = format!(
            "--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(0), "the panic button is never refused\n{transcript}");
        assert!(stdout.contains("accepted by the node"), "{transcript}");
        assert!(stdout.contains("`binance` is in the --reason text"), "{verb}\n{transcript}");
        assert!(stdout.contains("EVERY venue"), "{verb}\n{transcript}");
    }
}

/// **`mass-cancel <account> --reason why BTCUSDT` cancels the whole book**, the symbol being rationale. It is
/// sent, never refused, and the preview says the symbol word is only rationale.
#[test]
fn a_symbol_in_the_reason_of_a_mass_cancel_of_a_book_is_noted_and_never_refused() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    warm_up(addr, &address_str, settings_str);
    let args = [
        "trade",
        "order",
        "mass-cancel",
        json_read_fixture::VENUE,
        "--node",
        address_str.as_str(),
        "--yes",
        "--no-wait",
        "--reason",
        "why",
        "BTCUSDT",
    ];
    let out = run(&args, &[("VIKE_SETTINGS_DIR", settings_str)]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let transcript = format!(
        "--- stdout ---\n{stdout}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "never refused\n{transcript}");
    assert!(stdout.contains("accepted by the node"), "{transcript}");
    assert!(stdout.contains("`BTCUSDT` is in the --reason text"), "{transcript}");
    assert!(stdout.contains("EVERY symbol of the book"), "{transcript}");
}

/// **`reconcile cancel` on a live row, `reconcile confirm` of an id not held and `venue resume` with a link down are
/// refused only in a note of the core's, after the Ack, and they used to print `accepted by the node`
/// (`"outcome":"accepted"`).** Against a real node, whose core refuses all three here (it holds no such id
/// and arms no link switch): each says `sent` and where the verdict is read, exits 0, keeps `order_client_id` in its
/// `--json`, and never says accepted.
#[test]
fn a_verb_the_core_decides_after_the_ack_says_sent_never_accepted() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    for verb in [
        &["reconcile", "confirm", "424242"][..],
        &["reconcile", "cancel", "424242"][..],
        &["venue", "resume", json_read_fixture::VENUE][..],
    ] {
        let out = write_against(&address_str, settings_str, verb);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let transcript = format!(
            "{verb:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(0), "the node took the command\n{transcript}");
        assert!(stdout.contains("sent: the node queued"), "{transcript}");
        assert!(!stdout.contains("accepted by the node"), "{transcript}");

        let mut json = verb.to_vec();
        json.push("--json");
        let out = write_against(&address_str, settings_str, &json);
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("stdout is one JSON document: {e}\n{}", String::from_utf8_lossy(&out.stdout))
        });
        assert_eq!(doc["outcome"], "sent", "{doc}");
        assert_eq!(doc["order_client_id"], "", "the key a script read stays: {doc}");
        assert_eq!(doc["verify"], "recent", "{doc}");
    }
}

/// What the node's Preview says the order will go out as is printed BEFORE the confirm it answers. A closed stdin
/// is a NO, so nothing is sent.
#[test]
fn the_nodes_preview_note_is_printed_before_the_confirm() {
    let note = "qty 15 goes out as 20, the venue's step is 10";
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let addr = account_dropping_node::spawn_previewing(vec![note.to_string()]).to_string();
    let out = run(
        &[
            "trade",
            "order",
            "submit",
            json_read_fixture::VENUE,
            json_read_fixture::TOKEN,
            "buy",
            "15",
            "--no-wait",
            "--node",
            &addr,
        ],
        &[("VIKE_SETTINGS_DIR", settings_str)],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let transcript = format!(
        "--- stdout ---\n{stdout}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains(&format!("note: {note}")), "{transcript}");
    assert!(!out.status.success(), "a closed stdin is a NO\n{transcript}");
    assert!(!stdout.contains("accepted by the node"), "nothing was sent\n{transcript}");
}

/// The same gap for a BRACKET: `vike-cli trade order bracket` always names a book, and a node that
/// predates `bracket-account` refuses the frame's `account` key (`deny_unknown_fields`) or, from before
/// the verb, cannot decode it at all - so the client refuses LOCALLY, naming the string it needs, and
/// the node receives no command frame. The scripted double advertises neither `bracket` nor
/// `bracket-account`, the assertions are the capability refusal's usual three.
#[test]
fn a_bracket_to_a_node_without_bracket_account_is_refused_client_side() {
    let (addr, frames) = account_dropping_node::spawn();
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let out = write_against(
        &addr.to_string(),
        settings_str,
        &[
            "order",
            "bracket",
            json_read_fixture::VENUE,
            json_read_fixture::TOKEN,
            "buy",
            "1",
            "--stop-loss",
            "0.2",
            "--take-profit",
            "0.8",
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert_eq!(
        out.status.code(),
        Some(Exit::Refused as i32),
        "refused LOCALLY: this node cannot route a bracket to an account\n{transcript}"
    );
    assert!(stdout.contains("connected: CONTROL"), "{transcript}");
    assert!(stdout.contains("preview: BRACKET buy"), "{transcript}");
    assert!(!stdout.contains("accepted by the node"), "{transcript}");
    assert!(stderr.contains("refused CLIENT-side"), "{transcript}");
    assert!(stderr.contains("bracket-account"), "must name the capability needed\n{transcript}");
    let seen = frames
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("the scripted node never reported ({e})\n{transcript}"));
    assert!(
        !seen.iter().any(|f| f.starts_with("Command")),
        "NOTHING may be sent to a node that cannot route the bracket, saw: {seen:?}\n{transcript}"
    );
}

/// ⚠ **Since 2026-11-24 (D5) an unheld account is refused on THIS side by the one order gate**, for the reducing
/// verbs as for `submit`, once the frame shows the node's accounts: [`Exit::Refused`],
/// `order.edge.account_not_mounted`, nothing sent. What follows is how the test came to be.
///
/// ⚠ **RED BY DESIGN, and rewritten rather than deleted, on 2026-09-26 when the trade-CLI plane
/// merged `main`.** This was `a_labelled_risk_reducing_verb_is_refused_client_side_before_anything_is_sent`,
/// asserting every node built from this tree lacked `account-scoped-reduce`. Owner ruling "B" made
/// that false: `served_features` now advertises it because `vike_core`'s reducing arms genuinely
/// narrow to the named account. So a labelled `mass-cancel` / `flatten` / `close-all` now flies past
/// the client gate, and an UNHELD account is exactly the node-edge refusal
/// `a_labelled_submit_for_an_unheld_account_is_refused_by_the_one_order_gate` proves for `submit`
/// — this is that test's reduce-plane twin, over the same warm-up discipline for the same reason
/// (an empty roster reads UNKNOWN, not refused, so the case must run after a real order has been
/// seen).
///
/// The scripted-node CLIENT-side refusal this test used to prove did not vanish; it moved to
/// [`a_labelled_reducing_verb_to_a_released_node_that_drops_the_account_is_refused_client_side`],
/// reduce's twin of the submit-plane's own release-fleet test, because the real server built from
/// this tree cannot be made to lack a capability it serves.
#[test]
fn a_labelled_reducing_verb_for_an_unheld_account_is_refused_by_the_one_order_gate() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let observer = vike_tradehub_client::TradehubCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");

    let warm = write_against(
        &address_str,
        settings_str,
        &["order", "submit", json_read_fixture::VENUE, json_read_fixture::TOKEN, "buy", "1"],
    );
    assert_eq!(
        warm.status.code(),
        Some(0),
        "the warm-up order on the DEFAULT book must be accepted, or the fixture is wrong rather \
         than the case under test; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(10), || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's snapshot — the roster may still be empty, so \
         the case below would test the empty-roster window rather than the edge refusal"
    );

    let labelled = format!("{}/ALT", json_read_fixture::VENUE);
    let cases: [(&[&str], &str); 3] = [
        (&["order", "mass-cancel", labelled.as_str()], "The mass-cancel"),
        (&["position", "flatten", labelled.as_str(), json_read_fixture::TOKEN], "The flatten"),
        (&["position", "close-all", labelled.as_str()], "The market exit"),
    ];
    for (verb, subject) in cases {
        let out = write_against(&address_str, settings_str, verb);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let transcript = format!("{verb:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

        assert_eq!(
            out.status.code(),
            Some(Exit::Refused as i32),
            "refused on this side ({subject})\n{transcript}"
        );
        assert!(stdout.contains("connected: CONTROL"), "{transcript}");
        assert!(
            !stdout.contains("accepted by the node"),
            "a refused command must never print acceptance\n{transcript}"
        );
        assert!(
            stderr.contains("error [order.edge.account_not_mounted]: ") && stderr.contains("`ALT`"),
            "the one order gate's refusal, naming the account the node does not run\n{transcript}"
        );
    }
}

/// The CLIENT-side half of the same gap the submit-plane's own
/// `a_labelled_submit_to_a_released_node_that_drops_the_account_is_refused_client_side` closes:
/// every `vike-tradehub` released before this tree's reducing arms learned to narrow advertises
/// `account-routing` and `mount-account` and NOT `account-scoped-reduce` (the same
/// `account_dropping_node` double proves both gaps, since it never advertised either submit's or
/// reduce's scoped string). A labelled reduce sent to that node would be decoded fine and fanned
/// out over every account of the venue — the exact harm owner ruling "B" closed — so the client
/// must refuse locally rather than trust a node this old.
#[test]
fn a_labelled_reducing_verb_to_a_released_node_that_drops_the_account_is_refused_client_side() {
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let labelled = format!("{}/ALT", json_read_fixture::VENUE);

    let cases: [&[&str]; 3] = [
        &["order", "mass-cancel", labelled.as_str()],
        &["position", "flatten", labelled.as_str(), json_read_fixture::TOKEN],
        &["position", "close-all", labelled.as_str()],
    ];
    for verb in cases {
        // A fresh double per case: `account_dropping_node::spawn` accepts exactly ONE connection
        // (the shape the submit-plane's own single-command test needs), so three verbs sharing one
        // spawn would see "Connection refused" on the second and third.
        let (addr, frames) = account_dropping_node::spawn();
        let out = write_against(&addr.to_string(), settings_str, verb);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let transcript = format!("{verb:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

        assert_eq!(
            out.status.code(),
            Some(Exit::Refused as i32),
            "refused LOCALLY: this node would discard the account\n{transcript}"
        );
        assert!(stdout.contains("connected: CONTROL"), "{transcript}");
        assert!(!stdout.contains("accepted by the node"), "{transcript}");
        assert!(stderr.contains("refused CLIENT-side"), "{transcript}");
        assert!(
            stderr.contains("account-scoped-reduce"),
            "must name the capability this node would have to advertise\n{transcript}"
        );
        let seen = frames
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("the scripted node never reported ({e})\n{transcript}"));
        assert!(
            !seen.iter().any(|f| f.starts_with("Command")),
            "NOTHING may be sent to a node that would drop the account, saw: {seen:?}\n{transcript}"
        );
    }
}

/// ⚠ **RED BY DESIGN, and rewritten on 2026-09-26 for the same reason as the test above.** This was
/// `a_bare_venue_flatten_meets_the_same_client_gate_today`, pinning a bare venue's `flatten` as
/// unsendable because the grammar names `DEFAULT` positively and no node advertised the capability
/// that positive name would need. A node built from this tree now does, and the fixture's mount
/// HOLDS `DEFAULT`, so the frame is sent and accepted — the reopener its own doc named ("the day a
/// node advertises the capability") is the day this merged.
#[test]
fn a_bare_venue_flatten_now_flows_to_a_node_that_honours_the_field() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    warm_up(addr, &address_str, settings_str);

    let out = write_against(
        &address_str,
        settings_str,
        &["position", "flatten", json_read_fixture::VENUE, json_read_fixture::TOKEN],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert_eq!(
        out.status.code(),
        Some(0),
        "the fixture's venue has ONE account, so a bare venue resolves to it\n{transcript}"
    );
    assert!(stdout.contains("connected: CONTROL"), "{transcript}");
    assert!(stdout.contains("accepted by the node"), "{transcript}");
    assert!(stderr.is_empty(), "{transcript}");
}

/// **A bare venue names no account, and before the node has published its accounts it cannot tell how many that
/// venue runs**, so a bare reducing verb on a fresh node is refused by the node, by name, and the same verb naming the
/// book goes. Decision 0254: the one-shot relies on the door, on the [`Exit::Venue`] rung, and sends no `DEFAULT`
/// of its own.
#[test]
fn a_bare_reducing_verb_before_the_first_publish_is_refused_by_the_node_and_the_named_book_goes() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let out = write_against(&address_str, settings_str, &["position", "close-all", "polymarket"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert_eq!(out.status.code(), Some(Exit::Venue as i32), "{transcript}");
    assert!(stdout.contains("connected: CONTROL"), "{transcript}");
    assert!(stderr.contains("has not published its accounts"), "{transcript}");
    assert!(!stdout.contains("accepted by the node"), "{transcript}");

    let out =
        write_against(&address_str, settings_str, &["position", "close-all", "polymarket/DEFAULT"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}

/// **A node without the door would fan a bare venue over every account**, so the client sends it nothing and names
/// the word it would have to advertise (decision 0254). The node-wide panic button owes no word.
#[test]
fn a_bare_reducing_verb_to_a_node_without_the_door_is_refused_client_side() {
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let cases: [&[&str]; 3] = [
        &["order", "mass-cancel", json_read_fixture::VENUE],
        &["position", "flatten", json_read_fixture::VENUE, json_read_fixture::TOKEN],
        &["position", "close-all", json_read_fixture::VENUE],
    ];
    for verb in cases {
        let (addr, frames) = account_dropping_node::spawn();
        let out = write_against(&addr.to_string(), settings_str, verb);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(Exit::Refused as i32), "{verb:?}: {stderr}");
        assert!(stderr.contains("one-account-venue"), "{verb:?}: {stderr}");
        assert!(stderr.contains("/DEFAULT"), "the spelling that works is named: {stderr}");
        let seen = frames.recv_timeout(std::time::Duration::from_secs(10)).expect("the report");
        assert!(!seen.iter().any(|f| f.starts_with("Command")), "{verb:?} sent: {seen:?}");
    }
    let (addr, frames) = account_dropping_node::spawn();
    let out = write_against(&addr.to_string(), settings_str, &["position", "close-all"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen = frames.recv_timeout(std::time::Duration::from_secs(10)).expect("the report");
    assert!(seen.iter().any(|f| f.starts_with("Command")), "the button was not sent: {seen:?}");
}

/// **A bare `submit` to a node from before the door Acks and places nothing**: the node hands an account-less order
/// on a venue of several accounts to its core, whose ambiguity refusal is only a note, so the CLI printed `accepted`,
/// exit 0. It is refused here, nothing sent, naming the spelling that works there.
#[test]
fn a_bare_submit_to_a_node_without_the_door_is_refused_client_side() {
    let settings = json_read_fixture::settings_dir();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let submit =
        ["order", "submit", json_read_fixture::VENUE, json_read_fixture::TOKEN, "buy", "1"];

    let (addr, frames) = account_dropping_node::spawn_scoped_without_the_door();
    let out = write_against(&addr.to_string(), settings_str, &submit);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Refused as i32), "{stderr}");
    assert!(stderr.contains("one-account-venue") && stderr.contains("/DEFAULT"), "{stderr}");
    let seen = frames.recv_timeout(std::time::Duration::from_secs(10)).expect("the report");
    assert!(!seen.iter().any(|f| f.starts_with("Command")), "sent: {seen:?}");

    let named = format!("{}/DEFAULT", json_read_fixture::VENUE);
    let (addr, frames) = account_dropping_node::spawn_scoped_without_the_door();
    let verb = ["order", "submit", &named, json_read_fixture::TOKEN, "buy", "1", "--no-wait"];
    let out = write_against(&addr.to_string(), settings_str, &verb);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let seen = frames.recv_timeout(std::time::Duration::from_secs(10)).expect("the report");
    assert!(seen.iter().any(|f| f.starts_with("Command")), "the named book goes: {seen:?}");
}

// -------------------------------------------------------------------------------------------
// A `--json` READ against a REAL node.
// -------------------------------------------------------------------------------------------

/// A minimal real paper `vike-tradehub` node: `tests/common/mod.rs`'s `spawn_paper_node` (the one
/// `tests/shipped_cli/exit_codes.rs`'s `venue_reject_fixture` uses too) over this file's venue, token
/// and keys.
mod json_read_fixture {
    use std::net::SocketAddr;

    use vike_tradehub_core::StrategyCorePaper;

    pub(super) const TOKEN: &str = "TRADE_PLANE_CLI_JSON_TOKEN";
    /// The `venue` this fixture hands `vike_tradehub_core::test_support::paper_maker::paper_maker_core` — the book the warm-up order
    /// lands under, and the book this file's read narrows to.
    pub(super) const VENUE: &str = "polymarket";
    pub(super) const OBSERVE_KEY: &str = "DUMMY-observe-key-for-trade-plane-cli-json";
    pub(super) const CONTROL_KEY: &str = "DUMMY-control-key-for-trade-plane-cli-json";

    pub(super) fn spawn() -> (StrategyCorePaper, SocketAddr) {
        crate::common::spawn_paper_node(VENUE, TOKEN, OBSERVE_KEY, CONTROL_KEY)
    }

    /// The throwaway `<project>/settings` `VIKE_SETTINGS_DIR` is pointed at, its settings database
    /// holding the CONTROL-role device key.
    pub(super) fn settings_dir() -> tempfile::TempDir {
        crate::common::device_settings_dir(CONTROL_KEY)
    }
}

/// A SCRIPTED node advertising exactly what a released `vike-tradehub` that DISCARDS a `Submit`'s
/// account advertises — `account-routing` among the rest, and no `account-scoped-submit`. The
/// capability set is `served_features`' own list, which is identical from `v0.1.27` through
/// `v0.1.34` (measured over the tags); what changed at `v0.1.33` is BEHAVIOUR — the account started
/// reaching the order — with no string to say so, which is why a string the fleet already ships
/// could not be repaired and a new one had to be issued.
///
/// It accepts the mac unseen (the CLI signs honestly; what this double exists to observe is what
/// comes AFTER the auth), answers a `Command` with an `Ack` so a regression reads as a success
/// rather than a hang, and reports every post-auth frame once the client hangs up.
mod account_dropping_node {
    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc;
    use std::thread;

    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    use vike_tradehub_client::proto::{
        FEATURE_ACCOUNT_ROUTING, FEATURE_ACCOUNT_SCOPED_SUBMIT, FEATURE_MOUNT_ACCOUNT,
        FEATURE_MOUNT_CLASS, FEATURE_MOUNT_VERBS, FEATURE_OBSERVE_HEARTBEAT,
        FEATURE_ONE_ACCOUNT_VENUE, FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE,
        FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS, FEATURE_VENUE_ROUTING,
    };
    use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response};

    /// What the released node advertises; with `scoped`, also `account-scoped-submit` (a node that
    /// ROUTES a submit's account, which a plain `submit` needs: the noting node below).
    fn features(scoped: bool) -> Vec<String> {
        let mut features: Vec<String> = [
            "observe",
            "subscribe",
            "snapshot",
            "preview",
            FEATURE_STRATEGY_VERBS,
            FEATURE_SETTINGS_SHOW,
            FEATURE_SETTINGS_WRITE,
            FEATURE_MOUNT_VERBS,
            FEATURE_MOUNT_ACCOUNT,
            FEATURE_OBSERVE_HEARTBEAT,
            FEATURE_STRATEGY_PARAMS,
            FEATURE_MOUNT_CLASS,
            FEATURE_VENUE_ROUTING,
            FEATURE_ACCOUNT_ROUTING,
            vike_hub_protocol::access::FEATURE_PERSON_AUTH,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if scoped {
            features.push(FEATURE_ACCOUNT_SCOPED_SUBMIT.to_string());
            features.push(FEATURE_ONE_ACCOUNT_VENUE.to_string());
        }
        features
    }

    /// One connection. The channel carries the Debug rendering of every post-auth frame, and is
    /// dropped unsent if the handshake desyncs — which the caller reads as a failure.
    pub(super) fn spawn() -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
        spawn_with(false, Vec::new())
    }

    /// A node that ROUTES a submit's account (it advertises `account-scoped-submit`, so a plain `submit` is
    /// sent) and answers it with an `Ack` carrying `notes` (`Response::Ack`'s `notes`): the node of the
    /// closed-market note.
    pub(super) fn spawn_answering(notes: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
        spawn_with(true, notes)
    }

    /// A node that routes a submit's account and predates the door's one-account rule: every release until it.
    pub(super) fn spawn_scoped_without_the_door() -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
        spawn_features(
            features(true).into_iter().filter(|f| f != FEATURE_ONE_ACCOUNT_VENUE).collect(),
            Vec::new(),
        )
    }

    /// A node that serves every connection (a write that stops at its confirm opens several) and answers a
    /// `Preview` with `notes`.
    pub(super) fn spawn_previewing(notes: Vec<String>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let notes = notes.clone();
                thread::spawn(move || {
                    let Ok(HandshakeFrame::Hello { .. }) =
                        read_frame::<_, HandshakeFrame>(&mut stream)
                    else {
                        return;
                    };
                    let welcome = HandshakeReply::Welcome {
                        proto_version: NODE_PROTO_VERSION,
                        features: features(true),
                        nonce: Some([7u8; 32]),
                    };
                    let Ok(()) = write_frame(&mut stream, &welcome) else { return };
                    let Ok(
                        HandshakeFrame::Auth { scope, .. }
                        | HandshakeFrame::PersonAuth { scope, .. },
                    ) = read_frame::<_, HandshakeFrame>(&mut stream)
                    else {
                        return;
                    };
                    let Ok(()) =
                        write_frame(&mut stream, &HandshakeReply::AuthOk { scope, admitted: None })
                    else {
                        return;
                    };
                    while let Ok(req) = read_frame::<_, Request>(&mut stream) {
                        let reply = match req {
                            Request::Preview(_) => Response::Preview {
                                accepted: true,
                                reason: None,
                                refusal_id: None,
                                notes: notes.clone(),
                            },
                            Request::Ping => Response::Pong,
                            _ => Response::Error("unscripted request".to_string()),
                        };
                        if write_frame(&mut stream, &reply).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    fn spawn_with(scoped: bool, notes: Vec<String>) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
        spawn_features(features(scoped), notes)
    }

    fn spawn_features(
        features: Vec<String>,
        notes: Vec<String>,
    ) -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            if !matches!(
                read_frame::<_, HandshakeFrame>(&mut stream),
                Ok(HandshakeFrame::Hello { .. })
            ) {
                return;
            }
            let welcome = HandshakeReply::Welcome {
                proto_version: NODE_PROTO_VERSION,
                features,
                nonce: Some([7u8; 32]),
            };
            if write_frame(&mut stream, &welcome).is_err() {
                return;
            }
            let Ok(HandshakeFrame::Auth { scope, .. } | HandshakeFrame::PersonAuth { scope, .. }) =
                read_frame::<_, HandshakeFrame>(&mut stream)
            else {
                return;
            };
            if write_frame(&mut stream, &HandshakeReply::AuthOk { scope, admitted: None }).is_err()
            {
                return;
            }
            let mut seen = Vec::new();
            while let Ok(req) = read_frame::<_, Request>(&mut stream) {
                seen.push(format!("{req:?}"));
                let reply = match req {
                    Request::Command { .. } => Response::Ack {
                        order_client_id: String::new(),
                        notes: notes.clone(),
                        emulated: None,
                    },
                    Request::Ping => Response::Pong,
                    _ => Response::Error("unscripted request".to_string()),
                };
                if write_frame(&mut stream, &reply).is_err() {
                    break;
                }
            }
            let _ = tx.send(seen);
        });
        (addr, rx)
    }
}

/// `trade order ls <account> --node <addr> --json` against a real node holding one real order: stdout
/// must be ONE parseable JSON document naming that order, with nothing else mixed into it.
///
/// The order is placed by a genuine one-shot `submit` first, and its arrival is confirmed off a
/// FRESH observe connection before the read under test runs — otherwise this could race the node's
/// first-publish window and read an empty snapshot (see `tests/shipped_cli/exit_codes.rs`'s
/// `a_node_rejection_is_five` doc, which measured the identical race directly on the write side).
///
/// Catches: a regression that makes `order::run_ls` share the REPL session's own
/// `println!("connected: OBSERVE …")` diagnostic — that line would land on stdout ahead of the JSON
/// array and break `serde_json::from_str` on the WHOLE of stdout, which a mere `contains("[")` check
/// could not detect. Also catches the JSON rows silently losing a field (each is asserted against
/// what was actually submitted, not merely "the array is non-empty").
#[test]
fn a_json_read_produces_one_parseable_stdout_document_and_no_diagnostics() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let observer = vike_tradehub_client::TradehubCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");

    let submit = run(
        &[
            "trade",
            "order",
            "submit",
            json_read_fixture::VENUE,
            json_read_fixture::TOKEN,
            "buy",
            "1",
            "--node",
            &address_str,
            "--yes",
        ],
        &[("VIKE_SETTINGS_DIR", settings_str)],
    );
    assert_eq!(
        submit.status.code(),
        Some(0),
        "the fixture's own warm-up order must be accepted, or this test would prove nothing about \
         the read below; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&submit.stdout),
        String::from_utf8_lossy(&submit.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(10), || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's own published snapshot"
    );

    let out = run(
        &["trade", "order", "ls", json_read_fixture::VENUE, "--node", &address_str, "--json"],
        &[("VIKE_SETTINGS_DIR", settings_str)],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.is_empty(), "a successful read must print no diagnostics: {stderr}");

    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "stdout must be ONE parseable JSON document with nothing else mixed in (e.g. a \
             REPL-style \"connected: …\" line would break exactly this parse): {e}\nstdout: {stdout}"
        )
    });
    let rows = doc.as_array().expect("rows_json always renders a top-level array");
    assert!(!rows.is_empty(), "the submitted order must be visible: {stdout}");
    let row = &rows[0];
    assert_eq!(row["venue"], json_read_fixture::VENUE);
    assert_eq!(row["symbol"], json_read_fixture::TOKEN);
    assert_eq!(row["side"], "buy");
    assert_eq!(row["qty"], 1.0);
    assert!(
        row["order_client_id"].as_str().is_some_and(|c| !c.is_empty()),
        "a real order client id: {stdout}"
    );
}
