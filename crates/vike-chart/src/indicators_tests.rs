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

/// Oracle: a fresh `Active` folded over closed+forming in one go — by the
/// vike-indicators parity gate this equals `vectorize` bit-for-bit.
fn oracle(spec: &'static IndicatorMeta, closed: &[Bar], forming: Option<&Bar>) -> Vec<Vec<f64>> {
    let mut all: Vec<Bar> = closed.to_vec();
    if let Some(f) = forming {
        all.push(*f);
    }
    let a = Active::new(1, spec, &all);
    a.outputs.iter().map(|o| o.series.clone()).collect()
}

fn assert_bits_eq(got: &[Vec<f64>], want: &[Vec<f64>], ctx: &str) {
    assert_eq!(got.len(), want.len(), "{ctx}: line count");
    for (li, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(g.len(), w.len(), "{ctx}: line {li} length");
        for (i, (a, b)) in g.iter().zip(w.iter()).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "{ctx}: line {li}[{i}] diverged: {a} vs {b}");
        }
    }
}

/// The live *forming* bar mutating in place must be previewed speculatively —
/// bit-identical outputs to a full rebuild, WITHOUT recommitting history
/// (`committed_len` stays at the closed count across every tick).
#[test]
fn forming_ticks_preview_without_committing() {
    for name in ["sma", "macd", "psar", "vwap", "obv", "bollinger"] {
        let spec = get(name).unwrap_or_else(|| panic!("{name} not registered"));
        let closed = closed_bars(40);
        let mut a = Active::new(7, spec, &closed);
        for k in 0..5 {
            let forming = gb(40, 0.1 * k as f64 - 0.2);
            a.update(&closed, Some(&forming));
            assert_eq!(a.committed_len, 40, "{name}: forming tick must not commit");
            let got: Vec<Vec<f64>> = a.outputs.iter().map(|o| o.series.clone()).collect();
            assert_bits_eq(&got, &oracle(spec, &closed, Some(&forming)), name);
        }
    }
}

/// Bar close: the old forming bar joins the closed prefix (one streamed
/// `on_bar`, no refold) and a new forming bar previews on top; dropping the
/// forming bar entirely truncates the preview.
#[test]
fn bar_close_promotes_and_new_forming_previews() {
    for name in ["sma", "macd"] {
        let spec = get(name).unwrap_or_else(|| panic!("{name} not registered"));
        let mut closed = closed_bars(40);
        let mut a = Active::new(7, spec, &closed);

        let forming = gb(40, 0.25);
        a.update(&closed, Some(&forming));
        closed.push(forming); // the bar closes exactly as last previewed

        let forming2 = gb(41, -0.1);
        a.update(&closed, Some(&forming2));
        assert_eq!(a.committed_len, 41, "{name}: closed bar must commit");
        let got: Vec<Vec<f64>> = a.outputs.iter().map(|o| o.series.clone()).collect();
        assert_bits_eq(&got, &oracle(spec, &closed, Some(&forming2)), name);

        // no forming bar → outputs shrink back to the committed prefix
        a.update(&closed, None);
        let got: Vec<Vec<f64>> = a.outputs.iter().map(|o| o.series.clone()).collect();
        assert_bits_eq(&got, &oracle(spec, &closed, None), name);
    }
}

/// Structural change (history reload / symbol swap shrinking the series)
/// still falls back to a full refold and stays parity-exact.
#[test]
fn structural_change_refolds() {
    let spec = get("sma").unwrap();
    let closed = closed_bars(40);
    let mut a = Active::new(7, spec, &closed);
    let shorter = closed_bars(25);
    a.update(&shorter, None);
    assert_eq!(a.committed_len, 25);
    let got: Vec<Vec<f64>> = a.outputs.iter().map(|o| o.series.clone()).collect();
    assert_bits_eq(&got, &oracle(spec, &shorter, None), "sma shrink");
}

/// Independent oracle for a *reconfigured* indicator: fold a fresh
/// `make_with(name, p)` instance straight through `on_bar` over `bars`,
/// one buffer per output line. This path does NOT touch `Active::set_params`,
/// so comparing against it proves `set_params` truly applies `p` — a
/// `reset()`-in-place reconfig would silently keep the old params and diverge.
fn fold_with_params(name: &str, p: &[f64], bars: &[Bar]) -> Vec<Vec<f64>> {
    let nlines = get(name).unwrap().outputs.len();
    let mut ind = vike_indicators::make_with(name, p).unwrap();
    let mut lines = vec![Vec::<f64>::new(); nlines];
    for b in bars {
        let vals = ind.on_bar(&to_model_bar(b));
        for (k, line) in lines.iter_mut().enumerate() {
            line.push(vals.get(k).copied().unwrap_or(f64::NAN));
        }
    }
    lines
}

/// `set_params` reconfig is bit-identical to a fresh `make_with(p)` fold, and
/// is PATH-INDEPENDENT: reconfiguring from some *other* params to `p` equals
/// going straight to `p` (proves the streaming instance is rebuilt via
/// `make_with`, not `reset()`-in-place which would silently keep the old
/// params). The reconfig parity invariant — the T8 gate.
#[test]
fn set_params_refold_matches_fresh_build() {
    let cases: &[(&str, &[f64])] = &[
        ("sma", &[7.0]),
        ("ema", &[50.0]),
        ("rsi", &[9.0]),
        ("bollinger", &[10.0, 1.5]),
        ("macd", &[5.0, 40.0, 3.0]),
    ];
    for (name, p) in cases {
        let spec = get(name).unwrap();
        let bars = closed_bars(60);
        let want = fold_with_params(name, p, &bars);

        // default → other → p : a path-independent reconfig lands on p's series.
        let mut a = Active::new(1, spec, &bars);
        a.set_params(vec![3.0; p.len()], &bars);
        a.set_params(p.to_vec(), &bars);
        let got: Vec<Vec<f64>> = a.outputs.iter().map(|o| o.series.clone()).collect();
        assert_bits_eq(&got, &want, name);
        assert_eq!(a.params, p.to_vec(), "{name}: params stored");
    }
}

/// The chart source selector's parity-safety contract: the DEFAULT
/// [`Source::Close`] fold is bit-identical to the pre-source path (identity
/// `model_bar`), and switching to a derived source (`Hlc3`) genuinely moves the
/// series. Proven for close-reading indicators (SMA/EMA/RSI). Guards the invariant
/// that keeps the vike-indicators parity gate untouched: Close is a no-op.
#[test]
fn source_close_is_identity_and_hlc3_diverges() {
    for name in ["sma", "ema", "rsi"] {
        let spec = get(name).unwrap();
        // c_shift != 0 so hlc3 = (h+l+c)/3 differs from c on every bar.
        let bars = closed_bars(60);

        // Default Active is Close-sourced → identical to the oracle full build.
        let base = Active::new(1, spec, &bars);
        let base_series: Vec<Vec<f64>> = base.outputs.iter().map(|o| o.series.clone()).collect();
        assert_bits_eq(&base_series, &oracle(spec, &bars, None), name);
        assert_eq!(base.source, Source::Close, "{name}: default source is Close");

        // Switch to hlc3 + refold → the series must actually change.
        let mut hlc3 = Active::new(1, spec, &bars);
        hlc3.source = Source::Hlc3;
        hlc3.recompute_full(&bars);
        let hlc3_series: Vec<Vec<f64>> = hlc3.outputs.iter().map(|o| o.series.clone()).collect();
        let changed = base_series
            .iter()
            .zip(&hlc3_series)
            .any(|(b, h)| b.iter().zip(h).any(|(x, y)| x.to_bits() != y.to_bits()));
        assert!(changed, "{name}: hlc3 source must move the series off close");
    }
}

/// [`Source::value`] computes the standard TradingView price series off a GUI bar.
#[test]
fn source_value_computes_tradingview_series() {
    let b = Bar { t: 0.0, ot: 0, o: 10.0, h: 20.0, l: 4.0, c: 16.0, v: 0.0 };
    assert_eq!(Source::Close.value(&b), 16.0);
    assert_eq!(Source::Open.value(&b), 10.0);
    assert_eq!(Source::High.value(&b), 20.0);
    assert_eq!(Source::Low.value(&b), 4.0);
    assert_eq!(Source::Hl2.value(&b), (20.0 + 4.0) / 2.0);
    assert_eq!(Source::Hlc3.value(&b), (20.0 + 4.0 + 16.0) / 3.0);
    assert_eq!(Source::Ohlc4.value(&b), (10.0 + 20.0 + 4.0 + 16.0) / 4.0);
}

/// A same-interval foreign series aligns 1:1 onto the primary timeline by `ot`,
/// so a study folded over the aligned bars is EXACTLY a study folded over the
/// foreign bars directly (only the x-index `t` is re-stamped to the primary's).
/// This is the core correctness claim of the foreign-source feature.
#[test]
fn align_source_one_to_one_when_ots_match() {
    // primary and foreign share the SAME open-times (same interval) but differ in OHLCV.
    let primary: Vec<Bar> = (0..30).map(|i| gb(i, 0.3)).collect();
    let foreign: Vec<Bar> = (0..30)
        .map(|i| {
            let mut b = gb(i, -0.4);
            b.o += 50.0;
            b.h += 50.0;
            b.l += 50.0;
            b.c += 50.0; // a clearly-different price series, same ot grid
            b
        })
        .collect();
    let aligned = align_source_bars(&primary, &foreign);
    assert_eq!(aligned.len(), primary.len());
    for (i, (a, f)) in aligned.iter().zip(&foreign).enumerate() {
        assert_eq!(a.ot, f.ot, "bar {i}: foreign ot carried");
        assert_eq!(a.c.to_bits(), f.c.to_bits(), "bar {i}: foreign close carried");
        assert_eq!(a.t.to_bits(), primary[i].t.to_bits(), "bar {i}: primary x-index kept");
    }
    // Folding SMA over the aligned bars == folding over the foreign bars (t is inert).
    let spec = get("sma").unwrap();
    let a_aligned = Active::new(1, spec, &aligned);
    let a_foreign = Active::new(2, spec, &foreign);
    assert_bits_eq(
        &a_aligned.outputs.iter().map(|o| o.series.clone()).collect::<Vec<_>>(),
        &a_foreign.outputs.iter().map(|o| o.series.clone()).collect::<Vec<_>>(),
        "sma over aligned == over foreign",
    );
}

/// A foreign GAP (a missing bar) forward-fills the previous foreign bar; a
/// primary bar BEFORE the first foreign bar clamps to foreign[0]; the result is
/// always exactly `primary.len()` long and carries the primary x-indices.
#[test]
fn align_source_forward_fills_gaps_and_clamps_lead_in() {
    let mk = |ot: i64, c: f64| Bar { t: 0.0, ot, o: c, h: c, l: c, c, v: 1.0 };
    // primary every 60s from t=0..=300 (6 bars)
    let primary: Vec<Bar> =
        (0..6).map(|i| Bar { t: i as f64, ..mk(i as i64 * 60_000, 10.0 + i as f64) }).collect();
    // foreign STARTS LATE (at 60s) and is MISSING the 120s bar → gap.
    let foreign =
        vec![mk(60_000, 100.0), /* gap at 120_000 */ mk(180_000, 300.0), mk(240_000, 400.0)];
    let aligned = align_source_bars(&primary, &foreign);
    assert_eq!(aligned.len(), 6);
    // bar0 (ot 0 < first foreign 60_000) clamps to foreign[0]
    assert_eq!(aligned[0].c, 100.0);
    assert_eq!(aligned[0].t, 0.0, "primary x-index kept even on the clamp");
    assert_eq!(aligned[1].c, 100.0); // ot 60_000 exact
    assert_eq!(aligned[2].c, 100.0); // ot 120_000 gap → carry 60_000's bar forward
    assert_eq!(aligned[3].c, 300.0); // ot 180_000 exact
    assert_eq!(aligned[4].c, 400.0); // ot 240_000 exact
    assert_eq!(aligned[5].c, 400.0); // ot 300_000 past the last foreign bar → carry last
}

/// Empty foreign ⇒ empty alignment (the study renders nothing until the foreign
/// feed syncs — `Active::update(&[], None)` yields empty series).
#[test]
fn align_source_empty_foreign_is_empty() {
    let primary: Vec<Bar> = (0..5).map(|i| gb(i, 0.1)).collect();
    assert!(align_source_bars(&primary, &[]).is_empty());
    assert!(align_source_bars(&[], &[]).is_empty());
}

/// A fresh `Active` defaults to the PRIMARY source (`source_symbol == None`),
/// the byte-identical no-op — the invariant that keeps every existing render and
/// the vike-indicators parity gate untouched.
#[test]
fn default_active_has_no_source_symbol() {
    let spec = get("sma").unwrap();
    let a = Active::new(1, spec, &closed_bars(10));
    assert_eq!(a.source_symbol, None, "default study computes off the primary symbol");
}

/// A level nobody has coloured is `None` — "the theme's level line" — not a seeded grey:
/// `Active::new` has no `egui::Context` to read a theme from, so the colour is resolved when the
/// level is painted. RSI's 30/50/70 pin that it is EVERY level, not the first.
#[test]
fn a_fresh_oscillators_levels_have_no_colour_chosen() {
    let a = Active::new(1, get("rsi").unwrap(), &closed_bars(10));
    assert_eq!(a.bands.len(), 3, "RSI's levels are 30/50/70");
    assert!(a.bands.iter().all(|b| b.color.is_none() && b.show), "{:?}", a.bands);
}

/// Per-line colour/width edits (the paint metadata a user tweaks in the T8
/// dialog) MUST survive a `set_params` refold — only `series`+`ind` change.
#[test]
fn set_params_preserves_line_color_and_width() {
    let spec = get("macd").unwrap();
    let bars = closed_bars(60);
    let mut a = Active::new(9, spec, &bars);
    let before: Vec<u64> = a.outputs[2].series.iter().map(|v| v.to_bits()).collect();

    // user edits on line 0: a non-PALETTE colour + a non-default width.
    let custom = egui::Color32::from_rgb(1, 2, 3);
    a.outputs[0].color = custom;
    a.outputs[0].width = 4.25;

    a.set_params(vec![5.0, 40.0, 3.0], &bars); // fast/slow/signal all changed

    assert_eq!(a.outputs[0].color, custom, "line colour must survive set_params");
    assert_eq!(a.outputs[0].width, 4.25, "line width must survive set_params");
    let after: Vec<u64> = a.outputs[2].series.iter().map(|v| v.to_bits()).collect();
    assert_ne!(before, after, "series must actually recompute on set_params");
}

// ---------------------------------------------------------------- user studies

/// A stand-in for a compiled user prototype: `close * scale`, where `scale` is the one
/// declared parameter. Deliberately NOT any built-in's formula, so a series that matches it
/// can only have come from the factory.
#[derive(Clone)]
struct Scaled {
    scale: f64,
    last: f64,
}

impl Indicator for Scaled {
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) -> Vec<f64> {
        self.last = bar.close * self.scale;
        vec![self.last]
    }
    fn vectorize(&self, bars: &[vike_marketdata::Bar]) -> Vec<Vec<f64>> {
        vec![bars.iter().map(|b| b.close * self.scale).collect()]
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last]
    }
    fn reset(&mut self) {
        self.last = f64::NAN;
    }
    fn name(&self) -> &str {
        "scaled"
    }
}

/// A user study named `name` whose single line is `close * scale` (default scale 2).
fn user_scaled(name: &str) -> &'static IndicatorMeta {
    IndicatorMeta::user(
        name,
        "Scaled Close",
        RenderKind::Overlay,
        vec![vike_indicators::ParamSpec {
            name: "scale",
            default: 2.0,
            min: 0.0,
            max: 10.0,
            step: 0.5,
        }],
        &|raw: &[f64]| {
            Box::new(Scaled { scale: raw.first().copied().unwrap_or(2.0), last: f64::NAN })
        },
    )
}

/// `Active` over a USER study plots the FACTORY's values.
///
/// Non-vacuous in the strongest available way: a user row's built-in `make` slot is
/// `vike_indicators`' `unbuilt`, which panics. Reverting `Active::new` to `(spec.make)()`
/// does not make this read a wrong number — it aborts the test binary. The `close * 2`
/// assertion on top proves the series is this prototype's and not some built-in's.
#[test]
fn an_active_over_a_user_study_folds_the_factorys_indicator() {
    let spec = user_scaled("scaled_active");
    let bars = closed_bars(12);
    let a = Active::new(1, spec, &bars);
    assert_eq!(a.outputs.len(), 1, "a user study is single-output");
    assert_eq!(a.outputs[0].name, "scaled_active", "the line is named for the indicator");
    assert_eq!(a.params, vec![2.0], "params seed from the declared default");
    for (i, b) in bars.iter().enumerate() {
        assert_eq!(
            a.outputs[0].series[i].to_bits(),
            (b.c * 2.0).to_bits(),
            "bar {i}: the series must be the user prototype's own values"
        );
    }
    assert!(a.is_overlay(), "this one declared RenderKind::Overlay");
}

/// `set_params` on a user study reaches the factory too, and the streaming/append fast path
/// keeps agreeing with a full rebuild.
///
/// Non-vacuous twice over: `set_params`' old `(self.spec.make_with)(&p)` is the panicking
/// placeholder for a user row, and a `set_params` that stored `p` without rebuilding would
/// leave the series at `close * 2` while `params` claimed 4.
#[test]
fn set_params_on_a_user_study_rebuilds_through_the_factory() {
    let spec = user_scaled("scaled_params");
    let mut closed = closed_bars(20);
    let mut a = Active::new(2, spec, &closed);
    a.set_params(vec![4.0], &closed);
    assert_eq!(a.params, vec![4.0]);
    for (i, b) in closed.iter().enumerate() {
        assert_eq!(a.outputs[0].series[i].to_bits(), (b.c * 4.0).to_bits(), "bar {i}");
    }
    // ...and the one-bar-closed fast path in `update` stays on the reconfigured instance.
    let next = gb(20, 0.4);
    closed.push(next);
    a.update(&closed, None);
    assert_eq!(a.outputs[0].series[20].to_bits(), (next.c * 4.0).to_bits());
}

/// The two registries are a UNION at [`get_any`] and disjoint everywhere else.
///
/// This is the only test in this module that installs, because `install_user_studies` is a
/// once-per-process `OnceLock` and the lib tests share one process.
///
/// Non-vacuous: before this seam `get` was the only lookup, so `get_any("scaled_installed")`
/// could not resolve at all — and the built-in half asserts the union did not disturb `get`.
#[test]
fn install_makes_a_user_study_resolvable_by_name_without_disturbing_the_builtins() {
    let mine = user_scaled("scaled_installed");
    install_user_studies(vec![mine]).expect("first install");

    assert!(get("scaled_installed").is_none(), "the built-in catalog stays closed");
    let found = get_any("scaled_installed").expect("resolvable through the union");
    assert!(found.is_user());
    assert_eq!(user_registry().len(), 1);

    // A built-in still resolves, and still reports itself as a built-in.
    let sma = get_any("sma").expect("built-ins resolve through the union too");
    assert!(!sma.is_user());
    assert!(std::ptr::eq(sma, get("sma").unwrap()), "and it is the SAME row `get` returns");

    // An unknown name is still `None` — the property persistence leans on (see `get_any`).
    assert!(get_any("no_such_indicator_anywhere").is_none());

    // A second install is refused, naming both counts.
    let err = install_user_studies(vec![user_scaled("scaled_second")]).unwrap_err();
    assert!(err.contains("already installed"), "{err}");
    assert_eq!(user_registry().len(), 1, "...and the first set is untouched");
}

/// A built-in smuggled into the user list, a user study that would shadow a built-in, and two
/// studies claiming one name are all refused BEFORE anything is stored.
///
/// Non-vacuous: without the shadow check `get_any` would resolve built-ins first and the
/// study would be pickable yet never plot its own values — a bug with no error message
/// anywhere. The duplicate arm is the same bug between two USER rows, and it fails on the
/// SECOND row specifically, so a check that only looked at `studies[0]` still passes the first
/// two asserts. Checked here rather than only through the install above because all three must
/// fail without consuming the `OnceLock` (every assert below can run after it is already set).
#[test]
fn install_refuses_a_builtin_row_a_shadowing_name_and_a_duplicate() {
    let builtin = get("sma").expect("sma is a built-in");
    let err = install_user_studies(vec![builtin]).unwrap_err();
    assert!(err.contains("built-in indicator"), "{err}");

    let shadow = user_scaled("sma");
    let err = install_user_studies(vec![shadow]).unwrap_err();
    assert!(err.contains("already the name of a built-in"), "{err}");

    // Two DISTINCT `IndicatorMeta`s (each its own leak) claiming one name — the shape a
    // non-Rhai installer could hand over, since only a filesystem stem is unique for free.
    let dupes = vec![user_scaled("scaled_dupe"), user_scaled("scaled_dupe")];
    let err = install_user_studies(dupes).unwrap_err();
    assert!(err.contains("claimed by two studies"), "{err}");
}
