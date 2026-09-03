//! ⚠ **THE ONE [`Learner`] THAT DRIVES A REAL TRAINER.** Everything else that implements the seam
//! in this workspace is a double.
//!
//! [`crate::learner`] states the seam — fit a [`TrainData`] at a [`GridPoint`], read one
//! probability per row. This file is the impl that spawns a child process to do it, and it is
//! where everything the trainer insists on stops: a verified binary, per-fit scratch
//! directories, a parameter bag, a binned-dataset cache and an error enum.
//!
//! # Where it came from, and what the move changed
//!
//! It was the research crate's own `studies/cohort/ml_adapter.rs` — a STUDY's file, and the only
//! working `Learner` impl in the tree. `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md`'s
//! Phase 1 named it a promotion into this crate, with two surgical conditions, and its Phase 6
//! stated the deadline: *"Promotion is cheap while both trees are in one workspace and a single
//! `cargo check` proves the move; it is expensive the day after the split… Do it before the
//! departure, or do not do it at all."* The study has now departed to `user_data/research/`, and
//! this file is what stayed.
//!
//! **The two conditions, both honoured, both visible in the constructor:**
//!
//! * the base parameter bag is a CONSTRUCTOR PARAMETER ([`GbdtLearner::new`]'s `base`), so no
//!   caller's regularisation is compiled into this crate — see [`params_for`];
//! * the bin cache's capacity is a fixed [`DEFAULT_BIN_CAPACITY`] with a
//!   [`GbdtLearner::with_bin_capacity`] override, rather than a function of `rayon`'s pool — so
//!   promoting it added NO dependency to a crate whose value is that it is feature-free.
//!
//! Nothing else moved. The choreography, the cache, the identity digest and every test below are
//! the study's, unchanged — which is why [`Learner::fit_identity`]'s transcript tag still reads
//! `vike-research fit-identity v1`: it is a CACHE KEY, not a citation, and renaming it would
//! invalidate every fit cached under it for no correctness reason at all. ⚠ The crate that minted
//! that spelling no longer exists — `vike-research` was dissolved and deleted outright — and the
//! token is FROZEN anyway. A name that no longer names anything is precisely what a content-address
//! schema tag is allowed to be; the day it changes must be a day the fit identity's CONTENT
//! changed, never a tidy-up.
//!
//! ⚠ **One consequence to keep in view: the two `TrainData`s merged.** The study's had
//! `categorical`, an explicit row count and `split_at_row`; the library's had a validating
//! constructor and the CSV writer. They met HERE, under an alias, and every fit rebuilt the
//! labels into a fresh `Vec<f32>` to cross between them. There is one type now, the labels are
//! `f32` (the trainer's own width, packed once by the caller), and the shape check this
//! file used to own is `crates/vike-ml/src/train.rs`'s `TrainData::validate`.
//!
//! # The four questions this file exists to answer
//!
//! **(a) Where the NON-searched parameters live.** In the CALLER, handed over once at construction
//! and applied by [`params_for`]. Five parameters
//! (`num_leaves`, `max_depth`, `min_data_in_leaf`, `learning_rate`, `feature_fraction`) are the
//! searched axes and arrive on every [`Learner::fit`] call inside a [`GridPoint`], so a base bag
//! must NOT restate them — a base copy of a searched axis is dead state whose only effect would be
//! to hand a DROPPED mapping a plausible value to fail silently with, and
//! `no_searched_axis_is_also_pinned_in_the_base_where_it_could_mask_a_dropped_mapping` is that
//! rule as a test. Everything else — `objective`, `verbosity`, `num_iterations`, the two
//! `lambda_*`, the two `bagging_*` — is the caller's to state, over
//! `crates/vike-ml/src/params.rs`'s `GbdtParams::default`.
//!
//! **(b) How a categorical column is declared.** `TrainData::categorical` carries FEATURE column
//! indices (the label column is not counted, which is LightGBM's own convention); they become a
//! COMMA-SEPARATED string through `crates/vike-ml/src/params.rs`'s `categorical_feature_csv`. It
//! must be a string and never a list: LightGBM's config parser strips quotes but not brackets, so
//! `[0,3]` names no column at all, the column is binned as a NUMBER, nothing errors, and the saved
//! model's parameter echo still claims the column was categorical. An empty `categorical` declares
//! NOTHING (`None`), not an empty list. An index outside the row width is refused before anything
//! spawns — `crates/vike-ml/src/train.rs`'s `TrainData::validate` does it, reached through
//! [`check_shape`] — because LightGBM ignores an out-of-range index in silence, which is the same
//! silent outcome.
//!
//! **(c) `fit` and the model are `Sync`.** `GbdtLearner` is a verified binary path, a scratch root
//! and a counter; [`GbdtModel`] is `{usize, Objective, Vec<String>, Vec<Tree>}`, plain data
//! throughout. Neither bound the seam declares had to be relaxed and the grid search stays parallel —
//! `the_learner_and_its_model_satisfy_the_bounds_the_parallel_search_needs` asserts it at compile
//! time rather than leaving it to a `rayon` type error in another module, on `GbdtLearner` written
//! with no type argument, i.e. on the [`LightGbmCli`] instantiation a real caller builds.
//! ([`Trainer`] is `Sync` for the same reason and says so, so a trainer that is not cannot be
//! substituted into a learner the search then tries to share.) Each `fit` gets its OWN scratch
//! directory ([`GbdtLearner::fit_dir`]) for exactly this reason: the parallelism is real, and two
//! concurrent fits sharing one config path would each train the other's parameters.
//!
//! **(d) The error type is `String`.** [`MlError`] is mapped through `Display` at every
//! boundary. The seam asked for `Result<_, String>` because a point the learner rejects must not
//! abort the other 242, and a rejection's whole value is the sentence — `MlError::Backend` already
//! carries LightGBM's own stderr tail inside it.
//!
//! # Two divergences from the oracle, stated rather than discovered later
//!
//! * **`num_threads = 1`.** The oracle's `lgb.LGBMClassifier` uses every core. The pinned binary is
//!   built `-DUSE_OPENMP=OFF` and `crates/vike-ml/src/grid.rs`'s `check_grid_params` refuses
//!   anything else, because thread count changes the float reduction order — a reproducible model
//!   is worth one core here. Fits are parallel across PROCESSES, not inside one.
//! * **A full 243-point grid, not 20 TPE trials.** That divergence belongs to
//!   the caller's own search module, not here; this file fits ONE point.
//!
//! # The handover, and what gates it
//!
//! Every property stated above is killed by a test in this file that runs in an ordinary
//! `cargo test`, and that now includes the one that used to be excluded: that [`Learner::fit`]
//! hands the trainer THE PARAMETERS IT JUST BUILT rather than some other [`GbdtParams`]. Replacing
//! `&params` with `&GbdtParams::default()` in `fit` used to leave every unit test below GREEN
//! (measured, not supposed) and redden only two `#[ignore]`d smokes against a binary CI does not
//! have — `two_fits_with_different_seeds_differ` and
//! `a_grid_point_changes_the_model_the_seam_hands_back` in
//! `crates/vike-ml/tests/gbdt_learner_smoke.rs`. It is the most consequential silent failure
//! available here: every point of a caller's grid
//! would train identically, the search would still finish and still name a winner, and the winner
//! would be noise.
//!
//! [`Trainer`] is what closed it, and it is deliberately the smallest thing that could: the calls
//! `fit` makes into `crates/vike-ml/src/cli.rs`'s [`LightGbmCli`] — `train`, joined by
//! `save_binary` when the per-fold binary reuse landed, because binning is now the step that BAKES
//! `min_data_in_leaf` and the categorical declaration into the dataset — each implemented for the
//! real driver as a straight delegation. `#[cfg(test)]`'s `RecordingTrainer` is then handed the
//! same [`TrainConfig`] a fit passes and keeps the [`GbdtParams`] inside it (for the binning too),
//! and `the_trainer_is_handed_the_parameters_the_fit_just_built` asserts every searched axis, the
//! seed and the categorical CSV at that boundary — each against its value AND against the library
//! default it would wear had some other bag been handed over.
//!
//! ⚠ **The whole-bag equality at the end of that test and the per-field assertions above it kill
//! DIFFERENT mutations, and neither half is decoration.** An axis dropped from [`point_bag`]
//! vanishes from BOTH sides of the equality at once, so only the literals see it; `&params`
//! replaced by `&GbdtParams::default()` moves every field at once, so the equality alone sees that.
//! Measured on the branch that added them, by mutating the test as well as the code: with the
//! per-field assertions deleted, a dropped `feature_fraction` leaves this test GREEN and the
//! default-bag substitution still reddens it; with the equality deleted instead, the dropped axis
//! reddens it. Do not simplify either half away.
//!
//! Two things about a fit are still not gated by a test that runs in CI. Both are LOUD, which is
//! the whole difference — this file's residuals are about failures that produce a plausible model:
//!
//! * **That LightGBM HONOURS what it is handed.** A recording double proves the handover and can
//!   prove nothing whatever about the child process. That is what the `#[ignore]`d smokes above and
//!   `crates/vike-ml/tests/train_infer_equality.rs` are for. ⚠ The real [`Trainer`] impl being a
//!   REAL delegation rather than a canned answer is NOT in this bucket and is gated:
//!   `fit_checks_the_shape_before_it_spawns_anything` reaches the spawn through `Trainer::train` on
//!   a [`LightGbmCli`] and asserts the error names the missing binary.
//! * **The two PATHS in the config** — the binary `binned_for` just produced, and the model file
//!   the fit names. A wrong one is an immediate open failure from the child, not a plausible
//!   model, so it fails the first time anybody runs a fit at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::cli::LightGbmCli;
use crate::fit_cache;
use crate::grid::{bin_dataset, check_grid_params};
use crate::train::{BinPreFilter, Task, TrainConfig};
use crate::{
    importance_from_model_text, parse_model_text, Capture, CapturedFit, FitImportance, GbdtModel,
    GbdtParams, GridPoint, Learner, MlError, TrainData,
};

/// The parameters a fit starts from, before its [`GridPoint`] is applied.
///
/// ⚠ **A CONSTRUCTOR PARAMETER, not a constant, and that is the whole of what this promotion
/// changed.** The five fixed keys that used to live here — `num_iterations`, `lambda_l1`,
/// `lambda_l2`, `bagging_fraction`, `bagging_freq` — were one study's `config.py`, and a library
/// that baked them in would be a library with one caller's regularisation compiled into it. Every
/// caller now states its own bag; [`GbdtLearner::new`] uses [`GbdtParams::default`], which is
/// LightGBM's own defaults and therefore the answer that surprises nobody.
///
/// The two settings that bite when a caller assembles its own: `bagging_freq` defaults to `0`,
/// which DISABLES bagging however small `bagging_fraction` is — so those two are one setting
/// wearing two names and dropping either alone silently trains an unbagged model.
///
/// This function is `base` with the point applied, the seed pinned and the categorical columns
/// declared.
///
/// ⚠ The last step is [`check_grid_params`], and it belongs HERE because this is where a fit's
/// parameters are finalised — the 68x threading cliff has to be refused at the point the fits
/// happen, not at the point somebody writes a default. It has nothing to refuse today
/// (`GbdtParams::default` pins `num_threads = 1`, and no searched axis can reach the field at all
/// — `apply` refuses the NAME), which is the state a guard should be in; what it buys is that a
/// later edit to a caller's base bag, or to the library default it inherits, cannot turn a
/// 30-minute grid into a multi-week one in silence.
fn params_for(
    base: &GbdtParams,
    p: &GridPoint,
    seed: u64,
    categorical: &[usize],
) -> Result<GbdtParams, String> {
    let mut params = base.with_point(&p.as_param_point()).map_err(|e| e.to_string())?;
    params.seed = seed;
    params.categorical_feature =
        (!categorical.is_empty()).then(|| GbdtParams::categorical_feature_csv(categorical));
    check_grid_params(&params).map_err(|e| e.to_string())?;
    Ok(params)
}

/// The one train invocation, built in one place so a test can render exactly what a fit runs.
fn train_config<'a>(
    params: &'a GbdtParams,
    data: &'a Path,
    output_model: &'a Path,
) -> TrainConfig<'a> {
    TrainConfig {
        task: Task::Train,
        params,
        data,
        output_model: Some(output_model),
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
}

/// The shape refusal, in the error type this seam speaks.
///
/// ⚠ The CHECK itself is `crates/vike-ml/src/train.rs`'s `TrainData::validate` and no longer lives
/// here: it moved with the type it is about, when the study's `TrainData` and the library's merged
/// into one. What stays is the two things that are this file's own — running it BEFORE anything is
/// written or spawned, and rendering `MlError` through `Display`, because a point the learner
/// rejects must not abort the other 242 and the whole value of a rejection is its sentence.
fn check_shape(d: &TrainData<'_>) -> Result<(), String> {
    d.validate().map_err(|e| e.to_string())
}

/// The TWO calls a fit makes into the model crate's process driver — the train itself, and the
/// once-per-dataset binning the train reads from.
///
/// It exists for exactly one reason, and the reason is a test: without it, the lines that hand
/// [`train_config`]'s output (and the binning's parameter bag) over are unobservable from inside
/// this process, and replacing either with `GbdtParams::default()` is a change no `cargo test`
/// can see — see this module's "The handover" section. `save_binary` joined `train` when the
/// per-fold binary reuse landed, and for the same reason `train` existed: the binning is now the
/// step that BAKES `min_data_in_leaf` and the categorical declaration into the dataset, so an
/// unobserved handover there is exactly the silent-categorical failure class this crate drives a
/// process to avoid. Nothing else about `vike-ml` is abstracted here and nothing else should be:
/// the value of this seam is its thinness, and each method is one real call the fit makes.
///
/// `Sync` because [`Learner`] is — the grid search calls `fit` from `rayon::par_iter`, so whatever
/// sits in this slot is shared across threads. Stating it on the trait means a trainer that is not
/// `Sync` is refused where it is SUBSTITUTED rather than in a `rayon` type error somewhere else.
///
/// Crate-private on purpose: it is this file's own test seam, not API. That is also why the
/// learner's non-trait methods below are NOT in an `impl<T: Trainer>` block — a crate-private trait
/// in the bounds of an inherent impl on a `pub` type is `private_bounds`, which is `-D warnings`.
pub(crate) trait Trainer: Sync {
    fn train(&self, conf: &TrainConfig<'_>, scratch: &Path) -> Result<String, MlError>;
    /// Bin `data` into LightGBM's binary dataset format at `out`, with
    /// [`BinPreFilter::MatchFit`] semantics: `params.min_data_in_leaf` and
    /// `params.categorical_feature` are consumed at construction, so a later `train` reading
    /// `out` reproduces a plain per-fit CSV train at those values byte-for-byte.
    fn save_binary(
        &self,
        data: &TrainData<'_>,
        params: &GbdtParams,
        out: &Path,
        scratch: &Path,
    ) -> Result<(), MlError>;
}

/// The real trainer: straight delegations, with nothing of their own to get wrong.
///
/// ⚠ `train` is spelled with the fully-qualified call rather than `self.train(conf, scratch)` — an
/// inherent method wins method resolution over a trait one, so the short form is this function
/// calling ITSELF, forever. (`crates/vike-ml/src/learner.rs`'s `ProbaModel` impl for `GbdtModel`
/// is written the way it is for the same hazard in the other direction.)
///
/// `save_binary` delegates to the model crate's `bin_dataset` with [`BinPreFilter::MatchFit`] —
/// NEVER `MinDataAgnostic`, whose bins measurably diverge from a CSV fit's trees whenever the
/// default pre-filter would have dropped a feature (the pre-filter shrinks the
/// `feature_fraction` sampling universe). The parity claim is gated against the real binary by
/// `the_binned_fit_matches_the_per_fit_csv_train_across_every_searched_axis_value` below.
impl Trainer for LightGbmCli {
    fn train(&self, conf: &TrainConfig<'_>, scratch: &Path) -> Result<String, MlError> {
        LightGbmCli::train(self, conf, scratch)
    }

    fn save_binary(
        &self,
        data: &TrainData<'_>,
        params: &GbdtParams,
        out: &Path,
        scratch: &Path,
    ) -> Result<(), MlError> {
        bin_dataset(self, data, params, BinPreFilter::MatchFit, out, scratch).map(|_| ())
    }
}

// ---- the per-dataset binary cache -------------------------------------------------------------

/// Identity of a binned dataset: the training data's CONTENT plus the one searched axis that is
/// consumed at dataset construction.
///
/// The search hands the SAME [`TrainData`] to every trial of a fold, so within a fold the key is
/// stable and the fold's fits share one binary per `min_data_in_leaf` value. `min_data_in_leaf` is
/// in the key because it is DATASET-BAKED: under LightGBM's default `feature_pre_filter` it
/// decides which features survive binning, and a bin built at one value trains DIFFERENT trees at
/// another (measured on the real binary — see [`BinPreFilter`]'s doc and the matrix smoke below).
/// The other four searched axes are tree-growth parameters and deliberately absent.
///
/// `x_ptr`/lengths alone would be an ABA hazard — a freed fold's buffer address can be reused by a
/// later fold of identical size — so the key also carries a 64-bit content fingerprint of the
/// features and labels. A collision needs the same address, the same lengths AND the same
/// fingerprint over different bytes; with a handful of live keys per run that is astronomically
/// unlikely, and the fingerprint is the cheap insurance (a few ms on the fold sizes this study
/// trains), not a security boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BinKey {
    x_ptr: usize,
    x_len: usize,
    n_rows: usize,
    n_cols: usize,
    x_hash: u64,
    y_hash: u64,
    categorical: Vec<usize>,
    min_data_in_leaf: u32,
}

impl BinKey {
    fn of(d: &TrainData<'_>, min_data_in_leaf: u32) -> Self {
        Self {
            x_ptr: d.x.as_ptr() as usize,
            x_len: d.x.len(),
            n_rows: d.n_rows,
            n_cols: d.n_cols,
            x_hash: fingerprint_f64(d.x),
            y_hash: fingerprint_labels(d.y),
            categorical: d.categorical.to_vec(),
            min_data_in_leaf,
        }
    }
}

/// A word-at-a-time rotate–xor–multiply fingerprint (FxHash's mixing constant). Not cryptographic,
/// and does not need to be — see [`BinKey`]'s collision argument. Inline rather than a hasher
/// dependency: the workspace pins its dependency set, and eight lines do not buy a crate.
fn fingerprint_words<I: Iterator<Item = u64>>(words: I) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        h = (h.rotate_left(5) ^ w).wrapping_mul(K);
    }
    h
}

fn fingerprint_f64(xs: &[f64]) -> u64 {
    fingerprint_words(xs.iter().map(|v| v.to_bits()))
}

/// The label twin. ⚠ Hashes the BIT PATTERN of each label, one word each, for the same reason
/// [`fingerprint_f64`] does: `-0.0` and `0.0` are `==` as floats and are different bytes in the
/// CSV the binning writes, so a value-wise fold would give two different datasets one key. It
/// packed `u8` labels eight-to-a-word until the study's labels became `f32` — the trainer's own
/// width — and the density is not worth a second spelling of the rule.
fn fingerprint_labels(ys: &[f32]) -> u64 {
    fingerprint_words(ys.iter().map(|v| u64::from(v.to_bits())))
}

/// One binned dataset on disk. Dropping the LAST handle deletes its directory, which is what makes
/// eviction safe under concurrency: an in-flight fit holds an [`Arc`] to the file it is training
/// from, so an evicted-while-in-use binary outlives the fit and dies with it.
#[derive(Debug)]
struct BinFile {
    dir: PathBuf,
    bin: PathBuf,
}

impl Drop for BinFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One key's slot: a [`OnceLock`] so concurrent first fits of a fold bin ONCE — the losers block
/// on the winner's binning instead of each writing their own, which matters because the TPE
/// startup batch arrives ten-at-once on a cold key.
///
/// ⚠ The `Err` arm is cached too: a binning failure (bad shape aside, that is a spawn or disk
/// failure) is answered to every later fit of the same key without retrying. That is the same
/// blast radius the per-fit CSV scheme had — a disk that cannot take the CSV fails every fit of
/// the fold individually — spelled as one failure instead of twenty-one.
#[derive(Default)]
struct BinSlot {
    once: OnceLock<Result<Arc<BinFile>, String>>,
}

/// The fold-keyed binning cache the `fit` doc always said this seam would grow.
///
/// Insertion-ordered with move-to-back on hit, evicting from the front past `capacity` — LRU over
/// a `Vec`, linear because `capacity` is small. Entries die in two steps: eviction drops the
/// cache's handle, and the directory is deleted when the last in-flight fit drops its own
/// ([`BinFile`]'s `Drop`). A fold's binaries therefore live for the fold (plus at most the LRU
/// tail) and are gone by the time the learner is — dropping the cache drops every entry.
/// How many binned datasets a [`GbdtLearner`] keeps resident by default.
///
/// ⚠ **A CONSTANT rather than a function of the thread pool, and the reason is a dependency.**
/// The study this cache was written for sized it at `(8 * rayon::current_num_threads()).max(64)`,
/// which is a fine number and would have made `rayon` a normal dependency of this crate — for a
/// cache bound, in a crate whose whole value is that it is feature-free and cheap to depend on. A
/// caller that knows its own pool width passes that number to
/// [`GbdtLearner::with_bin_capacity`] instead; an under-sized cache is a RE-BIN, never a wrong
/// answer, which is what makes a fixed floor a safe default rather than a silent behaviour change.
///
/// The floor itself is the old expression's own `.max(64)`: a live fold holds at most four keys
/// (one bin per `min_data_in_leaf` value its trials reach, plus the final whole-fold refit), so 64
/// keeps sixteen concurrent folds resident with slack.
pub const DEFAULT_BIN_CAPACITY: usize = 64;

struct BinCache {
    capacity: usize,
    entries: Mutex<Vec<(BinKey, Arc<BinSlot>)>>,
}

impl BinCache {
    fn new(capacity: usize) -> Self {
        Self { capacity, entries: Mutex::new(Vec::new()) }
    }

    fn slot(&self, key: BinKey) -> Arc<BinSlot> {
        let mut entries = self.entries.lock().expect("bin cache poisoned");
        if let Some(pos) = entries.iter().position(|(k, _)| *k == key) {
            let hit = entries.remove(pos);
            let slot = Arc::clone(&hit.1);
            entries.push(hit);
            return slot;
        }
        let slot = Arc::new(BinSlot::default());
        entries.push((key, Arc::clone(&slot)));
        while entries.len() > self.capacity {
            // The front is the least recently used. Removing it only drops the CACHE's handle;
            // an in-flight fit still holding the file keeps it alive until it finishes.
            entries.remove(0);
        }
        slot
    }
}

/// The study's [`Learner`]: gradient-boosted trees, fitted by the pinned LightGBM binary.
///
/// ⚠ The plan sketched `new()`. The merged model crate takes the trainer binary as a PARAMETER and
/// reads no environment variable to find one (`crates/vike-ops/tests/settings_registry.rs`'s
/// `LIBRARY_PIN` is why), and it needs somewhere to put a fit's scratch files — so the real
/// constructor takes both. A learner that could not name its binary would be a learner that had to
/// go looking for one.
///
/// `T` defaults to [`LightGbmCli`], so every construction site, every signature and every
/// `GbdtLearner` written with no type argument means the real learner and reads as it did before
/// the parameter existed. The only thing that ever names another `T` is this file's own
/// `RecordingTrainer`.
pub struct GbdtLearner<T = LightGbmCli> {
    cli: T,
    scratch: PathBuf,
    /// Makes each fit's scratch directory unique. `fit` is called from `rayon::par_iter`, and one
    /// shared config path across concurrent fits is two fits training each other's parameters.
    next: AtomicU64,
    /// The per-dataset binaries the fits of a fold share — see [`BinCache`].
    bins: BinCache,
    /// The parameters every fit starts from, before its [`GridPoint`] is applied — see
    /// [`params_for`]. A CONSTRUCTOR parameter rather than a constant, so this crate carries no
    /// caller's regularisation.
    base: GbdtParams,
    /// SHA-256 of the trainer binary's `PROVENANCE` file bytes, read ONCE at construction — the
    /// trainer's contribution to [`Learner::fit_identity`]. `None` means the identity cannot be
    /// proven and the cross-run fit cache never engages, which is the safe direction (a refit,
    /// never a wrong answer); the test constructors below use exactly that.
    provenance: Option<[u8; 32]>,
}

impl GbdtLearner<LightGbmCli> {
    /// `binary` is the pinned LightGBM CLI (`just lightgbm-build` writes one at
    /// `bin/lightgbm/lightgbm`); `scratch` is a directory this learner may create per-fit
    /// subdirectories under, and it should be on the NVMe; `base` is the parameter bag every fit
    /// starts from, before its [`GridPoint`] is applied ([`GbdtParams::default`] is LightGBM's own
    /// defaults, and is what a caller with no opinion should pass).
    ///
    /// The binary's provenance is checked here, ONCE, rather than per fit: a wrong release parses,
    /// predicts and is wrong in ways only `crates/vike-ml/tests/train_infer_equality.rs` would see.
    ///
    /// The bin cache is sized at [`DEFAULT_BIN_CAPACITY`]; [`Self::with_bin_capacity`] is how a
    /// caller that knows its own parallelism widens it.
    pub fn new(binary: &Path, scratch: &Path, base: GbdtParams) -> Result<Self, String> {
        let cli = LightGbmCli::new(binary).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(scratch)
            .map_err(|e| format!("creating the scratch root {}: {e}", scratch.display()))?;
        // `LightGbmCli::new` just verified the file exists and records the pinned tag, so this
        // read succeeds in every ordinary construction; a race that removes it between the two
        // reads degrades to `None` — an uncached learner — rather than an error.
        let provenance = std::fs::read(crate::cli::provenance_path(binary))
            .ok()
            .map(|bytes| fit_cache::digest_of(&bytes));
        Ok(Self {
            cli,
            scratch: scratch.to_path_buf(),
            next: AtomicU64::new(0),
            bins: BinCache::new(DEFAULT_BIN_CAPACITY),
            base,
            provenance,
        })
    }
}

impl<T> GbdtLearner<T> {
    /// Resize the binned-dataset cache — see [`DEFAULT_BIN_CAPACITY`] for why the default is a
    /// fixed floor rather than a function of the caller's thread pool.
    ///
    /// ⚠ Capacity is a PERFORMANCE knob and nothing else: an evicted entry is re-binned, so no
    /// value of it can change a fitted model. A `0` is legal and means "bin every fit".
    #[must_use]
    pub fn with_bin_capacity(mut self, capacity: usize) -> Self {
        self.bins = BinCache::new(capacity);
        self
    }

    /// A directory no other fit in this process will use.
    fn fit_dir(&self) -> PathBuf {
        self.scratch.join(format!(
            "fit-{}-{}",
            std::process::id(),
            self.next.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A directory no other binning in this process will use — same counter, distinct prefix.
    fn bin_dir(&self) -> PathBuf {
        self.scratch.join(format!(
            "bin-{}-{}",
            std::process::id(),
            self.next.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

/// Bin one dataset for one `min_data_in_leaf`, into its own directory.
///
/// A free function rather than a method for the reason [`Trainer`]'s doc gives: an inherent
/// `impl<T: Trainer> GbdtLearner<T>` block would put a crate-private trait in the bounds of a `pub`
/// type's impl, which `private_bounds` refuses under `-D warnings`. A trait impl may carry the
/// bound; an inherent one may not.
fn bin_once<T: Trainer>(
    l: &GbdtLearner<T>,
    d: &TrainData<'_>,
    params: &GbdtParams,
) -> Result<Arc<BinFile>, String> {
    // ⚠ `d` goes STRAIGHT to the binning. Until the two `TrainData` types merged this line
    // rebuilt the labels into a fresh `Vec<f32>` and wrapped them in the library's own shape —
    // per fit, on the hot path — because the study's labels were `u8` and the trainer's are not.
    // `pack` now writes them in the trainer's width once.
    let dir = l.bin_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let bin = dir.join("train.bin");
    match l.cli.save_binary(d, params, &bin, &dir) {
        Ok(()) => Ok(Arc::new(BinFile { dir, bin })),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e.to_string())
        }
    }
}

/// The binary for `(d, params.min_data_in_leaf)`, binning it if this is the first fit to ask.
fn binned_for<T: Trainer>(
    l: &GbdtLearner<T>,
    d: &TrainData<'_>,
    params: &GbdtParams,
) -> Result<Arc<BinFile>, String> {
    let slot = l.bins.slot(BinKey::of(d, params.min_data_in_leaf));
    slot.once.get_or_init(|| bin_once(l, d, params)).clone()
}

/// The train half: run one fit off an already-binned dataset and parse the model it wrote.
fn fit_in<T: Trainer>(
    cli: &T,
    dir: &Path,
    data: &Path,
    params: &GbdtParams,
) -> Result<GbdtModel, String> {
    let model_path = dir.join("model.txt");
    let text =
        cli.train(&train_config(params, data, &model_path), dir).map_err(|e| e.to_string())?;
    parse_model_text(&text).map_err(|e| e.to_string())
}

impl<T: Trainer> Learner for GbdtLearner<T> {
    type Model = GbdtModel;

    /// ⚠ Trains from a PRE-BINNED dataset, shared through [`BinCache`] by every fit of the same
    /// data at the same `min_data_in_leaf` — the fold-keyed binning cache this doc used to
    /// prophesy ("if the grid ever gets slow enough to matter"), keyed on the data's CONTENT
    /// because the seam hands `fit` no fold identity. A fold's 20 search trials therefore write
    /// and parse the ~8 MB training CSV at most three times (once per `min_data_in_leaf` value
    /// reached) instead of twenty, and every later fit reads a ~1 MB binary.
    ///
    /// The models are BIT-IDENTICAL to per-fit CSV construction — not assumed: binning bakes
    /// `min_data_in_leaf` (via LightGBM's default `feature_pre_filter`) and the categorical
    /// declaration, which is why both are in [`BinKey`]; the matrix smoke below proves
    /// byte-identical model text against a per-fit CSV train for all three values of every
    /// searched axis, and `crates/vike-ml/src/train.rs`'s `BinPreFilter` carries the measured
    /// divergence of the two schemes that DON'T hold it.
    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Self::Model, String> {
        check_shape(d)?;
        let params = params_for(&self.base, p, seed, d.categorical)?;
        // Held for the whole fit: an LRU eviction drops only the cache's handle, so the binary
        // cannot be deleted out from under the child process reading it.
        let bin = binned_for(self, d, &params)?;
        let dir = self.fit_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let out = fit_in(&self.cli, &dir, &bin.bin, &params);
        // Removed on BOTH outcomes: a rejected grid point is an ordinary event here (242 usable
        // points still beat 20 TPE trials), and 243 leaked directories per fold is a worse
        // diagnostic than the error string, which already carries LightGBM's own stderr tail.
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// The cross-run fit cache's train half: a digest over THE ACTUAL BYTES [`Self::fit`] would
    /// consume for `(d, p, seed)` — content-addressed, never a description
    /// (see [`crate::fit_cache`]'s module doc for the constraint this serves).
    ///
    /// # The enumeration — every input of a fit, and which part covers it
    ///
    /// A fit is: bin `d` at `params` ([`bin_once`] → `write_csv` → the child's `save_binary`),
    /// then train from the binary ([`fit_in`] → [`train_config`] → the child), then parse the
    /// model text. Its inputs, exhaustively:
    ///
    /// * **the training data** (`d.x`, `d.y`, `d.n_rows`, `d.n_cols`) — covered by the CSV part:
    ///   the EXACT bytes `TrainData::write_csv` streams to the binning (label first, features,
    ///   `nan` spelling, shortest-roundtrip f64 formatting), hashed through [`fit_cache::DigestWriter`]
    ///   without materialising. Keying the RENDER rather than the raw f64 bits is deliberate: a
    ///   formatter change in the model crate changes what the child reads, and must re-key.
    /// * **the grid point, the seed, the categorical declaration and every non-searched
    ///   parameter** — covered by the two RENDER parts: the `Task::Train` config and the
    ///   `Task::SaveBinary(MatchFit)` config, rendered exactly as a fit renders them but with the
    ///   two PATH lines normalised to placeholders (paths differ per run and provably do not
    ///   reach the trees — the matrix smoke below is that proof). Between them these carry all
    ///   five axes, `seed`, `categorical_feature`, `num_iterations`/`lambda_l1`/`lambda_l2`/
    ///   `bagging_*`, `objective`, `num_threads`, `deterministic`, `force_row_wise`,
    ///   `precise_float_parser` — and the [`BinPreFilter`] CHOICE, which is dataset-baked and
    ///   whose `MatchFit`-vs-`MinDataAgnostic` difference trains measurably different models.
    /// * **the trainer binary** — covered by the PROVENANCE part: the digest of the `PROVENANCE`
    ///   file bytes read at construction. A residual inherited from the pin check itself
    ///   (`crates/vike-ml/src/cli.rs`'s module doc): the file is the build recipe's self-report,
    ///   so a hand-swapped binary under a stale PROVENANCE defeats both the pin and this key.
    /// * **the model TEXT FORMAT the parse expects** — `crate::MODEL_VERSION`, one part.
    ///
    /// NOT inputs, deliberately excluded: the scratch paths (normalised, see above) and the
    /// in-process inference/scorer code (the score key's declared residual — the cache module doc
    /// carries it, with the schema tag to bump).
    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        let provenance = self.provenance?;
        // The same refusals `fit` applies, in the same order — a shape or parameter `fit` would
        // reject gets no identity, so a rejection is never served from (or stored into) the cache.
        if check_shape(d).is_err() {
            return None;
        }
        let params = params_for(&self.base, p, seed, d.categorical).ok()?;
        let train_render =
            train_config(&params, Path::new("DATA"), Path::new("MODEL")).render().ok()?;
        let bin_render = TrainConfig {
            task: Task::SaveBinary(BinPreFilter::MatchFit),
            params: &params,
            data: Path::new("DATA"),
            output_model: None,
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .ok()?;
        let mut csv = fit_cache::DigestWriter::new();
        d.write_csv(&mut csv).ok()?;

        let mut t = fit_cache::Transcript::new("vike-research fit-identity v1");
        t.digest_part(provenance);
        t.part(crate::MODEL_VERSION.as_bytes());
        t.part(train_render.as_bytes());
        t.part(bin_render.as_bytes());
        t.digest_part(csv.finish());
        Some(t.finish())
    }

    /// The winner-refit capture: the same fit, keeping the model TEXT long enough to answer
    /// whatever `want` asked for — its `split_feature=`/`split_gain=` arrays folded into a
    /// [`FitImportance`] (`crates/vike-ml/src/importance.rs`'s `importance_from_model_text`
    /// carries the verified format — it parses vike-ml's own `save_model` text and lives with
    /// it), and the text ITSELF for `--model-out`.
    ///
    /// ⚠ The text handed back is the trainer's own bytes, never a re-serialisation of the parsed
    /// [`GbdtModel`]: this workspace has no model WRITER, and adding one would be a second
    /// spelling of the format to keep in step with LightGBM's. The scratch `model.txt` the child
    /// wrote is deleted below with the rest of the fit directory — the `String` is the artifact
    /// that survives, which is exactly why it has to be handed UP rather than left on disk.
    ///
    /// ⚠ The body is deliberately spelled BESIDE [`Learner::fit`] rather than folded into it:
    /// `fit` is the per-trial hot path (244 calls per fold) and its orchestration is left
    /// byte-untouched, while this method runs once per fold and only under a capture flag.
    /// The two share every real step through the same helpers (`check_shape`, `params_for`,
    /// `binned_for`, `train_config`, `parse_model_text`), so the only duplicated lines are the
    /// scratch-directory choreography — same lifecycle, removed on both outcomes.
    ///
    /// Each half of `want` is answered on its own: a `--model-out` run pays no importance parse
    /// and — the part that matters — cannot be FAILED by one, while an `--importance-out` run
    /// drops the text instead of carrying a megabyte per fold up to a caller that keeps none.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<Self::Model>, String> {
        check_shape(d)?;
        let params = params_for(&self.base, p, seed, d.categorical)?;
        let bin = binned_for(self, d, &params)?;
        let dir = self.fit_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let model_path = dir.join("model.txt");
        let text = self.cli.train(&train_config(&params, &bin.bin, &model_path), &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let text = text.map_err(|e| e.to_string())?;
        let model = parse_model_text(&text).map_err(|e| e.to_string())?;
        // The type is named rather than inferred, so `FitImportance` is a real use of the import
        // and the doc link above cannot rot into an unused one.
        let importance: Option<FitImportance> =
            want.importance.then(|| importance_from_model_text(&text)).transpose()?;
        Ok((model, importance, want.text.then_some(text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The trait whose impl for `GbdtModel` lives in `crates/vike-ml/src/learner.rs` now. Named
    // HERE rather than in the module above, because the library half of this file no longer
    // mentions it — only these tests reach for the trait method by its qualified name.
    use crate::ProbaModel;

    /// A point whose every axis differs from `GbdtParams::default()` (`31 / -1 / 20 / 0.1 / 1.0`).
    ///
    /// Load-bearing, not arbitrary: three of the study's grid values COINCIDE with a library
    /// default (`num_leaves=31`, `min_data_in_leaf=20`, `learning_rate=0.1`), so a point built
    /// from those would let a dropped mapping pass every assertion below.
    fn point() -> GridPoint {
        GridPoint {
            num_leaves: 7,
            max_depth: 3,
            min_data_in_leaf: 10,
            learning_rate: 0.03,
            feature_fraction: 0.5,
        }
    }

    /// A caller's base bag whose four regularisation keys all DIFFER from
    /// [`GbdtParams::default`] — the cohort study's own `config.py` values, kept here as a
    /// FIXTURE rather than as library state.
    ///
    /// Load-bearing in the same way [`point`] is: a base that agreed with the library everywhere
    /// would let `params_for` ignore its `base` argument entirely and still pass every assertion
    /// below.
    fn base() -> GbdtParams {
        GbdtParams {
            num_iterations: 100,
            lambda_l1: 0.1,
            lambda_l2: 0.1,
            bagging_fraction: 0.7,
            bagging_freq: 1,
            ..GbdtParams::default()
        }
    }

    fn line<'a>(conf: &'a str, key: &str) -> Option<&'a str> {
        conf.lines().find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
    }

    #[test]
    fn every_searched_axis_reaches_the_parameter_it_names() {
        let p = params_for(&base(), &point(), 0, &[]).unwrap();
        assert_eq!(p.num_leaves, 7);
        assert_eq!(p.max_depth, 3);
        assert_eq!(p.min_data_in_leaf, 10);
        assert_eq!(p.learning_rate, 0.03);
        assert_eq!(p.feature_fraction, 0.5);
        // ...and none of them is merely the library default surviving untouched, which is what a
        // DROPPED axis looks like from the inside.
        let d = GbdtParams::default();
        assert_ne!(p.num_leaves, d.num_leaves);
        assert_ne!(p.max_depth, d.max_depth);
        assert_ne!(p.min_data_in_leaf, d.min_data_in_leaf);
        assert_ne!(p.learning_rate, d.learning_rate);
        assert_ne!(p.feature_fraction, d.feature_fraction);
    }

    #[test]
    fn the_callers_non_searched_parameters_reach_the_fit_and_the_librarys_do_not() {
        let p = params_for(&base(), &point(), 0, &[]).unwrap();
        let d = GbdtParams::default();
        assert_eq!(p.num_iterations, 100, "the caller's bag, not the library's");
        assert_eq!(p.lambda_l1, 0.1);
        assert_eq!(p.lambda_l2, 0.1);
        assert_eq!(p.bagging_fraction, 0.7);
        assert_eq!(p.bagging_freq, 1);
        assert_eq!(p.objective, "binary");
        // The four that the library would NOT have given us. Each of these is a whole setting that
        // vanishes without a sound if the base bag is ignored, and `bagging_freq`
        // is the cruel one: at the library default of `0`, `bagging_fraction=0.7` is inert.
        assert_ne!(p.lambda_l1, d.lambda_l1);
        assert_ne!(p.lambda_l2, d.lambda_l2);
        assert_ne!(p.bagging_fraction, d.bagging_fraction);
        assert_ne!(p.bagging_freq, d.bagging_freq);
    }

    #[test]
    fn no_searched_axis_is_also_pinned_in_the_base_where_it_could_mask_a_dropped_mapping() {
        // Answer (a), asserted: the base must not restate a searched axis. If it did, a mapping
        // that dropped that axis would train the CALLER'S DEFAULT — a plausible number — and
        // `every_searched_axis_reaches_the_parameter_it_names` is the only thing that would notice.
        let b = base();
        let d = GbdtParams::default();
        assert_eq!(b.num_leaves, d.num_leaves);
        assert_eq!(b.max_depth, d.max_depth);
        assert_eq!(b.min_data_in_leaf, d.min_data_in_leaf);
        assert_eq!(b.learning_rate, d.learning_rate);
        assert_eq!(b.feature_fraction, d.feature_fraction);
    }

    #[test]
    fn the_seed_the_seam_was_called_with_is_the_seed_the_fit_uses() {
        // Reproducibility of the whole run driver rests on this one assignment, and the library's
        // own default (42) is a perfectly plausible number to see in a config file.
        let p = params_for(&base(), &point(), 7, &[]).unwrap();
        assert_eq!(p.seed, 7);
        assert_ne!(p.seed, GbdtParams::default().seed);
    }

    #[test]
    fn a_categorical_column_is_declared_as_a_comma_separated_list_with_no_brackets() {
        let p = params_for(&base(), &point(), 0, &[3, 5]).unwrap();
        assert_eq!(p.categorical_feature.as_deref(), Some("3,5"));
        let csv = p.categorical_feature.unwrap();
        // A bracketed list reaches LightGBM's `KV2Map` as the LITERAL `[3,5]`, names no column,
        // and the model is trained with the column binned as a NUMBER while its own parameter
        // echo still says otherwise.
        assert!(!csv.contains('['), "{csv}");
        assert!(!csv.contains(' '), "the parser splits on commas, not whitespace");
    }

    #[test]
    fn no_categorical_column_means_the_key_is_absent_rather_than_empty() {
        // `Some("")` renders as `categorical_feature=`, which is a value LightGBM has to
        // interpret; saying nothing is not the same thing.
        assert_eq!(params_for(&base(), &point(), 0, &[]).unwrap().categorical_feature, None);
    }

    #[test]
    fn every_parameter_this_file_sets_survives_into_the_rendered_config() {
        // The struct assertions above stop one step short of the wire. This renders the SAME
        // `TrainConfig` a fit runs, so a mapping that is right in `params_for` and unused in
        // `fit` — or a field the renderer does not write — is visible here and nowhere else.
        let p = params_for(&base(), &point(), 7, &[2]).unwrap();
        let conf =
            train_config(&p, Path::new("/tmp/d.csv"), Path::new("/tmp/m.txt")).render().unwrap();
        for (key, value) in [
            ("num_leaves", "7"),
            ("max_depth", "3"),
            ("min_data_in_leaf", "10"),
            ("learning_rate", "0.03"),
            ("feature_fraction", "0.5"),
            ("num_iterations", "100"),
            ("lambda_l1", "0.1"),
            ("lambda_l2", "0.1"),
            ("bagging_fraction", "0.7"),
            ("bagging_freq", "1"),
            ("objective", "binary"),
            ("seed", "7"),
            ("categorical_feature", "2"),
            // The 68x threading cliff, refused by `check_grid_params` at fit time and written
            // here so a binary rebuilt WITH OpenMP cannot resurrect it silently.
            ("num_threads", "1"),
        ] {
            assert_eq!(line(&conf, key), Some(value), "{key}");
        }
        assert_eq!(line(&conf, "task"), Some("train"));
    }

    /// A learner whose binary path is a lie — for the tests that are about what happens BEFORE
    /// anything is spawned. `LightGbmCli::unchecked` skips the provenance probe.
    ///
    /// ⚠ Returns the [`vike_model::scratch::ScratchDir`] ALONGSIDE the learner, and the caller must bind
    /// it — dropping it removes the scratch root the learner is pointed at. It used to build that
    /// root from `env::temp_dir()` + the pid and leave each test to `remove_dir_all` it on the
    /// PASSING path only, which leaks on every assertion failure and, because a PID is reused,
    /// hands the next run under the other the CI box user a directory it cannot write into.
    fn unchecked_learner(tag: &str) -> (vike_model::scratch::ScratchDir, GbdtLearner) {
        let scratch = vike_model::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            &format!("vike_ml_gbdt_learner_{tag}"),
        )
        .expect("scratch dir");
        let learner = GbdtLearner {
            cli: LightGbmCli::unchecked(Path::new("vike-no-such-lightgbm-binary")),
            scratch: scratch.path().to_path_buf(),
            next: AtomicU64::new(0),
            bins: BinCache::new(DEFAULT_BIN_CAPACITY),
            base: base(),
            provenance: None,
        };
        (scratch, learner)
    }

    #[test]
    fn fit_checks_the_shape_before_it_spawns_anything() {
        // Testing `check_shape` directly proves the function; this proves it is REACHED, and in
        // the right order. The control case is the whole point: with a well-shaped matrix the same
        // call gets all the way to the spawn and fails there instead.
        let (_scratch, l) = unchecked_learner("order");
        let x = [0.0; 6];
        let y = [0.0f32; 3];
        let bad = TrainData { x: &x[..4], n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let err = l.fit(&bad, &point(), 0).unwrap_err();
        assert!(err.contains("feature values"), "the shape refusal, not a spawn failure: {err}");
        assert!(!err.contains("vike-no-such-lightgbm-binary"), "{err}");

        let good = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let err = l.fit(&good, &point(), 0).unwrap_err();
        assert!(err.contains("vike-no-such-lightgbm-binary"), "reached the spawn: {err}");
    }

    #[test]
    fn a_failed_fit_leaves_no_scratch_directory_behind() {
        // 243 points x 92 folds of leaked directories is a filesystem problem, and a rejected
        // point is an ordinary event rather than an exceptional one.
        let (_scratch, l) = unchecked_learner("cleanup");
        let x = [0.0; 6];
        let y = [0.0f32; 3];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        assert!(l.fit(&d, &point(), 0).is_err());
        let left: Vec<_> = std::fs::read_dir(&l.scratch).unwrap().filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "{} entries left in the scratch root", left.len());
    }

    /// A [`Trainer`] that trains nothing and keeps what it was asked to train with.
    ///
    /// This is the whole reason [`Trainer`] exists. `fit` builds a [`GbdtParams`] and hands it to a
    /// child process; from inside this process that handover is invisible, so `&params` and
    /// `&GbdtParams::default()` were indistinguishable to every test that runs in CI. Here the
    /// handover lands in a `Vec` a test can read — for BOTH halves: the binning (which bakes
    /// `min_data_in_leaf` and the categorical declaration) records its params and output path, and
    /// the train records its params and the data path it was pointed at, so a test can assert the
    /// fit trains from the binary it just had made.
    ///
    /// `Mutex` rather than `RefCell` because [`Trainer`] is `Sync`: the grid search shares one
    /// learner across `rayon` threads, and a double that could not be is a double the real bound
    /// would reject.
    #[derive(Default)]
    struct RecordingTrainer {
        seen: std::sync::Mutex<Vec<(GbdtParams, PathBuf)>>,
        binned: std::sync::Mutex<Vec<(GbdtParams, PathBuf)>>,
    }

    impl Trainer for RecordingTrainer {
        fn train(&self, conf: &TrainConfig<'_>, _scratch: &Path) -> Result<String, MlError> {
            self.seen.lock().unwrap().push((conf.params.clone(), conf.data.to_path_buf()));
            Ok(canned_model_text())
        }

        fn save_binary(
            &self,
            _data: &TrainData<'_>,
            params: &GbdtParams,
            out: &Path,
            _scratch: &Path,
        ) -> Result<(), MlError> {
            self.binned.lock().unwrap().push((params.clone(), out.to_path_buf()));
            Ok(())
        }
    }

    /// A one-tree, one-leaf model in the text format `parse_model_text` reads — LightGBM's own
    /// constant tree, which is what `AsConstantTree` emits when a fit could not split at all.
    ///
    /// `leaf_value` is deliberately not `0`: it makes the model the recording double returns
    /// identifiable, so a test can assert the fit's answer CAME FROM the trainer rather than from
    /// somewhere else. The version comes from the model crate's own constant so a format bump is a
    /// compile-time-visible one-line fix here rather than a mystery parse error.
    fn canned_model_text() -> String {
        format!(
            "tree\nversion={}\nnum_class=1\nnum_tree_per_iteration=1\nmax_feature_idx=0\n\
             objective=binary sigmoid:1\nfeature_names=f0\nTree=0\nnum_leaves=1\n\
             leaf_value=0.75\nend of trees\n",
            crate::MODEL_VERSION
        )
    }

    /// A learner whose trainer records instead of spawning. Same scratch machinery as
    /// [`unchecked_learner`] — the fit under test is a REAL one right up to the process boundary,
    /// and the returned guard must be bound for the same reason.
    fn recording_learner(
        tag: &str,
    ) -> (vike_model::scratch::ScratchDir, GbdtLearner<RecordingTrainer>) {
        let scratch = vike_model::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            &format!("vike_ml_gbdt_learner_{tag}"),
        )
        .expect("scratch dir");
        let learner = GbdtLearner {
            cli: RecordingTrainer::default(),
            scratch: scratch.path().to_path_buf(),
            next: AtomicU64::new(0),
            bins: BinCache::new(DEFAULT_BIN_CAPACITY),
            base: base(),
            provenance: None,
        };
        (scratch, learner)
    }

    #[test]
    fn the_trainer_is_handed_the_parameters_the_fit_just_built() {
        // THE mutation this file could not previously kill: `&GbdtParams::default()` in place of
        // `&params` in `fit`. Every assertion above stops at `params_for` or at a config a TEST
        // rendered; this one reads what the fit itself passed across the boundary. Silent, and the
        // most consequential failure available here — all 243 grid points would train identically,
        // the search would still name a winner, and the winner would be noise.
        let (_scratch, l) = recording_learner("recorded");
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[1] };
        let m = l.fit(&d, &point(), 7).expect("the recording trainer answers a parsable model");
        // The control case: without it, everything below could be reading an empty recording made
        // by a fit that failed before it ever reached the trainer.
        assert_eq!(m.trees.len(), 1);
        assert_eq!(m.trees[0].leaf_value, vec![0.75], "the model came back from the trainer");

        let seen = l.cli.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one fit, one train call");
        let (got, trained_from) = &seen[0];
        let def = GbdtParams::default();
        // Each axis against its VALUE and against the library default it would wear if some other
        // bag had been handed over. Both halves are needed and neither is decoration: `point()` is
        // chosen so no axis coincides with a default (see its doc), and the whole-bag equality at
        // the end cannot see an axis dropped from `point_bag` — that drop removes it from BOTH
        // sides of the comparison at once.
        assert_eq!(got.num_leaves, 7, "num_leaves");
        assert_ne!(got.num_leaves, def.num_leaves, "num_leaves is the default's");
        assert_eq!(got.max_depth, 3, "max_depth");
        assert_ne!(got.max_depth, def.max_depth, "max_depth is the default's");
        assert_eq!(got.min_data_in_leaf, 10, "min_data_in_leaf");
        assert_ne!(got.min_data_in_leaf, def.min_data_in_leaf, "min_data_in_leaf is the default's");
        assert_eq!(got.learning_rate, 0.03, "learning_rate");
        assert_ne!(got.learning_rate, def.learning_rate, "learning_rate is the default's");
        assert_eq!(got.feature_fraction, 0.5, "feature_fraction");
        assert_ne!(got.feature_fraction, def.feature_fraction, "feature_fraction is the default's");
        // The seed and the categorical declaration are the same question about two fields the
        // search never sweeps: the run driver's reproducibility rests on one, and the pooled fit's
        // whole point rests on the other.
        assert_eq!(got.seed, 7, "seed");
        assert_ne!(got.seed, def.seed, "the seed is the default's (42), not the caller's");
        assert_eq!(got.categorical_feature.as_deref(), Some("1"), "categorical_feature");
        assert_eq!(def.categorical_feature, None, "...and the default declares nothing at all");
        // ...and the whole bag, so a field the list above forgets is still not free to differ.
        assert_eq!(*got, params_for(&base(), &point(), 7, &[1]).unwrap());

        // The binning half of the handover: the SAME bag reaches `save_binary` — where
        // `min_data_in_leaf` and the categorical declaration are actually consumed — and the
        // train reads the exact binary that binning produced, not a path of its own invention.
        let binned = l.cli.binned.lock().unwrap();
        assert_eq!(binned.len(), 1, "one new dataset, one binning");
        let (bin_params, bin_out) = &binned[0];
        assert_eq!(bin_params.min_data_in_leaf, 10, "the value the binning bakes");
        assert_eq!(bin_params.categorical_feature.as_deref(), Some("1"));
        assert_eq!(*bin_params, *got, "binning and train were handed different bags");
        assert_eq!(trained_from, bin_out, "the fit trained from some other file than its binary");
        drop(binned);
        drop(seen);
    }

    #[test]
    fn two_fits_never_share_a_scratch_directory() {
        // `fit` is called from `rayon::par_iter`. One shared config path across concurrent fits is
        // two fits training each other's parameters, and neither would report anything.
        let (_scratch, l) = unchecked_learner("unique");
        let dirs: Vec<PathBuf> = (0..64).map(|_| l.fit_dir()).collect();
        let mut sorted = dirs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), dirs.len(), "a fit directory was handed out twice");
    }

    #[test]
    fn a_folds_worth_of_fits_bins_once_and_every_fit_trains_from_that_binary() {
        // The whole point of the cache: a fold's search hands `fit` the SAME `TrainData` per
        // trial, so trials that share a `min_data_in_leaf` share one binning — the ~8 MB CSV is
        // written and parsed once, not once per trial.
        let (_scratch, l) = recording_learner("bin_shared");
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let other_leaves = GridPoint { num_leaves: 15, ..point() };
        assert_ne!(other_leaves.num_leaves, point().num_leaves);
        l.fit(&d, &point(), 7).unwrap();
        l.fit(&d, &point(), 7).unwrap();
        l.fit(&d, &other_leaves, 7).unwrap();

        let binned = l.cli.binned.lock().unwrap();
        let seen = l.cli.seen.lock().unwrap();
        assert_eq!(binned.len(), 1, "three fits at one min_data_in_leaf must share one binning");
        assert_eq!(seen.len(), 3, "the fits themselves are never deduplicated");
        for (i, (_, data)) in seen.iter().enumerate() {
            assert_eq!(*data, binned[0].1, "fit {i} trained from some other file");
        }
        drop(binned);
        drop(seen);
    }

    #[test]
    fn a_fit_at_a_different_min_data_in_leaf_gets_its_own_binary() {
        // ⚠ THE key rule, gated where CI can see it: `min_data_in_leaf` is consumed at dataset
        // CONSTRUCTION (LightGBM's default `feature_pre_filter` drops features against it, and a
        // dropped feature leaves the `feature_fraction` sampling universe), so a bin built at one
        // value trains DIFFERENT trees at another — measured on the real binary, pinned there by
        // the matrix smoke below. A cache key that forgot the axis would silently serve trial B
        // the pre-filter of trial A's value; this test is what a "simplified" key reddens in an
        // ordinary `cargo test`.
        let (_scratch, l) = recording_learner("bin_min_data");
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let coarse = GridPoint { min_data_in_leaf: 50, ..point() };
        l.fit(&d, &point(), 7).unwrap();
        l.fit(&d, &coarse, 7).unwrap();

        let binned = l.cli.binned.lock().unwrap();
        let seen = l.cli.seen.lock().unwrap();
        assert_eq!(binned.len(), 2, "two min_data_in_leaf values, two dataset constructions");
        assert_eq!(binned[0].0.min_data_in_leaf, 10);
        assert_eq!(binned[1].0.min_data_in_leaf, 50);
        assert_ne!(binned[0].1, binned[1].1, "the two binaries share a path");
        assert_eq!(seen[0].1, binned[0].1, "the first fit's data is the first binary");
        assert_eq!(seen[1].1, binned[1].1, "the second fit's data is the second binary");
        drop(binned);
        drop(seen);
    }

    #[test]
    fn a_different_dataset_gets_its_own_binary_even_at_the_same_point() {
        // The content half of [`BinKey`]: same shape, same point, different bytes — the final
        // whole-fold refit against the search's 70% slice, or two concurrent folds.
        let (_scratch, l) = recording_learner("bin_content");
        let x1 = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let x2 = [0.0, 1.0, 2.0, 3.0, 4.0, 6.0];
        let y = [0.0f32, 1.0, 0.0];
        let d1 = TrainData { x: &x1, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let d2 = TrainData { x: &x2, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        l.fit(&d1, &point(), 7).unwrap();
        l.fit(&d2, &point(), 7).unwrap();
        assert_eq!(l.cli.binned.lock().unwrap().len(), 2);
    }

    #[test]
    fn the_fingerprint_sees_a_change_the_pointer_and_lengths_cannot() {
        // The ABA guard: a freed fold's buffer address can be REUSED by a later fold of identical
        // size, and the pointer-plus-lengths half of the key would then serve the dead fold's
        // binary. Only the content fingerprint stands between those two keys, so it must actually
        // move when any element moves — including a tail element, which a truncated fold shares
        // everything before.
        let a = [0.0, 1.0, 2.0, 3.0];
        let b = [0.0, 1.0, 2.0, 4.0];
        assert_ne!(fingerprint_f64(&a), fingerprint_f64(&b));
        assert_eq!(fingerprint_f64(&a), fingerprint_f64(&[0.0, 1.0, 2.0, 3.0]));
        assert_ne!(fingerprint_labels(&[0.0, 1.0, 0.0]), fingerprint_labels(&[0.0, 1.0, 1.0]));
        assert_ne!(fingerprint_labels(&[0.0]), fingerprint_labels(&[-0.0]));
        // -0.0 and 0.0 are == as floats but different BYTES, and the CSV the binning writes spells
        // them differently — so the fingerprint must too, which is why it hashes bits.
        assert_ne!(fingerprint_f64(&[0.0]), fingerprint_f64(&[-0.0]));
    }

    #[test]
    fn an_evicted_binary_is_deleted_from_disk_and_an_in_flight_one_survives() {
        // Eviction drops the CACHE's handle; the directory dies with the LAST handle. An in-flight
        // fit holding the file must keep it alive — deleting a binary a child process is reading
        // is a fit that fails (or worse, reads a torn file) with nothing naming the cause.
        let root = vike_model::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            "vike_ml_gbdt_learner_evict",
        )
        .expect("scratch dir");
        let file = |tag: &str| {
            let dir = root.join(tag);
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("train.bin");
            std::fs::write(&bin, b"x").unwrap();
            Arc::new(BinFile { dir, bin })
        };
        let key = |min_data: u32| BinKey {
            x_ptr: 0,
            x_len: 6,
            n_rows: 3,
            n_cols: 2,
            x_hash: u64::from(min_data), // distinct keys, spelled through a field the test owns
            y_hash: 0,
            categorical: Vec::new(),
            min_data_in_leaf: min_data,
        };

        let cache = BinCache::new(2);
        let a = file("a");
        let held = Arc::clone(&a);
        cache.slot(key(1)).once.set(Ok(a)).unwrap();
        cache.slot(key(2)).once.set(Ok(file("b"))).unwrap();
        assert!(root.join("a").exists() && root.join("b").exists());

        // Key 3 evicts key 1 (LRU front) — but `held` is still in flight, so the file survives...
        cache.slot(key(3)).once.set(Ok(file("c"))).unwrap();
        assert!(root.join("a").exists(), "evicted while in flight must not be deleted");
        drop(held);
        assert!(!root.join("a").exists(), "...and dies with its last handle");
        assert!(root.join("b").exists() && root.join("c").exists());

        // A hit REFRESHES: touching key 2 makes key 3 the LRU victim of the next insert.
        cache.slot(key(2));
        cache.slot(key(4)).once.set(Ok(file("d"))).unwrap();
        assert!(root.join("b").exists(), "the refreshed entry outlived the newer one");
        assert!(!root.join("c").exists(), "the stale entry was the victim");
    }

    /// A [`Trainer`] answering a model text WITH splits, in the verified v4.7.0 per-tree shape —
    /// what lets the capture test below pin real gain arithmetic without a binary.
    struct SplitTextTrainer;

    impl Trainer for SplitTextTrainer {
        fn train(&self, _conf: &TrainConfig<'_>, _scratch: &Path) -> Result<String, MlError> {
            Ok(format!(
                "tree\nversion={}\nnum_class=1\nnum_tree_per_iteration=1\nmax_feature_idx=1\n\
                 objective=binary sigmoid:1\nfeature_names=Column_0 Column_1\nTree=0\n\
                 num_leaves=3\nsplit_feature=0 1\nsplit_gain=8.5 2.25\nthreshold=0.5 0.5\n\
                 decision_type=2 2\nleft_child=1 -1\nright_child=-2 -3\n\
                 leaf_value=0.1 0.2 0.3\nend of trees\n",
                crate::MODEL_VERSION
            ))
        }

        fn save_binary(
            &self,
            _data: &TrainData<'_>,
            _params: &GbdtParams,
            _out: &Path,
            _scratch: &Path,
        ) -> Result<(), MlError> {
            Ok(())
        }
    }

    /// The two capture requests the tests below make, named so a reader sees WHICH artifact each
    /// assertion is about rather than decoding a pair of bare bools at the call site.
    const IMPORTANCE_ONLY: Capture = Capture { importance: true, text: false };
    const TEXT_ONLY: Capture = Capture { importance: false, text: true };
    const BOTH: Capture = Capture { importance: true, text: true };

    #[test]
    fn fit_captured_reports_the_importance_the_trainers_own_text_says() {
        // The capture path end-to-end minus the child process: the SAME text the model is parsed
        // from is the text the importance is folded from, so the two cannot describe different
        // fits. Gains and split counts are pinned per index — an off-by-one in either the parser's
        // accumulation or a later name zip moves them.
        let scratch = vike_model::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            "vike_ml_gbdt_learner_imp",
        )
        .expect("scratch dir");
        let l = GbdtLearner {
            cli: SplitTextTrainer,
            scratch: scratch.path().to_path_buf(),
            next: AtomicU64::new(0),
            bins: BinCache::new(DEFAULT_BIN_CAPACITY),
            base: base(),
            provenance: None,
        };
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let (m, imp, text) = l.fit_captured(&d, &point(), 7, IMPORTANCE_ONLY).unwrap();
        assert_eq!(m.trees.len(), 1, "the model came back from the trainer");
        assert_eq!(text, None, "an importance-only request must not carry the model text");
        let imp = imp.expect("the real learner always reports importance");
        assert_eq!(imp.n_features, 2, "the MODEL's declaration, max_feature_idx + 1");
        assert_eq!(imp.gain, vec![8.5, 2.25]);
        assert_eq!(imp.splits, vec![1, 1]);
        // ...and the capture path leaves the scratch root as clean as `fit` does — once the
        // learner is gone. While it lives, its BinCache legitimately holds this fold's binned
        // dataset directory (that residency is the cache's whole point), so the emptiness claim
        // is about the learner's END, not its middle.
        drop(l);
        let left: Vec<_> = std::fs::read_dir(&scratch).unwrap().filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "{} entries left in the scratch root", left.len());
    }

    #[test]
    fn fit_captured_still_checks_the_shape_and_still_cleans_up_on_failure() {
        // The capture path shares `fit`'s door and its lifecycle: a malformed matrix is refused
        // before anything spawns, and a failed train leaves no scratch directory behind.
        let (_scratch, l) = unchecked_learner("imp_order");
        let x = [0.0; 6];
        let y = [0.0f32; 3];
        let bad = TrainData { x: &x[..4], n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let err = l.fit_captured(&bad, &point(), 0, BOTH).unwrap_err();
        assert!(err.contains("feature values"), "the shape refusal, not a spawn failure: {err}");

        let good = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let err = l.fit_captured(&good, &point(), 0, BOTH).unwrap_err();
        assert!(err.contains("vike-no-such-lightgbm-binary"), "reached the spawn: {err}");
        let left: Vec<_> = std::fs::read_dir(&l.scratch).unwrap().filter_map(|e| e.ok()).collect();
        assert!(left.is_empty(), "{} entries left in the scratch root", left.len());
    }

    #[test]
    fn fit_captured_on_a_constant_tree_reports_explicit_zeros() {
        // The recording trainer answers `canned_model_text` — LightGBM's own constant tree — so
        // the importance is a table of zeros at the model's declared width, never `None`: "never
        // used" is an answer the capture must state, not omit.
        let (_scratch, l) = recording_learner("imp_const");
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[1] };
        let (_, imp, _) = l.fit_captured(&d, &point(), 7, IMPORTANCE_ONLY).unwrap();
        let imp = imp.expect("the real learner always reports importance");
        assert_eq!(imp.n_features, 1, "canned_model_text declares max_feature_idx=0");
        assert_eq!(imp.gain, vec![0.0]);
        assert_eq!(imp.splits, vec![0]);
        // The handover is the same one `fit` makes — the recorded params prove the capture path
        // did not invent its own bag.
        assert_eq!(l.cli.seen.lock().unwrap().len(), 1);
    }

    /// ⚠ THE export claim, at the only boundary that can state it: the text handed back is the
    /// trainer's OWN bytes, unchanged — not a re-serialisation of the parsed [`GbdtModel`], which
    /// no crate in this workspace can produce and which would be a second spelling of the model to
    /// keep in step. `crate::parse_model_text` reads what is written here, so an exported file is
    /// loadable by the pure-Rust walker on any platform.
    #[test]
    fn fit_captured_hands_back_the_trainers_own_model_text_byte_for_byte() {
        let (_scratch, l) = recording_learner("text_export");
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[1] };
        let (m, imp, text) = l.fit_captured(&d, &point(), 7, TEXT_ONLY).unwrap();
        assert_eq!(m.trees.len(), 1, "the model came back from the trainer");
        assert_eq!(imp, None, "a text-only request must not pay for the importance parse");
        let text = text.expect("the real learner always has its model's text");
        assert_eq!(text, canned_model_text(), "the export is not the trainer's own bytes");
        // ...and the exported bytes are what the pure-Rust walker reads back: an export nobody can
        // load is the failure this whole flag exists to prevent, and it would be INVISIBLE to a
        // string equality alone (the trainer's text could be malformed and still round-trip).
        let reloaded = crate::parse_model_text(&text).expect("the exported text must reload");
        assert_eq!(reloaded.trees.len(), m.trees.len());
        assert_eq!(reloaded.predict_proba(&[0.0, 0.0]), m.predict_proba(&[0.0, 0.0]));
    }

    /// Compile-time, and that is the point: `crates/vike-ml/src/learner.rs` bounds `Learner: Sync`
    /// and `ProbaModel: Send + Sync` so a grid search can be parallel, and answer (c) above says
    /// neither bound had to be relaxed. This is that claim, checked.
    #[test]
    fn the_learner_and_its_model_satisfy_the_bounds_the_parallel_search_needs() {
        fn sync<T: Sync>() {}
        fn send_sync<T: Send + Sync>() {}
        sync::<GbdtLearner>();
        send_sync::<GbdtModel>();
    }

    /// A learner whose ONLY departure from [`unchecked_learner`] is a provenance digest, so
    /// [`Learner::fit_identity`] engages. The identity is PURE — renders and hashes, no trainer,
    /// no filesystem — which is what lets these tests run in CI with no binary anywhere.
    fn identity_learner(provenance: &[u8]) -> GbdtLearner {
        GbdtLearner {
            cli: LightGbmCli::unchecked(Path::new("vike-no-such-lightgbm-binary")),
            scratch: std::env::temp_dir(), // never touched: fit_identity creates nothing
            next: AtomicU64::new(0),
            bins: BinCache::new(DEFAULT_BIN_CAPACITY),
            base: base(),
            provenance: Some(fit_cache::digest_of(provenance)),
        }
    }

    /// The REAL learner's key coverage, one component at a time — the unit halves of mutations
    /// M1 (seed), M2 (grid point) and M3 (content, never a path or an address).
    #[test]
    fn the_fit_identity_covers_every_axis_the_seed_the_data_and_the_provenance() {
        let l = identity_learner(b"tag=v4.7.0\nsha256=aa\n");
        let x = vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = vec![0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[1] };
        let base = l.fit_identity(&d, &point(), 7).expect("a provenanced learner has an identity");

        // Deterministic, and CONTENT-addressed: the same bytes in a fresh allocation share the
        // identity. Without this property no second run could ever hit — every fold re-packs its
        // buffers, so an address- or path-shaped key would be a cache that never fires (and
        // silently, which is why it is pinned here rather than hoped).
        let x2 = x.clone();
        let y2 = y.clone();
        let d2 = TrainData { x: &x2, n_rows: 3, n_cols: 2, y: &y2, categorical: &[1] };
        assert_eq!(l.fit_identity(&d2, &point(), 7), Some(base));

        // M1: the fit seed separates.
        assert_ne!(l.fit_identity(&d, &point(), 8), Some(base), "seed");
        // M2: every searched axis separates.
        for moved in [
            GridPoint { num_leaves: 15, ..point() },
            GridPoint { max_depth: 4, ..point() },
            GridPoint { min_data_in_leaf: 20, ..point() },
            GridPoint { learning_rate: 0.05, ..point() },
            GridPoint { feature_fraction: 0.7, ..point() },
        ] {
            assert_ne!(l.fit_identity(&d, &moved, 7), Some(base), "{moved:?}");
        }
        // M3: one changed VALUE separates — same shape, same allocation reused in place.
        let mut x3 = x.clone();
        x3[5] = 6.0;
        let d3 = TrainData { x: &x3, n_rows: 3, n_cols: 2, y: &y, categorical: &[1] };
        assert_ne!(l.fit_identity(&d3, &point(), 7), Some(base), "feature content");
        let y3 = vec![1.0f32, 1.0, 0.0];
        let d4 = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y3, categorical: &[1] };
        assert_ne!(l.fit_identity(&d4, &point(), 7), Some(base), "label content");
        // The categorical declaration separates (it is dataset-baked at binning).
        let d5 = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        assert_ne!(l.fit_identity(&d5, &point(), 7), Some(base), "categorical");
        // The same flat buffer at a different width separates (a different CSV).
        let y6 = vec![0.0f32, 1.0];
        let d6 = TrainData { x: &x, n_rows: 2, n_cols: 3, y: &y6, categorical: &[1] };
        assert_ne!(l.fit_identity(&d6, &point(), 7), Some(base), "shape");
        // And the trainer's provenance separates: same everything, different recorded build.
        let other = identity_learner(b"tag=v4.7.0\nsha256=bb\n");
        assert_ne!(other.fit_identity(&d, &point(), 7), Some(base), "provenance");
    }

    /// The two `None` arms: no provenance means no identity (the cache never engages for an
    /// unverified trainer), and a shape `fit` would refuse gets no identity either — so a
    /// rejection can never be served from the cache.
    #[test]
    fn an_unverified_trainer_or_a_misshapen_fit_has_no_identity() {
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let y = [0.0f32, 1.0, 0.0];
        let d = TrainData { x: &x, n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        let (_scratch, unverified) = unchecked_learner("no_identity");
        assert_eq!(unverified.fit_identity(&d, &point(), 7), None);
        let _ = std::fs::remove_dir_all(&unverified.scratch);

        let l = identity_learner(b"tag=v4.7.0\n");
        let bad = TrainData { x: &x[..4], n_rows: 3, n_cols: 2, y: &y, categorical: &[] };
        assert_eq!(l.fit_identity(&bad, &point(), 7), None, "a refusable shape got an identity");
        assert!(l.fit_identity(&d, &point(), 7).is_some(), "the control case");
    }

    /// The cross-param matrix behind the per-fold binary reuse: for EVERY searched axis, at ALL
    /// THREE of its values, a fit through the production bin-cache path must reproduce a per-fit
    /// CSV train BYTE FOR BYTE (the model text differs only in its `[data: …]` path echo, which
    /// names a different input file by construction — and differed between per-fit CSVs already).
    ///
    /// Any other differing line on any cell means that axis is DATASET-BAKED beyond what
    /// [`BinKey`] accounts for, i.e. the reuse silently pins a searched parameter — the
    /// silent-categorical failure class that disqualified the in-process bindings. This is the
    /// gate the brief's mutation aims at: key the cache without `min_data_in_leaf` (or bin
    /// `MinDataAgnostic`) and the `min_data_in_leaf` rows go red.
    ///
    /// The fixture carries a NEAR-CONSTANT column (non-zero in fewer rows than the largest
    /// `min_data_in_leaf` value) precisely so LightGBM's construction-time pre-filter BITES
    /// between axis values — and the test proves it does (the two binaries differ) before trusting
    /// any equality, because a fixture the pre-filter ignores would pass the matrix under the
    /// mutation too.
    ///
    /// ```text
    /// cargo test -p vike-ml --lib gbdt_learner -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
    fn the_binned_fit_matches_the_per_fit_csv_train_across_every_searched_axis_value() {
        // The five searched axes' values, restated as a FIXTURE. They are one study's grid
        // (`hl_cohort_research`'s `config.py`), and this matrix wants a range a real grid reaches
        // rather than three numbers of its own invention — but the grid itself is a caller's, so
        // the arrays live here rather than in this crate's API.
        const NUM_LEAVES: [u32; 3] = [7, 15, 31];
        const MAX_DEPTH: [i32; 3] = [3, 4, 5];
        const MIN_DATA_IN_LEAF: [u32; 3] = [10, 20, 50];
        const LEARNING_RATE: [f64; 3] = [0.03, 0.05, 0.1];
        const FEATURE_FRACTION: [f64; 3] = [0.5, 0.7, 0.9];

        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/lightgbm/lightgbm");
        if !binary.exists() {
            eprintln!(
                "SKIP: no LightGBM binary at {} — run `just lightgbm-build`",
                binary.display()
            );
            return;
        }
        let scratch = vike_model::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            "vike_ml_gbdt_matrix",
        )
        .expect("scratch dir");
        let l = GbdtLearner::new(&binary, &scratch, base()).unwrap();

        // 400 rows, 4 columns: two informative features, a NEAR-CONSTANT third (30 non-zero rows,
        // between min_data_in_leaf 10 and 50 so the pre-filter's verdict on it CHANGES across the
        // axis), and the trailing categorical asset code.
        let n = 400usize;
        let cols = 4usize;
        let mut x = Vec::with_capacity(n * cols);
        let mut y = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64;
            let a = (t * 0.37).sin();
            let b = (t * 0.11).cos();
            let rare = f64::from(u8::from(i % 13 == 0));
            let cat = (i % 4) as f64;
            x.extend_from_slice(&[a, b, rare, cat]);
            let bump = if cat as u32 == 2 { 0.6 } else { -0.2 };
            y.push(if a + 0.5 * b + bump > 0.0 { 1.0 } else { 0.0 });
        }
        let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[cols - 1] };

        // The fixture's own control: the pre-filter must actually DISCRIMINATE the axis here, or
        // an equality below proves nothing about the mutation this matrix exists to catch.
        let p10 = params_for(&base(), &point(), 7, d.categorical).unwrap();
        let p50 =
            params_for(&base(), &GridPoint { min_data_in_leaf: 50, ..point() }, 7, d.categorical)
                .unwrap();
        let bin10 = binned_for(&l, &d, &p10).unwrap();
        let bin50 = binned_for(&l, &d, &p50).unwrap();
        assert_ne!(
            std::fs::read(&bin10.bin).unwrap(),
            std::fs::read(&bin50.bin).unwrap(),
            "the binaries for min_data_in_leaf 10 and 50 are byte-identical — the fixture does \
             not exercise the pre-filter, so this matrix cannot see a pinned axis"
        );

        // A per-fit CSV train — the pre-reuse production path, reproduced verbatim: same
        // write_csv, same train_config, same params_for.
        let csv_fit = |params: &GbdtParams, tag: &str| -> String {
            let dir = scratch.join(format!("csv-{tag}"));
            std::fs::create_dir_all(&dir).unwrap();
            let csv = dir.join("train.csv");
            let mut buf = Vec::new();
            d.write_csv(&mut buf).unwrap();
            std::fs::write(&csv, &buf).unwrap();
            let text =
                Trainer::train(&l.cli, &train_config(params, &csv, &dir.join("m.txt")), &dir)
                    .unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            text
        };
        // The bin-path text, THROUGH the production cache (`binned_for`), trained by the same
        // `train_config` the production `fit_in` renders.
        let bin_fit = |params: &GbdtParams, tag: &str| -> String {
            let bin = binned_for(&l, &d, params).unwrap();
            let dir = scratch.join(format!("bin-{tag}"));
            std::fs::create_dir_all(&dir).unwrap();
            let text =
                Trainer::train(&l.cli, &train_config(params, &bin.bin, &dir.join("m.txt")), &dir)
                    .unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            text
        };
        // Byte comparison, minus the one line that names the input file. Any OTHER difference is
        // a dataset-baked axis the cache does not account for.
        let non_echo_diff = |a: &str, b: &str| -> Vec<String> {
            let strip = |t: &str| -> Vec<String> {
                t.lines().filter(|l| !l.starts_with("[data: ")).map(str::to_string).collect()
            };
            let (a, b) = (strip(a), strip(b));
            let mut out: Vec<String> = Vec::new();
            for i in 0..a.len().max(b.len()) {
                let (la, lb) = (a.get(i), b.get(i));
                if la != lb {
                    out.push(format!("line {i}: {la:?} vs {lb:?}"));
                }
            }
            out
        };

        let base_point = point();
        let cells: Vec<(&str, GridPoint)> =
            NUM_LEAVES
                .iter()
                .map(|&v| ("num_leaves", GridPoint { num_leaves: v, ..base_point }))
                .chain(
                    MAX_DEPTH
                        .iter()
                        .map(|&v| ("max_depth", GridPoint { max_depth: v, ..base_point })),
                )
                .chain(MIN_DATA_IN_LEAF.iter().map(|&v| {
                    ("min_data_in_leaf", GridPoint { min_data_in_leaf: v, ..base_point })
                }))
                .chain(
                    LEARNING_RATE
                        .iter()
                        .map(|&v| ("learning_rate", GridPoint { learning_rate: v, ..base_point })),
                )
                .chain(FEATURE_FRACTION.iter().map(|&v| {
                    ("feature_fraction", GridPoint { feature_fraction: v, ..base_point })
                }))
                .collect();
        assert_eq!(cells.len(), 15, "5 searched axes x 3 values");

        let mut failures = Vec::new();
        for (i, (axis, p)) in cells.iter().enumerate() {
            let params = params_for(&base(), p, 7, d.categorical).unwrap();
            let csv = csv_fit(&params, &format!("{i}"));
            let bin = bin_fit(&params, &format!("{i}"));
            let diff = non_echo_diff(&csv, &bin);
            eprintln!(
                "matrix {axis}[{p:?}]: {}",
                if diff.is_empty() { "identical".to_string() } else { format!("{diff:?}") }
            );
            if !diff.is_empty() {
                failures.push(format!("{axis} at {p:?}: {} differing lines", diff.len()));
            }
        }
        assert!(
            failures.is_empty(),
            "the bin-reuse path trains DIFFERENT models than per-fit CSV construction — a \
             searched axis is dataset-baked beyond what BinKey accounts for: {failures:?}"
        );

        // ...and the production `fit` itself agrees with the CSV reference at the f64-bit level,
        // so the parity proven on text above is the parity of the models `fit` actually returns.
        let ref_model = parse_model_text(&csv_fit(&p10, "ref")).unwrap();
        let got = l.fit(&d, &point(), 7).unwrap();
        for i in 0..d.n_rows {
            assert_eq!(
                ProbaModel::predict_proba(&got, d.row(i)).to_bits(),
                ProbaModel::predict_proba(&ref_model, d.row(i)).to_bits(),
                "row {i}: the production fit diverges from the CSV reference"
            );
        }

        drop(bin10);
        drop(bin50);
    }

    /// The one silent degradation the whole driven-process design exists to prevent, asserted
    /// against a REAL model: the asset column must be split on as a CATEGORY SET, not binned as a
    /// number. `#[ignore]`d and self-skipping, the same double gate as the venue live smokes.
    ///
    /// It lives here rather than in `crates/vike-ml/tests/gbdt_learner_smoke.rs` because reading a tree's
    /// `decision_type` means naming the model crate, and this is the only file that may.
    ///
    /// ```text
    /// cargo test -p vike-ml --lib gbdt_learner -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the pinned LightGBM binary — run `just lightgbm-build`"]
    fn the_categorical_column_is_split_on_as_a_category_set_and_not_as_a_number() {
        let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/lightgbm/lightgbm");
        if !binary.exists() {
            eprintln!(
                "SKIP: no LightGBM binary at {} — run `just lightgbm-build`",
                binary.display()
            );
            return;
        }
        let scratch =
            vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike_ml_gbdt_cat")
                .expect("scratch dir");
        let l = GbdtLearner::new(&binary, &scratch, base()).unwrap();

        // The label depends on the CATEGORY and on nothing monotone in it, so a numeric binning
        // of column 2 cannot separate {0,3} from {1,2} with one threshold.
        let n = 400usize;
        let cols = 3usize;
        let mut x = Vec::with_capacity(n * cols);
        let mut y = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64;
            let cat = (i % 4) as f64;
            x.extend_from_slice(&[(t * 0.37).sin(), (t * 0.11).cos(), cat]);
            y.push(if cat as u32 == 1 || cat as u32 == 2 { 1.0f32 } else { 0.0 });
        }
        let d = TrainData { x: &x, n_rows: n, n_cols: cols, y: &y, categorical: &[2] };
        let m = l.fit(&d, &point(), 7).expect("fit");
        let cat_splits = m
            .trees
            .iter()
            .flat_map(|t| t.decision_type.iter())
            .filter(|&&dt| crate::model::is_categorical(dt))
            .count();
        assert!(
            cat_splits > 0,
            "no node splits on a category set: the asset column was binned as a NUMBER, which is \
             silent, produces a plausible model, and is what every in-process binding got wrong"
        );
    }
}
