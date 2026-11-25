//! `MountLossAction`: what a mount's loss budget does when it is breached.
//!
//! ONE type for the `policy.mount_loss_action` row (`vike_config::Policy::mount_loss_action`) and for
//! the budget the core enforces (`vike_execution::MountBudget`), with ONE default, `flatten`
//! (decision 0249 item 6: "flatten unless the operator writes cancel_only"). It was a vike-config
//! file-spelling enum, a `bool` on the budget with a default of its own (cancel-only) and a mapping
//! between the two in the composition root; it sits here because vike-config is below vike-execution
//! and both name this crate (the `DeadManAction` shape, `crate::risk::dead_man`).

use serde::{Deserialize, Serialize};

/// What a mount's loss budget does when it is breached.
///
/// Serde `snake_case`: the stored spellings are `"flatten"` and `"cancel_only"` (rows already
/// written keep their bytes), and an unknown one fails at deserialization naming the legal two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountLossAction {
    /// Cancel the breaching mount's own resting orders AND send a reduce-only market order that
    /// closes its attributed position, offered again at every sweep until the mount is flat.
    /// **The default**: a loss ceiling exists to stop the loss, and the other mode cancels the
    /// mount's protective stops with the rest of its orders and leaves the position standing with
    /// no cover.
    #[default]
    Flatten,
    /// Cancel the mount's own resting orders and discard its new intents, and leave its position
    /// for the operator to close. ⚠ The cancel takes the mount's resting STOPS too: the position it
    /// leaves is naked until a human closes it.
    CancelOnly,
}

vike_utility::word_enum! {
    impl MountLossAction {
        Flatten => "flatten",
        CancelOnly => "cancel_only",
    }
}
