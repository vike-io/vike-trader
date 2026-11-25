//! The per-mount BUDGET the LIVE start arms — a pure fold from `policy` and the mount rows onto the
//! core's config.
//!
//! The core has always been able to watch a mount's loss (`vike_core::CoreConfig::mount_budgets`, the
//! per-mount budget latch) and nothing outside its own tests ever gave it a budget, so no node could
//! latch a mount and the operator's un-latch (`vike-cli trade strategy reset-budget`) had nothing to clear.
//! This is the source: the `policy.max_mount_loss` and `policy.mount_loss_action` rows, folded ONCE at
//! the live start into `vike_core::CoreConfig::mount_budget_default`
//! (`docs/decisions/0249-a-mounts-loss-budget-has-a-source-a-policy-row-read-at-boot-and-judged-on-the-risk-clock.md`).
//!
//! **The LEVERAGE arm** (that record's amendment of 2026-11-23): the `policy.max_mount_leverage` row rides
//! the same default, and a boot mount whose own `mount.max_leverage` column states a figure gets a KEYED
//! budget (`CoreConfig::mount_budgets`, by the SANITIZED mount id) carrying the lower of the two, so the
//! column may only NARROW the row. A keyed budget exists for exactly those mounts: the core can tell a cap
//! the mount's own column set from the node-wide one by where it found it. A cap above the engine's own
//! applied leverage is inert (the engine's gate binds first): never refused, never clamped here.

use std::collections::HashMap;

use vike_model::MountLossAction;

/// The budget every live mount is held to, folded from `policy.max_mount_loss`,
/// `policy.mount_loss_action` and `policy.max_mount_leverage`.
///
/// `None` when neither the loss nor the leverage row is written — the ordinary state: no default
/// budget, so the core assembles every mount with the budget it always had (none) and the sweep is
/// never armed. `Some` carries the written arms and the mode; the notional arm is deliberately unset
/// (`max_notional: None`): the per-order and per-account exposure ceilings already bound what a mount
/// may hold. The file edge (`vike_config::Policy::apply`) has already refused a nonsense number, so
/// nothing is clamped here. The mode is the row's own `vike_model::MountLossAction`, carried as it is:
/// the core decides on that one type, so no second spelling of the mode stands here.
///
/// ⚠ Called from `live_runtime_with` ONLY. The gate-off `start_paper` arm builds its core through
/// `vike_tradehub_core::StrategyCorePaperConfig`, which carries no budget, by the same ruling that keeps every
/// other `policy.*` ceiling out of it (`vike_config::Policy::max_mount_loss` records the residual).
pub(crate) fn mount_budget_default_from_policy(
    policy: &vike_config::Policy,
) -> Option<vike_execution::MountBudget> {
    if policy.max_mount_loss.is_none() && policy.max_mount_leverage.is_none() {
        return None;
    }
    Some(vike_execution::MountBudget {
        max_loss: policy.max_mount_loss,
        max_notional: None,
        loss_action: policy.mount_loss_action,
        max_leverage: policy.max_mount_leverage,
    })
}

/// **The keyed budgets of the boot mounts whose own `mount.max_leverage` column states a cap**, keyed by
/// the SANITIZED mount id the core keys a mount by (`vike_core::strategy_state::mount_id_with`); `mounts` is
/// each boot mount's `(mount id, column)`.
///
/// A keyed budget WINS over the default in the core, so it carries every arm the default does (the loss
/// and its mode) and the mount's leverage cap: the column, or the `policy.max_mount_leverage` row when the
/// row is lower — the column may only narrow the row (`vike-cli config profile mount set-max-leverage` refuses a
/// higher one at the write; a row lowered since is the one that binds). A mount stating no column gets no
/// key and keeps the default, so a node with no column written builds the map it always built: empty.
pub(crate) fn mount_budgets_from_columns(
    policy: &vike_config::Policy,
    mounts: impl IntoIterator<Item = (String, Option<f64>)>,
) -> HashMap<String, vike_execution::MountBudget> {
    mounts
        .into_iter()
        .filter_map(|(id, column)| {
            let cap = column?;
            let cap = policy.max_mount_leverage.map_or(cap, |row| cap.min(row));
            let budget = vike_execution::MountBudget {
                max_loss: policy.max_mount_loss,
                max_notional: None,
                loss_action: policy.mount_loss_action,
                max_leverage: Some(cap),
            };
            Some((id, budget))
        })
        .collect()
}

/// The one start-up line that says the LOSS budget is ARMED, or `None` for an absent row. A budget an
/// operator wrote and cannot see in the log is a ceiling nobody can check against the node.
pub(crate) fn mount_budget_arming_line(policy: &vike_config::Policy) -> Option<String> {
    let max_loss = policy.max_mount_loss?;
    let act = match policy.mount_loss_action {
        MountLossAction::Flatten => "cancels its own orders and closes its position",
        MountLossAction::CancelOnly => {
            "cancels its own orders and leaves its position (and the stops it cancelled) to the operator"
        }
    };
    Some(format!(
        "MOUNT LOSS BUDGET ARMED: every mounted strategy may lose {max_loss} in its account currency \
         before its mount latches liquidate-only; a breach {act} (judged every 30 s and on every \
         closed bar; `vike-cli trade strategy reset-budget <mount>` clears it when the mount is flat)"
    ))
}

/// The one start-up line that says the node-wide LEVERAGE cap is ARMED, or `None` for an absent row.
pub(crate) fn mount_leverage_arming_line(policy: &vike_config::Policy) -> Option<String> {
    let cap = policy.max_mount_leverage?;
    Some(format!(
        "MOUNT LEVERAGE CAP ARMED: an opening order that would take a mounted strategy's gross notional \
         past {cap} times its account's sizing equity is refused (a mount's own `mount.max_leverage` may \
         only narrow it; a cap above an engine's own leverage changes nothing there)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_config::Policy;

    /// ⚠ **An absent row is NO budget** — the byte-identical path: the live mount hands the core `None`,
    /// so it arms no sweep and no mount can latch, exactly as the node behaved before the key existed.
    #[test]
    fn an_absent_row_is_no_budget_and_says_nothing() {
        let policy = Policy::default();
        assert_eq!(mount_budget_default_from_policy(&policy), None);
        assert_eq!(mount_budget_arming_line(&policy), None);
        assert_eq!(mount_leverage_arming_line(&policy), None);
        // The action alone arms nothing: with no budget there is nothing to breach.
        let policy = Policy { mount_loss_action: MountLossAction::CancelOnly, ..Policy::default() };
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
        assert_eq!(budget.max_leverage, None, "no leverage row, no leverage arm");
        assert_eq!(budget.loss_action, MountLossAction::Flatten, "the default mode stops the loss");
        assert!(budget.is_active(), "an inactive budget would arm nothing");
    }

    /// The leverage row alone is a budget with the leverage arm only; both rows ride one budget.
    #[test]
    fn the_leverage_row_rides_the_default_budget_with_or_without_the_loss() {
        let leverage = Policy { max_mount_leverage: Some(3.0), ..Policy::default() };
        let budget = mount_budget_default_from_policy(&leverage).expect("a budget");
        assert_eq!((budget.max_loss, budget.max_leverage), (None, Some(3.0)));
        assert_eq!(mount_budget_arming_line(&leverage), None, "no loss row, no loss line");
        let line = mount_leverage_arming_line(&leverage).expect("a line");
        assert!(line.contains("past 3 times"), "{line}");

        let both = Policy { max_mount_loss: Some(50.0), ..leverage };
        let budget = mount_budget_default_from_policy(&both).expect("a budget");
        assert_eq!((budget.max_loss, budget.max_leverage), (Some(50.0), Some(3.0)));
    }

    /// **A column NARROWS the row and never widens it**, a keyed budget carries the loss arm the
    /// default carries (a keyed entry wins in the core), and a mount stating no column gets no key.
    #[test]
    fn a_column_narrows_the_row_and_keys_only_its_own_mount() {
        let mounts = [
            ("bybit__BTCUSDT__1m".to_string(), Some(2.0)),
            ("bybit__ETHUSDT__1m".to_string(), Some(8.0)),
            ("bybit__SOLUSDT__1m".to_string(), None),
        ];
        let policy = Policy {
            max_mount_loss: Some(50.0),
            mount_loss_action: MountLossAction::CancelOnly,
            max_mount_leverage: Some(5.0),
            ..Policy::default()
        };
        let keyed = mount_budgets_from_columns(&policy, mounts.clone());
        assert_eq!(keyed.len(), 2, "no column, no key: {keyed:?}");
        let narrowed = keyed["bybit__BTCUSDT__1m"];
        assert_eq!(narrowed.max_leverage, Some(2.0), "the column narrows the row");
        assert_eq!(narrowed.max_loss, Some(50.0), "the loss arm rides the keyed budget");
        assert_eq!(narrowed.loss_action, MountLossAction::CancelOnly);
        let higher = keyed["bybit__ETHUSDT__1m"];
        assert_eq!(higher.max_leverage, Some(5.0), "the row binds a higher column");

        // No row: the column alone is the cap, and the loss arm stays as unwritten as its row.
        let keyed = mount_budgets_from_columns(&Policy::default(), mounts);
        let alone = keyed["bybit__ETHUSDT__1m"];
        assert_eq!((alone.max_leverage, alone.max_loss), (Some(8.0), None));

        // No column anywhere: the map the core always had, empty.
        let none = [("bybit__BTCUSDT__1m".to_string(), None)];
        assert!(mount_budgets_from_columns(&policy, none).is_empty());
    }

    /// The mode is carried both ways round.
    #[test]
    fn the_mode_reaches_the_budget_both_ways() {
        for action in MountLossAction::ALL {
            let policy = Policy {
                max_mount_loss: Some(1.5),
                mount_loss_action: action,
                ..Policy::default()
            };
            let budget = mount_budget_default_from_policy(&policy).expect("a budget");
            assert_eq!(budget.loss_action, action, "{action:?}");
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
            mount_loss_action: MountLossAction::CancelOnly,
            ..Policy::default()
        };
        let line = mount_budget_arming_line(&cancel).expect("a line");
        assert!(line.contains("leaves its position"), "{line}");
    }
}
