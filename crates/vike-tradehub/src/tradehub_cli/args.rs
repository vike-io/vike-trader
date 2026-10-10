//! The daemon's command line: `USAGE`, the parsed `Args`/`Parsed` shapes and the argv parser.

pub(super) const USAGE: &str = "\
usage: vike-tradehub

  --config PATH   RETIRED (0086): accepted for ONE release and IGNORED. The daemon profile comes
                  from the ACTIVE daemon-profile ROW; `vike-cli config bootstrap-daemon` creates
                  one on a box that has none.
  --allow-public-bind
                  let the node server bind a wildcard or public address (a container, which must
                  bind its namespace's wildcard). ORed with the flags.tradehub_allow_public_bind
                  row; the startup line still names the exposed address.
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

/// The refusal a `--profile` argument earns (decision 0111): no profile FILE is read, so a unit
/// that still names one is stopped and told where the run profile lives now.
pub(super) const PROFILE_FLAG_REMOVED: &str = "--profile is REMOVED (decision 0111): the run \
     profile is the ACTIVE `run` row of the settings database, and no file is read. Write it with \
     `vike-cli config bootstrap-run <name> --mode <mode> --risk.<key> <value> ...` (it is stored \
     and activated in one step), then drop --profile from the unit's ExecStart= line.";

/// The parsed command line. Neither profile is named on argv any more: the DAEMON profile (the
/// mount set) is the ACTIVE daemon-profile ROW since 0086, and the RUN profile (the operator's
/// `[risk]` budget, `[guards]`, `[sinks]`) is the ACTIVE `run` ROW since decision 0111 — so a
/// `--profile` argument is REFUSED by name ([`parse_args_from`]), never accepted and ignored.
///
/// ⚠ `config_path` is the RETIRED `--config` value, kept ONLY so [`run`] can warn that it is
/// ignored — never read as a profile source. See [`USAGE`]'s own line on the flag; `vike-cli`'s
/// `config bootstrap-daemon` (`crates/vike-cli/src/cmd/config/bootstrap_daemon.rs`) is the writer
/// that replaces it on a box with no active daemon-profile row.
///
/// `Debug` on both this and [`Parsed`] so a parse that was supposed to FAIL can report what it
/// produced instead (`Result::expect_err` requires it) — the same reason
/// `vike_backfill::cli::Parsed` derives it.
#[derive(Debug)]
pub(super) struct Args {
    pub(super) config_path: Option<String>,
    /// `--allow-public-bind`: the DEPLOYMENT's consent to a wildcard/public node-server bind,
    /// ORed with the `flags.tradehub_allow_public_bind` row. An argument and not a row because it
    /// is a fact about the deployment SHAPE (`deploy/docker/entrypoint.sh` passes it: a container
    /// must bind its namespace's wildcard for `-p` to reach it), not an operator preference stored
    /// in a database another deployment of the same project may share (decision 0111).
    pub(super) allow_public_bind: bool,
}

/// What a successful parse produced. `-h`/`--help` is NOT an error — there is nothing to run, but
/// nothing went wrong either, and it used to be spelled as `Err("help requested")`, which the
/// caller then printed to stderr and exited **1** for. A non-zero `--help` breaks `set -e`,
/// packaging smoke tests and every wrapper that checks a status, and the token itself is internal
/// control flow that reads to a user like an escaped error string. A variant, not a magic string,
/// makes the two outcomes impossible to confuse again.
#[derive(Debug)]
pub(super) enum Parsed {
    Args(Args),
    Help,
    /// `--version`/`-V`: the name and version on stdout, exit 0. It was UNRECOGNISED, so it fell
    /// into the `unknown argument` arm and exited **1** on stderr — the same defect class as a
    /// failing `--help`, and the first thing a packaging script or a bug report asks a binary.
    /// `-V`, never `-v`: lowercase `-v` is verbosity everywhere else on the box.
    Version,
}

/// Hand-rolled tiny arg parser (no `clap` — PR-9 adds no dependency). Accepts `--config value` and
/// `--config=value`; ⚠ `--config` is RETIRED (0086) and OPTIONAL — accepted for one release and
/// warned about, never required and never read as a profile source (see `Args`'s own doc on
/// `config_path`). `-h`/`--help` short-circuits to [`Parsed::Help`] and `-V`/`--version` to
/// [`Parsed::Version`], both of which are a SUCCESS (see those variants' docs, and `main`'s arms for
/// why stdout is right here).
/// `--profile` is REFUSED BY NAME (decision 0111): the run profile is the ACTIVE `run` row, and an
/// operator whose unit still names a file must be told the file stopped deciding anything rather
/// than have it ignored — it is the ceiling file a live mount's orders are judged against.
///
/// ⚠ Takes the argv TAIL — `argv[0]` is ALREADY STRIPPED, by the shim and by the `vike-backend`
/// dispatcher alike. The old wrapper here did its own `skip(1)`, which was correct for the
/// standalone shim (full process argv) and ate the first REAL argument through the dispatcher: the
/// v0.1.16 image smoke ran `vike-tradehub --version` through the symlink and was answered with the
/// usage error.
pub(super) fn parse_args(argv: &[String]) -> Result<Parsed, String> {
    parse_args_from(argv.iter().cloned())
}

/// **THE RULE, and this daemon's one addition to it.** A valued flag must be GIVEN a value: a
/// token beginning with `--` is a FLAG and never a value, and a value that is BLANK is no value at
/// all. The same rule `vike_backfill::cli::flag_value` spells for the backfill bins; this crate
/// cannot depend on that one (nothing may — it pulls every bridge crate), so the spelling is
/// repeated rather than shared.
///
/// It is `--`, not a bare `-`: a negative number is a real value in this workspace's parsers, so a
/// `starts_with('-')` test would turn every signed field into a usage error. Neither flag here
/// takes a number, but the predicate is the workspace's and does not fork per binary. A path that
/// genuinely begins with `--` is reachable as `./--weird.toml`.
///
/// **The BLANK clause is this daemon's, and it is what makes the two spellings of one flag agree.**
/// `--config=` used to be ACCEPTED with an empty config path — the inline branch took
/// `split_once`'s right half verbatim and no arm checked it — while `--config` with no value was
/// refused. A `systemd` `ExecStart` interpolating an unset shell variable produces exactly the
/// accepting form (`--config=$VIKE_CONFIG` with the variable unset, or `--config "$VIKE_CONFIG"`
/// with quotes), so the daemon started, satisfied its own required-flag check, and then failed
/// opening `""` — a file-open error instead of "you gave no --config". Nothing legitimately passes
/// an empty or whitespace path to it, so both spellings now refuse it.
fn flag_value(flag: &str, value: Option<String>) -> Result<String, String> {
    match value {
        None => Err(format!("{flag} requires a value")),
        Some(v) if v.starts_with("--") => {
            Err(format!("{flag} requires a value, but the next argument is another flag ({v})"))
        }
        Some(v) if v.trim().is_empty() => Err(format!(
            "{flag} requires a non-empty value (an unset shell variable in a systemd ExecStart= \
             line produces exactly `{flag}=`)"
        )),
        Some(v) => Ok(v),
    }
}

/// The whole of the daemon's argument surface, over an already-`argv[0]`-stripped stream. PURE — no
/// environment, no filesystem — so the LIVE daemon's command line is drivable from a test without a
/// process. [`parse_args`] is the thin process-argv wrapper; the SOURCE of argv is the only thing it
/// decides.
///
/// Both spellings of the valued flag resolve through [`flag_value`], which is what stops them
/// disagreeing: `--config`, `--config=` and `--config ""` are now the same refusal.
pub(super) fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut config_path: Option<String> = None;
    let mut allow_public_bind = false;
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        match flag.as_str() {
            // `inline.or_else(|| it.next())` rather than a match: the INLINE half is `Some("")` for
            // `--config=`, which must reach `flag_value`'s blank clause rather than falling through
            // to the next argument as if no value had been written at all.
            "--config" => {
                config_path = Some(flag_value("--config", inline.or_else(|| it.next()))?);
            }
            "--profile" => return Err(PROFILE_FLAG_REMOVED.to_string()),
            // A switch: a value (`--allow-public-bind=yes`) is refused rather than read.
            "--allow-public-bind" if inline.is_none() => allow_public_bind = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // ⚠ NO LONGER REQUIRED (0086): the daemon profile comes from the active row, and `--config` is
    // a retired argument kept only for the one-release warning `run` emits when it is given. See
    // `Args`'s own doc on `config_path`.
    Ok(Parsed::Args(Args { config_path, allow_public_bind }))
}
