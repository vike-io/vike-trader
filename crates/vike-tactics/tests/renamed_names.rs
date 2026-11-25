//! Every `vike_tactics` row of `vike_model::host_build::moved_name::RENAMED_NAMES` names a real
//! type: its new name resolves, or, for a row the scripted rename (plan 2b's order-language words)
//! has not reached yet, its old one does. So a misspelled row fails here on every tree, before the
//! rename and after it.
//!
//! ⚠ Unlike the hint's own goldens (`crates/vike-model/tests/host_build/moved_name.rs`), this file is
//! NOT excluded from the scripted rename: it rewrites the paths below to the new names, which keeps
//! it compiling and checking the new names once the old ones are gone.

use std::any::type_name;

use vike_model::host_build::moved_name::{RENAMED_NAMES, commission_hint, slippage_hint};
use vike_model::{Commission, Slippage};

/// A type's own name, without its module path or its generic arguments.
fn last(path: &'static str) -> &'static str {
    let bare = path.split('<').next().unwrap_or(path);
    bare.rsplit("::").next().unwrap_or(bare)
}

#[test]
fn every_renamed_language_row_names_a_real_type() {
    let resolved = [
        last(type_name::<vike_model::OrderSide>()),
        last(type_name::<vike_tactics::RuleDefaults>()),
        last(type_name::<vike_tactics::RuleRun<()>>()),
        last(type_name::<vike_tactics::RuleRunOptions>()),
        last(type_name::<vike_tactics::TacticsSizing>()),
        last(type_name::<vike_tactics::TacticsInput<'static>>()),
        last(type_name::<vike_tactics::TacticsEntry>()),
        last(type_name::<vike_tactics::TacticsOrder>()),
        last(type_name::<vike_tactics::TacticsClose>()),
        last(type_name::<vike_tactics::TacticsExit>()),
        last(type_name::<vike_tactics::TacticsTrail>()),
        last(type_name::<vike_tactics::TacticsBookLot>()),
        last(type_name::<vike_tactics::TacticsBookBinding>()),
        last(type_name::<vike_tactics::HigherTimeframePeriod>()),
        last(type_name::<vike_tactics::HigherTimeframeHistoryBefore>()),
    ];
    let rows: Vec<_> =
        RENAMED_NAMES.into_iter().filter(|(root, _, _)| *root == "vike_tactics").collect();
    assert_eq!(rows.len(), resolved.len(), "a row without a type above, or a type without a row");
    for (_, old, new) in rows {
        let new_name = new.rsplit("::").next().unwrap_or(new);
        assert!(
            resolved.contains(&new_name) || resolved.contains(&old),
            "`RENAMED_NAMES` row `{old}` -> `{new}`: neither name is a type of the order language"
        );
    }
}

/// The order language's old commission is THE cost type now (the owner's word of 2026-11-19): a file that
/// still reaches it through `vike_tactics` draws `commission_hint`, whose four constructors are real and
/// state the schedules the old variants stated. Each old name is built from parts, as the hint's own are.
#[test]
fn the_old_commission_draws_the_hint_its_constructors_answer() {
    for old in ["Commission", concat!("RuleDefaults", "Fee")] {
        let source = format!("use {}::{{Rule, RuleDefaults, {old}, Tactics}};", "vike_tactics");
        let hint = commission_hint(&source).expect("an old commission draws the hint");
        for written in ["percent(p)", "per_contract(c)", "per_order(c)", "Commission::None"] {
            assert!(hint.contains(written), "{hint}");
        }
        assert!(hint.starts_with(&format!("`{old}::Percent(p)`")), "{hint}");
    }
    assert_eq!(commission_hint(&format!("use {}::{{Rule, Tactics}};", "vike_tactics")), None);
    let per_contract =
        vike_model::FeeSchedule::PerShareWithFloor { per_share: 0.5, min: 0.0, max_percent: 0.0 };
    let per_order =
        vike_model::FeeSchedule::PerShareWithFloor { per_share: 0.0, min: 1.5, max_percent: 0.0 };
    let bps = 0.1 * 100.0; // the old `Percent(0.1)`'s own arithmetic
    let percent = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: bps, taker_bps: bps };
    assert_eq!(Commission::percent(0.1).schedule(), Some(percent));
    assert_eq!(Commission::per_contract(0.5).schedule(), Some(per_contract));
    assert_eq!(Commission::per_order(1.5).schedule(), Some(per_order));
    assert_eq!(Commission::None.schedule(), None);
}

/// The order language's old slippage is THE cost type's `Slippage` now (the owner's word of 2026-11-19): a file
/// that still reaches it through `vike_tactics` draws `slippage_hint`, whose constructors are real and state the
/// tables the old variants stated.
#[test]
fn the_old_slippage_draws_the_hint_its_constructors_answer() {
    let old = concat!("RuleDefaults", "Slippage");
    let source = format!("use {}::{{Rule, RuleDefaults, {old}, Tactics}};", "vike_tactics");
    let hint = slippage_hint(&source).expect("the old slippage draws the hint");
    for written in ["Slippage::percent(p)", "Slippage::ticks(ticks, tick)", "Slippage::None"] {
        assert!(hint.contains(written), "{hint}");
    }
    assert!(hint.starts_with(&format!("`{old}::Percent(p)`")), "{hint}");
    assert_eq!(slippage_hint(&format!("use {}::{{Rule, Tactics}};", "vike_tactics")), None);
    let percent = Slippage::percent(0.05).table().expect("stated");
    assert_eq!((percent.model.as_str(), percent.value), ("percent", Some(0.05 / 100.0)));
    let ticks = Slippage::ticks(2.0, 0.25).table().expect("stated");
    assert_eq!((ticks.model.as_str(), ticks.ticks, ticks.tick), ("ticks", Some(2.0), Some(0.25)));
    assert_eq!(Slippage::None.table(), None);
}
