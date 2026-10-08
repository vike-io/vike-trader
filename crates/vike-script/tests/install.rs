//! `install_user_indicators` — the process-wide set a BINARY installs at startup.
//!
//! ⚠ **This is its own test binary because the set is a `OnceLock`.** Cargo runs each
//! `tests/*.rs` as a separate process, so installing here cannot leak into `user_indicator.rs`'s
//! tests (which use the explicit `compile_with_indicators` path) or into any other file. Putting an
//! install test beside them would make their results depend on execution order.
//!
//! Everything below shares ONE install, in dependency order within a single `#[test]`, for the same
//! reason: two `#[test]`s in one binary run concurrently, and a once-per-process call cannot be
//! exercised twice.

mod common;
use common::{MockBroker, drive};
use vike_script::{RhaiStrategy, compile_indicator};

#[test]
fn the_installed_set_reaches_a_plain_compile_and_installs_exactly_once() {
    // ── before install: the name is unbound ─────────────────────────────────────────────────────
    // This is the property the install exists to change, measured first so the assertion after it
    // cannot be vacuous.
    assert!(vike_script::installed_user_indicators().is_empty());
    let mut s = RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(tripler()) }").unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[5.0]);
    assert!(b.submits.is_empty(), "an uninstalled name must not resolve");

    // ── install ─────────────────────────────────────────────────────────────────────────────────
    let tripler = compile_indicator("tripler", "fn on_bar(bar) { bar.close * 3.0 }").unwrap();
    let counter = compile_indicator(
        "installed_count",
        "fn init() { #{ n: 0 } } fn on_bar(bar) { this.n += 1; this.n }",
    )
    .unwrap();
    vike_script::install_user_indicators(vec![tripler, counter]).expect("first install succeeds");
    assert_eq!(vike_script::installed_user_indicators(), vec!["tripler", "installed_count"]);

    // ── after install: a PLAIN `compile` sees it, with no argument threaded anywhere ─────────────
    // This is the whole point: `harness::run_backtest` and the Studio runners never pass an
    // indicator set, and they reach `compile_with_params`, which is what this exercises.
    let mut s = RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(tripler()) }").unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[5.0, 6.0]);
    assert_eq!(b.submits, vec![(1, 15.0), (1, 18.0)]);

    // ...and it is still fed exactly once per bar through the shared cache.
    let src = "fn on_bar() { buy(installed_count() + installed_count()) }";
    let mut s = RhaiStrategy::<MockBroker>::compile(src).unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, 1.0]);
    assert_eq!(b.submits, vec![(1, 2.0), (1, 4.0)], "two references, one feed: 1+1 then 2+2");

    // ── each strategy gets its OWN streaming state from the shared prototype ────────────────────
    let mut a =
        RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(installed_count()) }").unwrap();
    let mut c =
        RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(installed_count()) }").unwrap();
    let (mut ba, mut bc) = (MockBroker::default(), MockBroker::default());
    drive(&mut a, &mut ba, &[1.0, 1.0, 1.0]);
    drive(&mut c, &mut bc, &[1.0]);
    assert_eq!(ba.submits.last().unwrap().1, 3.0);
    assert_eq!(bc.submits, vec![(1, 1.0)], "the second strategy starts at 1, not 4");

    // ── an EXPLICIT set overrides the installed one, per name ───────────────────────────────────
    // What lets a caller (or a test) bind its own without disturbing process state.
    let shadow = compile_indicator("tripler", "fn on_bar(bar) { 0.5 }").unwrap();
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(tripler()) }",
        Default::default(),
        &[shadow],
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[5.0]);
    assert_eq!(b.submits, vec![(1, 0.5)], "the explicit one wins over the installed one");
    // ...and the installed set is untouched by that override.
    let mut s = RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(tripler()) }").unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[5.0]);
    assert_eq!(b.submits, vec![(1, 15.0)]);

    // ── a SECOND install is an error, not a silent no-op ────────────────────────────────────────
    // Two callers believing they own this would each be wrong somewhere, so it says so.
    let extra = compile_indicator("late", "fn on_bar(bar) { 1.0 }").unwrap();
    let err = vike_script::install_user_indicators(vec![extra]).expect_err("must refuse");
    assert!(err.contains("already installed"), "{err}");
    assert_eq!(
        vike_script::installed_user_indicators(),
        vec!["tripler", "installed_count"],
        "the refused call must not have changed the set"
    );
}
