//! One bad tick must not leave a lasting mark: every indicator whose core carries a running sum of
//! squares (or cubes, or fourth powers) of a window is fed a smooth price series with ONE spike at bar
//! 300, and its output at the first bars after the spike has LEFT its window is compared with a
//! textbook reference computed here.
//!
//! The cores keep shifted running sums (`sum(x - shift)`, `sum((x - shift)^2)`, ...), update them as the
//! window slides and rebuild them from the window only every `ceil(512 / n)` laps. A spike of a thousand
//! times the price level puts a term of 1e6 level^2 through a sum whose true value is 1e-5 level^2, so
//! the rounding it leaves behind is bigger than the answer unless the core notices the sum has fallen
//! far under its recent peak and rebuilds at once. Whether each core notices is what is measured.
//!
//! * Series: level `L` (100 and 1e6), `close = L (1 + 0.02 sin(0.05 i) + 0.01 (u - 0.5))` with `u` a
//!   fixed uniform sequence, a bar around each close (open = the previous clean close, a wick of
//!   0.4% to 1.4% of `L`), and a second smooth series (half the level) for the pair indicators.
//! * Spike: at bar 300 the close is `factor * L` (1000 and 1e5) and the high is a wick above it; the
//!   open and the low stay normal; every other bar is untouched. The pair indicators are run twice, the
//!   spike in the primary and in the benchmark.
//! * Windows 5, 20 and 100. The spike has left every window `reach(n)` bars after it (`n + 2` for a
//!   close-based window: one return or true range reads the previous close; `2n + 2` for the TTM
//!   momentum). Checked: that bar and the three after it, all before the first scheduled rebuild
//!   (bar 519 for `n = 5`, 539 for 20, 699 for 100).
//! * Reference: the window folded from scratch, two passes, on the differences from the window mean
//!   (`devs`). Relative tolerance 1e-9 of the value, 1e-6 for the fourth-moment kurtosis. A few rows
//!   (`ttm_squeeze`, `rwi`, `acceleration_bands`, `half_life`, `hurst`) have no short textbook form and are
//!   compared with the same call on the series WITHOUT the spike: the spike is out of their reach, so
//!   the two must agree.
//! * A band is compared by its half width `(upper - mid) / mult` (the deviation itself), because the
//!   band as a price carries the level and would hide an error in the deviation.
//! * A control column checks the reference against the core on bars 240..244 (before the spike): a
//!   control that fails means the reference is wrong, not the core.
//!
//! The test prints the whole table (run it with `--nocapture`) and fails if any row is over its
//! tolerance, except the rows of a core marked `gap`: its fix was measured and rejected on speed, its rows
//! print as `KNOWN-GAP`, and the mark is a ratchet (a gap that has closed fails the test until the mark is
//! removed).

use vike_indicators::series;

const LEN: usize = 600;
const AT: usize = 300;
const TOL: f64 = 1.0e-9;
const TOL4: f64 = 1.0e-6;

type Lines = Vec<Vec<f64>>;

/// One summary line: name, worst error, hard failures, known-gap rows, rows, worst control, gap reason.
type Summary = (String, f64, usize, usize, usize, f64, Option<&'static str>);

// ------------------------------------------------------------------------------------------ data

#[derive(Clone)]
struct Data {
    o: Vec<f64>,
    h: Vec<f64>,
    l: Vec<f64>,
    c: Vec<f64>,
    b: Vec<f64>,
    v: Vec<f64>,
}

#[derive(Clone, Copy)]
enum Spike {
    None,
    Primary(f64),
    Bench(f64),
    Volume(f64),
}

fn build(level: f64, spike: Spike) -> Data {
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    // The volume has a stream of its own, so adding it moved no price of the earlier runs.
    let mut sv: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next_v = move || {
        sv ^= sv << 13;
        sv ^= sv >> 7;
        sv ^= sv << 17;
        (sv >> 11) as f64 / (1u64 << 53) as f64
    };
    let (mut o, mut h, mut l, mut c, mut b, mut v) =
        (vec![], vec![], vec![], vec![], vec![], vec![]);
    for i in 0..LEN {
        let x = i as f64;
        let ci = level * (1.0 + 0.02 * (x * 0.05).sin() + 0.01 * (next() - 0.5));
        let bi = 0.5 * level * (1.0 + 0.015 * (x * 0.07 + 1.0).sin() + 0.008 * (next() - 0.5));
        let open = if i == 0 { ci } else { c[i - 1] };
        let w = wick(level, i);
        o.push(open);
        h.push(open.max(ci) + w);
        l.push(open.min(ci) - w);
        c.push(ci);
        b.push(bi);
        v.push(1000.0 * (1.0 + 0.5 * (x * 0.03).sin() + 0.3 * (next_v() - 0.5)));
    }
    match spike {
        Spike::None => {}
        Spike::Primary(f) => {
            c[AT] = level * f;
            h[AT] = c[AT] + wick(level, AT);
        }
        Spike::Bench(f) => b[AT] = 0.5 * level * f,
        Spike::Volume(f) => v[AT] *= f,
    }
    Data { o, h, l, c, b, v }
}

fn wick(level: f64, i: usize) -> f64 {
    level * 0.004 * (1.0 + (i % 5) as f64 * 0.25)
}

// ------------------------------------------------------------------------- textbook references

/// The window's differences from its mean: the mean is taken on the differences from the first value.
fn devs(w: &[f64]) -> Vec<f64> {
    let b = w[0];
    let m = w.iter().map(|x| x - b).sum::<f64>() / w.len() as f64;
    w.iter().map(|x| (x - b) - m).collect()
}

fn mean_of(w: &[f64]) -> f64 {
    let b = w[0];
    b + w.iter().map(|x| x - b).sum::<f64>() / w.len() as f64
}

/// The k-th central moment, population (divided by n).
fn central(w: &[f64], k: i32) -> f64 {
    devs(w).iter().map(|d| d.powi(k)).sum::<f64>() / w.len() as f64
}

fn sd_pop(w: &[f64]) -> f64 {
    central(w, 2).sqrt()
}

fn sd_sample(w: &[f64]) -> f64 {
    (devs(w).iter().map(|d| d * d).sum::<f64>() / (w.len() - 1) as f64).sqrt()
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let (da, db) = (devs(a), devs(b));
    let sab: f64 = da.iter().zip(&db).map(|(x, y)| x * y).sum();
    let saa: f64 = da.iter().map(|x| x * x).sum();
    let sbb: f64 = db.iter().map(|x| x * x).sum();
    sab / (saa * sbb).sqrt()
}

/// The least-squares line through the window against 0..n, residuals folded directly.
struct Fit {
    n: f64,
    mean: f64,
    xbar: f64,
    slope: f64,
    ssr: f64,
    r2: f64,
}

fn fit(w: &[f64]) -> Fit {
    let n = w.len() as f64;
    let xbar = (n - 1.0) / 2.0;
    let dy = devs(w);
    let xs: Vec<f64> = (0..w.len()).map(|i| i as f64 - xbar).collect();
    let sxx: f64 = xs.iter().map(|x| x * x).sum();
    let sxy: f64 = xs.iter().zip(&dy).map(|(x, y)| x * y).sum();
    let syy: f64 = dy.iter().map(|y| y * y).sum();
    let slope = sxy / sxx;
    let ssr: f64 = xs.iter().zip(&dy).map(|(x, y)| (y - slope * x) * (y - slope * x)).sum();
    Fit { n, mean: mean_of(w), xbar, slope, ssr, r2: sxy * sxy / (sxx * syy) }
}

fn win(x: &[f64], i: usize, n: usize) -> Option<&[f64]> {
    if i + 1 >= n { Some(&x[i + 1 - n..=i]) } else { None }
}

/// `f(i)` at every bar, `nl` lines, `NaN` where `f` has no answer.
fn per_bar(len: usize, nl: usize, f: impl Fn(usize) -> Option<Vec<f64>>) -> Lines {
    let mut out = vec![vec![f64::NAN; len]; nl];
    for i in 0..len {
        if let Some(v) = f(i) {
            for (line, x) in out.iter_mut().zip(v) {
                line[i] = x;
            }
        }
    }
    out
}

fn log_returns(x: &[f64]) -> Vec<f64> {
    let mut r = vec![f64::NAN; x.len()];
    for i in 1..x.len() {
        r[i] = (x[i] / x[i - 1]).ln();
    }
    r
}

fn simple_returns(x: &[f64]) -> Vec<f64> {
    let mut r = vec![f64::NAN; x.len()];
    for i in 1..x.len() {
        r[i] = (x[i] - x[i - 1]) / x[i - 1];
    }
    r
}

/// Window of `n` returns ending at bar `i` (returns exist from bar 1).
fn rwin(r: &[f64], i: usize, n: usize) -> Option<&[f64]> {
    if i >= n { Some(&r[i + 1 - n..=i]) } else { None }
}

fn true_range(d: &Data, i: usize) -> f64 {
    if i == 0 {
        d.h[0] - d.l[0]
    } else {
        let pc = d.c[i - 1];
        (d.h[i] - d.l[i]).max((d.h[i] - pc).abs()).max((d.l[i] - pc).abs())
    }
}

fn band_lines(u: Vec<f64>, m: Vec<f64>, l: Vec<f64>, mult: f64) -> Lines {
    let hi = u.iter().zip(&m).map(|(u, m)| (u - m) / mult).collect();
    let lo = m.iter().zip(&l).map(|(m, l)| (m - l) / mult).collect();
    vec![m, hi, lo]
}

fn three(r: Result<[Vec<f64>; 3], vike_indicators::ParamError>) -> [Vec<f64>; 3] {
    r.unwrap()
}

// ------------------------------------------------------------------------------------------ cases

struct Case {
    name: &'static str,
    /// A fix for this core was REJECTED on speed (see the reason): its rows are expected over tolerance, are
    /// printed as `KNOWN-GAP` and do not fail the test; a gap that closes fails it, so the mark is removed.
    gap: Option<&'static str>,
    tol: f64,
    min_n: usize,
    pair: bool,
    /// The volume is spiked too (a second run with the spike in the volume column).
    vol: bool,
    /// Also run the spike of 1e7 times the level.
    wide: bool,
    reach: fn(usize) -> usize,
    lines: &'static [&'static str],
    run: fn(&Data, usize) -> Lines,
    /// The reference, from the spiked data and the clean data.
    want: fn(&Data, &Data, usize) -> Lines,
}

fn reach_close(n: usize) -> usize {
    n + 2
}

fn reach_double(n: usize) -> usize {
    2 * n + 2
}

fn f(n: usize) -> f64 {
    n as f64
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "stddev",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["sd"],
            run: |d, n| vec![series::stddev(&d.c, f(n)).unwrap()],
            want: |d, _, n| per_bar(LEN, 1, |i| win(&d.c, i, n).map(|w| vec![sd_pop(w)])),
        },
        Case {
            name: "bollinger",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["mid", "sd_hi", "sd_lo"],
            run: |d, n| {
                let [u, m, l] = three(series::bollinger(&d.c, f(n), 2.0));
                band_lines(u, m, l, 2.0)
            },
            want: |d, _, n| {
                per_bar(LEN, 3, |i| {
                    win(&d.c, i, n).map(|w| {
                        let s = sd_pop(w);
                        vec![mean_of(w), s, s]
                    })
                })
            },
        },
        Case {
            name: "bbands_pctb",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["pctb"],
            run: |d, n| vec![series::bbands_pctb(&d.c, f(n), 2.0).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| vec![((d.c[i] - mean_of(w)) / sd_pop(w) + 2.0) / 4.0])
                })
            },
        },
        Case {
            name: "bbands_width",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["width"],
            run: |d, n| vec![series::bbands_width(&d.c, f(n), 2.0).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| win(&d.c, i, n).map(|w| vec![4.0 * sd_pop(w) / mean_of(w)]))
            },
        },
        Case {
            name: "var",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["var"],
            run: |d, n| vec![series::var(&d.c, f(n)).unwrap()],
            want: |d, _, n| per_bar(LEN, 1, |i| win(&d.c, i, n).map(|w| vec![central(w, 2)])),
        },
        Case {
            name: "zscore",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["z"],
            run: |d, n| vec![series::zscore(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| vec![(d.c[i] - mean_of(w)) / sd_pop(w)])
                })
            },
        },
        Case {
            name: "skew",
            gap: None,
            tol: TOL,
            min_n: 3,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["skew"],
            run: |d, n| vec![series::skew(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let p = w.len() as f64;
                        let m2 = central(w, 2);
                        let g1 = central(w, 3) / (m2 * m2.sqrt());
                        vec![g1 * (p * (p - 1.0)).sqrt() / (p - 2.0)]
                    })
                })
            },
        },
        Case {
            name: "kurtosis",
            gap: None,
            tol: TOL4,
            min_n: 4,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["kurt"],
            run: |d, n| vec![series::kurtosis(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let p = w.len() as f64;
                        let m2 = central(w, 2);
                        let k = central(w, 4) / (m2 * m2);
                        let den = (p - 2.0) * (p - 3.0);
                        vec![(p + 1.0) * (p - 1.0) / den * k - 3.0 * (p - 1.0) * (p - 1.0) / den]
                    })
                })
            },
        },
        Case {
            name: "mad",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["mad"],
            run: |d, n| vec![series::mad(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n)
                        .map(|w| vec![devs(w).iter().map(|x| x.abs()).sum::<f64>() / f(w.len())])
                })
            },
        },
        Case {
            name: "linearreg",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["value"],
            run: |d, n| vec![series::linearreg(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let t = fit(w);
                        vec![t.mean + t.slope * t.xbar]
                    })
                })
            },
        },
        Case {
            name: "linearreg_slope",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["slope"],
            run: |d, n| vec![series::linearreg_slope(&d.c, f(n)).unwrap()],
            want: |d, _, n| per_bar(LEN, 1, |i| win(&d.c, i, n).map(|w| vec![fit(w).slope])),
        },
        Case {
            name: "linearreg_angle",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["angle"],
            run: |d, n| vec![series::linearreg_angle(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| win(&d.c, i, n).map(|w| vec![fit(w).slope.atan().to_degrees()]))
            },
        },
        Case {
            name: "linearreg_intercept",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["intercept"],
            run: |d, n| vec![series::linearreg_intercept(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let t = fit(w);
                        vec![t.mean - t.slope * t.xbar]
                    })
                })
            },
        },
        Case {
            name: "tsf",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["tsf"],
            run: |d, n| vec![series::tsf(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let t = fit(w);
                        vec![t.mean + t.slope * (t.xbar + 1.0)]
                    })
                })
            },
        },
        Case {
            name: "std_error",
            gap: None,
            tol: TOL,
            min_n: 3,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["se"],
            run: |d, n| vec![series::std_error(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let t = fit(w);
                        vec![(t.ssr / (t.n - 2.0)).sqrt() / t.n.sqrt()]
                    })
                })
            },
        },
        Case {
            name: "std_error_bands",
            gap: None,
            tol: TOL,
            min_n: 3,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["mid", "se_hi", "se_lo"],
            run: |d, n| {
                let [u, m, l] = three(series::std_error_bands(&d.c, f(n), 2.0));
                band_lines(u, m, l, 2.0)
            },
            want: |d, _, n| {
                per_bar(LEN, 3, |i| {
                    win(&d.c, i, n).map(|w| {
                        let t = fit(w);
                        let se = (t.ssr / (t.n - 2.0)).sqrt() / t.n.sqrt();
                        vec![t.mean + t.slope * t.xbar, se, se]
                    })
                })
            },
        },
        Case {
            name: "r_squared",
            gap: None,
            tol: TOL,
            min_n: 3,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["r2"],
            run: |d, n| vec![series::r_squared(&d.c, f(n)).unwrap()],
            want: |d, _, n| per_bar(LEN, 1, |i| win(&d.c, i, n).map(|w| vec![fit(w).r2])),
        },
        Case {
            name: "linreg_channel",
            gap: None,
            tol: TOL,
            min_n: 3,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["mid", "sd_hi", "sd_lo"],
            run: |d, n| {
                let [u, m, l] = three(series::linreg_channel(&d.c, f(n), 2.0));
                band_lines(u, m, l, 2.0)
            },
            want: |d, _, n| {
                per_bar(LEN, 3, |i| {
                    win(&d.c, i, n).map(|w| {
                        let t = fit(w);
                        let sd = (t.ssr / t.n).sqrt();
                        vec![t.mean + t.slope * t.xbar, sd, sd]
                    })
                })
            },
        },
        Case {
            name: "autocorr(lag 1)",
            gap: None,
            tol: TOL,
            min_n: 5,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["ac"],
            run: |d, n| vec![series::autocorr(&d.c, f(n), 1.0).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| vec![pearson(&w[..w.len() - 1], &w[1..])])
                })
            },
        },
        Case {
            name: "hvol",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["hvol"],
            run: |d, n| vec![series::hvol(&d.c, f(n), 252.0).unwrap()],
            want: |d, _, n| {
                let r = log_returns(&d.c);
                per_bar(LEN, 1, |i| {
                    rwin(&r, i, n).map(|w| vec![sd_pop(w) * 252.0_f64.sqrt() * 100.0])
                })
            },
        },
        Case {
            name: "relative_volatility",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["rvi"],
            run: |d, n| vec![series::relative_volatility(&d.c, f(n)).unwrap()],
            want: |d, _, n| vec![relative_volatility_ref(&d.c, n)],
        },
        Case {
            name: "ulcer",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["ulcer"],
            run: |d, n| vec![series::ulcer(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|w| {
                        let peak = w.iter().cloned().fold(f64::MIN, f64::max);
                        let q: f64 = w
                            .iter()
                            .map(|x| {
                                let dd = (x - peak) / peak * 100.0;
                                dd * dd
                            })
                            .sum();
                        vec![(q / f(w.len())).sqrt()]
                    })
                })
            },
        },
        Case {
            name: "chop",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["chop"],
            run: |d, n| vec![series::chop(&d.h, &d.l, &d.c, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    if i + 1 < n {
                        return None;
                    }
                    let s: f64 = (i + 1 - n..=i).map(|j| true_range(d, j)).sum();
                    let hh = d.h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
                    let ll = d.l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
                    Some(vec![100.0 * (s / (hh - ll)).log10() / f(n).log10()])
                })
            },
        },
        Case {
            name: "cci",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["cci"],
            run: |d, n| vec![series::cci(&d.h, &d.l, &d.c, f(n)).unwrap()],
            want: |d, _, n| {
                let tp: Vec<f64> = (0..LEN).map(|i| (d.h[i] + d.l[i] + d.c[i]) / 3.0).collect();
                per_bar(LEN, 1, |i| {
                    win(&tp, i, n).map(|w| {
                        let md = devs(w).iter().map(|x| x.abs()).sum::<f64>() / f(w.len());
                        vec![(tp[i] - mean_of(w)) / (0.015 * md)]
                    })
                })
            },
        },
        Case {
            name: "keltner(ema=atr=n)",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["mid", "atr_hi", "atr_lo"],
            run: |d, n| {
                let [u, m, l] = three(series::keltner(&d.h, &d.l, &d.c, f(n), f(n), 2.0));
                band_lines(u, m, l, 2.0)
            },
            want: |d, _, n| keltner_ref(d, n),
        },
        Case {
            name: "rvol_close_to_close",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["rvol"],
            run: |d, n| vec![series::rvol_close_to_close(&d.c, f(n)).unwrap()],
            want: |d, _, n| {
                let r = log_returns(&d.c);
                per_bar(LEN, 1, |i| rwin(&r, i, n).map(|w| vec![sd_sample(w)]))
            },
        },
        Case {
            name: "rvol_parkinson",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["rvol"],
            run: |d, n| vec![series::rvol_parkinson(&d.h, &d.l, f(n)).unwrap()],
            want: |d, _, n| {
                let t: Vec<f64> = (0..LEN).map(|i| (d.h[i] / d.l[i]).ln().powi(2)).collect();
                per_bar(LEN, 1, |i| {
                    win(&t, i, n)
                        .map(|w| vec![(w.iter().sum::<f64>() / f(n) / (4.0 * 2.0_f64.ln())).sqrt()])
                })
            },
        },
        Case {
            name: "rvol_garman_klass",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["rvol"],
            run: |d, n| vec![series::rvol_garman_klass(&d.o, &d.h, &d.l, &d.c, f(n)).unwrap()],
            want: |d, _, n| {
                let t: Vec<f64> = (0..LEN)
                    .map(|i| {
                        0.5 * (d.h[i] / d.l[i]).ln().powi(2)
                            - (2.0 * 2.0_f64.ln() - 1.0) * (d.c[i] / d.o[i]).ln().powi(2)
                    })
                    .collect();
                per_bar(LEN, 1, |i| {
                    win(&t, i, n).map(|w| vec![(w.iter().sum::<f64>() / f(n)).sqrt()])
                })
            },
        },
        Case {
            name: "rvol_rogers_satchell",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["rvol"],
            run: |d, n| vec![series::rvol_rogers_satchell(&d.o, &d.h, &d.l, &d.c, f(n)).unwrap()],
            want: |d, _, n| {
                let t = rs_terms(d);
                per_bar(LEN, 1, |i| {
                    win(&t, i, n).map(|w| vec![(w.iter().sum::<f64>() / f(n)).sqrt()])
                })
            },
        },
        Case {
            name: "rvol_yang_zhang",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["rvol"],
            run: |d, n| vec![series::rvol_yang_zhang(&d.o, &d.h, &d.l, &d.c, f(n)).unwrap()],
            want: |d, _, n| {
                let mut x = vec![f64::NAN; LEN];
                for (i, xi) in x.iter_mut().enumerate().skip(1) {
                    *xi = (d.o[i] / d.c[i - 1]).ln();
                }
                let y: Vec<f64> = (0..LEN).map(|i| (d.c[i] / d.o[i]).ln()).collect();
                let rs = rs_terms(d);
                let k = 0.34 / (1.34 + (f(n) + 1.0) / (f(n) - 1.0));
                per_bar(LEN, 1, |i| {
                    rwin(&x, i, n).map(|wx| {
                        let wy = &y[i + 1 - n..=i];
                        let wr = &rs[i + 1 - n..=i];
                        let vx = sd_sample(wx).powi(2);
                        let vy = sd_sample(wy).powi(2);
                        let mr = wr.iter().sum::<f64>() / f(n);
                        vec![(vx + k * vy + (1.0 - k) * mr).sqrt()]
                    })
                })
            },
        },
        Case {
            name: "spread_zscore",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: true,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["z"],
            run: |d, n| vec![series::spread_zscore(&d.c, &d.b, f(n), 1.0).unwrap()],
            want: |d, _, n| {
                let s: Vec<f64> = d.c.iter().zip(&d.b).map(|(a, b)| a - b).collect();
                per_bar(LEN, 1, |i| win(&s, i, n).map(|w| vec![(s[i] - mean_of(w)) / sd_pop(w)]))
            },
        },
        Case {
            name: "beta",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: true,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["beta"],
            run: |d, n| vec![series::beta(&d.c, &d.b, f(n)).unwrap()],
            want: |d, _, n| {
                let (ra, rb) = (simple_returns(&d.c), simple_returns(&d.b));
                per_bar(LEN, 1, |i| {
                    rwin(&ra, i, n).map(|wa| {
                        let wb = &rb[i + 1 - n..=i];
                        let (da, db) = (devs(wa), devs(wb));
                        let cov: f64 = da.iter().zip(&db).map(|(x, y)| x * y).sum();
                        let vb: f64 = db.iter().map(|y| y * y).sum();
                        vec![cov / vb]
                    })
                })
            },
        },
        Case {
            name: "correl",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: true,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["correl"],
            run: |d, n| vec![series::correl(&d.c, &d.b, f(n)).unwrap()],
            want: |d, _, n| {
                per_bar(LEN, 1, |i| {
                    win(&d.c, i, n).map(|wa| vec![pearson(wa, &d.b[i + 1 - n..=i])])
                })
            },
        },
        Case {
            name: "correl_log",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: true,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["correl_log"],
            run: |d, n| vec![series::correl_log(&d.c, &d.b, f(n)).unwrap()],
            want: |d, _, n| {
                let (ra, rb) = (log_returns(&d.c), log_returns(&d.b));
                per_bar(LEN, 1, |i| rwin(&ra, i, n).map(|wa| vec![pearson(wa, &rb[i + 1 - n..=i])]))
            },
        },
        // ---- compared with the same call on the series without the spike ------------------------
        Case {
            name: "ttm_squeeze [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_double,
            lines: &["momentum", "squeeze"],
            run: ttm_run,
            want: |_, clean, n| ttm_run(clean, n),
        },
        Case {
            name: "rwi [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["rwi_hi", "rwi_lo"],
            run: rwi_run,
            want: |_, clean, n| rwi_run(clean, n),
        },
        Case {
            name: "acceleration_bands [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["up", "mid", "lo"],
            run: accel_run,
            want: |_, clean, n| accel_run(clean, n),
        },
        Case {
            name: "half_life [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: true,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["half_life"],
            run: |d, n| vec![series::half_life(&d.c, &d.b, f(n), 1.0).unwrap()],
            want: |_, clean, n| vec![series::half_life(&clean.c, &clean.b, f(n), 1.0).unwrap()],
        },
        Case {
            name: "hurst [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 17,
            pair: false,
            vol: false,
            wide: false,
            reach: reach_close,
            lines: &["hurst"],
            run: |d, n| vec![series::hurst(&d.c, f(n)).unwrap()],
            want: |_, clean, n| vec![series::hurst(&clean.c, f(n)).unwrap()],
        },
        // ---- running sums of one stream or of two (products), compared with the call on the clean series
        Case {
            name: "sma [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["sma"],
            run: |d, n| vec![series::sma(&d.c, f(n)).unwrap()],
            want: |_, clean, n| vec![series::sma(&clean.c, f(n)).unwrap()],
        },
        Case {
            name: "wma [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["wma"],
            run: |d, n| vec![series::wma(&d.c, f(n)).unwrap()],
            want: |_, clean, n| vec![series::wma(&clean.c, f(n)).unwrap()],
        },
        Case {
            name: "trima [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["trima"],
            run: |d, n| vec![series::trima(&d.c, f(n)).unwrap()],
            want: |_, clean, n| vec![series::trima(&clean.c, f(n)).unwrap()],
        },
        Case {
            name: "alma [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["alma"],
            run: |d, n| vec![series::alma(&d.c, f(n), 0.85, 6.0).unwrap()],
            want: |_, clean, n| vec![series::alma(&clean.c, f(n), 0.85, 6.0).unwrap()],
        },
        Case {
            name: "awesome(n, 2n) [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_double,
            lines: &["ao"],
            run: |d, n| vec![series::awesome(&d.h, &d.l, f(n), f(2 * n)).unwrap()],
            want: |_, clean, n| vec![series::awesome(&clean.h, &clean.l, f(n), f(2 * n)).unwrap()],
        },
        Case {
            name: "qstick [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["qstick"],
            run: |d, n| vec![series::qstick(&d.o, &d.c, f(n)).unwrap()],
            want: |_, clean, n| vec![series::qstick(&clean.o, &clean.c, f(n)).unwrap()],
        },
        Case {
            name: "vwma [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: true,
            wide: true,
            reach: reach_close,
            lines: &["vwma"],
            run: |d, n| vec![series::vwma(&d.c, &d.v, f(n)).unwrap()],
            want: |_, clean, n| vec![series::vwma(&clean.c, &clean.v, f(n)).unwrap()],
        },
        Case {
            name: "cmf [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: true,
            wide: true,
            reach: reach_close,
            lines: &["cmf"],
            run: |d, n| vec![series::cmf(&d.h, &d.l, &d.c, &d.v, f(n)).unwrap()],
            want: |_, clean, n| {
                vec![series::cmf(&clean.h, &clean.l, &clean.c, &clean.v, f(n)).unwrap()]
            },
        },
        Case {
            name: "mfi [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: true,
            wide: true,
            reach: reach_close,
            lines: &["mfi"],
            run: |d, n| vec![series::mfi(&d.h, &d.l, &d.c, &d.v, f(n)).unwrap()],
            want: |_, clean, n| {
                vec![series::mfi(&clean.h, &clean.l, &clean.c, &clean.v, f(n)).unwrap()]
            },
        },
        Case {
            name: "eom [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: true,
            wide: true,
            reach: reach_close,
            lines: &["eom"],
            run: |d, n| vec![series::eom(&d.h, &d.l, &d.v, f(n)).unwrap()],
            want: |_, clean, n| vec![series::eom(&clean.h, &clean.l, &clean.v, f(n)).unwrap()],
        },
        Case {
            name: "vortex [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["vi_plus", "vi_minus"],
            run: vortex_run,
            want: |_, clean, n| vortex_run(clean, n),
        },
        Case {
            name: "ultosc(n,n,n) [clean ref]",
            gap: None,
            tol: TOL,
            min_n: 2,
            pair: false,
            vol: false,
            wide: true,
            reach: reach_close,
            lines: &["ultosc"],
            run: |d, n| vec![series::ultosc(&d.h, &d.l, &d.c, f(n), f(n), f(n)).unwrap()],
            want: |_, clean, n| {
                vec![series::ultosc(&clean.h, &clean.l, &clean.c, f(n), f(n), f(n)).unwrap()]
            },
        },
    ]
}

fn ttm_run(d: &Data, n: usize) -> Lines {
    let [m, s] = series::ttm_squeeze(&d.h, &d.l, &d.c, f(n), 2.0, 1.5).unwrap();
    vec![m, s]
}

fn vortex_run(d: &Data, n: usize) -> Lines {
    let [a, b] = series::vortex(&d.h, &d.l, &d.c, f(n)).unwrap();
    vec![a, b]
}

fn rwi_run(d: &Data, n: usize) -> Lines {
    let [a, b] = series::rwi(&d.h, &d.l, &d.c, f(n)).unwrap();
    vec![a, b]
}

fn accel_run(d: &Data, n: usize) -> Lines {
    let [a, b, c] = three(series::acceleration_bands(&d.h, &d.l, &d.c, f(n), 4.0));
    vec![a, b, c]
}

/// `ln(H/C) ln(H/O) + ln(L/C) ln(L/O)` of every bar.
fn rs_terms(d: &Data) -> Vec<f64> {
    (0..LEN)
        .map(|i| {
            let (o, h, l, c) = (d.o[i], d.h[i], d.l[i], d.c[i]);
            (h / c).ln() * (h / o).ln() + (l / c).ln() * (l / o).ln()
        })
        .collect()
}

/// The relative volatility index from scratch: the population standard deviation of every window
/// (two passes), split by the direction of the close, two EMAs seeded with the mean of their first `n`
/// values, the up side as a percentage of the two.
fn relative_volatility_ref(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    let alpha = 2.0 / (f(n) + 1.0);
    let (mut fed, mut su, mut sd_, mut eu, mut ed) = (0usize, 0.0, 0.0, 0.0, 0.0);
    for i in 1..c.len() {
        let Some(w) = win(c, i, n) else { continue };
        let s = sd_pop(w);
        let up = if c[i] > c[i - 1] { s } else { 0.0 };
        let down = if c[i] < c[i - 1] { s } else { 0.0 };
        if fed < n {
            fed += 1;
            su += up;
            sd_ += down;
            if fed < n {
                continue;
            }
            eu = su / f(n);
            ed = sd_ / f(n);
        } else {
            eu = alpha * up + (1.0 - alpha) * eu;
            ed = alpha * down + (1.0 - alpha) * ed;
        }
        let den = eu + ed;
        out[i] = if den != 0.0 { 100.0 * eu / den } else { 50.0 };
    }
    out
}

/// Keltner from scratch: the EMA seeded with the mean of the first `n` closes, the Wilder average of
/// the true range seeded with the mean of the first `n`; lines `[mid, atr, atr]`.
fn keltner_ref(d: &Data, n: usize) -> Lines {
    let (mut mid, mut band) = (Vec::with_capacity(LEN), Vec::with_capacity(LEN));
    let alpha = 2.0 / (f(n) + 1.0);
    let (mut ema, mut atr) = (0.0, 0.0);
    for i in 0..LEN {
        let tr = true_range(d, i);
        if i + 1 < n {
            mid.push(f64::NAN);
            band.push(f64::NAN);
            continue;
        }
        if i + 1 == n {
            ema = d.c[..n].iter().sum::<f64>() / f(n);
            atr = (0..n).map(|j| true_range(d, j)).sum::<f64>() / f(n);
        } else {
            ema = alpha * d.c[i] + (1.0 - alpha) * ema;
            atr = atr * (f(n) - 1.0) / f(n) + tr / f(n);
        }
        mid.push(ema);
        band.push(atr);
    }
    vec![mid, band.clone(), band]
}

// ------------------------------------------------------------------------------------- comparing

fn rel(got: f64, want: f64) -> f64 {
    if got.is_nan() && want.is_nan() {
        return 0.0;
    }
    if got.is_nan() || want.is_nan() {
        return f64::INFINITY;
    }
    if got == want {
        return 0.0;
    }
    (got - want).abs() / want.abs().max(f64::MIN_POSITIVE)
}

/// The worst relative error over the bars, and the line it was on.
fn worst(got: &Lines, want: &Lines, bars: std::ops::Range<usize>) -> (f64, usize) {
    let (mut w, mut at) = (0.0_f64, 0usize);
    for (li, (g, h)) in got.iter().zip(want).enumerate() {
        for i in bars.clone() {
            let e = rel(g[i], h[i]);
            if e > w || (e.is_nan() && !w.is_nan()) {
                w = e;
                at = li;
            }
        }
    }
    (w, at)
}

#[test]
fn a_bad_tick_leaves_no_lasting_mark() {
    let cases = cases();
    let mut summary: Vec<Summary> = Vec::new();
    println!(
        "{:<32} {:>3} {:>8} {:>8} {:<6} {:<9} {:>10} {:>10}  verdict",
        "indicator", "n", "level", "spike", "where", "line", "worst rel", "control"
    );
    for case in &cases {
        let (mut max_err, mut rows, mut failed, mut gapped, mut ctl_max) =
            (0.0_f64, 0usize, 0usize, 0usize, 0.0_f64);
        for &level in &[100.0, 1.0e6] {
            let factors: &[f64] =
                if case.wide { &[1000.0, 1.0e5, 1.0e7] } else { &[1000.0, 1.0e5] };
            for &factor in factors {
                let targets: &[u8] = if case.pair {
                    &[0, 1]
                } else if case.vol {
                    &[0, 2]
                } else {
                    &[0]
                };
                for &target in targets {
                    for &n in &[5usize, 20, 100] {
                        if n < case.min_n {
                            continue;
                        }
                        let sp = match target {
                            0 => Spike::Primary(factor),
                            1 => Spike::Bench(factor),
                            _ => Spike::Volume(factor),
                        };
                        let spiked = build(level, sp);
                        let clean = build(level, Spike::None);
                        let got = (case.run)(&spiked, n);
                        let want = (case.want)(&spiked, &clean, n);
                        let first = AT + (case.reach)(n);
                        let (err, line) = worst(&got, &want, first..first + 4);
                        let (ctl, _) = worst(&got, &want, 240..244);
                        let verdict = if ctl > case.tol || ctl.is_nan() {
                            "CONTROL-FAIL (reference or definition)"
                        } else if err > case.tol || err.is_nan() {
                            if case.gap.is_some() { "KNOWN-GAP" } else { "FAIL" }
                        } else {
                            "PASS"
                        };
                        println!(
                            "{:<32} {:>3} {:>8.0e} {:>8.0e} {:<6} {:<9} {:>10.3e} {:>10.3e}  {}",
                            case.name,
                            n,
                            level,
                            factor,
                            ["prim", "bench", "vol"][target as usize],
                            case.lines[line],
                            err,
                            ctl,
                            verdict
                        );
                        rows += 1;
                        if verdict == "KNOWN-GAP" {
                            gapped += 1;
                        } else if verdict != "PASS" {
                            failed += 1;
                        }
                        if err > max_err || err.is_nan() {
                            max_err = err;
                        }
                        if ctl > ctl_max || ctl.is_nan() {
                            ctl_max = ctl;
                        }
                    }
                }
            }
        }
        summary.push((case.name.to_string(), max_err, failed, gapped, rows, ctl_max, case.gap));
    }
    println!("\n==== summary (tolerance 1e-9, kurtosis 1e-6) ====");
    println!(
        "{:<32} {:>12} {:>12} {:>10}  verdict",
        "indicator", "worst rel", "worst ctl", "over tol"
    );
    let (mut bad, mut stale) = (0usize, Vec::new());
    for (name, err, failed, gapped, rows, ctl, gap) in &summary {
        let verdict = match (*failed, *gapped, gap) {
            (0, 0, None) => "PASS".to_string(),
            (0, 0, Some(_)) => {
                stale.push(name.clone());
                "PASS (the known gap has closed: remove its mark)".to_string()
            }
            (0, _, Some(why)) => format!("KNOWN-GAP: {why}"),
            _ => "FAIL".to_string(),
        };
        println!(
            "{:<32} {:>12.3e} {:>12.3e} {:>4}/{:<4}  {}",
            name,
            err,
            ctl,
            failed + gapped,
            rows,
            verdict
        );
        bad += failed;
    }
    assert_eq!(bad, 0, "{bad} rows are over their tolerance (see the table above)");
    assert!(stale.is_empty(), "known gaps that have closed, remove their marks: {stale:?}");
}
