//! The daemon's command line: [`Args`], the `clap` struct the argv is read into, and [`read_args`],
//! the one place it is read (decision 0155).
//!
//! `clap` reads the argv; `vike_util::cli::Program` decides what `clap` leaves to the program (the
//! one error path and exit code 2, `-h`/`--help` and `-V`/`--version` to stdout and exit 0, a blank
//! value refused, a typo refused with a suggestion, a repeated flag refused). What stays HERE, after
//! the parse, is the `--profile` removal message ([`PROFILE_FLAG_REMOVED`]).

use vike_util::cli::{CliExit, Program, USAGE_EXIT};

/// The name every message of this program carries: `vike-tradehub: <message>`, and the `Usage:` line.
const BIN: &str = "vike-tradehub";

/// The refusal a `--profile` argument earns (decision 0111): no profile FILE is read, so a unit
/// that still names one is stopped and told where the run profile lives now.
pub(super) const PROFILE_FLAG_REMOVED: &str = "--profile is REMOVED (decision 0111): the run \
     profile is the ACTIVE `run` row of the settings database, and no file is read. Write it with \
     `vike-cli config bootstrap-run <name> --risk.<key> <value> ...` (it is stored \
     and activated in one step), then drop --profile from the unit's ExecStart= line.";

// The parsed command line. Neither profile is named on argv any more: the DAEMON profile (the
// mount set) is the ACTIVE daemon-profile ROW since 0086, and the RUN profile (the operator's
// `[risk]` budget, `[guards]`, `[sinks]`) is the ACTIVE `run` ROW since decision 0111 — so a
// `--profile` argument is REFUSED by name ([`read_args`]), never accepted and ignored.
//
// ⚠ `config_path` is the RETIRED `--config` value, kept ONLY so [`run`] can warn that it is
// ignored — never read as a profile source. `vike-cli`'s `config bootstrap-daemon`
// (`crates/vike-cli/src/cmd/config/bootstrap_daemon.rs`) is the writer that replaces it on a box
// with no active daemon-profile row.
//
// ⚠ The doc comments on the FIELDS below are the `--help` text (that is what `clap` derives from
// them), so implementation notes sit in plain comments.
#[derive(clap::Parser, Debug)]
#[command(
    about = "The headless trading daemon: a strategy on a server with no GUI, driven by newline-JSON \
             commands on stdin, printing a periodic one-line JSON summary on stdout.",
    long_about = None,
    disable_help_subcommand = true
)]
pub(super) struct Args {
    /// RETIRED (0086): accepted for ONE release and IGNORED. The daemon profile comes from the
    /// ACTIVE daemon-profile ROW; `vike-cli config bootstrap-daemon` creates one on a box that has
    /// none.
    #[arg(long = "config", value_name = "PATH")]
    pub(super) config_path: Option<String>,
    /// Let the node server bind a wildcard or public address (a container, which must bind its
    /// namespace's wildcard). ORed with the flags.tradehub_allow_public_bind row; the startup line
    /// still names the exposed address.
    //
    // An argument and not a row because it is a fact about the deployment SHAPE
    // (`deploy/docker/entrypoint.sh` passes it: a container must bind its namespace's wildcard
    // for `-p` to reach it), not an operator preference stored in a database another deployment
    // of the same project may share (decision 0111).
    #[arg(long = "allow-public-bind")]
    pub(super) allow_public_bind: bool,
    // REMOVED (0111): declared only so that `--profile`, with or without a value, earns
    // [`PROFILE_FLAG_REMOVED`] from [`read_args`] instead of "unexpected argument". Hidden from
    // `--help`: the page does not advertise a flag it refuses.
    #[arg(long, hide = true, num_args = 0..=1, value_name = "PATH")]
    profile: Option<Option<String>>,
}

/// Read the daemon's command line (`argv[0]` ALREADY STRIPPED, by the shim and by the `vike-backend`
/// dispatcher alike).
///
/// ⚠ Takes the argv TAIL. An older wrapper here did its own `skip(1)`, which was correct for the
/// standalone shim and ate the first REAL argument through the dispatcher: the v0.1.16 image smoke
/// ran `vike-tradehub --version` through the symlink and was answered with the usage error.
///
/// # Errors
/// The [`CliExit`] to print and exit with: `-h`/`--help` and `-V`/`--version` (code 0, stdout),
/// every refusal (code 2, stderr).
pub(super) fn read_args(argv: &[String]) -> Result<Args, CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let program = Program::new(BIN, &version);
    let args: Args = program.try_parse_from(argv.iter().cloned())?;
    if args.profile.is_some() {
        let usage = program.command::<Args>().render_usage();
        return Err(CliExit {
            code: USAGE_EXIT,
            to_stdout: false,
            text: format!("{BIN}: {PROFILE_FLAG_REMOVED}\n\n{usage}\n"),
        });
    }
    Ok(args)
}
