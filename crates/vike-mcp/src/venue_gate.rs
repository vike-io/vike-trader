//! The ORDER GATE's I/O half on this door: the refusal of a write the one order gate refuses
//! (`crates/vike-client/src/order_gate.rs`'s `judge`, the rule `vike-cli` asks too: a venue or account the
//! node's frame shows it does not run, a bare venue of several accounts, a node that cannot route the venue,
//! a bare submit to a node without the one-account door, an order over this box's notional cap), in this
//! door's words, and the node-frame reads it stands on.
//!
//! It runs inside `Server::call_tool`'s `is_write_tool` arm BEFORE a preview token is minted, on the preview
//! call and the confirm call alike, and once more at the send (`Server::execute`), on the control link's real
//! features; so a new write tool routed through that arm inherits it.
//! `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries the disposition.
//!
//! The decision moved to `vike-client` on 2026-11-24 (the owner's D1-D5): `venue_verdict`, `commanded_account`
//! (once this file's `commanded_venue`), `lane_account`, `bare_venue_keys`, `known_keys` and `mounted_keys` (once
//! `mounted_accounts`, over the frame as raw JSON) are `order_gate.rs`'s `venue_refusal` and its helpers now. Their
//! names stay in this paragraph because records written while they shipped cite them here.

use vike_client::order_gate::{self, NodeLink, VenueCheck};
use vike_client::refusals::{OrderGateRefusal, OrderGateRefusalId};
use vike_client::verbs::Guardrail;
use vike_tradehub_client::wire::CommandWire;

use super::*;

/// What a refusal of the gate ends with on the PREVIEW call: nothing was asked of the node yet.
pub(super) const NOTHING_PREVIEWED: &str =
    "Nothing was previewed, no preview_token was issued, and nothing was sent.";
/// ...and at the SEND, where the confirming call spent its token.
pub(super) const NOTHING_SENT: &str =
    "Its preview_token is spent and nothing was sent: change the command and preview it again.";

impl Server {
    /// **Ask the one order gate about `cmd`**: the node's frame (read only for a command that names a venue or
    /// states a size, so a venue-less verb, the panic button above all, costs no node round trip and cannot be
    /// delayed by one), what the control link said of the node (nothing before it is open, which refuses nothing),
    /// and this box's caps. `Ok` is the guardrail the preview renders and its `venue_check` word; `Err` the refusal
    /// in this door's words, ending `ending`.
    ///
    /// ⚠ **NO EVIDENCE IS NOT A REFUSAL**, except against a node that does not route by venue (decision 0041
    /// verdict 3, amended 2026-11-24): a frame that cannot be read lets the command through as
    /// [`VENUE_CHECK_UNVERIFIED`], because it is read over the node connection the write itself needs, and
    /// [`Server::execute`] fails there anyway. The one configuration where the two come apart is a session holding a
    /// CONTROL key and no OBSERVE key, a declared residual: every documented setup resolves both from one store.
    pub(super) fn vet_commanded_venue(
        &mut self,
        cmd: &CommandWire,
        ending: &str,
    ) -> Result<(Guardrail, &'static str), ToolError> {
        let frame = if order_gate::reads_the_frame(cmd) {
            self.order_gate_frame()
        } else {
            Err("this command names no venue and states no size".to_string())
        };
        let link = self.control.as_ref().map_or_else(NodeLink::default, NodeLink::of);
        match order_gate::judge(cmd, self.caps, frame.as_deref().map_err(String::as_str), link) {
            Ok((guard, check)) => Ok((guard, venue_check_word(check))),
            Err(refusal) => Err(ToolError::refused(gate_refusal(&refusal, ending))),
        }
    }

    /// **This node's ACCOUNT-SET digest right now**, or `None` when the node could not be ASKED.
    ///
    /// `Some(0)` still collapses two states — a node predating the field and a node with no
    /// accounts — and that collapse is sound, because both are the node ANSWERING and neither can
    /// move an epoch it does not publish. Two `Some(0)`s compare EQUAL, which is the pre-field
    /// behaviour exactly.
    ///
    /// ⚠ **An UNREACHABLE node used to be folded into that same `0`, and it is now `None`.** The
    /// two are not the same fact: "the node says its epoch is 0" is an answer, and "I could not
    /// reach the node" is the absence of one. Folding them let the confirm guard report a LOST
    /// LINK as a CHANGED ACCOUNT SET — telling an agent that an account "may have been mounted,
    /// unmounted, or relabelled" when what happened is that the read socket died.
    ///
    /// ⚠ The fold's real cost was not the wording. It made the guard's verdict turn on WHEN
    /// visibility was lost: a read failing at BOTH preview and confirm stamped `0` both times and
    /// compared equal, so the confirm PROCEEDED — while a read failing only at confirm REFUSED.
    /// The case with less information was the one that was allowed through. MEASURED as CI run
    /// 35437491370, where `a_write_after_a_drop_is_refused_once_and_the_next_call_reconnects`
    /// received `(16314315392579917129 -> 0)` in place of the link answer it asserts.
    ///
    /// ⚠ It is still not evidence of anything ON ITS OWN. `None` says nothing about whether the
    /// set moved, and the confirm guard therefore refuses on it in NEITHER direction; a caller
    /// that wants to know whether the LINK is up must ask the thing that writes, not this.
    ///
    /// ⚠ **It reads through [`Server::order_gate_snapshot`] — the wait for a FOLD — and that is held
    /// by its OWN test.** Before a node's first fold its frame carries `accounts_epoch: 0`, so a
    /// read that stopped on it would stamp `0` on a preview whose confirm, taken after the fold,
    /// then refuses an account-set change nothing made. A write naming a venue cannot show this:
    /// [`Server::vet_commanded_venue`] reads first, on the same handle, and has already waited the
    /// fold out. `a_venue_less_preview_against_a_node_that_has_not_folded_stamps_the_post_fold_epoch`
    /// (the node-drop suite) previews `cancel_order`, whose first read of the frame is this one.
    pub(super) fn node_accounts_epoch(&mut self) -> Option<u64> {
        let snapshot = self.order_gate_snapshot().ok()?;
        Some(snapshot["accounts_epoch"].as_u64().unwrap_or(0))
    }
}

/// **A refusal of the one order gate in this door's words**: the rendered line every door prints
/// (`error [<id>]: <text>`, which no template ends with a full stop), this tool's tail (what to read or change),
/// then `ending` ([`NOTHING_PREVIEWED`] or [`NOTHING_SENT`]).
pub(super) fn gate_refusal(refusal: &OrderGateRefusal, ending: &str) -> String {
    let tail = match refusal.id() {
        OrderGateRefusalId::VenueNotMounted | OrderGateRefusalId::AccountNotMounted => {
            " Call node_snapshot and read `venues[].route_key`; the comparison is exact (case-sensitive)."
        }
        OrderGateRefusalId::BareVenueOfSeveralAccounts
        | OrderGateRefusalId::BareSubmitToOlderNode => {
            " On this tool a book is the venue with its label as `account` (`DEFAULT` for the unlabelled one)."
        }
        OrderGateRefusalId::OverMaxNotional => {
            " Lower `qty`; the ceiling is this box's `policy.max_notional_per_order`, which only the operator \
             changes, with `vike-cli config set`."
        }
        OrderGateRefusalId::VenueBlank
        | OrderGateRefusalId::NodeDoesNotRoute
        | OrderGateRefusalId::OrderClientIdCharset
        | OrderGateRefusalId::BracketStemShape => "",
    };
    format!("{}.{tail} {ending}", refusal.rendered())
}

/// The `venue_check` word of the gate's answer.
pub(super) fn venue_check_word(check: VenueCheck) -> &'static str {
    match check {
        VenueCheck::Mounted => VENUE_CHECK_MOUNTED,
        VenueCheck::NoVenue => VENUE_CHECK_NONE,
        VenueCheck::Unverified => VENUE_CHECK_UNVERIFIED,
    }
}

/// The three `venue_check` dispositions a write reports, spelled ONCE.
/// [`Server::vet_commanded_venue`] writes one of them; it is the one field that says whether the
/// venue in `wire_command` was compared against anything at all.
///
/// ⚠ Reported on BOTH halves of the two-call gate — the preview payload ([`preview_of`]) and the
/// answer to the confirmed call ([`Server::call_tool`]'s write arm). The confirm half is not
/// decoration: `unverified` is an ALLOW, so an accepted write that stated nothing would leave the
/// record of the write that actually happened unable to say whether the gate ran.
///
/// `mounted` — the command names a venue and the node reports mounting it.
pub(super) const VENUE_CHECK_MOUNTED: &str = "mounted";
/// `none` — the command names NO venue THE NODE COULD MOUNT, so there is nothing to check. The
/// `close_all` / `mass_cancel` "every engine" shape, and every venue-less verb.
///
/// ⚠ It also covers `delete_series`, whose selector DOES carry a `venue` — a STORE partition, which
/// is not a mount and which the node has no opinion about. Reporting `unverified` there would say a
/// check had been attempted and had failed, which is worse than saying there was none to make.
/// See [`VENUE_CHECK_MOUNTED`].
pub(super) const VENUE_CHECK_NONE: &str = "none";
/// `unverified` — the command names a venue and the node's mounted set could not be READ (no node,
/// no observe key, the node is down, or it has not folded yet). The command was NOT refused on an
/// absence, and the preview says so rather than presenting an unmade check as a passed one. See
/// [`VENUE_CHECK_MOUNTED`].
pub(super) const VENUE_CHECK_UNVERIFIED: &str = "unverified";
