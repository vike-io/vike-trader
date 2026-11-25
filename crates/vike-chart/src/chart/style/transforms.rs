//! Chart-style transforms ported from vike `core/chart_transforms.py` — turn an OHLC
//! series into Renko / Range / Line-break / Kagi / Point&Figure renderings. Close-based,
//! deterministic. Non-time styles live on an ordinal axis (x = unit index); each unit
//! keeps the source bar's timestamp (`ot`) for approximate time labels.

use crate::chart::state::bar::ChartBar;

/// Params the six transform styles take, threaded through
/// [`crate::chart::state::ChartState::transformed_shared`] and
/// folded into its cache key. Only the field(s) a given style reads matter — `line_break_n` for
/// `LineBreak`, `point_figure_reversal` for `PointFigure`; the others derive their box/reversal size from
/// the bars themselves (`transforms::auto_box`) and take no param. The chart call site passes the
/// same constants it always has (`line_break(bars, 3)` / `point_and_figure(bars, 3)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ChartStyleTransformParams {
    /// `n` for `transforms::line_break` (call site: 3).
    pub line_break_n: usize,
    /// `reversal` for `transforms::point_and_figure` (call site: 3).
    pub point_figure_reversal: i32,
}

const MAX_UNITS: usize = 20_000;

fn mk(timestamp: i64, o: f64, h: f64, l: f64, c: f64) -> ChartBar {
    ChartBar { t: 0.0, ot: timestamp, o, h, l, c, v: 0.0 }
}

/// Re-stamp `t` to the ordinal index (non-time styles render on a 0..n axis).
pub fn reindex(mut v: Vec<ChartBar>) -> Vec<ChartBar> {
    for (i, b) in v.iter_mut().enumerate() {
        b.t = i as f64;
    }
    v
}

/// ATR-based default box/reversal size; falls back to 0.5% of the last price.
pub(crate) fn auto_box(bars: &[ChartBar]) -> f64 {
    if bars.is_empty() {
        return 0.0;
    }
    if bars.len() < 2 {
        let d = (bars[0].h - bars[0].l).abs();
        return if d > 0.0 { d } else { (bars[0].c.abs() * 0.005).max(1.0) };
    }
    let (mut sum, mut previous) = (0.0, bars[0].c);
    for b in &bars[1..] {
        sum += (b.h - b.l).max((b.h - previous).abs()).max((b.l - previous).abs());
        previous = b.c;
    }
    let atr = sum / (bars.len() - 1) as f64;
    if atr <= 0.0 { (bars[bars.len() - 1].c.abs() * 0.005).max(1.0) } else { atr }
}

pub fn renko(bars: &[ChartBar]) -> Vec<ChartBar> {
    let box_ = auto_box(bars);
    if bars.is_empty() || box_ <= 0.0 {
        return vec![];
    }
    let mut bricks = Vec::new();
    let mut last = bars[0].c;
    for b in bars {
        let c = b.c;
        while c >= last + box_ && bricks.len() < MAX_UNITS {
            let (o, cl) = (last, last + box_);
            bricks.push(mk(b.ot, o, cl, o, cl));
            last = cl;
        }
        while c <= last - box_ && bricks.len() < MAX_UNITS {
            let (o, cl) = (last, last - box_);
            bricks.push(mk(b.ot, o, o, cl, cl));
            last = cl;
        }
        if bricks.len() >= MAX_UNITS {
            break;
        }
    }
    bricks
}

pub fn range_bars(bars: &[ChartBar]) -> Vec<ChartBar> {
    let rng = auto_box(bars);
    if bars.is_empty() || rng <= 0.0 {
        return vec![];
    }
    let mut out = Vec::new();
    let mut anchor = bars[0].o;
    let (mut hi, mut lo) = (bars[0].h, bars[0].l);
    for b in bars {
        hi = hi.max(b.h);
        lo = lo.min(b.l);
        while hi - lo >= rng && out.len() < MAX_UNITS {
            let (o, cl) = if b.c >= anchor { (lo, lo + rng) } else { (hi, hi - rng) };
            out.push(mk(b.ot, o, o.max(cl), o.min(cl), cl));
            anchor = cl;
            hi = cl;
            lo = cl;
        }
        if out.len() >= MAX_UNITS {
            break;
        }
    }
    out
}

pub fn line_break(bars: &[ChartBar], n: usize) -> Vec<ChartBar> {
    if bars.len() < 2 {
        return vec![];
    }
    struct Blk {
        bottom: f64,
        top: f64,
        up: bool,
        timestamp: i64,
    }
    let mut blocks: Vec<Blk> = Vec::new();
    let mut previous_close = bars[0].c;
    for b in &bars[1..] {
        if blocks.len() >= MAX_UNITS {
            break;
        }
        let c = b.c;
        if blocks.is_empty() {
            if c > previous_close {
                blocks.push(Blk { bottom: previous_close, top: c, up: true, timestamp: b.ot });
            } else if c < previous_close {
                blocks.push(Blk { bottom: c, top: previous_close, up: false, timestamp: b.ot });
            }
            previous_close = c;
            continue;
        }
        let start = blocks.len().saturating_sub(n);
        let hi = blocks[start..].iter().map(|x| x.top).fold(f64::MIN, f64::max);
        let lo = blocks[start..].iter().map(|x| x.bottom).fold(f64::MAX, f64::min);
        let (last_top, last_bottom) = (blocks.last().unwrap().top, blocks.last().unwrap().bottom);
        if c > hi {
            blocks.push(Blk { bottom: last_top, top: c, up: true, timestamp: b.ot });
            previous_close = c;
        } else if c < lo {
            blocks.push(Blk { bottom: c, top: last_bottom, up: false, timestamp: b.ot });
            previous_close = c;
        }
    }
    blocks
        .iter()
        .map(|x| {
            let (o, cl) = if x.up { (x.bottom, x.top) } else { (x.top, x.bottom) };
            mk(x.timestamp, o, x.top, x.bottom, cl)
        })
        .collect()
}

pub struct ChartStyleKagi {
    pub prices: Vec<f64>,
    pub(crate) thick: Vec<bool>, // thick[i] = yang flag for segment i→i+1
}

pub fn kagi(bars: &[ChartBar]) -> ChartStyleKagi {
    let rev = auto_box(bars);
    if bars.is_empty() || rev <= 0.0 {
        return ChartStyleKagi { prices: vec![], thick: vec![] };
    }
    let mut prices = vec![bars[0].c];
    let mut direction = 0i32;
    for b in &bars[1..] {
        if prices.len() >= MAX_UNITS {
            break;
        }
        let c = b.c;
        let current = *prices.last().unwrap();
        if direction == 0 {
            if (c - current).abs() >= rev {
                direction = if c > current { 1 } else { -1 };
                prices.push(c);
            }
        } else if direction > 0 {
            if c > current {
                *prices.last_mut().unwrap() = c;
            } else if current - c >= rev {
                prices.push(c);
                direction = -1;
            }
        } else if c < current {
            *prices.last_mut().unwrap() = c;
        } else if c - current >= rev {
            prices.push(c);
            direction = 1;
        }
    }
    let mut thick = Vec::new();
    for i in 1..prices.len() {
        let prior = if i >= 2 { &prices[..i - 1] } else { &prices[..1] };
        if prices[i] > prices[i - 1] {
            let m = prior.iter().cloned().fold(f64::MIN, f64::max);
            thick.push(prior.is_empty() || prices[i] >= m);
        } else {
            let m = prior.iter().cloned().fold(f64::MAX, f64::min);
            thick.push(!prior.is_empty() && prices[i] <= m);
        }
    }
    ChartStyleKagi { prices, thick }
}

pub struct ChartStylePointFigureColumn {
    pub up: bool,
    pub bottom: f64,
    pub top: f64,
}

pub fn point_and_figure(
    bars: &[ChartBar],
    reversal: i32,
) -> (Vec<ChartStylePointFigureColumn>, f64) {
    let box_ = if bars.is_empty() {
        0.0
    } else {
        let rng = bars.iter().map(|b| b.h).fold(f64::MIN, f64::max)
            - bars.iter().map(|b| b.l).fold(f64::MAX, f64::min);
        auto_box(bars).max(rng / 50.0)
    };
    if bars.is_empty() || box_ <= 0.0 {
        return (vec![], 0.0);
    }
    let fl = |p: f64| (p / box_).floor() * box_;
    struct Col {
        up: bool,
        top: f64,
        bottom: f64,
    }
    let mut cols: Vec<Col> = Vec::new();
    let mut current: Option<Col> = None;
    for b in bars {
        if cols.len() >= MAX_UNITS {
            break;
        }
        let c = b.c;
        if current.is_none() {
            current = Some(Col { up: true, top: fl(c), bottom: fl(c) });
            continue;
        }
        let col = current.as_ref().unwrap();
        let (up, top, bottom) = (col.up, col.top, col.bottom);
        if up {
            if c >= top + box_ {
                current.as_mut().unwrap().top = fl(c);
            } else if c <= top - reversal as f64 * box_ {
                cols.push(current.take().unwrap());
                current = Some(Col { up: false, top: top - box_, bottom: fl(c) });
            }
        } else if c <= bottom - box_ {
            current.as_mut().unwrap().bottom = fl(c);
        } else if c >= bottom + reversal as f64 * box_ {
            cols.push(current.take().unwrap());
            current = Some(Col { up: true, top: fl(c), bottom: bottom + box_ });
        }
    }
    if let Some(c) = current {
        cols.push(c);
    }
    (
        cols.into_iter()
            .map(|c| ChartStylePointFigureColumn { up: c.up, bottom: c.bottom, top: c.top })
            .collect(),
        box_,
    )
}
