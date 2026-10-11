//! Discovery registry — name → metadata + fresh constructor, plus the two
//! orthogonal axes: pick-grouping [`Category`] vs render placement [`RenderKind`]/
//! [`OutputStyle`]. Mirrors the `registry()`/`get()` seam of vike-trader-app
//! `core/indicators/base.py` (Qt-free: no colour/paint state — that stays in the GUI).

// Every indicator struct is pub-re-exported from `crate::indicators`; glob-import
// them (171+ types) rather than maintain an explicit list.
use crate::Indicator;
use crate::indicators::*;
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
    /// Every variant, in declaration order — `User` included, although no row of [`registry`]
    /// ever carries it. NOT complete by construction: the enum is hand-written above, so
    /// `crates/vike-indicators/src/registry/tests.rs`'s
    /// `category_all_is_every_variant_in_declaration_order` holds it, with an exhaustive `match`
    /// that stops compiling when a variant is added.
    pub const ALL: &'static [Category] = &[
        Category::Overlap,
        Category::Momentum,
        Category::Volatility,
        Category::Volume,
        Category::Statistics,
        Category::Pattern,
        Category::Price,
        Category::Structure,
        Category::User,
    ];

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
    /// `crates/vike-script/src/engine/user.rs`'s `register_user_indicators`, which leaks each generated
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

// A candlestick-pattern row. Every pattern is the same row apart from its name, its label and its
// type: a paramless signal marker on the price pane (`Pattern`, `Overlay`, not `batch_only`, no
// bands) whose ONE output is named for the indicator. `registry/patterns.rs` holds all 63 of them
// and there is no exception, so a pattern that ever needs a different shape is written as an
// `ind!` row instead, not bent into this one.
macro_rules! pattern {
    ($name:literal, $pretty:literal, $ty:ty) => {
        ind!($name, $pretty, Pattern, Overlay, false, out! {$name:Marker}, &[], &[], $ty)
    };
}

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

mod base;
mod momentum;
mod overlap;
mod patterns;
mod price;
mod statistics;
mod structure;
mod volatility;
mod volume;

fn build_registry() -> Vec<IndicatorMeta> {
    let mut rows = base::rows();
    rows.extend(momentum::rows());
    rows.extend(volatility::rows());
    rows.extend(overlap::rows());
    rows.extend(volume::rows());
    rows.extend(statistics::rows());
    rows.extend(structure::rows());
    rows.extend(patterns::rows());
    rows.extend(price::rows());
    rows
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
mod tests;
