//! **What this process actually resolved, rendered for the log at startup.**
//!
//! [`crate::provenance`] computes every setting's effective value and its ORIGIN, and
//! `vike-cli config show` is a command somebody has to think to run, on the box, after they
//! already suspect something. This module is the daemon's own disclosure.
//!
//! # Why
//!
//! A project-root walk that answers with an unrelated directory loads no policy, no config and NO
//! CREDENTIALS, so every venue silently drops to paper — with no error, because "there are no
//! settings here" and "the settings here say nothing" are indistinguishable downstream.
//! `crates/vike-model/src/paths/state_path.rs`'s `project_settings_dir` carries the history of that
//! walk. The fix is a one-line read at startup: the directory that answered, and whether a
//! credential store was found beside it.
//!
//! # ⚠ It describes the resolution THIS PROCESS IS RUNNING ON, which is why it takes the store arm
//!
//! [`boot_lines`] takes a [`crate::StoreLayer`] and hands it to [`crate::describe_with_source`].
//! Calling the store-BLIND [`crate::describe`] instead would make the whole block a SECOND,
//! differently-configured resolution rather than a description of the first one: a description
//! built without the store credits nothing for a value the rows supplied.
//!
//! There are no settings files to report the presence of (`docs/decisions/0086`), so a `db` origin
//! is stated unconditionally.
//!
//! ⚠ **The arm is DATA the caller already read, and this crate still opens no store** — which is
//! exactly the property `crates/vike-config/Cargo.toml`'s `vike-secrets` edge is declared on
//! (*"this crate never opens the store, and must not"*): under ONE database a handle that reads
//! settings also reaches the credential table, so the fix takes a `StoreLayer` rather than a path.
//! Every probe below is still stat-only.
//!
//! # What is printed, and what is deliberately not
//!
//! [`boot_lines`] renders, in order: the settings DIRECTORY, the resolved SETTINGS, the CREDENTIAL
//! STORE's presence, and a one-line tally.
//!
//! The settings half is not every key. It is **every `policy.*` row, always** — they are the risk
//! ceilings, they are DATABASE-only by construction, and a daemon that signs real orders should
//! state them whether or not anything set them — **plus every row some layer actually configured**.
//! The rest are named by count with a pointer to `vike-cli config show`, which prints all of them. A
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
//! An unsound settings store is MARKED rather than refused by [`fn@crate::load`] (see
//! [`crate::Settings::seal_refusal`]); this module renders that mark as a line, never a second
//! refusal. That matters for `vike-recorder`, where this is the first loader in the process.

use std::path::Path;

use crate::provenance::{Description, Origin, ResolvedSetting, describe_with_source};
use crate::source::StoreLayer;
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
/// `source` is **the settings-store arm the caller already read**, handed straight to
/// [`describe_with_source`]. It is a REQUIRED parameter rather than an `Option` and rather than a
/// second store-blind entry point: a shim that let a caller skip it would call
/// [`crate::describe`] with nothing saying the block described a resolution the process was not
/// running. A caller that genuinely has no store says so with
/// [`StoreLayer::NotConsulted`], whose whole job is to make that declaration visible in its own
/// diff.
///
/// ⚠ **It must be the arm the LOADER got, from the same read** — not a fresh one. `vike_boot::boot`
/// resolves the settings directory once and reads the store once, and
/// `crates/vike-boot/tests/one_owner.rs` is the gate on that ownership; a second read here could
/// answer differently from the settings the process is holding, which is the class of bug the boot
/// sequence exists to prevent.
///
/// Never fails and never panics: see the module doc's last section.
pub fn boot_lines(
    settings_dir: Option<&Path>,
    source: StoreLayer<'_>,
    env: &std::collections::HashMap<String, String>,
) -> Vec<String> {
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

    match describe_with_source(settings_dir, source, env) {
        Ok(description) => {
            // The per-venue ceilings, as ONE line, before the per-row loop — it leads the settings
            // block because "which venues is this box armed for" is the first thing an operator
            // reading a trading daemon's banner wants, and because the loop below skips its rows.
            lines.push(venues_line(&description));

            let mut defaulted = 0usize;
            let mut configured = 0usize;
            let mut from_rows = 0usize;
            for row in &description.rows {
                let is_default = row.origin == Origin::Default;
                if is_default {
                    defaulted += 1;
                } else {
                    configured += 1;
                }
                if row.origin == Origin::Db {
                    from_rows += 1;
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
            let by = match from_rows {
                0 => "the environment".to_string(),
                n => format!("the settings database ({n} rows) or the environment"),
            };
            lines.push(format!(
                "settings: {configured} set by {by}, {defaulted} at their compiled-in default \
                 (`vike-cli config show` prints every one)"
            ));

            // The loader's own non-fatal resolutions, INCLUDING a marked seal/store refusal. The
            // caller may already surface these; a duplicate line costs nothing next to a resolution
            // that reaches nobody.
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

/// The words the UNREAD-file finding carries, spelled ONCE so [`boot_line_level`] keys on the
/// producer's own text rather than on a copy of it that could drift.
const NOT_READ: &str = "is on disk but is NOT READ";

/// What [`boot_line_level`] answers: how loudly a root logs one [`boot_lines`] line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootLineLevel {
    /// The disclosure itself — what the box resolved.
    Info,
    /// A `credential store: ⚠` finding: something an operator should fix, that changes nothing
    /// about what this process does.
    Warn,
    /// A credential FILE on disk with NO settings database: the operator's keys may sit in a file
    /// nothing reads, so this box is paper in silence unless the banner SAYS it.
    Error,
}

/// **How loudly to log one [`boot_lines`] line.** The lines stay `String`s (a root decides where
/// they go); this is the one place that says which of them is an alarm.
///
/// ⚠ The UNREAD-file finding is an ERROR, and the reason is specific to the credential FILE store's
/// removal (2026-10-07): a box whose keys still sit in `secrets.env` has no settings database — and
/// the daemon profile lives in that database too, so a trading daemon on such a box refuses for want
/// of a profile BEFORE it ever reaches the credential read that would log the finding again. This
/// banner line is the only thing such a start says about the file, so it must not be an INFO line.
#[must_use]
pub fn boot_line_level(line: &str) -> BootLineLevel {
    if !line.starts_with("credential store: ⚠") {
        BootLineLevel::Info
    } else if line.contains(NOT_READ) {
        BootLineLevel::Error
    } else {
        BootLineLevel::Warn
    }
}

/// One resolved setting: `setting: policy.max_leverage = 1.0 [db]`.
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
    // would learn their write was not taken literally the day one returns.
    let adjusted = if row.adjusted { " (adjusted)" } else { "" };
    format!("setting: {} = {value} [{}]{adjusted}", row.key, row.origin.label())
}

/// **The per-venue arming ceilings, as one line.**
///
/// ```text
/// setting: policy.venues = live=[bybit] demo=[binance] paper=<12> [db sets 2 of 14; the rest default]
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
        n => format!("db sets {n} of {total}; the rest default"),
    };
    format!(
        "setting: policy.venues = live={} demo={} paper=<{paper}> [{origin}]",
        named(&live),
        named(&demo)
    )
}

/// Whether a credential store sits in the settings directory — the fact that was invisible.
///
/// PRESENCE only, and nothing is opened (see the module doc). The store is the settings DATABASE —
/// the only credential store since the credential FILE store was removed on 2026-10-07 — so this
/// line names it, and the three answers are kept distinct on purpose: an ABSENT store is the
/// ordinary unconfigured state and the live gate, while a store whose existence cannot even be
/// ESTABLISHED is a permissions problem wearing the "not configured" answer.
fn credential_store_line(settings_dir: Option<&Path>) -> String {
    let Some(dir) = settings_dir else {
        return format!(
            "credential store: NOT LOOKED FOR — there is no settings directory to hold {} (every \
             venue stays paper)",
            vike_secrets::DB_FILE
        );
    };
    let db = vike_secrets::db_path_in(dir);
    match db.try_exists() {
        Ok(true) if vike_secrets::database_present(&db) => format!(
            "credential store: {} PRESENT (the settings DATABASE — the only credential store)",
            db.display()
        ),
        Ok(_) => format!(
            "credential store: {} ABSENT — no credentials, so every venue stays paper \
             (`vike-cli secrets migrate --init` creates it)",
            db.display()
        ),
        Err(e) => format!(
            "credential store: {} could not be STATTED: {e} — this is not the same as absent, and \
             every venue will stay paper for the wrong reason",
            db.display()
        ),
    }
}

/// The store's stat-only findings: its exposure when it is there, the retired credential FILE
/// beside it (shadowed with a database, NOT READ without one), and the pre-one-store `.env` when
/// there is no store at all.
///
/// Every probe here is a `stat`; no file is opened — that is why the `vike-secrets` helpers are
/// exported at all, and why a disclosure surface may call them. (The daemon's credential read then
/// says the unread-file finding again, with the count of keyed names, through
/// `vike_bridge_core::credentials::try_load_workspace_secrets_at`.)
fn credential_store_findings(settings_dir: Option<&Path>) -> Vec<String> {
    let Some(dir) = settings_dir else { return Vec::new() };
    let path = dir.join(vike_secrets::SECRETS_FILE);
    let db = vike_secrets::db_path_in(dir);
    let migrated = vike_secrets::database_present(&db);
    let mut out = Vec::new();
    // A mode granting anything to group or other, or a symlink whose target is somewhere else
    // entirely. Paths and an octal mode; never a credential. Asked of the DATABASE, the one store —
    // CREATED 0600 inside a 0700 directory, so this probe is expected to stay quiet, which is
    // exactly why it must run: a quiet check that is not performed and a quiet check that passed are
    // indistinguishable in a log.
    if migrated && let Some(w) = vike_secrets::permission_warning(&db) {
        out.push(format!("credential store: ⚠ {w}"));
    }
    let file_present = matches!(path.try_exists(), Ok(true));
    // ⚠ The SHADOWED file, said at the banner. The credential read logs it too — but only once a
    // root gets as far as LOADING credentials, and the banner is printed before that and on roots
    // that never do.
    if migrated && file_present {
        out.push(format!(
            "credential store: ⚠ {} is still on disk and is NO LONGER READ — the settings database \
             above answers. Nothing has moved or deleted it; an edit to that file changes nothing \
             until it is migrated in (`vike-cli secrets migrate`).",
            path.display()
        ));
    }
    // ⚠ …and the UNREAD file: a `secrets.env` on a box with NO database. The credential FILE store
    // was removed on 2026-10-07, so this box has no credentials and every venue mounts paper while
    // the operator's keys may sit in that file — the one state in which a configured box goes paper
    // in silence. Said here, at the banner every root prints first.
    if !migrated && file_present {
        out.push(format!(
            "credential store: ⚠ {} {NOT_READ} — there is no settings database, the \
             only credential store, so this box has NO credentials and every venue mounts PAPER. \
             Carry it in with `vike-cli secrets migrate` (`--dry-run` first; it only READS the \
             file and never edits, moves or deletes it).",
            path.display()
        ));
    }
    // Only meaningful when there is no store AND no credential file — a `<project>/.env` beside a
    // store that loaded is a systemd `EnvironmentFile` doing its job, and beside a `secrets.env` the
    // finding above is the louder one.
    if !migrated
        && matches!(path.try_exists(), Ok(false))
        && let Some(w) = vike_secrets::legacy_store_warning(&path)
    {
        out.push(format!("credential store: ⚠ {w}"));
    }
    out
}

#[path = "boot_tests.rs"]
#[cfg(test)]
mod boot_tests;
