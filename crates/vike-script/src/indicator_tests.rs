use super::*;
use vike_indicators::BoxedClone;

fn bars(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, &c)| Bar {
            ts: i as i64 * 60_000,
            open: c,
            high: c + 1.0,
            low: c - 1.0,
            close: c,
            volume: 100.0 + i as f64,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

const RUNNING_MEAN: &str = r#"
        let lookback = 3;
        fn init() { #{ buf: [] } }
        fn warmup() { 2 }
        fn on_bar(bar) {
            this.buf.push(bar.close);
            if this.buf.len() > lookback { this.buf.remove(0); }
            if this.buf.len() < lookback { return (); }
            let s = 0.0;
            for v in this.buf { s += v; }
            s / lookback
        }
    "#;

#[test]
fn state_persists_across_bars_and_the_recurrence_is_the_authors_own() {
    let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
    let out: Vec<f64> = bars(&[1.0, 2.0, 3.0, 4.0]).iter().map(|b| ind.on_bar(b)[0]).collect();
    assert!(out[0].is_nan() && out[1].is_nan(), "warm-up reads as NaN: {out:?}");
    assert_eq!(out[2], 2.0);
    assert_eq!(out[3], 3.0);
}

/// The property the whole design rests on. It is true BY CONSTRUCTION (`vectorize` folds
/// `on_bar`), and this is what keeps that construction from being quietly replaced by a second
/// implementation later — which is exactly how the built-ins' parity gate earns its keep.
#[test]
fn vectorize_is_bit_identical_to_the_streaming_fold() {
    let ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
    let bs = bars(&[1.0, 2.5, 3.25, 4.125, 9.0, 11.5, 2.0]);
    let batch = &ind.vectorize(&bs)[0];
    let mut s = ind.clone();
    for (i, b) in bs.iter().enumerate() {
        let v = s.on_bar(b)[0];
        assert_eq!(v.to_bits(), batch[i].to_bits(), "bar {i}: {v} != {}", batch[i]);
    }
}

/// `vectorize` takes `&self` and must not advance the receiver — a caller drawing a chart from
/// a live indicator would otherwise double-feed it.
#[test]
fn vectorize_does_not_advance_the_instance_it_was_called_on() {
    let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
    let bs = bars(&[1.0, 2.0, 3.0, 4.0]);
    for b in &bs[..3] {
        ind.on_bar(b);
    }
    let before = ind.value();
    let _ = ind.vectorize(&bs);
    assert_eq!(ind.value()[0].to_bits(), before[0].to_bits());
    // ...and the NEXT streamed bar continues from bar 3, not from a replayed history.
    assert_eq!(ind.on_bar(&bs[3])[0], 3.0);
}

#[test]
fn reset_returns_the_state_to_construction() {
    let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
    let bs = bars(&[1.0, 2.0, 3.0, 4.0]);
    for b in &bs {
        ind.on_bar(b);
    }
    ind.reset();
    assert!(ind.value()[0].is_nan());
    let after: Vec<f64> = bs.iter().map(|b| ind.on_bar(b)[0]).collect();
    assert!(after[0].is_nan() && after[1].is_nan());
    assert_eq!(after[2], 2.0, "a reset instance must warm up again from scratch");
}

/// A clone must stream independently — `BoxedClone` exists so a caller can evaluate the live
/// FORMING bar on a throwaway copy, and that is only safe if the copy is truly detached.
#[test]
fn a_clone_advances_without_touching_its_original() {
    let mut ind = compile_indicator("mean3", RUNNING_MEAN).unwrap();
    let bs = bars(&[1.0, 2.0, 3.0, 4.0, 100.0]);
    for b in &bs[..3] {
        ind.on_bar(b);
    }
    let mut speculative = ind.clone_box();
    speculative.on_bar(&bs[4]);
    assert_eq!(ind.value()[0], 2.0, "the original must not have advanced");
    assert_eq!(ind.on_bar(&bs[3])[0], 3.0, "and it continues from where it was");
}

#[test]
fn a_top_level_let_is_visible_inside_on_bar_and_runs_once() {
    // `runs` counts top-level executions. If the top level re-ran per bar the counter would
    // reset and the recurrence below could never reach 3.
    let src = r#"
            let scale = 10.0;
            fn init() { #{ n: 0 } }
            fn on_bar(bar) { this.n += 1; this.n * scale }
        "#;
    let mut ind = compile_indicator("scaled", src).unwrap();
    let out: Vec<f64> = bars(&[1.0, 2.0, 3.0]).iter().map(|b| ind.on_bar(b)[0]).collect();
    assert_eq!(out, vec![10.0, 20.0, 30.0]);
}

#[test]
fn every_bar_field_reaches_the_script() {
    let src = "fn on_bar(bar) { bar.ts + bar.open + bar.high + bar.low + bar.close + \
                   bar.volume }";
    let mut ind = compile_indicator("sum", src).unwrap();
    let b = &bars(&[5.0])[0];
    // ts 0 + open 5 + high 6 + low 4 + close 5 + volume 100
    assert_eq!(ind.on_bar(b)[0], 120.0);
}

#[test]
fn init_is_optional_and_defaults_to_an_empty_map() {
    let src = "fn on_bar(bar) { this.n = if this.n == () { 1 } else { this.n + 1 }; this.n }";
    let mut ind = compile_indicator("count", src).unwrap();
    let out: Vec<f64> = bars(&[1.0, 2.0, 3.0]).iter().map(|b| ind.on_bar(b)[0]).collect();
    assert_eq!(out, vec![1.0, 2.0, 3.0]);
}

#[test]
fn warmup_is_optional_and_absent_means_the_conservative_zero() {
    let plain = compile_indicator("p", "fn on_bar(bar) { bar.close }").unwrap();
    assert_eq!(plain.lookback(), 0);
    assert_eq!(plain.lookback_full(), 0);
    let declared = compile_indicator("d", RUNNING_MEAN).unwrap();
    assert_eq!(declared.lookback(), 2);
    assert_eq!(declared.lookback_full(), 2, "the trait default forwards to lookback");
}

/// Over-claiming here would let a caller size seed history too short — see the method's doc.
#[test]
fn a_declared_warmup_is_never_reported_as_exact() {
    assert!(!compile_indicator("d", RUNNING_MEAN).unwrap().lookback_exact());
}

#[test]
fn a_negative_warmup_clamps_to_zero_rather_than_wrapping() {
    let src = "fn warmup() { -5 } fn on_bar(bar) { bar.close }";
    assert_eq!(compile_indicator("neg", src).unwrap().lookback(), 0);
}

#[test]
fn a_missing_on_bar_is_a_compile_error_naming_the_function() {
    let e = compile_indicator("bad", "fn init() { #{} }").unwrap_err().to_string();
    assert!(e.contains("on_bar"), "{e}");
    assert!(e.contains("bad"), "the error names the indicator: {e}");
}

/// A `fn on_bar()` with no parameter is the natural typo (it is the STRATEGY hook's shape), so
/// it must be caught at compile rather than as a per-bar function-not-found on every bar.
#[test]
fn an_on_bar_with_the_wrong_arity_is_rejected_at_compile() {
    assert!(compile_indicator("bad", "fn on_bar() { 1.0 }").is_err());
    assert!(compile_indicator("bad", "fn on_bar(a, b) { 1.0 }").is_err());
}

#[test]
fn a_syntax_error_is_a_compile_error_not_a_panic() {
    assert!(compile_indicator("bad", "fn on_bar(bar) { this. }").is_err());
}

/// ⚠ The NaN trap, from the other side. A runtime fault CANNOT be distinguished from warm-up
/// by its value, so it must be distinguishable some other way — that is what `fault` is for.
#[test]
fn a_runtime_fault_is_recorded_rather_than_passing_as_warm_up() {
    let src = "fn on_bar(bar) { if bar.close > 2.0 { throw \"boom\" } bar.close }";
    let mut ind = compile_indicator("boom", src).unwrap();
    let bs = bars(&[1.0, 3.0, 1.5]);
    assert_eq!(ind.on_bar(&bs[0])[0], 1.0);
    assert!(ind.fault().is_none());

    assert!(ind.on_bar(&bs[1])[0].is_nan(), "the trait has no error channel, so NaN");
    assert!(ind.fault().is_some_and(|f| f.contains("boom")), "...but the fault is HELD");

    // ...and a later good bar clears it, so the flag means "the last bar faulted", not "this
    // indicator faulted once". A sticky flag would disable a strategy for a transient.
    assert_eq!(ind.on_bar(&bs[2])[0], 1.5);
    assert!(ind.fault().is_none());
}

#[test]
fn a_non_numeric_return_is_a_fault_naming_the_type() {
    let src = "fn on_bar(bar) { \"twenty\" }";
    let mut ind = compile_indicator("s", src).unwrap();
    assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_nan());
    assert!(ind.fault().is_some_and(|f| f.contains("must return a number")));
}

/// See [`read_values`]: silently taking line 0 is the exact hazard `exclusion` refuses for the
/// built-in band indicators, so creating one from this side — WITHOUT declaring the lines — is
/// refused too. The refusal now names the declaration that makes it legal, which is the only
/// part of this that changed.
#[test]
fn a_multi_output_return_is_refused_rather_than_truncated_to_line_zero() {
    let mut ind = compile_indicator("bands", "fn on_bar(bar) { [1.0, 2.0, 3.0] }").unwrap();
    let v = ind.on_bar(&bars(&[1.0])[0])[0];
    assert!(v.is_nan(), "must NOT return 1.0, the first line");
    assert!(ind.fault().is_some_and(|f| f.contains("single-output")), "{:?}", ind.fault());
    assert!(
        ind.fault().is_some_and(|f| f.contains("outputs()")),
        "the refusal must name the way IN, not just the limitation: {:?}",
        ind.fault()
    );
}

/// A band indicator: three lines, a `param()` knob, and a real warm-up bar.
const BANDS: &str = r#"
        let width = param("width", 2.0);
        fn outputs() { ["upper", "mid", "lower"] }
        fn init() { #{ prev: () } }
        fn warmup() { 1 }
        fn on_bar(bar) {
            if this.prev == () { this.prev = bar.close; return (); }
            this.prev = bar.close;
            [bar.close + width, bar.close, bar.close - width]
        }
    "#;

/// ⚠ **The compatibility gate.** Every indicator written before `outputs()` existed declares
/// none, and for those NOTHING may change: one line named after the file, one value out of
/// `on_bar`, one row out of `vectorize`, and an ARRAY still refused — at EVERY length, including
/// the one-element array that would otherwise be a second, undeclared spelling of a number.
///
/// Non-vacuous because the last two assertions are the SAME source with `fn outputs()` added:
/// the array is accepted there, so this test measures the declaration and not the array shape.
/// An implementation that simply started accepting arrays would fail on the refusals; one that
/// never accepted them would fail on the acceptance.
#[test]
fn a_file_that_declares_no_outputs_is_exactly_single_output_as_before() {
    let mut plain = compile_indicator("plain", "fn on_bar(bar) { bar.close }").unwrap();
    assert_eq!(plain.outputs(), ["plain".to_string()], "one line, named after the file");
    let bs = bars(&[1.0, 2.0]);
    assert_eq!(plain.on_bar(&bs[0]).len(), 1);
    assert_eq!(plain.value().len(), 1);
    assert_eq!(plain.vectorize(&bs).len(), 1, "vectorize is one row per line");

    for body in ["[bar.close]", "[bar.close, bar.close]"] {
        let mut ind = compile_indicator("arr", &format!("fn on_bar(bar) {{ {body} }}")).unwrap();
        assert!(ind.on_bar(&bs[0])[0].is_nan(), "{body} must not resolve to a value");
        assert!(
            ind.fault().is_some_and(|f| f.contains("single-output")),
            "{body}: {:?}",
            ind.fault()
        );
    }
    // ...and the identical bodies with the lines DECLARED are fine, so the refusals above are
    // about the missing declaration.
    for (decl, body) in [(r#"["a"]"#, "[bar.close]"), (r#"["a", "b"]"#, "[bar.close, bar.close]")] {
        let src = format!("fn outputs() {{ {decl} }} fn on_bar(bar) {{ {body} }}");
        let mut ind = compile_indicator("arr", &src).unwrap();
        assert!(ind.on_bar(&bs[0])[0].is_finite(), "{decl}: {:?}", ind.fault());
        assert!(ind.fault().is_none(), "{decl}: {:?}", ind.fault());
    }
}

#[test]
fn a_declared_line_list_is_reported_in_order_and_drives_on_bars_width() {
    let mut ind = compile_indicator("bands", BANDS).unwrap();
    assert_eq!(ind.outputs(), ["upper".to_string(), "mid".into(), "lower".into()]);
    let bs = bars(&[10.0, 20.0]);
    let warming = ind.on_bar(&bs[0]);
    assert_eq!(warming.len(), 3, "a `()` return is warm-up on EVERY line, not just line 0");
    assert!(warming.iter().all(|v| v.is_nan()), "{warming:?}");
    assert_eq!(ind.on_bar(&bs[1]), vec![22.0, 20.0, 18.0]);
    assert_eq!(ind.value(), vec![22.0, 20.0, 18.0]);
}

/// ⚠ A mismatch is an ERROR naming BOTH counts — never a pad (which invents a number for a line
/// the author never computed) and never a truncation (which hides one they did). Both would be
/// silent, on the value path a strategy trades from.
#[test]
fn a_line_count_mismatch_is_an_error_naming_both_counts() {
    let short = "fn outputs() { [\"a\", \"b\", \"c\"] } fn on_bar(bar) { [1.0, 2.0] }";
    let mut ind = compile_indicator("short", short).unwrap();
    let out = ind.on_bar(&bars(&[1.0])[0]);
    assert_eq!(out.len(), 3, "the value vector still matches the DECLARATION");
    assert!(out.iter().all(|v| v.is_nan()), "and every line is the fault NaN: {out:?}");
    let f = ind.fault().unwrap_or_default().to_string();
    assert!(f.contains("declares 3"), "{f}");
    assert!(f.contains("returned 2"), "{f}");
    assert!(f.contains("a, b, c"), "the message names the lines: {f}");

    // ...and the same for a SCALAR return, which is the natural mistake when adding a line.
    let scalar = "fn outputs() { [\"a\", \"b\"] } fn on_bar(bar) { bar.close }";
    let mut ind = compile_indicator("scalar", scalar).unwrap();
    ind.on_bar(&bars(&[1.0])[0]);
    let f = ind.fault().unwrap_or_default().to_string();
    assert!(f.contains("declares 2") && f.contains("returned 1"), "{f}");
}

/// One line may warm up on its own — `[upper, (), lower]` — which is what lets a signal line
/// that needs more history than its own source say so.
#[test]
fn a_single_line_may_warm_up_while_the_others_are_real() {
    let src = "fn outputs() { [\"v\", \"sig\"] } fn on_bar(bar) { [bar.close, ()] }";
    let mut ind = compile_indicator("half", src).unwrap();
    let out = ind.on_bar(&bars(&[7.0])[0]);
    assert_eq!(out[0], 7.0);
    assert!(out[1].is_nan());
    assert!(ind.fault().is_none(), "a `()` LINE is warm-up, not a fault: {:?}", ind.fault());
}

/// The `vectorize`-is-the-fold property, now on every line at once. Line-major, and bit-for-bit
/// against the streaming path — an implementation that transposed the result, or that folded
/// only line 0, fails here rather than in a chart nobody diffed.
#[test]
fn vectorize_is_line_major_and_bit_identical_to_the_streaming_fold_on_every_line() {
    let ind = compile_indicator("bands", BANDS).unwrap();
    let bs = bars(&[1.0, 2.5, 3.25, 4.125, 9.0]);
    let batch = ind.vectorize(&bs);
    assert_eq!(batch.len(), 3, "one row per declared line");
    assert!(batch.iter().all(|l| l.len() == bs.len()), "each row is one value per bar");
    let mut s = ind.clone();
    for (i, b) in bs.iter().enumerate() {
        let streamed = s.on_bar(b);
        for (l, v) in streamed.iter().enumerate() {
            assert_eq!(
                v.to_bits(),
                batch[l][i].to_bits(),
                "line {l} bar {i}: {v} != {}",
                batch[l][i]
            );
        }
    }
}

/// A knob must reach every line, not only the one somebody tested.
#[test]
fn a_call_site_knob_moves_every_line() {
    let mut wide = compile_indicator_with("bands", BANDS, &[5.0]).unwrap();
    let bs = bars(&[10.0, 20.0]);
    wide.on_bar(&bs[0]);
    assert_eq!(wide.on_bar(&bs[1]), vec![25.0, 20.0, 15.0], "width 5, not the default 2");
}

#[test]
fn outputs_must_be_a_non_empty_array_of_named_lines() {
    for (src, needle) in [
        ("fn outputs() { 3 } fn on_bar(bar) { bar.close }", "array of line names"),
        ("fn outputs() { [] } fn on_bar(bar) { bar.close }", "empty array"),
        ("fn outputs() { [\"a\", 7] } fn on_bar(bar) { bar.close }", "not a line NAME"),
        ("fn outputs() { [\"a\", \"  \"] } fn on_bar(bar) { bar.close }", "is empty"),
    ] {
        let e = compile_indicator("bad", src).unwrap_err().to_string();
        assert!(e.contains(needle), "expected {needle:?} in: {e}");
    }
}

/// ⚠ Two names that SANITISE alike would generate ONE accessor with two meanings, silently —
/// `register_fn` replaces. That is the registry's `generated_line_names_are_unambiguous` hazard
/// asked of one user file, where the author can act on it, so it is a COMPILE error.
#[test]
fn two_lines_that_spell_one_accessor_are_refused_at_compile() {
    let src = "fn outputs() { [\"%K\", \"K\"] } fn on_bar(bar) { [1.0, 2.0] }";
    let e = compile_indicator("stoch_ish", src).unwrap_err().to_string();
    assert!(e.contains("both spell"), "{e}");
    assert!(e.contains("stoch_ish_k"), "the message names the collision: {e}");
    // ...and the same two lines under names that do NOT collide compile fine, so the rule is
    // about the collision rather than about `%`.
    let ok = "fn outputs() { [\"%K\", \"%D\"] } fn on_bar(bar) { [1.0, 2.0] }";
    assert!(compile_indicator("stoch_ish", ok).is_ok());
}

/// A line whose accessor would shadow a BUILT-IN is refused where the author can fix it — at
/// COMPILE, naming the accessor — rather than by silently losing that one accessor to the
/// built-in at registration time.
///
/// ⚠ The witness is DERIVED, and it has to be. [`crate::line_fn_name`] joins the two halves
/// with a literal `_`, so the obvious hand-written witness — `pos` + `ition` for the host read
/// `position` — spells `pos_ition` and collides with nothing; this test was written that way
/// and failed. Only a name CONTAINING a `_` is reachable, and every one of those is a registry
/// entry, so the pair comes from the registry.
#[test]
fn a_line_whose_accessor_would_take_a_builtins_name_is_refused_at_compile() {
    let (stem, line) = crate::engine::registry_name_a_user_line_could_spell();
    let taken = crate::line_fn_name(stem, line);
    let body = "fn on_bar(bar) { [1.0, 2.0] }";
    let e = compile_indicator(stem, &format!("fn outputs() {{ [\"{line}\", \"x\"] }} {body}"))
        .unwrap_err()
        .to_string();
    assert!(e.contains(&taken), "the refusal must name the accessor it would take: {e}");
    assert!(e.contains("built-in indicator's own name"), "{e}");
    // ...and the SAME file with that one line renamed compiles, so the refusal is about the
    // collision and not about declaring outputs at all.
    let ok = format!("fn outputs() {{ [\"{line}_of_mine\", \"x\"] }} {body}");
    assert!(compile_indicator(stem, &ok).is_ok(), "{:?}", compile_indicator(stem, &ok).err());
}

/// An integer return is the natural spelling of a counting indicator; requiring `.to_float()`
/// would be a papercut with no upside.
#[test]
fn an_integer_return_coerces() {
    let mut ind = compile_indicator("i", "fn on_bar(bar) { 42 }").unwrap();
    assert_eq!(ind.on_bar(&bars(&[1.0])[0])[0], 42.0);
}

/// The safety boundary from the module doc, asserted rather than asserted-in-prose: the
/// indicator engine registers no verbs, so an indicator that tries to trade fails to resolve.
#[test]
fn an_indicator_cannot_place_an_order_or_read_the_broker() {
    for verb in ["buy(1.0)", "sell(1.0)", "position()", "equity()", "sma(20)"] {
        let src = format!("fn on_bar(bar) {{ {verb}; bar.close }}");
        let mut ind = compile_indicator("rogue", &src).unwrap();
        assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_nan(), "{verb} must not resolve");
        assert!(ind.fault().is_some(), "{verb} must fault");
    }
}

/// The runaway bound. `set_max_operations` mirrors `build_engine`'s, so a user indicator
/// cannot hang the bar loop where a strategy could not.
#[test]
fn a_runaway_script_is_bounded_by_the_operation_limit() {
    let src = "fn on_bar(bar) { let i = 0; loop { i += 1; } }";
    let mut ind = compile_indicator("spin", src).unwrap();
    assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_nan());
    assert!(ind.fault().is_some(), "the operation limit must surface as a fault");
}

// ----------------------------------------------------- the chart-mounting seam

/// The DEFAULT is the safe one: an undeclared placement is the indicator's own pane.
///
/// Non-vacuous: the opposite default is one word away in `compile_indicator_with`, and it is
/// the one that silently ruins the price pane's autofit for any indicator whose scale is not a
/// price — which is most of them.
#[test]
fn overlay_is_optional_and_absent_means_its_own_pane() {
    assert!(!compile_indicator("p", "fn on_bar(bar) { bar.close }").unwrap().is_overlay());
    let src = "fn overlay() { true } fn on_bar(bar) { bar.close }";
    assert!(compile_indicator("o", src).unwrap().is_overlay());
    let src = "fn overlay() { false } fn on_bar(bar) { bar.close }";
    assert!(!compile_indicator("f", src).unwrap().is_overlay());
}

/// A non-bool `overlay()` is a COMPILE error naming the hook, not a silent `false`.
///
/// Non-vacuous: `as_bool().unwrap_or(false)` is the tempting spelling and would accept
/// `fn overlay() { 1 }` — the natural way to write it for anyone coming from C — by quietly
/// meaning the opposite of what was written.
#[test]
fn a_non_bool_overlay_is_rejected_at_compile_naming_the_hook() {
    let e = compile_indicator("o", "fn overlay() { 1 } fn on_bar(bar) { bar.close }")
        .unwrap_err()
        .to_string();
    assert!(e.contains("overlay"), "{e}");
}

/// [`user_meta`] carries the placement the script declared.
///
/// Non-vacuous: a hard-coded `RenderKind` (either one) passes half of this and fails the other.
#[test]
fn user_meta_maps_the_overlay_hook_onto_the_render_kind() {
    let osc = user_meta(&compile_indicator("um_osc", "fn on_bar(bar) { bar.close }").unwrap());
    assert_eq!(osc.kind, vike_indicators::RenderKind::Oscillator);
    let src = "fn overlay() { true } fn on_bar(bar) { bar.close }";
    let ovl = user_meta(&compile_indicator("um_ovl", src).unwrap());
    assert_eq!(ovl.kind, vike_indicators::RenderKind::Overlay);
    assert!(ovl.is_user() && osc.is_user());
    assert_eq!(ovl.name, "um_ovl");
    assert_eq!(ovl.pretty, "um_ovl", "the label is the file stem verbatim");
}

/// The declared `param()` knobs become the meta's parameter surface, IN ORDER — the order a
/// call site's positional arguments and the settings dialog's rows both depend on.
///
/// Non-vacuous: `RhaiIndicator::params` is an `IndexMap`-backed first-seen order, and any
/// re-collection through a `HashMap` would still produce two rows with the right names and
/// defaults while scrambling which one `my_thing(50)` sets.
#[test]
fn user_meta_carries_the_declared_knobs_in_first_seen_order() {
    let src = "let a = param(\"slow\", 50); let b = param(\"fast\", 0.25); \
                   fn on_bar(bar) { bar.close }";
    let m = user_meta(&compile_indicator("um_params", src).unwrap());
    assert_eq!(m.params.len(), 2);
    assert_eq!(m.params[0].name, "slow");
    assert_eq!(m.params[0].default, 50.0);
    assert_eq!(m.params[0].step, 1.0, "a whole-number default is a bar count");
    assert_eq!(m.params[1].name, "fast");
    assert_eq!(m.params[1].default, 0.25);
    assert_eq!(m.params[1].step, 0.001, "a fractional default must stay editable to 3 dp");
}

/// The point of the whole seam: the meta's `factory` builds an indicator that computes the
/// USER's recurrence at the CALL SITE's parameters.
///
/// Non-vacuous twice: `build_with` reaching the built-in `make_with` slot would panic
/// (`vike_indicators`' `unbuilt`), and a factory that ignored `raw` and cloned the prototype
/// unchanged would return 30.0 rather than 50.0 on the second assert.
#[test]
fn the_meta_factory_builds_the_users_recurrence_at_the_call_sites_params() {
    let src = "let k = param(\"k\", 3.0); fn on_bar(bar) { bar.close * k }";
    let m = user_meta(&compile_indicator("um_factory", src).unwrap());
    let b = &bars(&[10.0])[0];
    assert_eq!(m.build().on_bar(b)[0], 30.0, "no params -> the declared default");
    assert_eq!(m.build_with(&[5.0]).on_bar(b)[0], 50.0, "the call site's k reaches the script");
}

/// A knob whose value breaks the file's TOP LEVEL falls back to the declared defaults and
/// LOGS, rather than panicking or plotting a fabricated number — the residual `user_meta`'s
/// factory documents.
///
/// Non-vacuous: `compile_with(...).unwrap()` (the shorter spelling) turns this exact script
/// into a panic inside a chart repaint, which is the failure this arm exists to prevent.
///
/// ⚠ **`k.to_int()` is the whole reason this test tests anything.** `param()` hands back an
/// f64, and rhai resolves a mixed `INT / FLOAT` through its FLOAT `/`, which is plain IEEE
/// division with no zero check (in the rhai crate's own source: `Divide => impl_op!(FLOAT =>
/// $xx / $yy)` in `src/func/builtin.rs`, while only the INT `divide` in
/// `src/packages/arithmetic.rs` raises `Division by zero`). So the
/// obvious spelling — `100 / (k - 5)` — quietly evaluates to `inf` at k=5, compiles fine, and
/// leaves this `Err` arm with ZERO coverage while looking covered. Forcing the divisor to an
/// INT is what makes the failure real. `the_premise` below asserts it directly, so a future
/// rhai that changes either rule fails HERE, naming the reason, instead of failing on an
/// unexplained number three lines down.
#[test]
fn a_param_value_that_breaks_the_top_level_falls_back_to_the_defaults() {
    // `k == 5` is INTEGER division by zero at the top level — an error rhai does raise, and
    // one only the RE-run at that value hits.
    let src = "let k = param(\"k\", 3); let scale = 100 / (k.to_int() - 5); \
                   fn on_bar(bar) { bar.close * scale }";
    let proto = compile_indicator("um_bad", src).unwrap();

    // the_premise: k=5 must genuinely fail to compile, and k=4 must genuinely succeed.
    assert!(
        proto.compile_with(&[5.0]).is_err(),
        "premise: k=5 must break the file's top level, or the fallback below proves nothing"
    );
    assert!(proto.compile_with(&[4.0]).is_ok(), "premise: a good override still compiles");

    let m = user_meta(&proto);
    let b = &bars(&[1.0])[0];
    // defaults: k=3 -> scale = 100/(3-5) = -50
    assert_eq!(m.build().on_bar(b)[0], -50.0);
    // k=4 -> scale = 100/(4-5) = -100: a good override genuinely takes effect...
    assert_eq!(m.build_with(&[4.0]).on_bar(b)[0], -100.0);
    // ...and k=5 (division by zero at the top level) degrades to the defaults, not a panic.
    assert_eq!(m.build_with(&[5.0]).on_bar(b)[0], -50.0);
}

/// The rhai semantics the test above depends on, asserted on rhai itself rather than inferred.
///
/// This is the claim that was WRONG when the fallback test was first written: it used
/// `100 / (k - 5)` with a float `k`, believed that raised `Division by zero`, and so never
/// exercised the `Err` arm it existed to cover at all. Pinning the language rule separately
/// means the next person who reaches for the shorter spelling is told why it does not work.
#[test]
fn rhai_raises_on_integer_division_by_zero_but_not_on_float() {
    let float_div = "let k = param(\"k\", 3); let scale = 100 / (k - 5); \
                         fn on_bar(bar) { scale }";
    let mut ind = compile_indicator("f", float_div)
        .unwrap()
        .compile_with(&[5.0])
        .expect("float division by zero is NOT an error in rhai — it is `inf`");
    assert!(ind.on_bar(&bars(&[1.0])[0])[0].is_infinite(), "...and the value is infinite");

    let int_div = "let k = param(\"k\", 3); let scale = 100 / (k.to_int() - 5); \
                       fn on_bar(bar) { scale }";
    let e = compile_indicator("i", int_div).unwrap().compile_with(&[5.0]).unwrap_err();
    assert!(e.to_string().contains("Division by zero"), "{e}");
}

/// A paramless user indicator is a real one — `coerce` over an empty spec list must not turn
/// its zero-argument construction into an error or a NaN.
#[test]
fn a_paramless_user_indicator_builds_from_an_empty_slice() {
    let m = user_meta(&compile_indicator("um_plain", "fn on_bar(bar) { bar.close }").unwrap());
    assert!(m.params.is_empty());
    assert_eq!(m.build().on_bar(&bars(&[7.5])[0])[0], 7.5);
    assert_eq!(m.build_with(&[]).on_bar(&bars(&[7.5])[0])[0], 7.5);
}
