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
//!   already asserted over the shipped binary by `tests/exit_codes.rs`'s
//!   `an_unaddressable_book_is_four` and `a_node_rejection_is_five`. (This called each the rung's
//!   ONLY producer, which stopped being true for both: the client's capability refusal and the
//!   node's account refusal are asserted further down this file.) The second already spins up a
//!   genuine paper `vike-tradehub` node over the exact `MakerMount`/`server::serve` harness this
//!   file's own fixture below duplicates for its OWN cases, per that file's own note on why the
//!   fixture stays out of the shared `tests/common/mod.rs` both files declare.
//! - The PLANE-level `trade --help` (success, stdout, no leaked sentinel) is already asserted by
//!   `tests/help_cli.rs`'s `the_already_correct_surfaces_stay_correct`. What that file does NOT
//!   cover is the GROUP level (`trade order --help` / `trade position --help` / `trade strategy
//!   --help`): those routes never reach `crate::cmd::trade::run`'s own `cfg.help` arm at all — they
//!   go through each group's OWN `parse` plus `crate::cmd::args::exit_for_parse_error`, a SECOND
//!   mechanism this file is the first shipped-binary proof of.
//!
//! ⚠ Every invocation redirects the settings directory ON THE CHILD (`Command::env`, never
//! `std::env::set_var`, which is unsafe under threads and leaks across this binary's parallel
//! cases), pointed at a fresh, empty temp directory — the same isolation property
//! `tests/exit_codes.rs` and `tests/help_cli.rs` both state: without it a run on a developer box
//! resolves the real project settings and reads a real credential store into these assertions.

use std::process::Output;

mod common;
use common::{run, wait_until};

use vike_cli::exit::Exit;

/// The internal short-circuit token `crate::cmd::args::help_requested` carries — control flow, not
/// a diagnostic, so it must never reach a user's terminal. Redeclared here rather than imported
/// (it is `pub(crate)` inside the library): `tests/help_cli.rs` redeclares the same literal for the
/// same reason.
const HELP_SENTINEL: &str = "help requested";

// -------------------------------------------------------------------------------------------
// The group layer: bare group, unknown verb inside a group, an unbuilt group, a retired word.
// -------------------------------------------------------------------------------------------

/// A bare group (no verb at all) is a usage error naming the group's OWN verb roster — never the
/// generic "unknown group" wording, because the group itself claimed fine; what is missing is a
/// VERB.
///
/// Catches: the "needs a verb" arm collapsing into the "unknown verb" arm (the message would then
/// read "unknown ... verb ''" and the `!contains("unknown")` assertion below would fail), and the
/// verb roster going stale (in which case `"submit"` would vanish from it).
#[test]
fn a_bare_group_is_a_usage_error_naming_its_verbs() {
    let out = run(&["trade", "order"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Usage as i32), "stderr: {err}");
    assert!(err.contains("submit"), "the bare-group refusal must name its verbs: {err}");
    assert!(err.contains("needs a verb"), "and say what is missing, not merely fail: {err}");
    assert!(!err.contains("unknown"), "a bare group is not an unknown one: {err}");
}

/// An unknown verb INSIDE a built group is the mirror case: it names the bad word rather than
/// reporting the group as missing a verb entirely — the two "the group claimed, and then..."
/// failures must stay distinguishable from stderr alone, with no exit-code difference to lean on.
#[test]
fn an_unknown_verb_in_a_built_group_is_a_usage_error_naming_it() {
    let out = run(&["trade", "order", "not-a-real-verb"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Usage as i32), "stderr: {err}");
    assert!(err.contains("unknown"), "must say the verb is unknown: {err}");
    assert!(err.contains("not-a-real-verb"), "must name the offending word: {err}");
    assert!(!err.contains("needs a verb"), "a bad verb is not a MISSING one: {err}");
}

/// An UNBUILT group (`account`, `watch`) is refused BY NAME and says it is designed — never
/// reported as unknown, which is the one distinction an operator who typed it deserves: they read
/// the design table, they did not mistype a word.
#[test]
fn an_unbuilt_group_says_it_is_designed_and_does_not_read_as_unknown() {
    for group in ["account", "watch"] {
        let out = run(&["trade", group, "ls"], &[]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(Exit::Usage as i32), "group {group}: {err}");
        assert!(err.contains("designed"), "group {group}: {err}");
        assert!(
            !err.contains("unknown"),
            "an operator who typed `{group}` has read the design: {err}"
        );
        assert!(
            err.contains(&format!("trade {group}")),
            "must name the group itself, not a generic sentence: {err}"
        );
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
            !err.contains("unknown `trade` group"),
            "{retired}: a retired word is not an unknown group: {err}"
        );
    }
}

// -------------------------------------------------------------------------------------------
// `--help` at GROUP level — the group router's own mechanism
// (`crate::cmd::args::exit_for_parse_error`), never exercised by `tests/help_cli.rs`'s
// PLANE-level case (see this file's module doc).
// -------------------------------------------------------------------------------------------

/// Each built group's `--help`/`-h` must be a success, on stdout, with no internal token leaked to
/// either stream. Catches the exact historical defect `tests/help_cli.rs`'s module doc describes
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
                stdout.contains("usage:"),
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
/// every platform this ships to — the same fact `tests/exit_codes.rs`'s own connect cases rely on.
///
/// ⚠ The OBSERVE key is supplied on the child. Without one, `order ls` refuses BEFORE it ever tries
/// to open a socket (`keys.observe_absent_message`, plain `ExitCode::FAILURE` — a DIFFERENT rung),
/// so asserting the CONNECT classification requires getting a key to resolve first — exactly what
/// `tests/exit_codes.rs`'s `a_node_connect_failure_is_three` does for `trade status`.
#[test]
fn a_read_with_no_node_exits_on_the_connect_rung() {
    let out = run(
        &["trade", "order", "ls", "--node", "127.0.0.1:1", "--json"],
        &[("VIKE_TRADEHUB_OBSERVE_KEY", "not-a-real-key")],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(Exit::Connect as i32), "stderr: {err}");
    assert!(
        err.contains("cannot open observe connection"),
        "must classify the SOCKET, not merely fail: {err}"
    );
    assert!(out.stdout.is_empty(), "a connect failure must print nothing to stdout: {out:?}");
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

/// Run one one-shot write against the fixture node, with the node keys the fixture's settings
/// directory holds. `--yes` skips the confirm prompt (a child process has no terminal).
fn write_against(addr: &str, settings: &str, verb: &[&str]) -> Output {
    let mut args = vec!["trade"];
    args.extend_from_slice(verb);
    args.extend_from_slice(&["--node", addr, "--yes"]);
    run(&args, &[("VIKE_SETTINGS_DIR", settings)])
}

/// A labelled `submit` naming an account the node does NOT hold is refused by the NODE, at its
/// edge, before the Ack — not by this CLI. Two-sided: the CLI must have connected and sent
/// (`connected: CONTROL` on stdout — a local refusal prints nothing there), and the refusal must be
/// the node's own words (`crates/vike-tradehub/src/server/refusal.rs`'s `account_refusal`), folded onto
/// [`Exit::Venue`], the rung that means the far side spoke. The frame gets that far because the
/// fixture node, built from this tree, advertises `account-scoped-submit`; the test after this one
/// is the node that does not.
///
/// ⚠ **The warm-up is load-bearing, not tidiness.** That edge gate treats an EMPTY engine roster
/// as UNKNOWN rather than refusing (a core publishes only when its state goes dirty, and a gate that
/// refused on an empty roster would refuse everything a feed-less node ever received). So the case
/// runs only after a real order on the DEFAULT book has been seen on a fresh observe connection —
/// the same discipline `tests/exit_codes.rs`'s `a_node_rejection_is_five` measured the need for.
///
/// Replaces half of `order_submit_refuses_a_labelled_book_before_anything_is_sent` (this section's
/// header). A HELD labelled account is the other half of stage 3's promise — routed rather than
/// refused — and a single-account fixture has no second book to prove it on; that is pinned
/// node-side, over a real two-engine core, by
/// `crates/vike-core/src/runtime/tests/mount_account/account_field.rs`'s
/// `a_labelled_order_reaches_the_named_engine_and_only_it`.
#[test]
fn a_labelled_submit_for_an_unheld_account_is_refused_by_the_node_not_the_cli() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let addr_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let observer = vike_tradehub_client::RemoteCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");

    let warm = write_against(
        &addr_str,
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
        wait_until(10, || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's snapshot — the roster may still be empty, so \
         the case below would test the empty-roster window rather than the edge refusal"
    );

    let labelled = format!("{}/ALT", json_read_fixture::VENUE);
    let out = write_against(
        &addr_str,
        settings_str,
        &["order", "submit", labelled.as_str(), json_read_fixture::TOKEN, "buy", "1"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert_eq!(
        out.status.code(),
        Some(Exit::Venue as i32),
        "an unheld account is the NODE's refusal now, so it is the rung that means the far side \
         spoke\n{transcript}"
    );
    assert!(
        stdout.contains("connected: CONTROL"),
        "the CLI must have connected and sent — a LOCAL refusal (the deleted one) printed nothing \
         here\n{transcript}"
    );
    assert!(
        !stdout.contains("accepted by the node"),
        "a refused command must never print acceptance\n{transcript}"
    );
    assert!(stderr.contains("node rejected the command"), "{transcript}");
    assert!(
        stderr.contains("runs no account `ALT`"),
        "the refusal must be the node's own edge gate naming the account it does not hold\n\
         {transcript}"
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
/// `tests/trade_status_cli.rs`'s reason for its own scripted node. It advertises exactly what those
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

/// ⚠ **RED BY DESIGN, and rewritten rather than deleted, on 2026-09-26 when the trade-CLI plane
/// merged `main`.** This was `a_labelled_risk_reducing_verb_is_refused_client_side_before_anything_is_sent`,
/// asserting every node built from this tree lacked `account-scoped-reduce`. Owner ruling "B" made
/// that false: `served_features` now advertises it because `vike_core`'s reducing arms genuinely
/// narrow to the named account. So a labelled `mass-cancel` / `flatten` / `close-all` now flies past
/// the client gate, and an UNHELD account is exactly the node-edge refusal
/// `a_labelled_submit_for_an_unheld_account_is_refused_by_the_node_not_the_cli` proves for `submit`
/// — this is that test's reduce-plane twin, over the same warm-up discipline for the same reason
/// (an empty roster reads UNKNOWN, not refused, so the case must run after a real order has been
/// seen).
///
/// The scripted-node CLIENT-side refusal this test used to prove did not vanish; it moved to
/// [`a_labelled_reducing_verb_to_a_released_node_that_drops_the_account_is_refused_client_side`],
/// reduce's twin of the submit-plane's own release-fleet test, because the real server built from
/// this tree cannot be made to lack a capability it serves.
#[test]
fn a_labelled_reducing_verb_for_an_unheld_account_is_refused_by_the_node_not_the_cli() {
    let (_mount, addr) = json_read_fixture::spawn();
    let settings = json_read_fixture::settings_dir();
    let addr_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");
    let observer = vike_tradehub_client::RemoteCoreHandle::connect(
        addr,
        json_read_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");

    let warm = write_against(
        &addr_str,
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
        wait_until(10, || !observer.snapshot().orders.is_empty()),
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
        let out = write_against(&addr_str, settings_str, verb);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let transcript = format!("{verb:?}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

        assert_eq!(
            out.status.code(),
            Some(Exit::Venue as i32),
            "an unheld account is the NODE's refusal now, so it is the rung that means the far \
             side spoke\n{transcript}"
        );
        assert!(
            stdout.contains("connected: CONTROL"),
            "the CLI must have connected and sent — a LOCAL refusal printed nothing here\n\
             {transcript}"
        );
        assert!(
            !stdout.contains("accepted by the node"),
            "a refused command must never print acceptance\n{transcript}"
        );
        assert!(stderr.contains("node rejected the command"), "{transcript}");
        assert!(
            stderr.contains("runs no account `ALT`"),
            "the refusal must be the node's own edge gate naming the account it does not \
             hold\n{transcript}"
        );
        assert!(
            stderr.contains(subject),
            "the refusal must name which verb this was ({subject})\n{transcript}"
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
    let addr_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let out = write_against(
        &addr_str,
        settings_str,
        &["position", "flatten", json_read_fixture::VENUE, json_read_fixture::TOKEN],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert_eq!(
        out.status.code(),
        Some(0),
        "DEFAULT is a HELD account and the node now narrows to it — this must succeed\n{transcript}"
    );
    assert!(stdout.contains("connected: CONTROL"), "{transcript}");
    assert!(stdout.contains("accepted by the node"), "{transcript}");
    assert!(stderr.is_empty(), "{transcript}");
}

// -------------------------------------------------------------------------------------------
// A `--json` READ against a REAL node.
// -------------------------------------------------------------------------------------------

/// A minimal real paper `vike-tradehub` node, spawned the same way `tests/exit_codes.rs`'s
/// `venue_reject_fixture` and `tests/trade_node_e2e.rs` both do — duplicated rather than moved into
/// `tests/common/mod.rs`, which this file declares for its runners: every binary that declares that
/// module compiles all of it (the same tradeoff `tests/exit_codes.rs`'s own fixture doc states).
mod json_read_fixture {
    use std::net::{SocketAddr, TcpListener};
    use std::thread;

    use vike_mount::{MakerMount, MakerMountConfig, build_paper_maker_core};
    use vike_tradehub::{publish, server};
    use vike_tradehub_client::NodeKeys;

    pub(super) const TOKEN: &str = "TRADE_PLANE_CLI_JSON_TOKEN";
    /// The `venue` this fixture hands `MakerMountConfig::outcome_token` — the book the warm-up order
    /// lands under, and the book this file's read narrows to.
    pub(super) const VENUE: &str = "polymarket";
    /// Far-future resolution so the A-S horizon is positive — mirrors `tests/exit_codes.rs`'s own
    /// paper mount.
    const RESOLUTION_TS: i64 = 3_000_000_000;
    pub(super) const OBSERVE_KEY: &str = "DUMMY-observe-key-for-trade-plane-cli-json";
    pub(super) const CONTROL_KEY: &str = "DUMMY-control-key-for-trade-plane-cli-json";

    pub(super) fn spawn() -> (MakerMount, SocketAddr) {
        let cfg = MakerMountConfig::outcome_token(VENUE, TOKEN, Some(RESOLUTION_TS));
        let mount = build_paper_maker_core(&cfg);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
        let commands = Some(mount.handle.command_sink());
        let keys = NodeKeys::new(OBSERVE_KEY.as_bytes().to_vec(), CONTROL_KEY.as_bytes().to_vec());
        thread::spawn(move || {
            let _ = server::serve(
                listener,
                publisher,
                keys,
                commands,
                server::control::ControlLimitsConfig::default(),
                None,
                None,
                None,
            );
        });
        (mount, addr)
    }

    /// The throwaway `<project>/settings` node-key store `VIKE_SETTINGS_DIR` is pointed at — the
    /// same shape `tests/exit_codes.rs`'s `venue_reject_fixture::settings_dir` writes.
    pub(super) fn settings_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(vike_secrets::NODE_FILE);
        std::fs::write(
            &path,
            format!(
                "VIKE_TRADEHUB_OBSERVE_KEY={OBSERVE_KEY}\nVIKE_TRADEHUB_CONTROL_KEY={CONTROL_KEY}\n"
            ),
        )
        .expect("write node-key store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("chmod 600");
        }
        // A FILE is not a store since 2026-10-07: the carry `vike-cli secrets migrate` performs is
        // what puts the pair in the settings database's `node_key` table.
        crate::common::carry_into_store(dir.path());
        dir
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

    use vike_tradehub_client::proto::{
        FEATURE_ACCOUNT_ROUTING, FEATURE_MOUNT_ACCOUNT, FEATURE_MOUNT_CLASS, FEATURE_MOUNT_VERBS,
        FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE,
        FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS, FEATURE_TEARSHEET, FEATURE_VENUE_ROUTING,
    };
    use vike_tradehub_client::proto::{
        NODE_PROTO_VERSION, Request, Response, read_frame, write_frame,
    };

    fn features() -> Vec<String> {
        [
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
            FEATURE_TEARSHEET,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    /// One connection. The channel carries the Debug rendering of every post-auth frame, and is
    /// dropped unsent if the handshake desyncs — which the caller reads as a failure.
    pub(super) fn spawn() -> (SocketAddr, mpsc::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else { return };
            if !matches!(read_frame::<_, Request>(&mut stream), Ok(Request::Hello { .. })) {
                return;
            }
            let welcome = Response::Welcome {
                proto_version: NODE_PROTO_VERSION,
                nonce: [7u8; 32],
                features: features(),
            };
            if write_frame(&mut stream, &welcome).is_err() {
                return;
            }
            let Ok(Request::Auth { scope, .. }) = read_frame::<_, Request>(&mut stream) else {
                return;
            };
            if write_frame(&mut stream, &Response::AuthOk { scope }).is_err() {
                return;
            }
            let mut seen = Vec::new();
            while let Ok(req) = read_frame::<_, Request>(&mut stream) {
                seen.push(format!("{req:?}"));
                let reply = match req {
                    Request::Command { .. } => Response::Ack { coid: String::new() },
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

/// `trade order ls <book> --node <addr> --json` against a real node holding one real order: stdout
/// must be ONE parseable JSON document naming that order, with nothing else mixed into it.
///
/// The order is placed by a genuine one-shot `submit` first, and its arrival is confirmed off a
/// FRESH observe connection before the read under test runs — otherwise this could race the node's
/// first-publish window and read an empty snapshot (see `tests/exit_codes.rs`'s
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
    let addr_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let observer = vike_tradehub_client::RemoteCoreHandle::connect(
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
            &addr_str,
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
        wait_until(10, || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's own published snapshot"
    );

    let out = run(
        &["trade", "order", "ls", json_read_fixture::VENUE, "--node", &addr_str, "--json"],
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
    assert!(row["coid"].as_str().is_some_and(|c| !c.is_empty()), "a real coid: {stdout}");
}
