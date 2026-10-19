//! A MEASUREMENT of how closely this crate agrees with the reference C library TA-Lib. It reports
//! and asserts nothing about divergence: vike is the reference implementation
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`), so a difference here is
//! a number to read, never a tolerance to widen.
//!
//! The reference values come from `scripts/talib_probe/` (`run.sh` builds TA-Lib from its release
//! source, runs `dump_reference.c`, then runs this test). This file does NOT regenerate the input
//! series: it reads `bars.csv` / `bars2.csv` from the dump directory, so both sides see the same
//! bits.
//!
//! `cargo test -p vike-indicators --release --test talib_probe -- --ignored --nocapture`
//! with `TALIB_DUMP_DIR` pointing at a dump directory (a TEST harness may read the environment;
//! this is not a setting).
//!
//! Dump file format (one per TA-Lib function and parameter set, listed in `index.txt`):
//! line 1 `FUNC p1 p2 ..`; line 2 `beg=<begIdx> n=<nbElement> outs=<K> bars=<N> int=<0|1> rc=<code>`;
//! then one `idx v0 .. v(K-1)` line per bar, `nan` where TA-Lib computed nothing.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use vike_indicators::{
    Category, PairMeta, coerce, compute, get, pair_make_with, pair_registry, registry,
};
use vike_marketdata::Bar;

// ---------------------------------------------------------------------------------------------
// reading the dump
// ---------------------------------------------------------------------------------------------

struct Dump {
    func: String,
    params: Vec<f64>,
    beg: usize,
    nb: usize,
    rc: i32,
    /// `outs[k][i]` = TA-Lib output `k` at bar `i`; NaN before `beg`.
    outs: Vec<Vec<f64>>,
}

fn read_bars(path: &Path) -> Vec<Bar> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            let num =
                |i: usize| f[i].parse::<f64>().unwrap_or_else(|e| panic!("bar field {i}: {e}"));
            Bar::ohlcv(f[0].parse().expect("ts"), num(1), num(2), num(3), num(4), num(5))
        })
        .collect()
}

fn parse_f(s: &str) -> f64 {
    if s == "nan" { f64::NAN } else { s.parse().unwrap_or(f64::NAN) }
}

fn kv(line: &str, key: &str) -> i64 {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn read_dump(path: &Path) -> Dump {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines();
    let mut head = lines.next().expect("header").split_whitespace();
    let func = head.next().expect("func").to_string();
    let params: Vec<f64> = head.map(parse_f).collect();
    let meta = lines.next().expect("meta line");
    let (beg, nb, k, bars, rc) =
        (kv(meta, "beg"), kv(meta, "n"), kv(meta, "outs"), kv(meta, "bars"), kv(meta, "rc"));
    let mut outs = vec![vec![f64::NAN; bars as usize]; k as usize];
    for l in lines {
        let mut t = l.split_whitespace();
        let Some(idx) = t.next().and_then(|s| s.parse::<usize>().ok()) else { continue };
        for (j, v) in t.enumerate() {
            if j < outs.len() && idx < outs[j].len() {
                outs[j][idx] = parse_f(v);
            }
        }
    }
    Dump { func, params, beg: beg as usize, nb: nb as usize, rc: rc as i32, outs }
}

// ---------------------------------------------------------------------------------------------
// the mapping: TA-Lib function -> registry indicator, parameters, output lines
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Target {
    /// `compute(name, raw, bars)`.
    Single(&'static str),
    /// `pair_make_with(name, raw).vectorize_pair(primary, benchmark)`. The dump feeds TA-Lib
    /// in0 = `bars`, in1 = `bars2`; `swap` = false makes `bars` (TA in0) the primary, `swap` = true
    /// makes `bars2` (TA in1) the primary.
    Pair(&'static str, bool),
}

struct Map {
    ta: &'static str,
    target: Target,
    /// TA-Lib optional inputs (as dumped) -> this crate's raw parameter slice, or why this
    /// parameter set has no counterpart.
    params: fn(&[f64]) -> Result<Vec<f64>, &'static str>,
    /// (TA-Lib output index, this crate's output name; `"*"` = the first output line).
    outs: &'static [(usize, &'static str)],
}

fn first(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(vec![p[0]])
}
fn all(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(p.to_vec())
}
fn none(_: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(vec![])
}
fn first_two(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(vec![p[0], p[1]])
}
fn bbands(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    if p[1] != p[2] {
        return Err("TA-Lib takes separate up/down deviations; vike has one `mult`");
    }
    if p[3] != 0.0 {
        return Err("TA-Lib's non-SMA middle band; vike's bollinger is SMA only");
    }
    Ok(vec![p[0], p[1]])
}
fn stoch(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    if p[2] != 0.0 || p[4] != 0.0 {
        return Err("non-SMA smoothing; vike's stochastic smooths with SMA only");
    }
    Ok(vec![p[0], p[1], p[3]])
}
fn stochf(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    if p[2] != 0.0 {
        return Err("non-SMA %D; vike's stochf smooths with SMA only");
    }
    Ok(vec![p[0], p[1]])
}
fn stochrsi(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    if p[3] != 0.0 {
        return Err("non-SMA %D; vike's stochrsi smooths with SMA only");
    }
    Ok(vec![p[0], p[1], p[2]])
}
fn ema_type(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    if p[2] != 1.0 {
        return Err(
            "vike apo/ppo are EMA-based (no MAType); TA-Lib's default MAType is SMA, which has no vike counterpart",
        );
    }
    Ok(vec![p[0], p[1]])
}
fn nbdev1(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    if p[1] != 1.0 {
        return Err("nbdev != 1 scales the output; vike has no deviation multiplier here");
    }
    Ok(vec![p[0]])
}
/// TA-Lib's KAMA takes only `timeperiod`; its fast = 2 and slow = 30 are fixed inside the library.
fn kama_fixed(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(vec![p[0], 2.0, 30.0])
}
/// TA-Lib's ACCBANDS takes only `timeperiod`; its factor 4 is fixed inside the library.
fn accbands_fixed(p: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(vec![p[0], 4.0])
}
/// TA-Lib's VWAP has no parameter and no anchor argument; the closest vike counterpart is
/// `anchored_vwap` anchored at the first bar (`anchor_ts` 0).
fn anchor_at_start(_: &[f64]) -> Result<Vec<f64>, &'static str> {
    Ok(vec![0.0])
}

/// TA-Lib CDL name, this crate's pattern name.
const PATTERNS: &[(&str, &str)] = &[
    ("CDL2CROWS", "two_crows"),
    ("CDL3BLACKCROWS", "three_black_crows"),
    ("CDL3INSIDE", "three_inside"),
    ("CDL3LINESTRIKE", "three_line_strike"),
    ("CDL3OUTSIDE", "three_outside"),
    ("CDL3STARSINSOUTH", "three_stars_in_south"),
    ("CDL3WHITESOLDIERS", "three_white_soldiers"),
    ("CDLABANDONEDBABY", "abandoned_baby"),
    ("CDLADVANCEBLOCK", "advance_block"),
    ("CDLBELTHOLD", "belt_hold"),
    ("CDLBREAKAWAY", "breakaway"),
    ("CDLCLOSINGMARUBOZU", "closing_marubozu"),
    ("CDLCONCEALBABYSWALL", "concealing_baby_swallow"),
    ("CDLCOUNTERATTACK", "counterattack"),
    ("CDLDARKCLOUDCOVER", "dark_cloud_cover"),
    ("CDLDOJI", "doji"),
    ("CDLDOJISTAR", "doji_star"),
    ("CDLDRAGONFLYDOJI", "dragonfly_doji"),
    ("CDLENGULFING", "engulfing"),
    ("CDLEVENINGDOJISTAR", "evening_doji_star"),
    ("CDLEVENINGSTAR", "evening_star"),
    ("CDLGAPSIDESIDEWHITE", "gap_side_side_white"),
    ("CDLGRAVESTONEDOJI", "gravestone_doji"),
    ("CDLHAMMER", "hammer"),
    ("CDLHANGINGMAN", "hanging_man"),
    ("CDLHARAMI", "harami"),
    ("CDLHARAMICROSS", "harami_cross"),
    ("CDLHIGHWAVE", "high_wave"),
    ("CDLHIKKAKE", "hikkake"),
    ("CDLHIKKAKEMOD", "hikkake_mod"),
    ("CDLHOMINGPIGEON", "homing_pigeon"),
    ("CDLIDENTICAL3CROWS", "identical_three_crows"),
    ("CDLINNECK", "in_neck"),
    ("CDLINVERTEDHAMMER", "inverted_hammer"),
    ("CDLKICKING", "kicking"),
    ("CDLKICKINGBYLENGTH", "kicking_by_length"),
    ("CDLLADDERBOTTOM", "ladder_bottom"),
    ("CDLLONGLEGGEDDOJI", "longlegged_doji"),
    ("CDLLONGLINE", "long_line"),
    ("CDLMARUBOZU", "marubozu"),
    ("CDLMATCHINGLOW", "matching_low"),
    ("CDLMATHOLD", "mat_hold"),
    ("CDLMORNINGDOJISTAR", "morning_doji_star"),
    ("CDLMORNINGSTAR", "morning_star"),
    ("CDLONNECK", "on_neck"),
    ("CDLPIERCING", "piercing"),
    ("CDLRICKSHAWMAN", "rickshaw_man"),
    ("CDLRISEFALL3METHODS", "rise_fall_three_methods"),
    ("CDLSEPARATINGLINES", "separating_lines"),
    ("CDLSHOOTINGSTAR", "shooting_star"),
    ("CDLSHORTLINE", "short_line"),
    ("CDLSPINNINGTOP", "spinning_top"),
    ("CDLSTALLEDPATTERN", "stalled_pattern"),
    ("CDLSTICKSANDWICH", "stick_sandwich"),
    ("CDLTAKURI", "takuri"),
    ("CDLTASUKIGAP", "tasuki_gap"),
    ("CDLTHRUSTING", "thrusting"),
    ("CDLTRISTAR", "tristar"),
    ("CDLUNIQUE3RIVER", "unique_three_river"),
    ("CDLUPSIDEGAP2CROWS", "upside_gap_two_crows"),
    ("CDLXSIDEGAP3METHODS", "xside_gap_three_methods"),
];

fn mk(
    ta: &'static str,
    target: Target,
    params: fn(&[f64]) -> Result<Vec<f64>, &'static str>,
    outs: &'static [(usize, &'static str)],
) -> Map {
    Map { ta, target, params, outs }
}

fn pair_meta(name: &str) -> Option<&'static PairMeta> {
    pair_registry().iter().find(|m| m.name == name)
}

fn maps() -> Vec<Map> {
    use Target::{Pair, Single};
    let m = mk;
    let mut v = vec![
        m("SMA", Single("sma"), first, &[(0, "*")]),
        m("EMA", Single("ema"), first, &[(0, "*")]),
        m("WMA", Single("wma"), first, &[(0, "*")]),
        m("DEMA", Single("dema"), first, &[(0, "*")]),
        m("TEMA", Single("tema"), first, &[(0, "*")]),
        m("TRIMA", Single("trima"), first, &[(0, "*")]),
        m("T3", Single("t3"), first_two, &[(0, "*")]),
        m("KAMA", Single("kama"), kama_fixed, &[(0, "*")]),
        m(
            "ACCBANDS",
            Single("acceleration_bands"),
            accbands_fixed,
            &[(0, "upper"), (1, "mid"), (2, "lower")],
        ),
        // TA-Lib 0.8.x functions found in its own function table with a twin among the newest vike
        // indicators
        m("QSTICK", Single("qstick"), first, &[(0, "*")]),
        m("CVI", Single("chaikin_volatility"), first_two, &[(0, "*")]),
        m(
            "HA",
            Single("heikin_ashi"),
            none,
            &[(0, "ha_open"), (1, "ha_high"), (2, "ha_low"), (3, "ha_close")],
        ),
        m("WAD", Single("williams_ad"), none, &[(0, "*")]),
        m("VWAP", Single("anchored_vwap"), anchor_at_start, &[(0, "*")]),
        m("BBANDS", Single("bollinger"), bbands, &[(0, "upper"), (1, "mid"), (2, "lower")]),
        m("MIDPOINT", Single("midpoint"), first, &[(0, "*")]),
        m("MIDPRICE", Single("midprice"), first, &[(0, "*")]),
        m("SAR", Single("psar"), all, &[(0, "*")]),
        m("RSI", Single("rsi"), first, &[(0, "*")]),
        m("MACD", Single("macd"), all, &[(0, "macd"), (1, "signal"), (2, "hist")]),
        m("ADX", Single("adx"), first, &[(0, "adx")]),
        m("PLUS_DI", Single("adx"), first, &[(0, "plus_di")]),
        m("MINUS_DI", Single("adx"), first, &[(0, "minus_di")]),
        m("ADXR", Single("adxr"), first, &[(0, "*")]),
        // The standalone single-line indicators added with `dx` / `plus_di` / `minus_di` / `plus_dm` /
        // `minus_dm`; the PLUS_DI / MINUS_DI rows above measure `adx`'s own lines.
        m("DX", Single("dx"), first, &[(0, "*")]),
        m("PLUS_DI", Single("plus_di"), first, &[(0, "*")]),
        m("MINUS_DI", Single("minus_di"), first, &[(0, "*")]),
        m("PLUS_DM", Single("plus_dm"), first, &[(0, "*")]),
        m("MINUS_DM", Single("minus_dm"), first, &[(0, "*")]),
        m("AROON", Single("aroon"), first, &[(0, "aroon_down"), (1, "aroon_up")]),
        m("AROONOSC", Single("aroonosc"), first, &[(0, "*")]),
        m("CCI", Single("cci"), first, &[(0, "*")]),
        m("CMO", Single("cmo"), first, &[(0, "*")]),
        m("MOM", Single("mom"), first, &[(0, "*")]),
        m("ROC", Single("roc"), first, &[(0, "*")]),
        m("ROCP", Single("rocp"), first, &[(0, "*")]),
        m("ROCR", Single("rocr"), first, &[(0, "*")]),
        m("ROCR100", Single("rocr100"), first, &[(0, "*")]),
        m("PPO", Single("ppo"), ema_type, &[(0, "*")]),
        m("APO", Single("apo"), ema_type, &[(0, "*")]),
        m("STOCH", Single("stochastic"), stoch, &[(0, "%K"), (1, "%D")]),
        m("STOCHF", Single("stochf"), stochf, &[(0, "%K"), (1, "%D")]),
        m("STOCHRSI", Single("stochrsi"), stochrsi, &[(0, "%K"), (1, "%D")]),
        m("TRIX", Single("trix"), first, &[(0, "*")]),
        m("ULTOSC", Single("ultosc"), all, &[(0, "*")]),
        m("WILLR", Single("williams"), first, &[(0, "*")]),
        m("BOP", Single("bop"), none, &[(0, "*")]),
        m("MFI", Single("mfi"), first, &[(0, "*")]),
        m("OBV", Single("obv"), none, &[(0, "*")]),
        m("AD", Single("ad"), none, &[(0, "*")]),
        m("ADOSC", Single("adosc"), all, &[(0, "*")]),
        m("TRANGE", Single("true_range"), none, &[(0, "*")]),
        m("ATR", Single("atr"), first, &[(0, "*")]),
        m("NATR", Single("natr"), first, &[(0, "*")]),
        m("LINEARREG", Single("linearreg"), first, &[(0, "*")]),
        m("LINEARREG_SLOPE", Single("linearreg_slope"), first, &[(0, "*")]),
        m("LINEARREG_ANGLE", Single("linearreg_angle"), first, &[(0, "*")]),
        m("LINEARREG_INTERCEPT", Single("linearreg_intercept"), first, &[(0, "*")]),
        m("TSF", Single("tsf"), first, &[(0, "*")]),
        m("STDDEV", Single("stddev"), nbdev1, &[(0, "*")]),
        m("VAR", Single("var"), nbdev1, &[(0, "*")]),
        m("AVGPRICE", Single("avgprice"), none, &[(0, "*")]),
        m("MEDPRICE", Single("medprice"), none, &[(0, "*")]),
        m("TYPPRICE", Single("typprice"), none, &[(0, "*")]),
        m("WCLPRICE", Single("wclprice"), none, &[(0, "*")]),
        // The pair indicators take (primary, benchmark); which of TA-Lib's two inputs is "the
        // asset" is not stated by this crate's mapping, so BOTH orders are measured.
        m("BETA", Pair("beta", false), first, &[(0, "*")]),
        m("BETA", Pair("beta", true), first, &[(0, "*")]),
        m("CORREL", Pair("correl", false), first, &[(0, "*")]),
        m("CORREL", Pair("correl", true), first, &[(0, "*")]),
    ];
    for &(ta, vike) in PATTERNS {
        v.push(m(ta, Target::Single(vike), none, &[(0, "*")]));
    }
    v
}

// ---------------------------------------------------------------------------------------------
// comparison
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Verdict {
    Exact,
    Ulp,
    Close,
    Differs,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Verdict::Exact => "EXACT",
            Verdict::Ulp => "ULP",
            Verdict::Close => "CLOSE",
            Verdict::Differs => "DIFFERS",
        }
    }
}

struct Stat {
    first_ours: Option<usize>,
    first_ta: Option<usize>,
    compared: usize,
    exact: usize,
    max_abs: f64,
    max_rel: f64,
    /// bars where exactly one side is NaN
    nan_mismatch: usize,
    verdict: Verdict,
    /// first bar whose relative difference exceeds 1e-12, with both values
    first_div: Option<(usize, f64, f64)>,
    worst: Option<(usize, f64, f64)>,
    /// max relative difference over the last 100 compared bars (does the gap decay?)
    tail_rel: f64,
    ours_nz: usize,
    ta_nz: usize,
    /// TA-Lib signals where vike is 0, vike signals where TA-Lib is 0, opposite signs, same sign
    /// but different magnitude (e.g. 100 vs 200)
    ta_only: usize,
    our_only: usize,
    sign_opp: usize,
    mag_diff: usize,
    /// min and max of (vike - TA-Lib) over the compared bars: equal means a constant offset
    diff_min: f64,
    diff_max: f64,
}

fn compare(ours: &[f64], ta: &[f64]) -> Stat {
    let n = ours.len().min(ta.len());
    let first_ours = ours.iter().position(|v| !v.is_nan());
    let first_ta = ta.iter().position(|v| !v.is_nan());
    let mut s = Stat {
        first_ours,
        first_ta,
        compared: 0,
        exact: 0,
        max_abs: 0.0,
        max_rel: 0.0,
        nan_mismatch: 0,
        verdict: Verdict::Exact,
        first_div: None,
        worst: None,
        tail_rel: 0.0,
        ours_nz: 0,
        ta_nz: 0,
        ta_only: 0,
        our_only: 0,
        sign_opp: 0,
        mag_diff: 0,
        diff_min: f64::INFINITY,
        diff_max: f64::NEG_INFINITY,
    };
    let mut rels: Vec<(usize, f64)> = Vec::new();
    for i in 0..n {
        let (a, b) = (ours[i], ta[i]);
        if a.is_nan() != b.is_nan() {
            s.nan_mismatch += 1;
            continue;
        }
        if a.is_nan() {
            continue;
        }
        s.compared += 1;
        if a != 0.0 {
            s.ours_nz += 1;
        }
        if b != 0.0 {
            s.ta_nz += 1;
        }
        if b != 0.0 && a == 0.0 {
            s.ta_only += 1;
        }
        if a != 0.0 && b == 0.0 {
            s.our_only += 1;
        }
        if a != 0.0 && b != 0.0 {
            if a.signum() != b.signum() {
                s.sign_opp += 1;
            } else if a != b {
                s.mag_diff += 1;
            }
        }
        s.diff_min = s.diff_min.min(a - b);
        s.diff_max = s.diff_max.max(a - b);
        if a.to_bits() == b.to_bits() {
            s.exact += 1;
        }
        let abs = (a - b).abs();
        let denom = a.abs().max(b.abs());
        let rel = if abs == 0.0 {
            0.0
        } else if denom == 0.0 || !abs.is_finite() {
            f64::INFINITY
        } else {
            abs / denom
        };
        if abs > s.max_abs || (abs.is_infinite() && !s.max_abs.is_infinite()) {
            s.max_abs = abs;
        }
        if rel > s.max_rel {
            s.max_rel = rel;
            s.worst = Some((i, a, b));
        }
        if rel > 1e-12 && s.first_div.is_none() {
            s.first_div = Some((i, a, b));
        }
        rels.push((i, rel));
    }
    s.tail_rel = rels.iter().rev().take(100).map(|&(_, r)| r).fold(0.0, f64::max);
    s.verdict = if s.compared == 0 {
        Verdict::Differs
    } else if s.exact == s.compared {
        Verdict::Exact
    } else if s.max_rel < 1e-12 {
        Verdict::Ulp
    } else if s.max_rel < 1e-6 {
        Verdict::Close
    } else {
        Verdict::Differs
    };
    s
}

fn fmt_params(p: &[f64]) -> String {
    p.iter().map(|v| format!("{v}")).collect::<Vec<_>>().join(",")
}

fn sci(v: f64) -> String {
    if v == 0.0 { "0".into() } else { format!("{v:.2e}") }
}

struct Line {
    func: String,
    label: String,
    out: String,
    stat: Stat,
    clamped: bool,
    pattern: bool,
}

fn run_one(
    map: &Map,
    shared: bool,
    d: &Dump,
    bars: &[Bar],
    bars2: &[Bar],
) -> Result<Vec<Line>, String> {
    let raw = (map.params)(&d.params).map_err(|r| r.to_string())?;
    let (name, swap, pair) = match map.target {
        Target::Single(n) => (n, false, false),
        Target::Pair(n, s) => (n, s, true),
    };
    let (names, lines): (Vec<String>, Vec<Vec<f64>>) = if pair {
        let meta = pair_meta(name).ok_or_else(|| format!("no pair indicator `{name}`"))?;
        let r = catch_unwind(AssertUnwindSafe(|| {
            let ind = pair_make_with(name, &raw).expect("pair");
            if swap { ind.vectorize_pair(bars2, bars) } else { ind.vectorize_pair(bars, bars2) }
        }))
        .map_err(|_| "vike side PANICKED".to_string())?;
        (meta.outputs.iter().map(|o| o.name.to_string()).collect(), r)
    } else {
        let r = catch_unwind(AssertUnwindSafe(|| compute(name, &raw, bars)))
            .map_err(|_| "vike side PANICKED".to_string())?
            .ok_or_else(|| format!("no registry indicator `{name}`"))?;
        (r.iter().map(|(n, _)| n.to_string()).collect(), r.into_iter().map(|(_, v)| v).collect())
    };
    let specs = if pair { pair_meta(name).map(|m| m.params) } else { get(name).map(|m| m.params) };
    let clamped = specs.is_some_and(|s| {
        let c = coerce(s, &raw);
        c.iter().zip(&raw).any(|(a, b)| a.to_bits() != b.to_bits())
    });
    let is_pattern = get(name).is_some_and(|m| m.category == Category::Pattern);
    let mut out = Vec::new();
    for &(ti, vname) in map.outs {
        let idx = if vname == "*" { Some(0) } else { names.iter().position(|n| n == vname) };
        let Some(vi) = idx else { return Err(format!("`{name}` has no output `{vname}`")) };
        let Some(ta) = d.outs.get(ti) else { return Err(format!("TA-Lib output {ti} missing")) };
        let mut stat = compare(&lines[vi], ta);
        // TA-Lib's own begIdx is authoritative for where IT starts
        stat.first_ta = if d.nb == 0 { None } else { Some(d.beg) };
        let label = format!("{}({})", d.func, fmt_params(&d.params));
        let outname = if map.outs.len() > 1 || vname != "*" {
            if vname == "*" { names[vi].clone() } else { vname.to_string() }
        } else {
            String::new()
        };
        let func = match map.target {
            Target::Pair(_, true) => format!("{} [vike primary = TA in1]", d.func),
            Target::Pair(_, false) => format!("{} [vike primary = TA in0]", d.func),
            // two registry indicators answer to this TA-Lib name (PLUS_DI / MINUS_DI: `adx`'s lines
            // and the standalone indicator): keep their verdicts apart
            Target::Single(n) if shared => format!("{} [vike {n}]", d.func),
            Target::Single(_) => d.func.clone(),
        };
        let label = match map.target {
            Target::Pair(_, true) => format!("{label}[swap]"),
            Target::Single(n) if shared => format!("{label}[{n}]"),
            _ => label,
        };
        out.push(Line { func, label, out: outname, stat, clamped, pattern: is_pattern });
    }
    Ok(out)
}

/// A pattern whose TA-Lib output AND vike output are both zero on every compared bar: an EXACT
/// verdict there proves nothing about the pattern.
fn vacuous(l: &Line) -> bool {
    l.pattern && l.stat.ta_nz == 0 && l.stat.ours_nz == 0
}

fn offset(s: &Stat) -> String {
    match (s.first_ours, s.first_ta) {
        (Some(o), Some(t)) => format!("{:+}", o as i64 - t as i64),
        (None, Some(_)) => "ours:none".into(),
        (Some(_), None) => "ta:none".into(),
        (None, None) => "n/a".into(),
    }
}

#[test]
#[ignore = "needs the dump pointer (scripts/talib_probe/run.sh builds TA-Lib and writes it)"]
fn talib_probe_report() {
    // The dump directory and the TA-Lib version label come from a pointer file `run.sh` writes
    // (`tmp/talib_probe_pointer`: directory on line 1, label on line 2), not from the process
    // environment: no setting of this repository is read from an environment variable.
    let pointer_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tmp/talib_probe_pointer");
    let Ok(pointer) = std::fs::read_to_string(pointer_path) else {
        println!("no tmp/talib_probe_pointer; run scripts/talib_probe/run.sh");
        return;
    };
    let mut pointer_lines = pointer.lines();
    let dir = pointer_lines.next().unwrap_or("").trim().to_string();
    let version_label = pointer_lines.next().unwrap_or("?").trim().to_string();
    let dir = PathBuf::from(dir);
    let bars = read_bars(&dir.join("bars.csv"));
    let bars2 = read_bars(&dir.join("bars2.csv"));
    let index = std::fs::read_to_string(dir.join("index.txt")).expect("index.txt");
    let maps = maps();
    println!(
        "TA-Lib reference {:?} ({}); {} bars; label {}",
        dir,
        version_label,
        bars.len(),
        version_label
    );

    let mut lines: Vec<Line> = Vec::new();
    let mut not_comparable: Vec<(String, String)> = Vec::new();
    let mut not_run: Vec<(String, String)> = Vec::new();
    for file in index.lines().filter(|l| !l.trim().is_empty()) {
        let d = read_dump(&dir.join(file));
        let label = format!("{}({})", d.func, fmt_params(&d.params));
        if d.rc != 0 || d.outs.is_empty() {
            not_run.push((label, format!("TA-Lib rc={}", d.rc)));
            continue;
        }
        let mine: Vec<&Map> = maps.iter().filter(|m| m.ta == d.func).collect();
        if mine.is_empty() {
            not_comparable.push((label, "no mapping to a registry indicator".into()));
            continue;
        }
        let shared = mine.iter().filter(|m| matches!(m.target, Target::Single(_))).count() > 1;
        for m in mine {
            match run_one(m, shared, &d, &bars, &bars2) {
                Ok(ls) => lines.extend(ls),
                Err(why) => not_comparable.push((label.clone(), why)),
            }
        }
    }

    // ---- detail: indicators ----
    println!("\n## Indicators (value comparison over bars where BOTH sides are non-NaN)");
    println!(
        "{:<44} {:<11} {:>7} {:>6} {:>6} {:>10} {:>10} {:>6} {:<8}",
        "function(params)",
        "output",
        "offset",
        "cmp",
        "exact",
        "max_abs",
        "max_rel",
        "nanmis",
        "verdict"
    );
    for l in lines.iter().filter(|l| !l.pattern) {
        let s = &l.stat;
        println!(
            "{:<44} {:<11} {:>7} {:>6} {:>6} {:>10} {:>10} {:>6} {:<8}{}",
            l.label,
            l.out,
            offset(s),
            s.compared,
            s.exact,
            sci(s.max_abs),
            sci(s.max_rel),
            s.nan_mismatch,
            s.verdict.name(),
            if l.clamped { "  (vike CLAMPED a param!)" } else { "" }
        );
    }

    // ---- detail: patterns ----
    println!(
        "\n## Candlestick patterns (values: TA-Lib +-100/+-200/0; agreement over bars both sides define)"
    );
    println!(
        "{:<28} {:>6} {:>5} {:>6} {:>6} {:>6} {:>8} {:>7} {:>8} {:>8} {:>8} {:<14}",
        "function",
        "offset",
        "cmp",
        "ta_nz",
        "our_nz",
        "exact",
        "agree %",
        "ta_only",
        "our_only",
        "sign_opp",
        "mag_diff",
        "verdict"
    );
    for l in lines.iter().filter(|l| l.pattern) {
        let s = &l.stat;
        let agree = if s.compared == 0 { 0.0 } else { 100.0 * s.exact as f64 / s.compared as f64 };
        println!(
            "{:<28} {:>6} {:>5} {:>6} {:>6} {:>6} {:>7.2}% {:>7} {:>8} {:>8} {:>8} {:<14}",
            l.func,
            offset(s),
            s.compared,
            s.ta_nz,
            s.ours_nz,
            s.exact,
            agree,
            s.ta_only,
            s.our_only,
            s.sign_opp,
            s.mag_diff,
            if vacuous(l) { "EXACT(vacuous)" } else { s.verdict.name() }
        );
    }

    // ---- verdicts by function (worst case over its parameter sets and outputs) ----
    let mut by_func: BTreeMap<String, Vec<&Line>> = BTreeMap::new();
    for l in &lines {
        by_func.entry(l.func.clone()).or_default().push(l);
    }
    let mut groups: BTreeMap<Verdict, Vec<String>> = BTreeMap::new();
    let mut vacuous_funcs: Vec<String> = Vec::new();
    for (f, ls) in &by_func {
        let worst = ls.iter().map(|l| l.stat.verdict).max().unwrap();
        let note = if worst == Verdict::Exact {
            String::new()
        } else {
            let bad: Vec<String> = ls
                .iter()
                .filter(|l| l.stat.verdict == worst)
                .map(|l| format!("{}{}{}", l.label, if l.out.is_empty() { "" } else { "." }, l.out))
                .collect();
            format!("  <- {}", bad.join("; "))
        };
        let warm: Vec<String> = ls
            .iter()
            .filter(|l| {
                !l.pattern && (l.stat.first_ours != l.stat.first_ta || l.stat.nan_mismatch > 0)
            })
            .map(|l| {
                format!(
                    "{}{}{} off {}",
                    l.label,
                    if l.out.is_empty() { "" } else { "." },
                    l.out,
                    offset(&l.stat)
                )
            })
            .collect();
        let warm_note = if warm.is_empty() {
            String::new()
        } else {
            format!("  [warm-up differs: {}]", warm.join("; "))
        };
        if ls.iter().all(|l| vacuous(l)) {
            vacuous_funcs.push(f.clone());
        } else {
            groups.entry(worst).or_default().push(format!("{f}{note}{warm_note}"));
        }
    }
    println!("\n## Verdict by function (worst over its parameter sets and outputs)");
    let cases: BTreeMap<Verdict, usize> = lines.iter().fold(BTreeMap::new(), |mut m, l| {
        *m.entry(l.stat.verdict).or_default() += 1;
        m
    });
    for v in [Verdict::Exact, Verdict::Ulp, Verdict::Close, Verdict::Differs] {
        let g = groups.get(&v).cloned().unwrap_or_default();
        println!(
            "\n{} -- {} functions ({} function/param/output cases)",
            v.name(),
            g.len(),
            cases.get(&v).copied().unwrap_or(0)
        );
        for s in g {
            println!("  {s}");
        }
        if v == Verdict::Exact {
            println!(
                "  -- VACUOUS exact ({}): no signal on EITHER side on this series, so the match says nothing: {}",
                vacuous_funcs.len(),
                vacuous_funcs.join(", ")
            );
        }
    }
    println!(
        "
(candlestick patterns: vike emits a value from bar 0, TA-Lib only from its own lookback; the offset column is vike_first - TA_first, only bars where both are defined are compared)"
    );

    // ---- DIFFERS detail for hypothesis building ----
    println!(
        "\n## DIFFERS detail (first bar beyond 1e-12 relative; worst bar; max relative diff over the last 100 compared bars)"
    );
    for l in lines.iter().filter(|l| l.stat.verdict == Verdict::Differs && !l.pattern) {
        let s = &l.stat;
        let f = |x: Option<(usize, f64, f64)>| match x {
            Some((i, a, b)) => format!("bar {i}: vike {a:.10e} / TA-Lib {b:.10e}"),
            None => "none".into(),
        };
        // Evidence for a hypothesis, never a verdict: what the numbers alone say about the shape
        // of the gap.
        let mut hints: Vec<String> = Vec::new();
        if s.compared > 0
            && s.diff_max - s.diff_min <= 1e-9 * s.diff_max.abs().max(s.diff_min.abs())
        {
            hints.push(format!("CONSTANT offset vike-TA = {:.10e}", s.diff_min));
        }
        if s.tail_rel < 1e-9 {
            hints.push("gap DECAYS to <1e-9 by the last 100 bars (seed/warm-up difference)".into());
        }
        if s.max_abs < 1e-9 {
            hints.push("max_abs < 1e-9: relative diff inflated by values at ~0".into());
        }
        println!(
            "  {}{}{}: first {} ; worst {} ; tail_rel {} ; diff range [{}, {}] ; offset {}{}",
            l.label,
            if l.out.is_empty() { "" } else { "." },
            l.out,
            f(s.first_div),
            f(s.worst),
            sci(s.tail_rel),
            sci(s.diff_min),
            sci(s.diff_max),
            offset(s),
            if hints.is_empty() {
                String::new()
            } else {
                format!(" ; HINT: {}", hints.join(" | "))
            }
        );
    }

    println!("\n## NOT COMPARABLE");
    for (label, why) in &not_comparable {
        println!("  {label}: {why}");
    }
    let mapped: std::collections::BTreeSet<&str> = PATTERNS.iter().map(|&(_, v)| v).collect();
    let vike_only: Vec<&str> = registry()
        .iter()
        .filter(|m| m.category == Category::Pattern && !mapped.contains(m.name))
        .map(|m| m.name)
        .collect();
    println!(
        "  vike patterns with no TA-Lib CDL function dumped ({}): {}",
        vike_only.len(),
        vike_only.join(", ")
    );
    if !not_run.is_empty() {
        println!("\n## TA-Lib side did NOT run");
        for (label, why) in &not_run {
            println!("  {label}: {why}");
        }
    }
    println!(
        "\n## Totals: {} function/param/output cases over {} functions; {} not comparable; {} TA-Lib calls failed",
        lines.len(),
        by_func.len(),
        not_comparable.len(),
        not_run.len()
    );
}
