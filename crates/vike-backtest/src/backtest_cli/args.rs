//! `backtest`'s small argv readers: `--addr`, flag presence, and the profile in either spelling.

use vike_analytics::binutil::{arg, has_flag};

use super::usage::SUBCOMMANDS;

/// What `--addr` asked for. Three states, because the flag takes an OPTIONAL value.
///
/// ⚠ **The FLAG is what says "become a daemon", never the absence of a profile** — the owner's
/// distinction when they refused `--serve`, `backtest serve`, `backtest listen` and
/// `backtest daemon` on 2026-09-10. `--addr` names a THING (the socket to bind), the way `--out`
/// names a file, and being given one is what makes this process stay alive. So a bare `backtest`
/// with no profile is still the old argument error, not an accidental daemon.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AddrFlag {
    /// No `--addr` at all — run one backtest and exit, exactly as before ruling 7.
    Absent,
    /// `--addr` with no value: serve on the CONFIGURED address. This is the normal use, and it is
    /// the whole point of the value being optional — the owner's *"MAY WE SET THIS SOMEWHERE AND
    /// NOT MENTION IT ALL THE TIME??"*.
    Configured,
    /// `--addr <host:port>` (or `--addr=<host:port>`): serve THERE, overriding every lower rung.
    Explicit(String),
}

/// Parse the OPTIONAL-VALUE `--addr` flag out of argv.
///
/// The rule for "did a value follow", spelled out because an optional-value flag is where CLIs
/// usually get this wrong: the next token is the VALUE only when it exists and does not begin with
/// `-`. So `backtest --addr` and `backtest --addr --json` both mean [`AddrFlag::Configured`], while
/// `backtest --addr 0.0.0.0:9999` means [`AddrFlag::Explicit`]. The `=` form is accepted too,
/// because an operator who writes `--addr=1.2.3.4:9` in a systemd `ExecStart=` should not discover
/// at runtime that this binary takes only the spaced form.
///
/// ⚠ A BLANK value is an ERROR rather than a fall-through to the configured rung. `--addr ""` in a
/// unit file is a mistake — an empty string cannot be a socket — and silently reading it as "the
/// configured address" would bind somewhere the operator did not name and never say so.
pub(super) fn parse_addr_flag(args: &[String]) -> Result<AddrFlag, String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(rest) = a.strip_prefix("--addr=") {
            if rest.trim().is_empty() {
                return Err("--addr= was given an empty value".to_string());
            }
            return Ok(AddrFlag::Explicit(rest.to_string()));
        }
        if a == "--addr" {
            return match args.get(i + 1) {
                Some(v) if !v.starts_with('-') => {
                    if v.trim().is_empty() {
                        Err("--addr was given an empty value".to_string())
                    } else {
                        Ok(AddrFlag::Explicit(v.clone()))
                    }
                }
                _ => Ok(AddrFlag::Configured),
            };
        }
    }
    Ok(AddrFlag::Absent)
}

/// Whether `flag` was WRITTEN at all, in either valued spelling.
///
/// ⚠ Neither half alone is enough, and both misses are live: `has_flag` is exact-token so it never
/// sees `--euler-depth=99`, and `arg` answers `None` for a TRAILING bare `--search` with no value
/// token after it. An ownership rule written with one of them leaks exactly the argv the other
/// catches.
///
/// Local to this file rather than a fifth parser in `crates/vike-analytics/src/binutil.rs`: that
/// module is a layer-20 home shared by six bins, and this is a one-file need.
///
/// ⚠ The adjacent-name landmine, checked: `flag_given(args, "--seed")` is NOT tripped by
/// `--seed-demo` — `has_flag` is exact-token and `arg` is anchored on `--seed=`. Same for
/// `--fetch`/`--fetch-starter`.
pub(super) fn flag_given(args: &[String], flag: &str) -> bool {
    has_flag(args, flag) || arg(args, flag).is_some()
}

/// The value-taking flags REACHABLE on the profile path, so a flag's VALUE is never counted as the
/// positional profile.
///
/// ⚠ Deliberately not every flag this file knows. `--kind`/`--venue`/`--symbol`/`--group`/
/// `--interval`/`--produced-by`/`--out`/`--from`/`--to`/`--days` all belong to [`run_data`]'s
/// subcommand, which has already RETURNED by the time [`profile_from_args`] runs — a `data` line
/// never reaches the profile path at all. `--addr` cannot appear at all either:
/// [`parse_addr_flag`] answers `AddrFlag::Absent` only when argv holds no `--addr`
/// token in EITHER spelling, so reaching the profile path proves there is none. That is what keeps
/// this table short and lets it carry no optional-value concept — the one thing a positional
/// scanner cannot express.
pub(super) const PROFILE_PATH_VALUED: &[&str] = &[
    "--profile",
    "--store",
    // ⚠ Missing until 2026-10-06, while `open_store` read it: `backtest sweep.toml --archive DIR`
    // read `DIR` as a second profile and was refused for giving the profile twice; only
    // `--archive=DIR` worked. The test
    // `every_flag_the_profile_path_reads_a_value_of_is_declared_to_the_positional_scanner` now
    // holds this table to the flags the profile-path files READ a value of, both ways.
    "--archive",
    "--rank-by",
    "--optimizer",
    "--euler-depth",
    "--trials",
    "--seed",
    // ⚠ Every one of these is VALUED, so every one must be here or its value is read as the
    // positional profile — `no_flag_value_can_be_mistaken_for_the_positional_profile` iterates this
    // table, so adding the row is what buys the coverage.
    //
    // ⚠ `--min-trades 50` is the argv that makes it concrete: without its row the scan sees `50` as
    // a bare token, calls it the profile, and `backtest sweep.toml --min-trades 50` is refused for
    // giving the profile TWICE — naming a file the operator never typed. `--progress json` is worse
    // still, because `json` looks like a filename.
    "--keep-trials",
    "--resume",
    "--min-trades",
    "--progress",
];

/// The profile, in either spelling: `backtest my.toml` (ruling 14) or `backtest --profile my.toml`.
///
/// ⚠ **`--profile` CANNOT retire, and that is a wire fact rather than a preference.**
/// `crates/vike-cli/src/cmd/backtest.rs`'s local arm builds `vec!["--profile".into(), …]` and
/// SPAWNS this binary, and `scripts/cli_mcp_smoke.sh` runs it. So the positional is ADDITIVE.
/// (It used to be TWO arms; `vike-cli sweep` was deleted by ruling 13 and its search flags moved
/// onto `vike-cli backtest`, which spawns the same way.)
///
/// ⚠ **BOTH is a REFUSAL, not a precedence rule.** Two spellings that may name two different files
/// is the same defect class as two searcher selectors: picking a winner silently answers a question
/// the operator did not know they had asked.
///
/// `Ok(None)` means "none given" — the caller's own arm, because that is the one case with no
/// specific mistake to name and therefore the one that still prints the usage text.
///
/// The scan skips a valued flag's VALUE by table rather than by "does the next token start with
/// `-`", because `--store dir` puts a bare token in argv that is emphatically not a profile.
pub(super) fn profile_from_args(args: &[String]) -> Result<Option<String>, String> {
    let flagged = arg(args, "--profile");
    let mut bare: Vec<&String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // A bare valued flag consumes the next token. An INLINE `--flag=v` consumes nothing, and is
        // skipped by the `starts_with('-')` arm below like any other flag token.
        if PROFILE_PATH_VALUED.contains(&a.as_str()) {
            let _ = it.next();
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        bare.push(a);
    }
    match (flagged, bare.as_slice()) {
        // ⚠ `data` IS a positional, so ruling 12's subcommand and ruling 14's profile collide on
        // exactly one word — and [`run`] resolves it by routing only when `data` comes FIRST. That
        // leaves this case: `backtest --optimizer tpe data rm …` reaches here with `data` as the
        // profile, and without this arm it loads a file called `data`, fails on the open and names
        // a path the operator never typed. Refused by name instead, saying where the word goes.
        //
        // The declared cost, per word: a profile literally NAMED `data` or `trials` (no extension)
        // is no longer reachable positionally. `--profile data` still reaches it, which is what the
        // arm says. TABLE-DRIVEN since stage 5, so a third subcommand joins by adding a row rather
        // than a fourth match arm.
        (None, [p]) if SUBCOMMANDS.iter().any(|(w, _)| *w == p.as_str()) => {
            let (word, shape) =
                SUBCOMMANDS.iter().find(|(w, _)| *w == p.as_str()).expect("just matched");
            Err(format!(
                "`{word}` is a SUBCOMMAND here, not a profile, and it must come FIRST — write \
                 `backtest {word} {shape} …`. If you really meant a profile file called `{word}`, \
                 spell it `--profile {word}`"
            ))
        }
        (Some(p), []) => Ok(Some(p)),
        (None, [p]) => Ok(Some((*p).clone())),
        (None, []) => Ok(None),
        (Some(p), [b]) => Err(format!(
            "the profile was given twice — --profile {p:?} and the positional {b:?}. They may name \
             two different files, so this is refused rather than resolved: give it once"
        )),
        (flagged, extra) => Err(format!(
            "more than one profile was given: {}{extra:?}. Give exactly one",
            flagged.map(|p| format!("--profile {p:?} and ")).unwrap_or_default()
        )),
    }
}
