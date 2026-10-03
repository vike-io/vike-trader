//! `vike-cli config recorder` — **print the recorder profile rows, leading with the store that
//! answered.**
//!
//! # Why a read verb was UNAVOIDABLE, not a convenience
//!
//! Before 2026-09-16 an operator answered "what is this box recording, and where" with `grep`:
//!
//! ```text
//! grep '^store' <root>/settings/recorder.toml
//! ```
//!
//! and that command is not in one place. `docs/ops/recorder-deploy.md` runs it in the MERGE FLIP's
//! step 1, and **two shipped units embedded it in their own troubleshooting comments** — so it is a
//! live procedure on a box that may have no `sqlite3` binary at all. A migration that took the file
//! out of the read path without replacing the command would have left a runbook telling an operator
//! to grep a file the daemon no longer reads, which is worse than no runbook.
//!
//! Nothing existing could answer it. `vike-cli config show` is provenance over the FOUR typed
//! settings files plus the environment, keyed on `vike_config::provenance::setting_keys`; a
//! recorder profile is not a `vike_config::Settings` field and cannot acquire a row there.
//! `vike_secrets::profile_store::read_profiles` had no production caller at all.
//!
//! ⚠ **`sqlite3` is not needed and neither is an install.** Both shipped recording units carry
//! `ExecStartPre=<root>/bin/vike-cli config check`, so `vike-cli` is present and executable on any
//! box running the shipped daemon — which is why this is a verb here rather than a documented SQL
//! query.
//!
//! # The `source:` line
//!
//! Printed first, always, naming the DATABASE the answer came from. It is `vike-cli secrets list`'s
//! established idiom and it exists for the same reason: once a thing can live in two places, "which
//! one answered" is the question a reader cannot reconstruct from the answer. On a box that has not
//! migrated, the honest answer is that the store holds nothing and the FILE is still what the
//! daemon reads — and this verb says exactly that rather than printing an empty list.

use std::path::Path;
use std::process::ExitCode;

use crate::cmd::args::{self, Flags};

/// The verb's own usage, printed by `--help` and by a parse error.
pub const USAGE: &str = "usage: vike-cli config recorder [--name <name>]\n\n  Print the recorder \
                         profile rows in <project>/settings/db/vike.db — the store root, the\n  \
                         subscriptions, and the maintenance/alerting knobs. Leads with a \
                         `source:` line\n  naming the store that answered.\n\n  --name <name>   \
                         print only this profile (default: every recorder profile)";

/// Parsed flags.
#[derive(Debug)]
struct Args {
    name: Option<String>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args { name: None };
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "-h" | "--help" => return args::help_requested(),
            "--name" => out.name = Some(flags.value(&flag, inline)?),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    Ok(out)
}

/// Entry point the `config` dispatcher routes to.
pub fn run(args: impl Iterator<Item = String>, settings_dir: Option<&Path>) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config recorder", USAGE, &msg),
    };
    match execute(&args, settings_dir) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("vike-cli config recorder: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn execute(args: &Args, settings_dir: Option<&Path>) -> Result<String, String> {
    let Some(dir) = settings_dir else {
        return Err(
            "no settings directory resolved, so there is no store to read. Set VIKE_SETTINGS_DIR, \
             or run from a project that has one."
                .to_string(),
        );
    };
    let db = vike_secrets::db_path_in(dir);
    let profiles = vike_secrets::profile_store::read_profiles(&db).map_err(|e| e.to_string())?;
    Ok(render(&db.display().to_string(), &profiles, args.name.as_deref()))
}

/// The report, as text. Pure over an already-read `Profiles`, so the wording is testable without a
/// database.
fn render(
    store: &str,
    profiles: &vike_secrets::profile_store::Profiles,
    only: Option<&str>,
) -> String {
    use vike_secrets::profile_store::ProfileKind;
    let mut out = format!("source: {store}\n");
    let rows: Vec<_> = profiles
        .all()
        .iter()
        .filter(|p| p.row.kind == ProfileKind::Recorder)
        .filter(|p| only.is_none_or(|n| p.row.name == n))
        .collect();
    if rows.is_empty() {
        // ⚠ NOT an empty list. "No rows" and "this box has not migrated" are the same ANSWER and a
        // different FACT, and only the second tells an operator where the daemon is actually
        // reading from — which is the whole question this verb replaced a `grep` to answer.
        out.push_str(if profiles.tables_present() {
            "  (no recorder profile rows). A daemon started with --record <path> reads its TOML \
             FILE and is unaffected by this; `vike-cli config mirror --recorder <file>` is what \
             puts one here.\n"
        } else {
            "  (this store holds no profile tables — nothing has been mirrored into it). The \
             recorder profile this box uses is still the FILE its unit's --record names; \
             `vike-cli config mirror --recorder <file>` is what moves it.\n"
        });
        return out;
    }
    for p in rows {
        out.push_str(&format!(
            "\nprofile: {}{}\n",
            p.row.name,
            if p.row.active { " (active)" } else { "" }
        ));
        let Some(body) = &p.recorder else {
            out.push_str(
                "  ⚠ no recorder body — this profile row carries none, so a \
                          --record-profile naming it would be refused at startup\n",
            );
            continue;
        };
        // ⚠ `store` FIRST and on its own line, because it is the value the retired `grep '^store'`
        // printed and the one a MERGE FLIP compares against the unit's VIKE_DATAHUB_STORE. The
        // daemon refuses to start when the two disagree
        // (`crates/vike-datahub/src/recorder.rs`'s `one_store_root`), so this is the line an
        // operator checks BEFORE a first start.
        out.push_str(&format!("  store: {}\n", body.row.store));
        if let Some(note) = &body.row.note {
            out.push_str(&format!("  note: {note}\n"));
        }
        for s in &body.subscriptions {
            let what = match (&s.family, &s.symbols) {
                (Some(f), _) => format!("family {f}"),
                (None, Some(syms)) => format!("symbols {syms}"),
                (None, None) => "(nothing — this subscription would record no series)".to_string(),
            };
            // ⚠ The `note` is printed, and it is the ONLY reader either `note` column has. The
            // migration writes NULL into both — a TOML comment is not addressable by key, so it
            // cannot carry the operator's inline annotation — and no verb fills one yet. Printing
            // it here is what keeps the column from being a field nothing ever looks at, which is
            // the shape `vike_config::CONSUMPTION` exists to refuse one layer over.
            out.push_str(&format!(
                "  subscribe: {} {what}{}{}\n",
                s.venue,
                s.backfill.as_ref().map(|b| format!(" backfill={b}")).unwrap_or_default(),
                s.note.as_ref().map(|n| format!("  # {n}")).unwrap_or_default()
            ));
        }
        let m = &body.row;
        let knobs: Vec<String> = [
            m.interval_secs.map(|v| format!("interval_secs={v}")),
            m.min_parts.map(|v| format!("min_parts={v}")),
            m.target_mb.map(|v| format!("target_mb={v}")),
            m.max_merge_rows.map(|v| format!("max_merge_rows={v}")),
            m.retention_days.map(|v| format!("retention_days={v}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        // ⚠ An ABSENT knob is not printed as a default, and the line says which it is. The rows
        // store absence (see `vike_secrets::profile_store::RecorderRow`), so printing a resolved
        // default here would tell an operator a value was FILED that was not.
        out.push_str(&format!(
            "  maintenance: {}\n",
            if knobs.is_empty() {
                "(no keys filed — the built-in defaults apply)".to_string()
            } else {
                knobs.join(" ")
            }
        ));
        let alerts: Vec<String> = [
            m.alert_webhooks.clone().map(|v| format!("webhooks={v}")),
            m.alert_repeat_secs.map(|v| format!("repeat_secs={v}")),
            m.alert_series_prefix.clone().map(|v| format!("series_prefix={v}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push_str(&format!(
            "  alerting: {}\n",
            if alerts.is_empty() {
                "(no keys filed — the alert still fires and reaches the log; nothing is POSTed)"
                    .to_string()
            } else {
                alerts.join(" ")
            }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use vike_secrets::profile_store::{
        ProfileKind, ProfileRow, Profiles, RecorderBody, RecorderRow, StoredProfile,
        SubscriptionRow,
    };

    use super::*;

    fn sample() -> StoredProfile {
        StoredProfile {
            row: ProfileRow {
                name: "default".to_string(),
                kind: ProfileKind::Recorder,
                active: false,
                note: None,
            },
            mounts: Vec::new(),
            params: BTreeMap::new(),
            settings: BTreeMap::new(),
            recorder: Some(RecorderBody {
                row: RecorderRow {
                    store: "/srv/vike-<unit>/market_data/hist".to_string(),
                    interval_secs: Some(300),
                    min_parts: None,
                    target_mb: None,
                    max_merge_rows: None,
                    retention_days: None,
                    alert_webhooks: None,
                    alert_repeat_secs: None,
                    alert_series_prefix: None,
                    note: None,
                },
                subscriptions: vec![SubscriptionRow {
                    ord: 0,
                    venue: "polymarket".to_string(),
                    family: Some("btc-updown-5m".to_string()),
                    symbols: None,
                    backfill: Some("off".to_string()),
                    note: None,
                }],
            }),
        }
    }

    /// **The `store` line is what replaced the retired `grep '^store'`**, so it must be there and
    /// it must be the value a MERGE FLIP compares against the unit's `VIKE_DATAHUB_STORE`.
    #[test]
    fn the_report_leads_with_the_source_and_names_the_store() {
        let profiles = Profiles::from_rows(vec![sample()]);
        let out = render("/srv/x/settings/db/vike.db", &profiles, None);
        assert!(out.starts_with("source: /srv/x/settings/db/vike.db"), "{out}");
        assert!(out.contains("store: /srv/vike-<unit>/market_data/hist"), "{out}");
        assert!(out.contains("subscribe: polymarket family btc-updown-5m backfill=off"), "{out}");
        // An ABSENT knob is not rendered as a default — the rows can say "absent" and the report
        // must not turn that into a filed value.
        assert!(out.contains("interval_secs=300"), "{out}");
        assert!(!out.contains("min_parts"), "an absent knob must not print: {out}");
        assert!(out.contains("no keys filed"), "…and the alerting table says so: {out}");
    }

    /// **An un-migrated box gets a different sentence from an empty one**, because only one of them
    /// tells the operator that the FILE is still what the daemon reads.
    #[test]
    fn nothing_stored_says_where_the_daemon_is_actually_reading_from() {
        let out = render("/srv/x/settings/db/vike.db", &Profiles::none(), None);
        assert!(out.contains("no profile tables"), "{out}");
        assert!(out.contains("still the FILE"), "{out}");

        let empty = Profiles::from_rows(Vec::new());
        let out = render("/srv/x/settings/db/vike.db", &empty, None);
        assert!(out.contains("no recorder profile rows"), "{out}");
        assert!(out.contains("--record <path>"), "…and names what such a daemon reads: {out}");
    }
}
