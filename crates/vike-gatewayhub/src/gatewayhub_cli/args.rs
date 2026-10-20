//! The command line: `clap`'s struct, the parse, and `--help` (decision 0155).
//!
//! The daemon takes NO flags. Everything it can be told is a settings row (decision 0086), so the
//! only questions a command line can ask of it are `--help` and `--version`, and a typo in a unit's
//! `ExecStart=` must be refused rather than ignored: `vike_util::cli::Program` does the refusing
//! (an unknown flag with a suggestion, a stray positional, exit code 2) and answers `-h`/`--help`
//! and `-V`/`--version` on stdout with exit 0, before anything else runs.

use vike_util::cli::{CliExit, Program};

/// The name every message of this program carries: `vike-gatewayhub: <message>`, and the `Usage:`
/// line.
const BIN: &str = "vike-gatewayhub";

/// What `--help` prints after the (empty) flag list: the settings rows and the credential store
/// this daemon reads, none of which is on the command line.
const SETTINGS_HELP: &str = "\
settings (rows of the settings database: `vike-cli config set <key> <value>`, then restart; no
environment variable reaches any of them, and a retired one refuses startup naming its row):
  config.gatewayhub_addr
                       the address this daemon binds, and the one the data daemon dials (default
                       127.0.0.1:7883). ⚠ A NON-LOOPBACK address is REFUSED at startup, outright:
                       there is no opt-in flag and no key that lifts it. The daemon holds every
                       venue connection and its handshake is plaintext, so it serves this host
                       only; reach it from elsewhere through an SSH tunnel.
  preferences.log_file_level
                       the level of the rolling trace file under <project>/settings/state/logs
                       (prefix vike-gatewayhub).

what it does:
  It boots, declares the venue settings rows it was given (Polymarket's egress among them),
  applies the loopback bind guard, requires its hub keys (no keys, no start: it holds every venue
  connection, so it has no unauthenticated mode), binds the address and serves the venue lanes to
  its clients until it is stopped. SIGTERM and SIGINT are handled from the first moment: a stop
  drains the open connections within a few seconds and exits 0. A build without the `data`
  feature links no venue bridge, so it has nothing to serve and exits 2 saying so.";

// The command line, as `clap` reads it: no flags beyond the `--help`/`--version` that
// `vike_util::cli::Program` adds to every program. The doc comment of the struct is NOT the help
// text here (`about` below is), because a derive takes the struct's doc and this page wants it to
// say what the daemon is for.
#[derive(clap::Parser, Debug)]
#[command(
    about = "The gateway daemon: the one process that owns every venue connection. The datahub, \
             and in a later wave the tradehub, reach a venue only through it. It takes no flags; \
             its address is a settings row (below).",
    long_about = None,
    after_help = SETTINGS_HELP,
    disable_help_subcommand = true
)]
pub(super) struct Cli {}

/// Read the command line (`argv[0]` already stripped).
///
/// # Errors
/// The [`CliExit`] to print and exit with: `-h`/`--help` and `-V`/`--version` (code 0, stdout),
/// every refusal (code 2, stderr).
pub(super) fn read_args(argv: impl IntoIterator<Item = String>) -> Result<(), CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let program = Program::new(BIN, &version);
    let Cli {} = program.try_parse_from(argv)?;
    Ok(())
}

/// Answer `--help`, `--version` and a usage error, and return only when the command line is a
/// plain start. A command line that does not parse NEVER starts the daemon: the answer is printed
/// and the process exits.
///
/// Help is normal output a user pipes into a pager, so STDOUT and exit 0 (a non-zero `--help`
/// breaks `set -e` and every packaging smoke test); `--version` prints
/// `<name> <version> (<build identity>)`, the string the daemon's first log line carries.
pub(super) fn short_circuit(args: &[String]) {
    if let Err(exit) = read_args(args.iter().cloned()) {
        exit.emit();
    }
}
