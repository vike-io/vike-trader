//! Load/save the [`PaceBook`] as JSON, and the per-run [`PaceSession`] that carries one observation
//! from disk into a pager and back — the I/O half of `vike_model::rate_limits`' measured-pace types.
//!
//! **Why this exists.** Every backfill MEASURES its own pace (the wall clock of a request, the
//! weight one request costs — `vike_bridge_core::pacer`) and then discards it at process exit. Run
//! N+1 re-derives both from zero, opening on a pessimistic constant: on binance SPOT, where a page
//! costs weight 2, the pacer's seed of 5 paces the first pages 2.5x slower than the venue permits,
//! and the ETA a multi-minute backfill prints is unavailable until the first page has returned.
//! This module is the missing sentence: the measurement is written down, and the next run reads it.
//!
//! **Why HERE and not in a library.** `vike_model` is pure (no I/O) and `vike_bridge_core::pacer`
//! holds no clock and no file — both by contract. The workspace rule is that libraries take
//! configuration as parameters and only binaries read the environment or the filesystem, so the file
//! lands in the crate whose bins own the backfill. The shape is lifted verbatim from
//! `vike_alerting::persist`, including its never-brick law.
//!
//! **Never-brick.** A missing file, an unreadable one, and an unparseable one are all "no record" —
//! [`load_path`] returns an EMPTY book and logs, never an error. That is not politeness: the book is
//! a cache whose entire purpose is to make a backfill open faster, and a backfill that refuses to
//! run because a cache file is corrupt has turned an optimisation into an outage. The same applies
//! on the way out — [`PaceSession::save`] logs a write failure and returns, because a backfill that
//! fetched its data successfully has not failed just because it could not write a hint for next time.
//!
//! **Strictly additive.** No file ⇒ an empty book ⇒ nothing to seed with ⇒ every pacer starts
//! exactly where it always did. A record older than [`PaceSession`]'s max age is ignored rather than
//! trusted, and one measured against a different budget is refused by the pacer itself
//! (`Pacer::seed`), so a stale or foreign record can only ever be a no-op — never a faster pace.

use std::path::{Path, PathBuf};

use vike_model::rate_limits::{MeasuredPace, PaceBook, PaceSample, DEFAULT_MAX_PACE_AGE_MS};

/// Read + parse the pace file at a CALLER-SUPPLIED path — the only load seam, because the binary
/// owns the path resolution (see [`crate::cli::pace_book_path`], which resolves `$VIKE_PACE_BOOK`).
///
/// An EMPTY book when the file is absent, unreadable or unparseable — never an error, never a
/// panic. Each of those means the same thing to every caller ("nothing measured yet"), and the only
/// cost is one run's opening pages.
pub fn load_path(p: &Path) -> PaceBook {
    let Ok(raw) = std::fs::read_to_string(p) else {
        return PaceBook::default();
    };
    match serde_json::from_str::<PaceBook>(&raw) {
        Ok(book) => book,
        Err(e) => {
            tracing::warn!(path = %p.display(), "pace file unparseable, ignoring (no record): {e}");
            PaceBook::default()
        }
    }
}

/// Serialize + write the book to a CALLER-SUPPLIED path — the save twin of [`load_path`], public for
/// the same reason (the caller, not this module, decides where the file lives).
///
/// Pretty-printed because an operator reads and sometimes hand-edits this file; the book's
/// insertion-ordered map means an unchanged book re-serializes to identical bytes, so a diff shows
/// only what actually moved. Creates the parent directory when it is missing — the store root
/// normally exists by the time anything is measured, but a caller that pointed `$VIKE_PACE_BOOK` at
/// a fresh directory should not lose the record to an `ENOENT`.
pub fn save_path(book: &PaceBook, p: &Path) -> std::io::Result<()> {
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(book).map_err(std::io::Error::other)?;
    std::fs::write(p, json)
}

/// One backfill run's view of the pace file: which `(venue, market)` it is about, what the previous
/// run measured, and where to write what THIS run measures.
///
/// Deliberately a struct with a lifecycle rather than three loose functions, because the three steps
/// are one contract and getting any of them alone wrong is silent: load before the fetch, seed FROM
/// the loaded record, record back INTO the same key. It holds the whole book (not just its own row)
/// so a save never drops the other venues' rows — the file is shared by every backfill against this
/// store.
#[derive(Debug, Clone)]
pub struct PaceSession {
    book: PaceBook,
    path: PathBuf,
    venue: String,
    market: String,
    now_ms: i64,
    max_age_ms: i64,
    /// Set by [`Self::record`]. [`Self::save`] is a no-op without it, so a run that measured nothing
    /// never rewrites the file — which keeps `measured_at_ms` honest (a no-op run must not refresh
    /// another run's timestamp and make a stale record look current).
    dirty: bool,
}

impl PaceSession {
    /// Open the session for `(venue, market)` at `path`, reading the file if it exists.
    ///
    /// `now_ms` and `max_age_ms` are parameters, not reads: the clock and the staleness policy
    /// belong to the caller. [`Self::open`] is the ordinary entry point and supplies
    /// `vike_model::now_ms()` plus [`DEFAULT_MAX_PACE_AGE_MS`].
    pub fn open_at(
        path: PathBuf,
        venue: impl Into<String>,
        market: impl Into<String>,
        now_ms: i64,
        max_age_ms: i64,
    ) -> Self {
        PaceSession {
            book: load_path(&path),
            path,
            venue: venue.into(),
            market: market.into(),
            now_ms,
            max_age_ms,
            dirty: false,
        }
    }

    /// [`Self::open_at`] with the wall clock and [`DEFAULT_MAX_PACE_AGE_MS`] — what a bin wants.
    pub fn open(path: PathBuf, venue: impl Into<String>, market: impl Into<String>) -> Self {
        Self::open_at(path, venue, market, vike_model::now_ms(), DEFAULT_MAX_PACE_AGE_MS)
    }

    /// A session that will never seed and never write — the shape for a venue that measures nothing.
    ///
    /// Distinct from `Option<PaceSession>` at every call site: a pager that takes a session
    /// unconditionally cannot forget the `None` arm, and a venue opting out says so once, here,
    /// instead of at each of its own call sites.
    pub fn disabled(venue: impl Into<String>, market: impl Into<String>) -> Self {
        PaceSession {
            book: PaceBook::default(),
            path: PathBuf::new(),
            venue: venue.into(),
            market: market.into(),
            now_ms: 0,
            max_age_ms: 0,
            dirty: false,
        }
    }

    /// The previous run's measurement for this session's `(venue, market)`, if there is one that is
    /// young enough to trust. `None` for an absent, unusable, expired or future-dated record — and
    /// `None` is byte-identical to the pre-persistence behaviour at every consumer.
    ///
    /// A `Some` here is still not permission: the pacer independently refuses a sample whose
    /// `budget_per_min` is not the one it discovered, which is what keeps the venue's LIMIT a
    /// property of discovery and this record a hint about SPEED only.
    pub fn seed(&self) -> Option<PaceSample> {
        self.book
            .fresh(&self.venue, &self.market, self.now_ms, self.max_age_ms)
            .map(MeasuredPace::sample)
    }

    /// Fold THIS run's measurement into the book (the merge/EWMA law is `PaceBook::record`'s).
    ///
    /// `None` — a run that timed nothing — is a no-op, and specifically does NOT mark the session
    /// dirty: an empty window or a fully-deduped re-run must not be able to re-stamp an old record
    /// as freshly measured.
    pub fn record(&mut self, sample: Option<PaceSample>) {
        let Some(sample) = sample.filter(PaceSample::is_usable) else { return };
        if self.path.as_os_str().is_empty() {
            return; // a `disabled` session holds no file to write to
        }
        self.book.record(MeasuredPace::new(&self.venue, &self.market, sample, self.now_ms));
        self.dirty = true;
    }

    /// Write the book back, BEST EFFORT: a no-op unless [`Self::record`] took something, and a
    /// logged warning (never an error, never a panic) if the write fails.
    ///
    /// A backfill that fetched and ingested its data has succeeded. Failing it here would trade a
    /// completed job for an unwritten cache file, which is the wrong way round.
    pub fn save(&self) {
        if !self.dirty {
            return;
        }
        match save_path(&self.book, &self.path) {
            Ok(()) => tracing::debug!(
                path = %self.path.display(),
                venue = %self.venue,
                market = %self.market,
                "measured pace persisted"
            ),
            Err(e) => tracing::warn!(
                path = %self.path.display(),
                "could not write the pace file (the backfill itself is unaffected): {e}"
            ),
        }
    }

    /// The file this session reads and writes — for a caller that wants to name it in a log line.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `(venue, market)` this session is keyed on.
    pub fn key(&self) -> (&str, &str) {
        (&self.venue, &self.market)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(request_ms: u64, weight: f64, budget: Option<u64>) -> PaceSample {
        PaceSample { request_ms, per_request_weight: weight, budget_per_min: budget, samples: 4 }
    }

    /// The whole lifecycle across two "runs", plus the two never-brick cases the loader owes:
    /// a MISSING file and a CORRUPT one both mean "no record", never an error.
    #[test]
    fn disk_round_trip_and_missing_and_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pace.json");

        // missing file → an empty book (the OFF state), never an error.
        assert!(load_path(&p).is_empty());

        // RUN 1: nothing to seed with, and its measurement is written.
        let mut run1 = PaceSession::open_at(p.clone(), "binance", "perp", 1_000, 60_000);
        assert_eq!(run1.seed(), None, "the first run has nothing to start from");
        run1.record(Some(sample(280, 5.0, Some(2400))));
        run1.save();
        assert!(p.exists(), "the measurement reached the disk");

        // RUN 2: opens on run 1's numbers.
        let run2 = PaceSession::open_at(p.clone(), "binance", "perp", 2_000, 60_000);
        let seed = run2.seed().expect("run 2 starts from run 1's measurement");
        assert_eq!(seed.request_ms, 280);
        assert_eq!(seed.per_request_weight, 5.0);
        assert_eq!(seed.budget_per_min, Some(2400));

        // A DIFFERENT market on the same venue is a different row and does not see it.
        assert_eq!(PaceSession::open_at(p.clone(), "binance", "spot", 2_000, 60_000).seed(), None);
        // ...and a different venue likewise.
        assert_eq!(PaceSession::open_at(p.clone(), "bybit", "perp", 2_000, 60_000).seed(), None);

        // A record older than the session's max age is IGNORED, not trusted.
        assert_eq!(
            PaceSession::open_at(p.clone(), "binance", "perp", 999_000, 60_000).seed(),
            None
        );

        // corrupt file → an empty book (never bricks), same law as `vike_alerting::persist`.
        std::fs::write(&p, "{ not valid json ]").unwrap();
        assert!(load_path(&p).is_empty());
        assert_eq!(PaceSession::open_at(p.clone(), "binance", "perp", 2_000, 60_000).seed(), None);
        // ...and a run over a corrupt file still records and repairs it.
        let mut run3 = PaceSession::open_at(p.clone(), "binance", "perp", 3_000, 60_000);
        run3.record(Some(sample(300, 5.0, Some(2400))));
        run3.save();
        assert_eq!(load_path(&p).get("binance", "perp").unwrap().request_ms, 300);
    }

    /// A save must never drop another venue's row — the file is shared by every backfill against
    /// this store, so the session holds the whole book and writes it back whole.
    #[test]
    fn saving_one_venue_preserves_every_other_row() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pace.json");

        let mut a = PaceSession::open_at(p.clone(), "binance", "spot", 1_000, 60_000);
        a.record(Some(sample(120, 2.0, Some(6000))));
        a.save();

        let mut b = PaceSession::open_at(p.clone(), "binance", "perp", 1_100, 60_000);
        b.record(Some(sample(280, 5.0, Some(2400))));
        b.save();

        let book = load_path(&p);
        assert_eq!(book.len(), 2, "the second save kept the first row");
        assert_eq!(book.get("binance", "spot").unwrap().per_request_weight, 2.0);
        assert_eq!(book.get("binance", "perp").unwrap().per_request_weight, 5.0);
    }

    /// A run that measured NOTHING must not touch the file — otherwise an empty/already-ingested
    /// window would re-stamp a stale record as freshly measured and keep it alive forever.
    #[test]
    fn a_run_that_measured_nothing_never_rewrites_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pace.json");

        let mut seeded = PaceSession::open_at(p.clone(), "binance", "perp", 1_000, 60_000);
        seeded.record(Some(sample(280, 5.0, Some(2400))));
        seeded.save();
        let before = std::fs::read_to_string(&p).unwrap();

        // `None` (nothing timed) and an UNUSABLE sample are both no-ops, file untouched.
        for nothing in [None, Some(sample(0, 5.0, Some(2400))), Some(sample(280, 0.0, None))] {
            let mut run = PaceSession::open_at(p.clone(), "binance", "perp", 500_000, 60_000);
            run.record(nothing);
            run.save();
            assert_eq!(
                std::fs::read_to_string(&p).unwrap(),
                before,
                "{nothing:?} rewrote the file"
            );
        }
        // The stored timestamp is therefore still run 1's, and the record ages out on schedule.
        assert_eq!(load_path(&p).get("binance", "perp").unwrap().measured_at_ms, 1_000);
    }

    /// A `disabled` session is inert in both directions and writes no file at all — the shape a
    /// venue that measures nothing passes through the shared CLI body.
    #[test]
    fn a_disabled_session_never_seeds_and_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pace.json");
        // Prove it is not merely "no file yet": there IS a good record on disk at the default path.
        let mut real = PaceSession::open_at(p.clone(), "bybit", "spot", 1_000, 60_000);
        real.record(Some(sample(90, 1.0, None)));
        real.save();

        let mut off = PaceSession::disabled("bybit", "spot");
        assert_eq!(off.seed(), None);
        assert_eq!(off.key(), ("bybit", "spot"));
        assert_eq!(off.path(), Path::new(""));
        off.record(Some(sample(90, 1.0, None)));
        off.save(); // must not panic, must not create anything
        assert_eq!(load_path(&p).len(), 1, "the on-disk book is untouched by a disabled session");
    }

    /// `save_path` creates a missing parent rather than losing the record to an `ENOENT` — the
    /// `$VIKE_PACE_BOOK`-points-at-a-fresh-directory case.
    #[test]
    fn save_path_creates_a_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("deeper").join("pace.json");
        let mut book = PaceBook::default();
        book.record(MeasuredPace::new("binance", "perp", sample(280, 5.0, Some(2400)), 1_000));
        save_path(&book, &p).expect("a missing parent is created, not an error");
        assert_eq!(load_path(&p), book);
    }
}
