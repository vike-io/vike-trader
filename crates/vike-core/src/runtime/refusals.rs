//! The one place each un-routable command is refused: an ambiguous, an unheld, or a venue-less account,
//! an explicit client order id the engine already holds answered or finished, and the one place an
//! operator's CANCEL that the order registry cannot act on is said out loud (and, for the one explicit
//! verb decision 0196 rule (b) allows, `recon cancel <id>`, sent).

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

/// The client order id a held `UnknownOrder` row prints as `coid=Some("..")` in its identity or its
/// detail (`Divergence::identity_detail` / `describe`), the one spelling [`CoreThread::unheld_cancel_reason`]
/// also matches on. `None` for a row that prints `coid=None` (IG, FXCM, Polymarket report none), for a
/// keyed row that names only the venue's order id, and for an id that needs `Debug` escapes (a `\` or a
/// `"` in it): a fail-closed read, because a wrong id would cancel a different order.
fn held_row_coid(h: &HeldReconAlert) -> Option<&str> {
    [h.identity.instance.as_str(), h.detail.as_str()].into_iter().find_map(|text| {
        let coid = text.split_once("coid=Some(\"")?.1.split_once("\")")?.0;
        (!coid.is_empty() && !coid.contains(['\\', '"'])).then_some(coid)
    })
}

/// The unified symbol a held `UnknownOrder` row prints just before its coid: `Divergence::identity_detail`
/// and `Divergence::describe` (`vike_exec::recon`) both open `"{venue} {symbol} coid=Some(\"..\")"`, and
/// `crates/vike-core/src/runtime/tests/unheld_cancel.rs` builds that text from the real divergence, so
/// a change of the format there fails this reader's test rather than this function. `None` for a row
/// that prints no coid (the symbol sits in the same sentence), and for any text of another shape: a
/// fail-closed read, the caller then sends the cancel exactly as it did before the symbol was read.
///
/// shortcut: a text read like [`held_row_coid`], because the row keeps no symbol field and the alert
/// type that would carry one is `vike_exec::recon::ReconAlert` (a measured crate); upgrade it with the
/// symbol on the shared seam (decision 0175), which makes the whole teaching below unnecessary.
pub(super) fn held_row_symbol(h: &HeldReconAlert) -> Option<&str> {
    [h.identity.instance.as_str(), h.detail.as_str()].into_iter().find_map(symbol_before_coid)
}

/// [`held_row_symbol`]'s text half: `"{venue} {symbol} coid=Some(\"..\")..."` to `symbol` (a symbol
/// may hold a `/` or a `.`; the venue id holds no space, so the first space ends it).
pub(super) fn symbol_before_coid(text: &str) -> Option<&str> {
    let (head, _) = text.split_once(" coid=Some(\"")?;
    let symbol = head.split_once(' ')?.1;
    (!symbol.is_empty()).then_some(symbol)
}

/// Does `venue`'s client take a `(coid, symbol)` pair for an order of ours that rests on the venue,
/// outside the restore step? Only where BOTH existing tables say the pair is true and the client is
/// already taught it in the live system: the venue's restore is open
/// (`vike_venues::venue_restore::restores_orders`) AND its reports carry OUR coid (`echoes_coid`).
///
/// ⚠ **The second term is the one that matters.** Hyperliquid's row prints the venue's own hashed
/// `cloid`, not a coid: `adopt_restored` would seed its exec thread's order map under that hex, and
/// `crates/bridges/hyperliquid/src/exec/state.rs`'s `cancel_any` then hashes it a second time instead
/// of sending the wire path that `recon cancel` was proven on. So the pair is never taught there.
/// ⚠ **The first term keeps every other venue exactly as it was**: a venue whose restore is off (ibkr,
/// oanda, aster, ctrader, ...) reads `adopt_restored` as "the engine holds this order" (ibkr then
/// emits that order's fills), which a row the registry does not hold is not.
pub(super) fn row_symbol_is_taught(venue: &str) -> bool {
    vike_venues::venue_restore::restores_orders(venue)
        && vike_venues::venue_coid_budget::coid_budget_for(venue).echoes_coid()
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
                // An operator's core-held stop is cancelled by its arm id (`cancel_armed`): not unheld.
                let core_held_stop =
                    self.cond_engine.get(coid.as_str()).is_some_and(|a| a.reduce_only);
                if held_by_an_engine
                    || self.held_orders.contains_key(coid.as_str())
                    || core_held_stop
                {
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
            // `Flatten` is deliberately absent: it lowers to reduce-only MARKET legs on the positions
            // and has no cancel leg at all (`apply/exit.rs`'s `lower_flatten`), so it skips no held
            // row and there is nothing to say. Its cancel-bearing sibling is `MarketExit`, above.
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
                 Nothing was sent to the venue and `cancel` cannot reach it: cancel it at the venue \
                 (or, on a demo account, `vike-cli trade recon cancel {id}` sends the cancel)",
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

    /// **The operator's `recon cancel <id>` (decision 0196, rule (b)): ONE cancel, sent for the order
    /// ONE held `UnknownOrder` row names.** Nothing else ever sends such a cancel: [`Self::report_unheld_cancel`]
    /// only says that a `cancel` / `mass-cancel` / `market-exit` could not reach the row, and no strategy,
    /// GUI or MCP command builds [`Command::CancelReconOrder`].
    ///
    /// It checks, in this order, and a failed check is a ring note plus a WARN and **sends nothing**:
    /// the row exists; it is an `UnknownOrder` row; its account's latest pass still raises it
    /// ([`recon_pass::is_stale`]); the core runs an engine for its account; that engine's tier is
    /// `demo` (a `paper` book has no venue order and a `live` one is the owner's call, 0196); the row
    /// carries a client id the venue can cancel by ([`held_row_coid`]); and no registry holds that id (a
    /// registry order is cancelled by `cancel`, never by this verb).
    ///
    /// The cancel goes through the account's own client, `ExecutionClient::cancel_with_intent` with
    /// [`CancelIntent::Unspecified`] (never held back), the entry the engine itself calls once its registry
    /// has found the order; there is no new trait method. Whether the venue's adapter can act on an id
    /// this process did not place is the adapter's own fact (decision 0196 lists the venues; Hyperliquid's
    /// bridge reads the id as the venue's own `cloid` and cancels it as printed); the note
    /// therefore says SENT, never "cancelled". It folds nothing and removes no row: the order's end
    /// arrives as a venue event, and the row clears at the next reconcile pass that no longer raises it.
    ///
    /// ⚠ **The client is told the row's symbol FIRST** (`ExecutionClient::adopt_restored`, FIFO with the
    /// cancel behind it, over the trade link too), where [`row_symbol_is_taught`] says the pair is
    /// true. A per-symbol router (binance, bybit, okx) learns a foreign order's symbol only from its
    /// own submit or from this call, and sends a cancel of a coid it never heard of to the MOUNTED
    /// symbol, where the venue's "unknown order" is swallowed as "already gone": the verb said SENT,
    /// the order kept resting and the row stayed (`crates/bridges/binance/src/per_symbol.rs`'s
    /// `CoidSymbols`). It registers nothing in the engine and folds nothing.
    ///
    /// `#[inline(never)]` for the reason [`Self::report_unheld_cancel`] states: one call site in
    /// `dispatch`, and a body of `format!`s, a scan and a client call. A cold operator command.
    #[inline(never)]
    pub(crate) fn cancel_recon_order(&mut self, id: u64) {
        let Some(held) = self.recon_alerts.get(&id) else {
            return self.refuse_recon_cancel(
                id,
                "no held row has that id (cleared by a later pass, or not raised since the restart): \
                 run `vike-cli trade recon ls`"
                    .to_string(),
            );
        };
        let (kind, venue, route_key, detail) =
            (held.kind, held.venue.clone(), held.route_key.clone(), held.detail.clone());
        let stale = recon_pass::is_stale(&self.recon_pass, held);
        let coid = held_row_coid(held).map(str::to_string);
        let symbol = held_row_symbol(held).map(str::to_string);
        if kind != DivergenceKind::UnknownOrder {
            return self.refuse_recon_cancel(
                id,
                format!(
                    "this is a {kind:?} row; the verb cancels the venue order of an UnknownOrder row and \
                     nothing else (`vike-cli trade recon confirm` resolves the other kinds)"
                ),
            );
        }
        if stale {
            return self.refuse_recon_cancel(
                id,
                format!("the latest pass of {route_key} no longer raises it; nothing was sent"),
            );
        }
        let Some(eidx) = self.engine_idx_for_route_key(RouteKey::declared(&route_key)) else {
            return self
                .refuse_recon_cancel(id, format!("this node runs no engine for {route_key}"));
        };
        let tier = self.eng(eidx).tier;
        if tier != vike_model::AccountTier::Demo {
            return self.refuse_recon_cancel(
                id,
                format!(
                    "{route_key} runs at the {} tier and this verb works on a demo account only: \
                     cancel the order at the venue",
                    tier.as_str()
                ),
            );
        }
        let Some(coid) = coid else {
            return self.refuse_recon_cancel(
                id,
                "this row carries no id the venue can cancel by: cancel the order at the venue"
                    .to_string(),
            );
        };
        let registry_holds = self.engine.registry.contains_key(coid.as_str())
            || self.extra_engines.iter().any(|(_, e)| e.registry.contains_key(coid.as_str()));
        if registry_holds {
            return self.refuse_recon_cancel(
                id,
                format!(
                    "this node's order registry holds {coid}: use `vike-cli trade order cancel {coid}`; \
                     this verb is for orders the registry does not hold"
                ),
            );
        }
        if let Some(symbol) = symbol.filter(|_| row_symbol_is_taught(&venue)) {
            self.eng_mut(eidx).client.adopt_restored(&[(coid.clone(), symbol)]);
        }
        self.eng_mut(eidx).client.cancel_with_intent(&coid, CancelIntent::Unspecified);
        self.pump_client();
        tracing::info!(
            target: recon_pass::CONFIRM_VERDICT_TARGET, id, account = %route_key, coid = %coid,
            "reconcile cancel SENT"
        );
        self.note(format!(
            "RECON cancel #{id}: cancel SENT to {venue} for coid {coid} [account {route_key}] ({detail}). \
             Nothing was folded and the row stays until a reconcile pass no longer raises it; if it is \
             still listed after the next pass the venue did not cancel the order"
        ));
    }

    /// The refusal half of [`Self::cancel_recon_order`]: a ring note and a WARN on the confirm-verdict
    /// target (which the daemon's file layer keeps at `warn`), nothing sent. `#[inline(never)]` as its caller.
    #[inline(never)]
    fn refuse_recon_cancel(&mut self, id: u64, why: String) {
        tracing::warn!(
            target: recon_pass::CONFIRM_VERDICT_TARGET, id, why = %why,
            "reconcile cancel REFUSED"
        );
        self.note(format!("RECON cancel #{id}: REFUSED, nothing sent: {why}"));
    }

    /// The `confirm_recon` twin of the teaching in [`Self::cancel_recon_order`]: the orders an operator's
    /// confirm of a `JournalDivergence` just put back into the registry (`reregister_orders`) are
    /// announced to the account's client as the restore announces an adopted one, so a later `cancel`
    /// of one lands on its own symbol and not the mounted one. Where [`row_symbol_is_taught`] says
    /// so; an order with no coid or no symbol is skipped (nothing to teach). `#[inline(never)]`
    /// as its caller's other cold helpers; reached from `confirm_recon` only.
    #[inline(never)]
    pub(super) fn announce_recovered_orders(
        &mut self,
        idx: usize,
        venue: &str,
        orders: &[vike_model::OrderStatusReport],
    ) {
        if !row_symbol_is_taught(venue) {
            return;
        }
        let pairs: Vec<(String, String)> = orders
            .iter()
            .filter(|r| !r.symbol.is_empty())
            .filter_map(|r| Some((r.client_order_id.clone()?, r.symbol.clone())))
            .collect();
        if !pairs.is_empty() {
            self.eng_mut(idx).client.adopt_restored(&pairs);
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

    /// **The status of the order engine `eidx` holds under `coid`, when the venue has ANSWERED it or
    /// it is FINISHED**; `None` for a free id and for an order still unanswered (`INITIALIZED`,
    /// `SUBMITTED`). The one question `lower_submit` asks of an operator's explicit coid (decision 0207).
    ///
    /// The unanswered set is the in-flight RETRY, not a reuse: the first send is on its way, the
    /// gatewayhub's per-account dedup drops the repeat, and the venue's answer lands on the entry the
    /// retry wrote. Every other status is the case the registry's `insert` would turn into a lost
    /// order: an answered live order replaced by an `INITIALIZED` entry nobody will answer (the stuck-order
    /// watchdog then rejects it locally while the venue still holds the original), or a finished order's
    /// id given to a command the dedup swallows.
    ///
    /// ONE registry probe (a hash and a bucket), on the routed engine only: the adapters' dedup is per
    /// account, so the same id on another account's engine is another order. Only a submit that brought
    /// its own coid asks (`lower_submit`'s `minted` branch skips it), which is why this is the whole
    /// cost on the measured submit hop and nothing on a strategy's. `#[inline]` so the probe is the
    /// only call it adds to `lower_submit`.
    #[inline]
    pub(crate) fn answered_coid_status(
        &self,
        eidx: usize,
        coid: &str,
    ) -> Option<vike_model::OrderStatus> {
        use vike_model::OrderStatus::{Initialized, Submitted};
        self.eng(eidx)
            .registry
            .get(coid)
            .map(|mo| mo.status)
            .filter(|status| !matches!(status, Initialized | Submitted))
    }

    /// **THE ONE place an explicit coid the engine holds is refused** ([`Self::answered_coid_status`]
    /// says which statuses): said to the JSON log and to the recent-events ring, exactly as the
    /// account refusals above, and NOTHING else. No event is published and no registry entry is
    /// written: an `OrderRejected` or `OrderDenied` under this coid would be applied to the HELD order
    /// (the id is its key), and the order this command asked for does not exist.
    ///
    /// The words are the door's (`crates/vike-tradehub/src/server/refusal.rs`'s `coid_reuse_refusal`),
    /// which this core cannot name (the edge sits above it); the door speaks first from the published
    /// snapshot and this speaks for the window that snapshot leaves, or for a caller the door does not
    /// sit in front of.
    /// Deterministic: it reads the replayed registry, so a replay of the journaled `Cmd` refuses the
    /// same command the live run did.
    ///
    /// `#[cold]` + `#[inline(never)]`: reached from `lower_submit` only on a refusal, and its
    /// `format!`/`warn!` must not grow the function the submit hop measures.
    #[cold]
    #[inline(never)]
    pub(crate) fn refuse_held_coid(&mut self, coid: &str, status: vike_model::OrderStatus) {
        let status_word = status.as_str();
        let message = if status.is_terminal() {
            format!(
                "order {coid} REFUSED, nothing sent: coid {coid} is already used by an order that \
                 is {status_word}: pick a new client order id"
            )
        } else {
            format!(
                "order {coid} REFUSED, nothing sent: coid {coid} is already used by an order that \
                 is {status_word} (still live): pick a new client order id, or change that order by \
                 this id with modify or cancel"
            )
        };
        tracing::warn!(target: "vike_core::core", coid = %coid, status = status_word, "{message}");
        self.note(message);
    }
}
