//! **What a sandboxed run READ, each read with its WITNESS** (decision 0078's amendment of 2026-11-20, the owner's
//! "do"): what lets the hub answer an IDENTICAL request from the run it saved (`crate::backtesthub_server`'s
//! `answer_from_saved`) and know the data under it did not move. A `.rs` file's reads are known only to the loaded file
//! (`super`'s module doc), so they are recorded where every one of them crosses: the relay
//! (`super::relay`'s `InTurn::answer_in_turn`), one witness a read, taken BEFORE its answer is fetched.
//!
//! - **A bar or tick read** (`LoadBars`, `LoadBarsSourcedCapped`, `BarDays`, `ScanQuotes`, `ScanTrades`,
//!   `ScanBookUpdates`) is witnessed by the store's day stamps of it (`HistoryStore::series_days`, the read's own
//!   kind), never by its rows. Taken first, so a day restamped while the run read it leaves the recorded stamp OLDER
//!   than the rows the run got: a later check sees the newer stamp and misses, and never answers rows the stamp
//!   does not name.
//! - **Every other read** (the kept instrument lists, the series list, a coverage or a gap list, a property, a
//!   chain, cohort or depth read, perpetual metrics) is witnessed by the FNV-1a 64 of the very frames the child was
//!   answered: [`still_holds`] answers it again into the same hash. ⚠ No cheaper witness exists for a depth, chain,
//!   cohort or perpetual-metrics read (`HistoryStore::series_days` stamps bars, quotes, trades and book updates only),
//!   so a lookup reads those again in full; the series list, the inventory, a series' facts and gaps are manifest folds,
//!   cheap, and the kept lists come from the datahub handle's stamped cache. A run hashes at most
//!   [`HASHED_MAX_BYTES`] of such answers: past it the hashing STOPS and the run is not kept (a multi-gigabyte depth
//!   or chain run pays no hash for a reuse it is unlikely to see).
//! - **No witness, and the run is not kept for reuse** ([`ReadWitnesses::finish`]'s `Err`): a read whose window
//!   reaches TODAY's UTC day (an open window keeps growing), a day the store marks PROVISIONAL (rows a later settled
//!   commit replaces), a read the store cannot stamp, and a read the store refused (a failed answer may not fail
//!   again: the run's result leaned on a fault).
//!
//! Nothing here keeps a row: a witness is a stamp or a hash, and market data stays in the datahub.

use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::{Deserialize, Serialize};
use vike_data::{BarDay, HistoryStore, TimestampRange};
use vike_hub_protocol::child_pipe;
use vike_model::StoreKindId;
use vike_utility::fnv::Fnv1a64;

use super::wire::{ReadAnswerFrame, StoreRead};

/// One read a run made and its witness, as a saved run keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WitnessedRead {
    pub(crate) read: StoreRead,
    pub(crate) witness: Witness,
}

/// What stands for a read's answer: the store's day stamps of a bar or tick read, or the FNV-1a 64 of the frames
/// any other read was answered with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum Witness {
    Days(Vec<BarDay>),
    Answer(u64),
}

/// The most answer bytes ONE run hashes ([`Witness::Answer`]'s reads, all of them together): past it the run is not
/// kept for reuse and its later answers are not hashed.
pub(crate) const HASHED_MAX_BYTES: u64 = 64 << 20;

/// A read's witness as far as it is known BEFORE its answer is fetched.
pub(super) enum Before {
    Days(Vec<BarDay>),
    /// Hashed as it is answered.
    Answer,
    /// Not witnessed: the run is not kept for reuse, and why.
    Unkept(String),
}

impl Before {
    /// Whether the answer's frames are hashed (every read that has no day stamps).
    pub(super) fn hashes(&self) -> bool {
        matches!(self, Self::Answer)
    }
}

/// **The reads of ONE run, witnessed as its relay answers them.** Handed to the relay by the request that may be
/// kept (`crate::backtesthub_server::answer_from_saved`'s `Reusable`), read back once the run ended.
#[derive(Debug)]
pub(crate) struct ReadWitnesses {
    /// The first millisecond of today's UTC day when the request arrived: a read that reaches it is not witnessed.
    today: i64,
    recorded: Mutex<Recorded>,
    /// The answer bytes hashed so far, every read together ([`HASHED_MAX_BYTES`]).
    hashed: AtomicU64,
}

/// The reads recorded so far.
#[derive(Debug, Default)]
struct Recorded {
    /// Each read by its number in request order.
    reads: Vec<(u64, WitnessedRead)>,
    /// The first reason the run may not be kept.
    unkept: Option<String>,
}

impl ReadWitnesses {
    /// No read yet, `now_ms` the request's arrival.
    pub(crate) fn new(now_ms: i64) -> Self {
        let today = now_ms - now_ms.rem_euclid(vike_model::MS_PER_DAY);
        Self { today, recorded: Mutex::default(), hashed: AtomicU64::new(0) }
    }

    /// The first millisecond of today's UTC day when the request arrived: a window that reaches it is not witnessed.
    pub(crate) fn today(&self) -> i64 {
        self.today
    }

    /// What watches the answer of a read whose witness is `before`: its hash taken while the run's
    /// [`HASHED_MAX_BYTES`] has room.
    pub(super) fn seen(&self, before: &Before) -> Seen {
        let left = HASHED_MAX_BYTES.saturating_sub(self.hashed.load(Ordering::Relaxed));
        Seen::hashing_up_to(before.hashes().then_some(left))
    }

    /// `read`'s witness as far as it is known before its answer: its day stamps asked now, or a hash to take as it
    /// is answered. A store that panics answering is a read not witnessed, never a relay that stops.
    pub(super) fn before(
        &self,
        store: &(dyn HistoryStore + Send + Sync),
        read: &StoreRead,
    ) -> Before {
        if reaches(read, self.today) {
            return Before::Unkept(format!("{} reaches today's UTC day", name_of(read)));
        }
        let Some(asked) = days_of(read) else { return Before::Answer };
        let answered = catch_unwind(AssertUnwindSafe(|| asked.ask(store)));
        match answered {
            Ok(Ok(Some(days))) if days.iter().all(|day| !day.provisional) => Before::Days(days),
            Ok(Ok(Some(_))) => Before::Unkept(format!("{} reads a provisional day", name_of(read))),
            Ok(Ok(None)) => Before::Unkept(format!("the store stamps no day of {}", name_of(read))),
            Ok(Err(e)) => {
                Before::Unkept(format!("{}'s day stamps were not answered: {e}", name_of(read)))
            }
            Err(_) => Before::Unkept(format!("the store panicked stamping {}", name_of(read))),
        }
    }

    /// Record read `number` once its answer went down: `before` its witness, `seen` what was written of the answer,
    /// `refused` whether the relay refused it in its own words (a panic, a frame it could not write).
    pub(super) fn record(
        &self,
        number: u64,
        read: StoreRead,
        before: Before,
        seen: &Seen,
        refused: bool,
    ) {
        self.hashed.fetch_add(seen.hashed, Ordering::Relaxed);
        let witness = match before {
            _ if refused || seen.failed() => {
                Err(format!("the store refused {}, so the run leaned on a fault", name_of(&read)))
            }
            Before::Unkept(why) => Err(why),
            Before::Answer if seen.over => Err(format!(
                "its reads with no day stamps passed {HASHED_MAX_BYTES} bytes at {}, so their hashing stopped",
                name_of(&read)
            )),
            Before::Days(days) => Ok(Witness::Days(days)),
            Before::Answer => Ok(Witness::Answer(seen.fnv.finish())),
        };
        let mut kept = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);
        match witness {
            Ok(witness) => kept.reads.push((number, WitnessedRead { read, witness })),
            Err(why) => {
                kept.unkept.get_or_insert(why);
            }
        }
    }

    /// Every read in request order with its witness, or why the run may not be kept for reuse.
    pub(crate) fn finish(&self) -> Result<Vec<WitnessedRead>, String> {
        let mut kept = self.recorded.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(why) = kept.unkept.take() {
            return Err(why);
        }
        let mut reads = std::mem::take(&mut kept.reads);
        reads.sort_by_key(|(number, _)| *number);
        Ok(reads.into_iter().map(|(_, read)| read).collect())
    }
}

/// **Whether every recorded read would be answered as it was**: each bar or tick read's day stamps asked again and
/// equal, each other read answered again into the same hash. `Err` names the first that moved.
pub(crate) fn still_holds(
    reads: &[WitnessedRead],
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<(), String> {
    for WitnessedRead { read, witness } in reads {
        let now = match witness {
            Witness::Days(_) => match days_of(read).map(|asked| asked.ask(store)) {
                Some(Ok(Some(days))) => Witness::Days(days),
                _ => return Err(format!("the store no longer stamps {}", name_of(read))),
            },
            Witness::Answer(_) => {
                let mut seen = Seen::new(true);
                let answered =
                    super::relay::answer(&mut seen.over(&mut io::sink()), store, read.clone());
                if answered.is_err() || seen.failed() {
                    return Err(format!("the store did not answer {} again", name_of(read)));
                }
                Witness::Answer(seen.fnv.finish())
            }
        };
        if &now != witness {
            return Err(format!("{} changed since the run read it", name_of(read)));
        }
    }
    Ok(())
}

/// What was written of one read's answer: its first bytes (whether it was a `Failed` frame) and, for a hashed read,
/// the FNV-1a 64 of every byte, up to a bound.
pub(super) struct Seen {
    head: Vec<u8>,
    /// How many more bytes are hashed; `None`: none are.
    hash_left: Option<u64>,
    /// The bytes hashed.
    hashed: u64,
    /// Whether the answer passed the bound, its hash stopped.
    over: bool,
    fnv: Fnv1a64,
}

/// How many bytes of an answer [`Seen`] keeps: a frame's four length bytes and its body's first eight.
const HEAD: usize = 12;

impl Seen {
    /// Every byte hashed when `hashing`, none otherwise.
    pub(super) fn new(hashing: bool) -> Self {
        Self::hashing_up_to(hashing.then_some(u64::MAX))
    }

    fn hashing_up_to(hash_left: Option<u64>) -> Self {
        let head = Vec::with_capacity(HEAD);
        Self { head, hash_left, hashed: 0, over: false, fnv: Fnv1a64::new() }
    }

    /// `out` written through this record.
    pub(super) fn over<'a, W: Write>(&'a mut self, out: &'a mut W) -> Watched<'a, W> {
        Watched { out, seen: self }
    }

    /// Whether the answer's first frame is the store's refusal (`ReadAnswerFrame::Failed`): every answer opens with
    /// its first frame, and a refused read writes that frame alone.
    fn failed(&self) -> bool {
        self.head.len() == HEAD && self.head[4..] == *failed_head()
    }

    fn see(&mut self, bytes: &[u8]) {
        let room = HEAD - self.head.len();
        self.head.extend_from_slice(&bytes[..room.min(bytes.len())]);
        if let Some(left) = self.hash_left {
            let len = bytes.len() as u64;
            if len > left {
                self.over = true;
                self.hash_left = None;
            } else {
                self.fnv.write(bytes);
                self.hash_left = Some(left - len);
                self.hashed += len;
            }
        }
    }
}

/// A writer that hands its bytes on and shows them to a [`Seen`].
pub(super) struct Watched<'a, W> {
    out: &'a mut W,
    seen: &'a mut Seen,
}

impl<W: Write> Write for Watched<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.seen.see(&buf[..n]);
        Ok(n)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.out.write_all(buf)?;
        self.seen.see(buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// The first eight body bytes of a `ReadAnswerFrame::Failed` frame as the relay writes one (positional msgpack: the
/// variant's name opens it), read off a real encoding so no byte is spelled here.
fn failed_head() -> &'static [u8] {
    static HEAD_BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    HEAD_BYTES.get_or_init(|| {
        let mut frame = Vec::new();
        let failed = ReadAnswerFrame::<()>::Failed(vike_data::HistoryStoreError::Io(String::new()));
        let _ = child_pipe::write_message(&mut frame, &failed);
        frame.get(4..HEAD).map(<[u8]>::to_vec).unwrap_or_default()
    })
}

/// A bar or tick read's day-stamp question (`HistoryStore::series_days`'s arguments).
struct DaysAsked<'a> {
    kind: StoreKindId,
    venue: &'a str,
    source: Option<&'a str>,
    symbol: &'a str,
    interval: Option<&'a str>,
    range: TimestampRange,
}

impl DaysAsked<'_> {
    fn ask(
        &self,
        store: &(dyn HistoryStore + Send + Sync),
    ) -> Result<Option<Vec<BarDay>>, vike_data::HistoryStoreError> {
        store.series_days(
            self.kind,
            self.venue,
            self.source,
            self.symbol,
            self.interval,
            self.range,
        )
    }
}

/// `read`'s day-stamp question, or `None` for a read the store stamps no day of.
fn days_of(read: &StoreRead) -> Option<DaysAsked<'_>> {
    let (kind, venue, source, symbol, interval, range) = match read {
        StoreRead::LoadBars { venue, symbol, interval, range } => {
            (StoreKindId::Bar, venue, None, symbol, Some(interval.as_str()), range)
        }
        // A budget reads a part of the range: unchanged stamps over the whole of it leave that part unchanged.
        StoreRead::LoadBarsSourcedCapped { venue, source, symbol, interval, range, .. } => {
            (StoreKindId::Bar, venue, Some(source.as_str()), symbol, Some(interval.as_str()), range)
        }
        StoreRead::BarDays { venue, source, symbol, interval, range } => {
            (StoreKindId::Bar, venue, source.as_deref(), symbol, Some(interval.as_str()), range)
        }
        StoreRead::ScanQuotes { venue, symbol, range } => {
            (StoreKindId::Quote, venue, None, symbol, None, range)
        }
        StoreRead::ScanTrades { venue, symbol, range } => {
            (StoreKindId::Trade, venue, None, symbol, None, range)
        }
        StoreRead::ScanBookUpdates { venue, symbol, range } => {
            (StoreKindId::Book, venue, None, symbol, None, range)
        }
        _ => return None,
    };
    Some(DaysAsked { kind, venue, source, symbol, interval, range: *range })
}

/// Whether `read`'s window reaches `today` (the first millisecond of today's UTC day): an open end, or an end in it
/// or after it. A read with no window (a list, the kept rows) reaches no day.
fn reaches(read: &StoreRead, today: i64) -> bool {
    let end = match read {
        StoreRead::LoadBars { range, .. }
        | StoreRead::LoadBarsSourcedCapped { range, .. }
        | StoreRead::BarDays { range, .. }
        | StoreRead::ScanQuotes { range, .. }
        | StoreRead::ScanTrades { range, .. }
        | StoreRead::ScanBookUpdates { range, .. }
        | StoreRead::ScanDepth { range, .. }
        | StoreRead::ScanCohort { range, .. }
        | StoreRead::ScanPerpMetrics { range, .. }
        | StoreRead::ScanChain { range, .. } => range.end,
        StoreRead::PropertiesAsOf { timestamp, .. } => Some(*timestamp),
        StoreRead::ListSeries
        | StoreRead::Inventory
        | StoreRead::SeriesGaps(_)
        | StoreRead::SeriesFacts(_)
        | StoreRead::KeptInstruments { .. } => return false,
    };
    end.is_none_or(|end| end >= today)
}

/// A read named for a log line: its verb and series, never its answer.
fn name_of(read: &StoreRead) -> String {
    match read {
        StoreRead::LoadBars { venue, symbol, interval, .. }
        | StoreRead::LoadBarsSourcedCapped { venue, symbol, interval, .. }
        | StoreRead::BarDays { venue, symbol, interval, .. } => {
            format!("the {interval} bars of {venue} {symbol}")
        }
        StoreRead::ScanQuotes { venue, symbol, .. } => format!("the quotes of {venue} {symbol}"),
        StoreRead::ScanTrades { venue, symbol, .. } => format!("the trades of {venue} {symbol}"),
        StoreRead::ScanBookUpdates { venue, symbol, .. } => format!("the book of {venue} {symbol}"),
        StoreRead::ScanDepth { venue, symbol, .. } => format!("the depth of {venue} {symbol}"),
        StoreRead::ScanCohort { venue, asset, .. } => format!("the cohort of {venue} {asset}"),
        StoreRead::ScanPerpMetrics { venue, symbol, .. } => {
            format!("the perpetual metrics of {venue} {symbol}")
        }
        StoreRead::PropertiesAsOf { venue, symbol, .. } => {
            format!("the properties of {venue} {symbol}")
        }
        StoreRead::ScanChain { venue, underlying, .. } => {
            format!("the chain of {venue} {underlying}")
        }
        StoreRead::ListSeries => "the series list".to_string(),
        StoreRead::Inventory => "the inventory".to_string(),
        StoreRead::SeriesGaps(id) => format!("the gaps of {id:?}"),
        StoreRead::SeriesFacts(id) => format!("the facts of {id:?}"),
        StoreRead::KeptInstruments { .. } => "the kept instrument lists".to_string(),
    }
}

#[cfg(test)]
#[path = "witness_tests.rs"]
mod witness_tests;
