//! The old crate name's rows of `vike_model::host_build::moved_name::RENAMED_NAMES` (G14 renamed
//! the crate `vike_market_making`): each points at a name this crate's root really exports, under
//! the new crate name, and there is one row per public root name. A misspelled row fails here.
//!
//! ⚠ The old crate name is built from parts (`concat!`), as the hint module requires: the scripted
//! crate rename rewrites every whole spelling, string literals included.

use vike_model::host_build::moved_name::RENAMED_NAMES;

/// Each listed name resolves at this crate's root (the `use` is the check).
macro_rules! names_at_the_root {
    ($($name:ident),+ $(,)?) => {{
        #[expect(unused_imports, reason = "the import IS the check: the name must resolve there")]
        use crate::{$($name),+};
        [$(stringify!($name)),+]
    }};
}

#[test]
fn every_old_crate_row_names_a_public_root_name_of_this_crate() {
    let resolved = names_at_the_root!(
        reward,
        OwnBookFiltration,
        SpreadMakerPreset,
        rearm_fee_floor,
        MakerParamError,
    );
    let old_crate = concat!("vike", "_mm");
    let rows: Vec<(&str, &str)> = RENAMED_NAMES
        .iter()
        .filter(|(root, _, _)| *root == old_crate)
        .map(|(_, old, new)| (*old, *new))
        .collect();
    assert_eq!(rows.len(), resolved.len(), "one `RENAMED_NAMES` row per public root name");
    for (old, new) in rows {
        assert_eq!(
            new,
            format!("vike_market_making::{old}"),
            "a row keeps the name and moves the crate"
        );
        assert!(
            resolved.contains(&old),
            "`{old}` is not a public root name of `vike_market_making`"
        );
    }
}
