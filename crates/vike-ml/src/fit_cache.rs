//! The CROSS-RUN FIT CACHE: a (training bytes, hyperparameter-point, seed, validation bytes,
//! objective) validation score that was already computed by a previous run is not refitted.
//!
//! # Why it exists, measured
//!
//! Fitting is the whole cost of a hyperparameter search. One 20-trial run of the study that first
//! needed this (`crates/vike-research/src/cli.rs`'s `run_cohort`, a DELETED driver — that crate
//! dissolved and its CLI had no successor; the study itself lives on at
//! `user_data/research/studies/rust/cohort/run.rs`) is ~1,591 LightGBM fits at a p50
//! of 6.9s each; spawn+I/O is 2.3% of a fit, so wall clock is `fits / parallelism` and the only
//! lever is FEWER FITS. Cross-run duplication is real for any search: a seed-deterministic TPE
//! startup deck means a 20-trial and a 50-trial run at the same search seed share every startup
//! point per fold, seed sweeps overlap on grid points, and a re-run of an identical config
//! duplicates everything.
//!
//! # The design constraint that outranks everything
//!
//! **A wrong cache hit silently corrupts results and is worse than no cache.** The key therefore
//! CONTENT-ADDRESSES the actual bytes the fit consumes, never descriptions of them (a path, a fold
//! id, a config name). The key is a SHA-256 over three parts:
//!
//! * **the fit identity** — [`crate::Learner::fit_identity`], produced by the learner because only
//!   the learner knows what its trainer eats. For a real LightGBM learner
//!   (`crates/vike-ml/src/gbdt_learner.rs`'s `GbdtLearner`) it covers: the
//!   training CSV BYTES exactly as [`TrainData::write_csv`] streams them to the binning (labels,
//!   features, NaN spelling, shortest-roundtrip f64 formatting — so a formatter change re-keys),
//!   the RENDERED train config and the RENDERED save-binary config with the two path lines
//!   normalised (every searched axis, the fit seed, the categorical declaration, every non-searched
//!   parameter, `num_threads`, `precise_float_parser`, and the `BinPreFilter` choice — so any
//!   change to what is written into a LightGBM config re-keys), the model text-format version, and
//!   the trainer binary's PROVENANCE file bytes. A learner that cannot prove its inputs' identity
//!   answers `None` and is simply never cached.
//! * **the validation half's own bytes** — `n_rows`, `n_cols`, every feature's bit pattern and
//!   every label: the EXACT in-process input to [`crate::ProbaModel::predict_proba`] and to
//!   whatever scores its output.
//! * **the objective identity** — an opaque digest supplied by the CALLER, for exactly the reason
//!   the fit identity is supplied by the learner: only the code that owns the objective knows what
//!   its score consumes. [`Transcript`] and [`DigestWriter`] are what a caller builds it with, so
//!   the framing is shared even though the content is not. See [`score_key`].
//!
//! ⚠ **Content-addressing makes the cache exactly as reproducible as its INPUTS, and that is a
//! property of the data's producer, not of this file.** A source that does not answer one request
//! the same way twice — parallel-reduction noise in a server-side aggregate, say — gives every
//! re-run a fresh key for every fit, stores thousands of entries and grows a directory that is
//! never read. That happened for four weeks to the first consumer, and the cure was to canonicalise
//! AT THE PARSE BOUNDARY (`crates/vike-backfill/src/vikedata/parse.rs`'s `canonical_notional`
//! carries the measurement, and the three constants beside it — `NOTIONAL_SIG_DIGITS`,
//! `MIDPOINT_NUDGE`, `OBSERVED_JITTER` — are what it snaps with). ⚠ It
//! deliberately does NOT live here: snapping inside the key would declare two genuinely different
//! datasets equivalent, which is the wrong hit this whole design refuses, while snapping the data
//! leaves the key an exact address of what the trainer reads.
//!
//! Anything NOT in the key is enumerated at the sites that build it, with the one declared
//! residual: the pure-Rust inference ([`crate::parse_model_text`] / `predict_proba`) and the
//! scorer bodies are CODE, not data — a change to them changes scores without changing any keyed
//! input. [`KEY_SCHEMA`] (and the fit-identity schema tag beside each `fit_identity` impl) exists
//! to be BUMPED by such a change.
//!
//! # What this crate now owns, and what it still does not
//!
//! ⚠ This module brings the workspace's SHA-256 framing down to `vike-ml`, and that is the one
//! thing this crate previously refused to own. Three consequences, stated because each was
//! previously argued the other way:
//!
//! * **`vike-ml` now owns a HASH.** [`digest_of`], [`Transcript`] and [`DigestWriter`] are the
//!   domain-separated transcript this crate had deliberately kept out (its dependency list was
//!   `vike-model` and nothing else, which is why the framing lived with its first consumer). It
//!   costs `sha2` and `hex`, both already linked into every binary in this workspace through
//!   `vike-bridge-core`'s signer — no new hash crate, no new supply-chain surface.
//! * **`vike-ml` still owns NO objective and NO scorer.** [`score_key`] takes the objective as a
//!   digest it does not compute, and this crate has no opinion on what ranks a candidate. That is
//!   what keeps the cache usable by a caller whose scorer lives in a crate `vike-ml` may not
//!   depend on — `vike-analytics` declares the same layer 20 as this crate, so the layer gate
//!   forbids the edge outright and generality here is a requirement rather than a nicety.
//! * **`vike-ml` still answers NO `fit_identity` from its own test double.** Owning the hash was
//!   only ONE of the three reasons `crate::test_support`'s `ScriptedLearner` refuses that method,
//!   and it was not the one that decided it: a salt-keyed identity on a double is a footgun that a
//!   `None` makes unreachable. That module's doc carries the surviving argument.
//!
//! # The value, the location, the failure modes
//!
//! The value is the validation SCORE — one `f64` — never the model file: only the score enters an
//! argmin, and a caller's final refit (whose model IS consumed) is deliberately not cached.
//! Entries live one-file-per-key in a directory that survives across runs
//! ([`FitCache::dir_beside`]: a sibling of the caller's scratch root, so wiping scratch does not
//! wipe the cache and wiping the cache is one `rm -rf` of its own directory). A corrupt or
//! uninterpretable entry is a MISS — deleted and refitted — never an error: a cache must not be
//! able to fail a run. A NON-FINITE score is never persisted, because a caller that folds a
//! deterministically rejected point and a TRANSIENT spawn/disk failure into the same `INFINITY`
//! would otherwise make the transient permanent for that key.
//!
//! Two threads may race on one key. Both fit, both store, and the values are equal BY CONSTRUCTION
//! (the key covers every input of the fit), so last-write-wins through [`FitCache::store`]'s atomic
//! rename is benign — stated there, beside the Windows fallback.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::train::TrainData;

/// The score key's domain tag. BUMP IT when the meaning of a stored score changes without any
/// keyed byte changing — i.e. when the in-process inference or a scorer body changes semantics —
/// so every old entry becomes an ordinary miss instead of a silently stale hit.
///
/// ⚠ **The string names `vike-research` and is deliberately FROZEN at that spelling.** It is an
/// opaque domain-separation token, not a namespace declaration: its only job is to make two
/// different meanings of "a stored score" hash differently. It was minted in the study that first
/// needed a cache and it moved here with the code, unchanged, because renaming it IS a bump — it
/// re-keys every entry in every existing on-disk cache, and a move that changes no keyed byte has
/// no business charging a user thousands of refits for a tidier constant.
/// `the_key_bytes_are_frozen` is the gate that makes such a change deliberate.
///
/// ⚠ **The crate it names is now GONE** — `vike-research` was dissolved and deleted, its study
/// moved to `user_data/research/` and its learner to `crates/vike-ml/src/gbdt_learner.rs` — and
/// that changes nothing here. A domain tag is allowed to name something that no longer exists;
/// what it may never do is change for a reason other than the meaning of a stored score changing.
/// The identical argument governs `gbdt_learner.rs`'s `vike-research fit-identity v1`.
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
/// The enumeration of what each part covers is this function's body plus the learner's
/// [`crate::Learner::fit_identity`] plus whatever the caller hashed into `objective_identity` —
/// kept as code rather than prose so the coverage cannot drift from its statement. `va` arrives
/// ALREADY narrowed to the validation half, and `objective_identity` must be narrowed the same
/// way, so the train/validation cut needs no component of its own: a different cut is different
/// bytes on both parts.
///
/// ⚠ **The objective is a DIGEST, not a value, and that is the seam.** This crate has no objective
/// type, no scorer and no way to acquire one — `vike-analytics`, where the workspace's scorers
/// live, declares the same layer as `vike-ml` and the layer gate forbids the dependency. So the
/// caller hashes its own objective's identity and every parameter it scores with, using
/// [`Transcript`] / [`DigestWriter`] from this module, and hands the result in. The contract is
/// the same one [`crate::Learner::fit_identity`] carries: two calls may share a digest ONLY when
/// their scores are equal by construction.
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
    /// of every config shares one pool (the key carries the whole identity, so sharing is safe by
    /// construction).
    ///
    /// ⚠ `name` is the CALLER's, deliberately: the directory name is part of an application's
    /// on-disk layout — renaming it orphans a user's existing cache exactly as a [`KEY_SCHEMA`]
    /// bump invalidates its contents — and this crate has no business choosing one. What lives
    /// here is the only part that is a property of the cache: sibling, never child.
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
    /// correct entry, and none of them can fail the run.
    ///
    /// A NON-FINITE score is refused — see the module doc (a transient spawn failure and a
    /// deterministic rejection wear the same `INFINITY`, and only one of them is a fact about the
    /// inputs).
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
        // Two threads racing on one key both computed the score from identical inputs, so the
        // values are equal BY CONSTRUCTION and last-write-wins is benign. Windows refuses a
        // rename over an existing file (POSIX replaces), so the fallback removes the — equal —
        // destination and retries once; if even that loses another racer, the entry on disk is
        // still an equal value, which is why every error here is deliberately ignored.
        if std::fs::rename(&tmp, &dest).is_err() {
            let _ = std::fs::remove_file(&dest);
            let _ = std::fs::rename(&tmp, &dest);
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// ⚠ `pub` rather than crate-private: the code that decides a hit is a hit lives in whichever
    /// crate owns the search loop, which is never this one. Counting is the caller's to do because
    /// the lookup is — [`FitCache::lookup`] cannot tell a genuine miss from a caller that never
    /// consulted it.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache over a scratch directory this test OWNS. ⚠ The caller must bind the returned
    /// [`vike_model::scratch::ScratchDir`] for the whole test — dropping it removes the directory the
    /// `FitCache` is reading.
    ///
    /// ⚠ This used to hand back a bare `PathBuf` under a pid-keyed name and leave each test to
    /// `remove_dir_all` it at the END, which cleans up only on the PASSING path: an assertion
    /// failure returns early and leaks the directory, and a PID is reused, so the next run under
    /// the OTHER the CI box user meets a directory it cannot write into. The guard removes it while
    /// unwinding too, which is exactly where a leak is least likely to be noticed.
    fn temp_cache(tag: &str) -> (FitCache, vike_model::scratch::ScratchDir) {
        let dir = vike_model::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            &format!("vike_ml_fit_cache_{tag}"),
        )
        .expect("scratch dir");
        (FitCache::open(dir.path()).unwrap(), dir)
    }

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn a_stored_score_reads_back_bit_identical() {
        let (c, _dir) = temp_cache("roundtrip");
        // The awkward citizens: a negative zero, a subnormal, and a value with no short decimal.
        for (i, v) in
            [-0.0f64, f64::MIN_POSITIVE / 2.0, 1.0 / 3.0, -1.5e308].into_iter().enumerate()
        {
            let k = key(i as u8);
            c.store(&k, v);
            let got = c.lookup(&k).expect("a stored score must be found");
            assert_eq!(got.to_bits(), v.to_bits(), "entry {i} came back different bits");
        }
    }

    #[test]
    fn a_missing_entry_is_a_miss() {
        let (c, _dir) = temp_cache("missing");
        assert_eq!(c.lookup(&key(9)), None);
    }

    /// Garbage in an entry must MISS (and be deleted), never propagate.
    #[test]
    fn a_corrupt_entry_is_a_miss_and_is_deleted() {
        let (c, _dir) = temp_cache("corrupt");
        let k = key(1);
        let path = c.entry_path(&k);
        for (name, bytes) in [
            ("binary garbage", b"\x00\x01\x02garbage".to_vec()),
            ("wrong magic", b"vike-fit-cache v9\n0000000000000000\n".to_vec()),
            ("truncated", format!("{MAGIC}\n3fe0").into_bytes()),
            ("short hex", format!("{MAGIC}\n3fe\n").into_bytes()),
            ("long hex", format!("{MAGIC}\n3fe00000000000000\n").into_bytes()),
            ("not hex", format!("{MAGIC}\nzfe0000000000000\n").into_bytes()),
            ("trailing junk", format!("{MAGIC}\n3fe0000000000000\nx").into_bytes()),
            ("empty", Vec::new()),
            // A VALID spelling of a value `store` never writes: infinity's bit pattern. Reading
            // it back as a score would smuggle "this point failed once, transiently" into every
            // future run as a fact about the inputs.
            (
                "non-finite bits",
                format!("{MAGIC}\n{:016x}\n", f64::INFINITY.to_bits()).into_bytes(),
            ),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(c.lookup(&k), None, "{name}: a corrupt entry answered a score");
            assert!(!path.exists(), "{name}: the corrupt entry was not deleted");
        }
    }

    #[test]
    fn a_non_finite_score_is_never_persisted() {
        let (c, _dir) = temp_cache("nonfinite");
        c.store(&key(1), f64::INFINITY);
        c.store(&key(2), f64::NEG_INFINITY);
        c.store(&key(3), f64::NAN);
        for n in 1..=3 {
            assert!(!c.entry_path(&key(n)).exists(), "key({n}) was persisted");
        }
    }

    /// The Windows path of the benign write race: the second `store` renames over an EXISTING
    /// entry, which `std::fs::rename` refuses on Windows — the fallback must leave a readable,
    /// equal entry rather than a deleted or torn one.
    #[test]
    fn storing_over_an_existing_entry_keeps_a_readable_equal_value() {
        let (c, dir) = temp_cache("overwrite");
        let k = key(7);
        c.store(&k, 0.25);
        c.store(&k, 0.25);
        assert_eq!(c.lookup(&k), Some(0.25));
        // ...and no temp litter survived the fallback.
        let stray: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(stray.is_empty(), "{} temp files left behind", stray.len());
    }

    #[test]
    fn the_cache_directory_is_a_sibling_of_the_scratch_root() {
        assert_eq!(
            FitCache::dir_beside(Path::new("target/research-scratch"), "research-fit-cache"),
            Path::new("target/research-fit-cache")
        );
        // A single-component scratch keeps the cache beside it (in the same parent), and never
        // UNDER it — under it, a scratch wipe silently empties the cache too.
        let d = FitCache::dir_beside(Path::new("scratch"), "fits");
        assert_eq!(d, Path::new("fits"));
    }

    /// The framing property: parts enter as digests, so two sequences with equal concatenations
    /// differ. A running hash without framing passes every other test in this file.
    #[test]
    fn two_part_sequences_with_equal_concatenation_differ() {
        let mut a = Transcript::new("t");
        a.part(b"ab");
        a.part(b"c");
        let mut b = Transcript::new("t");
        b.part(b"a");
        b.part(b"bc");
        assert_ne!(a.finish(), b.finish());

        let mut c = Transcript::new("t1");
        c.part(b"x");
        let mut d = Transcript::new("t2");
        d.part(b"x");
        assert_ne!(c.finish(), d.finish(), "the domain tag is not part of the key");
    }

    fn va<'a>(x: &'a [f64], y: &'a [f32], n_cols: usize) -> TrainData<'a> {
        TrainData { x, n_rows: y.len(), n_cols, y, categorical: &[] }
    }

    /// Every component of [`score_key`] separates keys — and content at a DIFFERENT address does
    /// not, which is the property that makes CROSS-RUN hits possible at all.
    ///
    /// ⚠ The OBJECTIVE half of this test is a caller's, not this crate's: `score_key` sees an
    /// opaque digest, so all it can prove here is that a different digest separates. That a real
    /// objective hashes each of ITS parameters distinctly is proven where the objective lives —
    /// `user_data/research/studies/rust/cohort/search.rs`'s
    /// `the_objective_identity_separates_every_parameter`.
    #[test]
    fn the_score_key_separates_every_component_and_ignores_addresses() {
        let x = [0.1, 0.2, 0.3, 0.4];
        let y = [0.0f32, 1.0];
        let obj = key(0);
        let base = score_key(&key(1), &va(&x, &y, 2), &obj);

        // Same content, freshly allocated: EQUAL — the key addresses content, not memory.
        let x2 = x.to_vec();
        let y2 = y.to_vec();
        assert_eq!(base, score_key(&key(1), &va(&x2, &y2, 2), &obj));

        // A different fit identity separates.
        assert_ne!(base, score_key(&key(2), &va(&x, &y, 2), &obj));

        // One changed feature BIT separates (−0.0 vs 0.0 included: bits, not values).
        let x3 = [0.1, 0.2, 0.3, 0.5];
        assert_ne!(base, score_key(&key(1), &va(&x3, &y, 2), &obj));
        let xz = [0.0, 0.2, 0.3, 0.4];
        let xnz = [-0.0, 0.2, 0.3, 0.4];
        assert_ne!(
            score_key(&key(1), &va(&xz, &y, 2), &obj),
            score_key(&key(1), &va(&xnz, &y, 2), &obj)
        );

        // A changed label separates.
        let y3 = [1.0f32, 1.0];
        assert_ne!(base, score_key(&key(1), &va(&x, &y3, 2), &obj));

        // The same flat buffer at a different width separates.
        assert_ne!(base, score_key(&key(1), &va(&x, &[0.0f32], 4), &obj));

        // A different objective identity separates.
        assert_ne!(base, score_key(&key(1), &va(&x, &y, 2), &key(3)));
    }

    /// The key IS the on-disk address of every entry in every existing cache, so this pins the
    /// exact bytes rather than only the separations above.
    ///
    /// ⚠ **A failure here is not a bug to be re-baselined away.** It means the key changed, which
    /// means every stored entry on every machine just became an ordinary miss — thousands of
    /// refits at seconds each. If the change is deliberate, bump [`KEY_SCHEMA`] in the same commit
    /// so the invalidation is stated rather than merely suffered, then update this constant.
    ///
    /// The vector is the same one the separation test uses, with the objective digest this
    /// module's first caller produces for its cheapest arm (`DigestWriter::u64(0)`). ⚠ The
    /// constant was **not** read off this implementation: it was computed independently, with
    /// `sha256sum` over the byte stream the PRE-MOVE code specified — `u64le(26)` ‖ the tag ‖ the
    /// 32 identity bytes ‖ SHA-256(the validation stream) ‖ SHA-256(`u64le(0)`) — so the assertion
    /// below is evidence that promoting this file into `vike-ml` re-keyed nothing, rather than a
    /// value this code was asked to agree with itself about.
    #[test]
    fn the_key_bytes_are_frozen() {
        let x = [0.1, 0.2, 0.3, 0.4];
        let y = [0.0f32, 1.0];
        let mut o = DigestWriter::new();
        o.u64(0);
        let got = score_key(&[1u8; 32], &va(&x, &y, 2), &o.finish());
        assert_eq!(
            hex::encode(got),
            "deaa9eb5a9710b34a8fe55f5b8c5e6bf580644d8979b7c5dea4bc49a0fdb5ae7",
            "the fit-cache key changed — read this test's doc before touching the constant"
        );
    }
}
