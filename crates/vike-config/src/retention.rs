//! **`config.recorder_retention_days_by_kind`'s grammar** — the one parser both the write and the load
//! run.
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
//! A kind's word covers its `source=` lanes too. A lane word ([`RETENTION_LANES`]) gives one lane days
//! of its own, instead of its kind's: `quote:365,quote@recorder:30` keeps the archive a year and the
//! recorder's tape a month, and `quote@recorder:30` alone prunes the tape and leaves the archive to the
//! scalar. A lane word changes nothing for the sourceless series. `bar@recorder:30` likewise keeps a month
//! of the closed live bars the recorder keeps (`flags.record_bars`), and with them their commit keys, one
//! per flush: a key lives exactly as long as its part, so this word is what bounds the lane's keys as well
//! as its disk. `bar:30` would prune the venue's backfilled and imported candles too.
//!
//! ⚠ **Account data is not tape.** The words are the store's own, `vike_model::StoreKindId`'s, and a
//! kind's CLASS decides: every [`vike_model::StorePlane::Market`] kind may be named, and every
//! [`vike_model::StorePlane::Account`] kind (the execution history `exec_fill`, `exec_order`,
//! `exec_funding`, `exec_forecast`, and `equity`) is REFUSED by name, by the owner's rulings that
//! nothing on this row may expire the operator's own record (2026-11-21: `equity` too). No list is
//! written here: a new kind joins a side when its author decides its plane
//! (`crates/vike-model/src/store_plane.rs`'s `StoreKindId::plane`, an exhaustive match).
//!
//! The READER is the data daemon (`crates/vike-datahub/src/datahub_cli/recording.rs`), which hands the
//! parsed [`RetentionEntry`] rows to its maintenance pass; every refusal here applies at the write
//! and at the load (`Config::apply`), so a stored row the reader cannot parse does not exist.

use vike_model::{StoreKindId, StorePlane};

/// The `kind@source` words: the recorder's two lanes, its quote tape (decision 0193) and its closed live bars
/// (`flags.record_bars`, decision 0268's amendment of 2026-11-17). A lane's kind half is a market kind (a test
/// below holds it); that its lane half is a lane the kind's producers declare is the data daemon's to hold, since
/// this crate cannot name the store's producer table. Like every word here, a lane word has no default: unnamed,
/// the lane keeps its kind's days, and with no row at all the scalar (absent by default) keeps everything.
pub const RETENTION_LANES: [&str; 2] = ["bar@recorder", "quote@recorder"];

/// The fewest days a kind may be kept for. `0` would delete everything older than now, which for a
/// live series is the tape being written.
pub const MIN_RETENTION_DAYS: u32 = 1;

/// The longest an offending token is repeated in a refusal, in characters. A refusal quotes the
/// entry that failed so the operator can find it, but a value pasted into the wrong key must not be
/// printed whole.
const SHOWN_CHARS: usize = 24;

/// The kinds a retention may name, in `StoreKindId::ALL`'s (alphabetical) order: every market kind.
fn retainable() -> impl Iterator<Item = StoreKindId> {
    StoreKindId::ALL.into_iter().filter(|kind| kind.plane() == StorePlane::Market)
}

/// `s`, cut to [`SHOWN_CHARS`] characters with an ellipsis when it was longer.
fn shown(s: &str) -> String {
    if s.chars().count() <= SHOWN_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(SHOWN_CHARS).collect();
    format!("{head}…")
}

/// One entry of the row: a market kind, the lane its days are for, and the days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionEntry {
    /// The store kind the entry names; always a [`StorePlane::Market`] kind.
    pub kind: StoreKindId,
    /// `None` for a kind word (its sourceless series and every lane without a word of its own), the
    /// lane (`Some("recorder")`) for a [`RETENTION_LANES`] word.
    pub lane: Option<&'static str>,
    /// The days its parts are kept, at least [`MIN_RETENTION_DAYS`].
    pub days: u32,
}

/// Parse `config.recorder_retention_days_by_kind`.
///
/// `Ok` holds one [`RetentionEntry`] per entry, in the order written; empty for the empty string,
/// which is the row's "no per-kind retention" spelling. `Err` is a refusal sentence that names which
/// rule the value broke; it never echoes more than a couple of dozen characters of the offending
/// entry.
///
/// Grammar, every rule a refusal rather than a repair:
/// - entries are `kind:days`, joined by `,`; no whitespace anywhere, no empty entry;
/// - `kind` is a market kind's word or a [`RETENTION_LANES`] word, lower case; an account kind is
///   refused by name, and any other word is unknown;
/// - `days` is a whole number written in digits, from [`MIN_RETENTION_DAYS`] to `u32::MAX`;
/// - a kind (or lane) appears at most once, even with the same days.
pub fn parse_retention_days_by_kind(value: &str) -> Result<Vec<RetentionEntry>, String> {
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
    let mut parsed: Vec<RetentionEntry> = Vec::new();
    for entry in value.split(',') {
        if entry.is_empty() {
            return Err("has an empty entry — a leading, trailing or doubled comma; write \
                        `kind:days` entries joined by single commas"
                .to_string());
        }
        let Some((token, days)) = entry.split_once(':') else {
            return Err(format!(
                "the entry `{}` is not `kind:days` — every entry is a store kind, a colon and a \
                 whole number of days, `book:14`",
                shown(entry)
            ));
        };
        let (word, kind, lane) = kind_word(token)?;
        let days = days_of(word, days)?;
        if parsed.iter().any(|seen| (seen.kind, seen.lane) == (kind, lane)) {
            return Err(format!(
                "`{word}` is named twice — a kind has one retention, so the second entry would \
                 either repeat the first or contradict it; write it once"
            ));
        }
        parsed.push(RetentionEntry { kind, lane, days });
    }
    Ok(parsed)
}

/// The canonical word of `token`, its kind and lane, or the refusal for it.
fn kind_word(token: &str) -> Result<(&'static str, StoreKindId, Option<&'static str>), String> {
    if let Some(kind) = StoreKindId::from_word(token) {
        if kind.plane() == StorePlane::Account {
            return Err(format!(
                "`{}` cannot be given a retention — it is account data, the operator's own record, \
                 not tape, and nothing on this row is allowed to expire it. Leave it out",
                kind.word()
            ));
        }
        return Ok((kind.word(), kind, None));
    }
    if let Some(lane_word) = RETENTION_LANES.into_iter().find(|w| *w == token) {
        let (kind, lane) = lane_word.split_once('@').expect("a lane word is `kind@lane`");
        let kind = StoreKindId::from_word(kind).expect("a lane word's kind is a store kind");
        return Ok((lane_word, kind, Some(lane)));
    }
    let words = || retainable().map(StoreKindId::word).chain(RETENTION_LANES);
    let lower_case_hint = if words().any(|k| k.eq_ignore_ascii_case(token)) {
        " (kind words are lower case)"
    } else {
        ""
    };
    Err(format!(
        "`{}` is not a store kind a retention can name{lower_case_hint}; the kinds are {}, and the \
         lanes {}",
        shown(token),
        retainable().map(StoreKindId::word).collect::<Vec<_>>().join(", "),
        RETENTION_LANES.join(", ")
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
