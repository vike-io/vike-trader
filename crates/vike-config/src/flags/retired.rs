//! The FILE-key tombstones: the two deleted-key tables and the refusal and warning each produces.

use std::path::Path;

#[cfg(doc)]
use super::Flags;
use super::RECORD_DVOL_ENV;
use crate::error::ConfigError;

// -------------------------------------------------------------------------------------------
// The FILE-key tombstones
// -------------------------------------------------------------------------------------------

/// **Six `flags.toml` keys that were DELETED while the variable each mirrored was still LIVE.**
///
/// Each row is `(key, variable)`. [`Flags::apply`] refuses the key by name and names the variable,
/// the [`crate::preferences::Preferences`] `rate_utilization` shape — a declared, validated,
/// `config show`-displayed key that nothing acted on is worse than no key at all, so the answer is
/// deletion, and a deletion an operator can SEE is the difference between a fixed defect and a
/// silently-dropped setting.
///
/// ⚠ **This table and [`crate::REMOVED_ENV`] answer different questions, and that distinction is
/// the whole of why it exists separately.** `REMOVED_ENV` refuses a VARIABLE at startup because the
/// operator's belief in it became false when its last reader went; this table refuses a FILE key
/// whose field nothing downstream consumed. None of the six variables is read: decision 0095
/// retired them all.
///
/// The day a variable here is retired, it joins `REMOVED_ENV` and its row here STAYS — the refusal
/// then names the variable's new home instead of the variable. Decision 0095 did this for all six:
/// `poly_presubmit_register` and `poly_rate_gate` (now `venue.polymarket.*` rows),
/// `bybit_fast_exec` and `binance_trade_lite_fill` (now `venue.bybit.fast_exec` and
/// `venue.binance.trade_lite_fill`), and `poly_chain_watch` and `poly_chain_proxy`, which have NO
/// new home: the chain watcher their variables configured is started by nothing, so its values are
/// parameters now (D4), and the refusal says so rather than offering a row.
pub const REMOVED_FLAG_KEYS: &[(&str, &str)] = &[
    ("poly_presubmit_register", "POLY_PRESUBMIT_REGISTER"),
    ("poly_rate_gate", "POLY_RATE_GATE"),
    ("poly_chain_watch", "POLY_CHAIN_WATCH"),
    ("poly_chain_proxy", "POLY_CHAIN_PROXY"),
    ("bybit_fast_exec", "VIKE_BYBIT_FAST_EXEC"),
    ("binance_trade_lite_fill", "VIKE_BINANCE_TRADE_LITE_FILL"),
];

/// The refusal one [`REMOVED_FLAG_KEYS`] row produces — the whole operator-facing sentence, naming
/// the file, the key, the value they wrote, and where the setting lives now: the settings row that
/// replaced the variable, or that nothing reads it (D4). Decision 0095 retired every variable in the
/// table, so the last arm — "export the variable instead", for a variable an adapter still reads —
/// answers for no row today.
///
/// A [`ConfigError::Value`] rather than a bespoke variant because that is the shape every other
/// "your file says something this loader will not do" failure already takes, and
/// `crates/vike-config/tests/` reads all of them the same way.
pub(super) fn removed_flag_key(file: &Path, key: &str, written: bool) -> ConfigError {
    let var = REMOVED_FLAG_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map_or("its environment variable", |(_, v)| *v);
    let message = match crate::REMOVED_ENV.iter().find(|r| r.var == var) {
        // Decision 0095: once the VARIABLE is retired too, "export it instead" would send the
        // operator to a spelling that refuses startup, and no adapter reads that variable any more.
        // `crate::REMOVED_ENV`'s row names the new home.
        Some(crate::RemovedSetting { key: Some(home), .. }) => format!(
            "{written} is no longer a flag — NOTHING read the resolved value; this key only ever \
             mirrored {var} into a field with no consumer. {var} was retired too (decision \
             0095): the setting is `{home}` in the settings database. Delete this row and run \
             `vike-cli config set {home} {}` instead",
            switch_value(home, written)
        ),
        // …and one retired with NO new home (D4: its code is started by nothing, so its value is
        // a parameter now). Its `why` is the whole answer.
        Some(r) => format!(
            "{written} is no longer a flag — NOTHING read the resolved value; this key only ever \
             mirrored {var} into a field with no consumer. {var} was retired too: {}. Delete \
             this row",
            r.why
        ),
        None => format!(
            "{written} is no longer a flag — NOTHING read the resolved value. The behaviour is \
             still there and still reachable: it is gated by the venue adapter's own read of \
             {var}, which this key only ever mirrored into a field with no consumer. Delete the \
             key and export {var}=1 instead"
        ),
    };
    ConfigError::Value { file: file.to_path_buf(), key: key.to_string(), message }
}

/// The value `vike-cli config set <home>` takes for a switch that was `on` — `1`/`0` for an
/// exact-one venue field, `true`/`false` otherwise.
fn switch_value(home: &str, on: bool) -> &'static str {
    let exact_one = home
        .strip_prefix("venue.")
        .and_then(|rest| rest.split_once('.'))
        .and_then(|(venue, field)| vike_model::venues::venue_fields::venue_field(venue, field))
        .is_some_and(|f| f.grammar == vike_model::venues::venue_fields::FieldGrammar::ExactOne);
    match (exact_one, on) {
        (true, true) => "1",
        (true, false) => "0",
        (false, true) => "true",
        (false, false) => "false",
    }
}

/// **Flag keys that were DELETED with NO live variable to redirect an operator to** — the THIRD
/// tombstone family, and it exists because neither of the other two tells the truth about this
/// class.
///
/// Each row is `(key, variable, what_survives)`. [`Flags::apply`] refuses the key by name;
/// [`dead_flag_env_ignored`] is the loader's matching one-line WARNING for the variable half.
///
/// ⚠ **Why not [`REMOVED_FLAG_KEYS`]?** That table's refusal sends the operator to a LIVE home:
/// the variable its venue adapter still reads, or, where decision 0095 retired the variable, the
/// settings row that replaced it. Here the variable's ONLY reader was this type's own `apply_env`,
/// which goes with the field — so that refusal would send an operator to a spelling that has just
/// become as dead as the one they wrote.
///
/// ⚠ **Why not [`crate::REMOVED_ENV`]?** That table is a HARD STARTUP REFUSAL, and it is right for
/// a variable whose removal made an operator's belief false — a ceiling they think is armed. A
/// variable that configured nothing before the deletion and configures nothing after it makes no
/// belief false by going, and refusing it would stop a correct daemon dead over a spelling that
/// changes nothing. The record that ordered this deletion
/// (`docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`) names both misfits
/// as the reason a deletion here is *not free*; this table is the answer to that, not a way around
/// it.
///
/// ⚠ **Decision 0095 took the opposite call for VENUE variables, on purpose.** Its owner ruling is
/// that no environment variable configures a venue, and it refuses every retired venue variable —
/// including the ones only code nothing starts ever read (the settlement pollers' switches, the
/// chain watcher's settings, the DVOL cadence), which configured nothing in a running process
/// either. The argument there is the day something starts that code: a leftover variable must be
/// named then, not silently dropped. `VIKE_RECORD_DVOL` below stays a warning — it is the recorder
/// FLAG's spelling, deleted under 0057 before that ruling, not a venue setting it retired.
///
/// So the two halves are deliberately ASYMMETRIC: the FILE key is a refusal (`deny_unknown_fields`
/// would refuse it anyway — the row is what makes the refusal NAME the key and say what happened),
/// and the VARIABLE is a warning.
///
/// **What each row must carry**: the surviving symbols, so the deletion is a pointer to the
/// feature rather than an erasure of it. A row here is not a claim that the feature was removed —
/// it is the claim that nothing READS this setting, which is a different and much narrower thing.
pub const DEAD_FLAG_KEYS: &[(&str, &str, &str)] = &[(
    "record_dvol",
    RECORD_DVOL_ENV,
    // The feature is intact and UNMOUNTED: both symbols are still exported, still take the enable
    // gate as a parameter, and have no caller outside deribit's own `#[cfg(test)]` module. What a
    // re-mounting root must supply is NOT a deribit connection (the channel is public and keyless
    // and opens its own socket) but a composed `LiveDataSink` and a concrete store to record into
    // — `vike-tradehub`'s `record-feeds` feature built both until #2093 deleted it; the message
    // below says so in the past tense.
    "`vike_deribit::DvolRecorder::with_cadence` and `vike_deribit::spawn_deribit_dvol_feed` \
     still exist and still take the enable gate as a parameter; what is missing is a composition \
     root that mounts the feed with a composed LiveDataSink and a concrete store (the `record-feeds` \
     build of vike-tradehub had both until #2093 deleted it)",
)];

/// The refusal one [`DEAD_FLAG_KEYS`] row produces — naming the file, the key, the value written,
/// the variable that is equally dead, and the symbols that survive.
pub(super) fn dead_flag_key(file: &Path, key: &str, written: bool) -> ConfigError {
    let (var, survives) = DEAD_FLAG_KEYS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map_or(("its environment variable", ""), |(_, v, s)| (*v, *s));
    ConfigError::Value {
        file: file.to_path_buf(),
        key: key.to_string(),
        message: format!(
            "{written} is no longer a flag — NOTHING read it, on EITHER spelling. Exporting {var} \
             instead does nothing either: that variable's only reader was this loader, and it went \
             with the key. The FEATURE is unmounted rather than removed — {survives}. Delete the \
             key; it was changing nothing"
        ),
    }
}

/// The one-line WARNING the loader raises for a SET [`DEAD_FLAG_KEYS`] variable — returned as data
/// on [`crate::Settings::warnings`] for the reason every other loader resolution is (this crate
/// carries no `tracing` dependency, and the binaries emit it once a subscriber exists).
///
/// A warning rather than a refusal: see [`DEAD_FLAG_KEYS`]'s *why not `REMOVED_ENV`* paragraph.
#[must_use]
pub fn dead_flag_env_ignored(var: &str) -> String {
    let survives = DEAD_FLAG_KEYS.iter().find(|(_, v, _)| *v == var).map_or("", |(_, _, s)| *s);
    format!(
        "{var} is set in the environment and configures NOTHING — the flag it fed was deleted \
         because nothing in the tree read it, on either spelling. The feature is unmounted rather \
         than removed: {survives}. Unset the variable"
    )
}
