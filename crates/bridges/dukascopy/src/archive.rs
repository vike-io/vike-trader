//! Dukascopy's DAILY `.bi5` tick files — the archive import lane's format half: the path grammar,
//! bounded decompression, and a daily decoder that REFUSES rather than guesses.
//!
//! `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §3.1–§3.4 and §5 are the design
//! (task T1 of its §9). Everything here is a pure function over path segments and bytes: no
//! filesystem walk, no store, no network. The walk, the confined open and the read through
//! `take(cap + 1)` belong to the datahub that mounts the lane; the archive day key, the day
//! classification and the append belong to the store half. Both consume this module.
//!
//! # The layout (§3.1)
//!
//! The vendor's Requester-Pays bucket holds `SYMBOL/YEAR/MONTH/DAY_ticks.bi5`, one file per UTC day,
//! and its MONTH IS ZERO-BASED (January = `00`). [`classify_bi5_path`] reads a path RELATIVE to the
//! dataset directory:
//!
//! - `<YYYY>/<MM>/<DD>_ticks.bi5` is a [`Bi5Path::Daily`] file — `YYYY` 1970 to the current year,
//!   `MM` `00`–`11`, `DD` `01`–`31`, and the triple a real calendar date (`2024/01/30`, 30 February,
//!   is not one). The day starts at UTC midnight of that date. These are imported.
//! - `<YYYY>/<MM>/<DD>/<HH>h_ticks.bi5` is a [`Bi5Path::Hourly`] file — the HTTP datafeed's own
//!   layout (`crates/bridges/dukascopy/src/data.rs`'s `hour_url`) and the likeliest shape of the
//!   vendor's "legacy hourly files". Recognised and reported by day, NOT imported in v1 (Q6). A day
//!   holding both a daily file and hourly files is refused by the plan, which sees every path of a
//!   day; this function classifies one.
//! - Everything else, at any depth, is [`Bi5Path::Other`]: counted, never opened.
//!
//! [`is_bi5_layout_dir`] is the same grammar for the DIRECTORIES a walk may descend into.
//!
//! # The record, and how this decoder differs from the hourly one (§3.2)
//!
//! A record is 20 bytes, big-endian `>IIIff`: the millisecond offset, ask points, bid points, ask
//! volume, bid volume. `crates/bridges/dukascopy/src/data.rs`'s `decode_ticks` (the HTTP lane's)
//! shares the layout and differs in four ways, each deliberate:
//!
//! 1. **The base is the PATH's UTC midnight** ([`Bi5Day`]), never anything in the content.
//! 2. **The fields are UNSIGNED**, as the vendor documents; that decoder reads them signed (the
//!    design's F3). The two agree below 2^31 — any FX-scaled price under 21,474.84 — so they part
//!    only on an instrument the scale table does not admit yet.
//! 3. **A payload that is not a whole number of records is REFUSED as corrupt**, where that decoder's
//!    `chunks_exact` drops the trailing partial record without a word.
//! 4. **The content is checked against its time base (below) and decompression is bounded (§5)**;
//!    that decoder checks nothing, and its `decompress` has no output bound.
//!
//! It produces the same [`Tick`] that decoder does, for `crates/bridges/dukascopy/src/data.rs`'s
//! `tick_to_quote` to map: an identical record decodes to bit-identical `f64`s whichever lane read
//! it, which is what makes the lanes' one-owner-per-day rule honest (a test pins it).
//!
//! **Volumes** are passed through raw — the record's `f32`, widened — exactly as the HTTP lane
//! stores them. Their unit, per the vendor page, is **millions of base currency units**; neither
//! lane converts them.
//!
//! # The time base — decided by the path, checked on the content, refused on contradiction (§3.3)
//!
//! The vendor says older files "may use ms since start of hour instead of ms since start of day"
//! and that neither of its own decoders detects it. This one does, and answers ambiguity with
//! refusal — it never repairs an order, re-bases an offset or guesses an hour:
//!
//! 1. every offset is below 86,400,000, or the file is [`Bi5Refusal::OutOfRange`];
//! 2. offsets never DECREASE — equal ones are fine, several ticks can share a millisecond — and a
//!    decrease, the signature of hour-relative offsets restarting at 0, is
//!    [`Bi5Refusal::NotMonotonic`];
//! 3. a file whose EVERY tick lies in the day's first hour is [`Bi5Refusal::AmbiguousTimeBase`]:
//!    exactly what one hour-relative payload filed under a day name looks like, and nothing in it
//!    says WHICH hour it is. A genuine FX day almost never looks like it (Sunday's ticks start in
//!    the evening, UTC), and a false positive costs one refused, reported day;
//! 4. an EMPTY payload decodes to no ticks — it stores nothing and must spend no key.
//!
//! # The bounds (§5)
//!
//! [`Bi5Limits`] carries the caps, and every one of them is a DEFAULT until the acceptance run (T7)
//! measures the busiest real day. The decoded cap is enforced twice, because `lzma-rs`'s own
//! `memlimit` bounds only its DICTIONARY window and never its output: the header's declared size is
//! refused before anything is decoded, the dictionary window is held to the cap, and the output goes
//! through a writer that refuses the write that would pass the cap — so nothing is allocated past it.

#[cfg(doc)]
use self::{
    limits::Bi5Limits,
    path::{Bi5Day, Bi5Path, classify_bi5_path, is_bi5_layout_dir},
    refusal::Bi5Refusal,
};
#[cfg(doc)]
use crate::data::Tick;

pub(crate) mod decode;
pub(crate) mod limits;
pub(crate) mod path;
pub(crate) mod refusal;

#[cfg(test)]
mod tests;
