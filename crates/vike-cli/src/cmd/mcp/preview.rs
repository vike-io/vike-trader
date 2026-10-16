//! The preview-token store: what a preview was issued for, single use, expiring, and bound to it.

use std::time::Instant;

use vike_tradehub_client::wire::WireCommand;

use super::PREVIEW_WINDOW;
#[cfg(doc)]
use super::Server;
#[cfg(doc)]
use crate::cmd::verbs;

/// WHAT a preview was issued FOR.
///
/// ⚠ It became an enum when the surface grew a write that is not a node command. `delete_series`
/// removes stored history through a datahub; there is no `WireCommand` for it and there must not
/// be — `crate::cmd::verbs`'s vocabulary is the trade REPL's and the MCP surface's ONE construction
/// site for NODE commands, and widening it to carry a store operation would put a verb in it that
/// the REPL has no business spelling.
///
/// The alternative — a second token store beside [`PendingPreviews`] — was rejected for the reason
/// that module's doc gives about the roster: two stores means two windows, two single-use rules and
/// two binding compares, and the day they disagree is the day a token confirms something it did not
/// preview.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PreviewIntent {
    /// A node command — orders and node lifecycle alike, through `crate::cmd::verbs`.
    Node(Box<WireCommand>),
    /// A stored-series DELETION, through a datahub. See [`DeleteIntent`].
    Delete(DeleteIntent),
}

/// The `delete_series` tool's intent: WHICH series, and under WHICH provenance assertion.
///
/// ⚠ `produced_by` is part of the INTENT and not a side condition, so a token previewed under one
/// assertion cannot confirm a delete under another — which is the whole point of binding a token to
/// what it previewed, applied to the one argument that decides what actually goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeleteIntent {
    pub(super) kind: String,
    pub(super) venue: String,
    pub(super) symbol: Option<String>,
    pub(super) group: Option<String>,
    pub(super) interval: Option<String>,
    pub(super) produced_by: String,
}

/// A preview that was issued and not yet consumed by a confirming call.
pub(super) struct PendingPreview {
    pub(super) intent: PreviewIntent,
    pub(super) issued: Instant,
    /// **The node's ACCOUNT-SET digest as it stood when this preview was taken.**
    ///
    /// A confirm compares the node's CURRENT value against this one and refuses on a difference:
    /// a preview describes a routing decision, and a routing decision made against a set that has
    /// since changed describes a node that no longer exists. `Some(0)` when the node published
    /// none — an older node, or one with no accounts — and two `Some(0)`s compare equal, which is
    /// the pre-field behaviour exactly.
    ///
    /// ⚠ **`None` means the node could not be ASKED, and it is a different fact from any answer
    /// it could have given.** This was a `u64` that folded a failed read into `0`, and the fold
    /// was not merely imprecise — it made the guard's verdict depend on WHEN visibility was lost
    /// rather than on whether the set moved. See [`Server::node_accounts_epoch`].
    pub(super) accounts_epoch: Option<u64>,
}

/// The bounded preview-token store — single use, expiring, and BOUND to the command it previewed.
///
/// ⚠ **THE TOKEN IS NOT A SECRET, and does not need to be.** This is a stdio server: the only party
/// that can send it a request is the agent already holding both ends of the pipe, so there is no
/// third party to withhold it from. What a token proves is that a preview HAPPENED FOR THIS EXACT
/// COMMAND — it encodes a sequence, not an authorization. Guessing a token before any preview finds
/// an empty store; guessing a live one to confirm a DIFFERENT command fails the binding compare in
/// [`Server::call_tool`]. That is the whole property, and a counter delivers it.
///
/// ⚠ The store is bounded by pruning on every issue, not by a cap: an agent that previews without
/// ever confirming would otherwise grow it for the life of the process.
#[derive(Default)]
pub(super) struct PendingPreviews {
    pub(super) by_token: std::collections::BTreeMap<String, PendingPreview>,
    pub(super) next: u64,
}

/// Do two commands express the SAME INTENT — everything the agent specified, ignoring the
/// `client_order_id`?
///
/// ⚠ The id MUST be excluded, and finding out why is what the first version of this gate got wrong:
/// [`verbs::fill_client_order_id`] mints a fresh id on EVERY call, so a preview and its confirm can
/// never carry the same one and an exact compare rejected every confirm — `submit_order` would have
/// been permanently unusable through this surface.
///
/// Excluding it costs nothing, because the confirming call does not execute the command it rebuilt:
/// it executes the STORED one. So the id that reaches the venue is the id the preview displayed and
/// the node dry-ran, which is a stronger property than comparing it would have been.
pub(super) fn same_intent(a: &PreviewIntent, b: &PreviewIntent) -> bool {
    fn without_id(c: &PreviewIntent) -> PreviewIntent {
        let mut c = c.clone();
        if let PreviewIntent::Node(cmd) = &mut c
            && let WireCommand::Submit(o) = cmd.as_mut()
        {
            o.client_order_id = String::new();
        }
        c
    }
    // ⚠ A DELETE intent is compared WHOLE — there is no minted field to exclude, and every one of
    // its parts changes what goes: the four identity dimensions AND the provenance assertion.
    without_id(a) == without_id(b)
}

impl PendingPreviews {
    /// Mint a token for `intent` and remember the binding.
    pub(super) fn issue(&mut self, intent: PreviewIntent, accounts_epoch: Option<u64>) -> String {
        self.by_token.retain(|_, p| p.issued.elapsed() <= PREVIEW_WINDOW);
        self.next += 1;
        let token = format!("pv-{}", self.next);
        self.by_token.insert(
            token.clone(),
            PendingPreview { intent, issued: Instant::now(), accounts_epoch },
        );
        token
    }

    /// Consume a token. REMOVES before the caller executes, which is what makes it single-use even
    /// against a duplicated call. An EXPIRED entry is still returned and consumed — the caller
    /// reports the expiry distinctly, exactly as `PendingConfirms::take` does.
    pub(super) fn take(&mut self, token: &str) -> Option<PendingPreview> {
        self.by_token.remove(token)
    }
}
