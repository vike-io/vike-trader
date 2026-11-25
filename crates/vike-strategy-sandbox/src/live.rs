//! `strategy-sandbox live <builds-dir> <artifact>`: serve ONE live mount's hooks to the tradehub that spawned this
//! process (plan 2b §4, unit F5). The protocol and the serve loop are the tradehub's
//! (`vike_tradehub_core::remote_strategy`, whose module doc carries the failure table); this verb is only
//! the order of the three acts that make it safe:
//!
//! 1. **The jail, first** (`vike_sandbox::StrategyProcessPolicy::apply`): it refuses a process that holds a
//!    second thread or any descriptor above stderr, so it runs before anything opens a file or starts a
//!    thread (no log is initialised here at all: a log file would be a write channel past the jail). The
//!    artifact (its path under the builds directory, the builder's answer) is opened beneath that directory,
//!    as the backtest verb's are, so a link anywhere in it, a way out of the directory, or a missing or
//!    non-regular file refuses the jail by name (the crate doc's "The artifact grant"), and the mount does
//!    not open.
//! 2. **The protocol leaves fd 1.** A strategy file links its OWN copy of std, so a `println!` in a user's
//!    hook writes fd 1 whatever this process does with its own handle; on the protocol that line would be
//!    a garbage frame and a restart. The protocol gets a descriptor of its own (a copy of fd 1) and fd 1
//!    becomes a copy of stderr, the reason pipe the tradehub keeps the tail of.
//! 3. **Then the load** (`vike_rust_strategy::loader::load`), after the jail because a file can run code
//!    at load time; a load the loader refuses is answered to the tradehub's `Open` in the loader's words.
//!    At the `Open`, the file's own `refuse` is asked over the params first, then the strategy is created
//!    over the params and the mount FACTS (interface 9, as JSON) and refused when the file did not create it.
//!
//! Between acts 1 and 2 a stderr-only subscriber is installed (no log file: that would be a write channel
//! past the jail), so this process's own warnings reach the reason tail.
//!
//! `strategy-sandbox live-load <builds-dir> <artifact>` is the same process up to act 3, then exits: 0 when the
//! file loaded in the jail, 3 when the jail refused it, 4 when the loader did, each said on stderr. It serves
//! nothing and reads nothing from stdin. `deploy/docker/vikectl`'s `load_mounts` runs it on each artifact the
//! deploy's prebuild built, under the live unit's process properties, so a jail or a loader the new build
//! breaks refuses the deploy instead of the mount after the restart (2026-11-24: `openat2` answered `ENOSYS`
//! under `RestrictSUIDSGID=yes`, and no pre-flight had started a strategy process).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_rust_strategy::host::StrategyFileHost;
use vike_tradehub_core::remote_strategy::protocol::{Open, ViewBroker};
use vike_utility::errors::CarriedRefusal;

use crate::refusals::SandboxRefusal;

/// Run the live verb over `args` (the tail after `live`), stating `identity` as this process's build; with
/// `serve` false, the `live-load` verb (the module doc's last paragraph): the same acts up to the load, then exit.
pub(crate) fn run(args: &[String], identity: &str, serve: bool) -> ExitCode {
    let verb = if serve { "live" } else { "live-load" };
    let [builds, artifact] = args else {
        eprintln!(
            "strategy-sandbox {verb}: two arguments, the builds directory and the artifact under it"
        );
        return ExitCode::from(2);
    };
    let (builds, beneath) = (Path::new(builds), PathBuf::from(artifact));
    let policy = vike_sandbox::strategy_process_policy(builds, std::slice::from_ref(&beneath));
    if let Err(why) = policy.apply() {
        eprintln!("strategy-sandbox {verb}: {}", SandboxRefusal::Unjailed { why });
        return ExitCode::from(3);
    }
    let artifact = builds.join(beneath);
    // A stderr subscriber, AFTER the jail (it opens no file, starts no thread; the console layer only):
    // the child's own warnings (the loader's, the host wrappers', the serve loop's) reach the reason pipe,
    // and so do the strategy FILE's own `tracing` events, through the log slot the loader hands the file
    // (`vike_rust_strategy::host::host_log`, target `strategy_file`). The tradehub keeps the pipe's tail
    // for a crash's reason and logs each line under the mount's id.
    let _log = vike_log::init(vike_log::LogConfig {
        console_level: "info".to_string(),
        file_enabled: false,
        ..vike_log::LogConfig::default()
    });
    let output = match protocol_out() {
        Ok(output) => output,
        Err(e) => {
            eprintln!("strategy-sandbox {verb}: the protocol could not leave fd 1: {e}");
            return ExitCode::from(3);
        }
    };
    let loaded = vike_rust_strategy::loader::load(&artifact).map_err(|e| e.to_string());
    if !serve {
        return match loaded {
            Ok(_) => {
                eprintln!("strategy-sandbox {verb}: {} loaded in the jail", artifact.display());
                ExitCode::SUCCESS
            }
            Err(why) => {
                eprintln!("strategy-sandbox {verb}: {} did not load: {why}", artifact.display());
                ExitCode::from(4)
            }
        };
    }
    // A refusal is handed on with its id when it has one (this crate's `SandboxRefusal`), the loader's and the file's
    // own words bare: the mount's NOT RUNNING row shows the one id.
    let build = move |open: &Open| -> Result<_, CarriedRefusal> {
        let loaded = loaded?;
        // The file's own refusal of its params first, in its words: its build may panic on them.
        if let Some(why) = vike_rust_strategy::host::refusal(&loaded.vtable, &open.params) {
            return Err(why.into());
        }
        let facts = serde_json::to_string(&open.facts).map_err(|e| e.to_string())?;
        let host = StrategyFileHost::<ViewBroker>::new(loaded.vtable, &open.params, &facts);
        if !host.created() {
            return Err(SandboxRefusal::NotCreated { file: open.file.clone() }.carried());
        }
        let echo = host.echo();
        Ok((host, echo))
    };
    // The state crosses as the bytes the file encoded (msgpack, `ABI_VERSION` 15): nothing decodes it.
    match vike_tradehub_core::remote_strategy::serve(
        std::io::stdin().lock(),
        output,
        identity,
        build,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("strategy-sandbox live: {e}");
            ExitCode::from(4)
        }
    }
}

/// The protocol's own descriptor (a copy of fd 1), with fd 1 pointed at stderr: see the module doc, act 2.
#[cfg(target_os = "linux")]
fn protocol_out() -> std::io::Result<std::fs::File> {
    use std::os::fd::AsFd;
    let protocol = std::io::stdout().as_fd().try_clone_to_owned()?;
    rustix::stdio::dup2_stdout(std::io::stderr().as_fd())?;
    Ok(std::fs::File::from(protocol))
}

/// Off Linux the jail above has already refused; this is never reached.
#[cfg(not(target_os = "linux"))]
fn protocol_out() -> std::io::Result<std::io::Stdout> {
    Ok(std::io::stdout())
}
