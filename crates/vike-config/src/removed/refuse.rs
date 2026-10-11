//! `refuse_removed_env`: the startup refusal over `REMOVED_ENV`, and the helpers that render its message.

use std::collections::HashMap;

use super::{REMOVED_ENV, RemovedSetting, ValueMap};
use crate::write::SettingsSection;

/// Refuse to start when a REMOVED variable is set. `Ok(())` is the overwhelmingly common case.
///
/// "Set" means non-empty after trimming, EXCEPT for the rows that carry a
/// [`RemovedSetting::when_empty`]: an empty `POLY_SOCKS_PROXY` meant "connect direct", so it refuses
/// too (with a message about what the blank line used to do) — the module doc's "What counts as
/// set" argues why. Both the daemons' boots and the deploy pre-flight
/// (`vike-cli config retired-env`) call this one function, so they cannot disagree about WHICH
/// variables refuse — or about the message: the pre-flight's
/// `crates/vike-cli/src/cmd/config/retired_env.rs`'s `judge` hands it the value as the process
/// would receive it, padding included, so a `" 1"` gets `unacted_spelling`'s block from both. (It
/// used to trim first and print the ordinary line the boot withholds; that file's
/// `the_pre_flight_prints_exactly_what_the_daemons_boot_prints` holds the two equal over every row
/// and every spelling.) What the pre-flight can still miss is a value that never reaches it — the
/// deploy helper's own `Environment=` tokenization — which is the helper's, not this function's.
///
/// The `Err` string is the whole operator-facing message, ready to print: one block per offending
/// variable, each naming the variable, the key, and the exact line to write. Every
/// offender is reported in ONE pass — fixing a stale unit file one restart at a time is a worse
/// experience than being handed the full list.
///
/// `String` rather than [`crate::ConfigError`] on purpose: every `ConfigError` variant is shaped to
/// name a file that was read or a layer that failed, and this failure is neither — nothing was
/// read, and the thing to fix is the process environment.
pub fn refuse_removed_env(vars: &HashMap<String, String>) -> Result<(), String> {
    // `(row, the value as the process received it, that value trimmed)`.
    let offenders: Vec<(&RemovedSetting, &str, &str)> = REMOVED_ENV
        .iter()
        .filter_map(|r| {
            let full = vars.get(r.var)?.as_str();
            let raw = full.trim();
            // An empty value configured nothing — EXCEPT where the row says the old reader gave it a
            // meaning (`when_empty`): there, skipping it would change egress with no error.
            if raw.is_empty() && r.when_empty.is_none() {
                return None;
            }
            Some((r, full, raw))
        })
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }

    let mut out = String::new();
    for (r, full, raw) in &offenders {
        if !out.is_empty() {
            out.push('\n');
        }
        // A spelling an exact-match old reader never acted on has its own message: the process
        // ran the default, which is not what the ordinary line below would write.
        if let Some(block) = unacted_spelling(r, full, raw, vars) {
            out.push_str(&block);
            continue;
        }
        let (var, file) = (r.var, r.file);
        // The EMPTY-but-meaningful case has its own message: there is no value to echo or map, and
        // the point is what the blank line used to DO.
        if let (true, Some(empty), Some(key)) = (raw.is_empty(), r.when_empty, r.key) {
            let write = shell_word(empty.write);
            let line = match r.value {
                // A secret field takes its value on stdin only (`config set` refuses it on argv).
                ValueMap::Stdin => format!("printf {write} | vike-cli config set {key} -"),
                _ => format!("vike-cli config set {key} {write}"),
            };
            out.push_str(&format!(
                "{var} is set to an EMPTY value, but {var} is NO LONGER READ — removed in \
                 {removed_in}, because {why}.\n\
                 An empty value was NOT \"unset\" for this variable: its old reader took it to \
                 mean {meant}. Nothing reads it now, so this process would fall back to the \
                 built-in default instead — with no error anywhere. Say the same thing where the \
                 setting lives now:\n\n    {line}\n\nthen unset {var}.\n",
                removed_in = r.removed_in,
                why = r.why,
                meant = empty.meant,
            ));
            continue;
        }
        // …and the same case with NO key: the blank meant something, and nothing takes its place.
        if let (true, Some(empty), None) = (raw.is_empty(), r.when_empty, r.key) {
            out.push_str(&format!(
                "{var} is set to an EMPTY value, but {var} is NO LONGER READ — removed in \
                 {removed_in}, because {why}.\n\
                 An empty value was NOT \"unset\" for this variable: its old reader took it to \
                 mean {meant}. No setting takes its place, so there is nothing to write.\n\n\
                 then unset {var}.\n",
                removed_in = r.removed_in,
                why = r.why,
                meant = empty.meant,
            ));
            continue;
        }
        // The value is echoed only where the row says it may be — see `RemovedSetting::echo_value`.
        let setting = if r.echo_value { format!("{var}={raw}") } else { var.to_string() };
        out.push_str(&format!(
            "{setting} is set, but {var} is NO LONGER READ — removed in {removed_in}, \
             because {why}.\n",
            removed_in = r.removed_in,
            why = r.why,
        ));
        match r.key {
            Some(key) => {
                let (lead, value) = match r.value {
                    ValueMap::PositiveNumber => ("Set it instead", toml_value(raw)),
                    ValueMap::Verbatim => ("Set it instead", raw.to_string()),
                    ValueMap::Switch => (
                        "Set it instead",
                        (raw.split('#').next().unwrap_or("").trim() == "1").to_string(),
                    ),
                    ValueMap::Stdin => (
                        "Set it instead, with the value on stdin (never on the command line)",
                        "-".to_string(),
                    ),
                    // An exact spelling, or one whose first token is not `1`/`0`: the padded and
                    // commented `1`/`0` of an untrimmed reader never get here (`unacted_spelling`).
                    ValueMap::ExactOne | ValueMap::ExactOneUntrimmed => (
                        "Set it instead",
                        if first_token(raw) == "1" { "1" } else { "0" }.to_string(),
                    ),
                    ValueMap::TrueUnlessFalsey => (
                        "Set it instead",
                        (!matches!(
                            first_token(raw).to_ascii_lowercase().as_str(),
                            "false" | "0" | "no" | "off"
                        ))
                        .to_string(),
                    ),
                    ValueMap::FalseUnlessTruthy => (
                        "Set it instead",
                        matches!(
                            first_token(raw).to_ascii_lowercase().as_str(),
                            "1" | "true" | "yes" | "on"
                        )
                        .to_string(),
                    ),
                    ValueMap::FirstToken => ("Set it instead", shell_word(first_token(raw))),
                    ValueMap::List => ("Set it instead", shell_word(&list_value(raw))),
                    ValueMap::KillSwitchEach { lead, .. } => {
                        (lead, if first_token(raw) == "0" { "0" } else { "" }.to_string())
                    }
                };
                // A per-venue split names every key it became; every other row names its one, and
                // renders byte-identically to before the split existed.
                let also: &[&str] = match r.value {
                    ValueMap::KillSwitchEach { also, .. } => also,
                    _ => &[],
                };
                let keys = std::iter::once(key).chain(also.iter().copied());
                if value.is_empty() {
                    let named = keys.map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ");
                    out.push_str(&format!(
                        "Its value configured only the default, so there is nothing to write for \
                         {named}.\n\n"
                    ));
                } else {
                    out.push_str(&format!("{lead}:\n\n"));
                    for k in keys {
                        out.push_str(&format!("    vike-cli config set {k} {value}\n"));
                    }
                    out.push('\n');
                }
            }
            // A row with NO file and NO key has no settings home at all — the replacement is a flag,
            // a store row, or nothing (D4), and its `why` carries the whole answer.
            None if file.is_empty() => {}
            // A SETTINGS section (`file` is a section word): the key was deleted rather than moved,
            // so there is no row to name in its place — say so, naming the SECTION.
            None if SettingsSection::parse(file).is_some() => {
                let section = file;
                out.push_str(&format!(
                    "nothing in the settings database's `{section}` section reads it any more — \
                     the setting was deleted, not moved.\n\n"
                ));
            }
            // Any other home (the credential store's) renders its own line.
            None => {
                out.push_str(&format!(
                    "Credentials are read from {file} (<project>/settings/db/vike.db); nothing \
                     reads this variable.\n\n"
                ));
            }
        }
        out.push_str(&format!("then unset {var}.\n"));
    }
    Err(out)
}

/// The refusal for a spelling an exact-match old reader never acted on — `None` for every other
/// row and value, which keep the ordinary message.
///
/// [`ValueMap::ExactOneUntrimmed`] and [`ValueMap::KillSwitchEach`] rows had readers that compared
/// the UNTRIMMED value with `1`/`0`. So `" 1"`, `"1 # x"` or `"0 # x"` — a first token of `1`/`0`
/// that is not the whole value — ran the DEFAULT. The ordinary line would write what the operator
/// probably meant, which would change what the box does. This message shows the value as the
/// process received it, names the default that ran, makes writing nothing the way to keep it, and
/// prints the row only under "If you MEANT". For the master, a `1` means only "the defaults", so
/// it offers no row at all.
///
/// `vars` is the whole environment being judged. A venue whose OWN retired variable is also set
/// (`VIKE_MARK_STREAMS_ASTER` beside the master) did not run its default, so the master's block
/// names that variable for it instead of claiming a state.
fn unacted_spelling(
    r: &RemovedSetting,
    full: &str,
    raw: &str,
    vars: &HashMap<String, String>,
) -> Option<String> {
    let key = r.key?;
    let (also, meant): (&[&str], Option<&str>) = match (r.value, first_token(raw)) {
        (_, token) if full == token || !matches!(token, "0" | "1") => return None,
        (ValueMap::ExactOneUntrimmed, token) => (&[], Some(token)),
        (ValueMap::KillSwitchEach { also, .. }, token) => (also, (token == "0").then_some("0")),
        _ => return None,
    };
    let keys: Vec<&str> = std::iter::once(key).chain(also.iter().copied()).collect();
    // Another retired variable that writes this key and is set too: it decided that venue.
    let own_variable = |k: &str| {
        REMOVED_ENV
            .iter()
            .find(|o| {
                o.var != r.var
                    && o.key == Some(k)
                    && vars.get(o.var).is_some_and(|v| !v.trim().is_empty())
            })
            .map(|o| o.var)
    };
    let defaults = keys
        .iter()
        .map(|k| {
            let (venue, word) = declared_default(k);
            match (keys.len() > 1, own_variable(k)) {
                (true, Some(other)) => format!("{venue} per `{other}`"),
                (true, None) => format!("{venue} {word}"),
                (false, _) => word.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let var = r.var;
    // The value is echoed only where the row says it may be — see `RemovedSetting::echo_value`.
    let (setting, spelling) = if r.echo_value {
        (format!("{var}={full:?}"), format!("{full:?}"))
    } else {
        (var.to_string(), "that spelling".to_string())
    };
    let mut out = format!(
        "{setting} is set, but {var} is NO LONGER READ — removed in {removed_in}, because {why}.\n\
         Its old reader compared the value EXACTLY — no trimming, no `#` comment — so it did NOT \
         act on {spelling}: the process that ran took the default ({defaults}). To keep that, \
         write nothing.\n",
        removed_in = r.removed_in,
        why = r.why,
    );
    if let Some(meant) = meant {
        out.push_str(&format!("If you MEANT `{meant}`:\n\n"));
        for k in &keys {
            out.push_str(&format!("    vike-cli config set {k} {meant}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("then unset {var}.\n"));
    Some(out)
}

/// `(venue, "on" | "off")` for a `venue.<venue>.<field>` key, read from the venue-field catalog —
/// the default `config show` prints too, so the refusal cannot name a different one.
fn declared_default(key: &str) -> (&str, &'static str) {
    let (venue, field) =
        key.strip_prefix("venue.").and_then(|rest| rest.split_once('.')).unwrap_or((key, ""));
    let word = match vike_model::venues::venue_fields::venue_field(venue, field).map(|f| f.default)
    {
        Some("1") => "on",
        Some("0") => "off",
        _ => "its built-in default",
    };
    (venue, word)
}

/// Render the operator's own value into the suggested `config set` line when it is a value the key
/// would actually accept, else a placeholder.
///
/// Echoing garbage back (`max_notional_per_order = nope`) would hand over a line that fails the
/// loader with a *second*, unrelated error — and a non-positive number is rejected by
/// [`crate::Policy::apply`] as "a ceiling of 0 denies every order". Both cases get the placeholder
/// so the suggestion is always a line that works.
fn toml_value(raw: &str) -> String {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => raw.to_string(),
        _ => "<a positive number, in quote currency>".to_string(),
    }
}

/// The first whitespace-separated token before any `#` — how every Polymarket reader read its value.
fn first_token(raw: &str) -> &str {
    raw.split('#').next().unwrap_or("").split_whitespace().next().unwrap_or("")
}

/// A list value's tokens (commas and whitespace separate; `#` ends the list), joined by commas.
fn list_value(raw: &str) -> String {
    raw.split('#')
        .next()
        .unwrap_or("")
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// `value` as ONE shell word: unchanged when every character passes a shell untouched, single-quoted
/// otherwise — the printed line is meant to be pasted.
fn shell_word(value: &str) -> String {
    if value.chars().all(|c| c.is_ascii_alphanumeric() || "._-:/,@%+=".contains(c)) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}
