//! `strategy-sandbox live <artifact>`: serve ONE live mount's hooks to the tradehub that spawned this
//! process (plan 2b §4, unit F5). The protocol and the serve loop are the tradehub's
//! (`vike_tradehub_core::remote_strategy`, whose module doc carries the failure table); this verb is only
//! the order of the three acts that make it safe:
//!
//! 1. **The jail, first** (`vike_sandbox::StrategyProcessPolicy::apply`): it refuses a process that holds a
//!    second thread or any descriptor above stderr, so it runs before anything opens a file or starts a
//!    thread (no log is initialised here at all: a log file would be a write channel past the jail).
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

use std::path::Path;
use std::process::ExitCode;

use vike_rust_strategy::host::StrategyFileHost;
use vike_tradehub_core::remote_strategy::protocol::ViewBroker;

/// Run the live verb over `args` (the tail after `live`).
pub(crate) fn run(args: &[String]) -> ExitCode {
    let [artifact] = args else {
        eprintln!("strategy-sandbox live: one argument, the artifact");
        return ExitCode::from(2);
    };
    let artifact = Path::new(artifact);
    if let Err(why) = vike_sandbox::strategy_process_policy(artifact).apply() {
        eprintln!("strategy-sandbox live: no jail, so no strategy: {why}");
        return ExitCode::from(3);
    }
    // A stderr subscriber, AFTER the jail (it opens no file, starts no thread; the console layer only):
    // the child's own warnings (the loader's, the host wrappers', the serve loop's) reach the reason tail
    // the tradehub keeps. ⚠ A strategy FILE's own `tracing` calls do NOT reach it: the file links its own
    // copy of the dispatcher's process-global (`crates/vike-rust-strategy/template/Cargo.toml.in`'s note).
    let _log = vike_log::init(vike_log::LogConfig {
        console_level: "info".to_string(),
        file_enabled: false,
        ..vike_log::LogConfig::default()
    });
    let output = match protocol_out() {
        Ok(output) => output,
        Err(e) => {
            eprintln!("strategy-sandbox live: the protocol could not leave fd 1: {e}");
            return ExitCode::from(3);
        }
    };
    let loaded = vike_rust_strategy::loader::load(artifact).map_err(|e| e.to_string());
    let build = move |open: &vike_tradehub_core::remote_strategy::protocol::Open| {
        let loaded = loaded?;
        // The file's own refusal of its params first, in its words: its build may panic on them.
        if let Some(why) = vike_rust_strategy::host::refusal(&loaded.vtable, &open.params) {
            return Err(why);
        }
        let facts = serde_json::to_string(&open.facts).map_err(|e| e.to_string())?;
        let host = StrategyFileHost::<ViewBroker>::new(loaded.vtable, &open.params, &facts);
        if !host.created() {
            return Err(format!(
                "`{}` did not build from its params and mount facts (a params document that does \
                 not parse, a build that panicked, or a rule file handed no symbol)",
                open.file
            ));
        }
        let echo = host.echo();
        Ok((host, echo))
    };
    match vike_tradehub_core::remote_strategy::serve(std::io::stdin().lock(), output, build) {
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
