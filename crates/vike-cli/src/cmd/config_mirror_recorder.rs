//! `vike-cli config mirror --recorder <file>` — **the recorder profile becomes rows, and the
//! migration proves it changed nothing before it writes.**
//!
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` answered
//! `recorder.toml` with a NO. The owner OVERRULED that on 2026-09-16, and the record's own stated
//! reopener is what fired: the deploy-layout question was answered the same day. This module is the
//! WRITE half of that reversal.
//!
//! # ⚠ THE FENCE, AND WHY IT IS NOT [`plan_active_row`]
//!
//! Phase 3's fence is about the ACTIVE ROW: writing one can arm a mount that a missing selection was
//! holding. That hazard exists here too and is handled below — but it is the SMALLER of the two,
//! and taking it for the whole answer would have shipped the bigger one.
//!
//! **A recorder profile decides WHICH VENUE FEEDS OPEN.** A body that is not what the box runs
//! today is not a mis-read setting; it is a daemon subscribing to markets nobody asked for, writing
//! them into the live store, on the next restart. So the fence here is on the BODY:
//!
//! > the rows are written only when RENDERING THEM BACK produces a document that parses EQUAL to
//! > the file they came from.
//!
//! That is stronger than comparing typed values, and deliberately so: a `RecorderProfile` applies
//! defaults, so two profiles can be equal as values while one of them dropped a key the migration
//! failed to carry. Comparing the parsed DOCUMENTS catches exactly that — and it is why every
//! column in `vike_secrets::profile_store::RecorderRow` is nullable with NULL meaning ABSENT rather
//! than "the default".
//!
//! # …and the active row is still withheld
//!
//! `plan_active_row` is consulted, and on every box today it answers
//! `Withhold { NothingSelectedToday }` — because NOTHING selects a recorder profile:
//! `--record`/`--record-profile` are explicit arguments with no default and no environment
//! variable. The withholding is REPORTED rather than silent, because the operator who ran this verb
//! is the one who needs to know that installing a `--record-profile` unit is a second, deliberate
//! act.
//!
//! ⚠ **The condition that makes the active row mandatory rather than moot:** giving
//! `--record-profile` a default ("record whichever recorder profile is active"). Then an active row
//! turns a serve-only daemon into one that opens venue sockets, and `WithholdReason`'s whole
//! argument applies word for word. **Keep the flag required.**

use std::collections::BTreeMap;
use std::path::Path;

use vike_secrets::profile_store::{
    ActivePlan, OperatorWrite, ProfileKind, ProfileRow, RecorderBody, RecorderRow, StoredProfile,
    SubscriptionRow, plan_active_row, read_profiles, render_recorder_toml, store_profile,
    toml_string_array,
};

/// The profile NAME a mirrored recorder body is stored under when none is given.
///
/// One box, one recording daemon, one profile: a name is what the unit's `--record-profile` spells,
/// and a default keeps the common case from being a decision. It is NOT a selection — see this
/// module's doc.
pub const DEFAULT_PROFILE_NAME: &str = "default";

/// Every key this migration knows how to carry, by TOML path.
///
/// ⚠ **This table is the write-path half of `deny_unknown_fields`, and it exists because this
/// module cannot see `vike_recorder::config`.** That crate's `RecorderProfile` is one layer up and
/// drags DataFusion, which must never join the CLI's graph
/// (`scripts/ci_feature_suite.sh`'s `light-consumers` lane). So the mapping is spelled here — and
/// an unknown key is REFUSED BY NAME rather than dropped, which is the same refusal serde performs
/// on the read side.
///
/// ⚠ It is not the authority and must not become one: `crates/vike-recorder/src/config.rs` is. What
/// holds the two together is not this list but the ROUND TRIP — a key this table failed to carry
/// makes the rendered document differ from the file, and [`rows_from_profile_text`] refuses the
/// whole run. So a key added upstream and forgotten here is a loud migration failure, never a
/// silently dropped setting.
const KNOWN_KEYS: &[&str] = &[
    "store",
    "subscribe.venue",
    "subscribe.family",
    "subscribe.symbols",
    "subscribe.backfill",
    "maintenance.interval_secs",
    "maintenance.min_parts",
    "maintenance.target_mb",
    "maintenance.max_merge_rows",
    "maintenance.retention_days",
    "alerting.webhooks",
    "alerting.repeat_secs",
    "alerting.series_prefix",
];

/// What a mirror of one recorder profile produced.
#[derive(Debug)]
pub struct Mirrored {
    /// The rows, ready to store.
    pub stored: StoredProfile,
    /// What [`plan_active_row`] decided about this kind's active row.
    pub active: ActivePlan,
    /// One line per subscription, for the report.
    pub subscriptions: Vec<String>,
}

/// **Parse a recorder profile's TOML into rows, and REFUSE unless rendering them back reproduces
/// it.**
///
/// # Errors
///
/// A `String` naming what could not be carried — an unknown key, a wrongly-typed value, or a
/// round-trip difference. Nothing is written by this function in any case: it is pure over text.
pub fn rows_from_profile_text(name: &str, text: &str) -> Result<RecorderBody, String> {
    let doc: toml::Value =
        toml::from_str(text).map_err(|e| format!("recorder profile does not parse: {e}"))?;
    let table = doc.as_table().ok_or("recorder profile is not a TOML table")?;
    for key in table.keys() {
        if !matches!(key.as_str(), "store" | "subscribe" | "maintenance" | "alerting") {
            return Err(unknown_key(key));
        }
    }
    let store = table
        .get("store")
        .ok_or("recorder profile has no `store` key, which is required")?
        .as_str()
        .ok_or("`store` must be a string")?
        .to_string();

    let mut subscriptions = Vec::new();
    if let Some(list) = table.get("subscribe") {
        let list = list.as_array().ok_or("`subscribe` must be an array of tables")?;
        for (i, entry) in list.iter().enumerate() {
            let e = entry.as_table().ok_or("each `[[subscribe]]` entry must be a table")?;
            for key in e.keys() {
                if !matches!(key.as_str(), "venue" | "family" | "symbols" | "backfill") {
                    return Err(unknown_key(&format!("subscribe.{key}")));
                }
            }
            subscriptions.push(SubscriptionRow {
                ord: i64::try_from(i).map_err(|_| "too many subscriptions".to_string())?,
                venue: e
                    .get("venue")
                    .ok_or("a `[[subscribe]]` entry has no `venue`")?
                    .as_str()
                    .ok_or("`venue` must be a string")?
                    .to_string(),
                family: opt_str(e.get("family"), "family")?,
                symbols: opt_str_array(e.get("symbols"), "symbols")?,
                backfill: opt_str(e.get("backfill"), "backfill")?,
                note: None,
            });
        }
    }

    let m = table.get("maintenance").map(|v| v.as_table().ok_or("`[maintenance]` must be a table"));
    let m = match m {
        Some(r) => Some(r?),
        None => None,
    };
    if let Some(t) = m {
        for key in t.keys() {
            if !matches!(
                key.as_str(),
                "interval_secs" | "min_parts" | "target_mb" | "max_merge_rows" | "retention_days"
            ) {
                return Err(unknown_key(&format!("maintenance.{key}")));
            }
        }
    }
    let a = table.get("alerting").map(|v| v.as_table().ok_or("`[alerting]` must be a table"));
    let a = match a {
        Some(r) => Some(r?),
        None => None,
    };
    if let Some(t) = a {
        for key in t.keys() {
            if !matches!(key.as_str(), "webhooks" | "repeat_secs" | "series_prefix") {
                return Err(unknown_key(&format!("alerting.{key}")));
            }
        }
    }

    let body = RecorderBody {
        row: RecorderRow {
            store,
            interval_secs: opt_int(m.and_then(|t| t.get("interval_secs")), "interval_secs")?,
            min_parts: opt_int(m.and_then(|t| t.get("min_parts")), "min_parts")?,
            target_mb: opt_int(m.and_then(|t| t.get("target_mb")), "target_mb")?,
            max_merge_rows: opt_int(m.and_then(|t| t.get("max_merge_rows")), "max_merge_rows")?,
            retention_days: opt_int(m.and_then(|t| t.get("retention_days")), "retention_days")?,
            alert_webhooks: opt_str_array(a.and_then(|t| t.get("webhooks")), "webhooks")?,
            alert_repeat_secs: opt_int(a.and_then(|t| t.get("repeat_secs")), "repeat_secs")?,
            alert_series_prefix: opt_str(a.and_then(|t| t.get("series_prefix")), "series_prefix")?,
            // ⚠ The operator's inline comments are NOT carried, and the loss is DECLARED rather
            // than hidden: the CI box's profile annotates `max_merge_rows` with the measurement behind
            // the number, and a TOML comment is not addressable by key. The `note` columns exist so
            // an operator can put it back with a one-key write; this verb cannot invent the
            // association. The file is not deleted by this migration either, so the comments are
            // still on disk to copy from.
            note: None,
        },
        subscriptions,
    };

    // ── THE FENCE ────────────────────────────────────────────────────────────────────────────
    // Render the rows back and require the result to PARSE EQUAL to the file. A key this module
    // failed to carry, a value it coerced, an array it reordered — all of them show up here, and
    // all of them refuse the write.
    let rendered = render_recorder_toml(&body);
    let back: toml::Value = toml::from_str(&rendered).map_err(|e| {
        format!(
            "the rows rendered a document that does not parse ({e}). Nothing was written. This is \
             a defect in the migration, not in your profile — report it with the profile that \
             produced it."
        )
    })?;
    if back != doc {
        return Err(format!(
            "REFUSING to store recorder profile `{name}`: the rows do not reproduce the profile \
             they came from, so storing them would change what this box records.\n\nNothing was \
             written. A recorder profile decides which venue feeds OPEN, so a body that is not \
             what the box runs today is a daemon subscribing to markets nobody asked \
             for.\n\n--- the profile as given ---\n{}\n--- the profile as the rows render it \
             ---\n{rendered}",
            text.trim_end()
        ));
    }
    Ok(body)
}

/// The unknown-key refusal, naming the accepted set — the same shape `toml`'s own
/// `deny_unknown_fields` message has, because this is that refusal moved to the write path.
fn unknown_key(key: &str) -> String {
    format!(
        "recorder profile carries `{key}`, which this migration does not know how to store. \
         Nothing was written. Accepted keys: {}. If the key is a real one this table has not \
         learned, add it here AND to `vike_secrets::profile_store`'s schema — a key that is \
         silently dropped is a setting an operator believes is armed.",
        KNOWN_KEYS.join(", ")
    )
}

fn opt_str(v: Option<&toml::Value>, key: &str) -> Result<Option<String>, String> {
    match v {
        None => Ok(None),
        Some(v) => Ok(Some(v.as_str().ok_or(format!("`{key}` must be a string"))?.to_string())),
    }
}

fn opt_int(v: Option<&toml::Value>, key: &str) -> Result<Option<i64>, String> {
    match v {
        None => Ok(None),
        Some(v) => Ok(Some(v.as_integer().ok_or(format!("`{key}` must be an integer"))?)),
    }
}

fn opt_str_array(v: Option<&toml::Value>, key: &str) -> Result<Option<String>, String> {
    let Some(v) = v else { return Ok(None) };
    let arr = v.as_array().ok_or(format!("`{key}` must be an array of strings"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        out.push(item.as_str().ok_or(format!("`{key}` must be an array of STRINGS"))?.to_string());
    }
    Ok(Some(toml_string_array(&out)))
}

/// Read a recorder profile file, turn it into rows, and decide about the active row. **Writes
/// nothing** — the caller does that, after a `--dry-run` has had its chance to stop.
///
/// # Errors
///
/// A `String` naming the file and what was wrong with it.
pub fn plan(settings_dir: &Path, file: &Path, name: &str) -> Result<Mirrored, String> {
    let text =
        std::fs::read_to_string(file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let body =
        rows_from_profile_text(name, &text).map_err(|e| format!("{}: {e}", file.display()))?;
    let subscriptions = body
        .subscriptions
        .iter()
        .map(|s| match (&s.family, &s.symbols) {
            (Some(f), _) => format!("{} family {f}", s.venue),
            (None, Some(syms)) => format!("{} symbols {syms}", s.venue),
            (None, None) => format!("{} (nothing)", s.venue),
        })
        .collect();
    // What the store already selects for this kind, if anything. An absent database is not an
    // error to ASK — the write below is where that refusal lives, and asking first would make a
    // dry run on an unmigrated box fail for the wrong reason.
    let already_active = read_profiles(&vike_secrets::db_path_in(settings_dir))
        .ok()
        .and_then(|p| p.active(ProfileKind::Recorder).map(|s| s.row.name.clone()));
    // ⚠ `in_force: None` ALWAYS, and it is a fact rather than a conservative guess: nothing selects
    // a recorder profile on any box — `--record`/`--record-profile` are explicit arguments with no
    // default and no environment variable. So this resolves to
    // `Withhold { NothingSelectedToday }` today, every time, and the report says so.
    let active = plan_active_row(None, already_active.as_deref(), name);
    Ok(Mirrored {
        stored: StoredProfile {
            row: ProfileRow {
                name: name.to_string(),
                kind: ProfileKind::Recorder,
                active: false,
                note: None,
            },
            mounts: Vec::new(),
            params: BTreeMap::new(),
            settings: BTreeMap::new(),
            recorder: Some(body),
        },
        active,
        subscriptions,
    })
}

/// Write a planned recorder body. Separate from [`plan`] so `--dry-run` can stop between them.
///
/// # Errors
///
/// A `String` from the store — which on a project with no database is the refusal
/// `vike-cli secrets migrate` is the answer to, and which inside a daemon's own mount namespace is
/// the read-only refusal `docs/decisions/0057`'s EROFS section measures.
pub fn write(
    settings_dir: &Path,
    mirrored: &Mirrored,
    now_utc: i64,
    asset_class_words: &[&str],
) -> Result<(), String> {
    let write = OperatorWrite::claim("vike-cli config mirror --recorder");
    store_profile(
        &vike_secrets::db_path_in(settings_dir),
        &mirrored.stored,
        &write,
        now_utc,
        asset_class_words,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "store = \"market_data/hist\"\n\n[[subscribe]]\nvenue = \"polymarket\"\n\
                          family = \"btc-updown-5m\"\nbackfill = \"off\"\n\n[[subscribe]]\n\
                          venue = \"binance\"\nsymbols = [\"BTCUSDT.P\"]\n\n[maintenance]\n\
                          interval_secs = 300\nmin_parts = 4\ntarget_mb = 384\n\
                          max_merge_rows = 1000000\n";

    #[test]
    fn a_real_profile_round_trips_and_is_therefore_storable() {
        let body = rows_from_profile_text("default", SAMPLE).expect("the sample must round-trip");
        assert_eq!(body.row.store, "market_data/hist");
        assert_eq!(body.subscriptions.len(), 2);
        assert_eq!(body.subscriptions[0].family.as_deref(), Some("btc-updown-5m"));
        assert_eq!(body.subscriptions[1].symbols.as_deref(), Some("[\"BTCUSDT.P\"]"));
        assert_eq!(
            body.subscriptions[1].backfill, None,
            "an ABSENT key stays absent — a stored default would break the round trip"
        );
        assert_eq!(body.row.retention_days, None, "…and so does an absent [maintenance] key");
        assert_eq!(body.row.alert_webhooks, None, "…and a whole absent table");
    }

    /// **The unknown-key refusal is the write-path half of `deny_unknown_fields`.** A key this
    /// migration cannot carry must never be dropped: dropping `retention_days` because it was
    /// misspelled leaves an operator believing a retention policy is armed.
    #[test]
    fn an_unknown_key_refuses_the_whole_profile() {
        let e = rows_from_profile_text("default", "store = \"x\"\nretention_days = 30\n")
            .expect_err("a top-level unknown key must refuse");
        assert!(e.contains("retention_days"), "names the key: {e}");
        assert!(e.contains("Nothing was written"), "{e}");

        let e = rows_from_profile_text(
            "default",
            "store = \"x\"\n\n[maintenance]\nretention_day = 30\n",
        )
        .expect_err("a misspelled nested key must refuse");
        assert!(e.contains("maintenance.retention_day"), "names the PATH, not just the leaf: {e}");

        let e = rows_from_profile_text(
            "default",
            "store = \"x\"\n\n[[subscribe]]\nvenue = \"binance\"\nsymbol = [\"BTCUSDT\"]\n",
        )
        .expect_err("the singular `symbol` is the measured near-miss and must refuse");
        assert!(e.contains("subscribe.symbol"), "{e}");
    }

    /// **THE FENCE ITSELF, reached and proven.**
    ///
    /// ⚠ **THE FIRST VERSION OF THIS TEST COULD NOT FAIL FOR ITS STATED REASON, AND A MUTATION RUN
    /// CAUGHT IT.** It planted a `[subscribe]` TABLE where an array of tables belongs — which is
    /// refused by the PARSER, several returns before the round-trip comparison. Replacing
    /// `if back != doc` with `if false && back != doc` in production left that test green. What is
    /// planted now is a difference only the comparison can see.
    ///
    /// An EMPTY `[maintenance]` table is that difference, and it is the honest one: every key in it
    /// is absent, so the rows carry nothing, so [`render_recorder_toml`] emits no `[maintenance]`
    /// header at all — and the rendered document is therefore NOT the document that was read. The
    /// two are equivalent to `vike_recorder::config` (an empty table and an absent one both mean
    /// "defaults"), which is exactly why this case is worth pinning: **the fence is conservative,
    /// and refuses a shape that would in fact have been harmless.** That cost is accepted rather
    /// than engineered away, because the alternative is a fence that reasons about which
    /// differences matter — on a document that decides which venue feeds open.
    ///
    /// The refusal prints BOTH documents, so the operator's remedy (delete the empty header) is
    /// visible in the message rather than requiring them to know this rule.
    #[test]
    fn a_body_that_does_not_reproduce_its_profile_refuses_the_write() {
        let e = rows_from_profile_text("default", "store = \"x\"\n\n[maintenance]\n")
            .expect_err("an empty [maintenance] table does not survive the round trip");
        assert!(e.contains("REFUSING"), "{e}");
        assert!(e.contains("do not reproduce the profile"), "{e}");
        assert!(e.contains("the profile as given"), "the message shows what was read: {e}");
        assert!(e.contains("as the rows render it"), "…and what would have been stored: {e}");
        assert!(e.contains("Nothing was written"), "{e}");
        // …and the same for the other optional table, so the property is the RULE and not one
        // hand-picked key.
        assert!(
            rows_from_profile_text("default", "store = \"x\"\n\n[alerting]\n").is_err(),
            "an empty [alerting] table is the same case"
        );
    }

    /// The parser's own refusals fire BEFORE the fence, and that ordering is the thing the fence
    /// test above got wrong once. Kept as its own test so neither can be mistaken for the other.
    #[test]
    fn a_wrongly_shaped_table_is_refused_by_the_parser_not_by_the_fence() {
        let e = rows_from_profile_text("default", "store = \"x\"\n\n[subscribe]\nvenue = \"a\"\n")
            .expect_err("a `[subscribe]` table is not an array of tables");
        assert!(e.contains("array of tables"), "{e}");
        assert!(!e.contains("REFUSING"), "this is the PARSER's refusal, not the fence's: {e}");
    }

    /// A profile with no `store` is refused, exactly as `RecorderProfile::from_toml` refuses one —
    /// the key has no `#[serde(default)]` there either.
    #[test]
    fn a_profile_without_a_store_is_refused() {
        let e = rows_from_profile_text("default", "[[subscribe]]\nvenue = \"binance\"\n")
            .expect_err("`store` is required");
        assert!(e.contains("store"), "{e}");
    }
}
