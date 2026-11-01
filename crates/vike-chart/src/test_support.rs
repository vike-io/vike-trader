//! Fixtures the `headless` integration binary, vike-app-core's workspace tests and this crate's own
//! unit tests share, compiled ONCE here: as `crate::test_support` under `cfg(test)` and as
//! `vike_chart::test_support` behind `test-support`, which only dev-dependency edges turn on (this
//! crate's own and vike-app-core's).
//!
//! ⚠ Nothing here may call a transcendental `f64` METHOD (`sin`, `cos`, `log10`, …). A
//! feature-gated module is PRODUCTION to `crates/vike-chart/tests/libm_platform_probe.rs`'s
//! `production_code_calls_libm_not_the_platform`: the shared walk (`vike_model::libm_walk`,
//! decision 0074) knows a test module only by `#[cfg(test)]`. The `sin`/`cos` series therefore stay
//! in `crate::model::testutil` (unit tests) and the headless binary's `common`.

use vike_indicators::{Indicator, IndicatorMeta, ParamSpec, RenderKind};

use crate::model::{Bar, ChartState};
use crate::tz::DisplayTz;

/// A perfectly UNIFORM series: `n` one-minute bars, every one o 1, h 2, l 0.5, c 1.5, v 10. Because
/// all bars are identical, a hovered readout is `[o, h, l, c]` whichever bar the pointer resolves
/// to, so an assertion on it does not depend on the exact screen→plot-coordinate mapping.
pub fn flat_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            t: i as f64,
            ot: 1_700_000_000_000 + i as i64 * 60_000,
            o: 1.0,
            h: 2.0,
            l: 0.5,
            c: 1.5,
            v: 10.0,
        })
        .collect()
}

/// Build a fully-closed [`ChartState`] from render bars (mirrors `benches/render.rs`): pin the
/// display timezone, set `bars` + `closed_len`, then `refresh_caches()` to seed the y-extent /
/// hour-mark / grid-step caches `draw` reads.
///
/// ⚠ **The `set_tz` call is load-bearing and is the reason this helper exists rather than a plain
/// struct literal.** `ChartState`'s tz defaults to [`DisplayTz::Local`] — `chrono::Local`, i.e. the
/// MACHINE's zone — and it feeds `crate::model::hour_mark_indices` / `day_mark_indices` and every
/// x-axis tick label. Left at the default, a rendered frame differs between two machines in both
/// the label TEXT and the grid-line POSITIONS: not a subtle sub-pixel effect, a whole grid line
/// moving. That makes `tessellation_goldens.rs` unreproducible, and it makes every render test in
/// this crate quietly machine-dependent. Pinning to UTC here fixes both at the source. Measured:
/// running the golden suite under `TZ=Asia/Kolkata` against goldens generated under `TZ=UTC`
/// reddens without this line, and passes with it.
pub fn make_state(bars: Vec<Bar>) -> ChartState {
    let mut s = ChartState::default();
    s.set_tz(DisplayTz::Utc);
    let n = bars.len();
    s.bars = bars;
    s.closed_len = n;
    s.refresh_caches();
    s
}

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

/// A user study named `name`, "Scaled Close", whose single overlay line is `close * scale` (one
/// declared knob, `scale`: default 2, 0 to 10 in steps of 0.5).
pub fn user_scaled(name: &str) -> &'static IndicatorMeta {
    IndicatorMeta::user(
        name,
        "Scaled Close",
        RenderKind::Overlay,
        vec![ParamSpec { name: "scale", default: 2.0, min: 0.0, max: 10.0, step: 0.5 }],
        &|raw: &[f64]| {
            Box::new(Scaled { scale: raw.first().copied().unwrap_or(2.0), last: f64::NAN })
        },
    )
}
