//! A script that still names the order side's OLD Rhai module is refused AT COMPILE with the fix
//! (`crates/vike-rhai/src/strategy.rs`'s `RENAMED_MODULES`), never at its first bar with rhai's
//! "module not found". The old word is built from parts ([`old_side`]): the scripted rename of plan 2b
//! rewrites the joined spelling wherever it finds it, and a rewritten source would test nothing.

use vike_rhai::RhaiStrategy;
use vike_rhai::test_support::OrderBrokerMock;

/// The old module joined to `variant` by `::`, never one literal.
fn old_side(variant: &str) -> String {
    format!("{}::{variant}", "Side")
}

#[test]
fn the_old_side_module_is_refused_at_compile_naming_the_new_one() {
    let src = format!(r#"fn on_bar(strategy) {{ strategy.entry("L", {}); }}"#, old_side("Short"));
    let Err(e) = RhaiStrategy::<OrderBrokerMock>::compile(&src) else {
        panic!("a script naming the old module must not compile");
    };
    assert_eq!(
        e.summary(),
        "`Side` is now `OrderSide`: write OrderSide::Short (the order side's module was renamed)"
    );
    assert_eq!(e.line(), Some(1));
    // the CONTROL: the same script on the new name compiles
    let new = src.replace(&old_side("Short"), "OrderSide::Short");
    RhaiStrategy::<OrderBrokerMock>::compile(&new).expect("the new module name compiles");
}
