//! Fetch paging and reassembly — the server-agnostic half of "a reply that can exceed the frame
//! ceiling is paged".
//!
//! A bar series can reach ~64 MiB and a dukascopy day of ticks is the same order, so NO fetch reply
//! is one frame (drift register R9): the daemon splits the rows with [`pages_of`] (one page at a time,
//! so the split never doubles the answer's memory), writes them as
//! [`Response::FetchPage`](super::Response::FetchPage) frames and closes with a
//! [`Response::FetchDone`](super::Response::FetchDone) carrying the totals; the client feeds every
//! page to a [`FetchAssembler`], which checks the numbering and the totals and hands back the `Vec`
//! the in-process seam returns. Both halves live here so they cannot disagree about what a page is.
//!
//! ⚠ A short answer is never silent. A page lost or duplicated on the way is `page N followed page M`;
//! a terminal frame whose totals disagree with what arrived is an error naming both — the client
//! turns either into a `Fetch("gateway ...")`, not into a shorter result.

use std::io::{self, Write};

use serde::Serialize;
use vike_marketdata::Bar;
use vike_marketdata::QuoteTick;
use vike_model::Instrument;

use super::{FetchRows, WireFundingPoint};

/// The bytes a [`Response::FetchPage`](super::Response::FetchPage) frame adds around its rows (tags,
/// `rid`, `page`, brackets) — a generous allowance, so a page that [`pages_of`] fills to the ceiling
/// still frames under the server-frame ceiling.
const PAGE_ENVELOPE_BYTES: usize = 256;

/// A `Write` that only counts, so a row's JSON size is measured without allocating it.
struct Counter(usize);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The JSON size of `row`, in bytes. A row that does not serialize counts as zero: the frame write
/// that follows fails loudly, and a size is not the place to report it.
fn json_len<T: Serialize>(row: &T) -> usize {
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, row).map_or(0, |()| counter.0)
}

/// Split `rows` into pages whose JSON payload is at most `ceiling_bytes` (plus a small envelope
/// allowance), preserving order, with AT LEAST ONE ROW per page — a single row larger than the
/// ceiling is its own page, never split. No rows is no pages (`FetchDone { pages: 0 }` is a
/// legitimate empty result).
///
/// ⚠ **The pages are made ONE AT A TIME, as the iterator is pulled.** The whole answer is already in
/// memory (a lane returns it whole), and an eager split moved every row into a second set of `Vec`s
/// while the original buffer stayed allocated until the loop ended, so the daemon's peak for a
/// 10-million-bar answer was twice the answer. Here only the page being built exists beside the
/// original buffer, and a page the caller has written and dropped is gone before the next is made.
/// The page boundaries are the same ones the eager split drew (`the_lazy_pager_draws_the_eager_splits_boundaries`
/// in `proto_tests.rs` holds a reference copy of it).
pub fn pages_of<T: Serialize>(rows: Vec<T>, ceiling_bytes: usize) -> Pages<T> {
    Pages { rows: rows.into_iter(), carry: None, ceiling_bytes }
}

/// The pages of one answer, made as they are pulled. See [`pages_of`].
#[derive(Debug)]
pub struct Pages<T> {
    rows: std::vec::IntoIter<T>,
    /// The row that did not fit the page just returned, with its JSON size: the first row of the next.
    carry: Option<(T, usize)>,
    ceiling_bytes: usize,
}

impl<T: Serialize> Iterator for Pages<T> {
    type Item = Vec<T>;

    fn next(&mut self) -> Option<Vec<T>> {
        let mut page: Vec<T> = Vec::new();
        let mut bytes = PAGE_ENVELOPE_BYTES;
        while let Some((row, n)) = self.carry.take().or_else(|| {
            // +1 for the separating comma.
            self.rows.next().map(|row| {
                let n = json_len(&row) + 1;
                (row, n)
            })
        }) {
            if !page.is_empty() && bytes + n > self.ceiling_bytes {
                self.carry = Some((row, n));
                return Some(page);
            }
            bytes += n;
            page.push(row);
        }
        (!page.is_empty()).then_some(page)
    }
}

/// The client's reassembly of one fetch: pages in, one `Vec` out.
///
/// Feed every [`Response::FetchPage`](super::Response::FetchPage) to [`FetchAssembler::push`] in
/// arrival order, then close with the terminal frame's totals through one of the `finish_*` methods.
#[derive(Debug)]
pub struct FetchAssembler {
    rid: u64,
    next_page: u32,
    rows: Option<FetchRows>,
}

impl FetchAssembler {
    /// An empty assembly for the fetch `rid`.
    pub fn new(rid: u64) -> Self {
        Self { rid, next_page: 0, rows: None }
    }

    /// How many pages have been accepted.
    pub fn pages(&self) -> u32 {
        self.next_page
    }

    /// How many rows have been accepted.
    pub fn rows(&self) -> usize {
        self.rows.as_ref().map_or(0, FetchRows::len)
    }

    /// Accept one page. `Err` for a page of another fetch, a page out of order (lost or duplicated)
    /// or a page whose row kind differs from the pages before it.
    pub fn push(&mut self, rid: u64, page: u32, rows: FetchRows) -> Result<(), String> {
        if rid != self.rid {
            return Err(format!(
                "a page of fetch {rid} arrived on fetch {} (a connection carries one fetch at a time)",
                self.rid
            ));
        }
        if page != self.next_page {
            return Err(format!(
                "page {page} arrived where page {} was due — a page was lost or repeated",
                self.next_page
            ));
        }
        match &mut self.rows {
            None => self.rows = Some(rows),
            Some(acc) => acc.extend(rows)?,
        }
        self.next_page += 1;
        Ok(())
    }

    /// Verify the terminal frame's totals against what arrived and return the assembled rows, `None`
    /// when no page was written (an empty result).
    fn finish(self, pages: u32, rows: u64) -> Result<Option<FetchRows>, String> {
        let got_rows = self.rows() as u64;
        if self.next_page != pages || got_rows != rows {
            return Err(format!(
                "the fetch ended with {pages} pages / {rows} rows but {} pages / {got_rows} rows \
                 arrived — rows were lost on the way",
                self.next_page
            ));
        }
        Ok(self.rows)
    }

    /// Close a bars fetch. `Err` when the totals disagree or the pages were not bars.
    pub fn finish_bars(self, pages: u32, rows: u64) -> Result<Vec<Bar>, String> {
        match self.finish(pages, rows)? {
            None => Ok(Vec::new()),
            Some(FetchRows::Bars(v)) => Ok(v),
            Some(other) => Err(kind_mismatch("Bars", &other)),
        }
    }

    /// Close a funding fetch. `Err` when the totals disagree or the pages were not funding points.
    pub fn finish_funding(self, pages: u32, rows: u64) -> Result<Vec<WireFundingPoint>, String> {
        match self.finish(pages, rows)? {
            None => Ok(Vec::new()),
            Some(FetchRows::Funding(v)) => Ok(v),
            Some(other) => Err(kind_mismatch("Funding", &other)),
        }
    }

    /// Close a tick fetch. `Err` when the totals disagree or the pages were not quotes.
    pub fn finish_quotes(self, pages: u32, rows: u64) -> Result<Vec<QuoteTick>, String> {
        match self.finish(pages, rows)? {
            None => Ok(Vec::new()),
            Some(FetchRows::Quotes(v)) => Ok(v),
            Some(other) => Err(kind_mismatch("Quotes", &other)),
        }
    }

    /// Close an instrument-list fetch. `Err` when the totals disagree or the pages were not
    /// instruments.
    pub fn finish_instruments(self, pages: u32, rows: u64) -> Result<Vec<Instrument>, String> {
        match self.finish(pages, rows)? {
            None => Ok(Vec::new()),
            Some(FetchRows::Instruments(v)) => Ok(v),
            Some(other) => Err(kind_mismatch("Instruments", &other)),
        }
    }
}

fn kind_mismatch(wanted: &str, got: &FetchRows) -> String {
    format!("the fetch asked for {wanted} rows and the server sent {} pages", got.kind())
}
