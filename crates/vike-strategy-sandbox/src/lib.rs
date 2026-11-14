//! `vike-backend strategy-sandbox`: the ONE process that loads a Rust strategy file.
//!
//! A hub never loads a strategy file (decisions 0082/0083: the order-signing binary loads no code, and the
//! backtesthub joined it). It spawns this verb instead, one child per backtest request or per live mount,
//! with `vike_sandbox::spawn_strategy_process` (an empty environment, working directory `/`, three pipes),
//! and the child jails itself BEFORE it loads the file, because a file can run code at load time
//! (`.init_array`).
//!
//! # The verbs
//!
//! - `strategy-sandbox backtest <builds-dir> <sha>` — a whole backtesthub request runs inside the jail: the
//!   request comes down the protocol, the history the run reads goes up it (answered from the hub's store,
//!   read verbs only), and the finished wire answer comes back, produced by the backtesthub's own verb code
//!   ([`BacktestServe`]), so its bytes are the in-process run's. The artifact's PATH is computed here
//!   (`vike_rust_strategy::artifact::artifact_path` under this binary's build folder): the folder names the
//!   loader's ABI and toolchain fingerprint, which only this crate links.
//! - `strategy-sandbox live <artifact>` — one live mount's hooks (unit F5 of plan 2b).
//!
//! # The order inside the child (`vike_sandbox::StrategyProcessPolicy::apply`'s rules)
//!
//! 1. **The jail, first**: before any thread starts, before any descriptor opens (it refuses a process
//!    holding one above stderr) and before logging starts. Then a console-only subscriber on stderr at
//!    `warn` (`vike_log::init`, no file layer: the jail grants no write), so this process's own warnings
//!    reach the reason pipe. ⚠ A strategy FILE's `tracing` events do not reach it: the artifact links its
//!    own copy of `tracing` with its own global dispatcher.
//! 2. **The protocol's output leaves fd 1, and fd 1 becomes stderr.** A strategy file links its OWN copy
//!    of std, so a `println!` in a user's `on_bar` writes fd 1 whatever this process does with its
//!    `stdout()` handle; on a stdout protocol that line would be a garbage frame and kill the run. After
//!    the move it lands in the reason pipe the parent drains. Done after the jail, because the copy of
//!    the protocol's descriptor is a descriptor above stderr.
//! 3. **The load**, inside the jail (`vike_rust_strategy::loader::load`). A missing artifact (or a "sha" that
//!    is not 64 hex digits, which names none) is the loader's own `Missing` refusal, built without a load,
//!    and nothing is jailed or loaded.
//! 4. **The verb**, with stdin as the protocol's input.
//!
//! Where the jail cannot be applied (not Linux, a kernel without Landlock ABI 4, a unit without
//! `@sandbox`) nothing loads and the refusal, by name, is the run's strategy error. No override.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_model::Strategy;
use vike_rust_strategy::artifact;
use vike_rust_strategy::host::StrategyFileHost;
use vike_rust_strategy::loader::{self, LoadError};
use vike_simulator::OrderBrokerSimulator;

mod live;

/// The backtesthub's half of a sandboxed backtest (`vike_backtesthub::strategy_sandbox::serve_child`):
/// read ONE request from `input`, run it, write its answer to `output`, reading history through the
/// same channel. `build` is the loaded file — a factory of strategies over a params table (TOML text) and
/// the mount facts (JSON text), called once per run, sweep point and walk-forward window — or the refusal,
/// in the loader's words.
///
/// A function handed in by the composition root (`crates/vike/src/main.rs`), not a dependency: this crate
/// and `vike-backtesthub` are both layer 65, and the backtesthub must not link the loader.
pub type BacktestServe = fn(
    input: Box<dyn Read + Send>,
    output: Box<dyn Write + Send>,
    build: Result<
        Box<dyn Fn(&str, &str) -> Box<dyn Strategy<OrderBrokerSimulator>> + Send + Sync>,
        String,
    >,
) -> ExitCode;

/// The words `-h` prints. Nobody types this verb; a hub spawns it.
const USAGE: &str = "usage: vike-backend strategy-sandbox backtest <builds-dir> <sha>\n\
       vike-backend strategy-sandbox live <artifact>\n\n\
A hub spawns this verb to run ONE Rust strategy file jailed (no file, no network, no other process),\n\
with the protocol on stdin and stdout. It is not run by hand.\n";

/// Route `args` (the tail after `strategy-sandbox`) to its verb.
#[must_use]
pub fn run(args: &[String], backtest: BacktestServe) -> ExitCode {
    match args.split_first() {
        Some((verb, _)) if verb == "-h" || verb == "--help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some((verb, [builds, sha])) if verb == "backtest" => {
            backtest_verb(&artifact_of(Path::new(builds), sha), backtest)
        }
        Some((verb, rest)) if verb == "live" => live::run(rest),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The artifact built from text hashing to `sha` under `builds`, in THIS binary's build folder (its
/// profile and commit, the builder's rule: `vike_rust_strategy::artifact::build_folder`). A `sha` that is
/// not 64 hex digits names nothing, so no `..` in it can reach outside the folder. The ONE rule: the live
/// verb's parent (the tradehub, through its composition root) names the artifact by it too.
#[must_use]
pub fn artifact_of(builds: &Path, sha: &str) -> PathBuf {
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return builds.join("not-a-sha256");
    }
    let folder = artifact::build_folder(artifact::host_profile(), vike_buildinfo::GIT_SHA);
    artifact::artifact_path(builds, &folder, sha)
}

/// The backtest verb, in the module doc's order.
fn backtest_verb(artifact: &Path, serve: BacktestServe) -> ExitCode {
    let jailed = if artifact.is_file() {
        let mut policy = vike_sandbox::strategy_process_policy(artifact);
        // A whole run and its history live inside: the unit's cgroup bounds a backtest, not the live cap.
        policy.address_space_bytes = None;
        policy.apply().map_err(|why| format!("this box cannot run a Rust strategy file: {why}"))
    } else {
        Err(LoadError::Missing(artifact.to_path_buf()).to_string())
    };
    // After the jail (it refuses a process that started logging), console only: the jail grants no write.
    let _log = vike_log::init(vike_log::LogConfig {
        console_level: "warn".to_string(),
        file_enabled: false,
        ..vike_log::LogConfig::default()
    });
    let output = match protocol_output() {
        Ok(output) => output,
        Err(e) => {
            eprintln!("strategy-sandbox: the protocol could not leave stdout: {e}");
            return ExitCode::from(3);
        }
    };
    let input = Box::new(std::io::stdin());
    match jailed.and_then(|()| loader::load(artifact).map_err(|e| e.to_string())) {
        Ok(loaded) => {
            let vtable = loaded.vtable;
            serve(
                input,
                output,
                Ok(Box::new(move |params_toml: &str, facts_json: &str| {
                    Box::new(StrategyFileHost::<OrderBrokerSimulator>::new(
                        vtable,
                        params_toml,
                        facts_json,
                    )) as Box<dyn Strategy<OrderBrokerSimulator>>
                })),
            )
        }
        Err(refusal) => serve(input, output, Err(refusal)),
    }
}

/// The protocol's output, moved off fd 1, and fd 1 pointed at stderr (the module doc's step 2).
#[cfg(target_os = "linux")]
fn protocol_output() -> std::io::Result<Box<dyn Write + Send>> {
    use std::os::fd::AsFd;
    let protocol = std::io::stdout().as_fd().try_clone_to_owned()?;
    rustix::stdio::dup2_stdout(std::io::stderr().as_fd())?;
    Ok(Box::new(std::fs::File::from(protocol)))
}

/// Elsewhere the jail refuses and nothing loads, so stdout stays the protocol.
#[cfg(not(target_os = "linux"))]
fn protocol_output() -> std::io::Result<Box<dyn Write + Send>> {
    Ok(Box::new(std::io::stdout()))
}
