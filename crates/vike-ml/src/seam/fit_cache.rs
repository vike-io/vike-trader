//! The CROSS-RUN FIT CACHE: a (training bytes, hyperparameter-point, seed, validation bytes,
//! objective) validation score that was already computed by a previous run is not refitted.
//!
//! # Why it exists, measured
//!
//! Fitting is the whole cost of a hyperparameter search. One 20-trial cohort run (measured on
//! `crates/vike-research/src/cli.rs`'s `run_cohort`, since deleted) is ~1,591 LightGBM fits at a
//! p50 of 6.9s each; spawn+I/O is 2.3% of a fit, so the only lever is FEWER FITS. Cross-run
//! duplication is real for any search: a seed-deterministic TPE startup deck means a 20-trial and
//! a 50-trial run at the same search seed share every startup point per fold, seed sweeps overlap
//! on grid points, and a re-run of an identical config duplicates everything.
//!
//! # The design constraint that outranks everything
//!
//! **A wrong cache hit silently corrupts results and is worse than no cache.** The key therefore
//! CONTENT-ADDRESSES the actual bytes the fit consumes, never descriptions of them (a path, a fold
//! id, a config name). The key is a SHA-256 over three parts:
//!
//! * **the fit identity** — [`crate::Learner::fit_identity`], produced by the learner because only
//!   the learner knows what its trainer eats. For a real LightGBM learner
//!   (`crates/vike-ml/src/train/gbdt_learner.rs`'s `GbdtLearner`) it covers the training CSV BYTES
//!   exactly as [`TrainData::write_csv`] streams them, both RENDERED LightGBM configs, the model
//!   text-format version and the trainer's PROVENANCE file bytes — its `fit_identity` doc is the
//!   enumeration. A learner that cannot prove its inputs' identity answers `None` and is simply
//!   never cached.
//! * **the validation half's own bytes** — `n_rows`, `n_cols`, every feature's bit pattern and
//!   every label: the EXACT in-process input to [`crate::ProbaModel::predict_proba`] and to
//!   whatever scores its output.
//! * **the objective identity** — an opaque digest supplied by the CALLER, for exactly the reason
//!   the fit identity is supplied by the learner: only the code that owns the objective knows what
//!   its score consumes. [`Transcript`] and [`DigestWriter`] are what a caller builds it with, so
//!   the framing is shared even though the content is not. See [`score_key`].
//!
//! ⚠ **The cache is exactly as reproducible as its INPUTS, a property of the data's producer.** A
//! source that does not answer one request the same way twice (parallel-reduction noise in a
//! server-side aggregate, say) gives every re-run a fresh key for every fit and grows a directory
//! that is never read. The cure is to canonicalise AT THE PARSE BOUNDARY
//! (`crates/vike-backfill/src/vikedata/parse.rs`'s `canonical_notional` carries the measurement) —
//! ⚠ never here: snapping inside the key would declare two genuinely different datasets
//! equivalent, which is the wrong hit this design refuses.
//!
//! Anything NOT in the key is enumerated at the sites that build it, with the one declared
//! residual: the pure-Rust inference ([`crate::parse_model_text`] / `predict_proba`) and the
//! scorer bodies are CODE, not data — a change to them changes scores without changing any keyed
//! input. [`KEY_SCHEMA`] (and the fit-identity schema tag beside each `fit_identity` impl) exists
//! to be BUMPED by such a change.
//!
//! # What this module makes the crate own, and what it does not
//!
//! * **A HASH.** [`digest_of`], [`Transcript`] and [`DigestWriter`] cost `sha2` and `hex`, both
//!   already linked into every binary in this workspace through `vike-bridge-core`'s signer — no
//!   new supply-chain surface.
//! * **NO objective and NO scorer.** [`score_key`] takes the objective as a digest it does not
//!   compute: `vike-analytics`, where the scorers live, declares the same layer as this crate, so
//!   the layer gate forbids the edge and generality here is a requirement.
//! * **NO `fit_identity` from its own test double** — `crate::seam::test_support`'s module doc carries
//!   the argument.
//!
//! # The value, the location, the failure modes
//!
//! The value is the validation SCORE — one `f64` — never the model file: only the score enters an
//! argmin, and a caller's final refit (whose model IS consumed) is deliberately not cached.
//! Entries live one-file-per-key in a directory that survives across runs
//! ([`FitCache::dir_beside`]: a sibling of the caller's scratch root, so wiping scratch does not
//! wipe the cache). A corrupt or uninterpretable entry is a MISS — deleted and refitted — never an
//! error: a cache must not be able to fail a run. A NON-FINITE score is never persisted, because a
//! caller that folds a deterministically rejected point and a TRANSIENT spawn/disk failure into
//! the same `INFINITY` would otherwise make the transient permanent for that key.
//!
//! Two threads may race on one key; both store equal values BY CONSTRUCTION, so last-write-wins
//! through [`FitCache::store`]'s atomic rename is benign (the Windows fallback is stated there).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::train::TrainData;

/// The score key's domain tag. BUMP IT when the meaning of a stored score changes without any
/// keyed byte changing — i.e. when the in-process inference or a scorer body changes semantics —
/// so every old entry becomes an ordinary miss instead of a silently stale hit.
///
/// ⚠ **FROZEN at a spelling that names a deleted crate, deliberately**: it is a domain-separation
/// token, and renaming it re-keys every on-disk cache (`crates/vike-ml/CLAUDE.md`'s "The learner
/// seam, the double and the fit cache"). `the_key_bytes_are_frozen` makes such a change
/// deliberate.
pub const KEY_SCHEMA: &str = "vike-research fit-cache v1";

/// The entry file's first line. A file that does not start with it is not an entry.
const MAGIC: &str = "vike-fit-cache v1";

/// SHA-256 of one byte string.
pub fn digest_of(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A domain-separated SHA-256 transcript: the tag, then each part's own SHA-256.
///
/// Every part enters as its fixed-width DIGEST rather than as raw bytes, so two part sequences
/// with equal concatenations can never collide (`part(b"ab"), part(b"c")` vs
/// `part(b"a"), part(b"bc")`) — the framing a plain running hash silently lacks.
pub struct Transcript {
    h: Sha256,
}

impl Transcript {
    pub fn new(tag: &str) -> Self {
        let mut h = Sha256::new();
        h.update((tag.len() as u64).to_le_bytes());
        h.update(tag.as_bytes());
        Self { h }
    }

    /// One part, hashed whole.
    pub fn part(&mut self, bytes: &[u8]) {
        self.digest_part(digest_of(bytes));
    }

    /// One part whose digest the caller already holds (a streamed part, or another transcript).
    pub fn digest_part(&mut self, d: [u8; 32]) {
        self.h.update(d);
    }

    pub fn finish(self) -> [u8; 32] {
        self.h.finalize().into()
    }
}

/// An [`io::Write`] that hashes instead of storing — what lets an ~8 MB training CSV enter a key
/// as its exact bytes without ever materialising.
pub struct DigestWriter {
    h: Sha256,
}

impl DigestWriter {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self { h: Sha256::new() }
    }

    pub fn u64(&mut self, v: u64) {
        self.h.update(v.to_le_bytes());
    }

    /// The BIT PATTERN, not the value: `-0.0` and `0.0` compare equal as floats but are different
    /// bytes on every wire a trainer reads, and NaN must hash stably.
    pub fn f64_bits(&mut self, v: f64) {
        self.h.update(v.to_bits().to_le_bytes());
    }

    pub fn finish(self) -> [u8; 32] {
        self.h.finalize().into()
    }
}

impl io::Write for DigestWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.h.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The full score key: fit identity ⊕ validation bytes ⊕ objective identity.
///
/// What each part covers is this function's body plus the learner's
/// [`crate::Learner::fit_identity`] plus whatever the caller hashed into `objective_identity`.
/// `va` arrives ALREADY narrowed to the validation half, and `objective_identity` must be narrowed
/// the same way, so the train/validation cut needs no component of its own: a different cut is
/// different bytes on both parts.
///
/// ⚠ **The objective is a DIGEST, not a value** (the module doc says why): the caller hashes its
/// objective's identity and every parameter it scores with, using [`Transcript`] /
/// [`DigestWriter`]. Same contract as [`crate::Learner::fit_identity`]: two calls may share a
/// digest ONLY when their scores are equal by construction.
///
/// ⚠ **The three parts and their order are FROZEN** — they are the address of every entry in every
/// existing on-disk cache. Changing either is a [`KEY_SCHEMA`] bump with the full re-fit cost.
pub fn score_key(
    fit_identity: &[u8; 32],
    va: &TrainData<'_>,
    objective_identity: &[u8; 32],
) -> [u8; 32] {
    let mut t = Transcript::new(KEY_SCHEMA);
    t.digest_part(*fit_identity);

    // The validation half: the exact in-process input to `predict_proba` (row-major features,
    // width) and to the scorer (labels). Lengths lead, so the stream is self-delimiting.
    let mut va_part = DigestWriter::new();
    va_part.u64(va.n_rows as u64);
    va_part.u64(va.n_cols as u64);
    for v in va.x {
        va_part.f64_bits(*v);
    }
    let mut w = va_part; // labels ride the same part, after the features the counts describe
    for y in va.y {
        // The BIT PATTERN of the widened label, for the reason `f64_bits` exists: a label is an
        // `f32` ([`TrainData`]'s own width), and hashing a float by value would make `-0.0` and
        // `0.0` the same key.
        w.f64_bits(f64::from(*y));
    }
    t.digest_part(w.finish());

    t.digest_part(*objective_identity);
    t.finish()
}

/// Hit/miss counts over the CACHEABLE fits of one run (a learner with no
/// [`crate::Learner::fit_identity`], or a cache-off run, counts nothing). Printed on a run summary
/// line so reuse is visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FitCacheStats {
    pub hits: u64,
    pub misses: u64,
}

/// The on-disk cache: one file per key, named by the key's hex, atomically replaced.
pub struct FitCache {
    dir: PathBuf,
    seq: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl FitCache {
    /// Where the cache lives for a given scratch root: a SIBLING of it, named `name`.
    ///
    /// Beside the scratch root rather than under it, so it survives a scratch wipe and every run
    /// shares one pool (safe: the key carries the whole identity).
    ///
    /// ⚠ `name` is the CALLER's: it is part of an application's on-disk layout, and renaming it
    /// orphans a user's existing cache exactly as a [`KEY_SCHEMA`] bump does.
    pub fn dir_beside(scratch: &Path, name: &str) -> PathBuf {
        // `parent()` of a single-component relative path is `Some("")`, and `"".join(x)` is `x` —
        // the sibling in the current directory, which is exactly right. Only a filesystem ROOT
        // has no parent at all, and there the join-under-scratch fallback is the only place left.
        scratch.parent().unwrap_or(scratch).join(name)
    }

    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("creating the fit-cache directory {}: {e}", dir.display()))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            seq: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        })
    }

    fn entry_path(&self, key: &[u8; 32]) -> PathBuf {
        self.dir.join(format!("{}.score", hex::encode(key)))
    }

    /// The stored score, or `None` — and a present-but-uninterpretable entry is DELETED on the
    /// way to `None`, so one corrupt file costs one refit rather than erroring every run forever.
    pub fn lookup(&self, key: &[u8; 32]) -> Option<f64> {
        let path = self.entry_path(key);
        let text = std::fs::read_to_string(&path).ok()?;
        match parse_entry(&text) {
            Some(v) => Some(v),
            None => {
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Persist one score. Best-effort by design: every failure path leaves either no entry or a
    /// correct entry, and none of them can fail the run. A NON-FINITE score is refused (see the
    /// module doc).
    pub fn store(&self, key: &[u8; 32], score: f64) {
        if !score.is_finite() {
            return;
        }
        let tmp = self.dir.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            self.seq.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::write(&tmp, entry_bytes(score)).is_err() {
            return;
        }
        let dest = self.entry_path(key);
        // ⚠ Windows refuses a rename over an existing file (POSIX replaces), so the fallback
        // removes the destination — an EQUAL value, since racers share inputs — and retries once;
        // losing even that race leaves an equal entry, so every error here is ignored.
        if std::fs::rename(&tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&dest);
            let _ = std::fs::rename(&tmp, &dest);
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// ⚠ `pub`: the search loop that decides a hit is a hit lives in a caller's crate, and
    /// [`FitCache::lookup`] cannot tell a genuine miss from a caller that never consulted it.
    pub fn note_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// The twin of [`FitCache::note_hit`], with the same reason for being `pub`.
    pub fn note_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn stats(&self) -> FitCacheStats {
        FitCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}

fn entry_bytes(score: f64) -> String {
    format!("{MAGIC}\n{:016x}\n", score.to_bits())
}

/// Strict, whole-file parse: the magic line, exactly 16 hex digits, one trailing newline, nothing
/// else, and the bits must decode to a FINITE f64 (`store` never writes anything else, so a
/// non-finite value here is corruption wearing a valid spelling).
fn parse_entry(text: &str) -> Option<f64> {
    let rest = text.strip_prefix(MAGIC)?.strip_prefix('\n')?;
    let hex_bits = rest.strip_suffix('\n')?;
    if hex_bits.len() != 16 || !hex_bits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = f64::from_bits(u64::from_str_radix(hex_bits, 16).ok()?);
    v.is_finite().then_some(v)
}

#[path = "fit_cache_tests.rs"]
#[cfg(test)]
mod fit_cache_tests;
