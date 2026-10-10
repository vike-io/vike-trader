//! The one place each un-routable command is refused: an ambiguous, an unheld, or a venue-less account.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// **THE ONE place an ambiguous command is refused**, so no refusal can be written without the
    /// candidates and every one of them reads the same.
    ///
    /// Said TWICE on purpose, to the two surfaces that reach different people: `tracing::error!`
    /// for the JSON log an operator greps after the fact, and [`Self::note`] for the snapshot's
    /// recent-events ring — which the GUI, the Telegram channel and `vike-cli` all render, and
    /// which is the only one the person who just clicked the button is looking at. Off the
    /// per-message fold: this is an order-lowering path.
    pub(crate) fn refuse_ambiguous(&mut self, verb: &str, venue: &str, candidates: &[String]) {
        let message = crate::account_ambiguity::refusal(verb, venue, candidates);
        tracing::error!(
            target: "vike_core::core",
            venue = %venue,
            accounts = candidates.len(),
            "{message}"
        );
        self.note(message);
    }

    /// **THE ONE place a command naming an UNHELD account is refused**, the sibling of
    /// [`Self::refuse_ambiguous`] and said to the same two surfaces for the same reason: the JSON
    /// log an operator greps after the fact, and the recent-events ring the GUI, the Telegram
    /// channel and `vike-cli` render.
    ///
    /// ⚠ The operator-facing answer to this mistake is the node's EDGE refusing the ticket before
    /// the Ack — see `vike_core::account_ambiguity::unheld_account_refusal`, which argues why this
    /// text lists no candidates. What lands here is the core's own record that it declined to
    /// invent a destination, for the callers the edge does not sit in front of.
    ///
    /// Off the per-message fold: an order-lowering path, like its sibling.
    pub(crate) fn refuse_unheld_account(
        &mut self,
        verb: &str,
        venue: &str,
        account: &vike_model::accounts::account_keys::AccountLabel,
    ) {
        let message = crate::account_ambiguity::unheld_account_refusal(verb, venue, account);
        tracing::error!(
            target: "vike_core::core",
            venue = %venue,
            account = %account,
            "{message}"
        );
        self.note(message);
    }

    /// **THE ONE place a command naming an account and NO venue is refused** — the third sibling,
    /// said to the same two surfaces. Reachable only by the risk-REDUCING verbs whose venue is
    /// optional (`MassCancel`, `MarketExit`), where the arm the frame would otherwise reach is the
    /// global one — see `vike_core::account_ambiguity::account_without_venue_refusal`.
    ///
    /// Off the per-message fold: an operator-command path, like its siblings.
    pub(crate) fn refuse_venueless_account(
        &mut self,
        verb: &str,
        account: &vike_model::accounts::account_keys::AccountLabel,
    ) {
        let message = crate::account_ambiguity::account_without_venue_refusal(verb, account);
        tracing::error!(target: "vike_core::core", account = %account, "{message}");
        self.note(message);
    }
}
