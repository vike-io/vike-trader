//! Strategy-mount data: `Mount`, `MountLeg`, the buffered broker intents and readiness. The per-mount
//! budget a caller hands in, `vike_execution::MountBudget`, is read-model data and lives in
//! vike-execution, and so does the per-mount attribution ledger, `vike_execution::MountAttribution`, so
//! the live core and the backtest fold a mount's fills by one law.

use super::*;

/// One buffered order intent from a strategy handler, drained after the handler returns. `tag` is
/// a strategy-chosen stable id (HFT modify surface) — `None` for an untagged order (the order seam's
/// `place` under tag `""`, and every inherent verb that takes no tag).
///
/// `pub` (with the sibling `Buffered*` intents + [`LiveBroker`]'s buffer fields below) so the
/// maker files' white-box tests can build a `LiveBroker` and read back the verbs the generic
/// `SpreadMaker` pushed — a TEST-only surface (the strategy-lab crate compiling them dev-deps vike-core);
/// the live path still populates/drains these internally exactly as before.
#[derive(Default)]
pub struct BufferedSubmit {
    /// The symbol the STRATEGY named, when it named one. `None` (every inherent verb —
    /// `order_target`, `set_holdings`, the tagged HFT lane) means "this mount's own symbol".
    ///
    /// It is recorded UNCONDITIONALLY by the order seam's verbs and resolved at DRAIN time against
    /// the mount's [`Mount::symbols`] declaration: an undeclared mount ignores it exactly
    /// as before. Recording here rather than deciding in the broker is what keeps all nine
    /// `LiveBroker` construction sites untouched.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    pub order_type: vike_model::OrderType,
    pub price: Option<f64>,
    /// The trigger of a stop kind (`OrderRequest::trigger_price`), a trailing kind's activation level;
    /// `None` for every other verb.
    pub trigger: Option<f64>,
    /// A trailing kind's distance (`OrderRequest::trail`); `None` for every verb but
    /// [`LiveBroker::submit_trailing_reduce`].
    pub trail: Option<f64>,
    /// A trailing-limit's offset (`OrderRequest::limit_offset`); `None` for every other order.
    pub limit_offset: Option<f64>,
    pub reduce_only: bool,
    pub tag: Option<String>,
    /// The order's LIFETIME: `Gtc` for every order seam verb and every inherent verb that names none
    /// (the order as it always was); [`LiveBroker::submit_limit_post_only`] and
    /// [`LiveBroker::submit_limit_time_in_force`] are the two that set another. Meaningful on a limit order only —
    /// `drain_broker` refuses the rest in words (`vike_model::lifetime_refusal`, judged at the drain's
    /// `now`, beside `vike_model::order_terms_refusal`) as a `Denied` the mount hears, rather than drop it.
    /// An operator's order is judged by the same rule at the node's door, not here.
    pub time_in_force: vike_model::TimeInForce,
    /// The expiry (epoch ms) of a `Gtd` order; `None` for every other lifetime.
    pub gtd_expiry: Option<i64>,
    /// When the core may send it (`OrderRequest::release`): `None` (every verb but
    /// [`LiveBroker::submit_market_at`] and [`LiveBroker::submit_limit_at`]) is now. A release not
    /// after the drain's `now` is refused by `vike_model::lifetime_refusal` as a `Denied`.
    pub release: Option<vike_model::OrderRelease>,
    /// A limit's attributes (`OrderRequest::attributes`); `None` for every verb but
    /// [`LiveBroker::submit_limit_attributed`]. Judged by `vike_model::attributes_refusal` at the drain.
    pub attributes: Option<vike_model::OrderAttributes>,
    /// A trailing kind's PERCENT distance (`OrderRequest::trail_percent`); `None` for every verb but
    /// [`LiveBroker::submit_trailing_percent`].
    pub trail_percent: Option<f64>,
    /// A stop's adjustment (`OrderRequest::adjust`); `None` for every verb but
    /// [`LiveBroker::submit_stop_adjustable`]. Judged by `vike_model::stop_adjust_refusal` at the drain.
    pub adjust: Option<vike_model::OrderStopAdjust>,
    /// A pegged order's peg (`OrderRequest::peg`); `None` for every verb but
    /// [`LiveBroker::submit_pegged_tagged`]. Judged by `vike_model::order_terms_refusal` at the drain.
    pub peg: Option<vike_model::OrderPeg>,
    /// A limit's visible size (`OrderRequest::display_quantity`); `None` for every verb but
    /// [`LiveBroker::submit_limit_display_tagged`]. Judged by `vike_model::display_refusal` at the drain.
    pub display_quantity: Option<f64>,
    /// A refusal the strategy made itself (`vike_model::OrderBroker::deny`), in its words: the drain
    /// denies the order by them before anything else is judged. `None` for every other verb.
    pub refused: Option<String>,
}

/// One buffered modify-by-tag intent (RUST-NATIVE HFT surface; resolved to an order client id at drain time).
pub struct BufferedModify {
    pub tag: String,
    pub new_qty: Option<f64>,
    pub new_price: Option<f64>,
    /// A trigger order's new trigger (`OrderIntent::Modify::new_trigger_price`); `None` for
    /// [`LiveBroker::modify`], set by [`LiveBroker::modify_trigger`].
    pub new_trigger_price: Option<f64>,
}

/// One buffered bracket intent (entry + SL + TP). venue/symbol come from the mount at drain time;
/// the three order client ids are minted then, so id minting stays in the runtime's one place.
pub struct BufferedBracket {
    /// See [`BufferedSubmit::symbol`]. `None` = the mount's own symbol.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    pub entry_price: Option<f64>,
    pub stop_loss: f64,
    pub take_profit: f64,
}

/// One buffered ORDER LIST ([`LiveBroker::submit_group`], [`LiveBroker::submit_oco`],
/// [`LiveBroker::submit_ladder`], `vike_model::OrderBroker::place_group`): its legs, linked by index
/// (`vike_model::OrderListLeg`). The drain resolves its symbol as a submission's, stamps the venue and
/// lowers it as ONE `OrderIntent::OrderList`, whose one order client id per leg the runtime mints, as for
/// [`BufferedBracket`].
pub struct BufferedOrderList {
    /// The legs, or the words the verb refused them in (`vike_model::build_ladder`'s), which the drain
    /// says to the mount as a `Denied`.
    pub legs: Result<Vec<vike_model::OrderListLeg>, String>,
    /// Each leg's tag, in leg order (`vike_model::OrderBroker::place_group`), which the drain files
    /// against the order client id it mints for that leg as a tagged submit's is filed; EMPTY for the untagged
    /// verbs above.
    pub tags: Vec<String>,
    /// The symbol `vike_model::OrderBroker::place_group` named (`None`: `""`, the mount's own, and
    /// every verb above), resolved by the drain's one rule (`resolve_intent_symbol`) as a
    /// submission's is: a symbol the mount does not trade is REFUSED, never lowered on another.
    pub symbol: Option<String>,
}

/// One buffered COMBO ([`LiveBroker::submit_combo`]): two legs or more traded as one order on the mount's
/// account. The drain stamps the mount's venue, routes it by the mount (its one account) and lowers it as ONE
/// `OrderIntent::Combo`, whose one order client id the runtime mints, as for [`BufferedBracket`].
pub struct BufferedCombo {
    /// +1 buys the combo, -1 sells it.
    pub side: i32,
    /// Combo units.
    pub qty: f64,
    /// Each leg's instrument and SIGNED ratio, and its own limit on a combo priced by its legs.
    pub legs: Vec<vike_model::OrderComboLeg>,
    /// The signed net per combo unit; `None` with no leg limit is a combo market order.
    pub net_limit: Option<f64>,
}

/// One buffered conditional-order intent (stop / trailing) for the core-owned
/// [`crate::emulator::ConditionalArms`] — port of `exec/conditionals.py` registration:
/// ARMING bypasses the gate (only the FIRE goes mint → RiskGate → client). Trailing
/// extremes seed from the current mark at drain time (refused without a mark, like
/// `live_portfolio_engine.py::submit_trailing`).
pub struct BufferedConditional {
    /// See [`BufferedSubmit::symbol`]. `None` = the mount's own symbol.
    pub symbol: Option<String>,
    pub side: i32,
    pub qty: f64,
    /// Some = fixed stop trigger price
    pub price: Option<f64>,
    /// Some = trailing distance
    pub trail: Option<f64>,
    /// Some = a trailing-LIMIT's offset ([`LiveBroker::submit_trailing_limit`]): the fire releases a
    /// limit this far past the stop level it fired at
    pub limit_offset: Option<f64>,
}

/// What a SANDBOXED strategy answered one hook with (decision 0273 item 3), as the one parcel
/// `vike_execution::Ingest::MountReply` may carry. Built by the mount's supervisor in `vike-tradehub-core`.
pub struct MountReplyIntents {
    /// Writes the intents into the mount's own [`LiveBroker`], which the core builds exactly as an
    /// in-process hook's and then DRAINS, so the reply meets every gate an in-process strategy's orders meet.
    pub write: Box<dyn FnOnce(&mut LiveBroker) + Send>,
    /// Called once the drain AND the deliveries it caused (a `Denied` order event, a fill) are done: what
    /// lets the proxy keep its strategy hearing, as in-process, every consequence of its orders before
    /// any hook it has not heard yet.
    pub applied: Box<dyn FnOnce() + Send>,
}

/// One EXTRA leg a mount may trade beyond its own `(venue, symbol)` — see
/// [`Mount::symbols`].
///
/// `venue: None` (the common case) means the mount's OWN venue: the leg is a second instrument
/// on the same exchange. `Some(v)` routes it to a DIFFERENT venue, which is what a cross-exchange
/// strategy needs — an xEMM maker rests on one venue and hedges on another, so its two legs
/// cannot share a venue by construction.
///
/// The shape mirrors `vike_strategy::ControllerHarness`'s `venue_map` (`symbol -> venue`, empty
/// = single-venue): the same problem was already solved once for the funding-carry controller
/// (the file `strategies/funding/funding_carry.rs`), and two different answers to "which venue does this symbol trade on" would be a bug waiting
/// to happen.
///
/// **A leg is found by its NAME, an order is sent by its SYMBOL** (decision 0293). The name is what the
/// strategy calls the leg on every lane it sees it (an order, a `position`/`price`/`bars` read, a fill):
/// its symbol, or `SYMBOL@venue/LABEL` for a leg on another venue's account ([`Self::named`]), which is
/// how two venues' `BTCUSDT.P` stay two legs. The venue, its account and the journal book the symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountLeg {
    /// The strategy's name of the leg: [`Self::symbol`], or `SYMBOL@venue/LABEL` (see the type's doc).
    pub name: String,
    /// The instrument's user name on its venue: what the order, the account and the journal carry.
    pub symbol: String,
    /// `None` = the mount's own venue.
    pub venue: Option<String>,
    /// The ACCOUNT of [`Self::venue`] the leg trades on, which its orders carry
    /// (`vike_model::OrderRequest::account`); `None` = the account the mount's own rule resolves for the
    /// venue (`mount_engine_resolution`): the mount's own on its venue, a bare venue's one engine on another.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
}

impl MountLeg {
    /// A leg on the mount's OWN venue.
    pub fn same_venue(symbol: impl Into<String>) -> Self {
        let symbol = symbol.into();
        MountLeg { name: symbol.clone(), symbol, venue: None, account: None }
    }

    /// A leg on a DIFFERENT venue — the cross-exchange case.
    pub fn at(symbol: impl Into<String>, venue: impl Into<String>) -> Self {
        let symbol = symbol.into();
        MountLeg { name: symbol.clone(), symbol, venue: Some(venue.into()), account: None }
    }

    /// The leg a strategy NAMES `name` (decision 0293): a plain symbol is [`Self::same_venue`]; a name
    /// `SYMBOL@BOOK` (`vike_model::accounts::account_keys::parse_leg_name`) is `SYMBOL` on the venue and
    /// account `BOOK` names, keeping `name`.
    ///
    /// A `BOOK` on `mount_venue` itself is a leg on ANOTHER account of the mount's own venue and must name that
    /// account (`SYMBOL@venue/LABEL`); whether it is another account than the mount's is judged at the mount's
    /// start (`mount_legs_resolution`), where the engines are known.
    ///
    /// # Errors
    /// A book token the rule refuses, in its words; and a bare `SYMBOL@venue` of `mount_venue`, which names no
    /// account: a leg of the mount's own account is named by its symbol alone.
    pub fn named(name: &str, mount_venue: &str) -> Result<Self, String> {
        let Some(on) = vike_model::accounts::account_keys::parse_leg_name(name)? else {
            return Ok(MountLeg::same_venue(name));
        };
        if on.venue == mount_venue && on.account.is_none() {
            return Err(format!(
                "the leg `{name}` names the mount's own venue `{mount_venue}` and no account: a leg on \
                 the mount's own account is named by its symbol alone (`{}`), one on another account \
                 of `{mount_venue}` by that account (`{}@{mount_venue}/<LABEL>`)",
                on.symbol, on.symbol
            ));
        }
        Ok(MountLeg {
            name: name.to_string(),
            symbol: on.symbol.to_string(),
            venue: Some(on.venue.to_string()),
            account: on.account,
        })
    }

    /// Whether the strategy names this leg by its account (`SYMBOL@venue/LABEL`), so its name is not its
    /// symbol.
    pub fn names_an_account(&self) -> bool {
        self.name != self.symbol
    }

    /// **THE one rule for which of a strategy's symbols a mount declares**: append to `legs` what the
    /// strategy trades beside the mount's own `symbol` (`vike_model::Strategy::symbols`), each a leg on
    /// the mount's own `venue`, or on the account a `SYMBOL@venue/LABEL` name binds ([`Self::named`]); a
    /// blank, the own symbol and a repeated name declare nothing. A profile mount
    /// (`vike_tradehub_core::MountPlacement::declare_strategy_legs`) and a runtime one
    /// (`Command::MountStrategy`, `CoreThread::mount_strategy_runtime`) both declare through it.
    ///
    /// # Errors
    /// A leg whose kept row's user name is written another way (`vike_catalog::respelled_symbol`, the C-D4
    /// follow-up, decision 0290: `btcusdt.p` is `BTCUSDT.P` on bybit), in its words, and a leg name
    /// [`Self::named`] refuses; no leg is declared then. The strategy's own orders name the leg as
    /// written, so it is refused rather than respelled.
    pub fn declare_strategy_legs(
        legs: &mut Vec<MountLeg>,
        venue: &str,
        symbol: &str,
        symbols: Vec<String>,
    ) -> Result<(), String> {
        let mut named = Vec::with_capacity(symbols.len());
        for leg in &symbols {
            let leg = MountLeg::named(leg, venue)
                .map_err(|why| format!("a symbol the strategy declares: {why}"))?;
            let leg_venue = leg.venue.as_deref().unwrap_or(venue);
            if let Some(why) = vike_catalog::respelled_symbol(leg_venue, &leg.symbol) {
                return Err(format!("a symbol the strategy declares: {why}"));
            }
            named.push(leg);
        }
        for leg in named {
            if leg.name.trim().is_empty()
                || leg.name == symbol
                || legs.iter().any(|l| l.name == leg.name)
            {
                continue;
            }
            legs.push(leg);
        }
        Ok(())
    }
}

/// A strategy mounted on one (venue, symbol, interval) bar series.
pub struct Mount {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` — every mount that existed
    /// before this field — is the venue's DEFAULT account, whose route key IS the bare venue id, so
    /// [`assemble_core`] resolves the identical engine index the venue lookup resolved and nothing
    /// about a single-account process changes.
    ///
    /// `Some(label)` is resolved ONCE, at assembly, into [`CoreThread::mount_engine`] — an INDEX —
    /// and every lane that used to ask "which engine does this venue belong to" asks that vector
    /// instead. Two properties follow, and both are the point:
    ///
    /// * the account never touches a venue STRING. Decorating `venue` (the `"binance#ALT"` trap
    ///   `vike_execution::route_key`'s module doc names) would route correctly and then silently break
    ///   `Engine::apply_snapshot`'s reap filter, `resolved_position_price`'s position
    ///   lookup and `local_view`'s filter, all of which compare against the engine's CANONICAL
    ///   venue — while fills arrive from a bridge that carries the canonical id and cannot know
    ///   accounts exist;
    /// * the mount's READS move with its writes. `OrderBroker::position`/`equity`/`multiplier`/
    ///   `lot_size` resolve through the same `mount_engine` index, so a mount cannot trade one book
    ///   while sizing against another — which is a worse defect than the one this field fixes and
    ///   one no order-routing test would catch.
    ///
    /// ⚠ It is deliberately NOT part of the derived mount ID (`crate::strategy_state::mount_id_with`):
    /// the id is a state-sidecar FILENAME and the journal attribution key, so folding the account in
    /// would rename every existing sidecar. Two mounts differing only by account therefore collide
    /// on the id and are refused by the existing duplicate-id rule, which correctly tells the
    /// operator to name a `controller_id`.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    pub strategy: Box<dyn Strategy<LiveBroker> + Send>,
    /// Opt-in ADDITIONAL symbols (same `venue`) this mount may trade and read — the declaration
    /// that turns the otherwise-IGNORED `symbol` argument of the [`vike_model::OrderBroker`] order verbs
    /// into an AUTHORITATIVE one for this mount.
    ///
    /// EMPTY — every mount whose strategy declares no `vike_model::Strategy::symbols` (the daemon
    /// declares those as this mount's legs, same venue; a host building a mount by hand fills it
    /// itself) — keeps the single-symbol contract exactly: `drain_broker` stamps the mount's own
    /// `(venue, symbol)` onto every intent and the verbs' `symbol` argument is ignored,
    /// byte-identically to before this field existed. Declared, `""` is the mount's OWN symbol on
    /// every lane, whichever series dispatched (decision 0277's rule 1).
    ///
    /// **Why a declaration is required rather than simply honouring the argument.** Shipped
    /// strategies pass a symbol they never expect to be used: live bars carry `Bar::symbol == None`
    /// (`vike_bridge_core::klines::kline_to_bar`), so a strategy doing
    /// `bar.symbol.clone().unwrap_or_default()` passes `""` — `crates/vike-simulator/tests/r7_gate.rs`,
    /// `crates/vike-core/src/runtime/tests/safe_state/mount_budget.rs`'s `BudgetSubmit` and
    /// `vike-rhai`'s live bar lane all say so explicitly.
    /// Honouring the argument unconditionally would send `symbol: ""` to venues. The MOUNT, not the
    /// call site, is therefore where the intent has to be expressed.
    ///
    /// It exists because a TWO-LEG strategy (a pairs trade, an xEMM hedge) cannot be split across
    /// two mounts: a mount OWNS its strategy (`Box<dyn Strategy>`, not `Arc`), so two mounts hold
    /// two different objects with two different states and there is no shared-state seam. ONE mount
    /// emitting orders for more than one symbol is the only available shape.
    ///
    /// WHAT READS IT TODAY (it is no longer inert — an earlier revision of this doc said it was,
    /// and that has been stale since #916/#924/#997):
    /// - `drain_broker` → `resolve_intent_symbol` (a declared symbol on an order verb is honoured;
    ///   an UNDECLARED one is refused, never rewritten) and `resolve_intent_venue` (a
    ///   [`MountLeg::at`] leg's orders route to THAT venue's engine);
    /// - `declared_views` (the per-symbol `OrderBroker::position`/`price` tables — which carry every
    ///   declared instrument plus the mount's OWN symbol, minus whichever series is DISPATCHING,
    ///   because the ctx scalars are that series' own and fresher answer; built on EVERY
    ///   strategy-hook lane, and each row read out of that leg's OWN venue's engine — plus the
    ///   `OrderBroker::bars` table, which carries the dispatching symbol TOO and is therefore what
    ///   `LiveBroker::carries` reads to tell an uncarried symbol from a dispatching one);
    /// - the BAR and TICK audiences (`Audience::Bar`, `Audience::Tick`, read by
    ///   [`CoreThread::drive_strategy`] and [`CoreThread::drive_strategy_tick`]) — a declared
    ///   symbol's bars/ticks on the mount's own venue reach the mount, and live bars now carry their
    ///   series symbol so the legs are distinguishable;
    /// - [`CoreThread::drive_strategy_reference_quote`] — a leg declared on a DIFFERENT venue has
    ///   that venue's L1 touch delivered to [`Strategy::on_reference_quote`] (the xEMM lane).
    ///
    /// ⚠ STILL NOT ROUTED for a leg on a DIFFERENT venue: its BARS (the bar lane additionally
    /// requires that venue's engine to `accepts_symbol` the leg), its FEED STATUS (`FeedStatus`
    /// carries neither venue nor symbol) and its L2 BOOK (`L2Book` carries neither, so it cannot be
    /// attributed — its derived L1 is delivered instead).
    ///
    /// ⚠ ALL FOUR read gaps `docs/superpowers/specs/2026-08-07-multi-symbol-read-half.md` listed are
    /// CLOSED. The empty per-symbol tables on the non-dispatch lanes, the mount-venue resolution of a
    /// cross-venue leg and the drain-keyed tag registry went first
    /// (`crates/vike-core/tests/wiring/multi_symbol_reads.rs` is the regression proof for those
    /// three); `LiveBroker::bars` — which
    /// ignored its `symbol` argument on EVERY lane and returned the dispatching series — went with
    /// `LiveBroker::bar_views`, pinned by `crates/vike-simulator/tests/parity/multi_symbol_read_parity.rs`'s
    /// `live_bars_are_symbol_addressed`. A symbol a DECLARED mount does not carry now reads EMPTY
    /// (`0.0` / `0.0` / `&[]`) instead of the dispatching series' numbers, which is the read-side
    /// mirror of `resolve_intent_symbol` REFUSING that same symbol on the write side.
    pub symbols: Vec<MountLeg>,
    /// Optional CROSS-SYMBOL underlying/reference series this mount WATCHES ("Option B" routing): a
    /// DIFFERENT `symbol` (same `venue`) whose marks the strategy anchors on via [`Strategy::on_mark`]
    /// — e.g. the `btcusdt` RTDS spot a Polymarket BTC up/down maker blends its fair mid toward. When
    /// `Some`, the runtime routes that symbol's drained marks to this mount's `on_mark`, building the
    /// broker ctx on the mount's OWN (venue, symbol). `None` (the default for every existing mount) ⇒
    /// no mark is ever routed here and [`CoreThread::drive_strategy_mark`] early-returns, so a run
    /// with no underlying-anchored mount is byte-identical.
    pub underlying_symbol: Option<String>,
    /// Opt-in explicit CONTROLLER ID naming THIS mount (multi-mount correctness, gap C). When
    /// `Some`, it IS the mount id ([`crate::strategy_state::mount_id_with`], sanitized to a safe
    /// filename segment); `None` — every mount that exists today — keeps the legacy
    /// `{venue}__{symbol}__{interval}` derivation, so its state sidecar, its journal `mount_id`
    /// provenance and its budget/schedule keys are all byte-identical to before this field existed.
    ///
    /// It exists because the derived identity is NOT unique: two strategies mounted on the SAME
    /// `(venue, symbol, interval)` collide onto one state sidecar file and one journal `mount_id`.
    /// [`assemble_core`] asserts mount-id uniqueness and PANICS at mount time on a duplicate rather
    /// than silently sharing state, so a same-triple pair must name its mounts here.
    ///
    /// **The identity lives on the MOUNT, not beside it.** An earlier shape kept these ids in a
    /// POSITIONAL `CoreConfig` vec indexed by assembly order; reordering [`CoreConfig::extra_mounts`]
    /// then silently reassigned every id, so a strategy would cross-load a SIBLING's durable state on
    /// the next start (a mount id is both a sidecar FILENAME and the journal attribution key). A
    /// field on the mount cannot drift that way — see
    /// `multi_mount_tests::reordering_extra_mounts_preserves_each_mount_id`.
    pub controller_id: Option<String>,
    /// **The mount's STRATEGY CODE** (decision 0270): the four `strat` characters its order client ids carry,
    /// base36 of the vike.db `strategy_code` row the tradehub assigned to `(mount id, strategy name)`, never
    /// reused. A LABEL, never the ownership mechanism (decision 0113 stands). `None` — a mount nobody gave a
    /// code (a test, a replay core's inert mount) — mints `0000`, no strategy. Only the tradehub reads or
    /// writes the row; this library takes the code as a parameter.
    pub order_client_id_code: Option<vike_model::orders::client_id::OrderClientIdStrategyCode>,
}

/// Per-mount readiness (portfolio-observer PR-4 T5) — one entry in `CoreThread::mount_states`,
/// parallel to `CoreThread::mounts` (same index). A mount starts `Pending` only when
/// [`CoreConfig::readiness_gate`] is on ([`assemble_core`] seeds every slot `Ready` otherwise, so
/// the gate-off path never allocates a `Pending` state at all). `Pending` still receives every
/// strategy-hook call ([`CoreThread::drive_strategy`] / `drive_strategy_tick` / ... all still
/// call the hook) — only the ORDER OUTPUT is gated, at [`CoreThread::drain_broker`]. The boundary
/// probe ([`CoreThread::maintain_mount_readiness`]) is the ONLY place a mount ever flips
/// `Pending -> Ready`; nothing ever flips it back (a mount that has traded once stays eligible).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MountState {
    /// Not yet priced: the mount's (venue, symbol) has never resolved a bid OR ask/mark/trade/bar
    /// through the `PriceBoard`. Buffered order intents from this mount's hook calls are dropped
    /// at `drain_broker` — the strategy trades on paper-thin air otherwise (no venue-informed
    /// price to size/cross against), so nothing is submitted until there is one.
    Pending,
    /// Priced at least once (or the gate is off, in which case every mount starts here and stays
    /// here): `drain_broker` drains this mount's buffered intents to the engine normally.
    Ready,
}
