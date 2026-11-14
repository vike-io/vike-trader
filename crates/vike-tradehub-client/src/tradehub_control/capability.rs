//! The client-side refusal table: the `Welcome.features` string a command owes before it is sent.

use crate::proto::{
    FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_ACCOUNT_SCOPED_SUBMIT, FEATURE_ACCOUNT_TRADING_STATE,
    FEATURE_BRACKET, FEATURE_BRACKET_ACCOUNT, FEATURE_BRACKET_STEM, FEATURE_COMBO,
    FEATURE_MOUNT_ACCOUNT, FEATURE_MOUNT_UNLATCH, FEATURE_MOUNT_VERBS, FEATURE_ONE_ACCOUNT_VENUE,
    FEATURE_ORDER_ATTRIBUTES, FEATURE_ORDER_DISPLAY, FEATURE_ORDER_LIST,
    FEATURE_ORDER_MODIFY_TRIGGER, FEATURE_ORDER_PEG, FEATURE_ORDER_RELEASE,
    FEATURE_ORDER_TIME_IN_FORCE, FEATURE_ORDER_TRAILING, FEATURE_PARAMS_BY_MOUNT,
    FEATURE_RECONCILE_CANCEL, FEATURE_RECONCILE_VERBS, FEATURE_SETTINGS_WRITE,
    FEATURE_STRATEGY_VERBS, FEATURE_UNADMIT_SYMBOL, FEATURE_VENUE_RESUME,
};
use crate::wire::{CommandWire, OrderBracketWire};

/// The `Welcome.features` capability a [`CommandWire`] requires before it may be SENT, or `None`
/// for the pre-negotiation vocabulary every server speaks. The one authority both write paths
/// (`TradehubControlHandle::try_command_with_reason` and `preview_command`) consult.
///
/// Two failures stand behind an unadvertised string: a node that predates a VERB cannot decode it
/// (a loud `Response::Error`); a node that predates a FIELD decodes the frame, `#[serde(default)]`
/// drops the field, and it acts on the WRONG target behind a normal Ack. The second is why the
/// arm ORDER below is the check.
pub(super) fn required_feature(cmd: &CommandWire) -> Option<&'static str> {
    match cmd {
        // ⚠ FIRST of the bracket arms: a stem rides `bracket-stem`, which ships in a build newer than
        // `bracket-account`, so its one word covers a stem bracket that also names an account. A node
        // from before it REFUSES the key (`deny_unknown_fields`); this names the missing word instead.
        CommandWire::Bracket(OrderBracketWire { order_client_id: Some(_), .. }) => {
            Some(FEATURE_BRACKET_STEM)
        }
        // ⚠ BEFORE the plain arm below, which matches every bracket. A node from before the field
        // REFUSES a bracket carrying the key (`deny_unknown_fields`), so this guard is not the only
        // one, but a named account owes its own string, `DEFAULT` included.
        CommandWire::Bracket(OrderBracketWire { account: Some(_), .. }) => {
            Some(FEATURE_BRACKET_ACCOUNT)
        }
        // A pre-bracket node cannot decode the variant. This one names no account.
        CommandWire::Bracket(_) => Some(FEATURE_BRACKET),
        // A pre-order-list node cannot decode the variant. Its `account` shipped with the word, so a named one owes
        // nothing more, and a bare venue rides the door's one-account rule, which every such node has.
        CommandWire::OrderList(_) => Some(FEATURE_ORDER_LIST),
        // A pre-combo node cannot decode the variant (wave 6). Its `account` and leg limits ship with the word, the
        // order list's rule.
        CommandWire::Combo(_) => Some(FEATURE_COMBO),
        // ⚠ BEFORE the plain arm: `{ .. }` would swallow it. A `strategy-verbs`-only node drops
        // the `mount_id` and retunes the FIRST mount on the series.
        CommandWire::UpdateParams { mount_id: Some(_), .. } => Some(FEATURE_PARAMS_BY_MOUNT),
        CommandWire::UpdateParams { .. } => Some(FEATURE_STRATEGY_VERBS),
        // The mount verbs ride their OWN string: a B4-era node advertises `strategy-verbs` yet
        // cannot decode them (`FEATURE_MOUNT_VERBS`'s doc).
        // ⚠ BEFORE the arm below, which matches every mount: a node that predates `account`
        // decodes a labelled mount as `account: None` and runs it on the venue's default account.
        CommandWire::MountStrategy { account: Some(_), .. } => Some(FEATURE_MOUNT_ACCOUNT),
        CommandWire::MountStrategy { .. } | CommandWire::UnmountStrategy { .. } => {
            Some(FEATURE_MOUNT_VERBS)
        }
        // Its OWN string, not `settings-show`: a read-half node cannot decode this variant.
        CommandWire::SetSetting { .. } => Some(FEATURE_SETTINGS_WRITE),
        // A pre-recon node cannot decode the variant. It names no account (the held row carries it).
        CommandWire::ReconcileConfirm { .. } => Some(FEATURE_RECONCILE_VERBS),
        // Its OWN string: a `recon-verbs` node that predates the cancel cannot decode the variant.
        CommandWire::ReconcileCancel { .. } => Some(FEATURE_RECONCILE_CANCEL),
        // A pre-resume node cannot decode the variant: refused here, loudly, with nothing sent. The
        // venue it names is the node's to judge (`addressed_venue`).
        CommandWire::ResumeVenue { .. } => Some(FEATURE_VENUE_RESUME),
        // A bare venue (decision 0254): only a node with the door's one-account rule can decode the absent account.
        CommandWire::SetAccountTradingState { account: None, .. }
        | CommandWire::UnadmitSymbol { account: None, .. } => Some(FEATURE_ONE_ACCOUNT_VENUE),
        // A pre-account-state node cannot decode the variant: refused here, loudly, with nothing sent.
        // (Never lowered to the node-wide `SetTradingState`, which would act on every account.)
        CommandWire::SetAccountTradingState { .. } => Some(FEATURE_ACCOUNT_TRADING_STATE),
        // A pre-unadmit node cannot decode the variant: refused here, loudly, with nothing sent (decision 0205).
        CommandWire::UnadmitSymbol { .. } => Some(FEATURE_UNADMIT_SYMBOL),
        // A pre-unlatch node cannot decode the variant: refused here, loudly, with nothing sent (decision 0239).
        CommandWire::UnlatchMount { .. } => Some(FEATURE_MOUNT_UNLATCH),
        // ⚠ CONDITIONAL on the PAYLOAD: a command naming NO account serialises byte-identically to
        // the pre-field frame and owes nothing. One that NAMES an account is decoded FINE by an old
        // node with `account: None` — silently the wrong book — so this refusal is the only guard.
        // ⚠ BEFORE the `names_an_account` arm, which matches these three too: a node may advertise
        // `account-routing` truthfully (it routes a labelled `Submit`) and still fan these verbs over
        // EVERY account of the exchange. `FEATURE_ACCOUNT_SCOPED_REDUCE`'s doc carries the claim.
        // ⚠ `account: Some(..)` ONLY: the unscoped panic button (`MarketExit { venue: None, account: None }`)
        // must never be refused by any gate in this family, and falls through to `_ => None`.
        CommandWire::MassCancel { account: Some(_), .. }
        | CommandWire::Flatten { account: Some(_), .. }
        | CommandWire::MarketExit { account: Some(_), .. } => Some(FEATURE_ACCOUNT_SCOPED_REDUCE),
        // A venue and NO account: an older node fans it over every account of the venue behind a normal
        // Ack, which the owner's ruling forbids, and only a node with the door's rule may be sent one.
        CommandWire::MassCancel { venue: Some(_), account: None, .. }
        | CommandWire::Flatten { account: None, .. }
        | CommandWire::MarketExit { venue: Some(_), account: None } => {
            Some(FEATURE_ONE_ACCOUNT_VENUE)
        }
        // ⚠ The three wave-2 order FIELDS, each dropped silently by a node from before it: a release the order would
        // be sent AT ONCE without, a display it would rest wholly in view without, a modify's trigger it would change
        // the rest without. BEFORE the lifetime arm: the three words ship in ONE build, newer than `order-tif` and
        // `account-scoped-submit` (`vike-tradehub`'s `served_features` advertises them together), so the one word
        // returned covers a submit that also names a lifetime or an account. ONE word per command, so a submit naming
        // both a release and a display owes the release's.
        // The attributes wave's terms FIRST of all: `order-attributes` ships in a build newer than `order-peg`, so its
        // one word covers a submit that also names anything below.
        CommandWire::Submit(req)
            if req.attributes.is_some()
                || req.trail_percent.is_some()
                || req.adjust.is_some()
                || req.release.as_ref().is_some_and(|r| r.venue.is_some()) =>
        {
            Some(FEATURE_ORDER_ATTRIBUTES)
        }
        // The wave-5 peg next: `order-peg` ships in a build newer than `order-trailing`, so its one word
        // covers a pegged submit that also names anything below.
        CommandWire::Submit(req) if req.peg.is_some() => Some(FEATURE_ORDER_PEG),
        // The wave-3 trail and limit offset next: `order-trailing` ships in a build newer than the three, so its
        // one word covers a trailing submit that also names a release, a display, a lifetime or an account.
        CommandWire::Submit(req) if req.trail.is_some() || req.limit_offset.is_some() => {
            Some(FEATURE_ORDER_TRAILING)
        }
        CommandWire::Submit(req) if req.release.is_some() => Some(FEATURE_ORDER_RELEASE),
        CommandWire::Submit(req) if req.display_quantity.is_some() => Some(FEATURE_ORDER_DISPLAY),
        CommandWire::Modify { new_trigger_price: Some(_), .. } => {
            Some(FEATURE_ORDER_MODIFY_TRIGGER)
        }
        // ⚠ BEFORE the `names_an_account` arm below, which matches a `Submit` that names an account: a
        // node from before the field decodes a lifetime-carrying submit FINE and drops the keys, so a
        // `post-only` order would rest as a plain GTC limit that takes liquidity, behind a normal Ack.
        // A node advertising `order-tif` is newer than `account-scoped-submit`, so this one string also
        // covers a submit that names both.
        CommandWire::Submit(req) if req.time_in_force.is_some() || req.gtd_expiry.is_some() => {
            Some(FEATURE_ORDER_TIME_IN_FORCE)
        }
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
/// ⚠ `Cancel`/`Modify` are absent deliberately: their order-client-id already names the owning
/// engine, so there is no account to state or misread.
fn names_an_account(cmd: &CommandWire) -> bool {
    match cmd {
        CommandWire::Submit(req) => req.account.is_some(),
        CommandWire::MassCancel { account, .. }
        | CommandWire::Flatten { account, .. }
        | CommandWire::MarketExit { account, .. } => account.is_some(),
        _ => false,
    }
}
