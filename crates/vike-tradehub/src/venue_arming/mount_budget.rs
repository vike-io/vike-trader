//! The per-mount LOSS BUDGET the LIVE start arms — a pure fold from `policy` onto the core's config.
//!
//! The core has always been able to watch a mount's loss (`vike_core::CoreConfig::mount_budgets`, the
//! per-mount budget latch) and nothing outside its own tests ever gave it a budget, so no node could
//! latch a mount and the operator's un-latch (`vike-cli trade mount-unlatch`) had nothing to clear.
//! This is the source: the `policy.max_mount_loss` and `policy.mount_loss_action` rows, folded ONCE at
//! the live start into `vike_core::CoreConfig::mount_budget_default`
//! (`docs/decisions/0249-a-mounts-loss-budget-has-a-source-a-policy-row-read-at-boot-and-judged-on-the-risk-clock.md`).

/// The budget every live mount is held to, folded from `policy.max_mount_loss` and
/// `policy.mount_loss_action`.
///
/// `None` when the row is ABSENT — the ordinary state: no default budget, so the core assembles every
/// mount with the budget it always had (none) and the sweep is never armed. `Some` carries the loss
/// number and the mode; the notional arm is deliberately unset (`max_notional: None`): this source is a
/// LOSS budget, and the per-order and per-account exposure ceilings already bound what a mount may
/// hold. The file edge (`vike_config::Policy::apply`) has already refused a non-positive or non-finite
/// number, so nothing is clamped here, and the `match` on the mode is exhaustive, so a variant added
/// to `vike_config::MountLossActionSetting` is a compile error here rather than a spelling that
/// silently maps to the other mode.
///
/// ⚠ Called from `live_runtime_with` ONLY. The gate-off `start_paper` arm builds its core through
/// `vike_tradehub_core::PaperCoreOpts`, which carries no budget, by the same ruling that keeps every
/// other `policy.*` ceiling out of it (`vike_config::Policy::max_mount_loss` records the residual).
pub(crate) fn mount_budget_default_from_policy(
    policy: &vike_config::Policy,
) -> Option<vike_exec::MountBudget> {
    let max_loss = policy.max_mount_loss?;
    Some(vike_exec::MountBudget {
        max_loss: Some(max_loss),
        max_notional: None,
        flatten_on_breach: match policy.mount_loss_action {
            vike_config::MountLossActionSetting::Flatten => true,
            vike_config::MountLossActionSetting::CancelOnly => false,
        },
    })
}

/// The one start-up line that says the budget is ARMED, or `None` for an absent row. A budget an
/// operator wrote and cannot see in the log is a ceiling nobody can check against the node.
pub(crate) fn mount_budget_arming_line(policy: &vike_config::Policy) -> Option<String> {
    let budget = mount_budget_default_from_policy(policy)?;
    let act = if budget.flatten_on_breach {
        "cancels its own orders and closes its position"
    } else {
        "cancels its own orders and leaves its position (and the stops it cancelled) to the operator"
    };
    Some(format!(
        "MOUNT LOSS BUDGET ARMED: every mounted strategy may lose {} in its account currency \
         before its mount latches liquidate-only; a breach {act} (judged every 30 s and on every \
         closed bar; `vike-cli trade mount-unlatch <mount>` clears it when the mount is flat)",
        policy.max_mount_loss.unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_config::{MountLossActionSetting, Policy};

    /// ⚠ **An absent row is NO budget** — the byte-identical path: the live mount hands the core `None`,
    /// so it arms no sweep and no mount can latch, exactly as the node behaved before the key existed.
    #[test]
    fn an_absent_row_is_no_budget_and_says_nothing() {
        let policy = Policy::default();
        assert_eq!(mount_budget_default_from_policy(&policy), None);
        assert_eq!(mount_budget_arming_line(&policy), None);
        // The action alone arms nothing: with no budget there is nothing to breach.
        let policy =
            Policy { mount_loss_action: MountLossActionSetting::CancelOnly, ..Policy::default() };
        assert_eq!(mount_budget_default_from_policy(&policy), None);
    }

    /// A written budget reaches the core as a LOSS-only, ACTIVE budget, flattening by default.
    #[test]
    fn a_written_row_is_an_active_loss_budget_that_flattens_by_default() {
        let policy = Policy { max_mount_loss: Some(50.0), ..Policy::default() };
        let budget = mount_budget_default_from_policy(&policy).expect("a budget");
        assert_eq!(budget.max_loss, Some(50.0));
        assert_eq!(
            budget.max_notional, None,
            "a LOSS budget: the notional arm is not this source's"
        );
        assert!(budget.flatten_on_breach, "the default mode stops the loss");
        assert!(budget.is_active(), "an inactive budget would arm nothing");
    }

    /// The mode is carried both ways round.
    #[test]
    fn the_mode_reaches_the_budget_both_ways() {
        for (action, flatten) in
            [(MountLossActionSetting::Flatten, true), (MountLossActionSetting::CancelOnly, false)]
        {
            let policy = Policy {
                max_mount_loss: Some(1.5),
                mount_loss_action: action,
                ..Policy::default()
            };
            let budget = mount_budget_default_from_policy(&policy).expect("a budget");
            assert_eq!(budget.flatten_on_breach, flatten, "{action:?}");
        }
    }

    /// The start-up line names the number and the mode, and the cancel-only line says what it leaves.
    #[test]
    fn the_arming_line_names_the_number_and_the_mode() {
        let flatten = Policy { max_mount_loss: Some(250.0), ..Policy::default() };
        let line = mount_budget_arming_line(&flatten).expect("a line");
        assert!(line.contains("250") && line.contains("closes its position"), "{line}");
        let cancel = Policy {
            max_mount_loss: Some(250.0),
            mount_loss_action: MountLossActionSetting::CancelOnly,
            ..Policy::default()
        };
        let line = mount_budget_arming_line(&cancel).expect("a line");
        assert!(line.contains("leaves its position"), "{line}");
    }
}
