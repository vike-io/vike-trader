//! The live verb's production route, end to end (plan 2b F5): the real builder builds a real strategy
//! file, the real jail is applied by a real child process, the real loader opens the file, and a real
//! core mounts it through `vike_tradehub_core::remote_strategy`. Proven here, each a check of its own:
//!
//! - the child's orders reach the core's venue client as the mount's own;
//! - `kill -9` of the child cancels NOTHING (the orders rest), the mount restarts, and the restarted
//!   child adopts them from the state it last sent (it amends; it does not place again);
//! - a hook that hangs is killed at the deadline and applies nothing;
//! - a reply naming a symbol the mount does not trade applies nothing;
//! - an artifact that is not there leaves the mount unopened, with the reason.
//!
//! `harness = false`: this binary is ALSO the child. Started as `<this> strategy-sandbox live <artifact>`
//! it runs the crate's real verb (`vike_strategy_sandbox::run`), jail and all, which needs a process with
//! one thread and no descriptor above stderr — a libtest harness has neither.
//!
//! `-- --prepare-latency` builds the two latency probes and leaves them, with this binary's path, in
//! `sandbox-latency/` beside this binary (`target/<profile>/deps/`, where the latency binary is too) for `crates/vike-core/tests/runtime_latency.rs`'s `sandbox-mounted*`
//! variants, then exits (it runs cargo, so never under the latency job's `chrt`).

use std::io::{Read, Write};
use std::process::ExitCode;

const NAME: &str = "live_child";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("strategy-sandbox") {
        return vike_strategy_sandbox::run(&args[1..], no_backtest);
    }
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            println!("{NAME}: test");
        }
        return ExitCode::SUCCESS;
    }
    #[cfg(target_os = "linux")]
    return linux::main(&args);
    #[cfg(not(target_os = "linux"))]
    {
        println!("SKIP: a Rust strategy file runs only on Linux");
        ExitCode::SUCCESS
    }
}

/// The loaded file `vike_strategy_sandbox::BacktestServe` hands its serve function.
type LoadedFile = Box<
    dyn Fn(&str, &str) -> Box<dyn vike_model::Strategy<vike_simulator::OrderBrokerSimulator>>
        + Send
        + Sync,
>;

fn no_backtest(
    _input: Box<dyn Read + Send>,
    _output: Box<dyn Write + Send>,
    _build: Result<LoadedFile, String>,
) -> ExitCode {
    ExitCode::from(2)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use vike_core::{CoreConfig, CoreHandle, Mount, spawn_core};
    use vike_execution::{Account, BalanceMode, Engine, ExecutionClient, RiskGate};
    use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
    use vike_model::{OrderRequest, RiskLimits};
    use vike_strategy_builder::render;
    use vike_tradehub_core::remote_strategy::{
        Launched, Launcher, RemoteHandle, RemoteSpec, RemoteStatus, RemoteStrategy, SandboxChild,
    };

    /// The latency harness's `BookProbe`, as a strategy file: it exists, so the core runs a hook per book.
    pub const BOOK_PROBE: &str = r#"
use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};
use vike_marketdata::L2Book;

pub struct BookProbe;

impl<B: OrderBroker> Strategy<B> for BookProbe {
    fn on_order_book(&mut self, _broker: &mut B, _book: &L2Book) {}
}

pub fn build<B: OrderBroker + 'static>(_params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(BookProbe)
}
"#;

    /// The latency harness's `QuoteProbe`, as a strategy file, plus what the checks below need: its state
    /// (whether it placed), and a `mode` param (`hang`, `stray`) for the failure checks.
    pub const QUOTE_PROBE: &str = r#"
use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_marketdata::L2Book;

pub struct QuoteProbe {
    placed: bool,
    out: bool,
    mode: String,
}

impl<B: OrderBroker> Strategy<B> for QuoteProbe {
    fn on_order_book(&mut self, b: &mut B, book: &L2Book) {
        match self.mode.as_str() {
            "hang" => std::thread::sleep(std::time::Duration::from_secs(5)),
            "stray" => b.place("ETHUSDT", "", 1, 1.0, OrderPlacement::Market, false),
            _ => {}
        }
        let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) else { return };
        self.out = !self.out;
        let step = if self.out { 0.01 } else { 0.0 };
        let (bid, ask) = (bid.price - step, ask.price + step);
        if self.placed {
            b.amend("bid", None, Some(bid), None);
            b.amend("ask", None, Some(ask), None);
        } else {
            b.place("", "bid", 1, 1.0, OrderPlacement::Limit { price: bid }, false);
            b.place("", "ask", -1, 1.0, OrderPlacement::Limit { price: ask }, false);
            self.placed = true;
        }
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "placed": self.placed }))
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        self.placed = state["placed"].as_bool().unwrap_or(false);
    }
}

pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    let mode = params.get("mode").and_then(ParamValue::as_str).unwrap_or("quote").to_string();
    Box::new(QuoteProbe { placed: false, out: false, mode })
}
"#;

    /// A file that logs ONE warning of its own, from its `build` (so at the `Open`).
    pub const LOG_PROBE: &str = r#"
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_model::{OrderBroker, Strategy};

struct LogProbe;

impl<B: OrderBroker> Strategy<B> for LogProbe {
    fn on_bar(&mut self, _broker: &mut B, _bar: &Bar) {}
}

pub fn build<B: OrderBroker + 'static>(_params: &Params) -> Box<dyn Strategy<B> + Send> {
    tracing::warn!(rungs = 3, "f3-log-probe: a file's own warning");
    Box::new(LogProbe)
}
"#;

    pub fn main(args: &[String]) -> ExitCode {
        // This binary plays the hub: the boot's step 0 first, so no descriptor it inherited (CI's runner
        // hands it two pipes) reaches the strategy process, which would refuse it.
        let _ = vike_boot::descriptors::mark_inherited_close_on_exec();
        if let Err(why) = vike_sandbox::probe() {
            println!("SKIP: this box cannot apply the jail ({why}); nothing to prove here");
            return ExitCode::SUCCESS;
        }
        let prepare = args.iter().any(|a| a == "--prepare-latency");
        let dir = if prepare {
            let me = std::env::current_exe().expect("this binary's path");
            me.parent().expect("this binary's directory").join("sandbox-latency")
        } else {
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("live-child-strategy-files")
        };
        let book = build(BOOK_PROBE, "book_probe.rs", &dir);
        let quote = build(QUOTE_PROBE, "quote_probe.rs", &dir);
        if prepare {
            let me = std::env::current_exe().expect("this binary's path");
            std::fs::copy(&book, dir.join("book_probe.so")).expect("place the book probe");
            std::fs::copy(&quote, dir.join("quote_probe.so")).expect("place the quote probe");
            std::fs::write(dir.join("program"), me.to_string_lossy().as_bytes()).expect("program");
            println!("prepared {} (program {})", dir.display(), me.display());
            return ExitCode::SUCCESS;
        }
        a_killed_child_cancels_nothing_and_its_restart_adopts_the_orders(&quote);
        a_hung_hook_is_killed_and_applies_nothing(&quote);
        a_stray_symbol_applies_nothing(&quote);
        a_missing_artifact_is_the_mounts_reason();
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let maker = workspace.join("strategies/market_making/spread_maker.rs");
        let maker_text = std::fs::read_to_string(&maker)
            .unwrap_or_else(|e| panic!("{}: {e} (the shipped maker file)", maker.display()));
        the_spread_maker_file_quotes_in_the_sandbox_at_the_in_process_fee_floor(&build(
            &maker_text,
            "market_making/spread_maker.rs",
            &dir,
        ));
        super::a_files_own_warning_reaches_the_childs_stderr(&build(
            LOG_PROBE,
            "other/log_probe.rs",
            &dir,
        ));
        println!("{}: ok", super::NAME);
        ExitCode::SUCCESS
    }

    fn build(src: &str, name: &str, dir: &Path) -> PathBuf {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let env = render::child_env(&std::env::vars().collect());
        render::build_strategy_file(
            src,
            name,
            dir,
            &workspace,
            &cargo,
            &env,
            render::Profile::of_this_binary(),
            &render::CargoHomePin::disabled(),
        )
        .unwrap_or_else(|e| panic!("the template must build {name}:\n{e}"))
    }

    /// The child process, with its pid kept so a check can `kill -9` it.
    struct Process(std::process::Child);

    impl SandboxChild for Process {
        fn kill(&mut self) {
            let _ = self.0.kill();
        }
        fn wait(&mut self) -> String {
            self.0.wait().map_or_else(|e| e.to_string(), |s| s.to_string())
        }
    }

    /// The production start (`vike_sandbox::spawn_strategy_process` of the verb), recording each pid.
    fn launcher(artifact: &Path, pids: Arc<Mutex<Vec<u32>>>) -> Launcher {
        let me = std::env::current_exe().expect("this binary's path");
        let args = ["strategy-sandbox".into(), "live".into(), artifact.as_os_str().to_owned()];
        Box::new(move || {
            let mut child = vike_sandbox::spawn_strategy_process(&me, args.clone())?;
            pids.lock().unwrap().push(child.id());
            let (i, o, e) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
            Ok(Launched {
                to_child: Box::new(i.expect("piped")),
                from_child: Box::new(o.expect("piped")),
                reasons: e.map(|e| Box::new(e) as Box<dyn std::io::Read + Send>),
                child: Box::new(Process(child)),
            })
        })
    }

    /// The venue client: it counts what reaches the venue and keeps the ids it was sent.
    #[derive(Clone, Default)]
    struct Venue {
        order_client_ids: Arc<Mutex<Vec<String>>>,
        submits: Arc<AtomicU64>,
        amends: Arc<AtomicU64>,
        cancels: Arc<AtomicU64>,
    }

    impl ExecutionClient for Venue {
        fn submit(&mut self, request: &OrderRequest) {
            self.order_client_ids.lock().unwrap().push(request.order_client_id.clone());
            self.submits.fetch_add(1, Ordering::SeqCst);
        }
        fn cancel(&mut self, _order_client_id: &str) {
            self.cancels.fetch_add(1, Ordering::SeqCst);
        }
        fn modify(&mut self, _order: &OrderRequest, _quantity: Option<f64>, _price: Option<f64>) {
            self.amends.fetch_add(1, Ordering::SeqCst);
        }
        fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
            Some(vike_venues::AmendSemantics::InPlaceTotal)
        }
    }

    struct Node {
        core: CoreHandle,
        remote: RemoteHandle,
        venue: Venue,
        pids: Arc<Mutex<Vec<u32>>>,
    }

    fn node(artifact: &Path, params: &str) -> Result<Node, String> {
        let pids = Arc::new(Mutex::new(Vec::new()));
        let spec = RemoteSpec {
            file: "market_making/quote_probe.rs".into(),
            sha256: "test".into(),
            params: params.into(),
            facts: vike_model::params::MountFacts {
                symbol: "BTCUSDT".into(),
                venue: "sim".into(),
                interval: "1m".into(),
                ..Default::default()
            },
            controller_id: None,
        };
        let (proxy, remote) = RemoteStrategy::open(spec, launcher(artifact, Arc::clone(&pids)))?;
        let venue = Venue::default();
        let engine = Engine::new(
            Account::new(1_000_000.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            venue.clone(),
            "sim",
            "BTCUSDT",
        );
        let mount = Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(proxy),
        };
        let core =
            spawn_core(engine, CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
        remote.attach(core.event_sender());
        Ok(Node { core, remote, venue, pids })
    }

    /// **The production route of the shipped spread maker FILE** (plan 2b, the tradehub's sandboxed
    /// mount): built by the real builder from `strategies/market_making/spread_maker.rs`, opened in the
    /// sandbox on a live mount's facts (its grid and the static fee lane's floor, as the tradehub's
    /// `config/sandboxed.rs` hands them), it prices at the floor the in-process maker armed
    /// (`vike_tradehub_core::maker_round_trip_fee_for`); its account re-arm (`RemoteHandle::fee_rearm`)
    /// moves it to the account's own rate (`vike_model::maker_round_trip_fee`, the rate
    /// `vike_market_making::rearm_fee_floor` arms); and mounted on a core it quotes both sides.
    fn the_spread_maker_file_quotes_in_the_sandbox_at_the_in_process_fee_floor(artifact: &Path) {
        use vike_core::LiveBroker;
        use vike_model::{FeeSchedule, Strategy, StrategyParams};

        // The test's paper venue client is the `sim` mount every check here uses; the floor is the one a
        // hyperliquid BTC mount's static fee lane states, handed as the tradehub hands its mount's.
        let (venue_name, symbol) = ("sim", "BTCUSDT");
        let floor = vike_tradehub_core::maker_round_trip_fee_for("hyperliquid", "BTC");
        assert!(floor.is_some(), "hyperliquid's static fee lane states a floor");
        let spec = RemoteSpec {
            file: "market_making/spread_maker.rs".into(),
            sha256: "test".into(),
            params: "qty = 0.001\n".into(),
            facts: vike_model::params::MountFacts {
                symbol: symbol.into(),
                venue: venue_name.into(),
                interval: "1m".into(),
                tick_size: Some(1.0),
                static_round_trip_fee_rate: floor,
                account_round_trip_fee_rate: None,
            },
            controller_id: None,
        };
        let pids = Arc::new(Mutex::new(Vec::new()));
        let (proxy, remote) =
            RemoteStrategy::open(spec, launcher(artifact, pids)).expect("the maker file opens");
        let rate = |proxy: &RemoteStrategy| match Strategy::<LiveBroker>::params(proxy) {
            Some(StrategyParams::SpreadMaker(m)) => {
                m.avellaneda_stoikov.and_then(|a| a.round_trip_fee_rate)
            }
            other => panic!("not a spread maker's params: {other:?}"),
        };
        assert_eq!(rate(&proxy), floor, "the static floor, as the in-process maker armed it");
        let schedule = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 };
        let account_rate = vike_model::maker_round_trip_fee(schedule);
        assert_ne!(account_rate, floor, "a schedule whose rate moves the floor");
        assert!(remote.fee_rearm()(Some(schedule)).is_none(), "the mount keeps its proxy");
        assert_eq!(rate(&proxy), account_rate, "the account's own rate after the re-arm");

        let venue = Venue::default();
        let engine = Engine::new(
            Account::new(1_000_000.0, venue_name, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            venue.clone(),
            venue_name,
            symbol,
        );
        let mount = Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: venue_name.into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            strategy: Box::new(proxy),
        };
        let core =
            spawn_core(engine, CoreConfig { strategy: Some(mount), ..CoreConfig::default() });
        remote.attach(core.event_sender());
        for step in 0..5_u32 {
            let mid = 100_000.0 + f64::from(step);
            let mut b = vike_marketdata::L2Book::new(1.0);
            b.apply_snapshot(
                u64::from(step) + 1,
                &[vike_marketdata::L2BookLevel::new(mid - 1.0, 1.0)],
                &[vike_marketdata::L2BookLevel::new(mid + 1.0, 1.0)],
            );
            let update = vike_execution::BookUpdate {
                venue: venue_name.into(),
                symbol: symbol.into(),
                book: Arc::new(b),
            };
            core.tick_sender().book(update).expect("the core takes the book");
        }
        until("the maker's two quotes", || venue.submits.load(Ordering::SeqCst) >= 2);
        println!(
            "spread maker file: floor {floor:?} -> account rate {account_rate:?}, {} quotes",
            venue.submits.load(Ordering::SeqCst)
        );
        core.shutdown_and_join();
    }

    fn book(core: &CoreHandle) {
        let mut b = vike_marketdata::L2Book::new(0.01);
        b.apply_snapshot(
            1,
            &[vike_marketdata::L2BookLevel::new(100.0, 1.0)],
            &[vike_marketdata::L2BookLevel::new(100.02, 1.0)],
        );
        let update = vike_execution::BookUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(b),
        };
        core.tick_sender().book(update).expect("the core takes the book");
    }

    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn a_killed_child_cancels_nothing_and_its_restart_adopts_the_orders(artifact: &Path) {
        let n = node(artifact, "").expect("the strategy opens");
        book(&n.core);
        until("the two quotes", || n.venue.submits.load(Ordering::SeqCst) == 2);
        // The venue takes both: their order events reach the child, which then sends its state.
        let sender = n.core.event_sender();
        for order_client_id in n.venue.order_client_ids.lock().unwrap().clone() {
            let id = || order_client_id.clone();
            sender
                .blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: id(),
                    timestamp: 0,
                }))
                .unwrap();
            sender
                .blocking_send(Event::OrderAccepted(OrderAccepted {
                    order_client_id: id(),
                    venue_order_id: None,
                    timestamp: 0,
                }))
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(500));
        book(&n.core);
        until("the first amends", || n.venue.amends.load(Ordering::SeqCst) == 2);

        let first = n.pids.lock().unwrap()[0];
        let killed = std::process::Command::new("kill").args(["-9", &first.to_string()]).status();
        assert!(killed.expect("kill runs").success(), "kill -9 {first}");
        until("the restart", || {
            n.pids.lock().unwrap().len() == 2 && n.remote.status() == RemoteStatus::Running
        });
        let reason = n.remote.last_reason().unwrap_or_default();
        assert!(reason.contains("SIGKILL"), "the reason names the kill: {reason}");
        assert_eq!(n.venue.cancels.load(Ordering::SeqCst), 0, "a crash cancels nothing");
        let snap = n.core.snapshot();
        assert_eq!(snap.orders.len(), 2, "both quotes rest: {:?}", snap.orders);

        book(&n.core);
        until("the restarted child's amends", || n.venue.amends.load(Ordering::SeqCst) == 4);
        assert_eq!(n.venue.submits.load(Ordering::SeqCst), 2, "it adopted them: no second place");
        n.core.shutdown_and_join();
        println!("ok: kill -9 cancels nothing; the restart adopts the resting quotes ({reason})");
    }

    fn a_hung_hook_is_killed_and_applies_nothing(artifact: &Path) {
        let n = node(artifact, "mode = \"hang\"\n").expect("the strategy opens");
        let t = Instant::now();
        book(&n.core);
        until("the restart", || n.pids.lock().unwrap().len() == 2);
        let reason = n.remote.last_reason().unwrap_or_default();
        assert!(reason.contains("hang: no reply to on_order_book"), "{reason}");
        assert!(
            t.elapsed() < Duration::from_secs(4),
            "killed at the deadline, not at the hook's end"
        );
        assert_eq!(n.venue.submits.load(Ordering::SeqCst), 0, "the hung hook applied nothing");
        n.core.shutdown_and_join();
        println!("ok: a hung hook is killed at the deadline ({reason})");
    }

    fn a_stray_symbol_applies_nothing(artifact: &Path) {
        let n = node(artifact, "mode = \"stray\"\n").expect("the strategy opens");
        book(&n.core);
        until("the restart", || n.pids.lock().unwrap().len() == 2);
        let reason = n.remote.last_reason().unwrap_or_default();
        assert!(reason.contains("garbage: an intent for `ETHUSDT`"), "{reason}");
        assert_eq!(n.venue.submits.load(Ordering::SeqCst), 0, "none of the reply was applied");
        n.core.shutdown_and_join();
        println!("ok: garbage applies nothing ({reason})");
    }

    fn a_missing_artifact_is_the_mounts_reason() {
        let missing = Path::new("/nonexistent/vike/strategy.so");
        let Err(reason) = node(missing, "") else { panic!("a missing artifact opened") };
        println!("ok: a missing artifact does not open ({reason})");
    }
}

#[cfg(target_os = "linux")]
mod log_probe {
    use std::io::Read;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use vike_tradehub_core::remote_strategy::{Launched, Launcher, RemoteSpec, RemoteStrategy};

    /// The child's stderr, passed on to the supervisor's tail and copied here as it is read.
    struct Tee {
        inner: Box<dyn Read + Send>,
        copy: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for Tee {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buffer)?;
            self.copy.lock().unwrap().extend_from_slice(&buffer[..n]);
            Ok(n)
        }
    }

    struct Process(std::process::Child);

    impl vike_tradehub_core::remote_strategy::SandboxChild for Process {
        fn kill(&mut self) {
            let _ = self.0.kill();
        }
        fn wait(&mut self) -> String {
            self.0.wait().map_or_else(|e| e.to_string(), |s| s.to_string())
        }
    }

    /// A strategy file's OWN `tracing::warn!` (F3's log slot, `ABI_VERSION` 9) appears in the sandbox
    /// child's stderr log, written by the child's subscriber: the child is the production verb, jailed,
    /// started by `vike_sandbox::spawn_strategy_process`, and the file warns at the `Open`.
    pub(super) fn a_files_own_warning_reaches_the_childs_stderr(artifact: &Path) {
        let copy = Arc::new(Mutex::new(Vec::new()));
        let me = std::env::current_exe().expect("this binary's path");
        let args = ["strategy-sandbox".into(), "live".into(), artifact.as_os_str().to_owned()];
        let seen = Arc::clone(&copy);
        let launcher: Launcher = Box::new(move || {
            let mut child = vike_sandbox::spawn_strategy_process(&me, args.clone())?;
            let (i, o, e) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
            Ok(Launched {
                to_child: Box::new(i.expect("piped")),
                from_child: Box::new(o.expect("piped")),
                reasons: e.map(|e| {
                    Box::new(Tee { inner: Box::new(e), copy: Arc::clone(&seen) })
                        as Box<dyn Read + Send>
                }),
                child: Box::new(Process(child)),
            })
        });
        let spec = RemoteSpec {
            file: "other/log_probe.rs".into(),
            sha256: "test".into(),
            params: String::new(),
            facts: vike_model::params::MountFacts {
                symbol: "BTCUSDT".into(),
                venue: "sim".into(),
                interval: "1m".into(),
                ..Default::default()
            },
            controller_id: None,
        };
        let (_proxy, _remote) = RemoteStrategy::open(spec, launcher).expect("the log probe opens");
        let deadline = Instant::now() + Duration::from_secs(20);
        let line = loop {
            let text = String::from_utf8_lossy(&copy.lock().unwrap()).into_owned();
            if let Some(line) = text.lines().find(|l| l.contains("f3-log-probe")) {
                break line.to_string();
            }
            assert!(
                Instant::now() < deadline,
                "the file's own warning never reached the child's stderr; it held: {text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(line.contains("WARN") && line.contains("rungs=3"), "{line}");
        println!("ok: the file's own warning is on the child's stderr: {line}");
    }
}
#[cfg(target_os = "linux")]
use log_probe::a_files_own_warning_reaches_the_childs_stderr;
