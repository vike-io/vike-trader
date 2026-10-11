use super::*;

fn gb(i: usize, c_shift: f64) -> Bar {
    let base = 100.0 + (i as f64 * 0.31).sin() * 3.0;
    Bar {
        t: i as f64,
        ot: 1_700_000_000_000 + i as i64 * 60_000,
        o: base,
        h: base + 1.0 + c_shift.abs(),
        l: base - 1.0,
        c: base + c_shift,
        v: 500.0 + i as f64,
    }
}

fn closed_bars(n: usize) -> Vec<Bar> {
    (0..n).map(|i| gb(i, 0.3)).collect()
}

fn last(a: &Active) -> f64 {
    *a.outputs[0].series.last().unwrap()
}

/// ⚠ **The memoized preview must equal the unmemoized one on every path** — the memo is a
/// speed change, and any divergence is a WRONG NUMBER DRAWN ON THE CHART with nothing to catch
/// it (no gate pins an overlay's values).
///
/// This drives the sequence an adversarial review constructed against the first version of the
/// memo, where the invalidation sat at `update`'s CALL SITE instead of inside `push_bar`:
///
///   update(closed[..40], Some(f))   -> memo := (fp(f), preview folded over 40 bars)
///   push_bar(b)                     -> `ind` advances to 41; memo SURVIVED
///   update(closed[..41], Some(f))   -> served the 40-bar preview
///
/// The last call reaches the memo because `push_bar` leaves `fp_first` untouched and sets
/// `fp_last` to the new bar, so `prefix_unchanged` is TRUE and neither clearing branch runs.
///
/// NON-VACUOUS: the assertion compares against a FRESHLY built `Active` folded over the same
/// 41 closed bars, so it fails on any stale value rather than agreeing with itself. `push_bar`
/// has no caller outside this file today, which is precisely why this was a latent trap rather
/// than a visible bug — and why it needs a test rather than a comment.
#[test]
fn an_external_push_bar_cannot_leave_a_stale_forming_preview() {
    let spec = get("sma").expect("sma is registered");
    let mut closed = closed_bars(40);
    let mut a = Active::new(7, spec, &closed);
    let f = gb(41, 0.25);

    a.update(&closed, Some(&f));

    let b = gb(40, 0.10);
    a.push_bar(&b);
    closed.push(b);

    a.update(&closed, Some(&f));
    let memoized = last(&a);

    let mut fresh = Active::new(8, spec, &closed);
    fresh.update(&closed, Some(&f));
    let expected = last(&fresh);

    assert_eq!(
        memoized.to_bits(),
        expected.to_bits(),
        "the forming preview after an external `push_bar` must be folded through the CURRENT \
             committed state ({expected}), not the previous one ({memoized}) — the memo outlived \
             the `ind` it was computed from"
    );
}

/// The memo must also be transparent on the ordinary paths, so the win cannot come from
/// serving a wrong value cheaply. Each arm compares against a fresh `Active`.
#[test]
fn the_forming_memo_is_transparent_on_every_ordinary_path() {
    let spec = get("sma").expect("sma is registered");
    let closed = closed_bars(40);
    let f1 = gb(41, 0.25);
    let f2 = gb(41, 0.90); // same index, different close -> different fingerprint

    let mut a = Active::new(1, spec, &closed);
    let mut fresh = Active::new(2, spec, &closed);

    // repeated identical forming bar (the HIT path), then a moved one (the MISS path),
    // then back — each must match a fresh fold.
    for f in [&f1, &f1, &f2, &f1] {
        a.update(&closed, Some(f));
        fresh.recompute_full(&closed);
        fresh.update(&closed, Some(f));
        assert_eq!(
            last(&a).to_bits(),
            last(&fresh).to_bits(),
            "memoized preview diverged from a fresh fold on forming close {}",
            f.c
        );
    }

    // ...and a param change with the SAME closed bars, which re-derives the SAME
    // `fp_first`/`fp_last` — the case a prefix-keyed memo would get wrong.
    a.set_params(vec![5.0], &closed);
    a.update(&closed, Some(&f1));
    let mut after = Active::new(3, spec, &closed);
    after.set_params(vec![5.0], &closed);
    after.update(&closed, Some(&f1));
    assert_eq!(
        last(&a).to_bits(),
        last(&after).to_bits(),
        "a param change must invalidate the memo: the closed-prefix fingerprints are identical \
             across it, so nothing else would"
    );
}
