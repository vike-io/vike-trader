//! `vike-backend telegram` — **the program**: the stop handlers, the boot, the rows it reads, then
//! the poll loop on the main thread until a stop.
//!
//! ```text
//! vike-backend telegram [--node <HOST:PORT>] [--no-poll]
//! ```
//!
//! Run by `deploy/vike-telegram.service`, beside the tradehub's unit and independent of it: this
//! process holds no engine, no core and no venue session, so starting, stopping or restarting it
//! touches nothing of the node's, and the node runs with it stopped (decision 0256).
//!
//! # The ready line, and `--no-poll`
//!
//! [`READY_LINE`] is printed once every start step has passed and before the first Telegram call.
//! The deploy helper greps for it (`deploy/sbin/vike-trader-ci-deploy`'s `unit_shape`): its
//! pre-flight runs `telegram --no-poll` on a copy of the settings, which takes every start step,
//! emits [`READY_LINE`] and then waits for the stop, calling neither Telegram nor the node; its health
//! poll after the restart reads the same line from the unit's journal.
//!
//! # What it reads, and from where (all rows of `<project>/settings/db/vike.db`)
//!
//! - **the two gates**, `flags.tradehub_control` and `flags.telegram_control`;
//! - **the node's address**: `--node`, else `config.tradehub_addr` (where the tradehub on THIS box
//!   binds), else [`DEFAULT_NODE_ADDRESS`] (the tradehub's own compiled-in bind);
//! - **the bot token and the allowlists**, [`TELEGRAM_KEYS`], one scoped read of the credential
//!   store handle the boot opened under `CredentialScope::NoVenueKeys`: this process can read no
//!   venue or account credential value;
//! - **the node's two hub keys**, from the `hub_key` table (`vike_client::resolved::hub_keyring`, the
//!   read `vike-cli trade` makes), and the link's patience and the confirm's wait
//!   (`preferences.control_reply_timeout_ms`, `config.node_ack_wait_ms`), the CLI's rows.
//!
//! Nothing comes from the environment or the command line but `--node` and the pointer to the
//! database (decision 0111); a credential VALUE never travels on argv (decision 0036).
//!
//! # Exit codes
//!
//! - `0`: the gates are closed (the channel is OFF, and the unit then stays down: `systemctl
//!   restart vike-telegram` once a row is flipped), or a stop was asked for (`--no-poll` included).
//! - `2`: the command line, or the boot, refused.
//! - [`EXIT_CONFIG`] (78, `EX_CONFIG`): a gate is open and the channel cannot start (no token, no
//!   chat, no ledger, no hub key, an unsound settings seal), or Telegram refused the token for good.
//!   A restart cannot help, and the unit's `RestartPreventExitStatus=78` keeps systemd from trying.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_boot::stop::{self, StopSignal};
use vike_client::resolved::{Resolved, hub_keyring};
use vike_log::poller::STOP_POLL_SLICE;
use vike_utility::cli::Program;

use crate::{
    LoopEnd, NodeLink, ProdTelegramDependencies, TELEGRAM_KEYS, TelegramConfig,
    TelegramDependencies, UpdateLedger, arm, control_gates_open, run_loop,
};

/// The name a stderr line and a usage line open with.
const BIN: &str = "vike-backend telegram";

/// The line printed once every start step has passed, before the first Telegram call: the deploy
/// helper's readiness value for this unit (`deploy/sbin/vike-trader-ci-deploy`'s `unit_shape`, a
/// fixed string), in its pre-flight and in its health poll. Renaming it rolls back a good deploy.
pub const READY_LINE: &str = "vike-telegram: ready";

/// The tradehub's own compiled-in bind address (`vike_tradehub::server::DEFAULT_ADDRESS`, held equal by
/// `crates/vike-telegram/tests/telegram_control/node_wire.rs`'s
/// `the_default_node_is_the_tradehubs_own_default_bind`): the address this process dials on a box
/// whose `config.tradehub_addr` row is unset, which is where the tradehub then listens.
pub const DEFAULT_NODE_ADDRESS: &str = "127.0.0.1:7879";

/// `EX_CONFIG`: the channel cannot start as configured, and restarting cannot change that.
pub const EXIT_CONFIG: u8 = 78;

/// The program's command line.
#[derive(clap::Parser, Debug)]
#[command(about = "The Telegram control channel: a client of the tradehub's control wire", long_about = None)]
struct TelegramArgs {
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the vike-tradehub to control [default: config.tradehub_addr, then 127.0.0.1:7879]"
    )]
    node: Option<String>,
    #[arg(
        long,
        help = "take every start step, print the ready line, then wait for a stop calling neither \
                Telegram nor the node (the deploy helper's pre-flight)"
    )]
    no_poll: bool,
}

/// A resolved boolean row, spelled as the exact `"1"` string the pure gate parses (`false` is
/// absent, which that grammar has always read as closed).
fn as_gate(on: bool) -> Option<&'static str> {
    on.then_some("1")
}

/// The multicall's entry point. `args` is the tail after the verb.
pub fn run(env: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    // `--help` and `--version` before anything opens a store; a refusal is exit 2.
    let cli: TelegramArgs = match Program::new(BIN, &version).try_parse_from(args) {
        Ok(cli) => cli,
        Err(exit) => exit.emit(),
    };

    // THE STOP HANDLERS, FIRST: the handler only stores a bool, so this can never be too early, and
    // until it is installed SIGTERM kills the process where it stands.
    let stop = StopSignal::new();
    let handlers = stop::install_handlers(&stop.flag());

    let booted = match boot(env, cwd) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{BIN}: {e}");
            return ExitCode::from(2);
        }
    };
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "vike-telegram".to_string(),
        file_level: booted.settings.preferences.log_file_level.clone(),
        project_dir: booted.log_home.clone(),
        ..Default::default()
    });
    tracing::info!("{}", booted.identity_line);
    match handlers {
        stop::HandlerOutcome::Installed => {
            tracing::info!("vike-telegram: SIGTERM/SIGINT will stop gracefully");
        }
        stop::HandlerOutcome::Unsupported => {
            tracing::info!("vike-telegram: no POSIX signals on this platform; stop with Ctrl-C");
        }
        stop::HandlerOutcome::Failed(e) => tracing::error!(
            error = %e,
            "vike-telegram: could NOT install the signal handler — a SIGTERM will kill this \
             process where it stands (an update marked and not yet answered is lost, never replayed)"
        ),
    }
    // The loader's warnings are printed by `Resolved::from_booted` below, on stderr (the journal).
    let gates = (
        as_gate(booted.settings.flags.tradehub_control),
        as_gate(booted.settings.flags.telegram_control),
    );
    if !control_gates_open(gates.0, gates.1) {
        tracing::info!(
            "vike-telegram: the Telegram control channel is OFF (flags.tradehub_control and \
             flags.telegram_control must both be on); nothing was read and nothing started"
        );
        return ExitCode::SUCCESS;
    }
    let addr = cli
        .node
        .or_else(|| booted.settings.config.tradehub_addr.clone())
        .unwrap_or_else(|| DEFAULT_NODE_ADDRESS.to_string());

    let resolved = Resolved::from_booted(booted, env.clone(), BIN);
    // This program ACTS on the node's ceilings, so it refuses where `vike-cli trade` does.
    if let Some(why) = resolved.seal_refusal.as_deref() {
        tracing::error!("{}", vike_client::resolved::seal_refusal_text(BIN, why));
        return ExitCode::from(EXIT_CONFIG);
    }
    let keys = hub_keyring(&resolved, BIN);
    let (Some((observe_key, _)), Some((control_key, _))) = (keys.observe(), keys.control()) else {
        tracing::error!(
            "vike-telegram: Telegram control NOT started: {}",
            keys.absent_message("vike-backend telegram")
        );
        return ExitCode::from(EXIT_CONFIG);
    };
    let node = NodeLink {
        addr,
        observe_key: observe_key.to_string(),
        control_key: control_key.to_string(),
        reply_timeout: keys.patience().control_reply,
        ack_wait: resolved.node_ack_wait,
    };
    let ledger_path = resolved.state_dir.as_ref().map(|dir| dir.join(TELEGRAM_LEDGER_FILE));
    let Some((config, ledger)) =
        arm(gates.0, gates.1, || telegram_vars(&resolved.store), ledger_path)
    else {
        // `arm` said why, at `error`.
        return ExitCode::from(EXIT_CONFIG);
    };
    let addr = node.addr.clone();
    let dependencies = ProdTelegramDependencies::new(&config, node);
    match serve(&config, &ledger, &dependencies, &addr, &stop.flag(), cli.no_poll) {
        LoopEnd::Stopped => ExitCode::SUCCESS,
        LoopEnd::Refused => ExitCode::from(EXIT_CONFIG),
    }
}

/// Every start step has passed: print [`READY_LINE`], then poll until a stop. With `no_poll` (the
/// deploy helper's pre-flight) wait for the stop instead and call nothing on `dependencies`: no Telegram
/// request, no message, no dial of the node.
fn serve(
    config: &TelegramConfig,
    ledger: &UpdateLedger,
    dependencies: &dyn TelegramDependencies,
    node: &str,
    stop: &AtomicBool,
    no_poll: bool,
) -> LoopEnd {
    if no_poll {
        tracing::info!(node, "{READY_LINE}; --no-poll: Telegram is not called, waiting for a stop");
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(STOP_POLL_SLICE);
        }
        return LoopEnd::Stopped;
    }
    tracing::info!(
        node,
        "{READY_LINE}; polling Telegram; commands go to the tradehub's control port"
    );
    run_loop(config, ledger, dependencies, stop)
}

/// The at-most-once ledger's basename inside the project state directory: the file the tradehub's
/// in-process channel wrote, so the high-water mark carries over the move.
///
/// **`.ledger`, not the `.log` it used to be.** The file is not a log and never was — it holds one
/// integer per line and its only reader takes the max — and `<settings>/state/` is a SHARED
/// directory, so a name that says what the file IS earns its keep. Deliberately NOT its own knob:
/// `$VIKE_SETTINGS_DIR` already relocates it.
pub const TELEGRAM_LEDGER_FILE: &str = "telegram_updates.ledger";

/// The channel's three rows of the credential store, read under the boot's scope, narrowed to
/// [`TELEGRAM_KEYS`]. A store that exists and will not open is an `error!` and an EMPTY map, which
/// [`arm`] then refuses as unconfigured: nothing starts.
fn telegram_vars(store: &vike_secrets::CredentialStore) -> HashMap<String, String> {
    match store.read(&vike_secrets::CredentialKeyScope::of(TELEGRAM_KEYS)) {
        Ok(scoped) => {
            if let Some(w) = &scoped.warning {
                // A permission finding: paths and a mode, never a value. Reported, not enforced.
                tracing::warn!("vike-telegram: {w}");
            }
            scoped.into_map()
        }
        Err(e) => {
            tracing::error!("vike-telegram: the credential store exists and cannot be read: {e}");
            HashMap::new()
        }
    }
}

/// The startup this program needs: the ONE project walk, the settings (the two gates, the node's
/// address, the CLI's link rows), and a credential handle that can read no venue key.
fn boot(env: &HashMap<String, String>, cwd: Option<&Path>) -> Result<vike_boot::Booted, String> {
    vike_boot::boot(&vike_boot::BootSpec {
        env,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        // REPORT: `Resolved::seal_refusal` carries the mark and `run` refuses on it, after the log
        // is up, as `vike-mcp` does.
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::DeferredNoVenueKeys(
            "the channel reads its three rows (`TELEGRAM_KEYS`) AFTER the gates, scoped, so on the \
             closed path the bot token never enters this process; the hub keys are the `hub_key` \
             table's, and no venue or account credential is reachable at all.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        disclosure: vike_boot::Disclosure::Skip(
            "`boot_lines` re-reads every settings row for its origin; this process consumes four \
             rows and says which it acted on.",
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use vike_tradehub_client::wire::CommandWire;

    use super::*;
    use crate::{Outcome, PollError, ReadVerb, TelegramUpdate};

    /// A [`TelegramDependencies`] that counts EVERY call, and raises `stop` at the first `getUpdates`.
    struct Counted<'a> {
        calls: AtomicUsize,
        stop: &'a AtomicBool,
    }

    impl Counted<'_> {
        fn hit(&self) {
            self.calls.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl TelegramDependencies for Counted<'_> {
        fn get_updates(&self, _: i64) -> Result<Vec<TelegramUpdate>, PollError> {
            self.hit();
            self.stop.store(true, Ordering::Relaxed);
            Ok(Vec::new())
        }
        fn send_message(&self, _: i64, _: &str) {
            self.hit();
        }
        fn preview(&self, _: &CommandWire) -> Result<Vec<String>, String> {
            self.hit();
            Ok(Vec::new())
        }
        fn accept(&self, _: CommandWire, _: &str) -> Outcome {
            self.hit();
            Outcome::Refused(String::new())
        }
        fn read(&self, _: ReadVerb) -> String {
            self.hit();
            String::new()
        }
        fn fill_order_client_id(&self, cmd: CommandWire) -> CommandWire {
            self.hit();
            cmd
        }
        fn mint_token(&self) -> String {
            self.hit();
            String::new()
        }
        fn now_ms(&self) -> i64 {
            self.hit();
            0
        }
    }

    /// [`serve`] over [`Counted`], the stop also raised by another thread after 300 ms: how it ended,
    /// the messages it logged that open with [`READY_LINE`], and how many calls reached the deps.
    fn serve_counted(no_poll: bool) -> (LoopEnd, Vec<String>, usize) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let ledger = UpdateLedger::open(&dir.path().join(TELEGRAM_LEDGER_FILE)).expect("a ledger");
        let config = TelegramConfig::new("123:token", vec![42]).expect("a config");
        let stop = AtomicBool::new(false);
        let dependencies = Counted { calls: AtomicUsize::new(0), stop: &stop };
        let (end, events) = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                stop.store(true, Ordering::Relaxed);
            });
            vike_log::capture::captured(|| {
                serve(&config, &ledger, &dependencies, "127.0.0.1:7879", &stop, no_poll)
            })
        });
        let ready = events.into_iter().map(|e| e.message).filter(|m| m.starts_with(READY_LINE));
        (end, ready.collect(), dependencies.calls.load(Ordering::Relaxed))
    }

    /// The deploy helper's pre-flight runs `telegram --no-poll`: the ready line is printed, nothing
    /// reaches Telegram or the node, and the stop ends it. The control is the same pass without the
    /// flag, which prints the same line and polls.
    #[test]
    fn no_poll_prints_the_ready_line_and_calls_nothing_until_the_stop() {
        let (end, ready, calls) = serve_counted(true);
        assert_eq!(end, LoopEnd::Stopped);
        assert_eq!(ready.len(), 1, "one ready line, before the wait: {ready:?}");
        assert_eq!(calls, 0, "--no-poll reached Telegram or the node {calls} time(s)");

        let (end, ready, calls) = serve_counted(false);
        assert_eq!(end, LoopEnd::Stopped);
        assert_eq!(ready.len(), 1, "the unit's start prints the same line: {ready:?}");
        assert!(calls >= 1, "without --no-poll the program polls Telegram");
    }

    #[test]
    fn no_poll_is_a_flag_and_off_by_default() {
        let no_poll =
            |args: &[&str]| match Program::new(BIN, "test").try_parse_from(args.iter().copied()) {
                Ok(TelegramArgs { no_poll, .. }) => no_poll,
                Err(e) => panic!("{args:?} did not parse: {e:?}"),
            };
        assert!(no_poll(&["--no-poll"]));
        assert!(!no_poll(&[]));
    }
}
