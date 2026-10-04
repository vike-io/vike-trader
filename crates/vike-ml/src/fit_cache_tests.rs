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
    for (i, v) in [-0.0f64, f64::MIN_POSITIVE / 2.0, 1.0 / 3.0, -1.5e308].into_iter().enumerate() {
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
        ("non-finite bits", format!("{MAGIC}\n{:016x}\n", f64::INFINITY.to_bits()).into_bytes()),
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
