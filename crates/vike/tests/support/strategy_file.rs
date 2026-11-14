//! What the two sandboxed-backtest suites share (`tests/strategy_file_run.rs`, `tests/strategy_sandbox_backtest.rs`):
//! the strategy FILE, its build through the PRODUCTION builder, the project it is staged in (the file in
//! `strategies/`, its artifact in the build folder), the store, one raw exchange with a backtesthub, and
//! the sandboxed children of this process.
//!
//! ⚠ It names nothing of the loader: `strategy_file_run.rs` proves the production route reaches the file,
//! and its own source scan would not see a by-hand load hidden here.

use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_backtesthub_client::proto::{Request, Response};
use vike_backtesthub_client::wire_studio::{DataSliceWire, ReplayKindWire, StrategySpecWire};
use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_hub_protocol::frame::{read_frame_raw, write_frame};
use vike_strategy_builder::render::{CargoHomePin, Profile, build_strategy_file, child_env};

/// The user strategy the suites build — an ORDINARY strategy file, with its header and the `build`
/// entry, importing `vike_model` and `vike_marketdata` and nothing else.
///
/// It is PARAMETER-DRIVEN and it TRADES, and both are load-bearing: `every` decides how often it flips,
/// so two params tables produce two trade counts (params crossed the boundary is an observation, not an
/// assumption), and it alternates SIDE, so the engine closes round trips. `pause_ms` makes every bar
/// sleep, so a run lasts long enough to be cancelled.
///
/// It also PRINTS on its first bar. The child points its fd 1 at its stderr before it loads the file, so
/// the line lands in the reason pipe and never in the protocol: a run that answers at all proves it.
pub(crate) const SOURCE: &str = r#"//! Pulse: flips side every `every` bars (the sandboxed-backtest suites' strategy file).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//!
//! [params.every]
//! type = "integer"
//! default = 7
//! [params.size]
//! type = "number"
//! default = 1.0
//! [params.pause_ms]
//! type = "integer"
//! default = 0
//! ```

use vike_model::{OrderBroker, Strategy, OrderPlacement};
use vike_marketdata::Bar;

pub struct Pulse {
    size: f64,
    every: usize,
    pause_ms: u64,
    seen: usize,
    long: bool,
}

impl<B: OrderBroker> Strategy<B> for Pulse {
    fn warmup(&self) -> usize {
        1
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        if self.seen == 0 {
            println!("pulse: first bar at {}", bar.timestamp);
        }
        self.seen += 1;
        if self.pause_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.pause_ms));
        }
        if self.seen % self.every != 0 {
            return;
        }
        let Some(symbol) = bar.symbol.clone().filter(|s| !s.is_empty()) else {
            return;
        };
        let side = if self.long { -1 } else { 1 };
        broker.place(&symbol, "", side, self.size, OrderPlacement::Market, false);
        self.long = !self.long;
    }
}

pub fn build<B: vike_model::OrderBroker + 'static>(
    params: &vike_model::params::Params,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let every = params
        .get("every")
        .and_then(|v| v.as_integer().or_else(|| v.as_float().map(|f| f as i64)))
        .map(|i| i.max(1) as usize)
        .unwrap_or(7);
    let size = params
        .get("size")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    let pause_ms = params.get("pause_ms").and_then(|v| v.as_integer()).unwrap_or(0).max(0) as u64;
    Box::new(Pulse { size, every, pause_ms, seen: 0, long: false })
}
"#;

/// The file's path under the project's `strategies/` folder.
pub(crate) const FILE: &str = "other/join_probe.rs";

/// Build `text` (the file at `path`) through the production builder into a CACHED builds directory under
/// this test binary's scratch space, and return the artifact path: `<builds>/<folder>/<sha256>.so`, the
/// folder naming the ABI, the toolchain fingerprint, the template, the profile and the commit. `Err` is the
/// builder's refusal (the compiler's diagnostics for a file that does not build).
pub(crate) fn build_text(path: &str, text: &str) -> Result<PathBuf, String> {
    let builds = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("strategy-file-sandbox-builds");
    // Two levels up from this crate's manifest directory is the checkout root, which the rendered
    // scratch crate's `path` dependencies resolve against.
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    // The exact cargo running this test, so the nested build uses the same toolchain rather than
    // whatever is first on PATH. `Layer::TestOnly`.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    // The service's own cleared environment for the nested cargo (`render::child_env`).
    let env = child_env(&std::env::vars().collect());
    build_strategy_file(
        text,
        path,
        &builds,
        &workspace_root,
        &cargo,
        &env,
        Profile::of_this_binary(),
        &CargoHomePin::disabled(),
    )
    .map_err(|e| e.to_string())
}

/// [`SOURCE`] at [`FILE`], built ([`build_text`]).
pub(crate) fn build_the_artifact() -> PathBuf {
    build_text(FILE, SOURCE)
        .unwrap_or_else(|e| panic!("the production builder must build the fixture source:\n{e}"))
}

/// Copy `artifact` into `builds`, in its own build folder: what the builder service writes on a box.
pub(crate) fn stage(artifact: &Path, builds: &Path) {
    let folder = artifact.parent().and_then(Path::file_name).expect("a build folder");
    let staged = builds.join(folder);
    std::fs::create_dir_all(&staged).expect("plant the build folder");
    std::fs::copy(artifact, staged.join(artifact.file_name().expect("artifact name")))
        .expect("stage the artifact");
}

/// A fresh project (a `settings/` marker, the deployed shape) that the test ENTERS: [`SOURCE`] at [`FILE`]
/// under its `strategies/`, and `artifact` staged in its build folder where the daemon's OWN resolver
/// looks (`strategy_files_dir()`'s answer, never a path spelled here). Returns the project (keep it
/// alive), its `strategies/` folder, and the working directory to restore. The working directory is
/// process-global, which is why each suite runs its phases in ONE `#[test]`.
pub(crate) fn enter_a_project_holding(artifact: &Path) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let project = tempfile::tempdir().expect("project root");
    std::fs::create_dir_all(project.path().join("settings")).expect("plant the project marker");
    let strategies = project.path().join("strategies");
    let file = strategies.join(FILE);
    std::fs::create_dir_all(file.parent().expect("a folder")).expect("plant strategies/other");
    std::fs::write(&file, SOURCE).expect("write the strategy file");
    let restore = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(project.path()).expect("enter the project");
    let builds = vike_backtesthub::studio_run::strategy_files_dir();
    let root = builds.parent().and_then(Path::parent).expect("<project>");
    assert!(
        root.join("settings").is_dir(),
        "the resolver answered {}, whose project carries no `settings/` marker",
        builds.display()
    );
    stage(artifact, &builds);
    (project, strategies, restore)
}

/// Every sandboxed child of this process is `vike-backend strategy-sandbox`.
pub(crate) fn use_the_multicall_as_the_sandbox() {
    vike_backtesthub::strategy_sandbox::use_program(PathBuf::from(env!(
        "CARGO_BIN_EXE_vike-backend"
    )));
}

/// A deterministic random walk of `n` one-minute BTCUSDT bars on binance, in a memory store.
pub(crate) fn seeded_store(n: usize) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let bars = vike_studio_core::test_support::walk::random_walk_bars(n, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed bars");
    Arc::new(store)
}

/// The slice every Studio request here runs: binance BTCUSDT 1m, unbounded.
pub(crate) fn slice() -> Box<DataSliceWire> {
    Box::new(DataSliceWire {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    })
}

/// The spec under test: [`FILE`], with `every` and `pause_ms` the phases move.
pub(crate) fn spec(every: i64, pause_ms: i64) -> StrategySpecWire {
    StrategySpecWire::File {
        file: FILE.to_string(),
        params_toml: format!("every = {every}\npause_ms = {pause_ms}\nsize = 1.0\n"),
    }
}

/// One request to the backtesthub at `addr` and the answer's RAW frame body: the bytes it wrote.
pub(crate) fn ask_raw(addr: SocketAddr, request: &Request) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("dial the backtesthub");
    write_frame(&mut stream, request).expect("send the request");
    stream.flush().expect("flush");
    read_frame_raw(&mut stream).expect("read the answer")
}

/// [`ask_raw`], decoded.
pub(crate) fn ask(addr: SocketAddr, request: &Request) -> Response {
    serde_json::from_slice(&ask_raw(addr, request)).expect("decode the answer")
}

/// The pids of this process's `strategy-sandbox` children, read off `/proc` (`stat`'s fourth field is
/// the parent).
pub(crate) fn sandbox_children() -> Vec<u32> {
    let me = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let parent = stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(1))
                .and_then(|p| p.parse::<u32>().ok());
            parent == Some(me)
                && std::fs::read(format!("/proc/{pid}/cmdline"))
                    .is_ok_and(|c| c.windows(16).any(|w| w == b"strategy-sandbox"))
        })
        .collect()
}
