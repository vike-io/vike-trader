//! The one place each un-routable command is refused: an ambiguous, an unheld, or a venue-less account,
//! and the one place an operator's CANCEL that the order registry cannot act on is said out loud.

use super::*;

/// How many held-row ids one line of [`CoreThread::report_unheld_cancel`] names before it says "and N more".
const MAX_ROW_IDS_NAMED: usize = 5;

/// `#id, #id, ... and N more`, the ids exactly as `vike-cli trade recon ls` prints them.
fn named_row_ids(ids: &[u64]) -> String {
    let mut s =
        ids.iter().take(MAX_ROW_IDS_NAMED).map(|i| format!("#{i}")).collect::<Vec<_>>().join(", ");
    if ids.len() > MAX_ROW_IDS_NAMED {
        s.push_str(&format!(" and {} more", ids.len() - MAX_ROW_IDS_NAMED));
    }
    s
}

impl<C: ExecutionClient> CoreThread<C> {
    /// **An operator's cancel that reaches NOTHING is said out loud, never accepted in silence.**
    /// Called only from the external `Command::Order` arm of `dispatch` (a CLI verb, a GUI click, the
    /// MCP door, Telegram), BEFORE the cancel is applied, and it changes nothing about how the cancel
    /// is applied: no venue call, no new cancel path, no event for a registry order.
    ///
    /// The engine cancels only an order its registry holds (`ExecutionEngine::cancel_order_with_intent`
    /// answers `false` and sends nothing otherwise), and a venue order the registry does not hold is
    /// the normal state after a restart on every venue that does not restore its orders (measured on
    /// seven venues on the CI box after the 2026-10-24 restart: Hyperliquid, IBKR, OKX, Bybit, Deribit,
    /// cTrader, OANDA): the pass holds it as an `UnknownOrder` row, and a
    /// `cancel <coid>` / `mass-cancel <venue>` was Acked, logged as `command accepted`, and did
    /// nothing, with no event. Now:
    ///
    /// * `OrderIntent::Cancel(coid)` whose coid is in no engine's registry and is no held bracket exit
    ///   publishes an `OrderCancelRejected` line (reason in plain words, naming the held `UnknownOrder`
    ///   row when one names the coid) and a WARN.
    /// * `OrderIntent::MassCancel` and `OrderIntent::MarketExit` (whose cancel leg is the same registry
    ///   walk) say once that the `UnknownOrder` rows of their scope were NOT cancelled
    ///   ([`Self::report_unreached_rows`]).
    ///
    /// ⚠ **Operator path ONLY, by where it is called.** A STRATEGY's cancel of a registry-less order
    /// (`SpreadMaker`'s `cancel_tagged` of a restored quote before the first pass, or a cancel that
    /// races a fill) goes through `apply_intent_routed` and stays silent exactly as before; it would
    /// be one line per tick if it spoke. An order that IS in a registry but already terminal also stays
    /// silent: this answers "the registry does not hold it", nothing more.
    ///
    /// The line is rendered into the recent-events ring directly rather than published on the bus: the
    /// bus would hand the event to the engine, whose fold counts a lifecycle event for an unknown coid
    /// into `dropped_unknown_coid` (a health counter about VENUE events) and drops it. Nothing is
    /// journaled (the cancel command itself already is), so a replay is unchanged.
    ///
    /// Off the per-message fold: one operator command, and the scan of the held rows runs only on this
    /// miss.
    ///
    /// `#[inline(never)]` (as `set_account_trading_state`, `hold_account` and `account_command_lands`
    /// carry it; `crates/vike-core/CLAUDE.md` asks it of every cold helper `dispatch` calls): it has
    /// ONE call site, the shape the optimizer inlines first, and its body (`format!`, `tracing::warn!`,
    /// iterator chains, the ring write) would grow the function the core-hop gate measures. The same
    /// goes for the two helpers it calls, which only its miss branches reach. No `#[cold]`: the
    /// neighbours carry none, and an operator's cancel is rare, not unlikely.
    #[inline(never)]
    pub(crate) fn report_unheld_cancel(&mut self, intent: &OrderIntent) {
        match intent {
            OrderIntent::Cancel(coid) => {
                let held_by_an_engine = self.engine.registry.contains_key(coid.as_str())
                    || self
                        .extra_engines
                        .iter()
                        .any(|(_, e)| e.registry.contains_key(coid.as_str()));
                if held_by_an_engine || self.held_orders.contains_key(coid.as_str()) {
                    return;
                }
                let reason = self.unheld_cancel_reason(coid);
                tracing::warn!(target: "vike_core::core", coid = %coid, "cancel NOT sent: {reason}");
                let ev = Event::OrderCancelRejected(vike_model::events::OrderCancelRejected {
                    client_order_id: coid.clone(),
                    reason: reason.into(),
                    ts: self.engine.now_ms,
                });
                self.note_event(crate::recent::EventNote::capture(&ev));
            }
            OrderIntent::MassCancel { venue, symbol, account } => self.report_unreached_rows(
                "MassCancel",
                venue.as_deref(),
                symbol.as_deref(),
                account.as_ref(),
            ),
            // The panic button's cancel leg walks the same registry, so it skips the same rows.
            OrderIntent::MarketExit { venue, account } => self.report_unreached_rows(
                "MarketExit (its cancel leg)",
                venue.as_deref(),
                None,
                account.as_ref(),
            ),
            _ => {}
        }
    }

    /// The `MassCancel` / `MarketExit` half of [`Self::report_unheld_cancel`]: one ring note and one
    /// WARN naming the held `UnknownOrder` rows of the command's scope (the venue, and the account's
    /// route key when it names one), which the registry walk cannot reach. Nothing when there are
    /// none.
    ///
    /// Says nothing for a command the cancel itself refuses or ignores: an account named with no
    /// venue (`refuse_venueless_account`), and a symbol named with no venue (the arm notes "symbol
    /// without venue is ignored" and cancels nothing).
    ///
    /// ⚠ **A symbol is not a filter here.** `HeldReconAlert` has no symbol field (the symbol is text
    /// inside `detail`), so with a symbol the count is an upper bound and the line says "up to N".
    /// The symbol is not echoed: it is wire text.
    ///
    /// `#[inline(never)]` for the reason [`Self::report_unheld_cancel`] states; reached from it alone.
    #[inline(never)]
    fn report_unreached_rows(
        &mut self,
        verb: &str,
        venue: Option<&str>,
        symbol: Option<&str>,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
    ) {
        if venue.is_none() && (account.is_some() || symbol.is_some()) {
            return;
        }
        let route =
            venue.zip(account).map(|(v, a)| vike_model::accounts::account_keys::route_key_of(v, a));
        let ids: Vec<u64> = self
            .recon_alerts
            .iter()
            .filter(|(_, h)| {
                h.kind == DivergenceKind::UnknownOrder
                    && venue.is_none_or(|v| h.venue == v)
                    && route.as_deref().is_none_or(|r| h.route_key == r)
            })
            .map(|(&id, _)| id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let scope = venue.unwrap_or("this node");
        let (on, count, tail) = if symbol.is_some() {
            (
                format!("{scope} (a symbol was named)"),
                format!("up to {}", ids.len()),
                "; the held rows are not filed by symbol, so some may rest on another one",
            )
        } else {
            (scope.to_string(), ids.len().to_string(), "")
        };
        let message = format!(
            "{verb} on {on}: {count} order(s) the venue reports are NOT cancelled, because this \
             node's order registry does not hold them (held as UnknownOrder {}{tail}; see `vike-cli \
             trade recon ls`). Nothing was sent for them: cancel them at the venue",
            named_row_ids(&ids)
        );
        tracing::warn!(target: "vike_core::core", unreached = ids.len(), "{message}");
        self.note(message);
    }

    /// The plain-words reason of an unheld cancel, most specific first: the held `UnknownOrder` row
    /// that carries exactly this coid; an order the ownership file remembers that the first
    /// reconcile pass has not judged yet; the rows the node holds; "nothing known by that id".
    ///
    /// The row is matched on the exact `coid=Some("..")` the row's detail and identity print, never
    /// on a substring of the line: cancelling `b6lim` while only `b6lim2` is held names no row, and
    /// neither does `Some`, `BTC` or the venue's name.
    ///
    /// `#[inline(never)]` for the reason [`Self::report_unheld_cancel`] states; reached from it alone.
    #[inline(never)]
    fn unheld_cancel_reason(&self, coid: &str) -> String {
        let rows: Vec<(u64, &HeldReconAlert)> = self
            .recon_alerts
            .iter()
            .filter(|(_, h)| h.kind == DivergenceKind::UnknownOrder)
            .map(|(&id, h)| (id, h))
            .collect();
        let echoed = format!("coid=Some({coid:?})");
        let named = rows
            .iter()
            .find(|(_, h)| h.detail.contains(&echoed) || h.identity.instance.contains(&echoed));
        if let Some((id, h)) = named {
            format!(
                "not in this node's order registry: the venue reports it and this node holds it as \
                 UnknownOrder #{id} ({}), because it was placed before a restart or outside this node. \
                 Nothing was sent to the venue and this node cannot cancel it: cancel it at the venue",
                h.detail
            )
        } else if self.restored_orders.contains_key(coid) {
            "not in this node's order registry yet: the ownership file remembers it from before the \
             restart and the first reconcile pass has not judged it. Nothing was sent to the venue; \
             once the pass has run it is either adopted (cancel it again) or held as an UnknownOrder \
             (then cancel it at the venue)"
                .to_string()
        } else if !rows.is_empty() {
            let ids: Vec<u64> = rows.iter().map(|(id, _)| *id).collect();
            format!(
                "not in this node's order registry, so nothing was sent to the venue. This node holds {} \
                 UnknownOrder row(s) for venue orders its registry does not know ({}; see `vike-cli trade \
                 recon ls`): an order placed before a restart is one of them and still rests at its venue. \
                 It cannot be cancelled from here: cancel it at the venue",
                ids.len(),
                named_row_ids(&ids)
            )
        } else {
            "not in this node's order registry (never placed here, or a mistyped id), so nothing was \
             sent to the venue"
                .to_string()
        }
    }

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
        // An account the composition root knows and does NOT run (decision 0156) carries its reason
        // in `CoreConfig::not_running`; the refusal names it. One linear scan of a list sized by the
        // node's accounts, on a refusal — never the fold.
        let key = vike_model::accounts::account_keys::route_key_of(venue, account);
        let reason = self
            .config
            .not_running
            .iter()
            .find(|nr| nr.route_key == key)
            .map(|nr| nr.reason.as_str());
        let message =
            crate::account_ambiguity::unheld_account_refusal(verb, venue, account, reason);
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
