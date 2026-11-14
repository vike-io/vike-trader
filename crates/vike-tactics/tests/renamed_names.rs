//! Every `vike_tactics` row of `vike_model::host_build::moved_name::RENAMED_NAMES` names a real
//! type: its new name resolves, or, for a row the scripted rename (plan 2b's order-language words)
//! has not reached yet, its old one does. So a misspelled row fails here on every tree, before the
//! rename and after it.
//!
//! ⚠ Unlike the hint's own goldens (`crates/vike-model/tests/host_build/moved_name.rs`), this file is
//! NOT excluded from the scripted rename: it rewrites the paths below to the new names, which keeps
//! it compiling and checking the new names once the old ones are gone.

use std::any::type_name;

use vike_model::host_build::moved_name::RENAMED_NAMES;

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
        last(type_name::<vike_tactics::RuleDefaultsFee>()),
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
