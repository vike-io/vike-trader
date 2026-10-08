//! The daemon's command line: `USAGE`, the parsed `Args`/`Parsed` shapes and the argv parser.

pub(super) const USAGE: &str = "\
usage: vike-tradehub [--profile <run_profile.toml>]

  --config PATH   RETIRED (0086): accepted for ONE release and IGNORED. The daemon profile comes
                  from the ACTIVE daemon-profile ROW; `vike-cli config bootstrap-daemon` creates
                  one on a box that has none.
  --profile PATH  the operator-budget RunProfile TOML ([risk] ceilings); absent, $VIKE_RUN_PROFILE
  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0";

/// The parsed command line. `profile_path` is the OPERATOR-BUDGET `RunProfile` (RunProfile wiring,
/// Settings STEP 2 PR 1, Task 2) — independent of the DAEMON profile (venue/token_id/A-S mount
/// shape), which since 0086 comes from the ACTIVE daemon-profile ROW alone and is no longer named on
/// argv at all. `None` here (no `--profile` flag) falls through to
/// [`vike_core::resolve_profile`]'s `VIKE_RUN_PROFILE` env fallback; both absent ⇒ `Ok(None)` ⇒ the
/// daemon's risk budget is byte-identical to today.
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
    pub(super) profile_path: Option<String>,
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
/// `--profile`/`--profile=value` is OPTIONAL — the RunProfile risk-budget file (Task 2); absent, the
/// daemon still checks `VIKE_RUN_PROFILE` ([`vike_core::resolve_profile`]'s env fallback), and absent
/// BOTH the risk budget is untouched (today's behavior).
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
/// an empty or whitespace path to either flag: `--config` names the profile TOML that decides the
/// venue, the token and the mount shape, and `--profile` names a `RunProfile` risk-budget file
/// whose ABSENCE is already spelled by omitting the flag (it then falls through to
/// `VIKE_RUN_PROFILE`). So both spellings of both flags now refuse it.
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
/// Both spellings of both flags resolve through [`flag_value`], which is what stops them
/// disagreeing: `--config`, `--config=` and `--config ""` are now the same refusal.
pub(super) fn parse_args_from(mut it: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut config_path: Option<String> = None;
    let mut profile_path: Option<String> = None;
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
            "--profile" => {
                profile_path = Some(flag_value("--profile", inline.or_else(|| it.next()))?);
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    // ⚠ NO LONGER REQUIRED (0086): the daemon profile comes from the active row, and `--config` is
    // a retired argument kept only for the one-release warning `run` emits when it is given. See
    // `Args`'s own doc on `config_path`.
    Ok(Parsed::Args(Args { config_path, profile_path }))
}
