//! The ONE CSV bar decoder (decision 0268, verdict 7): a file of OHLCV candles read into [`Bar`]s, or
//! refused with the line and the rule. Two doors read it, the `csv-bars` import (`vike-backfill`) and a
//! backtest's file read (`crate::HistoryStoreCsv`), so a file imports exactly when it can be read, and a
//! line is refused for the same reason by both. An ambiguous file is refused, never guessed.
//!
//! The rules, each a [`CsvBarsRule`]:
//!
//! - **Columns** ([`CsvBarsRule::Header`], [`CsvBarsRule::Cells`]): the first line is a header, and each field is
//!   found by its accepted names in any case ([`CsvBarsColumns::Header`], [`HEADER_NAMES`]) or by an explicit
//!   mapping ([`CsvBarsColumns::Named`]). Other columns are ignored; every row has the header's number of cells.
//!   `time` and `close` are always read. `open`, `high` and `low` are read together or not at all: a file
//!   naming none of them holds one value per row, and each of the three is its close. `volume` may be
//!   absent, and is then 0 (decision 0272).
//! - **Form** ([`CsvBarsRule::Format`]): the delimiter, decimal mark, thousands separator, time pattern and
//!   zone are EXPLICIT options ([`CsvBarsFormat`], built from [`CsvBarsFormatWords`]); nothing reads the
//!   machine's locale. The default is `,`, `.`, no separator and integer epoch times, and a file in another form
//!   (a header split by `;` or a tab, a number with a comma decimal or grouped thousands, a text time) is
//!   refused naming the option that reads it.
//! - **Time** ([`CsvBarsRule::Time`], [`CsvBarsRule::Unit`]): by default an integer epoch in seconds,
//!   milliseconds or microseconds, which is UTC by definition, the unit read off the values: each unit
//!   maps a value to an instant in 1971-01-01..2100-01-01 UTC, and the three ranges do not overlap, so a
//!   value names one unit or none. A file whose rows name two units fits two, and which one is meant is
//!   not in it: refused. With a `time` pattern, a text time in that pattern exactly, in the format's
//!   zone; a wall-clock time a zone's clock passed twice or never is refused.
//! - **Order** ([`CsvBarsRule::Order`], [`CsvBarsRule::Duplicate`]): times strictly increase.
//! - **One interval** ([`CsvBarsRule::Interval`]): the step is the commonest distance between consecutive
//!   rows (the shorter on a tie). Every distance is a whole number of steps, so a missing bar is a gap,
//!   which is allowed; a distance that is not is a second interval.
//! - **Values** ([`CsvBarsRule::Number`], [`CsvBarsRule::Ohlc`], [`CsvBarsRule::Volume`]): finite numbers,
//!   `low <= open, close <= high` on every row, and a volume that is not negative.
//!
//! What it does not read, on purpose: a headerless file and a quoted cell holding the delimiter. Each
//! fails a rule above with its line rather than being guessed at.

use std::collections::BTreeMap;
use std::fmt;

use vike_marketdata::Bar;

// The format is its own file (its time pattern and zones are most of it), and the words it is built
// from another (a profile's published schema reads that file as text: one struct, its fields only).
// Both types are this module's vocabulary and both files are private, so each has the one name
// `csv_bars::X`.
mod format;
mod format_words;
pub use format::CsvBarsFormat;
pub use format_words::CsvBarsFormatWords;

/// Each field and the header names [`CsvBarsColumns::Header`] accepts for it, compared in any case. The first
/// name is the field's own, the spelling [`CsvBarsColumns::parse`] and every refusal use.
pub const HEADER_NAMES: [(&str, &[&str]); 6] = [
    ("time", &["time", "timestamp", "ts", "open_time"]),
    ("open", &["open", "o"]),
    ("high", &["high", "h"]),
    ("low", &["low", "l"]),
    ("close", &["close", "c"]),
    ("volume", &["volume", "vol", "v"]),
];

/// 1971-01-01T00:00:00Z in epoch ms: the earliest instant a time may name. A year past the epoch, so the
/// three units' ranges are disjoint (`csv_bars_tests.rs` checks both bounds against the calendar).
const EARLIEST_MS: i64 = 31_536_000_000;
/// 2100-01-01T00:00:00Z in epoch ms: the first instant past the range.
const END_MS: i64 = 4_102_444_800_000;
// The ranges are disjoint: the largest value in seconds is below the smallest in ms, and the largest in
// ms below the smallest in µs. So a value names one unit or none, and `CsvBarsTimeUnit::of` never chooses.
const _: () = assert!((END_MS - 1) / 1_000 < EARLIEST_MS && END_MS - 1 < EARLIEST_MS * 1_000);

/// How the fields are found in the header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsvBarsColumns {
    /// Each field is the one column whose header is among its [`HEADER_NAMES`], in any case.
    Header,
    /// Each field is the column whose header is exactly this name.
    Named(CsvBarsColumnNames),
}

/// The header name of each field, for [`CsvBarsColumns::Named`]. `open`, `high` and `low` are all `Some` or
/// all `None` ([`CsvBarsColumns::parse`] holds that): `None` is a file of one value per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvBarsColumnNames {
    pub time: String,
    pub open: Option<String>,
    pub high: Option<String>,
    pub low: Option<String>,
    pub close: String,
    /// `None`: the file has no volume, and every bar's is 0.
    pub volume: Option<String>,
}

impl CsvBarsColumns {
    /// A mapping as an operator writes it: `header` for [`CsvBarsColumns::Header`], else `field=<name>` pairs
    /// in any order, each field once: `time` and `close` always, `open`, `high` and `low` all three or
    /// none (a file of one value per row), and `volume` when the file has one.
    ///
    /// # Errors
    /// The sentence naming the pair or the field that is wrong.
    pub fn parse(spec: &str) -> Result<Self, String> {
        if spec.trim() == "header" {
            return Ok(CsvBarsColumns::Header);
        }
        let mut got: [Option<String>; 6] = Default::default();
        for pair in spec.split(',') {
            let Some((field, name)) = pair.split_once('=') else {
                return Err(format!("{pair:?} is not `field=header-name`"));
            };
            let (field, name) = (field.trim(), name.trim());
            let Some(i) = HEADER_NAMES.iter().position(|(f, _)| *f == field) else {
                return Err(format!("{field:?} is not a field: the six are {}", field_list()));
            };
            if name.is_empty() {
                return Err(format!("{field} is mapped to no header name"));
            }
            if got[i].replace(name.to_string()).is_some() {
                return Err(format!("{field} is mapped twice"));
            }
        }
        let [time, open, high, low, close, volume] = got;
        let (Some(time), Some(close)) = (time, close) else {
            return Err(
                "the mapping must name time and close: they are read from every file".into()
            );
        };
        ohl_together([open.is_some(), high.is_some(), low.is_some()], "the mapping")?;
        Ok(CsvBarsColumns::Named(CsvBarsColumnNames { time, open, high, low, close, volume }))
    }
}

/// `Ok` when `open`, `high` and `low` (in that order) are all present or all absent, else the sentence
/// naming which `whose` (the mapping, the header) gives and which it lacks.
fn ohl_together(present: [bool; 3], whose: &str) -> Result<(), String> {
    let names = ["open", "high", "low"];
    let given: Vec<&str> = names.iter().zip(present).filter(|(_, p)| *p).map(|(n, _)| *n).collect();
    if given.is_empty() || given.len() == names.len() {
        return Ok(());
    }
    let lacking: Vec<&str> =
        names.iter().zip(present).filter(|(_, p)| !p).map(|(n, _)| *n).collect();
    Err(format!(
        "{whose} names {} but not {}: a bar names open, high and low together, and a file of one \
         value per row names none of them (each is then the close)",
        given.join(", "),
        lacking.join(", ")
    ))
}

/// The unit a file's times were written in, read off its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvBarsTimeUnit {
    Seconds,
    Millis,
    Micros,
    /// Text times in the format's `time` pattern.
    Text,
}

impl CsvBarsTimeUnit {
    /// The unit's short spelling: `s`, `ms`, `µs` or `text`.
    pub fn as_str(self) -> &'static str {
        match self {
            CsvBarsTimeUnit::Seconds => "s",
            CsvBarsTimeUnit::Millis => "ms",
            CsvBarsTimeUnit::Micros => "µs",
            CsvBarsTimeUnit::Text => "text",
        }
    }

    /// The one unit that places `v` in 1971..2100 UTC, or `None`. The ranges are disjoint, so the first
    /// match is the only one.
    fn of(v: i64) -> Option<Self> {
        [CsvBarsTimeUnit::Seconds, CsvBarsTimeUnit::Millis, CsvBarsTimeUnit::Micros]
            .into_iter()
            .find(|u| {
                let ms = match u {
                    CsvBarsTimeUnit::Seconds => v.checked_mul(1_000),
                    CsvBarsTimeUnit::Millis => Some(v),
                    CsvBarsTimeUnit::Micros => Some(v.div_euclid(1_000)),
                    CsvBarsTimeUnit::Text => None,
                };
                ms.is_some_and(|ms| (EARLIEST_MS..END_MS).contains(&ms))
            })
    }
}

/// A decoded file: its bars, ts-ascending, the unit its times were in, and its step in ms (`None` for a
/// file of one bar, which has no step to read).
#[derive(Debug, Clone, PartialEq)]
pub struct CsvBars {
    pub bars: Vec<Bar>,
    pub unit: CsvBarsTimeUnit,
    pub step_ms: Option<i64>,
}

/// The rule a refused line breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvBarsRule {
    /// The file holds no header, or no rows under it.
    Empty,
    /// The header is split by another delimiter than the format's.
    Format,
    /// A field is in no header column, in two, or two fields name one column.
    Header,
    /// A row's cell count is not the header's.
    Cells,
    /// The time is not an integer epoch (a text time, a fraction, an offset), or not a whole ms.
    Time,
    /// The time is in no unit's range, or in another unit than the file's first row.
    Unit,
    /// The time is before the previous row's.
    Order,
    /// The time repeats the previous row's.
    Duplicate,
    /// The distance from the previous row is not a whole number of the file's step.
    Interval,
    /// A price or the volume is not a finite number.
    Number,
    /// `low <= open, close <= high` does not hold.
    Ohlc,
    /// The volume is negative.
    Volume,
}

impl CsvBarsRule {
    /// The rule's name in a refusal.
    pub fn as_str(self) -> &'static str {
        match self {
            CsvBarsRule::Empty => "empty",
            CsvBarsRule::Format => "format",
            CsvBarsRule::Header => "header",
            CsvBarsRule::Cells => "cells",
            CsvBarsRule::Time => "time",
            CsvBarsRule::Unit => "time unit",
            CsvBarsRule::Order => "order",
            CsvBarsRule::Duplicate => "duplicate time",
            CsvBarsRule::Interval => "interval",
            CsvBarsRule::Number => "number",
            CsvBarsRule::Ohlc => "low <= open, close <= high",
            CsvBarsRule::Volume => "volume",
        }
    }
}

/// Why a file was refused: the 1-based line of the file, the rule and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvBarsError {
    pub line: usize,
    pub rule: CsvBarsRule,
    pub detail: String,
}

impl fmt::Display for CsvBarsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}: {}", self.line, self.rule.as_str(), self.detail)
    }
}

impl std::error::Error for CsvBarsError {}

fn refuse(line: usize, rule: CsvBarsRule, detail: String) -> CsvBarsError {
    CsvBarsError { line, rule, detail }
}

fn field_list() -> String {
    HEADER_NAMES.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", ")
}

/// One line's cells: split on the delimiter, trimmed, one pair of surrounding `"` removed.
fn cells(line: &str, delimiter: char) -> Vec<&str> {
    line.split(delimiter)
        .map(|c| {
            let c = c.trim();
            c.strip_prefix('"').and_then(|c| c.strip_suffix('"')).unwrap_or(c)
        })
        .collect()
}

/// Whether a field may be absent from a file: `open`, `high` and `low` (together), and `volume`.
fn optional_field(f: usize) -> bool {
    !matches!(HEADER_NAMES[f].0, "time" | "close")
}

/// The column of each field, in [`HEADER_NAMES`] order: always `Some` for `time` and `close`, `None`
/// for an optional field the file does not hold.
fn locate(header: &[&str], columns: &CsvBarsColumns) -> Result<[Option<usize>; 6], String> {
    let named = match columns {
        CsvBarsColumns::Header => None,
        CsvBarsColumns::Named(m) => Some([
            Some(m.time.as_str()),
            m.open.as_deref(),
            m.high.as_deref(),
            m.low.as_deref(),
            Some(m.close.as_str()),
            m.volume.as_deref(),
        ]),
    };
    let mut at = [None; 6];
    for (f, (field, accepted)) in HEADER_NAMES.iter().enumerate() {
        // A mapping that leaves an optional field out says the file has none: nothing to look for.
        let name = match named {
            None => None,
            Some(names) => match names[f] {
                Some(name) => Some(name),
                None => continue,
            },
        };
        let hits: Vec<usize> = (0..header.len())
            .filter(|&i| match name {
                None => accepted.iter().any(|a| header[i].eq_ignore_ascii_case(a)),
                Some(name) => header[i] == name,
            })
            .collect();
        match hits.as_slice() {
            [one] => at[f] = Some(*one),
            // The header names have no such column: an optional field is then absent.
            [] if name.is_none() && optional_field(f) => {}
            [] => {
                let wanted = match name {
                    None => format!("one of {}", accepted.join(", ")),
                    Some(name) => format!("{name:?}"),
                };
                return Err(format!(
                    "no {field} column ({wanted}) among [{}]; the first line must be the header",
                    header.join(", ")
                ));
            }
            many => {
                let found: Vec<&str> = many.iter().map(|&i| header[i]).collect();
                return Err(format!(
                    "{} columns could be {field} ([{}]), and the decoder does not pick one",
                    many.len(),
                    found.join(", ")
                ));
            }
        }
    }
    ohl_together([at[1].is_some(), at[2].is_some(), at[3].is_some()], "the header")?;
    for (f, col) in at.iter().enumerate() {
        let Some(col) = col else { continue };
        if let Some(g) = at[..f].iter().position(|c| c == &Some(*col)) {
            return Err(format!(
                "{} and {} are both mapped to the column {:?}",
                HEADER_NAMES[g].0, HEADER_NAMES[f].0, header[*col]
            ));
        }
    }
    Ok(at)
}

/// The most header cells [`header_cells`] returns, and the most bytes of each.
pub const HEADER_CELLS_MAX: usize = 64;
/// See [`HEADER_CELLS_MAX`].
pub const HEADER_CELL_MAX_BYTES: usize = 64;

/// The file's first line split into its cells as [`decode_bars`] splits them in `format` (a leading
/// byte-order mark and blank lines skipped): what a client offers as the column choices of a mapping.
/// At most [`HEADER_CELLS_MAX`] cells, each cut to [`HEADER_CELL_MAX_BYTES`]; empty for a file with no
/// line.
pub fn header_cells(text: &str, format: &CsvBarsFormat) -> Vec<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(line) = text.lines().find(|l| !l.trim().is_empty()) else {
        return Vec::new();
    };
    cells(line, format.delimiter)
        .into_iter()
        .take(HEADER_CELLS_MAX)
        .map(|c| {
            let mut end = c.len().min(HEADER_CELL_MAX_BYTES);
            while !c.is_char_boundary(end) {
                end -= 1;
            }
            c[..end].to_string()
        })
        .collect()
}

/// The delimiters a header could be split by, and the option word that reads each.
const DELIMITERS: [(char, &str); 3] = [(',', "`,`"), (';', "`;`"), ('\t', "`tab`")];

/// Decode `text`, a whole CSV file, into its bars under the module's rules, its cells read in `format`.
/// A leading byte-order mark and blank lines are skipped; line numbers count every line of the file, the
/// header's being its own.
///
/// # Errors
/// The first line that breaks a rule, with the rule and what was found ([`CsvBarsError`]).
pub fn decode_bars(
    text: &str,
    columns: &CsvBarsColumns,
    format: &CsvBarsFormat,
) -> Result<CsvBars, CsvBarsError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines =
        text.lines().enumerate().map(|(i, l)| (i + 1, l)).filter(|(_, l)| !l.trim().is_empty());
    let Some((header_line, header_text)) = lines.next() else {
        return Err(refuse(1, CsvBarsRule::Empty, "the file holds no header and no rows".into()));
    };
    // A header the format's delimiter does not split, and another delimiter does: the file's form is
    // not the options' (an Excel export with regional settings writes `;`), and is named, not guessed.
    if !header_text.contains(format.delimiter)
        && let Some((_, word)) =
            DELIMITERS.iter().find(|(d, _)| *d != format.delimiter && header_text.contains(*d))
    {
        return Err(refuse(
            header_line,
            CsvBarsRule::Format,
            format!(
                "the header is split by {word}, not by the format's delimiter: set the delimiter \
                 option to {word}"
            ),
        ));
    }
    let header = cells(header_text, format.delimiter);
    let at = locate(&header, columns).map_err(|d| refuse(header_line, CsvBarsRule::Header, d))?;

    let mut bars: Vec<Bar> = Vec::new();
    let mut line_of: Vec<usize> = Vec::new();
    let mut first_unit: Option<(CsvBarsTimeUnit, usize)> = None;
    for (n, line) in lines {
        let row = cells(line, format.delimiter);
        if row.len() != header.len() {
            return Err(refuse(
                n,
                CsvBarsRule::Cells,
                format!("{} cells where the header has {}", row.len(), header.len()),
            ));
        }
        // `locate` holds `time` and `close` to a column; the optional ones are read where present.
        let (time_col, close_col) = (at[0].unwrap_or_default(), at[4].unwrap_or_default());
        let raw = row[time_col];
        let ts = match format.text_time(raw) {
            Some(read) => {
                first_unit.get_or_insert((CsvBarsTimeUnit::Text, n));
                read.map_err(|why| refuse(n, CsvBarsRule::Time, format!("{raw:?} {why}")))?
            }
            None => epoch_ms(raw, n, &mut first_unit)?,
        };
        if let (Some(previous), Some(&previous_line)) = (bars.last(), line_of.last()) {
            if ts == previous.timestamp {
                return Err(refuse(
                    n,
                    CsvBarsRule::Duplicate,
                    format!("{ts} ms is line {previous_line}'s time again"),
                ));
            }
            if ts < previous.timestamp {
                return Err(refuse(
                    n,
                    CsvBarsRule::Order,
                    format!(
                        "{ts} ms is before line {previous_line}'s {} ms: rows must be in strictly \
                         increasing time",
                        previous.timestamp
                    ),
                ));
            }
        }
        let number = |f: usize, col: usize| {
            let cell = row[col];
            format.number(cell).ok_or_else(|| {
                refuse(
                    n,
                    CsvBarsRule::Number,
                    format!(
                        "{} {cell:?} is not a finite number in this format{}",
                        HEADER_NAMES[f].0,
                        number_hint(cell, format)
                    ),
                )
            })
        };
        let close = number(4, close_col)?;
        // One value per row: the bar opens, peaks and bottoms at it. Without a volume column, 0.
        let (open, high, low) = match (at[1], at[2], at[3]) {
            (Some(o), Some(h), Some(l)) => (number(1, o)?, number(2, h)?, number(3, l)?),
            _ => (close, close, close),
        };
        let volume = match at[5] {
            Some(v) => number(5, v)?,
            None => 0.0,
        };
        if !(low <= open && low <= close && open <= high && close <= high) {
            return Err(refuse(
                n,
                CsvBarsRule::Ohlc,
                format!("open {open}, high {high}, low {low}, close {close}"),
            ));
        }
        if volume < 0.0 {
            return Err(refuse(n, CsvBarsRule::Volume, format!("{volume} is negative")));
        }
        bars.push(Bar::ohlcv(ts, open, high, low, close, volume));
        line_of.push(n);
    }
    let Some((unit, _)) = first_unit else {
        return Err(refuse(
            header_line + 1,
            CsvBarsRule::Empty,
            "the file holds a header and no rows".into(),
        ));
    };
    let step_ms = step(&bars, &line_of)?;
    Ok(CsvBars { bars, unit, step_ms })
}

/// An integer epoch time in the unit its value names, as epoch ms; `first_unit` is the file's first row's
/// unit, which every later row must share.
fn epoch_ms(
    raw: &str,
    n: usize,
    first_unit: &mut Option<(CsvBarsTimeUnit, usize)>,
) -> Result<i64, CsvBarsError> {
    let Ok(v) = raw.parse::<i64>() else {
        return Err(refuse(
            n,
            CsvBarsRule::Time,
            format!(
                "{raw:?} is not an integer epoch in s, ms or µs, which is UTC: for a text time set \
                 the time option to its pattern (e.g. `DD.MM.YYYY HH:mm`), and tz to its zone"
            ),
        ));
    };
    let Some(unit) = CsvBarsTimeUnit::of(v) else {
        return Err(refuse(
            n,
            CsvBarsRule::Unit,
            format!("{v} is no instant from 1971-01-01 to 2100-01-01 UTC in s, ms or µs"),
        ));
    };
    match *first_unit {
        None => *first_unit = Some((unit, n)),
        Some((first, first_line)) if first != unit => {
            return Err(refuse(
                n,
                CsvBarsRule::Unit,
                format!(
                    "{v} is in {} where line {first_line} is in {}: the file fits two units, and \
                     which one is meant is not in it",
                    unit.as_str(),
                    first.as_str()
                ),
            ));
        }
        Some(_) => {}
    }
    match unit {
        CsvBarsTimeUnit::Seconds => Ok(v * 1_000),
        CsvBarsTimeUnit::Micros if v % 1_000 == 0 => Ok(v / 1_000),
        CsvBarsTimeUnit::Micros => {
            Err(refuse(n, CsvBarsRule::Time, format!("{v} µs is not a whole millisecond")))
        }
        CsvBarsTimeUnit::Millis | CsvBarsTimeUnit::Text => Ok(v),
    }
}

/// What a number that did not read may be, said as the option that would read it: a comma decimal mark,
/// or a separator between thousands. Empty when the cell looks like neither.
fn number_hint(cell: &str, format: &CsvBarsFormat) -> String {
    let mut hints = Vec::new();
    if format.decimal == '.' && cell.contains(',') {
        hints.push("if its `,` is the decimal mark, set the decimal option to `,`");
    }
    if format.thousands.is_none() {
        if cell.trim().contains([' ', '\u{a0}', '\u{202f}']) {
            hints.push("if its space groups thousands, set the thousands option to `space`");
        }
        if format.decimal == ',' && cell.contains('.') {
            hints.push("if its `.` groups thousands, set the thousands option to `.`");
        }
    }
    if hints.is_empty() { String::new() } else { format!(": {}", hints.join("; ")) }
}

/// The file's step, the commonest distance between consecutive rows (the shorter on a tie), after every
/// distance is checked to be a whole number of it.
fn step(bars: &[Bar], line_of: &[usize]) -> Result<Option<i64>, CsvBarsError> {
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    for w in bars.windows(2) {
        *counts.entry(w[1].timestamp - w[0].timestamp).or_default() += 1;
    }
    // Ascending keys and a strict `>`: a tie keeps the shorter distance.
    let Some(step) = counts
        .iter()
        .fold(None, |best: Option<(i64, usize)>, (&d, &c)| match best {
            Some((_, bc)) if bc >= c => best,
            _ => Some((d, c)),
        })
        .map(|(d, _)| d)
    else {
        return Ok(None);
    };
    for (i, w) in bars.windows(2).enumerate() {
        let d = w[1].timestamp - w[0].timestamp;
        if d % step != 0 {
            return Err(refuse(
                line_of[i + 1],
                CsvBarsRule::Interval,
                format!(
                    "{d} ms after line {}, where the file's step is {step} ms: a gap is a whole \
                     number of steps, and this is a second interval",
                    line_of[i]
                ),
            ));
        }
    }
    Ok(Some(step))
}

#[path = "csv_bars_tests.rs"]
#[cfg(test)]
mod csv_bars_tests;
