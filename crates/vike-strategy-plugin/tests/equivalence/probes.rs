//! The cheap probes: no cargo, no plugin — each witnesses a claim the comparison cannot.

use std::path::PathBuf;

use super::diffs::trade_diffs;
use super::drive::{Lane, run_compiled, run_compiled_with};
use super::series::bar_series;
use super::vacuity::assert_not_vacuous;
use super::{
    FIXTURE_SOURCE, N_BARS, NO_CONTROLLER_PARAMS, NO_INDICATOR_PARAMS, PARAMS_TOML, ma_cross,
};

/// The compiled-in half is only the build-time tier's mechanism if the tier still reaches a user
/// file that way. This asserts it against the REAL renderer rather than against this file's
/// memory of it — costs no cargo, so it gates in an ordinary run.
#[test]
fn generated_registry_still_reaches_a_user_file_as_a_path_module() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vike-user-strategies")
        .join("tests")
        .join("fixture_user_data");
    let outcome = vike_user_strategies::codegen::scan(&fixtures);
    assert!(
        outcome.errors.is_empty(),
        "the committed fixture tree must scan clean: {:?}",
        outcome.errors
    );
    assert!(!outcome.strategies.is_empty(), "the committed fixture tree must hold strategies");
    let rendered = vike_user_strategies::codegen::render(&outcome.strategies);
    assert!(
        rendered.contains("#[path = \""),
        "the build-time tier must still compile a user file in as a `#[path]` MODULE — the \
         mechanism `tests/equivalence.rs` reproduces for its compiled-in half:\n{rendered}"
    );
    assert!(
        rendered.contains("::build::<B>(params)"),
        "the build-time tier must still reach the entry file's generic `build` directly:\n{rendered}"
    );
}

/// The equivalence comparison proves the two mechanisms AGREE. It cannot, by construction, prove
/// that the widened API surface is reached at all: a fixture that merely `use`d `vike_indicators`
/// without letting it decide anything would agree across both mechanisms for the same reason two
/// no-op trait defaults agree, and the manifest change underneath would be untested.
///
/// ⚠ **This is the shape the design's *Testing* section calls "a test that performs for the
/// system the step it was meant to witness", and it has produced six of them in this project.**
/// The witness that is not one: run the COMPILED half twice, differing in exactly the one params
/// key that switches the catalog indicator off, and require the two trade lists to DIFFER. If the
/// indicator is inert — never advanced, never resolved, dead-code-eliminated — the two runs are
/// identical and this goes red.
///
/// It costs no cargo and no `.so`, so it gates every PR. The link to the PLUGIN half is the
/// equivalence test: the compiled half's numbers now depend on `vike_indicators`, and the plugin
/// half is required to reproduce them bit for bit.
#[test]
fn the_indicator_catalog_changes_what_the_fixture_trades() {
    assert_a_params_switch_changes_the_trade_list(
        NO_INDICATOR_PARAMS,
        "the `vike_indicators` CATALOG",
        "rsi_period",
    );
}

/// The `vike_strategy` half of the same argument, switched by the presence of the `[controller]`
/// table. See [`the_indicator_catalog_changes_what_the_fixture_trades`] for why a differential is
/// the honest witness here and a `use` line is not.
#[test]
fn the_controller_framework_changes_what_the_fixture_trades() {
    assert_a_params_switch_changes_the_trade_list(
        NO_CONTROLLER_PARAMS,
        "the `vike_strategy` CONTROLLER framework",
        "[controller]",
    );
}

/// Both lanes, because the two emit different hooks and a contribution live on only one of them
/// is a fact worth failing over rather than averaging away.
fn assert_a_params_switch_changes_the_trade_list(off_params: &str, what: &str, key: &str) {
    for lane in [Lane::Bars, Lane::Ticks] {
        let on = run_compiled(lane);
        let off = run_compiled_with(off_params, lane);
        assert_not_vacuous(&on, lane);
        assert_not_vacuous(&off, lane);
        let diffs = trade_diffs(&on.result.trades, &off.result.trades);
        assert!(
            !diffs.is_empty(),
            "[{}] turning `{key}` OFF changed NOTHING about what the fixture traded, so {what} \
             is not reaching a decision in this fixture. The equivalence comparison would still \
             pass — two mechanisms can agree perfectly about a crate neither of them uses — and \
             the template's widened dependency table would be proven by nothing. Make the \
             contribution decide a size or a direction, do not relax this.",
            lane.label()
        );
    }
}

/// The catalog indicator must produce a reading that actually MOVES the multiplier over this
/// fixture's own series. A differential can be satisfied by a single warm-up bar's worth of
/// difference; this says the contribution is live across the run.
///
/// It calls the fixture's own [`ma_cross::make_rsi`] and [`ma_cross::rsi_scale`] rather than
/// re-typing either — the [`ma_cross::top_of_book_bias`] precedent, and for the same reason: a
/// re-typed copy drifts until the probe asserts arithmetic the strategy does not perform.
#[test]
fn the_catalog_indicator_resolves_and_its_reading_moves_the_size_multiplier() {
    // Read out of the params DOCUMENT rather than restated as a second constant: the document is
    // what both mechanisms are actually handed, and a literal here would be free to drift from it.
    let doc: toml::Value = toml::from_str(PARAMS_TOML).expect("fixture params must be valid TOML");
    let period = doc
        .get("rsi_period")
        .and_then(toml::Value::as_integer)
        .expect("the params document must carry `rsi_period`") as usize;
    let mut ind =
        ma_cross::make_rsi(period).expect("a non-zero period must resolve a catalog indicator");
    let mut scales: Vec<f64> = Vec::new();
    for bar in bar_series() {
        let v = ind.on_bar(&bar).first().copied().unwrap_or(f64::NAN);
        if v.is_finite() {
            scales.push(ma_cross::rsi_scale(v));
        }
    }
    assert!(
        scales.len() > N_BARS / 2,
        "the catalog indicator warmed on only {} of {N_BARS} bars — a reading that is NaN for \
         most of the run leaves `rsi_scale` at 1.0 and makes the surface it stands for nearly \
         inert",
        scales.len()
    );
    let (lo, hi) = scales.iter().fold((f64::MAX, f64::MIN), |(l, h), &s| (l.min(s), h.max(s)));
    assert!(
        lo < 1.0 && hi > 1.0,
        "the indicator's multiplier never crossed 1.0 in BOTH directions over this series \
         (min {lo}, max {hi}). One-sided or constant, it scales every order the same way and \
         says far less about whether the catalog is genuinely driving the fixture."
    );
    println!("MEASURED rsi size multiplier over {N_BARS} bars: min {lo}, max {hi}");
}

/// The fixture must not be a buy-and-hold, on EITHER lane. Same guard as the one inside the
/// equivalence test, broken out so a fixture edit that quietly made the comparison vacuous fails
/// FAST and in an ordinary run, without waiting for a cargo build.
#[test]
fn the_fixture_trades_both_directions_and_is_not_a_buy_and_hold() {
    for lane in [Lane::Bars, Lane::Ticks] {
        assert_not_vacuous(&run_compiled(lane), lane);
    }
}

/// Every hook `PluginVTable` carries must be one the FIXTURE actually overrides.
///
/// ⚠ **This is the guard against the failure this project has produced four times: a test that
/// performs for the system the very step it is meant to witness.** Wiring a hook and then not
/// exercising it leaves the equivalence comparison agreeing for a reason that has nothing to do
/// with that hook — both mechanisms run the trait's no-op default and agree perfectly. A hook
/// added to `WIRED_HOOKS` without a matching override in the fixture therefore fails HERE, by
/// name, rather than being quietly carried as covered.
///
/// Text over the fixture's own committed bytes, the same bytes both mechanisms compile — no
/// cargo, no reflection. It proves the OVERRIDE EXISTS; that each override changes behaviour is
/// what the fixture's own doc table and `assert_not_vacuous`'s hook half are for, and what the
/// bit-for-bit comparison then measures.
#[test]
fn the_fixture_overrides_every_hook_the_vtable_carries() {
    // ⚠ Over the fixture's CODE, not its text. A raw `contains` is satisfied by a hook named in a
    // comment — including the doc table above each override, which names every one of them — so
    // an override commented out during debugging and left that way would keep this green while
    // the comparison compared two no-ops. Comments are blanked first; a `//` inside a string
    // literal would truncate that line, which can only cause a FALSE FAILURE (loud, and the safe
    // direction) and which this fixture contains none of.
    let code = strip_comments(FIXTURE_SOURCE);
    let missing: Vec<&&str> = vike_strategy_plugin::host::WIRED_HOOKS
        .iter()
        .filter(|hook| !code.contains(&format!("fn {hook}(")))
        .collect();
    assert!(
        missing.is_empty(),
        "`vike_strategy_plugin::host::WIRED_HOOKS` names {missing:?}, which the equivalence \
         fixture (tests/fixtures/equivalence/ma_cross.rs) does not override. A wired hook the \
         fixture never implements is a hook this test suite COMPARES two no-ops for: both \
         mechanisms fall through to the trait default and agree perfectly, so the comparison \
         reads green while saying nothing. Add an override that CHANGES BEHAVIOUR (see that \
         file's own table of what each one puts at stake), or — if the hook genuinely cannot be \
         driven — say so where the narrower witness lives."
    );
}

/// The spy in [`assert_the_engine_emits_the_state_dependent_hooks`] must FORWARD exactly the hooks
/// `StrategyEngine` emits — no more, no fewer.
///
/// ⚠ **Its forwarding roster is hand-written, and a hand-written roster rots.** If `Strategy`,
/// `engine.rs` and the fixture all grew a tenth hook, the spy would silently swallow it to the
/// trait's own no-op default: its counters would then describe a DIFFERENT run from the one the
/// comparison compares, while every assertion stayed green. That is the roster-rot shape this repo
/// gates everywhere else, and the probe is worth nothing the moment its subject drifts from the
/// comparison's.
///
/// So the two sets are DERIVED from two independent sources and compared, the same shape
/// `tests/hook_roster.rs` uses on the trait: the engine's emitted set out of
/// `crates/vike-sim/src/engine.rs`'s own call sites, the spy's out of THIS file. Neither is
/// typed here, so neither can be quietly corrected into agreement.
///
/// ⚠ Scoped to `engine.rs` deliberately — that is the engine [`drive`] runs, both lanes of it.
/// A hook emitted only by `vector_engine.rs` reaches neither the probe nor the comparison and is
/// not this gate's subject.
#[test]
fn the_spy_forwards_exactly_the_hooks_the_engine_emits() {
    // Assembled from halves so this file's own scan cannot match the needle's own source — the
    // one trap a test that reads itself has, and the one `vike-studio-core`'s `plugin_run.rs`
    // records paying.
    let forward_prefix = concat!("self.", "inner.");

    let engine_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vike-sim")
        .join("src")
        .join("engine.rs");
    let engine =
        strip_comments(&std::fs::read_to_string(&engine_path).unwrap_or_else(|e| {
            panic!("cannot read the engine at {}: {e}", engine_path.display())
        }));
    let own = strip_comments(
        &[
            include_str!("../equivalence.rs"),
            include_str!("../common/mod.rs"),
            include_str!("diffs.rs"),
            include_str!("drive.rs"),
            include_str!("live_only.rs"),
            include_str!("probes.rs"),
            include_str!("series.rs"),
            include_str!("vacuity.rs"),
        ]
        .concat(),
    );

    // Candidates are the REAL trait's methods, so neither scan can invent a name out of a string
    // literal or an unrelated `strategy.` receiver.
    let every_hook: Vec<&str> = vike_strategy_plugin::host::WIRED_HOOKS
        .iter()
        .chain(vike_strategy_plugin::host::UNWIRED_HOOKS)
        .copied()
        .collect();

    let emitted: Vec<&str> =
        every_hook.iter().copied().filter(|h| engine.contains(&format!("strategy.{h}("))).collect();
    let forwarded: Vec<&str> = every_hook
        .iter()
        .copied()
        .filter(|h| own.contains(&format!("{forward_prefix}{h}(")))
        .collect();

    assert!(
        !emitted.is_empty(),
        "the engine scan found NO emitted hook, so this gate cannot fail for its stated reason. \
         `StrategyEngine`'s call sites are spelled `self.strategy.<hook>(` (and `strategy.warmup()` \
         in `new`); if that changed, this scan needs to change with it."
    );

    let missing: Vec<&&str> = emitted.iter().filter(|h| !forwarded.contains(h)).collect();
    assert!(
        missing.is_empty(),
        "`StrategyEngine` emits {missing:?}, which the probe's `HookSpy` does NOT forward — so it \
         swallows them to the trait's no-op default and its counters describe a different run from \
         the one the equivalence comparison compares, silently and greenly. Forward each one to \
         `inner` (count it too, if the probe should have an opinion about it)."
    );
    let extra: Vec<&&str> = forwarded.iter().filter(|h| !emitted.contains(h)).collect();
    assert!(
        extra.is_empty(),
        "`HookSpy` forwards {extra:?}, which `StrategyEngine` never emits. Harmless to run, but it \
         means this roster and the engine's have drifted — delete the forward, or (if the engine \
         genuinely grew the call site) confirm the scan above still sees it."
    );
}

// Unlike `crates/vike-boot/tests/common/mod.rs`'s `strip_comments` this also drops `/* */` blocks;
// both string-blind, unlike `crates/vike-ops/tests/common/strip.rs`'s `strip_comments_and_strings`.
/// Blank `//` line comments and `/* … */` blocks, keeping every other byte and every newline, so
/// a search over the result is a search over CODE.
///
/// Deliberately NOT string-literal aware, and the bias is chosen: a `//` inside a string truncates
/// that line, so the only thing this can get wrong is to HIDE code from the caller — which makes
/// the caller's assertion fail loudly rather than pass quietly. The opposite bias (treat
/// everything as code) is the one that reads a commented-out override as a live one.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_block = false;
    for line in src.lines() {
        let mut rest = line;
        loop {
            if in_block {
                match rest.find("*/") {
                    Some(i) => {
                        in_block = false;
                        rest = &rest[i + 2..];
                    }
                    None => {
                        rest = "";
                        break;
                    }
                }
            } else {
                // Whichever opener comes FIRST decides, and the match is written without a guard
                // on purpose: `match arms with guards don't count towards exhaustivity`, so the
                // guarded version compiled to a non-exhaustive match (E0004, caught on the lane).
                let opener = match (rest.find("//"), rest.find("/*")) {
                    (Some(l), Some(b)) => Some((l.min(b), b < l)),
                    (Some(l), None) => Some((l, false)),
                    (None, Some(b)) => Some((b, true)),
                    (None, None) => None,
                };
                match opener {
                    None => break,
                    Some((at, is_block)) => {
                        out.push_str(&rest[..at]);
                        if is_block {
                            in_block = true;
                            rest = &rest[at + 2..];
                        } else {
                            rest = "";
                            break;
                        }
                    }
                }
            }
        }
        out.push_str(rest);
        out.push('\n');
    }
    out
}

/// The blanking above is only worth anything if it can actually tell the two apart — the same
/// "a gate needs a non-empty input" rule every ratchet in `crates/vike-ops/tests` states.
#[test]
fn the_comment_blanker_hides_a_commented_out_override_and_keeps_a_live_one() {
    let src = "impl S {\n    fn on_fill(&mut self) {}\n    // fn on_mark(&mut self) {}\n    /* fn on_flow(&mut self) {} */\n}\n";
    let code = strip_comments(src);
    assert!(code.contains("fn on_fill("), "a LIVE override must survive: {code:?}");
    assert!(!code.contains("fn on_mark("), "a `//`-commented override must not: {code:?}");
    assert!(!code.contains("fn on_flow("), "a `/* */`-commented override must not: {code:?}");
    assert_eq!(
        code.lines().count(),
        src.lines().count(),
        "blanking must keep the line structure, so a future caller can report a line number"
    );
}
