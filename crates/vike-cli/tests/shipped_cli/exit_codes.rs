//! The numeric exit ladder, asserted over the SHIPPED binary.
//!
//! A script branches on these numbers, so they are a public interface: a rung's meaning may be
//! added to, never repurposed. `crates/vike-cli/src/exit.rs` argues each rung; this file is the
//! proof that the binary actually exits on it, which no unit test of a parser can see — every one
//! of these codes is produced by the `main` shim collapsing an `ExitCode`, several layers below
//! the function that decided it.
//!
//! ⚠ **This file asserts every rung — `Exit::Venue` (`5`) was the last one still RESERVED, and
//! task 7 of the trade-CLI-plane (the one-shot order-write verbs) gave it a producer.**
//! `a_node_rejection_is_five` is its case, and it has to be a REAL refusal rather than a fabricated
//! one: it spins up a genuine paper `vike-tradehub` node (the same harness
//! `tests/shipped_cli/trade_hub_e2e.rs` uses) and drives the shipped binary's `trade order submit` against a
//! venue that node runs no engine for, so `crates/vike-tradehub/src/server/refusal.rs`'s `venue_refusal`
//! answers with a REAL `Response::Error` — never a mock. `crates/vike-cli/src/exit.rs` carries the
//! producer and the correction (this rung used to be RESERVED there too).
//!
//! ⚠ **`Exit::Refused` (`4`) used to be reserved alongside it, and this paragraph said so.** Its
//! first producer turned out to be a READ refusal rather than the order-write guardrail this file
//! expected to wire it: `crates/vike-cli/src/cmd/trade/selector.rs`'s
//! `refuse_an_unaddressable_book`, which `trade order ls` calls to refuse a book selector naming an
//! account when the node's frame cannot attribute an order of the venue to one (a node that predates
//! `OrderViewWire::account` sends labelled orders without it), rather than silently narrowing past it.
//! `an_unaddressable_book_is_four` below is its case.
//!
//! `Exit::Breach` (`6`) and `Exit::Empty` (`7`) were the fifth and sixth to become live, and they
//! arrived under exactly that rule: the PR that wired `vike-cli backtest gate` is the PR that added
//! their cases below. `Exit::OutcomeUnknown` (`8`) came with its producer the same way:
//! `an_unanswered_write_is_eight`.
//!
//! ⚠ Every case pins the CHILD's environment (`Command::env` / `env_remove`, never
//! `std::env::set_var`, which is unsafe under threads and leaks across this binary's parallel
//! cases): without the settings redirect a run on a developer box resolves the REPO's settings
//! directory and reads a real credential store into a test's assertions.

use crate::common;
use common::{run, run_in};
use std::time::Duration;
use vike_log::poll::wait_until;

/// The exit code alone, for the cases whose message is not the point.
fn code(args: &[&str]) -> i32 {
    run(args, &[]).status.code().expect("the process exited rather than being signalled")
}

/// The smallest runs tree a gate can be pointed at: a baseline, a worse run, and a mark on the
/// first. Nine lines, kept INSIDE this file rather than in `tests/common/mod.rs`, the shared module
/// this file declares for its runners: every binary that declares that module compiles all of it,
/// and only this file uses these nine lines.
mod gate_fixture {
    use std::path::{Path, PathBuf};

    fn plant(user_data: &Path, run_id: &str, sharpe: f64) {
        let dir = user_data.join("runs").join(run_id);
        std::fs::create_dir_all(&dir).expect("run dir");
        std::fs::write(dir.join("report.json"), format!("{{\"sharpe\":{sharpe}}}\n"))
            .expect("report");
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                "{{\"run_id\":\"{run_id}\",\"kind\":\"backtest\",\"produced_by\":\"backtest\",\
                 \"started_at\":\"2025-08-24T01:46:40Z\",\"finished_at\":\"2025-08-24T01:47:00Z\",\
                 \"git_sha\":null,\"config\":{{\"path\":\"m.toml\",\"name\":null}},\
                 \"detail\":null}}\n"
            ),
        )
        .expect("manifest");
    }

    /// `(settings, user_data)`, with `@last` naming a run whose sharpe is well below the mark's.
    pub(super) fn plant_worse_than_baseline(scratch: &Path) -> (PathBuf, PathBuf) {
        let settings = scratch.join("settings");
        let user_data = scratch.join("user_data");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::create_dir_all(user_data.join("runs")).expect("runs dir");
        plant(&user_data, "1755900000-1-0", 1.82);
        plant(&user_data, "1756000000-1-0", 1.40);
        let out = super::run_in(
            &settings,
            &user_data,
            &["backtest", "tag", "1755900000-1-0", "--as", "baseline/m"],
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "the fixture's own mark must be set, or both cases below would test the wrong \
             failure: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (settings, user_data)
    }
}

/// A DECLARED THRESHOLD WAS BREACHED. Its own rung because the command WORKED — it evaluated every
/// criterion it was given and the answer was no — so a wrapper escalates it where it would RETRY a
/// run failure and FIX a usage error.
///
/// ⚠ The invocation has to be a real one. This file's rule is that a rung nothing produces gets no
/// case, because a case that could not fail would be worse than the gap; so this one plants a runs
/// tree, marks a baseline and gates a worse run against it.
#[test]
fn a_breached_threshold_is_six() {
    assert_gate_against_a_worse_run_exits("sharpe:-5%", 6);
}

/// NOTHING WAS EVALUATED — and it is NOT a zero. A gate whose criteria named no key the document
/// carries has checked nothing, and from a `0` a CI step cannot tell that from a pass.
#[test]
fn an_unevaluated_gate_is_seven() {
    assert_gate_against_a_worse_run_exits("nonsuch:-5%", 7);
}

/// `backtest gate @last --against @baseline/m --fail-if <fail_if>` over a planted run worse than
/// its baseline exits `want`.
fn assert_gate_against_a_worse_run_exits(fail_if: &str, want: i32) {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (settings, user_data) = gate_fixture::plant_worse_than_baseline(scratch.path());
    let out = run_in(
        &settings,
        &user_data,
        &["backtest", "gate", "@last", "--against", "@baseline/m", "--fail-if", fail_if],
    );
    assert_eq!(out.status.code(), Some(want), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn help_is_zero() {
    assert_eq!(code(&["--help"]), 0);
}

/// A missing required flag is a USAGE error, not a run failure — a script RETRIES a run failure and
/// FIXES a usage error, so they must not share a code.
#[test]
fn a_usage_error_is_two() {
    // ⚠ TWO different refusals on ONE rung, which is why both rows are here. A bare `backtest` is
    // now a missing SUB-VERB (decision 11 of the backtest-CLI-surface design: there is no bare
    // form); `backtest run` with nothing is stage 2's "nothing to run" — `--profile` is OPTIONAL
    // since then, so what is refused is a command line carrying NEITHER a file NOR anything to
    // build one from. The codes cannot tell them apart; the messages are pinned in
    // `crates/vike-cli/tests/shipped_cli/backtest_cli.rs`.
    assert_eq!(code(&["backtest"]), 2, "backtest with no SUBCOMMAND is a usage error");
    assert_eq!(
        code(&["backtest", "run"]),
        2,
        "…and `run` with no profile and no flags is a usage error on the same rung"
    );
    assert_eq!(code(&["backtest", "run", "--profile"]), 2, "a flag with no value is a usage error");
    assert_eq!(code(&["trade", "status"]), 2, "trade status with no --node is a usage error");
    assert_eq!(code(&["trade", "health"]), 2, "trade health with no --node is a usage error");
    // The two one-shot WRITE verbs own the same rung, and they own it through their own parser
    // rather than `exit_for_parse_error` (`--node` is checked after the flags are drained, so its
    // absence is not a parse error) — which is exactly why each is asserted here rather than
    // assumed from its sibling.
    assert_eq!(code(&["trade", "halt"]), 2, "trade halt with no --node is a usage error");
    assert_eq!(code(&["trade", "resume"]), 2, "trade resume with no --node is a usage error");
    // ⚠ **A RETIRED verb rides the SAME rung as an unknown one, so these rows cannot tell them
    // apart** — which is why each says what it is asserting rather than naming a flag the verb no
    // longer has. The messages are pinned in `crates/vike-cli/tests/cli_verbs/help_cli.rs`'s
    // `the_retired_sweep_verb_fails_naming_its_replacement` and
    // `the_retired_walkforward_verb_fails_naming_its_replacement`; if either test goes, the
    // matching row here silently stops testing anything.
    assert_eq!(code(&["sweep"]), 2, "a RETIRED verb is a usage error, like an unknown one");
    assert_eq!(code(&["walkforward"]), 2, "…and so is the second one (decision 3)");
    assert_eq!(code(&["secrets", "--nope"]), 2, "an unknown flag is a usage error");
    // `trade` parses its own command line rather than sharing `exit_for_parse_error` (it is a REPL
    // with a `Config`, not an `Args`), so its rung is asserted separately — it is exactly the sort
    // of surface that drifted before the decision was shared.
    assert_eq!(code(&["trade"]), 2, "trade with no --node is a usage error");
}

/// An unknown VERB is the same class as an unknown flag: the command line was wrong.
#[test]
fn an_unknown_command_is_two() {
    assert_eq!(code(&["definitely-not-a-verb"]), 2);
}

/// …and so is no verb at all. It used to exit 1, which a script could not tell from a run that
/// tried and failed.
#[test]
fn no_subcommand_is_two() {
    assert_eq!(code(&[]), 2);
}

/// A refused connection is distinguishable from a bad command line: a script WAITS on this one.
/// Port 1 on loopback refuses immediately on every platform this ships to.
///
/// ⚠ The profile is a REAL file, written into a temp directory: it is read BEFORE the socket is
/// opened, so a missing one would exit on the read's rung instead and this test would pass for the
/// wrong reason once — and then never notice the connect classification regressing.
#[test]
fn a_connect_failure_is_three() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = dir.path().join("run.toml");
    std::fs::write(
        &profile,
        "[strategy]
file = \"trend/buy_hold.rs\"
",
    )
    .expect("write profile");
    let out = run(
        &[
            "backtest",
            "run",
            "--profile",
            profile.to_str().expect("utf-8 temp path"),
            "--addr",
            "127.0.0.1:1",
        ],
        &[],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "no datahub is a connect-class failure; stderr: {err}");
}

/// The same rung through a DIFFERENT client crate — `trade status` speaks to a vike-tradehub
/// node over `vike_tradehub_client`, not to a datahub over `vike_datahub_client`, so the two
/// classifications are genuinely separate code.
///
/// ⚠ The child's settings database holds a device key (the only key a node-facing verb signs with),
/// because such a verb refuses BEFORE it opens a socket when this box holds none — that path is a
/// different failure with a different rung, and testing the connect rung requires getting past it.
#[test]
fn a_node_connect_failure_is_three() {
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let out = run(
        &["trade", "status", "--node", "127.0.0.1:1"],
        &[("VIKE_SETTINGS_DIR", settings.path().to_str().expect("utf-8 temp path"))],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "a refused node connection is rung 3; stderr: {err}");
}

/// A book naming an account, against a node whose frame cannot attribute an order of the venue to one
/// (it predates `OrderViewWire::account`: no block carries a `tier`, and the venue has two), REFUSES
/// rather than narrowing past that order — `crates/vike-cli/src/cmd/trade/selector.rs`'s
/// `refuse_an_unaddressable_book`. It needs the frame, so it runs after the read, and nothing reaches
/// stdout.
#[test]
fn an_unaddressable_book_is_four() {
    let block = |route_key: &str, account: Option<&str>| {
        serde_json::json!({
            "venue": "binance", "balance": 0.0, "realized_pnl": 0.0, "fees_paid": 0.0,
            "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0, "missing_prices": 0,
            "margin_used": 0.0, "free_buying_power": 0.0, "trading_state": "Active", "positions": [],
            "route_key": route_key, "account": account,
        })
    };
    let mut frame = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    frame.sequence = 3;
    frame.venues = serde_json::from_value(serde_json::json!([
        block("binance", None),
        block("binance#ALT", Some("ALT")),
    ]))
    .expect("two blocks with no tier");
    frame.orders = vec![common::wire_order("c-1", "binance", None)];
    let node = common::spawn_frame_node(frame).to_string();
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let dir = settings.path().to_str().expect("utf-8 temp path");

    let out = run(
        &["trade", "order", "ls", "binance/ALT", "--node", &node],
        &[("VIKE_SETTINGS_DIR", dir)],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(4),
        "a book this frame cannot attribute is REFUSED, not silently narrowed; stderr: {err}"
    );
    assert!(out.stdout.is_empty(), "nothing should print before the refusal: {out:?}");
    assert!(err.contains("`binance`"), "it names the spelling that still reads: {err}");

    // The bare venue still reads, and says it cannot attribute the row.
    let out = run(
        &["trade", "order", "ls", "binance", "--json", "--node", &node],
        &[("VIKE_SETTINGS_DIR", dir)],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one JSON array");
    assert_eq!(rows[0]["account_known"], false, "{rows}");
    assert!(rows[0]["account"].is_null(), "{rows}");
}

/// ⚠ An UNPARSEABLE `--profile` is rung **2**, and a MISSING one is rung **1**. Those are different
/// questions and stage 2 of the backtest-CLI-surface design MOVED the first of them.
///
/// Before the inversion the profile text crossed the wire unparsed, so malformed TOML was a
/// far-side failure — rung 1, after a dial. Now `build_profile_toml` parses it HERE, to apply
/// `--set` onto it, and `execute` classifies that with `CliError::usage`. A script should FIX a
/// profile it typed wrong and RETRY a profile it could not read, so the split is the right one.
///
/// It is pinned because nothing else notices the move: `a_run_failure_is_still_one` below uses a
/// file that does not EXIST, and that read still fails on rung 1 through a plain `?`. The two tests
/// are a pair and the difference between them is the whole point — change one and read the other.
#[test]
fn an_unparseable_profile_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = dir.path().join("malformed.toml");
    std::fs::write(&profile, "this is [not valid").expect("write profile");
    let out =
        run(&["backtest", "run", "--profile", profile.to_str().expect("utf-8 temp path")], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a profile that is not TOML is a command line the user must FIX; stderr: {err}"
    );
    assert!(err.contains("profile"), "and it says which input was wrong: {err}");
}

/// The pre-existing catch-all keeps its number, so every script written against the old
/// two-value behaviour still reads correctly.
///
/// ⚠ The file is MISSING, not malformed — see `an_unparseable_profile_is_a_usage_error` directly
/// above for why that distinction carries a different rung since stage 2.
#[test]
fn a_run_failure_is_still_one() {
    let out = run(&["backtest", "run", "--profile", "no-such-profile.toml"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an unreadable profile is the ordinary run failure; stderr: {err}"
    );
    assert!(err.contains("no-such-profile.toml"), "and it names the file: {err}");
}

/// A REAL paper `vike-tradehub` node, and the throwaway store whose device key it lists — the
/// smallest fixture that can genuinely REFUSE a command server-side, which is the one thing
/// `Exit::Venue`'s case needs and no mock can supply. The spawner and the store are
/// `tests/common/mod.rs`'s `spawn_paper_node` and `device_settings_dir` (compiled once by the
/// `shipped_cli` group); this module names the venue, token and key values. It is NOT
/// `crates/vike-cli/tests/shipped_cli/trade_hub_e2e.rs`'s `spawn_hub_serving`, which also takes the
/// control switch and a settings source.
mod venue_reject_fixture {
    use std::net::SocketAddr;

    use vike_tradehub_core::StrategyCorePaper;

    pub(super) const TOKEN: &str = "EXIT_CODES_VENUE_TOKEN";
    /// Obviously-fake key VALUES, each naming a fixture device (`common::device_for`) the node lists at
    /// that role; the `DUMMY-` prefix makes it unmistakable in a diff that no real credential is involved.
    pub(super) const OBSERVE_KEY: &str = "DUMMY-observe-key-for-exit-codes-venue-test";
    pub(super) const CONTROL_KEY: &str = "DUMMY-control-key-for-exit-codes-venue-test";
    /// A venue this fixture's mount does NOT run — `venue_refusal`'s whole reason to exist. Spelled
    /// so it cannot collide with a real roster id.
    pub(super) const UNMOUNTED_VENUE: &str = "not-a-mounted-venue";
    /// The venue this fixture's mount DOES run (the `venue` it hands
    /// `vike_tradehub_core::test_support::paper_maker::paper_maker_core`) — the warm-up order's target, so the core's routing
    /// roster is proven populated before the actual (refused) case runs. See
    /// `a_node_rejection_is_five`'s doc.
    pub(super) const MOUNTED_VENUE: &str = "polymarket";

    /// A PAPER node mounted on `polymarket`/[`TOKEN`] and served on an ephemeral loopback port with
    /// CONTROL enabled. The mount's engine roster is what `crates/vike-tradehub/src/server/refusal.rs`'s
    /// `venue_refusal` checks a command's addressed venue against — read straight off the core's
    /// OWN snapshot cell per call (`crate::publish::PublisherHandle::engine_venues`), never off the
    /// publisher's async fan-out.
    ///
    /// ⚠ **That does NOT mean the roster is populated the moment this function returns — an earlier
    /// draft of this doc said so, and it was wrong.** `engine_venues()` reads `portfolio.venues`,
    /// which starts EMPTY and is filled in only once the core folds something into it; a feed-less
    /// mount goes dirty only when something happens to it (an order, a bar), not merely by being
    /// built. So there IS a race to win here, on THIS side of the call this doc used to say had
    /// none: a command sent before the core's first fold sees an empty roster, and
    /// `venue_refusal`'s own doc says an empty roster is treated as UNKNOWN rather than refused —
    /// the command falls through to the primary engine instead of being refused. Measured directly:
    /// the first version of `a_node_rejection_is_five` sent its bad-venue submit immediately after
    /// this function returned and the node ACCEPTED it. See that test's own doc for the warm-up
    /// order it now sends first, and for why fixing the daemon's window itself is out of scope here.
    pub(super) fn spawn() -> (StrategyCorePaper, SocketAddr) {
        crate::common::spawn_paper_node(MOUNTED_VENUE, TOKEN, OBSERVE_KEY, CONTROL_KEY)
    }

    /// A throwaway `<project>/settings` whose settings database holds the CONTROL-role device key,
    /// so `vike-cli`'s dispatcher signs with it exactly as it would on a real box.
    pub(super) fn settings_dir() -> tempfile::TempDir {
        crate::common::device_settings_dir(CONTROL_KEY)
    }
}

/// **The far side spoke, and it said no.** `Exit::Venue`'s ONE case: a `trade order submit` reusing the order client id
/// of the order this real paper node already holds is well-formed and REACHES the wire, then is REFUSED server-side by
/// `crates/vike-tradehub/src/server/refusal.rs`'s `order_client_id_reuse_refusal`, which no client can make: only the
/// node knows its book. `--yes` skips the confirm prompt (this is a one-shot child with no terminal attached); the
/// device key lives only in the store the settings directory points at, exactly like
/// `tests/shipped_cli/trade_hub_e2e.rs`'s own one-shot cases.
///
/// ⚠ **A venue the node runs no engine for was this rung's case until 2026-11-24**, and it is `Exit::Refused` now: the
/// one order gate (`vike_client::order_gate::judge`, D5 of that day) refuses it on this side, before the wire, once the
/// frame shows the node's engines. That half is pinned here too, nothing sent.
///
/// ⚠ **`venue_refusal` reads `engine_venues()` FRESH per command off the core's own snapshot cell,
/// and that roster starts EMPTY until the core's first PUBLISH** — a feed-less paper mount goes
/// dirty only when something happens to it, and `crates/vike-tradehub/src/server.rs`'s own routing
/// arm names the window ("routing UNCHECKED on this connection: the core has published no engine
/// set yet … it resolves itself on this core's first publish"). So this test WARMS the core with
/// one real, valid order first and waits for it to land — proving the roster is populated — before
/// the actual case runs; skipping that step raced the empty-roster window and the bad-venue submit
/// was silently accepted onto the primary engine instead of refused (measured directly: exit `0`,
/// "accepted by the node", the first time this test was written without the warm-up).
///
/// The proof is TWO-SIDED, not just the exit code: the refusal message names the real gap (never a
/// client-side capability refusal, which would mean the command never reached the node at all), and
/// a fresh OBSERVE connection shows nothing was booked under the bad venue — a refused command must
/// leave no order behind for this rung to mean what it claims.
#[test]
fn a_node_rejection_is_five() {
    let (_mount, addr) = venue_reject_fixture::spawn();
    let settings = venue_reject_fixture::settings_dir();
    let address_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let observer = vike_tradehub_client::TradehubCoreHandle::connect(
        addr,
        venue_reject_fixture::OBSERVE_KEY.as_bytes(),
    )
    .expect("observe connect");

    // WARM-UP: a real order on the venue this node actually runs, forcing the core's first
    // publish (see the doc above). Waited out on the OBSERVE side, never assumed from the exit
    // code alone — the ACK can return before the core has folded and published.
    let warm = run(
        &[
            "trade",
            "order",
            "submit",
            venue_reject_fixture::MOUNTED_VENUE,
            venue_reject_fixture::TOKEN,
            "buy",
            "1",
            "--node",
            &address_str,
            "--yes",
        ],
        &[("VIKE_SETTINGS_DIR", settings_str)],
    );
    assert_eq!(
        warm.status.code(),
        Some(0),
        "the warm-up order (on a venue this node DOES run) must be accepted, or the fixture is \
         wrong rather than the rung under test; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(10), || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's own snapshot — the core has not published, \
         so the case below would test an empty-roster race rather than a real refusal"
    );

    let submit = |venue: &str, symbol: &str, pinned: &str| {
        let out = run(
            &[
                "trade",
                "order",
                "submit",
                venue,
                symbol,
                "buy",
                "1",
                "--order-client-id",
                pinned,
                "--node",
                &address_str,
                "--yes",
            ],
            &[("VIKE_SETTINGS_DIR", settings_str)],
        );
        let transcript = format!(
            "--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out, transcript)
    };

    // The venue the node runs no engine for: refused on THIS side by the one order gate, nothing sent.
    let (out, transcript) =
        submit(venue_reject_fixture::UNMOUNTED_VENUE, "IRRELEVANT", "Elsewhere1");
    assert_eq!(out.status.code(), Some(4), "refused client-side is Exit::Refused; {transcript}");
    assert!(transcript.contains("error [order.edge.venue_not_mounted]: "), "{transcript}");

    // The id the warm-up order already holds: only the node can see that, so the far side says no.
    let warm_id = observer.snapshot().orders.first().map(|o| o.order_client_id.clone());
    let warm_id = warm_id.expect("the warm-up order is on the node's frame");
    let (out, transcript) =
        submit(venue_reject_fixture::MOUNTED_VENUE, venue_reject_fixture::TOKEN, &warm_id);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(5), "a node rejection is Exit::Venue; {transcript}");
    assert!(
        !stdout.contains("accepted by the node"),
        "a REFUSED command must never print acceptance\n{transcript}"
    );
    assert!(
        stderr.contains("node rejected the command"),
        "the far side must be named as having spoken, not merely as unreachable\n{transcript}"
    );
    assert!(
        stderr.contains(&format!("order client id {warm_id} is already used")),
        "the node's own reuse wording must survive to this side of the wire — a client-side refusal (which \
         never reaches the node) would print a DIFFERENT message and prove nothing about this rung\n{transcript}"
    );

    // Nothing was booked under the bad venue: a real refusal, not a message printed over an order
    // that landed anyway. The warm-up order (on the MOUNTED venue) is expected to be present.
    let snap = observer.snapshot();
    assert!(
        snap.orders.iter().all(|o| o.venue != venue_reject_fixture::UNMOUNTED_VENUE),
        "the refused order must not appear in the node's own book\n{transcript}"
    );
}

/// **A write SENT and never answered is `Exit::OutcomeUnknown`, not the `1` a wrapper re-runs.**
/// `common::spawn_frame_node` answers an observe read only: a control connection's first request is a
/// `Command`, not the `Subscribe` it waits for, so it READS the command and hangs up — the frame went
/// out and no verdict came back (`CommandOutcome::Disconnected`). Both write tails meet it: the plane's
/// one (`trade position close-all`, the unscoped panic button, which needs no capability word) and the
/// one-shot mode verbs' (`trade halt`).
#[test]
fn an_unanswered_write_is_eight() {
    let node = common::spawn_frame_node(vike_tradehub_client::wire::CoreSnapshotWire::empty());
    let node = node.to_string();
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let dir = settings.path().to_str().expect("utf-8 temp path");
    for verb in [&["position", "close-all"][..], &["halt"]] {
        let argv: Vec<&str> = ["trade"]
            .into_iter()
            .chain(verb.iter().copied())
            .chain(["--node", &node, "--yes"])
            .collect();
        let out = run(&argv, &[("VIKE_SETTINGS_DIR", dir)]);
        let transcript = format!(
            "{argv:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.status.code(),
            Some(i32::from(vike_utility::cli::OUTCOME_UNKNOWN_EXIT)),
            "a write the node never answered may have happened: rung 8, read before a resend\n{transcript}"
        );
        assert!(
            transcript.contains("OUTCOME UNKNOWN"),
            "it says the outcome is unknown\n{transcript}"
        );
        assert!(!transcript.contains("accepted by the node"), "never acceptance\n{transcript}");
    }
}

/// **A remote delete whose answer was lost is eight**, in the words MCP's `delete_series` answers too
/// (`vike_datahub_client::client::data_ops::DeleteRefusal`, decision 0303): the plan came back, the delete went
/// out and the datahub hung up before answering, so rows may be gone and a script plans again before it
/// resends. It used to be a plain failure, `1`.
#[test]
fn a_delete_whose_answer_was_lost_is_eight() {
    use vike_data::store::removal::{RemovalPlan, RemovalPlanSeries};
    use vike_data::{SeriesCoverage, SeriesId};
    use vike_datahub_client::proto::{DeleteDone, FEATURE_DELETE_SERIES, Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("the bound address").to_string();
    let datahub = std::thread::Builder::new()
        .name("lost-delete-datahub".into())
        .spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let features = vec![FEATURE_DELETE_SERIES.to_string()];
            assert!(vike_datahub_client::test_support::accept_keyless(&mut stream, features));
            let Ok(Request::DeleteSeries { selector, dry_run: true, .. }) = read_frame(&mut stream)
            else {
                panic!("the plan pass comes first");
            };
            let id = SeriesId::per_symbol(selector.kind, "binance", "BTCUSDT", Some("1h".into()));
            let series =
                vec![RemovalPlanSeries::new(id, SeriesCoverage::default(), Vec::new(), None)];
            let plan = RemovalPlan { selector, produced_by: None, series };
            write_frame(&mut stream, &Response::Deleted(DeleteDone { plan, outcome: None }))
                .expect("answer the plan");
            // The delete pass: read it, then hang up without an answer.
            let _ = read_frame::<_, Request>(&mut stream);
        })
        .expect("spawn the fake datahub");
    let settings = tempfile::tempdir().expect("tempdir");
    let dir = settings.path().to_str().expect("utf-8 temp path");
    let argv = [
        "data",
        "history",
        "rm",
        "--kind",
        "bar",
        "--venue",
        "binance",
        "--symbol",
        "BTCUSDT",
        "--interval",
        "1h",
        "--addr",
        &addr,
        "--yes",
    ];
    let out = run(&argv, &[("VIKE_SETTINGS_DIR", dir)]);
    datahub.join().expect("the fake datahub");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(i32::from(vike_utility::cli::OUTCOME_UNKNOWN_EXIT)),
        "rows may be gone: rung 8, plan again before a resend\n{stderr}"
    );
    assert!(stderr.contains("error [datahub.delete.outcome_unknown]"), "{stderr}");
    assert!(stderr.contains("OUTCOME UNKNOWN") && stderr.contains("fresh plan"), "{stderr}");
}

/// **Under `--json` an unanswered order write names its order client id on stdout**, the key of the read that
/// settles it (decision 0303), beside the same rung 8: before, a `--json --yes` run printed no preview and so
/// never showed the id at all. The node is [`an_unanswered_write_is_eight`]'s, which hangs up on a command.
#[test]
fn an_unanswered_write_names_its_order_client_id_under_json() {
    let node = common::spawn_frame_node(vike_tradehub_client::wire::CoreSnapshotWire::empty());
    let node = node.to_string();
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_device(settings.path(), "not-a-real-key");
    let dir = settings.path().to_str().expect("utf-8 temp path");
    let argv = ["trade", "order", "cancel", "ORDER1", "--node", node.as_str(), "--yes", "--json"];
    let out = run(&argv, &[("VIKE_SETTINGS_DIR", dir)]);
    let transcript = format!(
        "{argv:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.status.code(),
        Some(i32::from(vike_utility::cli::OUTCOME_UNKNOWN_EXIT)),
        "{transcript}"
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is one JSON object ({e})\n{transcript}"));
    assert_eq!(
        doc,
        serde_json::json!({ "outcome": "outcome_unknown", "order_client_id": "ORDER1" }),
        "{transcript}"
    );
}
