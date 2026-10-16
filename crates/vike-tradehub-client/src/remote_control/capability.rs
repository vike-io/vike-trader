//! The client-side refusal table: the `Welcome.features` string a command owes before it is sent.

use crate::proto::{
    FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_ACCOUNT_SCOPED_SUBMIT, FEATURE_BRACKET,
    FEATURE_MOUNT_ACCOUNT, FEATURE_MOUNT_VERBS, FEATURE_PARAMS_BY_MOUNT, FEATURE_SETTINGS_WRITE,
    FEATURE_STRATEGY_VERBS,
};
use crate::wire::WireCommand;

/// The `Welcome.features` capability a [`WireCommand`] requires before it may be SENT, or `None`
/// for the pre-negotiation vocabulary every server speaks. The one authority both write paths
/// (`RemoteControlHandle::try_command_with_reason` and `preview_command`) consult.
///
/// Two failures stand behind an unadvertised string: a node that predates a VERB cannot decode it
/// (a loud `Response::Error`); a node that predates a FIELD decodes the frame, `#[serde(default)]`
/// drops the field, and it acts on the WRONG target behind a normal Ack. The second is why the
/// arm ORDER below is the check.
pub(super) fn required_feature(cmd: &WireCommand) -> Option<&'static str> {
    match cmd {
        // A pre-bracket node cannot decode the variant. It names no account.
        WireCommand::Bracket(_) => Some(FEATURE_BRACKET),
        // ⚠ BEFORE the plain arm: `{ .. }` would swallow it. A `strategy-verbs`-only node drops
        // the `mount_id` and retunes the FIRST mount on the series.
        WireCommand::UpdateParams { mount_id: Some(_), .. } => Some(FEATURE_PARAMS_BY_MOUNT),
        WireCommand::UpdateParams { .. } => Some(FEATURE_STRATEGY_VERBS),
        // The mount verbs ride their OWN string: a B4-era node advertises `strategy-verbs` yet
        // cannot decode them (`FEATURE_MOUNT_VERBS`'s doc).
        // ⚠ BEFORE the arm below, which matches every mount: a node that predates `account`
        // decodes a labelled mount as `account: None` and runs it on the venue's default account.
        WireCommand::MountStrategy { account: Some(_), .. } => Some(FEATURE_MOUNT_ACCOUNT),
        WireCommand::MountStrategy { .. } | WireCommand::UnmountStrategy { .. } => {
            Some(FEATURE_MOUNT_VERBS)
        }
        // Its OWN string, not `settings-show`: a read-half node cannot decode this variant.
        WireCommand::SetSetting { .. } => Some(FEATURE_SETTINGS_WRITE),
        // ⚠ CONDITIONAL on the PAYLOAD: a command naming NO account serialises byte-identically to
        // the pre-field frame and owes nothing. One that NAMES an account is decoded FINE by an old
        // node with `account: None` — silently the wrong book — so this refusal is the only guard.
        // ⚠ BEFORE the `names_an_account` arm, which matches these three too: a node may advertise
        // `account-routing` truthfully (it routes a labelled `Submit`) and still fan these verbs over
        // EVERY account of the exchange. `FEATURE_ACCOUNT_SCOPED_REDUCE`'s doc carries the claim.
        // ⚠ `account: Some(..)` ONLY: an unscoped risk-REDUCING verb fans out to every account by
        // design (§4.5), and the unscoped panic button (`MarketExit { venue: None, account: None }`)
        // must never be refused by any gate in this family. Both fall through to `_ => None`.
        WireCommand::MassCancel { account: Some(_), .. }
        | WireCommand::Flatten { account: Some(_), .. }
        | WireCommand::MarketExit { account: Some(_), .. } => Some(FEATURE_ACCOUNT_SCOPED_REDUCE),
        // What remains naming an account is a `Submit`, and it owes `account-scoped-submit`, NOT
        // `account-routing`: released nodes v0.1.27-v0.1.32 advertise that string yet route a
        // labelled submit by venue alone (`FEATURE_ACCOUNT_SCOPED_SUBMIT`'s doc has the measurement).
        // Every named account owes it, the positive `DEFAULT` included.
        c if names_an_account(c) => Some(FEATURE_ACCOUNT_SCOPED_SUBMIT),
        _ => None,
    }
}

/// Does this command NAME an account? The payload half of [`required_feature`]'s account arms.
///
/// ⚠ `Cancel`/`Modify` are absent deliberately: their client-order-id already names the owning
/// engine, so there is no account to state or misread.
fn names_an_account(cmd: &WireCommand) -> bool {
    match cmd {
        WireCommand::Submit(req) => req.account.is_some(),
        WireCommand::MassCancel { account, .. }
        | WireCommand::Flatten { account, .. }
        | WireCommand::MarketExit { account, .. } => account.is_some(),
        _ => false,
    }
}
