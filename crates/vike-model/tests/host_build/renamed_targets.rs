//! Every `vike_model` row of `RENAMED_NAMES` names real root names: its new name resolves there, or,
//! for a row the scripted seam renames have not reached yet, its old one does. So a misspelled row
//! fails here on every tree, before the renames and after them.
//!
//! ⚠ Unlike its sibling `host_build/moved_name.rs`, this file is NOT excluded from the scripted
//! renames: they rewrite the names in the `use` below to the new spellings, which is what keeps it
//! compiling and checking the new names once the old ones are gone.

use vike_model::host_build::moved_name::RENAMED_NAMES;

/// Each listed name resolves at `vike_model`'s root (the `use` is the check).
macro_rules! names_at_the_model_root {
    ($($name:ident),+ $(,)?) => {{
        #[expect(unused_imports, reason = "the import IS the check: the name must resolve there")]
        use vike_model::{$($name),+};
        [$(stringify!($name)),+]
    }};
}

#[test]
fn every_renamed_name_row_names_real_model_root_names() {
    let resolved = names_at_the_model_root!(
        OrderBroker,
        OrderPlacement,
        OrderEvent,
        TradeClosedRow,
        TripleBarrierHit,
        TripleBarrierKind,
        PositionControllerParams,
        TickMark,
        Grid,
        StreamDelay,
        OrderPeg,
        share_quantity,
        time_in_force_expired,
        OrderClientIdOrigin,
        OrderClientIdOriginError,
        is_valid_crypto_order_client_id,
    );
    // the `vike_tactics` and old maker crate rows are held in those crates' own `renamed_names.rs`
    for (_, old, new) in RENAMED_NAMES.into_iter().filter(|(root, _, _)| *root == "vike_model") {
        assert!(
            resolved.contains(&new) || resolved.contains(&old),
            "`RENAMED_NAMES` row `{old}` -> `{new}`: neither name resolves at `vike_model`'s root"
        );
    }
}
