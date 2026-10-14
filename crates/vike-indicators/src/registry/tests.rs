//! Registry tests — user-row / factory construction, the closed built-in catalog, and
//! [`Category::ALL`]'s completeness.
use super::*;
use vike_marketdata::Bar;

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
/// `vike_chart::Active` builds one `OutputLine` from, and the shape
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

/// [`Category::ALL`] holds every variant exactly once, at its declaration index.
///
/// The guard is the `match` that `arms!` expands to: it has no wildcard, so a variant added to
/// the enum stops this file compiling until it gets an arm here. Every arm also lands in `ARMS`,
/// so nothing counts the arms by hand, and the asserts then demand the new variant in `ALL` at
/// the index its arm gives it. The arm index must be the declaration index (`k as usize`), so
/// reordering `ALL` together with the arms still fails.
#[test]
fn category_all_is_every_variant_in_declaration_order() {
    macro_rules! arms {
        ($($variant:ident => $i:literal),+ $(,)?) => {
            fn idx(k: Category) -> usize {
                match k {
                    $(Category::$variant => $i,)+
                }
            }
            const ARMS: &[Category] = &[$(Category::$variant),+];
        };
    }
    arms! {
        Overlap => 0,
        Momentum => 1,
        Volatility => 2,
        Volume => 3,
        Statistics => 4,
        Pattern => 5,
        Price => 6,
        Structure => 7,
        User => 8,
    }
    assert_eq!(Category::ALL.len(), ARMS.len(), "ALL has one entry per match arm");
    for &k in ARMS {
        assert_eq!(idx(k), k as usize, "{k:?}'s arm index is its declaration index");
        assert_eq!(Category::ALL.get(idx(k)), Some(&k), "{k:?} belongs at index {} of ALL", idx(k));
    }
}
