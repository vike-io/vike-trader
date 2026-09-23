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
//! # ⚠ It describes the resolution THIS PROCESS IS RUNNING ON, which is why it takes the store arm
//!
//! [`boot_lines`] takes a [`crate::StoreLayer`] and hands it to [`crate::describe_with_source`]. It
//! called the store-BLIND [`crate::describe`] until 2026-09-22, and that made the whole block a
//! SECOND, differently-configured resolution rather than a description of the first one — on an
//! ADOPTED box (`docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s
//! crossing) the rows answer, and a description built without them credits a file for a value the
//! file did not supply.
//!
//! MEASURED on the live the CI box `vike-tradehub`, one start, two lines 24 µs apart: a WARN reading
//! *"this box resolves its settings from the settings DATABASE … so policy.toml, config.toml,
//! flags.toml on disk are INERT — editing one changes nothing"* ([`crate::drift`]'s
//! `boot_warnings`, re-emitted below as a `settings:` line) followed by
//! `setting: policy.max_notional_per_order = 100.0 [policy.toml]`. Both true of different
//! resolutions; only one of them was running.
//!
//! ⚠ The version that BITES is the one that arrives after the inert files are deleted, which is
//! the CI box's state since 2026-09-22: the store-blind read then resolves to COMPILED-IN DEFAULTS, so
//! the block would have announced `policy.max_notional_per_order = <unset> [default]` and every
//! venue at `paper` while the daemon enforced `100.0` and armed three venues LIVE from the rows. A
//! disclosure that under-reports a live posture is worse than no disclosure: `crate::consumed`
//! exists one level down for the same reason, where it is a KEY nothing reads instead of a whole
//! RESOLUTION nothing is running.
//!
//! ⚠ **The arm is DATA the caller already read, and this crate still opens no store** — which is
//! exactly the property `crates/vike-config/Cargo.toml`'s `vike-secrets` edge is declared on
//! (*"this crate never opens the store, and must not"*) and the one
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` singles out for this
//! function: under ONE database a handle that reads settings also reaches the credential table, so
//! the fix takes a `StoreLayer` rather than a path. Every probe below is still stat-only.
//!
//! # What is printed, and what is deliberately not
//!
//! [`boot_lines`] renders, in order: the settings DIRECTORY, one line per settings FILE (present or
//! absent, with its key count), **which SOURCE answered when it is not the files**, the resolved
//! SETTINGS, the CREDENTIAL STORE's presence, and a one-line tally.
//!
//! The source line is printed only under [`Authority::Store`], and the asymmetry is the point:
//! [`crate::Authority::Files`] is the state of every CI lane, every fresh clone and every box that
//! has not run `vike-cli config adopt`, and those boxes must read exactly what they read before
//! this parameter existed. Under [`Authority::Store`] it discharges the obligation
//! [`crate::Description::authority`] states in as many words — a file listed as `present` beside a
//! value it did not supply is an ambiguity a renderer must remove.
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

use crate::provenance::{Description, Origin, ResolvedSetting, describe_with_source};
use crate::source::{Authority, StoreLayer};
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
/// second store-blind entry point, because a shim that let a caller skip it is exactly what this
/// function was: it called [`crate::describe`], and nothing in the type system said the block
/// described a resolution the process was not running (see the module doc). A caller that genuinely
/// has no store says so with [`StoreLayer::NotConsulted`], whose whole job is to make that
/// declaration visible in its own diff.
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
    env: &HashMap<String, String>,
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
            for file in &description.files {
                lines.push(match file.present {
                    true => format!("settings file: {} present, {} keys", file.name, file.keys),
                    false => {
                        format!("settings file: {} ABSENT ({})", file.name, file.path.display())
                    }
                });
            }

            // ⚠ **WHICH SOURCE answered, said straight after the files it qualifies.** Under
            // `Authority::Store` every `settings file:` line above describes a stale DRAFT rather
            // than a layer, and `Description::authority`'s own doc says a renderer must say so.
            //
            // Printed under that authority ONLY. `Authority::Files` is every CI lane, every fresh
            // clone and every box that has not run `vike-cli config adopt`, and its block has to
            // stay byte-identical to what it printed before this function took a store arm —
            // `crates/vike-config/tests/boot.rs`'s
            // `a_box_whose_store_supplies_nothing_renders_the_same_block_under_every_arm` is that gate.
            //
            // ⚠ It is NOT a duplicate of `crate::drift`'s `boot_warnings` line, which is re-emitted
            // below with the loader's other warnings: that one fires only when an inert FILE is
            // still on disk to warn about, and the CI box deleted all four on 2026-09-22. This line is
            // the one that survives the deletion, which is the state the rows alone decide in.
            if description.authority == Authority::Store {
                lines.push(
                    "settings: the settings DATABASE answers for every key on this box (`vike-cli \
                     config adopt` sealed it) — the settings files above are NOT read for \
                     resolution, whatever they hold, and every `db` origin below is a row"
                        .to_string(),
                );
            }

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
            // ⚠ **The layer list NAMES THE STORE only when a row actually supplied a value**, and
            // the no-row spelling is a LITERAL rather than something derived. That asymmetry is
            // deliberate: this sentence must be byte-identical on every box whose store contributed
            // nothing — which is every CI lane, every fresh clone and every box that has not run
            // `vike-cli config mirror` — while on a box where the rows DID supply values, a tally
            // reading "set by a file or the environment" is the aggregate wearing exactly the
            // defect the per-row cells were just fixed for. `from_rows` is the count of rows the
            // store answered for, so an operator can see how much of `configured` is not in a file.
            //
            // A file is not named under `Authority::Store`, where `Origin::File` is unreachable by
            // construction (the loader skips layer 2 whole) — naming it there would advertise a
            // layer nothing applied, which is `crate::PRECEDENCE_STORE`'s own argument one surface
            // over.
            let by = match (from_rows, description.authority) {
                // ⚠ The zero case splits on authority for the SAME reason the arms below do: under
                // `Authority::Store` no key can have come from a file, so an env-only box must
                // not be told "a file". Measured 2026-09-22 on an adopted, row-less store with
                // `VIKE_RECONCILE=1`, which rendered "1 set by a file or the environment" two
                // lines under a banner saying the database answers for every key.
                (0, Authority::Store) => "the environment".to_string(),
                (0, Authority::Files) => "a file or the environment".to_string(),
                (n, Authority::Store) => {
                    format!("the settings database ({n} rows) or the environment")
                }
                (n, Authority::Files) => {
                    format!("a file, the settings database ({n} rows) or the environment")
                }
            };
            lines.push(format!(
                "settings: {configured} set by {by}, {defaulted} at their compiled-in default \
                 (`vike-cli config show` prints every one)"
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
///
/// ⚠ **And the ARTIFACT it names is read off those rows too, where it used to be the literal
/// `policy.toml`.** That literal was written when a file was the only thing a venue row could come
/// from; on an ADOPTED box every one of them carries [`Origin::Db`], and the line went on crediting
/// a file for an arming ceiling the file did not set — the aggregate's copy of the defect the
/// module doc measures. Deriving it cannot drift from the per-row cells, and it renders the mixed
/// case an unadopted box with a hand-edited store really produces (`policy.toml` for the keys the
/// file sets, `db/vike.db` for the ones only the store does), which a single authority word could
/// not say.
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
    let configured: Vec<&Origin> = d
        .rows
        .iter()
        .filter(|r| r.key.starts_with(VENUES_PREFIX) && r.origin != Origin::Default)
        .map(|r| &r.origin)
        .collect();
    // `none` rather than `[]`: an empty bracket pair reads as a rendering bug at a glance, and this
    // is the line whose whole job is being read at a glance.
    let named = |v: &[&str]| match v.is_empty() {
        true => "none".to_string(),
        false => format!("[{}]", v.join(",")),
    };
    let origin = match configured.len() {
        0 => format!("all {total} at their compiled-in default"),
        n => format!("{} sets {n} of {total}; the rest default", artifacts(&configured)),
    };
    format!(
        "setting: policy.venues = live={} demo={} paper=<{paper}> [{origin}]",
        named(&live),
        named(&demo)
    )
}

/// **The distinct ARTIFACTS a set of configured rows came out of**, in first-seen order — the
/// subject of [`venues_line`]'s origin clause.
///
/// [`Origin::detail`] rather than [`Origin::label`] for the two artifact-bearing layers, because
/// this slot has always held an artifact NAME an operator can go and edit: `policy.toml` for a file
/// rung (where the two spellings are the same string, which is what keeps an unadopted box
/// byte-identical) and `db/vike.db` for the store's, where `label` would render the bare word `db`
/// the per-ROW cells use. The absolute location of that database is disclosed once by
/// [`credential_store_line`] lower down, so naming the artifact here is not naming it twice.
///
/// The `other` arm is unreachable for a `policy.*` key — [`crate::Policy`] implements neither
/// sealed override trait, so no venue row can carry [`Origin::Env`], and [`Origin::Default`] is
/// filtered out by the caller. It renders rather than ignores, so that a layer added to
/// [`crate::PRECEDENCE_FILES`] in future shows up here instead of being silently attributed to
/// whichever artifact happened to be first.
fn artifacts(origins: &[&Origin]) -> String {
    let mut out: Vec<String> = Vec::new();
    for origin in origins {
        let name = match origin {
            Origin::File(_) | Origin::Db => origin.detail().to_string(),
            other => other.label(),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out.join(" and ")
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
    // ⚠ **WHICH STORE ANSWERS, asked before the file is described.** This line named
    // `secrets.env` unconditionally, and on a MIGRATED box that is a file the process does not
    // read: `vike_secrets::Backend` makes its choice on one `is_file` over the DATABASE, before any
    // name is looked up, so once `settings/db/vike.db` exists the text file answers for nothing.
    //
    // MEASURED on the live the CI box daemon (v0.1.26, 2026-09-19): it printed
    // `credential store: …/secrets.env PRESENT` while `vike-cli secrets path` on the same box
    // printed *"answers: the settings DATABASE above … The store line names a file that is NO
    // LONGER READ"*, and the daemon said "shadowed" exactly ZERO times in its whole run. So an
    // operator reading the daemon's own banner was sent to edit a file nothing opens — the precise
    // failure this whole module exists to prevent, wearing a correct-looking sentence.
    //
    // ⚠ Still STAT-ONLY, so the module doc's rule is intact: `database_present` is one `is_file`
    // and nothing here opens either store. What changes is which PATH the line describes, never how
    // hard it looks at it.
    let db = vike_secrets::db_path_in(dir);
    if vike_secrets::database_present(&db) {
        return format!(
            "credential store: {} PRESENT (the settings DATABASE — {} is not read on this box, \
             whether or not it is still on disk)",
            db.display(),
            vike_secrets::SECRETS_FILE
        );
    }
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
    let db = vike_secrets::db_path_in(dir);
    let migrated = vike_secrets::database_present(&db);
    let mut out = Vec::new();
    // A mode granting anything to group or other, or a symlink whose target is somewhere else
    // entirely. Paths and an octal mode; never a credential.
    //
    // ⚠ **Asked of the store that ANSWERS, not of `secrets.env` always.** On a migrated box the
    // database is what holds the keys, and reporting a text file's mode while saying nothing about
    // the file 67 credentials actually live in is a disclosure that looks complete and is not. The
    // database is CREATED 0600 inside a 0700 directory, explicitly — so this probe is expected to
    // stay quiet, which is exactly why it must run: a quiet check that is not performed and a quiet
    // check that passed are indistinguishable in a log.
    if let Some(w) = vike_secrets::permission_warning(if migrated { &db } else { &path }) {
        out.push(format!("credential store: ⚠ {w}"));
    }
    // Only meaningful when the store is ABSENT — a `<project>/.env` beside a store that loaded is a
    // systemd `EnvironmentFile` doing its job. `legacy_store_warning` applies that gate itself only
    // insofar as its caller does, so the presence check is here.
    //
    // ⚠ And only on an UNMIGRATED box: once the database answers, `secrets.env` being absent says
    // nothing about a `.env` predecessor, because neither file is consulted for a credential.
    if !migrated
        && matches!(path.try_exists(), Ok(false))
        && let Some(w) = vike_secrets::legacy_store_warning(&path)
    {
        out.push(format!("credential store: ⚠ {w}"));
    }
    // ⚠ The SHADOWED store, said at the banner. `vike_secrets` returns this finding beside the
    // credentials and `vike_bridge_core::credentials::try_load_workspace_secrets_at` logs it — but
    // only once a root gets as far as LOADING credentials, and the banner is printed before that
    // and on roots that never do. MEASURED on the live the CI box daemon: across its whole run it said
    // "NO LONGER READ" zero times while `secrets.env` sat on disk, unread, beside a database
    // holding every key. The operator's only signal was a banner line naming the wrong file.
    if migrated && matches!(path.try_exists(), Ok(true)) {
        out.push(format!(
            "credential store: ⚠ {} is still on disk and is NO LONGER READ — the settings database \
             above answers. Nothing has moved or deleted it; an edit to that file changes nothing \
             until it is migrated in (`vike-cli secrets migrate`).",
            path.display()
        ));
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
