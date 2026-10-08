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
    let conf = train_config(&p, Path::new("/tmp/d.csv"), Path::new("/tmp/m.txt")).render().unwrap();
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
        eprintln!("SKIP: no LightGBM binary at {} — run `just lightgbm-build`", binary.display());
        return;
    }
    let scratch =
        vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike_ml_gbdt_matrix")
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
    let p50 = params_for(&base(), &GridPoint { min_data_in_leaf: 50, ..point() }, 7, d.categorical)
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
            Trainer::train(&l.cli, &train_config(params, &csv, &dir.join("m.txt")), &dir).unwrap();
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
    let cells: Vec<(&str, GridPoint)> = NUM_LEAVES
        .iter()
        .map(|&v| ("num_leaves", GridPoint { num_leaves: v, ..base_point }))
        .chain(MAX_DEPTH.iter().map(|&v| ("max_depth", GridPoint { max_depth: v, ..base_point })))
        .chain(
            MIN_DATA_IN_LEAF
                .iter()
                .map(|&v| ("min_data_in_leaf", GridPoint { min_data_in_leaf: v, ..base_point })),
        )
        .chain(
            LEARNING_RATE
                .iter()
                .map(|&v| ("learning_rate", GridPoint { learning_rate: v, ..base_point })),
        )
        .chain(
            FEATURE_FRACTION
                .iter()
                .map(|&v| ("feature_fraction", GridPoint { feature_fraction: v, ..base_point })),
        )
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
        eprintln!("SKIP: no LightGBM binary at {} — run `just lightgbm-build`", binary.display());
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
