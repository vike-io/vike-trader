//! `vike-cli config retired-env` — read `KEY=VALUE` lines on stdin and print, as a daemon's boot
//! would, the startup refusal for any variable `vike_config::REMOVED_ENV` has retired.
//!
//! The deploy helper's judge (`deploy/sbin/vike-trader-ci-deploy`'s `retired_env_report`): before a
//! release is installed it feeds each roster unit's merged environment here, so a leftover
//! `Environment=` line fails the DEPLOY rather than the daemon (decision 0095, spec §3). Exit 0: none
//! set. Exit 6 (`Exit::Breach`): at least one, and the refusal is on stdout. An older release has no
//! such verb and answers 1, which the helper reads as "cannot check".

use std::collections::HashMap;
use std::io::BufRead;
use std::process::ExitCode;

use crate::exit::Exit;

const USAGE: &str = "\
usage: vike-cli config retired-env < environment

  Reads KEY=VALUE lines (a unit's Environment= list, an EnvironmentFile=, a /proc environ) and
  prints the startup refusal a daemon would give for every RETIRED variable among them.
  Exit 0: none. Exit 6: at least one (printed).";

/// Entry point. `args` is everything after `config retired-env`.
pub fn run(mut args: impl Iterator<Item = String>) -> ExitCode {
    if let Some(a) = args.next() {
        if matches!(a.as_str(), "-h" | "--help") {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        eprintln!("vike-cli config retired-env: unexpected argument `{a}`\n{USAGE}");
        return Exit::Usage.into();
    }
    match judge(&mut std::io::stdin().lock()) {
        None => ExitCode::SUCCESS,
        Some(refusal) => {
            println!("{refusal}");
            Exit::Breach.into()
        }
    }
}

/// PURE over the reader. Tolerates `export `, a leading quote (`systemctl show -p Environment`
/// quotes an assignment whose value has spaces) and quoted values.
///
/// ⚠ **The value reaches `vike_config::refuse_removed_env` AS THE DAEMON WOULD RECEIVE IT** — never
/// trimmed. That function is the one the daemons' boots call, and its message depends on the exact
/// spelling (`unacted_spelling`: a padded `" 1"` ran the DEFAULT, so the refusal says "write
/// nothing"). This judge used to `trim()` every value first, so for a padded value the pre-flight
/// printed the ordinary `config set … 1` line the daemon's own refusal withholds — and in the normal
/// upgrade flow the pre-flight is the one that refuses first.
fn judge(input: &mut dyn BufRead) -> Option<String> {
    let mut vars = HashMap::new();
    for line in input.lines().map_while(Result::ok) {
        if let Some((name, value)) = assignment(&line) {
            vars.insert(name.to_string(), value.to_string());
        }
    }
    vike_config::refuse_removed_env(&vars).err()
}

/// One `KEY=VALUE` line as the process it describes would see it: `(name, value)`, the value
/// verbatim except for the quoting that is syntax rather than content. `None` for a line with no
/// `=`.
///
/// Three shapes arrive on stdin and they quote differently:
///
/// * `systemctl show -p Environment` wraps a whole assignment whose value has spaces in ONE pair of
///   quotes (`"NAME=a b"`) — one leading quote, and its closing partner on the end.
/// * An `EnvironmentFile=` line may quote the value (`NAME="a b"`, `NAME='a b'`); systemd's syntax
///   strips exactly that one pair.
/// * `/proc/<pid>/environ` is already the received bytes, so an unquoted value is taken as it
///   stands — padding included, which is the point.
///
/// ⚠ **The residual, stated rather than hidden:** systemd also strips the whitespace around an
/// UNQUOTED `EnvironmentFile=` value, and this judge cannot tell such a line from an environ line,
/// which keeps it. The helper lists a running unit's environ LAST, so for a running unit the
/// received value wins; only a padded unquoted file line of a unit that is not running is judged by
/// its raw text. The deploy helper's own `Environment=` split (`tr ' ' '\n'` over
/// `systemctl show`) drops the second half of a value that contains a space, so such a value never
/// reaches this function at all — that half is the helper's to fix, not this verb's.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    let line = line.strip_prefix("export ").map_or(line, str::trim_start);
    let (wrapper, line) = match line.chars().next() {
        Some(quote @ ('"' | '\'')) => (Some(quote), &line[1..]),
        _ => (None, line),
    };
    let (name, value) = line.split_once('=')?;
    let value = match wrapper {
        Some(quote) => value.strip_suffix(quote).unwrap_or(value),
        None => unquote(value),
    };
    Some((name.trim(), value))
}

/// `value` without ONE wrapping pair of matching quotes, else unchanged.
fn unquote(value: &str) -> &str {
    ['"', '\'']
        .into_iter()
        .find_map(|q| value.strip_prefix(q).and_then(|rest| rest.strip_suffix(q)))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::judge;

    #[test]
    fn a_clean_environment_passes() {
        assert!(
            judge(&mut "PATH=/usr/bin\nVIKE_SETTINGS_DIR=/srv/x/settings\n".as_bytes()).is_none()
        );
    }

    #[test]
    fn a_retired_switch_is_refused_with_its_config_set_line() {
        let input = concat!("VIKE_LOG=info\n\"", "BYBIT", "_MAINNET=1\"\n");
        let refusal = judge(&mut input.as_bytes()).expect("refused");
        assert!(refusal.contains("vike-cli config set policy.venues.bybit live"), "{refusal}");
    }

    #[test]
    fn a_blank_retired_variable_is_not_a_belief_worth_refusing() {
        assert!(judge(&mut concat!("OKX", "_MAINNET=\n").as_bytes()).is_none());
    }

    /// …except the two whose blank value MEANT something: an `Environment=POLY_SOCKS_PROXY=` line in
    /// a unit told the old resolver to connect direct, so the deploy pre-flight (which is this
    /// judge) must catch it as well — a release that installed over it would start the daemon on
    /// the built-in SOCKS proxy without a word. `vike_config::refuse_removed_env` owns the rule;
    /// this pins that the verb the helper calls reaches it, quoted the way `systemctl show` quotes.
    #[test]
    fn a_blank_retired_variable_that_used_to_mean_direct_is_refused() {
        for line in [
            concat!("POLY", "_SOCKS_PROXY=\n"),
            concat!("\"POLY", "_SOCKS_PROXY=\"\n"),
            concat!("export POLY", "_SOCKS_PROXY=   \n"),
        ] {
            let refusal = judge(&mut line.as_bytes()).unwrap_or_else(|| panic!("{line:?} passed"));
            assert!(refusal.contains("EMPTY"), "{refusal}");
            assert!(refusal.contains("venue.polymarket.socks_proxy"), "{refusal}");
        }
        let ws = judge(&mut concat!("POLY", "_WS_PROXY_ENABLED=\n").as_bytes()).expect("refused");
        assert!(ws.contains("venue.polymarket.ws_proxy_enabled false"), "{ws}");
    }

    /// …and the auto-redeem kill switch, whose readers halted on PRESENCE: a blank line in a unit
    /// halted as `=1` did, so the pre-flight catches it too (decision 0095). It prints no row to
    /// write, because nothing starts the poller it halted.
    #[test]
    fn a_blank_retired_kill_switch_is_refused_with_nothing_to_write() {
        let refusal = judge(&mut concat!("POLY", "_REDEEM_HALT=\n").as_bytes())
            .expect("a blank halt refuses");
        assert!(refusal.contains("EMPTY"), "{refusal}");
        assert!(refusal.contains("nothing to write"), "{refusal}");
        assert!(!refusal.contains("vike-cli config set"), "{refusal}");
    }

    /// The spellings a unit can hand a daemon that differ only in padding, a trailing comment or
    /// emptiness. `1`/`0`/`yes` keep the grid honest at the edges: the rows that compare the value
    /// EXACTLY treat only the unpadded `1`/`0` as acted on.
    const SPELLINGS: [&str; 12] =
        ["1", "0", " 1", "1 ", " 1 ", "\t1", "1 # why", " 0 ", "0 # off", "yes", "  ", ""];

    /// **The pre-flight prints what the daemon's boot prints — for every retired variable and every
    /// way a unit can spell the same received value (decision 0095).** The judge used to `trim()`
    /// each value before calling `vike_config::refuse_removed_env`, so a value the PROCESS would
    /// receive as `" 1"` reached the function as `1`: the pre-flight printed the ordinary
    /// `config set … 1` line for a spelling whose old reader never acted on it, while the daemon's
    /// own refusal (which is handed the value as received) said "write nothing". In the normal
    /// upgrade flow the pre-flight refuses first, so the operator was handed the line that turns a
    /// toggle ON that the old build never had on.
    ///
    /// The oracle is the daemon's side of the SAME function over the value as received; the lines
    /// are the shapes the deploy helper hands the judge — a `/proc/<pid>/environ` line verbatim, an
    /// `export` line, `systemctl show -p Environment`'s quote-wrapped assignment, and an
    /// `EnvironmentFile=` line whose value is quoted (systemd's syntax strips the quotes).
    #[test]
    fn the_pre_flight_prints_exactly_what_the_daemons_boot_prints() {
        for row in vike_config::REMOVED_ENV {
            let var = row.var;
            for value in SPELLINGS {
                let daemon = vike_config::refuse_removed_env(&HashMap::from([(
                    var.to_string(),
                    value.to_string(),
                )]))
                .err();
                for (shape, line) in [
                    ("a /proc environ line", format!("{var}={value}\n")),
                    ("an export line", format!("export {var}={value}\n")),
                    ("a quote-wrapped assignment", format!("\"{var}={value}\"\n")),
                    ("a double-quoted file value", format!("{var}=\"{value}\"\n")),
                    ("a single-quoted file value", format!("{var}='{value}'\n")),
                ] {
                    assert_eq!(
                        judge(&mut line.as_bytes()),
                        daemon,
                        "{var} received as {value:?}, handed to the judge as {shape}"
                    );
                }
            }
        }
    }

    /// The concrete case the grid above generalises: a padded value of a row whose old reader
    /// compared EXACTLY gets the "write nothing" refusal from the pre-flight, not the ordinary
    /// line that would turn the toggle on.
    #[test]
    fn a_padded_value_gets_the_write_nothing_refusal_not_the_ordinary_line() {
        let row = vike_config::REMOVED_ENV
            .iter()
            .find(|r| matches!(r.value, vike_config::ValueMap::ExactOneUntrimmed))
            .expect("the table has an exact-match row");
        let refusal =
            judge(&mut format!("{}= 1\n", row.var).as_bytes()).expect("a padded value refuses");
        assert!(refusal.contains("compared the value EXACTLY"), "{refusal}");
        assert!(refusal.contains("write nothing"), "{refusal}");
        assert!(!refusal.contains("Set it instead"), "{refusal}");
    }

    /// The kill switch's path override (decision 0099): a unit that still carries
    /// `Environment=VIKE_HALT_FILE=…` fails the DEPLOY, not the daemon — and the refusal names the
    /// one file to `touch`, because an installed unit holding the old line is exactly the operator
    /// who is about to `touch` the path they wrote there.
    #[test]
    fn a_unit_that_still_sets_the_halt_file_override_is_refused_naming_the_sentinel() {
        for line in [
            concat!("VIKE", "_HALT_FILE=/srv/vike-<unit>/settings/state/HALT\n"),
            concat!("\"VIKE", "_HALT_FILE=/srv/vike-<unit>/settings/state/HALT\"\n"),
        ] {
            let refusal = judge(&mut line.as_bytes()).unwrap_or_else(|| panic!("{line:?} passed"));
            assert!(refusal.contains("NO LONGER READ"), "{refusal}");
            assert!(refusal.contains("<project>/settings/state/HALT"), "{refusal}");
        }
        // A blank line was never an override (the old resolver fell through on it): it passes.
        assert!(judge(&mut concat!("VIKE", "_HALT_FILE=\n").as_bytes()).is_none());
    }
}
