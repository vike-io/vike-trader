//! **What this process actually resolved, rendered for the log at startup.**
//!
//! [`crate::provenance`] has computed every setting's effective value and its ORIGIN since Phase 0,
//! and until now the only consumer was `vike-cli config show` — a command somebody has to think to
//! run, on the box, after they already suspect something. A daemon disclosed none of it.
//!
//! # The incident
//!
//! On the CI box the project-root walk answered with an unrelated directory. The daemon loaded no
//! policy, no config and NO CREDENTIALS, so every venue silently dropped to paper — with no error,
//! because "there are no settings here" and "the settings here say nothing" are indistinguishable
//! downstream. `crates/vike-model/src/state_path.rs`'s `project_settings_dir` carries the whole
//! history of that walk and three separate bugs in its precedence rule. Every one of them would
//! have been a one-line read at startup: the directory that answered, whether each file was there,
//! and whether a credential store was found beside them.
//!
//! # What is printed, and what is deliberately not
//!
//! [`boot_lines`] renders, in order: the settings DIRECTORY, one line per settings FILE (present or
//! absent, with its key count), the resolved SETTINGS, the CREDENTIAL STORE's presence, and a
//! one-line tally.
//!
//! The settings half is not every key. It is **every `policy.*` row, always** — they are the risk
//! ceilings, they are file-only by construction, and a daemon that signs real orders should state
//! them whether or not anything set them — **plus every row some layer actually configured**. The
//! rest are named by count with a pointer to `vike-cli config show`, which prints all of them. A
//! full dump is ~45 lines on every boot, and a startup banner nobody reads is worth as little as no
//! banner at all.
//!
//! ⚠ **The credential store is reported by PRESENCE, never by contents.** This code never opens the
//! file: `std::fs` is asked whether the path exists, and `vike_secrets::permission_warning` /
//! `vike_secrets::legacy_store_warning` are both stat-only probes (that is why they are `pub`). So
//! no key NAME and no key VALUE can reach a log line from here, whatever the store holds — a
//! property [`crate::redact`] cannot give you, because it only governs text you have already
//! decided to print. Key names are `vike-cli secrets list`'s job, and the credentials themselves
//! are opened by each root on the path that needs them, where
//! `vike_bridge_core::credentials::try_load_workspace_secrets_at` already logs both findings.
//! (It logs them only when a root gets that far — a paper daemon never does, which is exactly the
//! case where this line is the only signal, and why re-reporting a finding is worth the duplicate.)
//!
//! ⚠ **A settings VALUE is redacted by name shape** ([`crate::redact::is_secret_key`]). No settings
//! field is credential-shaped today and `crates/vike-config/tests/boot.rs`'s
//! `the_redaction_shapes_cover_a_credential_shaped_settings_key` asserts that emptiness rather than
//! assuming it — but this renderer writes into a log FILE that gets shipped to bug reports, so
//! "remember to redact the day somebody adds a `bot_token` field" is not a mechanism.
//!
//! # Where the caller puts it
//!
//! **After `vike_log::init`, and only after** — the log directory and both log levels are
//! themselves settings, so a subscriber built first could only ever honour the environment. That
//! ordering is why the loader returns its warnings as DATA rather than logging them; this module
//! keeps the same discipline and returns LINES, so a binary whose stdout is a protocol (`vike-cli
//! mcp`, `vike-tradehub`) decides where they go.
//!
//! # Failure is a line, never a refusal
//!
//! A settings directory holding a broken `policy.toml` makes [`crate::load`] fail, and the binaries
//! that load settings for real already refuse to start on that. This is a DISCLOSURE, so it reports
//! the failure and returns — a daemon must not acquire a new fatal error from the code that
//! describes it. That matters for `vike-recorder`, where this is the first loader in the process.

use std::collections::HashMap;
use std::path::Path;

use crate::provenance::{Description, Origin, ResolvedSetting, describe};
// The redaction decision AND the two words it prints. Both come from [`crate::redact`]: this is the
// second disclosure surface, and the one thing a second surface must not do is spell either of them
// for itself — see that module's doc.
use crate::redact::{SET, UNSET, is_secret_key};
use crate::venue_mode::VenueMode;

/// The section whose rows are ALWAYS printed, set or not. See the module doc.
const ALWAYS: &str = "policy.";

/// The one policy sub-table rendered as a SINGLE aggregated line ([`venues_line`]) instead of one
/// row each.
///
/// ⚠ [`ALWAYS`] means every `policy.*` row prints even at its default, and `policy.venues.*` is one
/// row per ROSTER VENUE — so without this the block would grow by a line per venue on EVERY start
/// of EVERY binary, to say "paper" fourteen times. A startup banner nobody reads is worth as little
/// as no banner at all (this module's doc), and burying the four risk ceilings under fourteen
/// default venue rows is exactly how a banner stops being read.
///
/// The aggregate is not a shortening: it is the better rendering. "which venues is this box armed
/// for" is one question, and a per-row dump makes the reader answer it by scanning.
const VENUES_PREFIX: &str = "policy.venues.";

/// **The startup disclosure, as log-ready lines.** One `tracing::info!` per line at the caller.
///
/// `settings_dir` is `<project>/settings` as the caller's own walk resolved it (`None` when no
/// project sits above the working directory), and `env` is the single `std::env::vars()` sweep the
/// composition root already owns — this crate reads no environment, per the settings-registry rule.
///
/// Never fails and never panics: see the module doc's last section.
pub fn boot_lines(settings_dir: Option<&Path>, env: &HashMap<String, String>) -> Vec<String> {
    let mut lines = Vec::new();

    match settings_dir {
        Some(dir) => lines.push(format!("settings dir: {}", dir.display())),
        // The the CI box shape, named in full: this ONE line is what the incident cost was for.
        None => lines.push(
            "settings dir: NONE — no project marker above the working directory, so every setting \
             below is a compiled-in default and NO credential store was found (every venue stays \
             paper). Name it outright with VIKE_SETTINGS_DIR."
                .to_string(),
        ),
    }

    match describe(settings_dir, env) {
        Ok(description) => {
            for file in &description.files {
                lines.push(match file.present {
                    true => format!("settings file: {} present, {} keys", file.name, file.keys),
                    false => {
                        format!("settings file: {} ABSENT ({})", file.name, file.path.display())
                    }
                });
            }

            // The per-venue ceilings, as ONE line, before the per-row loop — it leads the settings
            // block because "which venues is this box armed for" is the first thing an operator
            // reading a trading daemon's banner wants, and because the loop below skips its rows.
            lines.push(venues_line(&description));

            let mut defaulted = 0usize;
            let mut configured = 0usize;
            for row in &description.rows {
                let is_default = row.origin == Origin::Default;
                if is_default {
                    defaulted += 1;
                } else {
                    configured += 1;
                }
                // COUNTED above, never printed here: they are rendered by `venues_line`. The tally
                // still includes them, because `vike-cli config show` — which the tally points at —
                // prints every one of them individually.
                if row.key.starts_with(VENUES_PREFIX) {
                    continue;
                }
                if is_default && !row.key.starts_with(ALWAYS) {
                    continue;
                }
                lines.push(setting_line(row));
            }
            lines.push(format!(
                "settings: {configured} set by a file or the environment, {defaulted} at their \
                 compiled-in default (`vike-cli config show` prints every one)"
            ));

            // The loader's own non-fatal resolutions. The caller may already surface these; a
            // duplicate line costs nothing next to a resolution that reaches nobody.
            for warning in &description.settings.warnings {
                lines.push(format!("settings: {warning}"));
            }
        }
        Err(e) => lines.push(format!(
            "settings: COULD NOT BE LOADED: {e} — nothing here is in force; this process is \
             running on compiled-in defaults"
        )),
    }

    lines.push(credential_store_line(settings_dir));
    lines.extend(credential_store_findings(settings_dir));
    lines
}

/// One resolved setting: `setting: policy.max_leverage = 1.0 [policy.toml]`.
///
/// The value is REDACTED by name shape before it is formatted, not while it is printed — the same
/// placement `vike-cli config show` uses, so there is no second rendering path that could forget.
fn setting_line(row: &ResolvedSetting) -> String {
    let value = match (is_secret_key(&row.key), row.value.as_deref()) {
        (true, Some(v)) if !v.is_empty() => SET.to_string(),
        (true, _) => UNSET.to_string(),
        (false, Some(v)) => v.to_string(),
        (false, None) => UNSET.to_string(),
    };
    // `adjusted` means a later rule moved the value away from what the layer literally holds. No
    // such rule exists today (see `ResolvedSetting::adjusted`); reporting it is how an operator
    // would learn their file was not taken literally the day one returns.
    let adjusted = if row.adjusted { " (adjusted)" } else { "" };
    format!("setting: {} = {value} [{}]{adjusted}", row.key, row.origin.label())
}

/// **The per-venue arming ceilings, as one line.**
///
/// ```text
/// setting: policy.venues = live=[bybit] demo=[binance] paper=<12> [policy.toml sets 2 of 14; the rest default]
/// setting: policy.venues = live=none demo=none paper=<14> [all 14 at their compiled-in default]
/// ```
///
/// The two risk-bearing tiers are NAMED in full and paper is a COUNT, deliberately: naming twelve
/// paper venues is the noise this aggregate exists to remove, while a `live` list that grew long is
/// precisely the thing an operator must see at a glance. Both lists are short by construction on any
/// sane deployment, and if one is not, that IS the disclosure.
///
/// ⚠ The modes come from the RESOLVED [`crate::Policy`] rather than from the rendered rows, so the
/// match below is exhaustive over [`VenueMode`] and a fourth tier breaks this line instead of being
/// silently counted as paper — the direction that would under-report risk. The ORIGIN half still
/// comes from the rows, because that is where provenance lives.
fn venues_line(d: &Description) -> String {
    let (mut live, mut demo, mut paper) = (Vec::new(), Vec::new(), 0usize);
    for (venue, mode) in d.settings.policy.venues.iter() {
        match mode {
            VenueMode::Live => live.push(venue),
            VenueMode::Demo => demo.push(venue),
            VenueMode::Paper => paper += 1,
        }
    }
    let total = d.settings.policy.venues.len();
    let configured = d
        .rows
        .iter()
        .filter(|r| r.key.starts_with(VENUES_PREFIX) && r.origin != Origin::Default)
        .count();
    // `none` rather than `[]`: an empty bracket pair reads as a rendering bug at a glance, and this
    // is the line whose whole job is being read at a glance.
    let named = |v: &[&str]| match v.is_empty() {
        true => "none".to_string(),
        false => format!("[{}]", v.join(",")),
    };
    let origin = match configured {
        0 => format!("all {total} at their compiled-in default"),
        n => format!("policy.toml sets {n} of {total}; the rest default"),
    };
    format!(
        "setting: policy.venues = live={} demo={} paper=<{paper}> [{origin}]",
        named(&live),
        named(&demo)
    )
}

/// Whether a credential store sits in the settings directory — the fact that was invisible.
///
/// PRESENCE only, and the file is never opened (see the module doc). The three answers are kept
/// distinct on purpose: an ABSENT store is the ordinary unconfigured state and the live gate, while
/// a store whose existence cannot even be ESTABLISHED is a permissions problem wearing the
/// "not configured" answer — `vike_secrets::resolve` draws the same line one level down.
fn credential_store_line(settings_dir: Option<&Path>) -> String {
    let Some(dir) = settings_dir else {
        return format!(
            "credential store: NOT LOOKED FOR — there is no settings directory to hold {} (every \
             venue stays paper)",
            vike_secrets::SECRETS_FILE
        );
    };
    let path = dir.join(vike_secrets::SECRETS_FILE);
    match path.try_exists() {
        Ok(true) => format!("credential store: {} PRESENT", path.display()),
        Ok(false) => format!(
            "credential store: {} ABSENT — no credentials, so every venue stays paper",
            path.display()
        ),
        Err(e) => format!(
            "credential store: {} could not be STATTED: {e} — this is not the same as absent, and \
             every venue will stay paper for the wrong reason",
            path.display()
        ),
    }
}

/// The store's stat-only findings: its exposure when it is there, and its ABANDONED predecessor
/// when it is not.
///
/// Both come straight from `vike-secrets`, which returns them as data because it carries no logging
/// dependency. Neither probe opens the file — that is why they are exported at all, and why a
/// disclosure surface may call them.
fn credential_store_findings(settings_dir: Option<&Path>) -> Vec<String> {
    let Some(dir) = settings_dir else { return Vec::new() };
    let path = dir.join(vike_secrets::SECRETS_FILE);
    let mut out = Vec::new();
    // A mode granting anything to group or other, or a symlink whose target is somewhere else
    // entirely. Paths and an octal mode; never a credential.
    if let Some(w) = vike_secrets::permission_warning(&path) {
        out.push(format!("credential store: ⚠ {w}"));
    }
    // Only meaningful when the store is ABSENT — a `<project>/.env` beside a store that loaded is a
    // systemd `EnvironmentFile` doing its job. `legacy_store_warning` applies that gate itself only
    // insofar as its caller does, so the presence check is here.
    if matches!(path.try_exists(), Ok(false))
        && let Some(w) = vike_secrets::legacy_store_warning(&path)
    {
        out.push(format!("credential store: ⚠ {w}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Origin;

    fn row(key: &str, value: Option<&str>, origin: Origin) -> ResolvedSetting {
        ResolvedSetting {
            key: key.to_string(),
            file: "policy.toml",
            value: value.map(str::to_string),
            default: None,
            origin,
            origin_value: value.map(str::to_string),
            adjusted: false,
        }
    }

    /// A credential-shaped KEY never renders its value, whichever layer set it — the insurance the
    /// module doc argues for, exercised with a field the model does not have yet.
    #[test]
    fn a_credential_shaped_key_renders_set_not_its_value() {
        let secret = row("config.bot_token", Some("hunter2"), Origin::File("config.toml"));
        let line = setting_line(&secret);
        assert!(!line.contains("hunter2"), "the value leaked: {line}");
        assert!(line.contains(SET), "{line}");
        // …and the ORIGIN is still reported: hiding a value must not hide which layer set it.
        assert!(line.contains("config.toml"), "{line}");
    }

    /// An unset secret is `<unset>`, not `<set>`: a credential's compile-time default is always
    /// "absent" (absent credentials ARE the live gate), and an empty string is the same answer.
    #[test]
    fn an_absent_secret_is_unset_not_set() {
        for value in [None, Some("")] {
            let line = setting_line(&row("config.api_key", value, Origin::Default));
            assert!(line.contains(UNSET), "{line}");
            assert!(!line.contains(SET), "{line}");
        }
    }

    /// An ordinary knob is printed verbatim — the redaction must not swallow the disclosure.
    #[test]
    fn an_ordinary_setting_prints_its_value_and_origin() {
        let line =
            setting_line(&row("policy.max_leverage", Some("3"), Origin::File("policy.toml")));
        assert_eq!(line, "setting: policy.max_leverage = 3 [policy.toml]");
    }
}
