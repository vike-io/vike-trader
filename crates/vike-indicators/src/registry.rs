//! Discovery registry: every built-in indicator by name, with its metadata and constructors.
//!
//! [`registry`] lists the catalog, [`get`] looks a row up by name, [`make`] / [`make_with`] build a
//! fresh streaming instance, [`compute`] runs one over a whole history, and [`lookback`] /
//! [`lookback_full`] report how many bars it needs to warm up. Each row is an [`IndicatorMeta`]:
//! its name, display label, picker [`Category`], where it draws ([`RenderKind`]), its output lines
//! ([`OutSpec`], with an [`OutputStyle`] each) and its parameters ([`ParamSpec`]: default and
//! allowed range). The metadata carries no colours or other paint state.
//
// Mirrors the `registry()`/`get()` seam of vike-trader-app `core/indicators/base.py` (Qt-free: no
// colour/paint state — that stays in the GUI).

// Every indicator struct is pub-re-exported from `crate::indicators`; glob-import
// them (171+ types) rather than maintain an explicit list.
use crate::Indicator;
use crate::indicators::*;
use std::sync::OnceLock;
use vike_marketdata::Bar;

/// The family an indicator belongs to, for grouping in a picker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    /// Trend-following overlays on the price pane (moving averages and the like). Its
    /// [`Category::label`] is `"Trend"`.
    Overlap,
    /// Momentum indicators and oscillators.
    Momentum,
    /// Volatility measures and bands.
    Volatility,
    /// Volume-based indicators.
    Volume,
    /// Statistical functions (regression, deviation, correlation and the like).
    Statistics,
    /// Candlestick patterns.
    Pattern,
    /// Price transforms (typical price, median price and the like).
    Price,
    /// Structure series (pivots, swings, fractals and the like).
    Structure,
    /// A user-written indicator ([`IndicatorMeta::user`]). It is not a family: it is the answer
    /// for an indicator the crate has never seen, so it is not filed under any of the families
    /// above. No row of [`registry`] carries it; it only appears on rows a caller builds with
    /// [`IndicatorMeta::user`].
    User,
}
impl Category {
    /// Every variant, in declaration order, `User` included although no row of [`registry`]
    /// carries it.
    //
    // NOT complete by construction: the enum is hand-written above, so
    // `crates/vike-indicators/src/registry/tests.rs`'s
    // `category_all_is_every_variant_in_declaration_order` holds it, with an exhaustive `match`
    // that stops compiling when a variant is added.
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

/// Where an indicator draws: on the price pane or in its own oscillator pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderKind {
    /// Drawn on the price pane.
    Overlay,
    /// Drawn in a separate pane below the price.
    Oscillator,
}

/// How a single output line is meant to be rendered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputStyle {
    /// A continuous line.
    Line,
    /// One edge of a band or channel.
    Band,
    /// Bars from a baseline.
    Histogram,
    /// Isolated dots.
    Dots,
    /// A per-bar candlestick-pattern signal (`+100` bullish, `-100` bearish, `0` none), not a
    /// price: it should be drawn as a marker on the flagged bar rather than plotted on the price
    /// axis. Only pattern indicators use it; structure series that carry real prices say what they
    /// are instead (`zigzag` is a `Line` through its pivots, `williams_fractal` is `Dots` at its
    /// fractal highs and lows).
    //
    // (`vike-chart`'s `render_overlay` anchors a glyph to the flagged bar's own extreme and keeps
    // the series out of the price-pane autofit; plotting it on the price axis is meaningless.)
    Marker,
}

/// One output line's name and render style. The order of an indicator's `OutSpec`s matches the
/// order of the lines `vectorize` and `on_bar` return.
pub struct OutSpec {
    /// The line's name (for example `"signal"` for MACD's second line).
    pub name: &'static str,
    /// How the line is meant to be drawn.
    pub style: OutputStyle,
}

/// One parameter's description: its display name, default and allowed range.
///
/// `default` is the value `make()` uses, so `make_with` given the defaults reproduces `make()`
/// bit for bit. `min` and `max` bound the accepted values (see [`coerce`]) and `step` is the
/// suggested increment for a slider or spinner. `name` is a display label only.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    /// Display name of the parameter (for example `"period"`).
    pub name: &'static str,
    /// The value used when the parameter is not given.
    pub default: f64,
    /// Smallest accepted value.
    pub min: f64,
    /// Largest accepted value.
    pub max: f64,
    /// Suggested increment for an input control.
    pub step: f64,
}

/// The constructor of a user-written indicator: a closure that builds a fresh instance from the
/// (already coerced) parameter values. It may capture state, which a bare `fn` pointer cannot.
///
/// It is `&'static` because the indicator it constructs lives for the whole process, and
/// `Send + Sync` because [`IndicatorMeta`] is shared across threads.
//
// `&'static` rather than `Arc` because the thing it constructs is process-lifetime by
// nature — installed once at startup beside the built-ins, exactly like [`registry`]
// itself — so refcounting it would buy nothing and cost a clone on every construction.
// (The prototype it clones is a script's engine + AST + initial state.)
pub type UserFactory = &'static (dyn Fn(&[f64]) -> Box<dyn Indicator> + Send + Sync);

/// Static description of an indicator, and the means to build a fresh streaming instance of it.
///
/// Rows of [`registry`] describe the built-in indicators; [`IndicatorMeta::user`] builds a row for
/// an indicator you supply. Construct an instance with [`IndicatorMeta::build`] (default
/// parameters) or [`IndicatorMeta::build_with`] (explicit parameters); the constructor fields are
/// private so that there is one way to build and a user-supplied constructor is always handed
/// coerced parameters. [`IndicatorMeta::is_user`] says which kind of row this is.
//
// ⚠ **Construct through [`IndicatorMeta::build`]/[`IndicatorMeta::build_with`], never from a
// field.** All THREE constructor fields below are PRIVATE for that reason: a bare `fn` cannot
// capture a compiled prototype, so a [`IndicatorMeta::user`] row parks placeholders in `make`/
// `make_with` and carries its real constructor in `factory`. A public constructor field would be
// a landmine — reachable, plausible-looking, and wrong for exactly the rows a caller is least
// likely to be thinking about — and that argument does not stop at the `fn` pointers: a public
// `factory` invites `(m.factory.unwrap())(raw)`, which skips the [`coerce`] that
// [`IndicatorMeta::build_with`] promises every [`UserFactory`] is handed. [`IndicatorMeta::is_user`]
// is the whole of what a caller needs to READ. The three methods pick the right constructor;
// nothing else can pick the wrong one.
pub struct IndicatorMeta {
    /// The registry key (for example `"macd"`).
    pub name: &'static str,
    /// Human-readable label for a picker or legend (for example `"Bollinger %B"`).
    pub pretty: &'static str,
    /// The family the indicator is grouped under.
    pub category: Category,
    /// Whether it draws on the price pane or in its own pane.
    pub kind: RenderKind,
    /// The output lines, in the order `vectorize` and `on_bar` return them.
    pub outputs: &'static [OutSpec],
    /// Horizontal reference levels worth drawing in the pane (for example `30.0`, `50.0` and `70.0`
    /// for RSI); empty when there are none.
    pub bands: &'static [f64],
    /// `true` when the streaming `on_bar` path cannot equal `vectorize` because the batch reads
    /// future bars (Ichimoku's chikou line, `zigzag`, `williams_fractal`). For these `vectorize`
    /// is authoritative and `on_bar` is a causal best effort (`NaN` at the tip that cannot be
    /// known yet). Every other indicator sets `false`.
    pub batch_only: bool,
    // Built-in default constructor. Private — see the type doc; [`IndicatorMeta::build`].
    make: fn() -> Box<dyn Indicator>,
    /// The indicator's parameters, in the order the slice given to
    /// [`IndicatorMeta::build_with`] is read. Empty for indicators without parameters (`vwap`,
    /// `obv`).
    pub params: &'static [ParamSpec],
    // Built-in parameterised constructor (each registry entry runs its slice through [`coerce`]
    // internally). Private — see the type doc; [`IndicatorMeta::build_with`].
    make_with: fn(&[f64]) -> Box<dyn Indicator>,
    // `Some` only for a [`IndicatorMeta::user`] row. When set it is the AUTHORITY: `build`/
    // `build_with` consult it and never touch `make`/`make_with`. `None` on every [`registry`]
    // row, which is what keeps the built-in path byte-identical — and is machine-checked by
    // `tests::no_builtin_row_carries_a_factory`, so the claim cannot rot into a comment.
    //
    // Private, like its two siblings — see the type doc. [`IndicatorMeta::is_user`] is the
    // read-only view of it, and the only one a caller outside this module has ever needed.
    factory: Option<UserFactory>,
}

impl IndicatorMeta {
    /// A fresh streaming instance at this indicator's default parameters.
    //
    // For a built-in that is `Default::default()` verbatim — the pre-`factory` expression,
    // unchanged, so nothing about a registry indicator moved. For a user row it is the factory at
    // the declared defaults (`coerce` of an empty slice).
    pub fn build(&self) -> Box<dyn Indicator> {
        match self.factory {
            Some(f) => f(&coerce(self.params, &[])),
            None => (self.make)(),
        }
    }

    /// A fresh streaming instance at `raw` parameters, read in the order of [`IndicatorMeta::params`].
    ///
    /// The values are first run through [`coerce`]: missing and `NaN` entries take the default and
    /// the rest are clamped to the allowed range, so a user-supplied [`UserFactory`] is always
    /// handed clean values.
    //
    // Both arms see COERCED values, but by different routes, which is worth knowing: a built-in
    // entry's `make_with` runs [`coerce`] itself (it always has), while the user arm coerces here
    // — a [`UserFactory`] is handed already-clean params so a script author's constructor never
    // has to re-implement the NaN/missing/clamp rules.
    pub fn build_with(&self, raw: &[f64]) -> Box<dyn Indicator> {
        match self.factory {
            Some(f) => f(&coerce(self.params, raw)),
            None => (self.make_with)(raw),
        }
    }

    /// `true` when this row describes a user-supplied indicator (built by [`IndicatorMeta::user`])
    /// rather than one of the built-ins.
    //
    // Derived from the private `factory` rather than carried as its own flag, so it cannot
    // disagree with which constructor `build` actually uses — the picker's "·user" tag and the
    // thing that gets constructed are then the same fact.
    pub fn is_user(&self) -> bool {
        self.factory.is_some()
    }

    /// Describes a user-supplied indicator and returns the descriptor with a `'static` lifetime.
    ///
    /// `name` is the key the indicator is known by and also the name of its single output line;
    /// `pretty` is the display label; `kind` says where it draws; `params` describes its
    /// parameters; `factory` builds a fresh instance from the coerced parameter values. The
    /// output is one [`OutputStyle::Line`], the category is [`Category::User`] and the row is not
    /// `batch_only`.
    ///
    /// The descriptor is leaked on purpose, so call this once at start-up, not per frame. It is
    /// not added to [`registry`], which stays the closed set of built-ins; keep the returned
    /// reference yourself.
    //
    // Leaking is the honest lifetime, not a shortcut: a user indicator is installed once per
    // process at startup and lives until exit, exactly like [`registry`]'s own `OnceLock`, so
    // there is no point at which a free would be correct. The idiom is already this tree's — see
    // `crates/vike-script/src/engine/user.rs`'s `register_user_indicators`, which leaks each generated
    // accessor name for the same reason. ⚠ It is therefore a STARTUP call: leaking per chart
    // frame would be an unbounded leak.
    //
    // `name` doubles as the persistence key (a workspace stores studies by name) and as the sole
    // output line's name. `params` is the script's own declared knob surface.
    //
    // Deliberately NOT inserted into [`registry`]: that catalog is the closed, parity-gated set of
    // built-ins, and `vike_script::user_indicator_conflict` refuses a user file that would shadow
    // one *precisely* so the two name spaces stay disjoint. A user row reaches a caller through a
    // second registry it opts into, never through `get`.
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

    /// Warm-up depth for this indicator at `raw` parameters (coerced as in
    /// [`IndicatorMeta::build_with`]): the index of the first bar at which `vectorize` yields a
    /// non-NaN value on any output line. See [`Indicator::lookback`]: exact when
    /// [`IndicatorMeta::lookback_exact`] is `true`, otherwise a conservative lower bound (`0`).
    /// Pass `&[]` for the defaults.
    pub fn lookback(&self, raw: &[f64]) -> usize {
        self.build_with(raw).lookback()
    }

    /// Full warm-up depth at `raw` parameters: the first index at which every output line is
    /// non-NaN, which is the number to size a seed history with. Always
    /// `>=` [`IndicatorMeta::lookback`]. See [`Indicator::lookback_full`].
    pub fn lookback_full(&self, raw: &[f64]) -> usize {
        self.build_with(raw).lookback_full()
    }

    /// `true` when [`IndicatorMeta::lookback`] is a real, parameter-derived value rather than the
    /// conservative `0` default.
    pub fn lookback_exact(&self) -> bool {
        self.build().lookback_exact()
    }

    /// `true` when this indicator is path-dependent: its `lookback()` of `0` is truthful, but a
    /// caller seeding that few bars is not warm. See [`Indicator::warmup_path_dependent`].
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

/// Warm-up depth by registry name and raw parameters, for callers that only hold a name (for
/// example to size a seed history). `None` for an unknown name. See [`Indicator::lookback`] for
/// the contract.
pub fn lookback(name: &str, raw: &[f64]) -> Option<usize> {
    get(name).map(|m| m.lookback(raw))
}

/// Full warm-up depth by registry name and raw parameters (every output line valid), the number
/// to size a seed history with. `None` for an unknown name. See [`Indicator::lookback_full`].
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

/// Coerce a raw parameter slice against `specs`: a missing entry (`raw` shorter than `specs`)
/// becomes the spec's `default`, a `NaN` becomes the `default`, and every value is then clamped to
/// `[min, max]`. Entries of `raw` beyond `specs.len()` are ignored. The result has one value per
/// spec. [`make_with`] and [`IndicatorMeta::build_with`] apply this before constructing an
/// indicator.
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
mod oscillators;
mod overlap;
mod overlays;
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
    rows.extend(overlays::rows());
    rows.extend(oscillators::rows());
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

/// Construct a fresh streaming instance by registry name with an explicit raw parameter slice,
/// read in the order of [`IndicatorMeta::params`] and coerced (see [`coerce`]): missing or `NaN`
/// entries take the default and out-of-range values are clamped. `None` for an unknown name. To
/// have an out-of-range value refused instead, use the typed constructors in [`crate::indicators`].
pub fn make_with(name: &str, raw: &[f64]) -> Option<Box<dyn Indicator>> {
    get(name).map(|m| m.build_with(raw))
}

/// Run the registry indicator `name` over `bars` in one call and return each output line with its
/// name, in the row's `outputs` order: `compute("macd", &[], &bars)` is
/// `[("macd", ..), ("signal", ..), ("hist", ..)]`, each series as long as `bars` with `NaN` while
/// warming up. `raw` is the parameter slice of [`make_with`] (clamped the same way; `&[]` means
/// the defaults). `None` for an unknown name. This is [`Indicator::vectorize`] plus the names.
pub fn compute(name: &str, raw: &[f64], bars: &[Bar]) -> Option<Vec<(&'static str, Vec<f64>)>> {
    let meta = get(name)?;
    let lines = meta.build_with(raw).vectorize(bars);
    Some(meta.outputs.iter().map(|out| out.name).zip(lines).collect())
}

#[cfg(test)]
mod tests;
