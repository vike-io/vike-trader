//! Headless IBKR mount. Loads `IBKR_{DEMO|LIVE}_*` config, connects the chosen backend
//! (socket|cpapi) as a real out-of-process `ExecutionClient`, wires its venue events into a
//! `vike-core` runtime, and drives place/cancel over stdin. Proof-of-life for the socket backend;
//! serves the cpapi backend once Task 12 lands. egui surfacing is deferred.
//!
//! Mount pattern (mirrors vike-app/src/main.rs): the live client can't be handed the core's
//! `EventSender` because the core doesn't exist until `spawn_core` — and the client is moved INTO
//! the engine. So: build a standalone event lane, connect the client against it, spawn the core,
//! then run a forwarder thread relaying the standalone lane into `core.event_sender()`. Orders are
//! submitted through the core's command lane (`send_command`), NOT by holding the client.

use std::io::BufRead;
use std::process::ExitCode;
use std::sync::Arc;

use vike_core::{spawn_core, CoreConfig};
use vike_data::{DataClient, LiveDataSink};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate, RiskLimits,
};
use vike_ibkr::config::load_ibkr_config_from;
use vike_ibkr::load_workspace_dotenv;
use vike_ibkr::{Environment, IbkrBackend, IbkrExecutionClient};
use vike_model::OrderRequest;

/// The engine's primary symbol. Orders carry their own `symbol` and route by `venue`, so this is
/// only the engine's default display symbol.
const PRIMARY_SYMBOL: &str = "AAPL.SMART.USD";
const SEED_CASH: f64 = 100_000.0;

/// `Debug` so a parse that was supposed to FAIL can report what it produced instead
/// (`Result::expect_err` requires it).
#[derive(Debug)]
struct Args {
    env: Environment,
    backend: Option<String>,
    /// Repeatable `--sub SYM@INTERVAL` (interval defaults to "1m" when omitted). Each entry
    /// subscribes IBKR bars (at the given interval) + trades for that symbol into the core's
    /// data lanes once the mount is up.
    subs: Vec<String>,
}

/// What a successful parse produced. `-h`/`--help` is NOT an error — see `vike-tradehub`'s
/// identical `Parsed::Help` shape, the in-tree precedent this follows (a non-zero `--help` breaks
/// `set -e` and any wrapper that checks a status). `-V`/`--version` is UNCHANGED by this: this
/// binary has no version flag, so it still falls through to the unknown-argument arm below — only
/// `--help` was in scope for this fix.
#[derive(Debug)]
enum Parsed {
    Args(Args),
    Help,
}

/// The usage line, shared between the `--help` success path and the error path in `main`.
const USAGE: &str =
    "usage: ibkr_mount --env demo|live [--backend socket|cpapi] [--sub SYM@INTERVAL ...]";

/// The process-argv entry point: [`parse_args_from`] over `std::env::args().skip(1)`. The SOURCE of
/// argv is the only thing this wrapper decides.
fn parse_args() -> Result<Parsed, String> {
    parse_args_from(std::env::args().skip(1))
}

/// **THE RULE: a valued flag must be GIVEN a value, and a token beginning with `--` is a FLAG,
/// never a value.** The same rule `vike_backfill::cli::flag_value` spells for the backfill bins;
/// this crate cannot depend on that one (nothing may — it pulls every bridge crate), so the
/// spelling is repeated rather than shared.
///
/// It is `--`, not a bare `-`: a negative number is a real value in this workspace's parsers, and
/// a `starts_with('-')` test would turn every signed field into a usage error. Nothing this bin
/// takes legitimately begins with `--`.
///
/// This parser was saved from most of the swallow by ACCIDENT rather than by design: eating a flag
/// left that flag's own value sitting in flag position, where `other =>` rejected it — so
/// `--backend --sub AAPL` died on `AAPL`, naming the wrong token. The swallow survived silently
/// exactly when the eaten flag was the LAST token, and `--sub --env` then subscribed to a symbol
/// literally named `--env` with no environment ever read.
///
/// ⚠ **It never fires on `-h`/`--help`, and the one place the two rules MEET is a value slot.**
/// `--help` takes no value, so it never reaches this function; but `--backend --help` does, and it
/// is refused as a swallow rather than short-circuiting to [`Parsed::Help`] — the `--backend` arm
/// matches the token in FLAG position first and asks for its value. That is the intended order:
/// printing usage and exiting 0 would silently discard the `--backend` the operator typed, which is
/// the exact shape of failure this rule exists to end. They still see the usage — `main` prints it
/// on the error path — plus a line naming both tokens. `--help` FIRST, or after a fed flag, still
/// short-circuits to a success.
fn flag_value(flag: &str, next: Option<String>) -> Result<String, String> {
    match next {
        None => Err(format!("{flag} needs a value")),
        Some(v) if v.starts_with("--") => {
            Err(format!("{flag} needs a value, but the next argument is another flag ({v})"))
        }
        Some(v) => Ok(v),
    }
}

/// Parse an already-`argv[0]`-stripped argument stream. PURE — no environment, no filesystem.
///
/// Every valued flag resolves through [`flag_value`], so a flag that was MENTIONED and not fed is
/// an error — a missing value, or a value that is itself a flag. An unmentioned flag still keeps
/// its default (`--env` in particular stays DEMO, which is this parser's safety property).
fn parse_args_from(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut env = Environment::Demo;
    let mut backend = None;
    let mut subs = Vec::new();
    let argv: Vec<String> = argv.collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--env" => {
                i += 1;
                env = match flag_value("--env", argv.get(i).cloned())?.as_str() {
                    "demo" | "paper" => Environment::Demo,
                    "live" => Environment::Live,
                    other => return Err(format!("unknown --env {other}")),
                };
            }
            "--backend" => {
                i += 1;
                backend = Some(flag_value("--backend", argv.get(i).cloned())?);
            }
            "--sub" => {
                i += 1;
                subs.push(flag_value("--sub", argv.get(i).cloned())?);
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            other => return Err(format!("unknown arg {other}")),
        }
        i += 1;
    }
    Ok(Parsed::Args(Args { env, backend, subs }))
}

// The IBKR feed→core-lane forwarder is now `vike_core::CoreLaneSink` (constructed at the
// subscribe site below) — it also forwards stream-health to any mounted strategy, which the old
// hand-rolled sink did not.

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Args(a)) => a,
        Err(e) => {
            eprintln!("arg error: {e}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` instead of
    // vike-log's `<exe_dir>/logs` last resort. `$VIKE_LOG_DIR` still wins.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "ibkr-mount".to_string(),
        project_dir: std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_log_dir(&cwd)),
        ..Default::default()
    });

    let mut cfg = match load_ibkr_config_from(args.env, &load_workspace_dotenv()) {
        Some(c) => c,
        None => {
            eprintln!("no IBKR_{}_ACCOUNT in .env — nothing to mount", args.env.as_str());
            return ExitCode::FAILURE;
        }
    };
    if let Some(b) = &args.backend {
        match IbkrBackend::parse_public(b) {
            Some(parsed) => cfg.backend = parsed,
            None => {
                eprintln!("unknown --backend {b}");
                return ExitCode::FAILURE;
            }
        }
    }

    // Standalone event lane: the client pushes venue events here; the forwarder (below) relays them
    // into the core after spawn.
    let (live_events, live_rx) = vike_exec::event_channel(4096);
    let client = match IbkrExecutionClient::connect(&cfg, live_events.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("IBKR connect failed (gateway down / backend unavailable?): {e:?}");
            return ExitCode::FAILURE;
        }
    };
    // Drop our own sender: only the client's clone keeps `live_rx` alive, so the forwarder ends when
    // the client is torn down.
    drop(live_events);

    let engine = ExecutionEngine::new(
        // NOTE the signature: `Account::new`'s FIRST arg is the contract MULTIPLIER, not a cash
        // balance. Passing SEED_CASH here gave every IBKR position a 100_000x multiplier, scaling
        // its PnL, margin and notional by that factor. Seed cash is wired separately below
        // (`seed_cash: SEED_CASH`), which is the intended path. 1.0 = no contract scaling, matching
        // vike-mount's make_engine and vike-run's lib.rs mount.
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        PRIMARY_SYMBOL,
    );
    // Write-ahead journal (live-tearsheet sink-enablement) — OFF unless `VIKE_JOURNAL_DIR` /
    // `VIKE_RUN_PROFILE` is set. See `vike_core::journal_config_from_env`.
    let core = spawn_core(
        engine,
        CoreConfig {
            seed_cash: SEED_CASH,
            journal: vike_core::journal_config_from_env(),
            // STUCK-ORDER WATCHDOG — armed, matching `vike-app`'s live mount. This is not a
            // nice-to-have here, it is the backstop this bridge's own code NAMES and did not have:
            // `crates/bridges/vike-ibkr/src/event_mapper.rs`'s `on_error` ends its unroutable-reject
            // arm with "relying on submit-ack watchdog", and
            // `crates/bridges/vike-ibkr/src/error.rs`'s `ORDER_REJECTION` is a five-code ALLOWLIST —
            // so an IB rejection carrying any other code, or an id-less one arriving while two
            // orders are unacked, emits no terminal at all. Under `CoreConfig::default()`
            // (`submit_ack_timeout: None`) that residual had no net in this binary, and an order
            // IBKR had already refused sat SUBMITTED forever.
            //
            // 30s satisfies both bounds `crates/vike-core/src/runtime/mod.rs` documents on this
            // field, and is the same pairing `vike-app` uses with the retained 15s
            // `submit_ack_confirm_grace` default: (b) it exceeds any venue's worst-case
            // order-visibility latency, and (a) `2·grace = 30s > submit_ack_timeout/2 +
            // N·requery`. It FLAGS at 30s and starts the confirm ladder — the last-resort
            // synthesized reject only fires after the grace window and only if the order is still
            // pre-ack then, so a slow-but-real IB ack is never raced into a phantom reject.
            submit_ack_timeout: Some(std::time::Duration::from_secs(30)),
            ..CoreConfig::default()
        },
    );

    // Forwarder: relay the standalone live lane into the core ingest.
    {
        let core_events = core.event_sender();
        let mut live_rx = live_rx;
        std::thread::Builder::new()
            .name("ibkr-event-forward".into())
            .spawn(move || {
                while let Some(ing) = live_rx.blocking_recv() {
                    if let vike_exec::lanes::Ingest::Event(e) = ing {
                        if core_events.blocking_send(e).is_err() {
                            break; // core gone
                        }
                    }
                }
            })
            .expect("spawn ibkr-event-forward thread");
    }

    // Optional market-data path: `--sub SYM@INTERVAL` (repeatable) subscribes IBKR bars + trades
    // into the core's data lanes via a `vike_core::CoreLaneSink`, so a session can chart/feed off
    // IBKR (IBKR's bars ride straight through — unlike Polymarket, no tick-bar synthesis). IBKR's
    // own connection is separate from the execution client above (a distinct client_id) and is only
    // opened when at least one `--sub` was given. Stored (not `mem::forget`) so `shutdown()` runs
    // (or its `Drop` does, if the explicit call below is ever skipped) at teardown.
    let mut ibkr_feeds: Option<vike_ibkr::IbkrFeeds> = None;
    if !args.subs.is_empty() {
        let sink: Arc<dyn LiveDataSink> = Arc::new(vike_core::CoreLaneSink::new(
            core.bar_sender(),
            core.market_sender(),
            core.tick_sender(),
        ));
        match vike_ibkr::IbkrFeeds::connect(&cfg, sink) {
            Ok(mut feeds) => {
                for s in &args.subs {
                    let (sym, iv) = s.split_once('@').unwrap_or((s.as_str(), "1m"));
                    if let Err(e) = feeds.subscribe_bars(sym, iv) {
                        eprintln!("ibkr subscribe_bars {sym}@{iv} failed: {e}");
                    }
                    if let Err(e) = feeds.subscribe_trades(sym) {
                        eprintln!("ibkr subscribe_trades {sym} failed: {e}");
                    }
                }
                ibkr_feeds = Some(feeds);
            }
            Err(e) => eprintln!("ibkr data connect failed: {e:?}"),
        }
    }

    println!(
        "ibkr_mount ready (account {} backend {:?}). commands:\n  buy SYM QTY [PX] | sell SYM QTY [PX] | cancel COID | status | quit",
        cfg.account, cfg.backend
    );
    let stdin = std::io::stdin();
    let mut next_coid = 1u64;
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["quit"] | ["exit"] => break,
            [side @ ("buy" | "sell"), sym, qty, rest @ ..] => {
                let coid = format!("ibkr-{next_coid}");
                next_coid += 1;
                let px = rest.first().and_then(|p| p.parse::<f64>().ok());
                let req = OrderRequest {
                    client_order_id: coid.clone(),
                    venue: "ibkr".to_string(),
                    symbol: sym.to_string(),
                    side: if *side == "buy" { 1 } else { -1 },
                    qty: qty.parse().unwrap_or(0.0),
                    order_type: if px.is_some() {
                        "limit".to_string()
                    } else {
                        "market".to_string()
                    },
                    price: px,
                    ..Default::default()
                };
                core.send_command(Command::Order(OrderIntent::Submit(Box::new(req))));
                println!("submitted {coid}");
            }
            ["cancel", coid] => {
                core.send_command(Command::Order(OrderIntent::Cancel((*coid).to_string())));
                println!("cancel {coid}");
            }
            ["status"] => {
                let snap = core.snapshot();
                println!("orders={} positions={}", snap.orders.len(), snap.positions.len());
            }
            [] => {}
            _ => println!("? (buy|sell SYM QTY [PX] | cancel COID | status | quit)"),
        }
    }
    if let Some(mut feeds) = ibkr_feeds {
        feeds.shutdown(); // stop+join every subscription's pump thread
    }
    core.shutdown_and_join();
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| (*s).to_string()).collect::<Vec<String>>().into_iter()
    }

    /// The parsed `Args`, or a panic naming what came back instead — every happy-path case here
    /// expects a run rather than help.
    fn run_args(v: &[&str]) -> Args {
        match parse_args_from(args(v)) {
            Ok(Parsed::Args(a)) => a,
            other => panic!("expected a run from {v:?}, got {other:?}"),
        }
    }

    /// **The DEFAULT is DEMO**, and that is the safety property of this parser: this binary connects
    /// a real out-of-process `ExecutionClient` and drives place/cancel from stdin, so a forgotten or
    /// mistyped `--env` must never resolve to a live account.
    #[test]
    fn a_bare_invocation_is_demo_with_no_backend_and_no_subscriptions() {
        let a = run_args(&[]);
        assert_eq!(a.env, Environment::Demo);
        assert_eq!(a.backend, None, "the config's own backend is kept when the flag is absent");
        assert!(a.subs.is_empty(), "no --sub ⇒ no market-data connection is opened at all");
    }

    /// `--env` maps `paper` onto DEMO and REFUSES everything that is not one of its three spellings
    /// — including `LIVE` in the wrong case, which is the shape a typo actually takes.
    #[test]
    fn the_env_flag_accepts_only_its_three_spellings() {
        for (spelled, want) in
            [("demo", Environment::Demo), ("paper", Environment::Demo), ("live", Environment::Live)]
        {
            let a = run_args(&["--env", spelled]);
            assert_eq!(a.env, want, "--env {spelled}");
        }
        for bad in ["LIVE", "Live", "prod", "real", ""] {
            let e = parse_args_from(args(&["--env", bad]))
                .expect_err("only demo|paper|live may arm this mount");
            assert!(e.contains("--env"), "the error names the flag for {bad:?}: {e}");
        }
        let trailing = parse_args_from(args(&["--env"])).expect_err("a trailing --env");
        assert!(trailing.contains("--env"), "{trailing}");
    }

    /// `--sub` is REPEATABLE and order-preserving — the one flag here that accumulates rather than
    /// overwriting, which is what makes a multi-symbol session possible at all.
    #[test]
    fn sub_accumulates_in_order_while_the_other_flags_overwrite() {
        let a = run_args(&[
            "--sub",
            "AAPL@1m",
            "--backend",
            "socket",
            "--sub",
            "MSFT",
            "--backend",
            "cpapi",
            "--sub",
            "NVDA@5m",
        ]);
        assert_eq!(a.subs, vec!["AAPL@1m", "MSFT", "NVDA@5m"]);
        assert_eq!(a.backend.as_deref(), Some("cpapi"), "the LAST --backend wins, silently");
    }

    /// A typo'd flag is REJECTED, not ignored, and a trailing `--sub`/`--backend` is an error rather
    /// than a silently-skipped subscription.
    #[test]
    fn unknown_and_valueless_flags_are_rejected() {
        let typo = parse_args_from(args(&["--subs", "AAPL"])).expect_err("a typo must not run");
        assert!(typo.contains("--subs"), "{typo}");
        for flag in ["--sub", "--backend"] {
            let e = parse_args_from(args(&[flag])).expect_err("a trailing valued flag");
            assert!(e.contains(flag), "{e}");
        }
    }

    /// **A FINDING, now refused.** A valued flag used to consume WHATEVER token followed, including
    /// another flag. This parser was saved from most of that by accident rather than by design:
    /// swallowing a flag left that flag's OWN value sitting in flag position, where the `other =>`
    /// arm rejected it — so `--backend --sub AAPL` died on `AAPL`, i.e. named the wrong token. The
    /// swallow survived SILENTLY exactly when the eaten flag was the LAST token, which is what the
    /// first two rows below were.
    ///
    /// All three are now refused BY NAME, naming the unfed flag AND the token it would have eaten.
    /// The middle row is the one worth reading twice: `--sub --env` used to subscribe to a symbol
    /// literally called `--env` while the environment stayed at its DEMO default, so an operator
    /// who meant `--sub AAPL --env live` got neither.
    ///
    /// Note the `expect_err` on `parse_args_from` itself rather than an unwrap through [`Parsed`]:
    /// the refusal happens BEFORE any `Parsed` is produced, so there is nothing to unwrap.
    #[test]
    fn a_valued_flag_may_not_swallow_a_following_flag() {
        for line in
            [&["--backend", "--sub"][..], &["--sub", "--env"][..], &["--env", "--backend"][..]]
        {
            let e = parse_args_from(args(line)).expect_err("a flag is not a value");
            assert!(e.contains(line[0]), "the message names the unfed flag: {e}");
            assert!(e.contains(line[1]), "…and the token it would have eaten: {e}");
        }
        // The shape that used to die on the wrong token now dies on the right one: the diagnostic
        // is about `--backend`/`--sub`, never about the symbol that trailed them.
        let e = parse_args_from(args(&["--backend", "--sub", "AAPL"]))
            .expect_err("still an error, now with the right subject");
        assert!(e.contains("--backend") && e.contains("--sub"), "{e}");
        assert!(!e.contains("AAPL"), "the message is no longer about the trailing value: {e}");
    }

    /// …and the flags STILL work when they are given real values, in the order that used to fail —
    /// the property the refusal above must not have bought.
    #[test]
    fn a_real_value_beside_a_following_flag_still_parses() {
        let a = run_args(&["--backend", "socket", "--sub", "AAPL@1m", "--env", "live"]);
        assert_eq!(a.backend.as_deref(), Some("socket"));
        assert_eq!(a.subs, vec!["AAPL@1m"]);
        assert_eq!(a.env, Environment::Live);
    }

    /// **The interaction between this rule and the `--help` short-circuit**, which the two fixes
    /// that landed together both have an opinion about. `--help` takes no value, so the swallow
    /// rule can never fire on it — but a `--help` sitting in a VALUE slot reaches the valued flag's
    /// arm first, and is refused as a swallow rather than printing usage and exiting 0. That is the
    /// deliberate order: a success there would silently discard the `--backend` the operator typed.
    /// `--help` in FLAG position — first, or after a fed flag — still short-circuits, which is the
    /// property [`help_is_now_ok_but_version_is_still_an_unknown_argument`] owns.
    #[test]
    fn a_help_in_value_position_is_a_swallow_while_one_in_flag_position_still_succeeds() {
        let e = parse_args_from(args(&["--backend", "--help"])).expect_err("a value slot");
        assert!(e.contains("--backend") && e.contains("--help"), "both tokens are named: {e}");
        assert!(matches!(parse_args_from(args(&["--help", "--backend"])), Ok(Parsed::Help)));
        assert!(matches!(
            parse_args_from(args(&["--backend", "socket", "--help"])),
            Ok(Parsed::Help)
        ));
    }

    /// **FIXED.** This binary used to answer `--help` with an ERROR (`main` printed the usage and
    /// returned `ExitCode::FAILURE`), unlike `vike-tradehub` and every `vike-backfill` bin, which
    /// treat it as a success. A non-zero `--help` breaks `set -e` and any wrapper that checks a
    /// status — the exact defect `vike_tradehub`'s `Parsed::Help` was introduced to fix. This binary
    /// now follows the same shape: both spellings short-circuit to `Parsed::Help`.
    ///
    /// `-V`/`--version` is UNCHANGED — this binary has no version flag (unlike `vike-tradehub`), so
    /// it still falls through to the `other =>` arm below and stays an error; only `--help` was in
    /// scope for this fix.
    #[test]
    fn help_is_now_ok_but_version_is_still_an_unknown_argument() {
        for flag in ["-h", "--help"] {
            assert!(matches!(parse_args_from(args(&[flag])), Ok(Parsed::Help)), "{flag}");
        }
        for flag in ["-V", "--version"] {
            assert!(
                parse_args_from(args(&[flag])).is_err(),
                "{flag} is still an unknown argument to this bin"
            );
        }
    }
}
