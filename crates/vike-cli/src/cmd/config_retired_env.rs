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
fn judge(input: &mut dyn BufRead) -> Option<String> {
    let mut vars = HashMap::new();
    for line in input.lines().map_while(Result::ok) {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start_matches(['"', '\'']);
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_end_matches(['"', '\'']);
            vars.insert(k.trim().to_string(), v.to_string());
        }
    }
    vike_config::refuse_removed_env(&vars).err()
}

#[cfg(test)]
mod tests {
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
}
