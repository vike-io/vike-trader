//! Discovery registry — name → metadata + fresh constructor, plus the two
//! orthogonal axes: pick-grouping [`Category`] vs render placement [`RenderKind`]/
//! [`OutputStyle`]. Mirrors the `registry()`/`get()` seam of vike-trader-app
//! `core/indicators/base.py` (Qt-free: no colour/paint state — that stays in the GUI).

// Every indicator struct is pub-re-exported from `crate::indicators`; glob-import
// them (171+ types) rather than maintain an explicit list.
use crate::indicators::*;
use crate::Indicator;
use std::sync::OnceLock;

/// Picker grouping (what family the indicator belongs to).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Overlap,
    Momentum,
    Volatility,
    Volume,
    Statistics,
    Pattern,
    Price,
    Structure,
    /// A USER-WRITTEN indicator ([`IndicatorMeta::user`]) — not a family at all, but the one
    /// honest answer for a file this crate has never seen. Filing one under `Statistics` (or any
    /// other family) would claim a classification nobody made, and a picker tab is exactly where
    /// that lie would be read as fact. Nothing in [`registry`] ever carries it: the built-in
    /// catalog is closed, so this variant only ever reaches a caller through a second registry.
    User,
}
impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Overlap => "Trend",
            Category::Momentum => "Momentum",
            Category::Volatility => "Volatility",
            Category::Volume => "Volume",
            Category::Statistics => "Statistics",
            Category::Pattern => "Patterns",
            Category::Price => "Price",
            Category::Structure => "Structure",
            Category::User => "User",
        }
    }
}

/// Where the indicator draws: on the price panel or in its own oscillator pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderKind {
    Overlay,
    Oscillator,
}

/// How a single output line renders.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputStyle {
    Line,
    Band,
    Histogram,
    Dots,
    /// A per-bar candlestick-pattern SIGNAL (`+100` bullish / `-100` bearish / `0` none)
    /// — NOT a price. `vike-chart`'s `render_overlay` anchors a glyph to the flagged bar's
    /// own extreme and keeps the series out of the price-pane autofit; plotting it on the
    /// price axis is meaningless. Pattern-exclusive: structure series that carry real
    /// PRICES say what they are instead — `zigzag` is a `Line` through its pivots,
    /// `williams_fractal` is `Dots` at its fractal highs/lows.
    Marker,
}

/// One output line's name + render style (order matches the `vectorize`/`on_bar` line order).
pub struct OutSpec {
    pub name: &'static str,
    pub style: OutputStyle,
}

/// One parameter's UI-facing spec: `default` is the value `new()` sets (so
/// `make_with(defaults)` reproduces `make()` bit-for-bit); `min`/`max`/`step`
/// drive a later dialog's sliders (T8). `name` is a DISPLAY string only — it
/// never renames the indicator struct's own field.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub name: &'static str,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

/// A USER-WRITTEN indicator's constructor: a closure that clones a specific compiled
/// prototype (a script's engine + AST + initial state), which is captured state a bare
/// `fn` pointer cannot reach.
///
/// `&'static` rather than `Arc` because the thing it constructs is process-lifetime by
/// nature — installed once at startup beside the built-ins, exactly like [`registry`]
/// itself — so refcounting it would buy nothing and cost a clone on every construction.
/// `Send + Sync` because [`IndicatorMeta`] lives behind a `&'static` shared across threads.
pub type UserFactory = &'static (dyn Fn(&[f64]) -> Box<dyn Indicator> + Send + Sync);

/// Static description of an indicator + constructors for a fresh streaming instance.
///
/// ⚠ **Construct through [`IndicatorMeta::build`]/[`IndicatorMeta::build_with`], never from a
/// field.** All THREE constructor fields below are PRIVATE for that reason: a bare `fn` cannot
/// capture a compiled prototype, so a [`IndicatorMeta::user`] row parks placeholders in `make`/
/// `make_with` and carries its real constructor in `factory`. A public constructor field would be
/// a landmine — reachable, plausible-looking, and wrong for exactly the rows a caller is least
/// likely to be thinking about — and that argument does not stop at the `fn` pointers: a public
/// `factory` invites `(m.factory.unwrap())(raw)`, which skips the [`coerce`] that
/// [`IndicatorMeta::build_with`] promises every [`UserFactory`] is handed. [`IndicatorMeta::is_user`]
/// is the whole of what a caller needs to READ. The three methods pick the right constructor;
/// nothing else can pick the wrong one.
pub struct IndicatorMeta {
    pub name: &'static str,
    pub pretty: &'static str,
    pub category: Category,
    pub kind: RenderKind,
    pub outputs: &'static [OutSpec],
    pub bands: &'static [f64],
    /// `true` when the streaming `on_bar` path cannot equal `vectorize` because
    /// the batch reads future bars (ichimoku's chikou, zigzag, williams_fractal).
    /// `vectorize` is the faithful authority; `on_bar` is a causal best-effort
    /// (NaN at the un-knowable tip) and the `on_bar == vectorize` parity gate
    /// skips these. Every other indicator sets `false`.
    pub batch_only: bool,
    /// Built-in default constructor. Private — see the type doc; [`IndicatorMeta::build`].
    make: fn() -> Box<dyn Indicator>,
    /// This indicator's parameter surface, in the order `build_with`'s slice
    /// reads them. Empty for paramless indicators (Vwap, Obv).
    pub params: &'static [ParamSpec],
    /// Built-in parameterised constructor (each registry entry runs its slice through [`coerce`]
    /// internally). Private — see the type doc; [`IndicatorMeta::build_with`].
    make_with: fn(&[f64]) -> Box<dyn Indicator>,
    /// `Some` only for a [`IndicatorMeta::user`] row. When set it is the AUTHORITY: `build`/
    /// `build_with` consult it and never touch `make`/`make_with`. `None` on every [`registry`]
    /// row, which is what keeps the built-in path byte-identical — and is machine-checked by
    /// `tests::no_builtin_row_carries_a_factory`, so the claim cannot rot into a comment.
    ///
    /// Private, like its two siblings — see the type doc. [`IndicatorMeta::is_user`] is the
    /// read-only view of it, and the only one a caller outside this module has ever needed.
    factory: Option<UserFactory>,
}

impl IndicatorMeta {
    /// A fresh streaming instance at this indicator's DEFAULT parameters.
    ///
    /// For a built-in that is `Default::default()` verbatim — the pre-`factory` expression,
    /// unchanged, so nothing about a registry indicator moved. For a user row it is the factory at
    /// the declared defaults (`coerce` of an empty slice).
    pub fn build(&self) -> Box<dyn Indicator> {
        match self.factory {
            Some(f) => f(&coerce(self.params, &[])),
            None => (self.make)(),
        }
    }

    /// A fresh streaming instance at `raw` parameters.
    ///
    /// Both arms see COERCED values, but by different routes, which is worth knowing: a built-in
    /// entry's `make_with` runs [`coerce`] itself (it always has), while the user arm coerces here
    /// — a [`UserFactory`] is handed already-clean params so a script author's constructor never
    /// has to re-implement the NaN/missing/clamp rules.
    pub fn build_with(&self, raw: &[f64]) -> Box<dyn Indicator> {
        match self.factory {
            Some(f) => f(&coerce(self.params, raw)),
            None => (self.make_with)(raw),
        }
    }

    /// `true` when this row is a USER-written indicator rather than one of the built-ins.
    ///
    /// Derived from the private `factory` rather than carried as its own flag, so it cannot
    /// disagree with which constructor `build` actually uses — the picker's "·user" tag and the
    /// thing that gets constructed are then the same fact.
    pub fn is_user(&self) -> bool {
        self.factory.is_some()
    }

    /// Builds a USER-written indicator's descriptor and LEAKS it to `'static`.
    ///
    /// Leaking is the honest lifetime, not a shortcut: a user indicator is installed once per
    /// process at startup and lives until exit, exactly like [`registry`]'s own `OnceLock`, so
    /// there is no point at which a free would be correct. The idiom is already this tree's — see
    /// `crates/vike-script/src/engine.rs`'s `register_user_indicators`, which leaks each generated
    /// accessor name for the same reason. ⚠ It is therefore a STARTUP call: leaking per chart
    /// frame would be an unbounded leak.
    ///
    /// `name` doubles as the persistence key (a workspace stores studies by name) and as the sole
    /// output line's name. `params` is the script's own declared knob surface.
    ///
    /// Deliberately NOT inserted into [`registry`]: that catalog is the closed, parity-gated set of
    /// built-ins, and `vike_script::user_indicator_conflict` refuses a user file that would shadow
    /// one *precisely* so the two name spaces stay disjoint. A user row reaches a caller through a
    /// second registry it opts into, never through `get`.
    pub fn user(
        name: &str,
        pretty: &str,
        kind: RenderKind,
        params: Vec<ParamSpec>,
        factory: UserFactory,
    ) -> &'static IndicatorMeta {
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        Box::leak(Box::new(IndicatorMeta {
            name,
            pretty: Box::leak(pretty.to_string().into_boxed_str()),
            category: Category::User,
            kind,
            // Single-output by construction: `vike_script::RhaiIndicator` REFUSES a multi-line
            // return rather than truncating it to line 0, so one line is not a simplification
            // here — it is the whole shape of a user indicator.
            outputs: Box::leak(vec![OutSpec { name, style: OutputStyle::Line }].into_boxed_slice()),
            bands: &[],
            batch_only: false,
            make: unbuilt,
            params: Box::leak(params.into_boxed_slice()),
            make_with: unbuilt_with,
            factory: Some(factory),
        }))
    }

    /// Warm-up depth for THIS indicator at `raw` params (run through [`coerce`] by
    /// [`IndicatorMeta::build_with`]): the index of the first bar at which `vectorize` yields a
    /// non-NaN value on any output line. See [`Indicator::lookback`] — exact when
    /// [`IndicatorMeta::lookback_exact`] is `true`, otherwise a conservative lower
    /// bound (`0`). Pass `&[]` for the defaults.
    pub fn lookback(&self, raw: &[f64]) -> usize {
        self.build_with(raw).lookback()
    }

    /// FULL warm-up depth at `raw` params: the first index at which EVERY output
    /// line is non-NaN — the number to size seed history with. Always
    /// `>= IndicatorMeta::lookback`. See [`Indicator::lookback_full`].
    pub fn lookback_full(&self, raw: &[f64]) -> usize {
        self.build_with(raw).lookback_full()
    }

    /// `true` when this indicator's [`IndicatorMeta::lookback`] is a real
    /// param-derived override rather than the conservative `0` default.
    pub fn lookback_exact(&self) -> bool {
        self.build().lookback_exact()
    }

    /// `true` when this indicator is path-dependent: its `lookback() == 0` is
    /// truthful but a caller seeding that many bars is NOT warm. See
    /// [`Indicator::warmup_path_dependent`].
    pub fn warmup_path_dependent(&self) -> bool {
        self.build().warmup_path_dependent()
    }
}

/// The `make` a [`IndicatorMeta::user`] row parks in the built-in slot.
///
/// UNREACHABLE by construction, not by convention: the field is private, every method above goes
/// through `build`/`build_with`, and both prefer `factory` — which a user row always has. It
/// panics rather than returning an inert all-NaN indicator because the two failure modes are not
/// equally bad: a silent NaN series is indistinguishable from a warming-up indicator (the exact
/// trap `vike_script::RhaiIndicator`'s `fault` channel exists to avoid), while a panic here can
/// only mean somebody added a third construction path inside this module and forgot the user arm.
fn unbuilt() -> Box<dyn Indicator> {
    panic!(
        "IndicatorMeta::user rows are constructed through IndicatorMeta::build/build_with (which \
         consult `factory`); the `make` slot is a placeholder because a bare fn cannot capture a \
         compiled prototype"
    )
}

/// [`unbuilt`]'s parameterised twin — same slot, same reasoning.
fn unbuilt_with(_: &[f64]) -> Box<dyn Indicator> {
    unbuilt()
}

/// Warm-up depth by registry name + raw params — the by-name entry point for
/// callers that only hold a string (chart/studio/backtest seed sizing).
/// `None` for an unknown name. See [`Indicator::lookback`] for the contract.
pub fn lookback(name: &str, raw: &[f64]) -> Option<usize> {
    get(name).map(|m| m.lookback(raw))
}

/// FULL warm-up depth by registry name + raw params (every output line valid) —
/// the number a seed-sizing caller wants. `None` for an unknown name. See
/// [`Indicator::lookback_full`].
pub fn lookback_full(name: &str, raw: &[f64]) -> Option<usize> {
    get(name).map(|m| m.lookback_full(raw))
}

fn mk<T: Indicator + Default + 'static>() -> Box<dyn Indicator> {
    Box::new(T::default())
}

macro_rules! out {
    ($($n:literal : $s:ident),* $(,)?) => {
        &[$( OutSpec { name: $n, style: OutputStyle::$s } ),*]
    };
}

macro_rules! params {
    ($(($n:literal, $default:expr, $min:expr, $max:expr, $step:expr)),* $(,)?) => {
        &[$( ParamSpec { name: $n, default: $default, min: $min, max: $max, step: $step } ),*]
    };
}

// Compact registry entry for the ported indicators: `make_with` runs the raw
// slice through `coerce($params)` then the struct's `with_params` (which reads
// params positionally — paramless structs ignore the empty slice, so this one
// form covers both). `make` uses `Default`.
macro_rules! ind {
    ($name:literal, $pretty:literal, $cat:ident, $kind:ident, $batch_only:expr,
     $outputs:expr, $bands:expr, $params:expr, $ty:ty) => {
        IndicatorMeta {
            name: $name,
            pretty: $pretty,
            category: $cat,
            kind: $kind,
            outputs: $outputs,
            bands: $bands,
            batch_only: $batch_only,
            make: mk::<$ty>,
            params: $params,
            make_with: |raw| Box::new(<$ty>::with_params(&coerce($params, raw))),
            factory: None,
        }
    };
}

// Period-type ParamSpecs share one range: any positive integer count of bars
// up to 1000 (a generous ceiling — no base-set indicator is meaningfully used
// beyond a few hundred), stepping by whole bars.
const SMA_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const EMA_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const WMA_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
// Bollinger's field is `m`; Keltner's is `mult` — both display as "mult" here
// (T7a inconsistency reconciled at the display-name layer only).
const BOLLINGER_PARAMS: &[ParamSpec] =
    params!(("length", 20.0, 1.0, 1000.0, 1.0), ("mult", 2.0, 0.1, 10.0, 0.1),);
const DONCHIAN_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const KELTNER_PARAMS: &[ParamSpec] = params!(
    ("ema_length", 20.0, 1.0, 1000.0, 1.0),
    ("atr_length", 10.0, 1.0, 1000.0, 1.0),
    ("mult", 2.0, 0.1, 10.0, 0.1),
);
const VWAP_PARAMS: &[ParamSpec] = &[];
// PSAR's step/max_af are small positive multipliers, not periods: step ranges
// over the classic Wellesley-Wilder 0.01-0.20 neighbourhood with headroom;
// max_af caps it below 1.0 (an af >= 1 makes the SAR jump straight to the EP).
const PSAR_PARAMS: &[ParamSpec] =
    params!(("step", 0.02, 0.001, 0.5, 0.001), ("max_af", 0.20, 0.05, 1.0, 0.01),);
const RSI_PARAMS: &[ParamSpec] = params!(("length", 14.0, 1.0, 1000.0, 1.0));
const MACD_PARAMS: &[ParamSpec] = params!(
    ("fast_length", 12.0, 1.0, 1000.0, 1.0),
    ("slow_length", 26.0, 1.0, 1000.0, 1.0),
    ("signal_length", 9.0, 1.0, 1000.0, 1.0),
);
const STOCHASTIC_PARAMS: &[ParamSpec] = params!(
    ("length", 14.0, 1.0, 1000.0, 1.0),
    ("smooth_k", 3.0, 1.0, 1000.0, 1.0),
    ("smooth_d", 3.0, 1.0, 1000.0, 1.0),
);
const ATR_PARAMS: &[ParamSpec] = params!(("length", 14.0, 1.0, 1000.0, 1.0));
const CCI_PARAMS: &[ParamSpec] = params!(("length", 20.0, 1.0, 1000.0, 1.0));
const ROC_PARAMS: &[ParamSpec] = params!(("length", 10.0, 1.0, 1000.0, 1.0));
const WILLIAMS_PARAMS: &[ParamSpec] = params!(("length", 14.0, 1.0, 1000.0, 1.0));
const OBV_PARAMS: &[ParamSpec] = &[];
const AWESOME_PARAMS: &[ParamSpec] =
    params!(("fast_length", 5.0, 1.0, 1000.0, 1.0), ("slow_length", 34.0, 1.0, 1000.0, 1.0),);

/// Coerce a raw parameter slice against `specs` — the ONE value-coercion site:
/// missing entries (`raw` shorter than `specs`) -> `default`; `NaN` -> `default`;
/// then clamp to `[min, max]`. Extra `raw` entries beyond `specs.len()` are
/// ignored. Every `make_with` fn below runs its raw slice through this before
/// handing it to the indicator's `with_params`.
pub fn coerce(specs: &[ParamSpec], raw: &[f64]) -> Vec<f64> {
    specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let mut v = raw.get(i).copied().unwrap_or(f64::NAN);
            if v.is_nan() {
                v = spec.default;
            }
            v.clamp(spec.min, spec.max)
        })
        .collect()
}

fn build_registry() -> Vec<IndicatorMeta> {
    use Category::*;
    use RenderKind::*;
    vec![
        IndicatorMeta {
            name: "sma",
            pretty: "Simple MA",
            category: Overlap,
            kind: Overlay,
            outputs: out! {"sma":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Sma>,
            params: SMA_PARAMS,
            make_with: |raw| Box::new(Sma::with_params(&coerce(SMA_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "ema",
            pretty: "Exponential MA",
            category: Overlap,
            kind: Overlay,
            outputs: out! {"ema":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Ema>,
            params: EMA_PARAMS,
            make_with: |raw| Box::new(Ema::with_params(&coerce(EMA_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "wma",
            pretty: "Weighted MA",
            category: Overlap,
            kind: Overlay,
            outputs: out! {"wma":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Wma>,
            params: WMA_PARAMS,
            make_with: |raw| Box::new(Wma::with_params(&coerce(WMA_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "bollinger",
            pretty: "Bollinger Bands",
            category: Volatility,
            kind: Overlay,
            outputs: out! {"upper":Band,"mid":Line,"lower":Band},
            bands: &[],
            batch_only: false,
            make: mk::<Bollinger>,
            params: BOLLINGER_PARAMS,
            make_with: |raw| Box::new(Bollinger::with_params(&coerce(BOLLINGER_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "donchian",
            pretty: "Donchian Channel",
            category: Volatility,
            kind: Overlay,
            outputs: out! {"upper":Band,"mid":Line,"lower":Band},
            bands: &[],
            batch_only: false,
            make: mk::<Donchian>,
            params: DONCHIAN_PARAMS,
            make_with: |raw| Box::new(Donchian::with_params(&coerce(DONCHIAN_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "keltner",
            pretty: "Keltner Channel",
            category: Volatility,
            kind: Overlay,
            outputs: out! {"upper":Band,"mid":Line,"lower":Band},
            bands: &[],
            batch_only: false,
            make: mk::<Keltner>,
            params: KELTNER_PARAMS,
            make_with: |raw| Box::new(Keltner::with_params(&coerce(KELTNER_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "vwap",
            pretty: "VWAP (session)",
            category: Volume,
            kind: Overlay,
            outputs: out! {"vwap":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Vwap>,
            params: VWAP_PARAMS,
            make_with: |_| Box::new(Vwap::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "psar",
            pretty: "Parabolic SAR",
            category: Overlap,
            kind: Overlay,
            outputs: out! {"psar":Dots},
            bands: &[],
            batch_only: false,
            make: mk::<Psar>,
            params: PSAR_PARAMS,
            make_with: |raw| Box::new(Psar::with_params(&coerce(PSAR_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "rsi",
            pretty: "RSI",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"rsi":Line},
            bands: &[30.0, 50.0, 70.0],
            batch_only: false,
            make: mk::<Rsi>,
            params: RSI_PARAMS,
            make_with: |raw| Box::new(Rsi::with_params(&coerce(RSI_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "macd",
            pretty: "MACD",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"macd":Line,"signal":Line,"hist":Histogram},
            bands: &[0.0],
            batch_only: false,
            make: mk::<Macd>,
            params: MACD_PARAMS,
            make_with: |raw| Box::new(Macd::with_params(&coerce(MACD_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "stochastic",
            pretty: "Stochastic",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"%K":Line,"%D":Line},
            bands: &[20.0, 80.0],
            batch_only: false,
            make: mk::<Stochastic>,
            params: STOCHASTIC_PARAMS,
            make_with: |raw| Box::new(Stochastic::with_params(&coerce(STOCHASTIC_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "atr",
            pretty: "ATR",
            category: Volatility,
            kind: Oscillator,
            outputs: out! {"atr":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Atr>,
            params: ATR_PARAMS,
            make_with: |raw| Box::new(Atr::with_params(&coerce(ATR_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "cci",
            pretty: "CCI",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"cci":Line},
            bands: &[-100.0, 100.0],
            batch_only: false,
            make: mk::<Cci>,
            params: CCI_PARAMS,
            make_with: |raw| Box::new(Cci::with_params(&coerce(CCI_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "roc",
            pretty: "Rate of Change",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"roc":Line},
            bands: &[0.0],
            batch_only: false,
            make: mk::<Roc>,
            params: ROC_PARAMS,
            make_with: |raw| Box::new(Roc::with_params(&coerce(ROC_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "williams",
            pretty: "Williams %R",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"%R":Line},
            bands: &[-80.0, -20.0],
            batch_only: false,
            make: mk::<Williams>,
            params: WILLIAMS_PARAMS,
            make_with: |raw| Box::new(Williams::with_params(&coerce(WILLIAMS_PARAMS, raw))),
            factory: None,
        },
        IndicatorMeta {
            name: "obv",
            pretty: "On-Balance Volume",
            category: Volume,
            kind: Oscillator,
            outputs: out! {"obv":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Obv>,
            params: OBV_PARAMS,
            make_with: |_| Box::new(Obv::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "awesome",
            pretty: "Awesome Oscillator",
            category: Momentum,
            kind: Oscillator,
            outputs: out! {"ao":Histogram},
            bands: &[0.0],
            batch_only: false,
            make: mk::<Awesome>,
            params: AWESOME_PARAMS,
            make_with: |raw| Box::new(Awesome::with_params(&coerce(AWESOME_PARAMS, raw))),
            factory: None,
        },
        // ---- momentum (momentum.py) ----
        ind!(
            "mom",
            "Momentum",
            Momentum,
            Oscillator,
            false,
            out! {"mom":Line},
            &[0.0],
            params!(("period", 10.0, 1.0, 400.0, 1.0)),
            Mom
        ),
        ind!(
            "rocp",
            "ROC Percentage",
            Momentum,
            Oscillator,
            false,
            out! {"rocp":Line},
            &[0.0],
            params!(("period", 10.0, 1.0, 400.0, 1.0)),
            Rocp
        ),
        ind!(
            "rocr",
            "ROC Ratio",
            Momentum,
            Oscillator,
            false,
            out! {"rocr":Line},
            &[],
            params!(("period", 10.0, 1.0, 400.0, 1.0)),
            Rocr
        ),
        ind!(
            "rocr100",
            "ROC Ratio ×100",
            Momentum,
            Oscillator,
            false,
            out! {"rocr100":Line},
            &[],
            params!(("period", 10.0, 1.0, 400.0, 1.0)),
            Rocr100
        ),
        ind!(
            "apo",
            "Absolute Price Osc",
            Momentum,
            Oscillator,
            false,
            out! {"apo":Line},
            &[0.0],
            params!(("fast", 12.0, 2.0, 200.0, 1.0), ("slow", 26.0, 2.0, 400.0, 1.0)),
            Apo
        ),
        ind!(
            "ppo",
            "Percentage Price Osc",
            Momentum,
            Oscillator,
            false,
            out! {"ppo":Line},
            &[0.0],
            params!(("fast", 12.0, 2.0, 200.0, 1.0), ("slow", 26.0, 2.0, 400.0, 1.0)),
            Ppo
        ),
        ind!(
            "cmo",
            "Chande Momentum Osc",
            Momentum,
            Oscillator,
            false,
            out! {"cmo":Line},
            &[-50.0, 50.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Cmo
        ),
        ind!(
            "bop",
            "Balance of Power",
            Momentum,
            Oscillator,
            false,
            out! {"bop":Line},
            &[0.0],
            &[],
            Bop
        ),
        ind!(
            "dpo",
            "Detrended Price Osc",
            Momentum,
            Oscillator,
            false,
            out! {"dpo":Line},
            &[0.0],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Dpo
        ),
        ind!(
            "trix",
            "TRIX",
            Momentum,
            Oscillator,
            false,
            out! {"trix":Line},
            &[0.0],
            params!(("period", 18.0, 2.0, 200.0, 1.0)),
            Trix
        ),
        ind!(
            "tsi",
            "True Strength Index",
            Momentum,
            Oscillator,
            false,
            out! {"tsi":Line},
            &[0.0],
            params!(("long", 25.0, 2.0, 400.0, 1.0), ("short", 13.0, 2.0, 200.0, 1.0)),
            Tsi
        ),
        ind!(
            "smi_ergodic",
            "SMI Ergodic",
            Momentum,
            Oscillator,
            false,
            out! {"smi":Line, "signal":Line},
            &[0.0],
            params!(
                ("long", 20.0, 2.0, 400.0, 1.0),
                ("short", 5.0, 2.0, 200.0, 1.0),
                ("signal", 5.0, 2.0, 200.0, 1.0)
            ),
            SmiErgodic
        ),
        ind!(
            "coppock",
            "Coppock Curve",
            Momentum,
            Oscillator,
            false,
            out! {"coppock":Line},
            &[0.0],
            params!(
                ("wma_p", 10.0, 2.0, 100.0, 1.0),
                ("roc_long", 14.0, 2.0, 200.0, 1.0),
                ("roc_short", 11.0, 2.0, 200.0, 1.0)
            ),
            Coppock
        ),
        ind!(
            "kst",
            "Know Sure Thing",
            Momentum,
            Oscillator,
            false,
            out! {"kst":Line, "signal":Line},
            &[0.0],
            params!(
                ("roc1", 10.0, 1.0, 200.0, 1.0),
                ("sma1", 10.0, 2.0, 200.0, 1.0),
                ("roc2", 15.0, 1.0, 200.0, 1.0),
                ("sma2", 10.0, 2.0, 200.0, 1.0),
                ("roc3", 20.0, 1.0, 200.0, 1.0),
                ("sma3", 10.0, 2.0, 200.0, 1.0),
                ("roc4", 30.0, 1.0, 200.0, 1.0),
                ("sma4", 15.0, 2.0, 200.0, 1.0),
                ("signal", 9.0, 2.0, 200.0, 1.0)
            ),
            Kst
        ),
        ind!(
            "aroon",
            "Aroon",
            Momentum,
            Oscillator,
            false,
            out! {"aroon_up":Line, "aroon_down":Line},
            &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)),
            Aroon
        ),
        ind!(
            "aroonosc",
            "Aroon Oscillator",
            Momentum,
            Oscillator,
            false,
            out! {"aroonosc":Line},
            &[0.0],
            params!(("period", 14.0, 2.0, 400.0, 1.0)),
            Aroonosc
        ),
        ind!(
            "adx",
            "ADX",
            Momentum,
            Oscillator,
            false,
            out! {"adx":Line, "plus_di":Line, "minus_di":Line},
            &[20.0, 25.0],
            params!(("period", 14.0, 2.0, 100.0, 1.0)),
            Adx
        ),
        ind!(
            "adxr",
            "ADXR",
            Momentum,
            Oscillator,
            false,
            out! {"adxr":Line},
            &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)),
            Adxr
        ),
        ind!(
            "elder_ray",
            "Elder Ray",
            Momentum,
            Oscillator,
            false,
            out! {"bull_power":Line, "bear_power":Line},
            &[0.0],
            params!(("period", 13.0, 2.0, 200.0, 1.0)),
            ElderRay
        ),
        ind!(
            "stochf",
            "Fast Stochastic",
            Momentum,
            Oscillator,
            false,
            out! {"%K":Line, "%D":Line},
            &[20.0, 80.0],
            params!(("k", 14.0, 2.0, 100.0, 1.0), ("d", 3.0, 1.0, 50.0, 1.0)),
            Stochf
        ),
        ind!(
            "stochrsi",
            "Stochastic RSI",
            Momentum,
            Oscillator,
            false,
            out! {"%K":Line, "%D":Line},
            &[20.0, 80.0],
            params!(
                ("rsi_p", 14.0, 2.0, 100.0, 1.0),
                ("k", 14.0, 2.0, 100.0, 1.0),
                ("d", 3.0, 1.0, 50.0, 1.0)
            ),
            Stochrsi
        ),
        ind!(
            "ultosc",
            "Ultimate Oscillator",
            Momentum,
            Oscillator,
            false,
            out! {"ultosc":Line},
            &[30.0, 70.0],
            params!(
                ("p1", 7.0, 2.0, 100.0, 1.0),
                ("p2", 14.0, 2.0, 200.0, 1.0),
                ("p3", 28.0, 2.0, 400.0, 1.0)
            ),
            Ultosc
        ),
        ind!(
            "vortex",
            "Vortex",
            Momentum,
            Oscillator,
            false,
            out! {"vi_plus":Line, "vi_minus":Line},
            &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Vortex
        ),
        ind!(
            "chande_kroll_stop",
            "Chande Kroll Stop",
            Momentum,
            Overlay,
            false,
            out! {"long_stop":Line, "short_stop":Line},
            &[],
            params!(
                ("p", 10.0, 2.0, 200.0, 1.0),
                ("x", 1.0, 1.0, 10.0, 1.0),
                ("q", 9.0, 2.0, 200.0, 1.0)
            ),
            ChandeKrollStop
        ),
        ind!(
            "asi",
            "Accumulative Swing Index",
            Momentum,
            Oscillator,
            false,
            out! {"asi":Line},
            &[0.0],
            params!(("limit", 1.0, 0.1, 10.0, 0.1)),
            Asi
        ),
        ind!(
            "fisher",
            "Fisher Transform",
            Momentum,
            Oscillator,
            false,
            out! {"fisher":Line, "trigger":Line},
            &[0.0],
            params!(("period", 9.0, 2.0, 100.0, 1.0)),
            Fisher
        ),
        ind!(
            "connors_rsi",
            "Connors RSI",
            Momentum,
            Oscillator,
            false,
            out! {"crsi":Line},
            &[30.0, 70.0],
            params!(
                ("rsi_p", 3.0, 2.0, 100.0, 1.0),
                ("streak_p", 2.0, 2.0, 100.0, 1.0),
                ("rank_p", 100.0, 10.0, 500.0, 1.0)
            ),
            ConnorsRsi
        ),
        ind!(
            "relative_vigor",
            "Relative Vigor Index",
            Momentum,
            Oscillator,
            false,
            out! {"rvgi":Line, "signal":Line},
            &[0.0],
            params!(("period", 10.0, 2.0, 200.0, 1.0)),
            RelativeVigor
        ),
        ind!(
            "ac",
            "Accelerator Oscillator",
            Momentum,
            Oscillator,
            false,
            out! {"ac":Histogram},
            &[0.0],
            &[],
            Ac
        ),
        // ---- volatility (volatility.py) ----
        ind!(
            "true_range",
            "True Range",
            Volatility,
            Oscillator,
            false,
            out! {"true_range":Line},
            &[],
            &[],
            TrueRange
        ),
        ind!(
            "natr",
            "Normalized ATR",
            Volatility,
            Oscillator,
            false,
            out! {"natr":Line},
            &[],
            params!(("period", 14.0, 2.0, 100.0, 1.0)),
            Natr
        ),
        ind!(
            "stddev",
            "Std Deviation",
            Volatility,
            Oscillator,
            false,
            out! {"stddev":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Stddev
        ),
        ind!(
            "hvol",
            "Historical Volatility",
            Volatility,
            Oscillator,
            false,
            out! {"hvol":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("ann", 365.0, 1.0, 365.0, 1.0)),
            Hvol
        ),
        ind!(
            "bbands_pctb",
            "Bollinger %B",
            Volatility,
            Oscillator,
            false,
            out! {"pctb":Line},
            &[0.0, 1.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("k", 2.0, 0.5, 5.0, 0.1)),
            BbandsPctb
        ),
        ind!(
            "bbands_width",
            "Bollinger Width",
            Volatility,
            Oscillator,
            false,
            out! {"width":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("k", 2.0, 0.5, 5.0, 0.1)),
            BbandsWidth
        ),
        ind!(
            "donchian_width",
            "Donchian Width",
            Volatility,
            Oscillator,
            false,
            out! {"width":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            DonchianWidth
        ),
        ind!(
            "ulcer",
            "Ulcer Index",
            Volatility,
            Oscillator,
            false,
            out! {"ulcer":Line},
            &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Ulcer
        ),
        ind!(
            "chop",
            "Choppiness Index",
            Volatility,
            Oscillator,
            false,
            out! {"chop":Line},
            &[38.2, 61.8],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Chop
        ),
        ind!(
            "relative_volatility",
            "Relative Volatility Index",
            Volatility,
            Oscillator,
            false,
            out! {"rvi":Line},
            &[20.0, 80.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            RelativeVolatility
        ),
        ind!(
            "high_low_52w",
            "52-Week High/Low",
            Volatility,
            Overlay,
            false,
            out! {"high_n":Line, "low_n":Line},
            &[],
            params!(("period", 252.0, 2.0, 1000.0, 1.0)),
            HighLow52w
        ),
        ind!(
            "mass",
            "Mass Index",
            Volatility,
            Oscillator,
            false,
            out! {"mass":Line},
            &[26.5, 27.0],
            params!(("period", 25.0, 2.0, 200.0, 1.0), ("ema_period", 9.0, 2.0, 50.0, 1.0)),
            Mass
        ),
        // ---- overlap / trend (overlap.py) ----
        ind!(
            "dema",
            "Double EMA",
            Overlap,
            Overlay,
            false,
            out! {"dema":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Dema
        ),
        ind!(
            "tema",
            "Triple EMA",
            Overlap,
            Overlay,
            false,
            out! {"tema":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Tema
        ),
        ind!(
            "trima",
            "Triangular MA",
            Overlap,
            Overlay,
            false,
            out! {"trima":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Trima
        ),
        ind!(
            "smma",
            "Smoothed MA",
            Overlap,
            Overlay,
            false,
            out! {"smma":Line},
            &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)),
            Smma
        ),
        ind!(
            "zlema",
            "Zero-Lag EMA",
            Overlap,
            Overlay,
            false,
            out! {"zlema":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Zlema
        ),
        ind!(
            "hma",
            "Hull MA",
            Overlap,
            Overlay,
            false,
            out! {"hma":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Hma
        ),
        ind!(
            "vwma",
            "Volume-Weighted MA",
            Overlap,
            Overlay,
            false,
            out! {"vwma":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0)),
            Vwma
        ),
        ind!(
            "t3",
            "Tillson T3",
            Overlap,
            Overlay,
            false,
            out! {"t3":Line},
            &[],
            params!(("period", 20.0, 2.0, 400.0, 1.0), ("v", 0.7, 0.0, 1.0, 0.05)),
            T3
        ),
        ind!(
            "alma",
            "Arnaud Legoux MA",
            Overlap,
            Overlay,
            false,
            out! {"alma":Line},
            &[],
            params!(
                ("period", 20.0, 2.0, 400.0, 1.0),
                ("offset", 0.85, 0.0, 1.0, 0.05),
                ("sigma", 6.0, 1.0, 20.0, 0.5)
            ),
            Alma
        ),
        ind!(
            "midpoint",
            "Midpoint",
            Overlap,
            Overlay,
            false,
            out! {"midpoint":Line},
            &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)),
            Midpoint
        ),
        ind!(
            "midprice",
            "Midprice",
            Overlap,
            Overlay,
            false,
            out! {"midprice":Line},
            &[],
            params!(("period", 14.0, 2.0, 400.0, 1.0)),
            Midprice
        ),
        ind!(
            "supertrend",
            "Supertrend",
            Overlap,
            Overlay,
            false,
            out! {"supertrend":Line, "direction":Line},
            &[],
            params!(("period", 10.0, 1.0, 100.0, 1.0), ("mult", 3.0, 0.5, 10.0, 0.5)),
            Supertrend
        ),
        ind!(
            "ichimoku",
            "Ichimoku Cloud",
            Overlap,
            Overlay,
            true,
            out! {"tenkan":Line, "kijun":Line, "senkou_a":Line, "senkou_b":Line, "chikou":Line},
            &[],
            params!(
                ("tenkan", 9.0, 2.0, 100.0, 1.0),
                ("kijun", 26.0, 2.0, 100.0, 1.0),
                ("senkou", 52.0, 2.0, 200.0, 1.0)
            ),
            Ichimoku
        ),
        ind!(
            "mcginley",
            "McGinley Dynamic",
            Overlap,
            Overlay,
            false,
            out! {"mcginley":Line},
            &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Mcginley
        ),
        ind!(
            "gmma",
            "Guppy MMA",
            Overlap,
            Overlay,
            false,
            out! {"s3":Line, "s5":Line, "s8":Line, "s10":Line, "s12":Line, "s15":Line,
            "l30":Line, "l35":Line, "l40":Line, "l45":Line, "l50":Line, "l60":Line},
            &[],
            &[],
            Gmma
        ),
        ind!(
            "envelopes",
            "Envelopes",
            Overlap,
            Overlay,
            false,
            out! {"upper":Line, "mid":Line, "lower":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("pct", 2.5, 0.1, 20.0, 0.1)),
            Envelopes
        ),
        ind!(
            "alligator",
            "Alligator",
            Overlap,
            Overlay,
            false,
            out! {"jaw":Line, "teeth":Line, "lips":Line},
            &[],
            &[],
            Alligator
        ),
        // ---- volume (volume.py) ----
        ind!(
            "ad",
            "Accumulation/Distribution",
            Volume,
            Oscillator,
            false,
            out! {"ad":Line},
            &[],
            &[],
            Ad
        ),
        ind!(
            "adosc",
            "Chaikin A/D Oscillator",
            Volume,
            Oscillator,
            false,
            out! {"adosc":Line},
            &[0.0],
            params!(("fast", 3.0, 2.0, 50.0, 1.0), ("slow", 10.0, 2.0, 200.0, 1.0)),
            Adosc
        ),
        ind!(
            "cmf",
            "Chaikin Money Flow",
            Volume,
            Oscillator,
            false,
            out! {"cmf":Line},
            &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Cmf
        ),
        ind!(
            "efi",
            "Elder Force Index",
            Volume,
            Oscillator,
            false,
            out! {"efi":Line},
            &[0.0],
            params!(("period", 13.0, 2.0, 200.0, 1.0)),
            Efi
        ),
        ind!(
            "eom",
            "Ease of Movement",
            Volume,
            Oscillator,
            false,
            out! {"eom":Line},
            &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Eom
        ),
        ind!(
            "kvo",
            "Klinger Volume Oscillator",
            Volume,
            Oscillator,
            false,
            out! {"kvo":Line, "signal":Line},
            &[0.0],
            params!(
                ("fast", 34.0, 2.0, 200.0, 1.0),
                ("slow", 55.0, 2.0, 500.0, 1.0),
                ("signal", 13.0, 2.0, 100.0, 1.0)
            ),
            Kvo
        ),
        ind!(
            "mfi",
            "Money Flow Index",
            Volume,
            Oscillator,
            false,
            out! {"mfi":Line},
            &[20.0, 80.0],
            params!(("period", 14.0, 2.0, 100.0, 1.0)),
            Mfi
        ),
        ind!(
            "net_volume",
            "Net Volume",
            Volume,
            Oscillator,
            false,
            out! {"net_volume":Histogram},
            &[0.0],
            &[],
            NetVolume
        ),
        ind!(
            "nvi",
            "Negative Volume Index",
            Volume,
            Oscillator,
            false,
            out! {"nvi":Line},
            &[],
            &[],
            Nvi
        ),
        ind!(
            "pvi",
            "Positive Volume Index",
            Volume,
            Oscillator,
            false,
            out! {"pvi":Line},
            &[],
            &[],
            Pvi
        ),
        ind!(
            "pvt",
            "Price Volume Trend",
            Volume,
            Oscillator,
            false,
            out! {"pvt":Line},
            &[],
            &[],
            Pvt
        ),
        ind!(
            "volume_osc",
            "Volume Oscillator",
            Volume,
            Oscillator,
            false,
            out! {"volume_osc":Line},
            &[0.0],
            params!(("short", 5.0, 2.0, 50.0, 1.0), ("long", 10.0, 2.0, 200.0, 1.0)),
            VolumeOsc
        ),
        // ---- statistics (statistics.py) — single-series only ----
        ind!(
            "linearreg",
            "Linear Regression",
            Statistics,
            Oscillator,
            false,
            out! {"linearreg":Line},
            &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Linearreg
        ),
        ind!(
            "linearreg_slope",
            "Linear Reg Slope",
            Statistics,
            Oscillator,
            false,
            out! {"slope":Line},
            &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            LinearregSlope
        ),
        ind!(
            "linearreg_angle",
            "Linear Reg Angle",
            Statistics,
            Oscillator,
            false,
            out! {"angle":Line},
            &[0.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            LinearregAngle
        ),
        ind!(
            "linearreg_intercept",
            "Linear Reg Intercept",
            Statistics,
            Oscillator,
            false,
            out! {"intercept":Line},
            &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            LinearregIntercept
        ),
        ind!(
            "tsf",
            "Time Series Forecast",
            Statistics,
            Oscillator,
            false,
            out! {"tsf":Line},
            &[],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            Tsf
        ),
        ind!(
            "var",
            "Variance",
            Statistics,
            Oscillator,
            false,
            out! {"var":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Var
        ),
        ind!(
            "zscore",
            "Z-Score",
            Statistics,
            Oscillator,
            false,
            out! {"zscore":Line},
            &[-2.0, 0.0, 2.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Zscore
        ),
        ind!(
            "skew",
            "Skewness",
            Statistics,
            Oscillator,
            false,
            out! {"skew":Line},
            &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Skew
        ),
        ind!(
            "kurtosis",
            "Kurtosis",
            Statistics,
            Oscillator,
            false,
            out! {"kurtosis":Line},
            &[0.0],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Kurtosis
        ),
        ind!(
            "mad",
            "Mean Abs Deviation",
            Statistics,
            Oscillator,
            false,
            out! {"mad":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            Mad
        ),
        ind!(
            "std_error",
            "Standard Error",
            Statistics,
            Oscillator,
            false,
            out! {"std_error":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0)),
            StdError
        ),
        ind!(
            "std_error_bands",
            "Std Error Bands",
            Statistics,
            Oscillator,
            false,
            out! {"upper":Line, "mid":Line, "lower":Line},
            &[],
            params!(("period", 20.0, 2.0, 200.0, 1.0), ("mult", 2.0, 0.1, 10.0, 0.1)),
            StdErrorBands
        ),
        ind!(
            "rank_correlation",
            "Rank Correlation",
            Statistics,
            Oscillator,
            false,
            out! {"rci":Line},
            &[-80.0, 80.0],
            params!(("period", 14.0, 2.0, 200.0, 1.0)),
            RankCorrelation
        ),
        // ---- structure (structure.py) ----
        ind!(
            "pivot_points",
            "Pivot Points",
            Structure,
            Overlay,
            false,
            out! {"p":Line, "r1":Line, "r2":Line, "r3":Line, "s1":Line, "s2":Line, "s3":Line},
            &[],
            &[],
            PivotPoints
        ),
        ind!(
            "volume_profile_poc",
            "Volume Profile POC",
            Structure,
            Overlay,
            false,
            out! {"poc":Line},
            &[],
            params!(("window", 50.0, 5.0, 500.0, 1.0), ("bins", 24.0, 4.0, 200.0, 1.0)),
            VolumeProfilePoc
        ),
        ind!(
            "zigzag",
            "ZigZag",
            Structure,
            Overlay,
            true,
            // `batch_zigzag` emits the PIVOT PRICE at each confirmed pivot (NaN between), so
            // this is a real price series drawn as the connecting zigzag line — `Line` says so.
            // Pixel-identical to the previous `Marker`: both take the non-Band overlay width
            // (1.5) and the same `seg_line` path, which already breaks the line at the NaN gaps.
            out! {"zigzag":Line},
            &[],
            params!(("deviation", 5.0, 0.1, 50.0, 0.5)),
            Zigzag
        ),
        ind!(
            "williams_fractal",
            "Williams Fractal",
            Structure,
            Overlay,
            true,
            // `batch_williams_fractal` emits the fractal's own high/low PRICE at each fractal
            // bar (NaN between) — discrete points, never a path. As `Marker` they fell to
            // `seg_line` and got joined into a zigzagging line across unrelated bars; `Dots`
            // renders each one where it belongs, on its bar.
            out! {"fractal_up":Dots, "fractal_down":Dots},
            &[],
            params!(("n", 2.0, 1.0, 10.0, 1.0)),
            WilliamsFractal
        ),
        // ---- candlestick patterns (patterns.py) ----
        ind!("doji", "Doji", Pattern, Overlay, false, out! {"doji":Marker}, &[], &[], Doji),
        ind!(
            "engulfing",
            "Engulfing",
            Pattern,
            Overlay,
            false,
            out! {"engulfing":Marker},
            &[],
            &[],
            Engulfing
        ),
        ind!("hammer", "Hammer", Pattern, Overlay, false, out! {"hammer":Marker}, &[], &[], Hammer),
        ind!(
            "inverted_hammer",
            "Inverted Hammer",
            Pattern,
            Overlay,
            false,
            out! {"inverted_hammer":Marker},
            &[],
            &[],
            InvertedHammer
        ),
        ind!(
            "hanging_man",
            "Hanging Man",
            Pattern,
            Overlay,
            false,
            out! {"hanging_man":Marker},
            &[],
            &[],
            HangingMan
        ),
        ind!(
            "shooting_star",
            "Shooting Star",
            Pattern,
            Overlay,
            false,
            out! {"shooting_star":Marker},
            &[],
            &[],
            ShootingStar
        ),
        ind!(
            "dragonfly_doji",
            "Dragonfly Doji",
            Pattern,
            Overlay,
            false,
            out! {"dragonfly_doji":Marker},
            &[],
            &[],
            DragonflyDoji
        ),
        ind!(
            "gravestone_doji",
            "Gravestone Doji",
            Pattern,
            Overlay,
            false,
            out! {"gravestone_doji":Marker},
            &[],
            &[],
            GravestoneDoji
        ),
        ind!(
            "longlegged_doji",
            "Long-Legged Doji",
            Pattern,
            Overlay,
            false,
            out! {"longlegged_doji":Marker},
            &[],
            &[],
            LongleggedDoji
        ),
        ind!(
            "rickshaw_man",
            "Rickshaw Man",
            Pattern,
            Overlay,
            false,
            out! {"rickshaw_man":Marker},
            &[],
            &[],
            RickshawMan
        ),
        ind!("takuri", "Takuri", Pattern, Overlay, false, out! {"takuri":Marker}, &[], &[], Takuri),
        ind!(
            "marubozu",
            "Marubozu",
            Pattern,
            Overlay,
            false,
            out! {"marubozu":Marker},
            &[],
            &[],
            Marubozu
        ),
        ind!(
            "closing_marubozu",
            "Closing Marubozu",
            Pattern,
            Overlay,
            false,
            out! {"closing_marubozu":Marker},
            &[],
            &[],
            ClosingMarubozu
        ),
        ind!(
            "spinning_top",
            "Spinning Top",
            Pattern,
            Overlay,
            false,
            out! {"spinning_top":Marker},
            &[],
            &[],
            SpinningTop
        ),
        ind!(
            "high_wave",
            "High Wave",
            Pattern,
            Overlay,
            false,
            out! {"high_wave":Marker},
            &[],
            &[],
            HighWave
        ),
        ind!(
            "long_line",
            "Long Line",
            Pattern,
            Overlay,
            false,
            out! {"long_line":Marker},
            &[],
            &[],
            LongLine
        ),
        ind!(
            "short_line",
            "Short Line",
            Pattern,
            Overlay,
            false,
            out! {"short_line":Marker},
            &[],
            &[],
            ShortLine
        ),
        ind!(
            "belt_hold",
            "Belt Hold",
            Pattern,
            Overlay,
            false,
            out! {"belt_hold":Marker},
            &[],
            &[],
            BeltHold
        ),
        ind!(
            "opening_marubozu",
            "Opening Marubozu",
            Pattern,
            Overlay,
            false,
            out! {"opening_marubozu":Marker},
            &[],
            &[],
            OpeningMarubozu
        ),
        ind!(
            "doji_star",
            "Doji Star",
            Pattern,
            Overlay,
            false,
            out! {"doji_star":Marker},
            &[],
            &[],
            DojiStar
        ),
        ind!("harami", "Harami", Pattern, Overlay, false, out! {"harami":Marker}, &[], &[], Harami),
        ind!(
            "harami_cross",
            "Harami Cross",
            Pattern,
            Overlay,
            false,
            out! {"harami_cross":Marker},
            &[],
            &[],
            HaramiCross
        ),
        ind!(
            "piercing",
            "Piercing",
            Pattern,
            Overlay,
            false,
            out! {"piercing":Marker},
            &[],
            &[],
            Piercing
        ),
        ind!(
            "dark_cloud_cover",
            "Dark Cloud Cover",
            Pattern,
            Overlay,
            false,
            out! {"dark_cloud_cover":Marker},
            &[],
            &[],
            DarkCloudCover
        ),
        ind!(
            "counterattack",
            "Counterattack",
            Pattern,
            Overlay,
            false,
            out! {"counterattack":Marker},
            &[],
            &[],
            Counterattack
        ),
        ind!(
            "meeting_lines",
            "Meeting Lines",
            Pattern,
            Overlay,
            false,
            out! {"meeting_lines":Marker},
            &[],
            &[],
            MeetingLines
        ),
        ind!(
            "separating_lines",
            "Separating Lines",
            Pattern,
            Overlay,
            false,
            out! {"separating_lines":Marker},
            &[],
            &[],
            SeparatingLines
        ),
        ind!(
            "matching_low",
            "Matching Low",
            Pattern,
            Overlay,
            false,
            out! {"matching_low":Marker},
            &[],
            &[],
            MatchingLow
        ),
        ind!(
            "on_neck",
            "On Neck",
            Pattern,
            Overlay,
            false,
            out! {"on_neck":Marker},
            &[],
            &[],
            OnNeck
        ),
        ind!(
            "in_neck",
            "In Neck",
            Pattern,
            Overlay,
            false,
            out! {"in_neck":Marker},
            &[],
            &[],
            InNeck
        ),
        ind!(
            "thrusting",
            "Thrusting",
            Pattern,
            Overlay,
            false,
            out! {"thrusting":Marker},
            &[],
            &[],
            Thrusting
        ),
        ind!(
            "kicking",
            "Kicking",
            Pattern,
            Overlay,
            false,
            out! {"kicking":Marker},
            &[],
            &[],
            Kicking
        ),
        ind!(
            "kicking_by_length",
            "Kicking by Length",
            Pattern,
            Overlay,
            false,
            out! {"kicking_by_length":Marker},
            &[],
            &[],
            KickingByLength
        ),
        ind!(
            "homing_pigeon",
            "Homing Pigeon",
            Pattern,
            Overlay,
            false,
            out! {"homing_pigeon":Marker},
            &[],
            &[],
            HomingPigeon
        ),
        ind!(
            "gap_side_side_white",
            "Gapping Side-by-Side White",
            Pattern,
            Overlay,
            false,
            out! {"gap_side_side_white":Marker},
            &[],
            &[],
            GapSideSideWhite
        ),
        ind!(
            "tasuki_gap",
            "Tasuki Gap",
            Pattern,
            Overlay,
            false,
            out! {"tasuki_gap":Marker},
            &[],
            &[],
            TasukiGap
        ),
        ind!(
            "morning_star",
            "Morning Star",
            Pattern,
            Overlay,
            false,
            out! {"morning_star":Marker},
            &[],
            &[],
            MorningStar
        ),
        ind!(
            "evening_star",
            "Evening Star",
            Pattern,
            Overlay,
            false,
            out! {"evening_star":Marker},
            &[],
            &[],
            EveningStar
        ),
        ind!(
            "morning_doji_star",
            "Morning Doji Star",
            Pattern,
            Overlay,
            false,
            out! {"morning_doji_star":Marker},
            &[],
            &[],
            MorningDojiStar
        ),
        ind!(
            "evening_doji_star",
            "Evening Doji Star",
            Pattern,
            Overlay,
            false,
            out! {"evening_doji_star":Marker},
            &[],
            &[],
            EveningDojiStar
        ),
        ind!(
            "three_white_soldiers",
            "Three White Soldiers",
            Pattern,
            Overlay,
            false,
            out! {"three_white_soldiers":Marker},
            &[],
            &[],
            ThreeWhiteSoldiers
        ),
        ind!(
            "three_black_crows",
            "Three Black Crows",
            Pattern,
            Overlay,
            false,
            out! {"three_black_crows":Marker},
            &[],
            &[],
            ThreeBlackCrows
        ),
        ind!(
            "identical_three_crows",
            "Identical Three Crows",
            Pattern,
            Overlay,
            false,
            out! {"identical_three_crows":Marker},
            &[],
            &[],
            IdenticalThreeCrows
        ),
        ind!(
            "three_inside",
            "Three Inside",
            Pattern,
            Overlay,
            false,
            out! {"three_inside":Marker},
            &[],
            &[],
            ThreeInside
        ),
        ind!(
            "three_outside",
            "Three Outside",
            Pattern,
            Overlay,
            false,
            out! {"three_outside":Marker},
            &[],
            &[],
            ThreeOutside
        ),
        ind!(
            "three_line_strike",
            "Three-Line Strike",
            Pattern,
            Overlay,
            false,
            out! {"three_line_strike":Marker},
            &[],
            &[],
            ThreeLineStrike
        ),
        ind!(
            "three_stars_in_south",
            "Three Stars in the South",
            Pattern,
            Overlay,
            false,
            out! {"three_stars_in_south":Marker},
            &[],
            &[],
            ThreeStarsInSouth
        ),
        ind!(
            "abandoned_baby",
            "Abandoned Baby",
            Pattern,
            Overlay,
            false,
            out! {"abandoned_baby":Marker},
            &[],
            &[],
            AbandonedBaby
        ),
        ind!(
            "advance_block",
            "Advance Block",
            Pattern,
            Overlay,
            false,
            out! {"advance_block":Marker},
            &[],
            &[],
            AdvanceBlock
        ),
        ind!(
            "stalled_pattern",
            "Stalled Pattern",
            Pattern,
            Overlay,
            false,
            out! {"stalled_pattern":Marker},
            &[],
            &[],
            StalledPattern
        ),
        ind!(
            "two_crows",
            "Two Crows",
            Pattern,
            Overlay,
            false,
            out! {"two_crows":Marker},
            &[],
            &[],
            TwoCrows
        ),
        ind!(
            "upside_gap_two_crows",
            "Upside Gap Two Crows",
            Pattern,
            Overlay,
            false,
            out! {"upside_gap_two_crows":Marker},
            &[],
            &[],
            UpsideGapTwoCrows
        ),
        ind!(
            "tristar",
            "Tristar",
            Pattern,
            Overlay,
            false,
            out! {"tristar":Marker},
            &[],
            &[],
            Tristar
        ),
        ind!(
            "unique_three_river",
            "Unique Three River",
            Pattern,
            Overlay,
            false,
            out! {"unique_three_river":Marker},
            &[],
            &[],
            UniqueThreeRiver
        ),
        ind!(
            "stick_sandwich",
            "Stick Sandwich",
            Pattern,
            Overlay,
            false,
            out! {"stick_sandwich":Marker},
            &[],
            &[],
            StickSandwich
        ),
        ind!(
            "ladder_bottom",
            "Ladder Bottom",
            Pattern,
            Overlay,
            false,
            out! {"ladder_bottom":Marker},
            &[],
            &[],
            LadderBottom
        ),
        ind!(
            "concealing_baby_swallow",
            "Concealing Baby Swallow",
            Pattern,
            Overlay,
            false,
            out! {"concealing_baby_swallow":Marker},
            &[],
            &[],
            ConcealingBabySwallow
        ),
        ind!(
            "rise_fall_three_methods",
            "Rising/Falling Three Methods",
            Pattern,
            Overlay,
            false,
            out! {"rise_fall_three_methods":Marker},
            &[],
            &[],
            RiseFallThreeMethods
        ),
        ind!(
            "mat_hold",
            "Mat Hold",
            Pattern,
            Overlay,
            false,
            out! {"mat_hold":Marker},
            &[],
            &[],
            MatHold
        ),
        ind!(
            "hikkake",
            "Hikkake",
            Pattern,
            Overlay,
            false,
            out! {"hikkake":Marker},
            &[],
            &[],
            Hikkake
        ),
        ind!(
            "hikkake_mod",
            "Modified Hikkake",
            Pattern,
            Overlay,
            false,
            out! {"hikkake_mod":Marker},
            &[],
            &[],
            HikkakeMod
        ),
        ind!(
            "xside_gap_three_methods",
            "Up/Down-side Gap Three Methods",
            Pattern,
            Overlay,
            false,
            out! {"xside_gap_three_methods":Marker},
            &[],
            &[],
            XsideGapThreeMethods
        ),
        ind!(
            "breakaway",
            "Breakaway",
            Pattern,
            Overlay,
            false,
            out! {"breakaway":Marker},
            &[],
            &[],
            Breakaway
        ),
        // ---- price transforms (price.py) — no params, no warm-up ----
        IndicatorMeta {
            name: "avgprice",
            pretty: "Average Price",
            category: Price,
            kind: Overlay,
            outputs: out! {"avgprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Avgprice>,
            params: &[],
            make_with: |_| Box::new(Avgprice::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "medprice",
            pretty: "Median Price",
            category: Price,
            kind: Overlay,
            outputs: out! {"medprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Medprice>,
            params: &[],
            make_with: |_| Box::new(Medprice::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "typprice",
            pretty: "Typical Price",
            category: Price,
            kind: Overlay,
            outputs: out! {"typprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Typprice>,
            params: &[],
            make_with: |_| Box::new(Typprice::new()),
            factory: None,
        },
        IndicatorMeta {
            name: "wclprice",
            pretty: "Weighted Close",
            category: Price,
            kind: Overlay,
            outputs: out! {"wclprice":Line},
            bands: &[],
            batch_only: false,
            make: mk::<Wclprice>,
            params: &[],
            make_with: |_| Box::new(Wclprice::new()),
            factory: None,
        },
    ]
}

/// The full indicator set, built once.
pub fn registry() -> &'static [IndicatorMeta] {
    static R: OnceLock<Vec<IndicatorMeta>> = OnceLock::new();
    R.get_or_init(build_registry)
}

/// Look up an indicator's metadata by registry name.
pub fn get(name: &str) -> Option<&'static IndicatorMeta> {
    registry().iter().find(|s| s.name == name)
}

/// Construct a fresh streaming instance by registry name.
pub fn make(name: &str) -> Option<Box<dyn Indicator>> {
    get(name).map(IndicatorMeta::build)
}

/// Construct a fresh streaming instance by registry name with an explicit raw
/// parameter slice (coerced internally by the entry's constructor). The T8
/// dialog's entry point.
pub fn make_with(name: &str, raw: &[f64]) -> Option<Box<dyn Indicator>> {
    get(name).map(|m| m.build_with(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::Bar;

    /// A stand-in for a compiled user prototype: it reports the ONE parameter it was constructed
    /// with, so a test can see WHICH constructor ran and WHAT the parameter arrived as.
    ///
    /// That is the whole point — the built-in `make`/`make_with` slots of a user row are
    /// [`unbuilt`], which panics, so any test below that reads a real number has proved the
    /// factory arm was taken rather than assumed it.
    #[derive(Clone)]
    struct EchoParam(f64);

    impl Indicator for EchoParam {
        fn on_bar(&mut self, _bar: &Bar) -> Vec<f64> {
            vec![self.0]
        }
        fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
            vec![bars.iter().map(|_| self.0).collect()]
        }
        fn value(&self) -> Vec<f64> {
            vec![self.0]
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "echo_param"
        }
        /// Deliberately parameter-DERIVED, so `IndicatorMeta::lookback` reading a real number
        /// proves it reached this instance (and therefore the factory) rather than a placeholder.
        fn lookback(&self) -> usize {
            self.0 as usize
        }
        fn lookback_exact(&self) -> bool {
            true
        }
    }

    fn echo_spec() -> ParamSpec {
        ParamSpec { name: "n", default: 3.0, min: 1.0, max: 100.0, step: 1.0 }
    }

    fn echo_meta(name: &str) -> &'static IndicatorMeta {
        IndicatorMeta::user(
            name,
            "Echo",
            RenderKind::Oscillator,
            vec![echo_spec()],
            &|raw: &[f64]| Box::new(EchoParam(raw.first().copied().unwrap_or(f64::NAN))),
        )
    }

    /// The claim the whole design rests on: adding `factory` left the BUILT-IN path alone.
    ///
    /// Non-vacuous because `build`/`build_with` branch on exactly this field — a built-in row that
    /// acquired a factory (or a user row that leaked into the closed catalog) would silently route
    /// the whole catalog through a different constructor than the one its parity fixtures were
    /// generated against, and this is the only place that would notice.
    #[test]
    fn no_builtin_row_carries_a_factory() {
        for m in registry() {
            assert!(m.factory.is_none(), "{} is a built-in and must have no factory", m.name);
            assert!(!m.is_user(), "{} must not report itself as user-written", m.name);
            assert_ne!(m.category, Category::User, "{} must not be filed under User", m.name);
        }
    }

    /// `IndicatorMeta::user` builds a row that is deliberately NOT in the closed built-in catalog.
    ///
    /// Non-vacuous: `user` could just as easily have pushed into `registry()`'s `OnceLock`, which
    /// is precisely what would collide with `vike_script::user_indicator_conflict`'s disjoint-name
    /// -space rule. `get` answering `None` is what keeps the two lookups separate.
    #[test]
    fn a_user_row_never_enters_the_builtin_registry() {
        let m = echo_meta("echo_not_in_registry");
        assert!(m.is_user());
        assert!(get("echo_not_in_registry").is_none(), "a user row must not be reachable via get");
        assert!(!registry().iter().any(|r| r.name == "echo_not_in_registry"));
    }

    /// `build_with` takes the FACTORY arm for a user row.
    ///
    /// Non-vacuous in the strongest available way: the built-in arm for a user row is [`unbuilt`],
    /// which panics. Reverting `build_with` to `(self.make_with)(raw)` does not make this test
    /// fail on a wrong number — it aborts the test binary.
    #[test]
    fn build_with_routes_a_user_row_through_its_factory() {
        let m = echo_meta("echo_build_with");
        assert_eq!(m.build_with(&[7.0]).value(), vec![7.0]);
        // ...and `build` (no params) is the same arm at the declared default.
        assert_eq!(m.build().value(), vec![3.0]);
    }

    /// A [`UserFactory`] is handed COERCED params, never the caller's raw slice — the promise
    /// `build_with`'s doc makes, so a script author's constructor need not re-implement `coerce`.
    ///
    /// Non-vacuous: with `f(raw)` in place of `f(&coerce(self.params, raw))` the first assert reads
    /// back `NaN` and the second `500.0`, both of which the factory would have passed straight to
    /// the indicator.
    #[test]
    fn a_user_factory_sees_coerced_params_not_the_raw_slice() {
        let m = echo_meta("echo_coerce");
        assert_eq!(m.build_with(&[f64::NAN]).value(), vec![3.0], "NaN falls back to the default");
        assert_eq!(m.build_with(&[500.0]).value(), vec![100.0], "out of range clamps to max");
        assert_eq!(m.build_with(&[]).value(), vec![3.0], "a missing entry takes the default");
    }

    /// The four derived-fact methods construct through `build`/`build_with` too, so they answer
    /// about the USER's indicator rather than about a placeholder.
    ///
    /// Non-vacuous the same way as above — each of these was `(self.make…)` before, which for a
    /// user row is [`unbuilt`]'s panic. `EchoParam::lookback` is param-derived, so the numbers
    /// also prove the params reached the instance.
    #[test]
    fn the_derived_facts_of_a_user_row_come_from_its_factory() {
        let m = echo_meta("echo_derived");
        assert_eq!(m.lookback(&[9.0]), 9);
        assert_eq!(m.lookback_full(&[9.0]), 9, "the trait default forwards to lookback");
        assert_eq!(m.lookback(&[]), 3, "no params -> the declared default");
        assert!(m.lookback_exact());
        assert!(!m.warmup_path_dependent());
    }

    /// A user row's descriptor is single-output and named after the indicator — the shape
    /// `vike_chart::indicators::Active` builds one `OutputLine` from, and the shape
    /// `vike_script::RhaiIndicator` enforces by REFUSING a multi-line return.
    #[test]
    fn a_user_row_is_single_output_named_for_the_indicator() {
        let m = echo_meta("echo_shape");
        assert_eq!(m.outputs.len(), 1);
        assert_eq!(m.outputs[0].name, "echo_shape");
        assert_eq!(m.outputs[0].style, OutputStyle::Line);
        assert!(m.bands.is_empty());
        assert!(!m.batch_only);
        assert_eq!(m.category, Category::User);
        assert_eq!(m.pretty, "Echo");
    }
}
