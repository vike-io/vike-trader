//! **`config.recorder_retention_days_by_kind`'s grammar** — the one parser both the write and the load
//! run, and the one place the words it accepts are spelled.
//!
//! The row is `kind:days` entries joined by commas, `book:14,depth:14,trade:90`. It makes the data
//! daemon's maintenance pass delete every part of every series of that kind whose newest row is
//! older than that many days, whatever wrote the series: the recorder's live tape AND a series a
//! backfill filled with history. ⚠ **Deletion is irreversible.** A pruned part is unlinked from
//! disk, and the only way back is to fetch the window again from its provider.
//!
//! Absent, or the empty string, is today's behaviour: the recorder profile's scalar
//! `[maintenance].retention_days` (itself absent by default, which keeps everything) decides every
//! kind. A kind this row names uses its own days instead of the scalar; a kind it does not name keeps
//! the scalar.
//!
//! ⚠ **Execution history is not tape.** `exec_fill` and `exec_order` (the kinds the journal
//! materializer writes: the operator's own trade record) are REFUSED by name, by the owner's ruling
//! that nothing on this row may expire them. `exec_funding`, the account's realized funding
//! payments, is refused with them as the same kind of record; the owner ruled on the first two only,
//! so lifting that third refusal is a one-word edit of [`RETENTION_REFUSED_KINDS`] and
//! [`RETENTION_KINDS`] if it was not meant.
//!
//! This crate cannot name `vike_data::store::store_kind::StoreKindId` (it depends on `vike-model` and
//! `vike-secrets` only), so the kind words are DATA here and
//! `crates/vike-datahub/src/datahub_cli_tests.rs`'s `the_retention_grammar_and_the_store_kinds_agree`
//! holds the two lists equal to the enum. The READER is
//! `crates/vike-datahub/src/datahub_cli/recording.rs`'s `retention_by_kind_of`, which hands the parsed
//! pairs to `MaintenanceConfig`; every refusal here applies at the write and at the load
//! (`Config::apply`), so a stored row the reader cannot parse does not exist.

/// The stored kinds a retention may name: every `StoreKindId` word except the three execution kinds
/// in [`RETENTION_REFUSED_KINDS`], in the enum's alphabetical order.
pub const RETENTION_KINDS: [&str; 10] = [
    "bar",
    "book",
    "chain",
    "cohort",
    "depth",
    "equity",
    "perp_metrics",
    "properties",
    "quote",
    "trade",
];

/// The stored kinds a retention REFUSES to name: the operator's execution history. See the module doc.
pub const RETENTION_REFUSED_KINDS: [&str; 3] = ["exec_fill", "exec_funding", "exec_order"];

/// The fewest days a kind may be kept for. `0` would delete everything older than now, which for a
/// live series is the tape being written.
pub const MIN_RETENTION_DAYS: u32 = 1;

/// The longest an offending token is repeated in a refusal, in characters. A refusal quotes the
/// entry that failed so the operator can find it, but a value pasted into the wrong key must not be
/// printed whole.
const SHOWN_CHARS: usize = 24;

/// `s`, cut to [`SHOWN_CHARS`] characters with an ellipsis when it was longer.
fn shown(s: &str) -> String {
    if s.chars().count() <= SHOWN_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(SHOWN_CHARS).collect();
    format!("{head}…")
}

/// Parse `config.recorder_retention_days_by_kind`.
///
/// `Ok` holds the `(kind, days)` pairs in the order written: empty for the empty string, which is
/// the row's "no per-kind retention" spelling. `Err` is a refusal sentence that names which rule the
/// value broke; it never echoes more than a couple of dozen characters of the offending entry.
///
/// Grammar, every rule a refusal rather than a repair:
/// - entries are `kind:days`, joined by `,`; no whitespace anywhere, no empty entry;
/// - `kind` is a word of [`RETENTION_KINDS`], lower case; the three in [`RETENTION_REFUSED_KINDS`]
///   are refused by name, and any other word is unknown;
/// - `days` is a whole number written in digits, from [`MIN_RETENTION_DAYS`] to `u32::MAX`;
/// - a kind appears at most once, even with the same days.
pub fn parse_retention_days_by_kind(value: &str) -> Result<Vec<(&'static str, u32)>, String> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.chars().any(char::is_whitespace) {
        return Err(
            "contains whitespace — write the entries as `kind:days` joined by commas with no \
             spaces, `book:14,trade:90`"
                .to_string(),
        );
    }
    let mut parsed: Vec<(&'static str, u32)> = Vec::new();
    for entry in value.split(',') {
        if entry.is_empty() {
            return Err("has an empty entry — a leading, trailing or doubled comma; write \
                        `kind:days` entries joined by single commas"
                .to_string());
        }
        let Some((kind, days)) = entry.split_once(':') else {
            return Err(format!(
                "the entry `{}` is not `kind:days` — every entry is a store kind, a colon and a \
                 whole number of days, `book:14`",
                shown(entry)
            ));
        };
        let kind = kind_word(kind)?;
        let days = days_of(kind, days)?;
        if parsed.iter().any(|(seen, _)| *seen == kind) {
            return Err(format!(
                "`{kind}` is named twice — a kind has one retention, so the second entry would \
                 either repeat the first or contradict it; write it once"
            ));
        }
        parsed.push((kind, days));
    }
    Ok(parsed)
}

/// The canonical word of `token`, or the refusal for it.
fn kind_word(token: &str) -> Result<&'static str, String> {
    if let Some(exec) = RETENTION_REFUSED_KINDS.iter().find(|k| **k == token) {
        return Err(format!(
            "`{exec}` cannot be given a retention — execution history is the operator's own trade \
             record, not tape, and nothing on this row is allowed to expire it. Leave it out"
        ));
    }
    if let Some(known) = RETENTION_KINDS.iter().copied().find(|k| *k == token) {
        return Ok(known);
    }
    let lower_case_hint = if RETENTION_KINDS.iter().any(|k| k.eq_ignore_ascii_case(token)) {
        " (kind words are lower case)"
    } else {
        ""
    };
    Err(format!(
        "`{}` is not a store kind a retention can name{lower_case_hint}; the kinds are {}",
        shown(token),
        RETENTION_KINDS.join(", ")
    ))
}

/// `days` as a count for `kind`, or the refusal for it.
fn days_of(kind: &str, days: &str) -> Result<u32, String> {
    if days.is_empty() || !days.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "the days of `{kind}` must be a whole number written in digits, with no sign or \
             fraction; got `{}`",
            shown(days)
        ));
    }
    let Ok(n) = days.parse::<u32>() else {
        return Err(format!(
            "the days of `{kind}` do not fit a count (the most is {}); write a smaller number",
            u32::MAX
        ));
    };
    if n < MIN_RETENTION_DAYS {
        return Err(format!(
            "`{kind}:0` would delete every `{kind}` part older than NOW, which is all of the \
             tape being written. The smallest retention is {MIN_RETENTION_DAYS} day; leave the \
             kind out to keep it (the default)"
        ));
    }
    Ok(n)
}

#[path = "retention_tests.rs"]
#[cfg(test)]
mod retention_tests;
