//! The numeric exit ladder, asserted over the SHIPPED binary.
//!
//! A script branches on these numbers, so they are a public interface: a rung's meaning may be
//! added to, never repurposed. `crates/vike-cli/src/exit.rs` argues each rung; this file is the
//! proof that the binary actually exits on it, which no unit test of a parser can see — every one
//! of these codes is produced by the `main` shim collapsing an `ExitCode`, several layers below
//! the function that decided it.
//!
//! ⚠ **This file now asserts all EIGHT rungs — `Exit::Venue` (`5`) was the last one still RESERVED,
//! and task 7 of the trade-CLI-plane (the one-shot order-write verbs) gave it a producer.**
//! `a_node_rejection_is_five` is its case, and it has to be a REAL refusal rather than a fabricated
//! one: it spins up a genuine paper `vike-tradehub` node (the same harness
//! `tests/trade_node_e2e.rs` uses) and drives the shipped binary's `trade order submit` against a
//! venue that node runs no engine for, so `crates/vike-tradehub/src/server.rs`'s `venue_refusal`
//! answers with a REAL `Response::Error` — never a mock. `crates/vike-cli/src/exit.rs` carries the
//! producer and the correction (this rung used to be RESERVED there too).
//!
//! ⚠ **`Exit::Refused` (`4`) used to be reserved alongside it, and this paragraph said so.** Its
//! first producer turned out to be a READ refusal rather than the order-write guardrail this file
//! expected to wire it: `crates/vike-cli/src/cmd/trade/selector.rs`'s
//! `refuse_an_unaddressable_book`, which `trade order ls` calls to refuse a book selector naming an
//! account the wire cannot yet address, rather than silently widening to every account of a venue.
//! `an_unaddressable_book_is_four` below is its case.
//!
//! `Exit::Breach` (`6`) and `Exit::Empty` (`7`) were the fifth and sixth to become live, and they
//! arrived under exactly that rule: the PR that wired `vike-cli backtest gate` is the PR that added
//! their cases below.
//!
//! ⚠ Every case pins the CHILD's environment (`Command::env` / `env_remove`, never
//! `std::env::set_var`, which is unsafe under threads and leaks across this binary's parallel
//! cases): without the settings redirect a run on a developer box resolves the REPO's settings
//! directory and reads a real credential store into a test's assertions.

use std::process::{Command, Output};

/// Run the shipped binary against an EMPTY settings directory and return its exit code.
///
/// The two removed-variable removals are not decoration: `vike_config::refuse_removed_env` runs
/// before any verb is routed, so an exported `VIKE_MAX_ORDER_NOTIONAL` on the developer's box
/// would make every case below exit on the startup-refusal path instead of the rung it is testing.
fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let empty = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", empty.path())
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// The exit code alone, for the cases whose message is not the point.
fn code(args: &[&str]) -> i32 {
    run(args, &[]).status.code().expect("the process exited rather than being signalled")
}

/// [`run`]'s twin for the cases that need a POPULATED project — the two gate rungs below.
///
/// ⚠ `run` above points `VIKE_SETTINGS_DIR` at an EMPTY directory and sets nothing else, and its
/// emptiness is what every other case here relies on. So this is a second helper rather than an
/// edit: it takes both directories and adds `VIKE_USER_DATA_DIR`, keeping every `env_remove`.
fn run_in(settings: &std::path::Path, user_data: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// The smallest runs tree a gate can be pointed at: a baseline, a worse run, and a mark on the
/// first. Nine lines, kept INSIDE this file rather than in a shared `tests/` helper module — two
/// integration binaries sharing one would need a `mod` file both include, which is more machinery
/// than the thing it shares.
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
    let scratch = tempfile::tempdir().expect("tempdir");
    let (settings, user_data) = gate_fixture::plant_worse_than_baseline(scratch.path());
    let out = run_in(
        &settings,
        &user_data,
        &["backtest", "gate", "@last", "--against", "@baseline/m", "--fail-if", "sharpe:-5%"],
    );
    assert_eq!(out.status.code(), Some(6), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

/// NOTHING WAS EVALUATED — and it is NOT a zero. A gate whose criteria named no key the document
/// carries has checked nothing, and from a `0` a CI step cannot tell that from a pass.
#[test]
fn an_unevaluated_gate_is_seven() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let (settings, user_data) = gate_fixture::plant_worse_than_baseline(scratch.path());
    let out = run_in(
        &settings,
        &user_data,
        &["backtest", "gate", "@last", "--against", "@baseline/m", "--fail-if", "nonsuch:-5%"],
    );
    assert_eq!(out.status.code(), Some(7), "stderr: {}", String::from_utf8_lossy(&out.stderr));
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
    // `crates/vike-cli/tests/backtest_cli.rs`.
    assert_eq!(code(&["backtest"]), 2, "backtest with no SUBCOMMAND is a usage error");
    assert_eq!(
        code(&["backtest", "run"]),
        2,
        "…and `run` with no profile and no flags is a usage error on the same rung"
    );
    assert_eq!(code(&["backtest", "run", "--profile"]), 2, "a flag with no value is a usage error");
    assert_eq!(code(&["trade", "status"]), 2, "trade status with no --node is a usage error");
    // The two one-shot WRITE verbs own the same rung, and they own it through their own parser
    // rather than `exit_for_parse_error` (`--node` is checked after the flags are drained, so its
    // absence is not a parse error) — which is exactly why each is asserted here rather than
    // assumed from its sibling.
    assert_eq!(code(&["trade", "halt"]), 2, "trade halt with no --node is a usage error");
    assert_eq!(code(&["trade", "resume"]), 2, "trade resume with no --node is a usage error");
    // ⚠ **A RETIRED verb rides the SAME rung as an unknown one, so these rows cannot tell them
    // apart** — which is why each says what it is asserting rather than naming a flag the verb no
    // longer has. The messages are pinned in `crates/vike-cli/tests/help_cli.rs`'s
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
name = \"buy_hold\"
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
/// ⚠ The observe key is supplied on the child, because a node-facing verb refuses BEFORE it opens
/// a socket when no key resolves anywhere — that path is a different failure with a different rung,
/// and testing the connect rung requires getting past it.
#[test]
fn a_node_connect_failure_is_three() {
    let out = run(
        &["trade", "status", "--node", "127.0.0.1:1"],
        &[("VIKE_TRADEHUB_OBSERVE_KEY", "not-a-real-key")],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "a refused node connection is rung 3; stderr: {err}");
}

/// A labelled book selector this wire cannot yet address REFUSES rather than silently widening to
/// every account of the venue — `crates/vike-cli/src/cmd/trade/selector.rs`'s
/// `refuse_an_unaddressable_book`, called by `trade order ls` BEFORE it resolves a key or opens a
/// connection. So a bogus, unreachable `--node` and no key anywhere still land on THIS rung: the
/// book address is judged before either of those would matter, and nothing reaches stdout.
#[test]
fn an_unaddressable_book_is_four() {
    let out = run(&["trade", "order", "ls", "binance/ALT", "--node", "127.0.0.1:1"], &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(4),
        "a labelled selector this wire cannot address is REFUSED, not silently widened; stderr: {err}"
    );
    assert!(out.stdout.is_empty(), "nothing should print before the refusal: {out:?}");
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

/// A REAL paper `vike-tradehub` node, and the throwaway node-key store that addresses it — the
/// smallest fixture that can genuinely REFUSE a command server-side, which is the one thing
/// `Exit::Venue`'s case needs and no mock can supply. Deliberately NOT shared with
/// `tests/trade_node_e2e.rs`: cargo compiles integration test files as independent binaries with no
/// shared `mod`, and duplicating this much setup here mirrors `gate_fixture` just above rather than
/// growing a cross-file dependency for one case.
mod venue_reject_fixture {
    use std::net::{SocketAddr, TcpListener};
    use std::thread;

    use vike_run::{MakerMount, MakerMountConfig, build_paper_maker_core};
    use vike_tradehub::{publish, server};
    use vike_tradehub_client::NodeKeys;

    pub(super) const TOKEN: &str = "EXIT_CODES_VENUE_TOKEN";
    /// Far-future resolution so the A-S horizon is positive (mirrors `trade_node_e2e.rs`'s own
    /// paper mount and the offline `vike-run` mount test it was copied from).
    const RESOLUTION_TS: i64 = 3_000_000_000;
    /// Obviously-fake HMAC keys — any bytes work as long as both sides agree, and the `DUMMY-`
    /// prefix makes it unmistakable in a diff that no real credential is involved.
    pub(super) const OBSERVE_KEY: &str = "DUMMY-observe-key-for-exit-codes-venue-test";
    pub(super) const CONTROL_KEY: &str = "DUMMY-control-key-for-exit-codes-venue-test";
    /// A venue this fixture's mount does NOT run — `venue_refusal`'s whole reason to exist. Spelled
    /// so it cannot collide with a real roster id.
    pub(super) const UNMOUNTED_VENUE: &str = "not-a-mounted-venue";
    /// The venue this fixture's mount DOES run (`MakerMountConfig::polymarket`'s own hardcoded
    /// `venue` field) — the warm-up order's target, so the core's routing roster is proven
    /// populated before the actual (refused) case runs. See `a_node_rejection_is_five`'s doc.
    pub(super) const MOUNTED_VENUE: &str = "polymarket";

    /// A PAPER node mounted on `polymarket`/[`TOKEN`] and served on an ephemeral loopback port with
    /// CONTROL enabled. The mount's engine roster is what `crates/vike-tradehub/src/server.rs`'s
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
    pub(super) fn spawn() -> (MakerMount, SocketAddr) {
        let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
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
                server::ControlLimitsConfig::default(),
                None,
                None,
                None,
            );
        });
        (mount, addr)
    }

    /// A throwaway `<project>/settings` NODE-KEY store holding both keys — the same shape
    /// `tests/trade_node_e2e.rs`'s `settings_dir_with_store` writes, so `vike-cli`'s dispatcher
    /// resolves them exactly as it would against a real daemon's store.
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
        dir
    }
}

/// Poll `cond` up to `secs`, the same shape `tests/trade_node_e2e.rs`'s own `wait_until` uses (not
/// shared across files — see `venue_reject_fixture`'s own doc for why duplicating this much is the
/// chosen tradeoff here).
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// **The far side spoke, and it said no.** `Exit::Venue`'s ONE case: a `trade order submit` naming
/// a venue this real paper node runs no engine for is well-formed (the book parses, the node
/// advertises `account-routing`) and REACHES the wire, then is REFUSED server-side by
/// `crates/vike-tradehub/src/server.rs`'s `venue_refusal` — the same gate that stops a mis-typed
/// venue from silently landing on the primary engine. `--yes` skips the confirm prompt (this is a
/// one-shot child with no terminal attached); the keys live only in the node-key store the settings
/// directory points at, exactly like `tests/trade_node_e2e.rs`'s own one-shot cases.
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
    let addr_str = addr.to_string();
    let settings_str = settings.path().to_str().expect("utf-8 tempdir path");

    let observer = vike_tradehub_client::RemoteCoreHandle::connect(
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
            &addr_str,
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
        wait_until(10, || !observer.snapshot().orders.is_empty()),
        "the warm-up order never reached the node's own snapshot — the core has not published, \
         so the case below would test an empty-roster race rather than a real refusal"
    );

    let out = run(
        &[
            "trade",
            "order",
            "submit",
            venue_reject_fixture::UNMOUNTED_VENUE,
            "IRRELEVANT",
            "buy",
            "1",
            "--node",
            &addr_str,
            "--yes",
        ],
        &[("VIKE_SETTINGS_DIR", settings_str)],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let transcript = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

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
        stderr
            .contains(&format!("no engine for venue `{}`", venue_reject_fixture::UNMOUNTED_VENUE)),
        "the real venue_refusal wording must survive to this side of the wire — a client-side \
         capability refusal (which never reaches the node) would print a DIFFERENT message and \
         prove nothing about this rung\n{transcript}"
    );

    // Nothing was booked under the bad venue: a real refusal, not a message printed over an order
    // that landed anyway. The warm-up order (on the MOUNTED venue) is expected to be present.
    let snap = observer.snapshot();
    assert!(
        snap.orders.iter().all(|o| o.venue != venue_reject_fixture::UNMOUNTED_VENUE),
        "the refused order must not appear in the node's own book\n{transcript}"
    );
}
