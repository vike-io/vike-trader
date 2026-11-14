//! One Binance daily aggTrades zip, decoded: the zip reader, the row parser and the id continuity
//! check. Pure over the file's bytes, which the datahub's safe open reads.
//!
//! # The reader refuses rather than guesses
//!
//! A zip here is one local header, one deflate stream and a central directory naming one member.
//! Anything else is refused, and so is a stream that ends early or whose CRC-32 disagrees with the
//! directory's. ⚠ The 32-bit size fields are never read: a member can pass 4 GiB, and neither the
//! decode nor its checks need them.
//!
//! # The rows
//!
//! `agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker`. A newer
//! file opens with that header line and an older one does not, so the FIRST line of each file decides.
//! A row that does not parse refuses the whole day: such a row is never skipped. A refusal names the
//! line and the field and never echoes file bytes. `transact_time` must be epoch milliseconds inside
//! the file's own UTC day, which also refuses a file in any other time unit.
//!
//! # An exact repeat is dropped, a differing one refuses the day
//!
//! A row whose seven fields all equal an earlier row of the same file is dropped and counted
//! ([`DecodedDay::duplicates`]): it is the same aggregated trade, and storing it twice would double
//! the day's volume. A row that repeats an earlier `agg_trade_id` with any field different is a
//! contradiction, and nothing in the file says which row is right, so the day is refused
//! (`ContradictoryRepeat`). ⚠ Measured: Binance's `SOLUSDT-aggTrades-2022-09-11.zip` holds every row
//! exactly twice, 215,630 rows and 107,815 ids, each pair byte-identical.
//!
//! # The continuity check reports and repairs nothing
//!
//! Consecutive rows should run `agg_trade_id + 1` and `first_trade_id = previous last_trade_id + 1`.
//! [`DayContinuity`] counts every step that does not, and every timestamp that goes backward, over the
//! rows kept once the exact repeats are dropped. Those rows are stored as published, so a gap does not
//! refuse a day.
//!
//! ⚠ **A skipped trade id is normal, and only the rest needs an operator** ([`DayContinuity::is_broken`]).
//! Measured on SOLUSDT from 2026-10-01 to 2026-11-06: inside a day `agg_trade_id` has no gap in 9,268,065
//! rows and no time goes backward, while `first_trade_id` skips ids on 331,565 rows, nearly always by a
//! few. Reporting those as a break would flag every day, and a report on every day is read by nobody.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};

use flate2::CrcReader;
use flate2::bufread::DeflateDecoder;
use vike_data::archive_day::ArchiveDayRefusal;
use vike_marketdata::TickTrade;
use vike_model::MS_PER_DAY;

/// The bytes a zip's local file header holds before its name and extra field.
pub const LOCAL_HEADER_BYTES: usize = 30;

const LOCAL_SIGNATURE: &[u8; 4] = b"PK\x03\x04";
const CENTRAL_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
const END_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
const END64_SIGNATURE: &[u8; 4] = b"PK\x06\x06";
const CENTRAL_HEADER_BYTES: usize = 46;
const FLAG_ENCRYPTED: u16 = 0x0001;
/// The sizes and CRC follow the stream in a data descriptor rather than sit in the local header.
const FLAG_DATA_DESCRIPTOR: u16 = 0x0008;
const METHOD_DEFLATE: u16 = 8;
/// The most bytes a data descriptor can take: a signature, a CRC and two 64-bit sizes.
const MAX_DESCRIPTOR_BYTES: usize = 24;

/// The header line a newer file opens with.
const HEADER_LINE: &str =
    "agg_trade_id,price,quantity,first_trade_id,last_trade_id,transact_time,is_buyer_maker";

/// The longest row this format can hold, generously: seven short fields. It bounds what a file with
/// no line ends can make the reader buffer.
pub(super) const MAX_LINE_BYTES: u64 = 512;

/// How many gaps of each kind a [`DayContinuity`] keeps by position. The counts go on past it.
const MAX_LISTED_GAPS: usize = 32;

/// The decode's two bounds, against a wrong or hostile file: DEFAULTS, until a real day says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggLimits {
    /// The largest zip the lane reads into memory.
    pub max_file_bytes: u64,
    /// The most rows one day may decode to, the exact repeats it drops not counted. One kept row is
    /// a 64-byte [`TickTrade`] plus its 24 bytes of ids while the day decodes, so this bounds that day
    /// inside the datahub's memory cap.
    pub max_rows: usize,
}

impl AggLimits {
    /// The default bounds: 1 GiB of zip and 50 million rows, about 4.4 GB in memory while decoding.
    pub const DEFAULT: AggLimits =
        AggLimits { max_file_bytes: 1024 * 1024 * 1024, max_rows: 50_000_000 };
}

/// What a zip's first bytes say, read without decoding anything.
pub(super) struct LocalHeader {
    pub(super) flags: u16,
    pub(super) name_len: usize,
    pub(super) extra_len: usize,
}

pub(super) fn refused(class: &str, detail: String) -> ArchiveDayRefusal {
    ArchiveDayRefusal::new(
        class,
        format!("{detail} Nothing was stored and no commit key was spent."),
    )
}

pub(super) fn not_one_member(why: &str) -> ArchiveDayRefusal {
    refused(
        "NotOneDeflatedMember",
        format!("the file is not one deflated member in a zip: {why}. Download it again."),
    )
}

fn le16(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

/// Check a zip's local header from its first [`LOCAL_HEADER_BYTES`] bytes: what a plan learns about a
/// file before anything is decoded. `Err` when the header alone already refuses the file.
pub fn check_local_header(prefix: &[u8]) -> Result<(), ArchiveDayRefusal> {
    read_local_header(prefix).map(|_| ())
}

pub(super) fn read_local_header(bytes: &[u8]) -> Result<LocalHeader, ArchiveDayRefusal> {
    if bytes.len() < LOCAL_HEADER_BYTES || !bytes.starts_with(LOCAL_SIGNATURE) {
        return Err(not_one_member("it does not open with a zip local file header"));
    }
    let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
    if flags & FLAG_ENCRYPTED != 0 {
        return Err(not_one_member("its member is encrypted"));
    }
    if le16(bytes, 8) != usize::from(METHOD_DEFLATE) {
        return Err(not_one_member("its member is not deflated"));
    }
    Ok(LocalHeader { flags, name_len: le16(bytes, 26), extra_len: le16(bytes, 28) })
}

/// Check what follows the deflate stream: an optional data descriptor, then exactly one central
/// directory entry whose CRC-32 is `crc`, then the end record.
pub(super) fn check_after_stream(
    tail: &[u8],
    flags: u16,
    crc: u32,
) -> Result<(), ArchiveDayRefusal> {
    if tail.is_empty() {
        return Err(refused(
            "Truncated",
            "the file ends before its deflate stream does, so it is a partial download. Download it \
             again."
                .to_string(),
        ));
    }
    let reach = if flags & FLAG_DATA_DESCRIPTOR != 0 {
        MAX_DESCRIPTOR_BYTES + CENTRAL_SIGNATURE.len()
    } else {
        4
    };
    let window = &tail[..tail.len().min(reach)];
    let Some(at) = window.windows(4).position(|w| w == CENTRAL_SIGNATURE) else {
        return Err(not_one_member(
            "something other than its central directory follows the stream",
        ));
    };
    let Some(entry) = tail.get(at..at + CENTRAL_HEADER_BYTES) else {
        return Err(not_one_member("its central directory is cut short"));
    };
    let next = at + CENTRAL_HEADER_BYTES + le16(entry, 28) + le16(entry, 30) + le16(entry, 32);
    match tail.get(next..next + 4) {
        Some(s) if s == END_SIGNATURE || s == END64_SIGNATURE => {}
        Some(s) if s == CENTRAL_SIGNATURE => {
            return Err(not_one_member("its central directory names more than one member"));
        }
        _ => return Err(not_one_member("its central directory does not end where it should")),
    }
    let want = u32::from_le_bytes([entry[16], entry[17], entry[18], entry[19]]);
    if want != crc {
        return Err(refused(
            "Corrupt",
            format!(
                "the decoded rows' CRC-32 is {crc:08x} and the zip's directory says {want:08x}. \
                 Download the file again and check it against its .CHECKSUM."
            ),
        ));
    }
    Ok(())
}

/// One row, parsed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AggRow {
    agg_trade_id: u64,
    price: f64,
    quantity: f64,
    first_trade_id: u64,
    last_trade_id: u64,
    transact_time: i64,
    is_buyer_maker: bool,
}

/// Parse one line, or name the field that does not parse.
fn parse_row(line: &str) -> Result<AggRow, &'static str> {
    let mut fields = line.split(',');
    let mut field = |name| fields.next().ok_or(name);
    let id = |raw: &str, name| raw.parse::<u64>().map_err(|_| name);
    let positive = |raw: &str, name| match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => Ok(v),
        _ => Err(name),
    };
    let row = AggRow {
        agg_trade_id: id(field("agg_trade_id")?, "agg_trade_id")?,
        price: positive(field("price")?, "price")?,
        quantity: positive(field("quantity")?, "quantity")?,
        first_trade_id: id(field("first_trade_id")?, "first_trade_id")?,
        last_trade_id: id(field("last_trade_id")?, "last_trade_id")?,
        transact_time: field("transact_time")?.parse().map_err(|_| "transact_time")?,
        is_buyer_maker: match field("is_buyer_maker")? {
            "true" => true,
            "false" => false,
            _ => return Err("is_buyer_maker"),
        },
    };
    if fields.next().is_some() {
        return Err("a field past the seventh");
    }
    if row.first_trade_id > row.last_trade_id {
        return Err("first_trade_id, which is past last_trade_id");
    }
    Ok(row)
}

/// The ids of every row kept so far, aligned with its trade, and how to find one by `agg_trade_id`:
/// what a repeat is compared against.
///
/// ⚠ The bound is [`AggLimits::max_rows`]: 24 bytes a kept row, plus an index of a few dozen bytes a
/// row only once a row arrives below the one before, which no measured file does. Dropped at the end
/// of the decode, before anything is stored.
struct Kept {
    /// `agg_trade_id`, `first_trade_id` and `last_trade_id` of each kept row.
    ids: Vec<[u64; 3]>,
    /// Each kept row's position by `agg_trade_id`, `None` while `ids` ascends and a binary search
    /// finds it.
    index: Option<HashMap<u64, usize>>,
}

impl Kept {
    /// The position of the kept row carrying `agg_trade_id`, if one does.
    fn find(&self, agg_trade_id: u64) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(&agg_trade_id).copied(),
            None if self.ids.last().is_none_or(|last| agg_trade_id > last[0]) => None,
            None => self.ids.binary_search_by_key(&agg_trade_id, |ids| ids[0]).ok(),
        }
    }

    fn keep(&mut self, row: &AggRow) {
        let ids = [row.agg_trade_id, row.first_trade_id, row.last_trade_id];
        if self.index.is_none() && self.ids.last().is_some_and(|last| ids[0] < last[0]) {
            self.index = Some(self.ids.iter().enumerate().map(|(at, ids)| (ids[0], at)).collect());
        }
        if let Some(index) = &mut self.index {
            index.insert(ids[0], self.ids.len());
        }
        self.ids.push(ids);
    }

    /// The kept row at `at`, whole again from its ids and its `trade`.
    fn row(&self, at: usize, trade: &TickTrade) -> AggRow {
        let [agg_trade_id, first_trade_id, last_trade_id] = self.ids[at];
        AggRow {
            agg_trade_id,
            price: trade.price,
            quantity: trade.size,
            first_trade_id,
            last_trade_id,
            transact_time: trade.timestamp,
            is_buyer_maker: trade.is_buyer_maker,
        }
    }
}

/// The ids at one end of a file: what the next file's first row must follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggEdge {
    /// The row's `agg_trade_id`.
    pub agg_trade_id: u64,
    /// At the first row its `first_trade_id`; at the last row its `last_trade_id`.
    pub trade_id: u64,
    /// The row's time, in epoch milliseconds.
    pub ts: i64,
}

/// One break in an id sequence: where it is and how many ids are missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdGap {
    /// The last id before the break.
    pub after: u64,
    /// The first id after it.
    pub next: u64,
    /// The time of the row after the break, in epoch milliseconds.
    pub ts: i64,
}

impl IdGap {
    /// How many ids are missing.
    pub fn missing(&self) -> u64 {
        self.next - self.after - 1
    }
}

/// What the continuity check found in one day's rows. It repairs and filters nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DayContinuity {
    /// The ids of the day's first row, `None` for a file with no rows.
    pub first: Option<AggEdge>,
    /// The ids of the day's last row.
    pub last: Option<AggEdge>,
    /// How many times `agg_trade_id` jumped forward, and the first [`MAX_LISTED_GAPS`] of them.
    pub agg_gaps: u64,
    /// See [`Self::agg_gaps`].
    pub agg_listed: Vec<IdGap>,
    /// How many times `first_trade_id` jumped forward past the previous `last_trade_id + 1`, and the
    /// first [`MAX_LISTED_GAPS`] of them.
    pub trade_gaps: u64,
    /// See [`Self::trade_gaps`].
    pub trade_listed: Vec<IdGap>,
    /// How many rows carry an `agg_trade_id` at or below the row before: ids out of order inside the
    /// day, or a repeat across the step from the file before. A repeat inside the day never reaches
    /// this count: [`decode_daily_zip`] drops an exact one and refuses the day for a differing one.
    pub repeated_agg_ids: u64,
    /// How many rows carry a `first_trade_id` at or below the previous `last_trade_id`.
    pub repeated_trade_ids: u64,
    /// How many rows carry a time before the row above them.
    pub backward_timestamp: u64,
}

impl DayContinuity {
    fn note_row(&mut self, row: &AggRow) {
        let edge = AggEdge {
            agg_trade_id: row.agg_trade_id,
            trade_id: row.last_trade_id,
            ts: row.transact_time,
        };
        match self.last {
            None => {
                self.first = Some(AggEdge { trade_id: row.first_trade_id, ..edge });
            }
            Some(previous) => {
                self.note_step(previous, row.agg_trade_id, row.first_trade_id, row.transact_time);
                if row.transact_time < previous.ts {
                    self.backward_timestamp += 1;
                }
            }
        }
        self.last = Some(edge);
    }

    fn note_step(&mut self, previous: AggEdge, agg_trade_id: u64, first_trade_id: u64, ts: i64) {
        if agg_trade_id <= previous.agg_trade_id {
            self.repeated_agg_ids += 1;
        } else if agg_trade_id != previous.agg_trade_id + 1 {
            self.agg_gaps += 1;
            if self.agg_listed.len() < MAX_LISTED_GAPS {
                self.agg_listed.push(IdGap {
                    after: previous.agg_trade_id,
                    next: agg_trade_id,
                    ts,
                });
            }
        }
        if first_trade_id <= previous.trade_id {
            self.repeated_trade_ids += 1;
        } else if first_trade_id != previous.trade_id + 1 {
            self.trade_gaps += 1;
            if self.trade_listed.len() < MAX_LISTED_GAPS {
                self.trade_listed.push(IdGap {
                    after: previous.trade_id,
                    next: first_trade_id,
                    ts,
                });
            }
        }
    }

    /// Check this day's first row against the last row of the file before it, counted like any step.
    pub fn note_previous_file(&mut self, previous: AggEdge) {
        if let Some(first) = self.first {
            self.note_step(previous, first.agg_trade_id, first.trade_id, first.ts);
        }
    }

    /// Whether an operator must read this day: an aggregated trade missing or repeated, a trade id
    /// repeated, or a time going backward. A skipped trade id alone is not one: see the module doc.
    pub fn is_broken(&self) -> bool {
        self.agg_gaps + self.repeated_agg_ids + self.repeated_trade_ids + self.backward_timestamp
            > 0
    }

    /// One operator's sentence over everything found, `None` for a day with nothing to report.
    pub fn report(&self) -> Option<String> {
        let found = self.agg_gaps
            + self.trade_gaps
            + self.repeated_agg_ids
            + self.repeated_trade_ids
            + self.backward_timestamp;
        if found == 0 {
            return None;
        }
        let listed = |gaps: &[IdGap]| {
            let each: Vec<String> = gaps
                .iter()
                .map(|g| {
                    format!("{} -> {} ({} missing, at ts {})", g.after, g.next, g.missing(), g.ts)
                })
                .collect();
            each.join("; ")
        };
        Some(format!(
            "{} agg_trade_id gaps [{}] · {} trade id gaps [{}] · {} repeated agg_trade_ids · {} \
             repeated trade ids · {} backward timestamps",
            self.agg_gaps,
            listed(&self.agg_listed),
            self.trade_gaps,
            listed(&self.trade_listed),
            self.repeated_agg_ids,
            self.repeated_trade_ids,
            self.backward_timestamp
        ))
    }
}

/// One file, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedDay {
    /// Every row kept, in file order: each exact repeat of an earlier row dropped.
    pub trades: Vec<TickTrade>,
    /// How many rows were dropped as an exact repeat of an earlier row: see the module doc.
    pub duplicates: u64,
    /// What the continuity check found.
    pub continuity: DayContinuity,
}

impl DecodedDay {
    /// The operator's sentence for this day, and what an import does with it: what the continuity
    /// check found that an operator must read ([`DayContinuity::is_broken`]), and the exact repeats
    /// dropped. `None` for a day with neither.
    pub fn report(&self) -> Option<String> {
        let c = &self.continuity;
        let found = c.report().filter(|_| c.is_broken());
        match (found, self.duplicates) {
            (None, 0) => None,
            (Some(found), 0) => Some(format!("{found}. An import stores this day as published.")),
            (found, n) => Some(format!(
                "{}{n} rows repeat an earlier row of the file exactly, and are dropped. An import \
                 stores each of the {} trades once, as published.",
                found.map(|f| format!("{f} · ")).unwrap_or_default(),
                self.trades.len()
            )),
        }
    }
}

/// Decode the zip `blob` for the UTC day starting at `day`, whose one member must be `member`.
///
/// ⚠ `member` is checked because the walk can see only a file's name, never which dataset it sits
/// under: this is what stops a renamed file or another symbol's file landing in this series.
pub fn decode_daily_zip(
    blob: &[u8],
    member: &str,
    day: i64,
    limits: &AggLimits,
) -> Result<DecodedDay, ArchiveDayRefusal> {
    let header = read_local_header(blob)?;
    let start = LOCAL_HEADER_BYTES + header.name_len + header.extra_len;
    let name = blob.get(LOCAL_HEADER_BYTES..LOCAL_HEADER_BYTES + header.name_len);
    let Some(stream) = blob.get(start..) else {
        return Err(not_one_member("its local header runs past the end of the file"));
    };
    if name != Some(member.as_bytes()) {
        return Err(refused(
            "ForeignMember",
            format!(
                "this zip does not hold `{member}`, the one member this dataset's day must hold. It \
                 was renamed, or it is another instrument's file."
            ),
        ));
    }

    let inflate = CrcReader::new(DeflateDecoder::new(stream));
    let mut reader = BufReader::with_capacity(1 << 20, inflate);
    // Roughly 14 compressed bytes a row: reserving up front spares the day's peak the doubling a
    // growing `Vec` would pay, and it is only address space until a row lands.
    let capacity = (blob.len() / 14).min(limits.max_rows);
    let mut decoded = DecodedDay {
        trades: Vec::with_capacity(capacity),
        duplicates: 0,
        continuity: DayContinuity::default(),
    };
    let mut kept = Kept { ids: Vec::with_capacity(capacity), index: None };
    let end = day + MS_PER_DAY - 1;
    let mut buffer = Vec::with_capacity(MAX_LINE_BYTES as usize);
    let mut number = 0u64;
    loop {
        buffer.clear();
        let n = (&mut reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut buffer)
            .map_err(|e| corrupt_stream(&e))?;
        if n == 0 {
            break;
        }
        number += 1;
        let complete = buffer.last() == Some(&b'\n');
        let text = if complete { &buffer[..n - 1] } else { &buffer[..] };
        let row = std::str::from_utf8(text)
            .map_err(|_| "a field, because the line is not UTF-8")
            .and_then(|line| {
                if number == 1 && line == HEADER_LINE {
                    Ok(None)
                } else {
                    parse_row(line).map(Some)
                }
            });
        let row = match row {
            Ok(Some(row)) => row,
            Ok(None) => continue,
            Err(field) => {
                // A partial last line is what a truncated stream looks like: name that, if it is.
                if !complete {
                    let mut inflate = reader.into_inner();
                    drain(&mut inflate)?;
                    let crc = inflate.crc().sum();
                    check_after_stream(inflate.into_inner().into_inner(), header.flags, crc)?;
                }
                return Err(refused(
                    "BadRow",
                    format!(
                        "line {number} of `{member}` does not parse: {field}. A row that does not \
                         parse is never skipped, so the whole day is refused."
                    ),
                ));
            }
        };
        if !(day..=end).contains(&row.transact_time) {
            return Err(refused(
                "OutsideDay",
                format!(
                    "line {number} of `{member}` has a transact_time outside the file's UTC day \
                     [{day}, {end}] in epoch milliseconds. Another time unit, or a mis-filed file."
                ),
            ));
        }
        if let Some(at) = kept.find(row.agg_trade_id) {
            if kept.row(at, &decoded.trades[at]) != row {
                return Err(refused(
                    "ContradictoryRepeat",
                    format!(
                        "line {number} of `{member}` repeats agg_trade_id {} of an earlier row, \
                         with a different row. The file contradicts itself and nothing here can say \
                         which row is right, so the whole day is refused. Report it, with the \
                         file's name.",
                        row.agg_trade_id
                    ),
                ));
            }
            decoded.duplicates += 1;
            continue;
        }
        if decoded.trades.len() == limits.max_rows {
            return Err(refused(
                "DecodedTooLarge",
                format!(
                    "`{member}` holds more than the {} rows one day may decode to.",
                    limits.max_rows
                ),
            ));
        }
        decoded.continuity.note_row(&row);
        kept.keep(&row);
        decoded.trades.push(TickTrade {
            timestamp: row.transact_time,
            local_timestamp: 0,
            price: row.price,
            size: row.quantity,
            is_buyer_maker: row.is_buyer_maker,
            symbol: String::new(),
        });
    }
    let inflate = reader.into_inner();
    let crc = inflate.crc().sum();
    check_after_stream(inflate.into_inner().into_inner(), header.flags, crc)?;
    Ok(decoded)
}

/// Read a stream to its end, for the check that follows it.
pub(super) fn drain<R: Read>(reader: &mut R) -> Result<(), ArchiveDayRefusal> {
    std::io::copy(reader, &mut std::io::sink()).map(|_| ()).map_err(|e| corrupt_stream(&e))
}

pub(super) fn corrupt_stream(e: &std::io::Error) -> ArchiveDayRefusal {
    refused(
        "Corrupt",
        format!("its deflate stream does not decode ({}). Download it again.", e.kind()),
    )
}
