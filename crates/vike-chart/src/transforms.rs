//! Chart-style transforms ported from vike `core/chart_transforms.py` — turn an OHLC
//! series into Renko / Range / Line-break / Kagi / Point&Figure renderings. Close-based,
//! deterministic. Non-time styles live on an ordinal axis (x = unit index); each unit
//! keeps the source bar's timestamp (`ot`) for approximate time labels.

use crate::model::Bar;

const MAX_UNITS: usize = 20_000;

fn mk(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar { t: 0.0, ot: ts, o, h, l, c, v: 0.0 }
}

/// Re-stamp `t` to the ordinal index (non-time styles render on a 0..n axis).
pub fn reindex(mut v: Vec<Bar>) -> Vec<Bar> {
    for (i, b) in v.iter_mut().enumerate() {
        b.t = i as f64;
    }
    v
}

/// ATR-based default box/reversal size; falls back to 0.5% of the last price.
pub fn auto_box(bars: &[Bar]) -> f64 {
    if bars.is_empty() {
        return 0.0;
    }
    if bars.len() < 2 {
        let d = (bars[0].h - bars[0].l).abs();
        return if d > 0.0 { d } else { (bars[0].c.abs() * 0.005).max(1.0) };
    }
    let (mut sum, mut prev) = (0.0, bars[0].c);
    for b in &bars[1..] {
        sum += (b.h - b.l).max((b.h - prev).abs()).max((b.l - prev).abs());
        prev = b.c;
    }
    let atr = sum / (bars.len() - 1) as f64;
    if atr <= 0.0 {
        (bars[bars.len() - 1].c.abs() * 0.005).max(1.0)
    } else {
        atr
    }
}

pub fn renko(bars: &[Bar]) -> Vec<Bar> {
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

pub fn range_bars(bars: &[Bar]) -> Vec<Bar> {
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

pub fn line_break(bars: &[Bar], n: usize) -> Vec<Bar> {
    if bars.len() < 2 {
        return vec![];
    }
    struct Blk {
        bottom: f64,
        top: f64,
        up: bool,
        ts: i64,
    }
    let mut blocks: Vec<Blk> = Vec::new();
    let mut prev_close = bars[0].c;
    for b in &bars[1..] {
        if blocks.len() >= MAX_UNITS {
            break;
        }
        let c = b.c;
        if blocks.is_empty() {
            if c > prev_close {
                blocks.push(Blk { bottom: prev_close, top: c, up: true, ts: b.ot });
            } else if c < prev_close {
                blocks.push(Blk { bottom: c, top: prev_close, up: false, ts: b.ot });
            }
            prev_close = c;
            continue;
        }
        let start = blocks.len().saturating_sub(n);
        let hi = blocks[start..].iter().map(|x| x.top).fold(f64::MIN, f64::max);
        let lo = blocks[start..].iter().map(|x| x.bottom).fold(f64::MAX, f64::min);
        let (last_top, last_bottom) = (blocks.last().unwrap().top, blocks.last().unwrap().bottom);
        if c > hi {
            blocks.push(Blk { bottom: last_top, top: c, up: true, ts: b.ot });
            prev_close = c;
        } else if c < lo {
            blocks.push(Blk { bottom: c, top: last_bottom, up: false, ts: b.ot });
            prev_close = c;
        }
    }
    blocks
        .iter()
        .map(|x| {
            let (o, cl) = if x.up { (x.bottom, x.top) } else { (x.top, x.bottom) };
            mk(x.ts, o, x.top, x.bottom, cl)
        })
        .collect()
}

pub struct Kagi {
    pub prices: Vec<f64>,
    pub thick: Vec<bool>, // thick[i] = yang flag for segment i→i+1
}

pub fn kagi(bars: &[Bar]) -> Kagi {
    let rev = auto_box(bars);
    if bars.is_empty() || rev <= 0.0 {
        return Kagi { prices: vec![], thick: vec![] };
    }
    let mut prices = vec![bars[0].c];
    let mut direction = 0i32;
    for b in &bars[1..] {
        if prices.len() >= MAX_UNITS {
            break;
        }
        let c = b.c;
        let cur = *prices.last().unwrap();
        if direction == 0 {
            if (c - cur).abs() >= rev {
                direction = if c > cur { 1 } else { -1 };
                prices.push(c);
            }
        } else if direction > 0 {
            if c > cur {
                *prices.last_mut().unwrap() = c;
            } else if cur - c >= rev {
                prices.push(c);
                direction = -1;
            }
        } else if c < cur {
            *prices.last_mut().unwrap() = c;
        } else if c - cur >= rev {
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
    Kagi { prices, thick }
}

pub struct PnFColumn {
    pub up: bool,
    pub bottom: f64,
    pub top: f64,
}

pub fn point_and_figure(bars: &[Bar], reversal: i32) -> (Vec<PnFColumn>, f64) {
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
    let mut cur: Option<Col> = None;
    for b in bars {
        if cols.len() >= MAX_UNITS {
            break;
        }
        let c = b.c;
        if cur.is_none() {
            cur = Some(Col { up: true, top: fl(c), bottom: fl(c) });
            continue;
        }
        let col = cur.as_ref().unwrap();
        let (up, top, bottom) = (col.up, col.top, col.bottom);
        if up {
            if c >= top + box_ {
                cur.as_mut().unwrap().top = fl(c);
            } else if c <= top - reversal as f64 * box_ {
                cols.push(cur.take().unwrap());
                cur = Some(Col { up: false, top: top - box_, bottom: fl(c) });
            }
        } else if c <= bottom - box_ {
            cur.as_mut().unwrap().bottom = fl(c);
        } else if c >= bottom + reversal as f64 * box_ {
            cols.push(cur.take().unwrap());
            cur = Some(Col { up: true, top: fl(c), bottom: bottom + box_ });
        }
    }
    if let Some(c) = cur {
        cols.push(c);
    }
    (cols.into_iter().map(|c| PnFColumn { up: c.up, bottom: c.bottom, top: c.top }).collect(), box_)
}
