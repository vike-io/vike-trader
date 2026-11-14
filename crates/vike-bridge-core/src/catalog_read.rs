//! The ONE catalog read loop the venues' `CatalogProvider` impls share (their instrument lists), in
//! two shapes: [`catalog_read_tolerant`], a list of reads where one failing never drops another's
//! rows, and [`CatalogPageWalk`], one listing followed page by page.
//!
//! **What stays in the venue** (owner 10-25: a one-venue fact is a parameter, never an
//! `if venue ==` here): the read list (its URLs and their order), the row parsers, how a page names
//! the next one (a cursor the venue hands back, or an offset the caller counts), the page ceiling,
//! and every log line, its `tracing` target and its words. A log target is a compile-time constant
//! of the callsite, so neither shape logs: the tolerant read hands each failure to the venue's
//! `skipped` callback and the walk hands back why it stopped ([`CatalogPageWalkEnd`]), and the venue
//! says it, as [`crate::per_symbol::RefusalLog`] does for a refused order. Nothing here fetches or
//! names `CatalogError`: the error is the venue's own type, passed through untouched, so a line the
//! venue writes from it reads byte for byte as before.
//!
//! **The tolerant read.** Each read's rows are kept, in read order; a read that fails goes to
//! `skipped` and the loop goes on, so one endpoint down (a spot outage) never wipes the venue's
//! other classes from the picker, and every failure is said. All of them failing is an empty list,
//! never an error: the picker's answer for a venue it cannot read is "nothing", and the venue's
//! lines say why.
//!
//! **The page walk.** [`CatalogPageWalk::walk`] reads the page at `first`, then the page each page
//! names as its `next`, until one names none ([`CatalogPageWalkEnd::Listed`]), reading at most
//! `max_pages` pages. The first page failing is always the walk's error: the listing is
//! unreachable. A later page failing ends the walk with that error when `strict` (a predicate that
//! must not answer from a short list), otherwise with the rows already read and
//! [`CatalogPageWalkEnd::Failed`] (a picker keeps what it has). A page that names the position just
//! read ends EVERY walk ([`CatalogPageWalkEnd::Repeated`]); that check comes before the ceiling's,
//! so a venue echoing one cursor forever stops at its second page. A walk that read `max_pages`
//! pages while the last still named a next one ends [`CatalogPageWalkEnd::Ceiling`], having read
//! no extra page to find out. The walk never decides what a short list means: the end says why it
//! stopped, and the venue answers (an error or a warn line).
//!
//! Every walk stops on a repeated position and every venue writes a warn line when its walk hits
//! the ceiling (owner ruling C-D2, 2026-11-12); tolerant or strict stays the venue's parameter:
//! - bybit's picker categories: tolerant, a repeat and the ceiling each a warn line;
//! - bybit's route listings: `strict`, a repeat and the ceiling each a warn line and an error in
//!   bybit's words (a short list must not answer);
//! - polymarket's Gamma browse: tolerant, an offset position (a next offset only after a full page,
//!   so it never repeats), the ceiling a warn line;
//! - polymarket's CLOB `/markets` walk: `strict`, the ceiling the caller's page count, a repeat and
//!   the ceiling each a warn line over the rows read.

/// Every read's rows, in read order. `read` fetches and parses one read; a read it fails is handed
/// to `skipped` (the venue's own line) and the loop goes on to the next.
pub fn catalog_read_tolerant<R, T, E>(
    reads: impl IntoIterator<Item = R>,
    mut read: impl FnMut(&R) -> Result<Vec<T>, E>,
    mut skipped: impl FnMut(&R, &E),
) -> Vec<T> {
    let mut out = Vec::new();
    for r in reads {
        match read(&r) {
            Ok(rows) => out.extend(rows),
            Err(e) => skipped(&r, &e),
        }
    }
    out
}

/// One page of a listing, as the venue's fetch read it: its rows, and the position of the page after
/// it (`None`: the venue says the listing ends here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogPage<K, T> {
    /// The page's rows, parsed.
    pub rows: Vec<T>,
    /// Where the next page starts: the cursor the venue handed back, or the next offset.
    pub next: Option<K>,
}

/// Why a [`CatalogPageWalk`] stopped. `page` counts from 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogPageWalkEnd<E> {
    /// A page named no next one: the venue's end of the listing.
    Listed,
    /// Page `page` failed with `error` (a tolerant walk only); the rows are the pages before it.
    Failed { page: usize, error: E },
    /// Page `page` named the position it was read at as its next one.
    Repeated { page: usize },
    /// `max_pages` pages were read and the last one still named a next one.
    Ceiling,
}

/// The rules of one paged listing walk (see the module doc); [`Self::walk`] runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogPageWalk {
    /// The most pages one walk reads. 0 reads none and ends [`CatalogPageWalkEnd::Ceiling`].
    pub max_pages: usize,
    /// A page after the first that fails: `true` ends the walk with that error, `false` keeps the
    /// rows already read ([`CatalogPageWalkEnd::Failed`]).
    pub strict: bool,
}

impl CatalogPageWalk {
    /// Read the listing from `first`: `fetch` reads and parses the page at one position. Answers the
    /// rows of every page read, in order, with why the walk stopped.
    ///
    /// # Errors
    /// The first page's failure, or (on a `strict` walk) any page's.
    pub fn walk<K: PartialEq, T, E>(
        &self,
        first: K,
        mut fetch: impl FnMut(&K) -> Result<CatalogPage<K, T>, E>,
    ) -> Result<(Vec<T>, CatalogPageWalkEnd<E>), E> {
        let mut rows = Vec::new();
        let mut at = first;
        for page in 0..self.max_pages {
            let read = match fetch(&at) {
                Ok(read) => read,
                Err(error) if page == 0 || self.strict => return Err(error),
                Err(error) => return Ok((rows, CatalogPageWalkEnd::Failed { page, error })),
            };
            rows.extend(read.rows);
            match read.next {
                None => return Ok((rows, CatalogPageWalkEnd::Listed)),
                Some(next) if next == at => {
                    return Ok((rows, CatalogPageWalkEnd::Repeated { page }));
                }
                Some(next) => at = next,
            }
        }
        Ok((rows, CatalogPageWalkEnd::Ceiling))
    }
}

#[cfg(test)]
#[path = "catalog_read_tests.rs"]
mod catalog_read_tests;
