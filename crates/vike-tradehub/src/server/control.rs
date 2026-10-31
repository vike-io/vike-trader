//! `server::control` — the CONTROL path's gating and acceptance: the server-edge limits
//! ([`ControlLimitsConfig`], [`ControlLimits`]), the ONE acceptance path every control surface
//! funnels through ([`accept_command`], with its [`AcceptError`] and [`Accepted`] verdicts) and
//! the lowering of a wire command into the core's real `Command` (`lower_command`).
//!
//! Split out of `server.rs` as a pure move; the module doc there carries the acceptance contract
//! (the gate ORDER, why a `Gone` closes the connection, why the rationale never reaches the core).
//! The routing gates `accept_command` consults are `super::refusal`'s.

use std::time::{Duration, Instant};

use vike_core::{CommandRejected, CommandSink};
use vike_exec::{Command, MountSpec, OrderIntent, ParamsUpdate, SymbolAdmission};
use vike_model::{
    BracketSpec, ContractValue, NotionalKind, OrderRequest, OrderType, StrategyParams, TimeInForce,
    TradingState, lifetime_refusal,
};
use vike_tradehub_client::wire::{WireCommand, WireTradingState};

use super::refusal::{
    account_refusal, awaiting_balance_refusal, bracket_refusal, check_bracket, coid_reuse_refusal,
    lane_twin_closing_refusal, not_running_refusal, venue_refusal,
};
use super::session_note;
use super::settings::SettingsShowSource;
use super::symbol_admission::{
    SymbolFacts, SymbolPlan, admission_refusal_while_not_active, await_price,
    foreign_contract_value, lane_addressed, live_twin_extra_symbol, live_twin_unsized_open,
    plan_submit_symbol, priced_for_sizing, unconfirmed_leverage_refusal, with_confirmed_leverage,
};
use super::unadmit::{UnadmitPlan, plan_unadmit};
use super::unlatch::{UnlatchPlan, plan_unlatch};
use crate::audit;

/// The default command rate cap (commands/sec) when the rate knob is unset or unparseable.
pub(super) const DEFAULT_CONTROL_RATE: f64 = 20.0;

/// The server-edge control-limit KNOBS — the pure CONFIG half of [`ControlLimits`], owned by the
/// CALLER (audit F13): the daemon binary resolves both ONCE at startup (see `main.rs`'s
/// `resolve_control_limits`) and hands the result to [`super::serve`]. Fixed for the server's lifetime;
/// every accepted connection builds its own [`ControlLimits`] token bucket from it.
///
/// The two knobs come from DIFFERENT places, and deliberately so:
///
/// - `max_notional` is a **policy ceiling** — `policy.max_notional_per_order`, via
///   `vike_config::Policy`. It has no environment layer at all.
///   ⚠ It was `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` until Phase 5 of the settings-unification design;
///   a ceiling that a shell export or a stale systemd unit can raise is not a ceiling, so the
///   variable was removed and a daemon that still finds it set refuses to start.
/// - `rate_per_sec` is the `config.tradehub_control_rate` row: it is a throughput knob, not a risk
///   ceiling — raising it cannot place a larger order — so it keeps the normal env layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlLimitsConfig {
    /// Per-order notional ceiling on Submit/Modify/Bracket (a bracket's every PRICED leg), the
    /// notional being `vike_model::ContractValue::order_notional`'s — `|qty| · |price| · |unit|` for a
    /// linear contract, `|qty| · |unit|` for an inverse one — see [`ControlLimits::vet`] for where the
    /// value comes from. `None` = no size cap (the core `RiskGate` is still the floor).
    pub max_notional: Option<f64>,
    /// Command rate cap, commands/sec (token bucket; the bucket caps at ~1s of rate).
    pub rate_per_sec: f64,
}

impl Default for ControlLimitsConfig {
    /// No policy ceiling + [`DEFAULT_CONTROL_RATE`] commands/sec — exactly what
    /// [`Self::from_policy`] resolves with no `policy.max_notional_per_order` row and the rate
    /// variable unset.
    fn default() -> Self {
        ControlLimitsConfig { max_notional: None, rate_per_sec: DEFAULT_CONTROL_RATE }
    }
}

impl ControlLimitsConfig {
    /// Combine the resolved POLICY ceiling with the RAW rate value (`None` = no ceiling / variable
    /// unset) — both already obtained by the caller, so the binary owns the I/O and this stays a
    /// pure, unit-testable resolver.
    ///
    /// `max_notional_per_order` arrives already typed and already validated (`vike_config` rejects
    /// a non-positive or non-finite ceiling when loading the `policy` rows, naming the section and key);
    /// the guard here is the belt to that braces, for a caller that built a `Policy` some other
    /// way — `0.0` would refuse every order, a silent halt. The rate keeps the old string
    /// semantics exactly: trimmed `f64` parse, garbage/non-positive treated as unset, defaulting
    /// to [`DEFAULT_CONTROL_RATE`].
    ///
    /// ⚠ Was `from_values(Option<&str>, Option<&str>)`, whose first argument was
    /// `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` — see [`ControlLimitsConfig`]'s doc for why that
    /// variable no longer exists.
    pub fn from_policy(max_notional_per_order: Option<f64>, rate_per_sec: Option<&str>) -> Self {
        let max_notional = max_notional_per_order.filter(|n| n.is_finite() && *n > 0.0);
        let rate_per_sec = rate_per_sec
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|r| *r > 0.0)
            .unwrap_or(DEFAULT_CONTROL_RATE);
        ControlLimitsConfig { max_notional, rate_per_sec }
    }
}

/// Server-side control-command limits — DEFENSE-IN-DEPTH beyond the scope key + the core `RiskGate`:
/// a per-order NOTIONAL ceiling and a command RATE limiter, enforced at the ONE edge every remote
/// command funnels through ([`accept_command`], which the TCP
/// [`vike_tradehub_client::proto::Request::Command`] arm AND the
/// `telegram` channel both call). So even a leaked `VIKE_TRADEHUB_CONTROL_KEY` — or any
/// non-GUI client — cannot place an arbitrarily large order or flood the core. Per-SURFACE (control
/// connections are few; each connection, and the Telegram poller, gets its own bucket), built from
/// the ONE [`ControlLimitsConfig`] the binary resolved at startup and [`super::serve`] carries.
pub struct ControlLimits {
    /// Reject a Submit/Modify/Bracket whose notional (`ContractValue::order_notional` of the
    /// addressed engine's published value, the SAME function the core `RiskGate` calls) exceeds this —
    /// for a bracket, at any of its priced legs. `None` = no size cap.
    max_notional: Option<f64>,
    rate_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
    /// The price of each recent ALLOWED priced `Modify` this surface passed on, newest last, for
    /// [`RECENT_MODIFY_PX_TTL`]: the published snapshot is coalesced, so for a moment it still shows
    /// the price from BEFORE that modify. Bounded by [`RECENT_MODIFY_PX_CAP`].
    recent_modify_px: Vec<(String, f64, Instant)>,
    /// The wall clock the closed-market note reads (`crates/vike-tradehub/src/server/session_note.rs`): `vike_model::now_ms`, and a
    /// field only so a test can stand the door on a closed market (`Self::set_clock`).
    now_ms: fn() -> i64,
}

/// How long a passed modify's price outlives it here: far over the publisher's 15 ms poll plus the
/// core's coalescing, so the snapshot has caught up by the time it expires.
const RECENT_MODIFY_PX_TTL: Duration = Duration::from_secs(5);
/// Entries kept; the oldest is dropped first (the rate bucket already bounds how fast they arrive).
const RECENT_MODIFY_PX_CAP: usize = 64;

impl ControlLimits {
    /// A fresh per-surface limiter over `cfg` — the token bucket starts full (`rate_per_sec`).
    pub fn new(cfg: ControlLimitsConfig) -> Self {
        ControlLimits {
            max_notional: cfg.max_notional,
            rate_per_sec: cfg.rate_per_sec,
            tokens: cfg.rate_per_sec,
            last_refill: Instant::now(),
            recent_modify_px: Vec::new(),
            now_ms: vike_model::now_ms,
        }
    }

    /// Stand this surface's clock on `now_ms` (a test of the closed-market note).
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, now_ms: fn() -> i64) {
        self.now_ms = now_ms;
    }

    /// Remember the price an allowed `Modify` carries (see [`Self::recent_modify_px`]).
    fn note_modify_price(&mut self, cmd: &WireCommand) {
        let WireCommand::Modify { client_order_id, new_price: Some(p), .. } = cmd else { return };
        if !p.is_finite() {
            return;
        }
        let now = Instant::now();
        self.recent_modify_px.retain(|(_, _, at)| now.duration_since(*at) < RECENT_MODIFY_PX_TTL);
        if self.recent_modify_px.len() >= RECENT_MODIFY_PX_CAP {
            self.recent_modify_px.remove(0);
        }
        self.recent_modify_px.push((client_order_id.clone(), *p, now));
    }

    /// The highest price a recent allowed modify of `coid` carried, if any is still within the TTL.
    fn recent_modify_price(&self, coid: &str) -> Option<f64> {
        let now = Instant::now();
        self.recent_modify_px
            .iter()
            .filter(|(c, _, at)| c == coid && now.duration_since(*at) < RECENT_MODIFY_PX_TTL)
            .map(|(_, p, _)| *p)
            .max_by(f64::total_cmp)
    }

    /// Vet one command before it reaches the core. `Some(reason)` ⇒ REFUSE (surfaced as a
    /// `Response::Error`); `None` ⇒ allow. The rate token is consumed on EVERY command; the notional
    /// cap applies ONLY to order-INCREASING verbs that carry a price (Submit / Modify / Bracket) —
    /// Cancel/Flatten/MassCancel/MarketExit/SetTradingState are risk-reducing and never size-capped.
    ///
    /// `blocks` is the node's PUBLISHED engine blocks ([`crate::publish::PublisherHandle::engine_blocks`])
    /// — the one thing the notional cap reads from them is the instrument's contract value (kind and
    /// per-unit factor), and the private `notional_reason`'s doc carries the whole argument, including
    /// the empty case: `&[]` is "this surface knows no engine roster" and sizes at the default value
    /// (`Linear`, 1.0).
    ///
    /// `orders` is the OPEN order a `Modify` names, off the same published snapshot
    /// ([`crate::publish::PublisherHandle::orders_named_by`]) — the only way this edge learns
    /// which instrument a bare client order id rests on. Every other verb ignores it, and `&[]` is
    /// "the node does not know that order", which sizes a `Modify` at the default value as it always
    /// did; the same `notional_reason` doc carries the rest.
    pub fn vet(
        &mut self,
        cmd: &WireCommand,
        blocks: &[vike_exec::VenueBlock],
        orders: &[vike_exec::OrderView],
    ) -> Option<String> {
        if let Some(refusal) = self.vet_rate() {
            return Some(refusal);
        }
        self.vet_notional(cmd, blocks, orders, None)
    }

    /// [`Self::vet`] WITHOUT its rate token (the caller took one with [`Self::vet_rate`]), and with
    /// `foreign`: the contract value of the symbol a `Submit` names when the addressed engine does not
    /// trade it yet, from the symbol's own facts ([`super::symbol_admission::foreign_contract_value`]).
    /// An engine's published grid lists no row for a symbol it was never told about, so the order would
    /// be sized as `Linear` and an INVERSE foreign symbol (an amount of US dollars) would read as
    /// `qty · price` times too large, refused by a ceiling it is under, and never admitted.
    pub fn vet_notional(
        &mut self,
        cmd: &WireCommand,
        blocks: &[vike_exec::VenueBlock],
        orders: &[vike_exec::OrderView],
        foreign: Option<ContractValue>,
    ) -> Option<String> {
        let verdict = self.notional_reason(cmd, blocks, orders, foreign);
        if verdict.is_none() {
            self.note_modify_price(cmd);
        }
        verdict
    }

    /// Whether a notional ceiling is set: only then is a foreign symbol's value worth a lookup here.
    pub(crate) fn has_ceiling(&self) -> bool {
        self.max_notional.is_some()
    }

    /// **The RATE token alone** — `Some(reason)` ⇒ REFUSE, `None` ⇒ allow.
    ///
    /// For a verb that is not a [`WireCommand`] at all and therefore has no notional to size: the
    /// account-admin plane (`docs/decisions/0065`). A flood is still a flood — this surface opens
    /// a SQLite transaction per frame — and the cap being n/a is the `SetSetting` vetting decision
    /// verbatim: the one policy key it enforces is defined over ONE ORDER's notional.
    ///
    /// ⚠ It shares the CONNECTION's bucket rather than owning a second one, so an Admin peer
    /// cannot spend an account-verb budget and an order budget at once.
    pub fn vet_rate(&mut self) -> Option<String> {
        (!self.take_token()).then(|| {
            format!("rate limited: control commands capped at {}/s on this node", self.rate_per_sec)
        })
    }

    /// The NOTIONAL-cap check in isolation — no rate token consumed, `&self`. Shared by the executing
    /// [`Self::vet`] (which consumes a token first) and the read-only [`Self::preview_vet`].
    /// `Some(reason)` ⇒ would refuse; `None` ⇒ passes (no cap set, nothing to size, or within cap).
    ///
    /// ⚠ **THE CONTRACT VALUE IS PART OF THE NOTIONAL, AND IT IS THE CORE'S OWN NUMBER** (decision
    /// 0170). The notional is `ContractValue::order_notional` of the value the addressed engine
    /// published: `|qty| · |price| · |unit|` for a linear contract and `|qty| · |unit|` for an
    /// INVERSE one (deribit's `BTC-PERPETUAL`, whose quantity is already US dollars, so an order of
    /// 10 is worth 10 and the price never enters). This is the SAME function the core's `RiskGate`
    /// and the desktop's order preview call, on the same value, so the door and the gate cannot
    /// disagree about one order. They did: this edge floored the scalar multiplier at 1.0 while the
    /// gate did not, which sized a linear USDC perpetual (lot 0.0001, quantity in BTC) at the gate
    /// 10,000x below the door. That floor is deleted; there is one number. This is the ONLY site
    /// that enforces `policy.max_notional_per_order`: it reaches no `RiskLimits`
    /// (`crates/vike-tradehub-core/src/policy.rs`'s module doc carries why).
    ///
    /// The value is read where the core's own gate reads it — the engine's account grid, as
    /// `vike_exec::VenueBlock::contract_value_of` over the published `blocks` that [`accept_command`]
    /// already carries for the bracket gate. No wire field, no new lookup across the core's thread
    /// boundary, and the grid is immutable after construction, so a published copy cannot be stale
    /// about it. The block is the one whose `route_key` the command addresses (the SAME composer
    /// [`account_refusal`] uses, so `DEFAULT` and an absent account both name the bare-venue book).
    ///
    /// **What it cannot resolve it reads as the default value** (`Linear`, 1.0), the answer
    /// `CoreSnapshot::contract_value_of` gives the desktop and the mount gives an instrument that
    /// reports none: an empty `blocks` (the core has not published yet — [`venue_refusal`]'s UNKNOWN
    /// rule), a venue or account with no block, a label that does not parse (`lower_command` refuses
    /// that one). ⚠ **Not a symbol the engine does not trade that the symbol door REFUSES** (a pair quoted
    /// in another money than the account's with no rate, no converting account, or an unanchored ledger):
    /// the default value would count the pair's quote money (yen for USDJPY) against a ceiling stated in
    /// the account's, so the callers never size it (`foreign_contract_value`'s `Err`): the door's own
    /// refusal is what the operator reads, and what refuses the order.
    ///
    /// **A `Modify` names a client order id and nothing else, so its instrument is read off the
    /// order that id names.** `orders` is the node's published OPEN order for that id
    /// ([`crate::publish::PublisherHandle::orders_named_by`] — the snapshot already carries
    /// every engine's registry with each order's venue, account and symbol), and the value is the
    /// one of the engine that holds it, for that order's symbol: the same two steps the core takes
    /// when it judges the projected order — `ExecutionEngine::modify_order` reads the resting
    /// request out of the engine's registry, and the risk context it builds reads that request's
    /// symbol in the engine's account grid. The id is matched on OPEN orders only (a terminal order
    /// is one the core ignores a modify of).
    ///
    /// **The order this snapshot does not know is sized at the default value, exactly as a `Modify`
    /// was before this edge could resolve one — declared, not a claim that the value does not
    /// apply.** The snapshot is built by a coalesced publish, so a coid placed moments ago may not
    /// be in it yet; an id that never existed, one already terminal, a bracket exit still HELD off
    /// the venue (those sit in `held_exits`, not in `orders`) and a surface that passes `&[]` all
    /// land there. The core's own `RiskGate` judges the projected order with the real value whenever
    /// the modify reaches an engine, and it is the enforcing gate regardless; this edge is
    /// defense-in-depth.
    ///
    /// **An INVERSE contract needs no price to be sized**, so a priceless (market) `Submit` on one
    /// is sized here too, by its quantity alone; a priceless order on a linear contract still has
    /// nothing to size at this edge and is the core's to judge. A stop or take-profit `Submit` is not
    /// priceless: it is sized at its trigger price when it names no price of its own.
    ///
    /// ⚠ **A non-finite size or price is REFUSED, not compared.** `NaN > max` is false, so a NaN
    /// notional used to read as "within the ceiling" — the same "cannot evaluate" reading the
    /// priceless-modify arm below exists to delete. A zero or negative qty is NOT this site's: a
    /// negative one is sized by its magnitude (pinned) and both are refused by the core `RiskGate`'s
    /// `non-positive-size` lane before any venue client sees them. The TCP wire cannot carry a
    /// non-finite number into this function (JSON has none) and the Telegram parser refuses one, so
    /// this is the guard for a caller that builds a [`WireCommand`] in process.
    fn notional_reason(
        &self,
        cmd: &WireCommand,
        blocks: &[vike_exec::VenueBlock],
        orders: &[vike_exec::OrderView],
        foreign: Option<ContractValue>,
    ) -> Option<String> {
        let max = self.max_notional?;
        // NOTIONAL IS A MAGNITUDE — `vike_model::order_notional` is the workspace definition and it
        // takes `.abs()` of every factor. This site used to abs the QTY only, so a NEGATIVE price
        // yielded a negative notional, `n > max` could never trip, and the ceiling was bypassed by a
        // sign alone. (The core `RiskGate` never had this bug: it routes through `order_notional`.)
        //
        // The arms below produce the three FACTORS, `(qty, price, value)`, and the one site
        // after the match prices them — so the finite check and the comparison cannot be
        // skipped by an arm that sizes itself.
        let sized: Option<(f64, f64, ContractValue)> = match cmd {
            WireCommand::Submit(r) => {
                // A symbol the engine does not trade yet has no row in its published grid; its own
                // facts' value (`foreign`) stands in, so an inverse foreign symbol is sized like one.
                let value = foreign.unwrap_or_else(|| {
                    book_value(&r.venue, r.account.as_deref(), &r.symbol, blocks)
                });
                // A stop or take-profit names its TRIGGER and no price: it is sized at the trigger,
                // the reference price the core's `RiskGate` gives it (`price.or(trigger_price)`), so
                // the door and the gate judge one number. A limit order (and a stop-limit) is sized
                // at its price as before; only an order with NEITHER (a market order) is unsized.
                match (r.price.or(r.trigger_price), value.kind) {
                    (Some(p), _) => Some((r.qty, p, value)),
                    // an inverse notional does not contain the price: size it by the quantity
                    (None, NotionalKind::Inverse) => Some((r.qty, 0.0, value)),
                    (None, NotionalKind::Linear) => None,
                }
            }
            // The value is the one of the order this coid names, read off the published OPEN
            // order (the doc above); an order the snapshot does not hold is the default, the
            // arithmetic this arm used before it could resolve one.
            WireCommand::Modify { client_order_id, new_qty: Some(q), new_price: Some(p) } => {
                Some((*q, *p, order_value(client_order_id, orders, blocks)))
            }
            // ⚠ A qty-RAISING modify that names NO price is sized at the STANDING order's price
            // (the venue keeps the resting price), read off the published open order. This shape
            // used to fall into `_ => None` — "no price to size, therefore fine" — which let
            // `/modify <coid> qty=<huge>` walk past the node's one order-size ceiling completely:
            // place a small in-cap order, then modify it up. When the snapshot has no price to give
            // (a market/stop order, a coid it does not know yet) it is UNCHECKABLE and REFUSED
            // rather than waved through: "I cannot evaluate this ceiling" must never render as
            // "the ceiling passed". The operator's fix then is to name the price. The core
            // `RiskGate` DOES resolve the resting terms and is the enforcing gate regardless —
            // this is the defense-in-depth edge, failing CLOSED.
            WireCommand::Modify { client_order_id, new_qty: Some(q), new_price: None } => {
                //
                // ⚠ The snapshot price can be STALE (a price modify moments ago, the publish
                // coalesced, the venue not yet answered), so the size is checked at the HIGHEST of
                // it, the price of a modify the order view says is still IN FLIGHT whichever surface
                // sent it (`OrderView::pending_price`), and any price an allowed modify of this coid
                // carried through THIS surface in the last [`RECENT_MODIFY_PX_TTL`]. A standing price
                // that is zero, negative or non-finite is "no price": notional 0 would let any size
                // pass, and so is a pending one.
                let open = orders
                    .iter()
                    .find(|o| o.client_order_id == *client_order_id && !o.status.is_terminal());
                if let Some(p) = open.and_then(|o| o.price).filter(|p| p.is_finite() && *p > 0.0) {
                    let pending =
                        open.and_then(|o| o.pending_price).filter(|p| p.is_finite() && *p > 0.0);
                    let p = pending.map_or(p, |r| p.max(r));
                    let p = self.recent_modify_price(client_order_id).map_or(p, |r| p.max(r));
                    Some((*q, p, order_value(client_order_id, orders, blocks)))
                } else {
                    return Some(format!(
                        "modify to qty {q} names no price and the order has no standing price, so \
                         its notional cannot be checked against the node's policy ceiling \
                         max_notional_per_order {max:.2} — re-send the modify with an explicit price"
                    ));
                }
            }
            // VETTING DECISION (split-plane B4): the notional cap does NOT apply to `UpdateParams`.
            // A params update is not an order — it carries no qty×price to size, and the policy key
            // this ceiling enforces (`max_notional_per_order`) is defined over ONE ORDER's
            // notional. This is not the priceless-modify hole above wearing a new verb: that shape
            // names a CONCRETE projected order this arm merely cannot price, whereas a re-tune
            // names none — every order a re-tuned strategy later emits still passes the core
            // `RiskGate` and the mount's mandatory live risk budget (`ProfileRisk`), which are the
            // enforcing gates for strategy-originated size. The rate token in `vet` DOES apply
            // (consumed for every command — a flood of re-tunes is still a flood).
            WireCommand::UpdateParams { .. } => None,
            // VETTING DECISION (split-plane B5): the mount verbs are not orders either — a mount
            // request carries no qty×price to size, and every order the mounted strategy later
            // emits passes the core `RiskGate` + the mount's live risk budget, the enforcing gates
            // for strategy-originated size (the `UpdateParams` argument verbatim). The unmount is
            // risk-REDUCING (it cancels the mount's resting orders). The rate token in `vet` still
            // applies to both.
            WireCommand::MountStrategy { .. } | WireCommand::UnmountStrategy { .. } => None,
            // VETTING DECISION (operator reconcile verb): a confirm carries no qty x price - it
            // folds what the held divergence proposed, and the book it reaches is the one the
            // divergence names. The rate token in `vet` still applies.
            WireCommand::ReconConfirm { .. } => None,
            // VETTING DECISION (operator cancel of a held row): it carries no qty x price and sends a
            // single cancel; the rate token in `vet` still applies.
            WireCommand::ReconCancel { .. } => None,
            // VETTING DECISION (operator per-venue resume): it carries no qty x price, and it only
            // releases a halt the link dead-man itself set; the rate token in `vet` still applies.
            WireCommand::ResumeVenue { .. } => None,
            // VETTING DECISION (account-scoped trading state): carries no qty x price, and it is the
            // node-wide `SetTradingState`'s narrower twin (risk-REDUCING or a resume, never sized).
            WireCommand::SetAccountTradingState { .. } => None,
            // VETTING DECISION (the give-back of an admitted symbol, decision 0205): carries no qty x price and
            // opens nothing (it frees a slot and writes nothing at the venue); the rate token in `vet` applies.
            WireCommand::UnadmitSymbol { .. } => None,
            // VETTING DECISION (the un-latch of a mount's budget, decision 0239): carries no qty x price and
            // places nothing (it lets a mount that is flat and quiet trade again, and every order the mount
            // then emits passes the core `RiskGate` and its re-armed budget); the rate token in `vet` applies.
            WireCommand::UnlatchMount { .. } => None,
            // VETTING DECISION (split-plane REQ-7): the notional cap does NOT apply to
            // `SetSetting` — a settings write is not an order and carries no qty×price to size,
            // and the one policy key this ceiling enforces (`max_notional_per_order`) is defined
            // over ONE ORDER's notional (the `UpdateParams` argument verbatim). Note the
            // direction that DOES matter is already closed elsewhere: a settings write that
            // RAISES the ceiling itself lands restart-to-apply — this running node's
            // `ControlLimits` keeps its boot-time cap regardless — and passes the loader's bounds
            // first. (It used to face a typed confirm at the acceptance arm as well; that ceremony
            // is deleted for every key, `docs/decisions/0086` point 7.) The rate token in `vet`
            // still applies (a flood of writes is still a flood).
            WireCommand::SetSetting { .. } => None,
            // THE BRACKET: three orders of ONE size, and every priced leg is capped as a `Submit`
            // at that price would be — the entry when it is a limit, the take-profit, and the
            // stop-loss at its trigger (a stop `Submit` is sized at its trigger too). A MARKET entry is unpriced like a market `Submit`, but its exits
            // are not. The refusal NAMES the leg, which the shared sentence below cannot: the
            // breaching legs are filtered BEFORE the largest is picked, so a NaN leg (which
            // `n > max` never selects) cannot hide a finite leg that does breach. ⚠ That also
            // means a NON-FINITE bracket is not refused HERE — the finite check after this match
            // is the Submit/Modify arms', and a bracket's own is `check_bracket` (which names the
            // field), run right after this gate by `accept_command` and both previews.
            //
            // The value is the bracket's engine's: [`bracket_refusal`] admits a bracket only
            // to the ONE engine it names (the bare venue id when it names no account), which is
            // the block read here, by the same account text a `Submit` is sized by.
            WireCommand::Bracket(b) => {
                let value = book_value(&b.venue, b.account.as_deref(), &b.symbol, blocks);
                let breach = [
                    ("entry", b.entry_price),
                    ("stop-loss", Some(b.stop_loss)),
                    ("take-profit", Some(b.take_profit)),
                ]
                .into_iter()
                .filter_map(|(leg, px)| px.map(|p| (leg, value.order_notional(b.qty, p))))
                .filter(|(_, n)| *n > max)
                .max_by(|x, y| x.1.total_cmp(&y.1));
                if let Some((leg, n)) = breach {
                    let note = value_note(value);
                    return Some(format!(
                        "order notional {n:.2} (the bracket's {leg} leg) exceeds the node's \
                         policy ceiling max_notional_per_order {max:.2}{note}"
                    ));
                }
                None
            }
            // NO WILDCARD. A `_ => None` here is how a new order verb would reach this node
            // UNCAPPED without anyone deciding so — it is exactly how a bracket sat unsized while
            // its arm refused it — so a new variant must be classified on this list. These carry
            // nothing to size, and the core `RiskGate` stays the floor.
            WireCommand::Cancel(_)
            | WireCommand::Modify { new_qty: None, .. }
            | WireCommand::MassCancel { .. }
            | WireCommand::Flatten { .. }
            | WireCommand::MarketExit { .. }
            | WireCommand::SetTradingState(_) => None,
        };
        if let Some((qty, price, value)) = sized {
            // ⚠ BEFORE the comparison, because a comparison cannot answer this: `NaN > max` is
            // false. An INFINITE factor would mostly be refused by the cap anyway (`inf > max`),
            // except `inf · 0` is NaN — so the check is on the FACTORS, not the product. The value's
            // unit is a factor too: there is no floor under it any more (one number with the core's
            // gate), so a published unit that is not a finite POSITIVE number is refused as well,
            // never compared: `0` would size every order at nothing and pass any size, a negative one
            // is a sign error. (The mount folds such a value to 1.0 before it is published; this is
            // the door's own fail-closed answer for a roster that was built some other way.)
            if !(qty.is_finite()
                && price.is_finite()
                && value.per_unit.is_finite()
                && value.per_unit > 0.0)
            {
                return Some(format!(
                    "order qty {qty} at price {price} has no finite notional, so it cannot be \
                     checked against the node's policy ceiling max_notional_per_order {max:.2} — \
                     refused rather than passed (a NaN notional is never above the ceiling)"
                ));
            }
            let n = value.order_notional(qty, price);
            if n > max {
                // Names the SETTING, not a variable: the ceiling is `policy.max_notional_per_order`
                // on the node (settings unification, Phase 5) and telling the caller
                // to export something would send them looking for a knob that no longer exists.
                // The value clause is EMPTY at the default value (`Linear`, 1.0), so every sentence
                // that existed before the node counted a value reads byte-identically.
                let note = value_note(value);
                return Some(format!(
                    "order notional {n:.2} exceeds the node's policy ceiling \
                     max_notional_per_order {max:.2}{note}"
                ));
            }
        }
        None
    }

    /// The DRY-RUN verdict for the Preview verb: the same edge policy [`Self::vet`] enforces, but
    /// WITHOUT consuming a rate token — a preview is read-only and must not drain the command budget,
    /// nor be rate-limited into uselessness. Of THIS limiter's two policies only the deterministic
    /// notional cap is evaluated; the transient rate limiter is deliberately not assessed.
    /// `Some(reason)` ⇒ would refuse; `None` ⇒ would pass. ⚠ It is the FIRST of a preview's four
    /// steps, not the whole preview: both preview surfaces then run the venue gate, the account gate
    /// and a bracket's own verdict, as [`accept_command`] does.
    ///
    /// `blocks` and `orders` are [`Self::vet`]'s, and a preview must hand them the SAME roster and
    /// the SAME named order the real command would meet — a preview that sized at 1.0 where the
    /// send sizes with the instrument's multiplier would report as accepted a command that is then
    /// refused.
    pub fn preview_vet(
        &self,
        cmd: &WireCommand,
        blocks: &[vike_exec::VenueBlock],
        orders: &[vike_exec::OrderView],
    ) -> Option<String> {
        self.notional_reason(cmd, blocks, orders, None)
    }

    /// [`Self::preview_vet`] with the foreign symbol's value ([`Self::vet_notional`]'s `foreign`), so
    /// a preview sizes an inverse foreign symbol as the send will.
    pub fn preview_vet_foreign(
        &self,
        cmd: &WireCommand,
        blocks: &[vike_exec::VenueBlock],
        orders: &[vike_exec::OrderView],
        foreign: Option<ContractValue>,
    ) -> Option<String> {
        self.notional_reason(cmd, blocks, orders, foreign)
    }

    /// Token-bucket refill+take. The bucket caps at ~1s of rate (a short burst, never an unbounded
    /// backlog after an idle period).
    fn take_token(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.rate_per_sec).min(self.rate_per_sec.max(1.0));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// What one unit of quantity is worth on the engine a command addresses, for the instrument it
/// names — read off the published `blocks`, and **the default value (`Linear`, 1.0) when it cannot
/// be resolved** (`notional_reason`'s doc has the argument and the list of what "cannot" covers).
///
/// `account` is the WIRE spelling: absent and `DEFAULT` both name the bare-venue book and a label
/// names `venue#LABEL`, resolved through `route_key_of` — the composer [`account_refusal`] and the
/// core's own routing use — never by suffixing text here. The symbol is the ORDER's, looked up in
/// that engine's grid exactly as the core `RiskGate` does (`Account::contract_value_of` on the
/// request's symbol), so a symbol the grid does not list falls to the engine's default, `Linear`
/// 1.0 for every engine `vike_tradehub_core::make_engine` builds.
fn book_value(
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    blocks: &[vike_exec::VenueBlock],
) -> ContractValue {
    use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};
    // A label that does not parse is `lower_command`'s refusal, not a reason to guess a book here.
    let label = match account.map(parse_wire_account).transpose() {
        Ok(label) => label.unwrap_or(AccountLabel::Default),
        Err(_) => return ContractValue::default(),
    };
    label_value(venue, &label, symbol, blocks)
}

/// The value of the instrument the OPEN order `coid` rests on — the engine that holds it (its venue
/// and account, as the published order says them) and its symbol, resolved by [`label_value`]
/// exactly as [`book_value`] resolves a `Submit`'s — and **the default value when the published
/// `orders` hold no open order of that id** (`notional_reason`'s doc has the list of what that
/// covers, and why it is the arithmetic a `Modify` always had).
///
/// Open means NOT terminal, the question `vike_exec::CoreSnapshot::open_order` asks of the same
/// rows: a terminal order is one the core ignores a modify of, so it lends this edge nothing.
fn order_value(
    coid: &str,
    orders: &[vike_exec::OrderView],
    blocks: &[vike_exec::VenueBlock],
) -> ContractValue {
    let Some(order) = orders.iter().find(|o| o.client_order_id == coid && !o.status.is_terminal())
    else {
        return ContractValue::default();
    };
    // `None` is how a snapshot spells the DEFAULT account (`OrderView::account`'s doc).
    let label = order.account.clone().unwrap_or_default();
    label_value(&order.venue, &label, &order.symbol, blocks)
}

/// The value of `symbol` on the engine whose routing key is `route_key_of(venue, label)`, read off
/// the published `blocks` — the ONE lookup behind [`book_value`] (a frame names the book) and
/// [`order_value`] (a resting order names it), so the two cannot answer differently about which
/// block is which. **The default value when no published block carries that key.** There is no
/// floor under it: the door and the core's `RiskGate` call one function on one value (decision 0170).
fn label_value(
    venue: &str,
    label: &vike_model::accounts::account_keys::AccountLabel,
    symbol: &str,
    blocks: &[vike_exec::VenueBlock],
) -> ContractValue {
    let key = vike_model::accounts::account_keys::route_key_of(venue, label);
    blocks
        .iter()
        .find(|b| b.route_key == key)
        .map_or_else(ContractValue::default, |b| b.contract_value_of(symbol))
}

/// The clause that tells an operator why a refused notional is not `qty × price` — EMPTY at the
/// default value (`Linear`, 1.0), so every refusal sentence that existed before this edge counted a
/// value reads byte-identically (tests and runbooks key off those words).
fn value_note(value: ContractValue) -> String {
    match value.kind {
        NotionalKind::Linear if value.per_unit == 1.0 => String::new(),
        NotionalKind::Linear => format!(
            " (the notional counts this instrument's contract multiplier {})",
            value.per_unit
        ),
        NotionalKind::Inverse => format!(
            " (an inverse contract: the notional is the quantity times {}, the price does not enter)",
            value.per_unit
        ),
    }
}

/// Why [`accept_command`] refused, and whether the SURFACE must close.
///
/// The three variants are exactly the three non-`Ack` outcomes the TCP arm has always produced;
/// splitting them out (instead of one `String`) is what preserves the "a `Gone` closes the
/// connection, a `Busy`/refusal does not" rule across BOTH surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptError {
    /// Refused at the server edge — the rate limiter, the notional cap, or a command
    /// [`lower_command`] declines to lower (e.g. a submit with no pre-minted client-order-id). The
    /// surface stays open; the caller may send another command.
    Refused(String),
    /// The core's ingest lane was FULL. Transient — retry.
    Busy,
    /// The core's ingest lane is CLOSED (the core is shutting down). Terminal for the surface.
    Gone,
}

impl AcceptError {
    /// The operator-visible text. The `Busy`/`Gone` strings are the verbatim `Response::Error`
    /// bodies the TCP arm sent before this was factored out — do not reword them casually, a
    /// client may key off them.
    pub fn message(&self) -> String {
        match self {
            AcceptError::Refused(msg) => msg.clone(),
            AcceptError::Busy => "core busy, retry".to_string(),
            AcceptError::Gone => "core is shutting down".to_string(),
        }
    }

    /// True when the surface must stop after reporting this (only [`AcceptError::Gone`]).
    pub fn is_fatal(&self) -> bool {
        matches!(self, AcceptError::Gone)
    }
}

impl std::fmt::Display for AcceptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// What [`accept_command`] accepted — the reply-shaping half of its verdict. Almost every command
/// lowers into the core and echoes a coid ([`vike_tradehub_client::proto::Response::Ack`]); the ONE exception is the REQ-7
/// settings write, which lands on DISK at the daemon edge (nothing enters the core, there is no
/// coid) and whose caller instead needs `restart_required`
/// ([`vike_tradehub_client::proto::Response::SettingsWritten`]). An enum rather than a stringly convention so a surface cannot
/// accidentally ack a settings write as an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// The command entered the core's single-writer lane; the coid to echo (empty for the
    /// account-wide verbs, exactly as before), and the door's NOTES on it: plain-words sentences about an
    /// order that WAS sent (today one: it was placed into a closed market, `crates/vike-tradehub/src/server/session_note.rs`'s `note_for`).
    /// Empty for every command but an order, and for an order with nothing to say.
    Coid {
        /// The minted/echoed client-order-id.
        coid: String,
        /// What the door says about the order it sent; never a reason to refuse.
        notes: Vec<String>,
    },
    /// A `SetSetting` landed on disk. `restart_required` is the restart-to-apply signal
    /// ([`SettingsShowSource::apply_set_setting`] decides it): `false` ONLY when the key is
    /// hot-safe ([`crate::hot_reload::classify`]) AND the daemon's summary tick confirmed the
    /// apply executed; `true` otherwise — the running node keeps its boot-time value.
    SettingsWritten {
        /// `true` ⇒ the running node keeps its boot-time value for this key until restarted.
        restart_required: bool,
    },
}

impl Accepted {
    /// The coid for surfaces whose reply vocabulary is coid-shaped (the Telegram channel): the
    /// echoed coid, or the account-wide empty string for a settings write. ⚠ It drops the notes: the
    /// Telegram channel's reply carries the coid alone (the door writes each note to its log line too).
    pub fn into_coid(self) -> String {
        match self {
            Accepted::Coid { coid, .. } => coid,
            Accepted::SettingsWritten { .. } => String::new(),
        }
    }
}

/// **The ONE acceptance path.** Every remote control SURFACE — the TCP
/// [`vike_tradehub_client::proto::Request::Command`] arm and
/// the opt-in `telegram` channel (behind the crate feature of the same name) — funnels through
/// here, so neither can be gated differently from the other by accident.
///
/// In order:
/// 1. [`ControlLimits::vet`] — consumes a rate token, then applies the per-order notional cap.
///    Refusal short-circuits: NOTHING is lowered and nothing is audited.
///    Then [`venue_refusal`] — the ROUTING gate: a command naming a venue this node runs no engine
///    for is REFUSED rather than falling through to the primary engine. Inside this step, AFTER the
///    rate token (so mis-addressed frames cannot be flooded past the bucket) and before everything
///    below. Then [`account_refusal`] (the BOOK within that venue) and, for a bracket,
///    [`bracket_refusal`] (its values, and what the one engine it reaches can hold) and, for a plain
///    submit, [`plan_submit_symbol`] (a symbol its engine does not trade: admitted first when the
///    venue's adapter signs on the order's own symbol, refused when it signs the mounted one).
/// 2. [`audit::sanitize_reason`] — the rationale is remote free text landing in a structured JSON
///    log line, so it is stripped/capped HERE, once, for every surface.
/// 3. [`command_kind`] — read off the wire command BEFORE [`lower_command`] consumes it.
/// 4. [`lower_command`] — into the core's real `Command`/`OrderIntent`.
/// 5. `CommandSink::try_command` — the non-blocking single-writer lane.
/// 6. [`audit::record`] — ONLY on acceptance, exactly as before.
///
/// **The ONE branch: `WireCommand::SetSetting` (REQ-7) replaces steps 4–6** — after the SAME rate
/// token and the SAME sanitizer, it lowers into the settings DATABASE instead of into the core
/// ([`SettingsShowSource::apply_set_setting`]: the loader-validated one-row write, no confirm
/// ceremony — 0086 point 7) and its audit record is [`audit::record_settings_write`], which carries
/// the old→new values. `settings` is that boot-threaded source — `None` (a surface with no settings
/// lowering: the Telegram channel, a server started without one) refuses the verb; every other
/// command ignores the parameter entirely.
///
/// The `reason` never reaches [`lower_command`], so it can never touch `OrderRequest`, the core
/// fold, the journal, or a venue. `peer` is the TCP peer for a socket surface and `None` for a
/// non-socket one (the Telegram channel names its origin inside `reason` instead).
///
/// `key_id` is the caller's authenticated identity — `vike_hub_proto::auth::HubKeys`'
/// `key_id`, the stable non-secret fingerprint of the key whose mac verified, resolved once per
/// connection in `handle_connection`. It is the ACTOR the change journal records, because this
/// daemon authenticates a key and never a person. `None` for a surface that authenticates no key
/// (the Telegram channel, whose own identity is its chat id) — recorded as an ABSENT field rather
/// than as an invented one. ⚠ It is an `Option<&str>` next to an `Option<SocketAddr>`, so the two
/// cannot be transposed at a call site the way four adjacent `&str`s could (the hazard
/// [`audit::SettingsWriteAudit`] exists for).
///
/// `engines` is the venues this node runs an engine for
/// ([`crate::publish::PublisherHandle::engine_venues`]), against which the command's addressed
/// venue is checked by [`venue_refusal`] — step 1b, immediately after the rate token so a peer
/// cannot spam mis-addressed commands for free, and before ANYTHING is lowered, audited or sent.
/// An EMPTY slice means "this node cannot answer that question" and refuses nothing (see
/// [`venue_refusal`] for why that is a deadlock-avoidance rule rather than a soft default); a
/// surface with no engine roster at all passes `&[]` and is byte-identical to before this gate
/// existed. ⚠ The Telegram channel is exactly that surface today — see its call site.
///
/// `route_keys` is the ROUTING KEYS that same roster publishes, one per engine
/// ([`crate::publish::PublisherHandle::engine_route_keys`]), against which the named ACCOUNT of a
/// submit — or of a runtime MOUNT, the same verdict one plane up, or of a mass-cancel / flatten /
/// market exit that names one — is checked by
/// [`account_refusal`] — step 1c, immediately after the venue gate and for the case
/// that gate passes: the venue IS run and the account is not. It is a SECOND slice rather than a
/// wider first one because the two gates answer different questions and a refusal that widened
/// would change what an existing sentence means; [`account_refusal`]'s own doc argues it. Empty has
/// the identical UNKNOWN meaning `engines` has, and cannot disagree with it — both are projections
/// of one `portfolio.venues`, so they are empty together. ⚠ For every verb but a BRACKET: its gate
/// needs a POSITIVE answer ("the account it names runs", or, naming none, "this venue's one account
/// is the default one") that an empty roster cannot give, and it refuses there
/// (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_book_refusal` and `bracket_roster_unknown`).
///
/// `blocks` is the engine BLOCKS that same roster publishes
/// ([`crate::publish::PublisherHandle::engine_blocks`]), read by TWO gates. A bracket's, step
/// 1d ([`bracket_refusal`]), which must know what the one engine the account gate admitted it to
/// (the one it NAMES, or the venue's only default one) TRADES — a venue adapter places every order on its engine's own instrument, whatever the frame
/// names. And the notional ceiling of step 1, which reads the addressed engine's CONTRACT
/// MULTIPLIER for the instrument it sizes (`ControlLimits::vet`; an empty or unmatched roster
/// sizes at 1.0, as the ceiling did before it counted one). Every other verb ignores it. A surface
/// that passes `&[]` refuses every bracket rather than admitting one unchecked
/// ([`super::refusal::bracket_engine_refusal`]'s fail-closed arm).
///
/// `orders` is the order the command names, off the same published snapshot
/// ([`crate::publish::PublisherHandle::orders_named_by`]), read by TWO gates: step 1c', which refuses
/// a `Submit` whose client order id an order already carries (`coid_reuse_refusal`; ANY status), and
/// the notional ceiling of step 1, which sizes a `Modify` with the contract multiplier of the
/// instrument that (OPEN) order rests on (`ControlLimits::vet`; an order the snapshot does not
/// hold sizes at 1.0, as a `Modify` always did). Every other verb ignores it, and a surface that
/// passes `&[]` is byte-identical to the node before the edge could resolve one.
///
/// `recon` is the HELD reconcile divergence a `ReconConfirm` names, off the same published snapshot
/// ([`crate::publish::PublisherHandle::recon_alerts_named_by`]), read by ONE consumer: the audit
/// record, which names the divergence's account, kind and description beside the id
/// ([`audit::record_recon_confirm`]). Every other verb passes it empty and ignores it, and it gates
/// nothing: an id the snapshot does not hold is still accepted and refused by the core.
///
/// `mounts` is the published mount rows an `UnlatchMount` names ([`crate::publish::PublisherHandle::mounts_named_by`]:
/// every real mount row for that verb and EMPTY for every other command), read by ONE gate: step 1d'',
/// `super::unlatch::plan_unlatch` (decision 0239), which refuses a mount the node does not run, one that is not
/// latched and one that still holds a position, in words, before the `Ack`. It is advice: the core sees the
/// mount's live orders and armed conditionals too and refuses on them itself. EMPTY is "this node cannot judge"
/// and passes the frame on.
///
/// `facts` is where this surface looks up the FACTS of a symbol the addressed engine was not started
/// on (its grid and contract value), read by ONE gate: [`plan_submit_symbol`], step 1e, which
/// sends the engine a journaled `Command::AdmitSymbol` ahead of the order on the same lane
/// (decision 0173). `None` (a surface with no source) leaves that gate refusing a foreign symbol
/// by name, as the interim gate did; a symbol the engine already trades never asks for it.
///
/// `Ok` is the [`Accepted`] verdict the surface shapes its reply from ([`Accepted::Coid`] ⇒
/// `Ack`, [`Accepted::SettingsWritten`] ⇒ `SettingsWritten`).
#[expect(clippy::too_many_arguments)] // see `serve_with_link_policy`'s note — each engine roster is
// one more gate INPUT on a function whose parameter list is the whole acceptance surface, and each
// one is documented at its own name above.
pub fn accept_command(
    cmd: WireCommand,
    reason: Option<&str>,
    limits: &mut ControlLimits,
    sink: &CommandSink,
    settings: Option<&SettingsShowSource>,
    engines: &[String],
    route_keys: &[String],
    not_running: &[vike_exec::NotRunningAccount],
    blocks: &[vike_exec::VenueBlock],
    orders: &[vike_exec::OrderView],
    recon: &[vike_exec::ReconAlertView],
    mounts: &[vike_exec::MountView],
    facts: Option<&dyn SymbolFacts>,
    peer: Option<std::net::SocketAddr>,
    key_id: Option<&str>,
) -> Result<Accepted, AcceptError> {
    if let Some(refusal) = limits.vet_rate() {
        return Err(AcceptError::Refused(refusal));
    }
    // A venue whose one account runs two engines (a lane twin is up) has the order put on the engine of
    // its lane before any gate reads the address ([`lane_addressed`]); every other command passes as it is.
    let mut cmd = lane_addressed(cmd, blocks, facts);
    // The ceiling sizes a Submit with its engine's value for the symbol, and an engine lists no row for a
    // symbol it was never told about: its facts' value stands in (an inverse foreign symbol would
    // otherwise read as `qty · price`). Only asked when a ceiling is set. ⚠ An `Err` is the symbol door
    // REFUSING the symbol: there is no value to size it with, and the engine's default would count the
    // pair's own quote money against a ceiling in the account's (1,000 USDJPY "157000"), so the ceiling
    // is not applied here and the door's refusal is said at step 1e, in its place in the order of gates.
    let foreign =
        if limits.has_ceiling() { foreign_contract_value(&cmd, blocks, facts) } else { Ok(None) };
    if let Ok(value) = &foreign
        && let Some(refusal) = limits.vet_notional(&cmd, blocks, orders, *value)
    {
        return Err(AcceptError::Refused(refusal));
    }
    // 1a. AN ACCOUNT THAT DOES NOT RUN (decision 0156), FIRST among the routing gates so the
    // refusal names its REASON rather than the venue's or the account's bare absence — and so an
    // account-less order for a venue whose default account does not run is answered before the
    // `Ack` instead of by the core after it. `not_running_refusal`'s doc carries the classification.
    if let Some(refusal) = not_running_refusal(&cmd, not_running) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1a'. AN ACCOUNT STARTED IN THIS RUNNING NODE THAT HAS NOT YET BEEN GIVEN ITS VENUE BALANCE (decision
    // 0169): an OPENING command is held in words until the first reconcile pass lands the balance, because
    // until then its engine judges on the seed cash. Right behind the not-running gate (the account
    // runs, but is not yet orderable) and ahead of everything that could cost a lookup or a lowering.
    if let Some(refusal) = awaiting_balance_refusal(&cmd, facts) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1a''. A LANE TWIN KEPT UP ONLY TO BE CLOSED (its row is gone, it still holds something): it takes no new
    // position. Reducing commands pass; `lane_twin_closing_refusal`'s doc carries the classification.
    if let Some(refusal) = lane_twin_closing_refusal(&cmd, orders, blocks, facts) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1b. THE ROUTING GATE. Deliberately here — after the rate token (so a peer cannot flood
    // mis-addressed frames past the bucket) and before the sanitizer, the lowering, the core and
    // the audit record: a command that names a book this node does not have is not a command this
    // node may reinterpret, so nothing downstream should ever see it. A refusal is audited nowhere,
    // which is this daemon's uniform rule for every refused command (see the `SetSetting` arm's
    // declared note below).
    if let Some(refusal) = venue_refusal(&cmd, engines) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1c. THE ACCOUNT GATE, immediately after the venue one and deliberately in that order: a venue
    // this node does not run is refused in the VENUE's words above, so what reaches here is the
    // narrower fault — the exchange is mounted and the named BOOK is not. Before this the node
    // Acked such a frame and the core refused it out of band, so the client printed `accepted` over
    // an order that never existed; the whole of the change is that the node answers first. Same
    // audit rule as every refusal on this path: none.
    if let Some(refusal) = account_refusal(&cmd, route_keys) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1c'. A SUBMIT WHOSE CLIENT ORDER ID AN ORDER ALREADY CARRIES, said in words before the Ack. Past this
    // door the core registers the new order over the old one and the gatewayhub does not send a coid it has
    // seen, so the command would be Acked and then die silently. `coid_reuse_refusal`'s doc has the case.
    if let Some(refusal) = coid_reuse_refusal(&cmd, orders) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1d. THE BRACKET'S OWN VERDICT, immediately after the account gate has established which ONE
    // engine a bracket reaches: its values, then what that engine can hold (does it trade the
    // bracket's symbol, and is its own lane one that can hold a stop-loss). It is the SAME call both
    // preview surfaces make as their fourth step, rather than a second composition of the same
    // checks, so a Preview and a Command cannot answer one frame differently. `lower_command`
    // checks the values again for a caller that lowers a frame without this gate; the engine half
    // it cannot check, because it holds no roster. Same audit rule as every refusal here: none.
    if let Some(refusal) = bracket_refusal(&cmd, blocks) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1d'. THE GIVE-BACK OF AN ADMITTED SYMBOL (decision 0205), judged against the same published block: the symbol
    // must be one the engine admitted, and the engine must be flat in it with no live order. The statement carries
    // the block's spelling of the symbol (`plan_unadmit`'s doc); it is advice, the engine is the authority.
    match plan_unadmit(&cmd, blocks, orders) {
        UnadmitPlan::Pass => {}
        UnadmitPlan::Refuse(why) => return Err(AcceptError::Refused(why)),
        UnadmitPlan::Release { symbol: engine_spelling, .. } => {
            if let WireCommand::UnadmitSymbol { symbol, .. } = &mut cmd {
                *symbol = engine_spelling;
            }
        }
    }
    // 1d''. THE UN-LATCH OF A MOUNT'S BUDGET (decision 0239), judged against the published mount rows: the mount must
    // be one the node runs, latched, and flat. The command carries the published spelling of its id; it is advice,
    // the core is the authority (it also sees the mount's live orders and armed conditionals).
    match plan_unlatch(&cmd, mounts) {
        UnlatchPlan::Pass => {}
        UnlatchPlan::Refuse(why) => return Err(AcceptError::Refused(why)),
        UnlatchPlan::Send { mount_id: published } => {
            if let WireCommand::UnlatchMount { mount_id } = &mut cmd {
                *mount_id = published;
            }
        }
    }
    // 1e. A PLAIN SUBMIT's symbol, against the same engine block: the plain-order twin of 1d. A
    // symbol the venue's adapter can sign comes back canonical (the venue's spelling) with its
    // admission; the admission is sent below, ahead of the order.
    //
    // The order is lowered ONCE here first, only to learn whether it can be: the plan below may open a
    // price subscription, and an order that cannot be lowered (an empty client order id, an unknown
    // order type) must not have cost one.
    if matches!(cmd, WireCommand::Submit(_)) {
        lower_command(cmd.clone()).map_err(AcceptError::Refused)?;
    }
    // The symbol the ceiling could not size (step 1) is refused now, with the door's words: the committing
    // plan below is asked again and a facts source that changed its answer meanwhile must not let an
    // unsized order through.
    if let Err(why) = foreign {
        return Err(AcceptError::Refused(why));
    }
    let mut admit = None;
    // Whatever the plan opened is given back on EVERY way out that does not admit the symbol.
    let mut _price_guard = None;
    match plan_submit_symbol(&cmd, blocks, facts, true) {
        SymbolPlan::Pass => {}
        SymbolPlan::Refuse(refusal) => return Err(AcceptError::Refused(refusal)),
        SymbolPlan::Place { symbol, admit: mut admission, wait } => {
            if let (Some(source), Some(a)) = (facts, admission.as_deref()) {
                _price_guard = Some(PriceGuard {
                    source,
                    route_key: a.route_key.clone(),
                    symbol: a.symbol.clone(),
                });
            }
            // An opening order on a symbol with no price yet is held for one (bounded), BEFORE
            // anything is sent: a refusal here leaves the engine as it was. The price it waited
            // for becomes the engine's first mark for the symbol.
            let mut waited_px = None;
            if let (Some(wait), Some(source)) = (&wait, facts) {
                let held = std::time::Instant::now();
                let px = await_price(source, wait, &symbol).map_err(|why| {
                    // ⚠ The reply may go to a client that has stopped listening: a client whose
                    // `config.node_ack_wait_ms` row is shorter than this hold (`PRICE_WAIT`) hangs up
                    // INSIDE it (the default outlasts it now; it used to be 2 s against 4 s), and
                    // a refusal is audited nowhere. v0.1.81's IG drill left no row, no audit line and no
                    // log line; this line is the one place that refusal can still be read.
                    let coid = match &cmd {
                        WireCommand::Submit(req) => req.client_order_id.as_str(),
                        _ => "",
                    };
                    tracing::warn!(
                        coid,
                        route_key = %wait.route_key,
                        venue = %wait.venue,
                        %symbol,
                        held_ms = u64::try_from(held.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "vike-tradehub control: an order was held for a price and REFUSED at the door \
                         (its client may have stopped listening before this reply): {why}"
                    );
                    AcceptError::Refused(why)
                })?;
                waited_px = Some(px);
                // A symbol the engine already trades (a restored one) has no admission riding this order:
                // the cached facts carry the price instead, so its first mark does not race the tick.
                if admission.is_none() {
                    admission =
                        source.admission(&wait.route_key, &wait.venue, &symbol).ok().map(Box::new);
                }
                if let Some(a) = admission.as_mut() {
                    a.initial_mark = Some(px);
                }
            }
            // A priceless opening order on a LIVE lane twin's own symbol is sized HERE, at the price the door
            // holds, so `policy.max_notional_per_order` binds whatever mark the twin engine has
            // ([`live_twin_unsized_open`]'s doc has the argument). Nothing to size it with is a refusal.
            //
            // A BRACKET with a MARKET entry is the same hole one verb over: its legs are sized by their
            // priced exits alone, which a client sets far under the entry's worth, so its ENTRY is sized
            // here at the same price ([`priced_for_sizing`] gives it that price as its entry price).
            if limits.has_ceiling()
                && let Some((route_key, own)) = live_twin_unsized_open(&cmd, blocks, facts)
                && let Some(venue) = cmd.addressed_venue()
            {
                let (verb, what) = if matches!(cmd, WireCommand::Bracket(_)) {
                    ("bracket", "bracket")
                } else {
                    ("submit", "order")
                };
                let px = waited_px.or_else(|| {
                    facts.and_then(|source| {
                        source.price(&route_key, venue, &own, false).ok().flatten()
                    })
                });
                let Some(px) = px.filter(|p| p.is_finite() && *p > 0.0) else {
                    return Err(AcceptError::Refused(format!(
                        "{verb}: the perp lane `{route_key}` has no price for `{own}` to size this \
                         {what} against the node's notional ceiling. The {what} was REFUSED: retry in a \
                         moment"
                    )));
                };
                if let Some(sized) = priced_for_sizing(&cmd, px)
                    && let Some(refusal) = limits.vet_notional(&sized, blocks, orders, None)
                {
                    return Err(AcceptError::Refused(refusal));
                }
            }
            if let WireCommand::Submit(req) = &mut cmd {
                req.symbol = symbol;
            }
            admit = admission;
        }
    }
    let audit_reason = audit::sanitize_reason(reason);
    let kind = command_kind(&cmd);
    if let WireCommand::SetSetting { file: _, key, value, confirm: _ } = cmd {
        // The REQ-7 settings write: same gate order as every command (the rate token above, the
        // sanitizer above), then the DATABASE write instead of the core one. NOTE the notional cap
        // is n/a by `notional_reason`'s own vetting decision at its arm. `file` and `confirm` are
        // not consulted (0086 point 6/7 — see `apply_set_setting`'s own doc): a row is named by
        // its `key`. They may arrive absent since step 1 of their retirement (`#[serde(default)]`
        // on the wire type) and are deleted from the wire in step 2.
        let src = settings.ok_or_else(|| {
            AcceptError::Refused(
                "settings write unavailable: this surface serves no settings source".to_string(),
            )
        })?;
        // ⚠ **DECLARED: a REFUSED settings write appends NO change-journal row on this surface**,
        // and the `?` below is the whole mechanism — it returns before
        // [`audit::record_settings_write`] is reached, so a refusal of any kind (an unknown key, a
        // loader validation, a `Busy`/`ArmingRosterEmpty`/`Rejected` from the row writer) is
        // answered on the wire and recorded nowhere durable. That is the UNDER-recording direction,
        // on the surface whose whole reason for journalling is attribution, and it is kept
        // deliberately for now: it is this DAEMON's uniform rule, not a SetSetting quirk —
        // `audit::record` fires only after `CommandSink::try_command` succeeds, so no refused
        // control command of any verb has ever been journalled, and recording refusals for one verb
        // alone would make the daemon's own trail inconsistent with itself.
        //
        // The LOCAL surfaces answer the other way and say so at their own sites:
        // `crates/vike-cli/src/cmd/settings_write.rs`'s module doc (decision 2) and
        // `crates/vike-app-core/src/ui/tool_views/venues.rs`'s `apply_arming` both append a refusal
        // row. The asymmetry is now declared on both sides rather than on one.
        let (write, restart_required) =
            src.apply_set_setting(&key, &value).map_err(AcceptError::Refused)?;
        // The record goes BOTH to the tracing line (console/journald) and to the durable change
        // journal — see `audit`'s module doc for why the log line alone was measured not to survive
        // on a real deployment. The journal is derived from the same boot-resolved settings
        // directory this write just landed in, so the ledger and the file it describes can never
        // resolve to two different projects.
        let journal = src.change_journal();
        audit::record_settings_write(audit::SettingsWriteAudit {
            peer,
            key_id,
            journal: journal.as_ref(),
            now_ms: vike_model::now_ms(),
            write: &write,
            reason: audit_reason.as_deref(),
            // The restart-to-apply bit the peer is about to be told, recorded so the journal
            // answers "was that ceiling actually ARMED" rather than only "was it written".
            outcome: if restart_required {
                vike_change_journal::Outcome::AppliedPendingRestart
            } else {
                vike_change_journal::Outcome::Applied
            },
        });
        return Ok(Accepted::SettingsWritten { restart_required });
    }
    // **A node that is not ACTIVE changes nothing on the exchange** (review r2, item 9). Admitting a symbol
    // commits to the gatewayhub, which on a venue that posts a leverage (bybit, okx, binance) makes the exchange
    // hold the account's figure for the symbol: a WRITE on the venue account that neither the HALT sentinel nor the
    // kill switch used to stop, and that nothing undoes. So while the addressed engine is `Halted` (no new
    // orders) or `Reducing` (the daemon cannot tell here whether the write would RAISE the leverage, so any
    // might), an OPENING order whose admission would commit is refused before anything is sent; a reducing order
    // is let through WITHOUT the admission (the engine already trades the symbol), so a close is never held up.
    let not_active = admit.as_deref().and_then(|admission| {
        blocks
            .iter()
            .find(|b| b.route_key == admission.route_key)
            .map(|b| b.trading_state)
            .filter(|state| *state != TradingState::Active)
            .map(|state| (admission.route_key.clone(), admission.symbol.clone(), state))
    });
    if let Some((route_key, symbol, state)) = not_active {
        if matches!(&cmd, WireCommand::Submit(req) if req.reduce_only) {
            admit = None;
        } else {
            return Err(AcceptError::Refused(admission_refusal_while_not_active(
                &route_key, &symbol, state,
            )));
        }
    }
    // Read before the command is lowered (moved): whether it is an OPENING order, for the live twin's rule below.
    // A bracket always opens (its entry does), so it is held to the same rule on an admitted symbol.
    let opens = match &cmd {
        WireCommand::Submit(req) => !req.reduce_only,
        WireCommand::Bracket(_) => true,
        _ => false,
    };
    // **The closed-market NOTE** (decision 0236): an order whose market is closed now is still sent, and the
    // answer says so. Read off the command as it will go (its symbol is the door's canonical spelling by
    // now), after every refusal above has passed, and said once the lane takes it, below. Never a refusal.
    let notes: Vec<String> =
        session_note::closed_market_note(&cmd, (limits.now_ms)()).into_iter().collect();
    let (lowered, coid) = lower_command(cmd).map_err(AcceptError::Refused)?;
    // The admission rides the lane the order is about to ride, so the fold applies it first. A
    // `Busy` on the order after it went is a retry that sends it again, and the command is
    // idempotent (it refreshes the grid); lowering ran first, so a refused frame admits nothing.
    if let Some(admission) = admit {
        tracing::info!(
            route_key = %admission.route_key,
            symbol = %admission.symbol,
            "admitting a symbol into the account's engine on demand (decision 0173)"
        );
        let (route_key, symbol) = (admission.route_key.clone(), admission.symbol.clone());
        let venue = route_key.split('#').next().unwrap_or_default().to_string();
        // What the source's commit can restate (below) is built from this copy; a surface with no source
        // keeps no copy.
        let first = facts.map(|_| admission.clone());
        match sink.try_command(Command::AdmitSymbol(admission)) {
            Ok(()) => {
                if let Some(source) = facts {
                    source.admitted(&route_key, &venue, &symbol);
                }
            }
            Err(rejected) => {
                return Err(match rejected {
                    CommandRejected::Busy => AcceptError::Busy,
                    CommandRejected::Gone => AcceptError::Gone,
                });
            }
        }
        // **The symbol's own margin rate** (decision 0182's per-symbol twin). `admitted` just committed the
        // admission to the daemon, which on a venue that posts a leverage made the venue hold the
        // account's figure for the symbol and read it back; a figure that came back is restated as a
        // SECOND `AdmitSymbol` (a refresh: the engine writes the symbol's `im_by_symbol` row), on the lane
        // the order is about to ride, so the order is judged at the leverage the venue holds. Nothing
        // confirmed, nothing sent: the symbol stays at the engine's own rate, as before.
        if let (Some(source), Some(first)) = (facts, first.as_deref())
            && let Some(second) = with_confirmed_leverage(source, first)
        {
            tracing::info!(
                %route_key, %symbol, im_requirement = ?second.im_requirement,
                "restating the admission with the symbol's own margin rate (the venue holds the \
                 account's leverage for it)"
            );
            if let Err(rejected) = sink.try_command(Command::AdmitSymbol(Box::new(second))) {
                return Err(match rejected {
                    CommandRejected::Busy => AcceptError::Busy,
                    CommandRejected::Gone => AcceptError::Gone,
                });
            }
        }
        // **A LIVE lane twin's extra symbol must be at the leverage the exchange was SEEN holding** (review r2,
        // finding 6): the commit above made the exchange hold the account's figure for the symbol and read it
        // back. Nothing confirmed is a refusal of the OPENING order on real money, where a demo twin trades the
        // symbol at the engine's own rate (the exchange keeping whatever it held for it). The symbol stays
        // admitted; the next order asks the exchange again ([`live_twin_extra_symbol`]).
        if let Some(source) = facts
            && opens
            && let Some(engine) = blocks.iter().find(|b| b.route_key == route_key)
            && live_twin_extra_symbol(&venue, &route_key, engine, facts, &symbol)
            && source.confirmed_leverage(&route_key, &symbol).is_none()
        {
            return Err(AcceptError::Refused(unconfirmed_leverage_refusal(&route_key, &symbol)));
        }
    }
    let confirmed = if let Command::ConfirmRecon(id) = &lowered { Some(*id) } else { None };
    // The held row a `recon cancel` names, for the same audit reason: the generic line would not say
    // WHICH order was sent a cancel.
    let recon_cancelled =
        if let Command::CancelReconOrder(id) = &lowered { Some(*id) } else { None };
    // The venue (and the override) the line must carry: `audit::record` names neither, and a resume
    // of ONE venue is not an account-wide `set_trading_state`.
    let resumed = if let Command::ResumeVenue { venue, force } = &lowered {
        Some((venue.clone(), *force))
    } else {
        None
    };
    // The account (and the state) the line must carry: the generic line would read as the node-wide
    // `set_trading_state` it is not.
    let account_set = if let Command::SetAccountTradingState { route_key, state } = &lowered {
        Some((route_key.clone(), *state))
    } else {
        None
    };
    // The book and the symbol a give-back names (decision 0205), for the audit line and for the facts source: the
    // generic line would not say which slot was freed.
    let given_back = match &lowered {
        Command::AdmitSymbol(a) if a.release => Some((a.route_key.clone(), a.symbol.clone())),
        _ => None,
    };
    // The mount an un-latch names (decision 0239), for the audit line: the generic one would not say which mount.
    let unlatched = if let Command::UnlatchMount { mount_id } = &lowered {
        Some(mount_id.clone())
    } else {
        None
    };
    match sink.try_command(lowered) {
        Ok(()) => {
            if let Some(mount_id) = &unlatched {
                audit::record_unlatch_mount(peer, mount_id, audit_reason.as_deref());
            } else if let Some((route_key, symbol)) = &given_back {
                audit::record_unadmit_symbol(peer, route_key, symbol, audit_reason.as_deref());
                // The source now WATCHES for the engine to let go of the symbol, and forgets what it kept
                // for it when it has (`LeaseFacts::settle_releases`).
                if let Some(source) = facts {
                    let venue = route_key.split('#').next().unwrap_or_default();
                    source.release_requested(route_key, venue, symbol);
                }
            } else {
                match (confirmed, &resumed) {
                    (Some(id), _) => audit::record_recon_confirm(
                        peer,
                        id,
                        recon.iter().find(|a| a.id == id),
                        audit_reason.as_deref(),
                    ),
                    (None, Some((venue, force))) => {
                        audit::record_resume_venue(peer, venue, *force, audit_reason.as_deref())
                    }
                    (None, None) => match (&account_set, recon_cancelled) {
                        (Some((route_key, state)), _) => audit::record_set_account_state(
                            peer,
                            route_key,
                            *state,
                            audit_reason.as_deref(),
                        ),
                        (None, Some(id)) => audit::record_recon_cancel(
                            peer,
                            id,
                            recon.iter().find(|a| a.id == id),
                            audit_reason.as_deref(),
                        ),
                        (None, None) => audit::record(peer, kind, &coid, audit_reason.as_deref()),
                    },
                }
            }
            // The log carries every note too: the Telegram channel's reply is the coid alone.
            for note in &notes {
                tracing::info!(%coid, "control door note: {note}");
            }
            Ok(Accepted::Coid { coid, notes })
        }
        Err(CommandRejected::Busy) => Err(AcceptError::Busy),
        Err(CommandRejected::Gone) => Err(AcceptError::Gone),
    }
}

/// Lower ONE [`WireCommand`] into the core's real `(Command, coid)` — the TOTAL mapping (every wire
/// variant maps). The returned `coid` is what the [`vike_tradehub_client::proto::Response::Ack`] echoes: the client-order-id for
/// order-scoped verbs, empty for the account-wide ones (mass-cancel / flatten / market-exit /
/// trading-state). `Err(msg)` is a request the server refuses to lower — surfaced as
/// [`vike_tradehub_client::proto::Response::Error`], never silently dropped.
pub(super) fn lower_command(wc: WireCommand) -> Result<(Command, String), String> {
    match wc {
        WireCommand::Submit(req) => {
            // SECURITY / idempotency policy: the NETWORK path must pre-mint its own coid. An empty
            // one would let the runtime mint (fine in-process, but a remote peer then has no stable
            // handle to cancel/dedup by), so it is refused rather than lowered.
            if req.client_order_id.trim().is_empty() {
                return Err("remote submit requires a pre-minted client_order_id".to_string());
            }
            let coid = req.client_order_id.clone();
            // `parse_wire_account` is the ONE parser for this field, and it is the same one
            // `MountStrategy`'s arm below calls — so `DEFAULT` is admitted here exactly as it is
            // there, and the reserved spelling stays that function's business, not this arm's.
            // Absence still means the default account, so every pre-existing client's frame
            // lowers byte-identically. A malformed label is REFUSED here, not swallowed into
            // `None`: `None` routes to the venue's default book, and turning a typo'd account
            // name into a silent trade on the wrong book is exactly the misroute this field
            // exists to delete.
            let account = match req
                .account
                .as_deref()
                .map(vike_model::accounts::account_keys::parse_wire_account)
                .transpose()
            {
                Ok(a) => a,
                Err(e) => return Err(format!("submit: `account` — {e}")),
            };
            // The INPUT door (`OrderType::parse`): the wire keeps the kind a string (the client
            // crate has no vike-model dependency, decision 0091), trimmed and case-insensitive here,
            // and a word that is not one of ours is REFUSED, naming the accepted words — never
            // lowered into a kind every adapter would coerce into a market order.
            let order_type = match OrderType::parse(&req.order_type) {
                Ok(t) => t,
                Err(e) => return Err(format!("submit: `order_type` — {e}")),
            };
            // The LIFETIME (`time_in_force`, the second INPUT door of this arm): absent is the default
            // `gtc`, which is every frame sent before the field existed. A word that is not one of ours
            // is REFUSED naming the accepted ones, never lowered to `gtc` (an order the operator meant to
            // expire, or to keep off the book's taker side, would rest as a plain limit). The same
            // `lifetime_refusal` the CLI applies first judges the pair with the order's kind: a
            // lifetime belongs to a LIMIT order and `gtd` needs a future expiry.
            let time_in_force =
                match req.time_in_force.as_deref().map(TimeInForce::parse).transpose() {
                    Ok(t) => t.unwrap_or_default(),
                    Err(e) => return Err(format!("submit: `time_in_force` — {e}")),
                };
            if let Some(why) =
                lifetime_refusal(&order_type, time_in_force, req.gtd_expiry, vike_model::now_ms())
            {
                return Err(format!("submit: `time_in_force` — {why}"));
            }
            let order = OrderRequest {
                client_order_id: req.client_order_id,
                venue: req.venue,
                symbol: req.symbol,
                side: req.side,
                qty: req.qty,
                order_type,
                price: req.price,
                trigger_price: req.trigger_price,
                reduce_only: req.reduce_only,
                time_in_force,
                gtd_expiry: req.gtd_expiry,
                account,
                ..Default::default()
            };
            Ok((Command::Order(OrderIntent::Submit(Box::new(order))), coid))
        }
        WireCommand::Cancel(coid) => Ok((Command::Order(OrderIntent::Cancel(coid.clone())), coid)),
        WireCommand::Modify { client_order_id, new_qty, new_price } => {
            let coid = client_order_id.clone();
            Ok((Command::Order(OrderIntent::Modify { client_order_id, new_qty, new_price }), coid))
        }
        // THE RISK-REDUCING TRIO carry their account through now. These three used to destructure
        // as `{ venue, symbol, .. }` — the `..` swallowed the account, so a `market-exit binance
        // ALT` reached the core as `market-exit binance` and fanned over every account of the
        // exchange. The reader is the submit arm's (`lower_reduce_account`, over the ONE parser),
        // and absence still lowers to `None`, which the core fans out exactly as before: the
        // account-less frame every existing client sends is unchanged, the panic button included.
        WireCommand::MassCancel { venue, symbol, account } => {
            let account = lower_reduce_account("mass_cancel", account.as_deref())?;
            Ok((Command::Order(OrderIntent::MassCancel { venue, symbol, account }), String::new()))
        }
        WireCommand::Flatten { venue, symbol, account } => {
            let account = lower_reduce_account("flatten", account.as_deref())?;
            Ok((Command::Order(OrderIntent::Flatten { venue, symbol, account }), String::new()))
        }
        WireCommand::MarketExit { venue, account } => {
            let account = lower_reduce_account("market_exit", account.as_deref())?;
            Ok((Command::Order(OrderIntent::MarketExit { venue, account }), String::new()))
        }
        WireCommand::SetTradingState(ws) => {
            Ok((Command::SetTradingState(project_wire_trading_state(ws)), String::new()))
        }
        // STRATEGY-level write (split-plane B4): the wire carries the core's own externally-tagged
        // `StrategyParams` JSON (delegated, not mirrored — see the variant's doc in the client
        // crate), so lowering IS a deserialize into the journaled schema. An undecodable payload is
        // REFUSED here — surfaced as `Response::Error`, never silently dropped — which is also what
        // keeps "the daemon folds only params shapes the core itself defines" true by construction.
        // The empty coid is the account-wide-verb convention (nothing order-scoped to echo).
        WireCommand::UpdateParams { venue, symbol, interval, mount_id, params } => {
            let params: StrategyParams = serde_json::from_value(params).map_err(|e| {
                format!(
                    "update_params: payload is not a vike StrategyParams \
                     (expected the externally-tagged form, e.g. {{\"SpreadMaker\": {{…}}}}): {e}"
                )
            })?;
            let update = ParamsUpdate { venue, symbol, interval, mount_id, params };
            Ok((Command::UpdateParams(Box::new(update)), String::new()))
        }
        // RUNTIME strategy MOUNT (split-plane B5): validate the SPEC at this edge with the daemon's
        // own profile machinery — `crate::mount_factory::validate_spec` runs the SAME
        // `DaemonProfile` refusals a `[strategy]` table faces at load (unknown name,
        // simulator-only, unread/mistyped params keys, name-XOR-rhai), so a remote peer gets a
        // `Response::Error` carrying the profile vocabulary's own message instead of a silent
        // recent-events note. RESOLUTION (compiling/instantiating the strategy) still happens in
        // the core via the injected `CoreConfig::strategy_factory` (which validates AGAIN — this
        // edge is UX, the factory is the authority); a refusal the core raises later (duplicate
        // LIVE id, unknown venue) surfaces in recent-events, the `UpdateParams` unknown-target
        // contract. The empty coid is the account-wide-verb convention.
        WireCommand::MountStrategy {
            venue,
            account,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        } => {
            // ⚠ **THE WIRE CAN NAME ONE NOW, AND THE ARGUMENT THAT SAID IT MUST NOT IS ANSWERED
            // RATHER THAN DROPPED.** This arm hard-coded `account: None` and said why: *"a remote
            // peer mounting onto a second account would be naming a book the operator never armed
            // for that channel"*, with `DaemonProfile::account` — a file the operator edits — as
            // the only door to a second account.
            //
            // What answers it is that the premise is not reachable. A peer cannot name a book the
            // operator never armed, because naming one resolves NOTHING: `mount_engine_resolution`
            // matches the label against the route keys of the engines this core actually runs, and
            // an account with no ACTIVE `account` row and no `__<LABEL>`
            // credentials has no engine and no route key. It is refused BY NAME. So the wire's
            // reach is exactly the set of books the operator already armed — which is the same
            // authority the profile door has, arrived at through a frame this peer signed.
            //
            // What the old comment got RIGHT and this keeps: absence still means the default
            // account, so every frame an existing client sends is unchanged, and
            // `DaemonProfile::account` is untouched as the file-shaped door.
            //
            // `parse_wire_account` is the reader, and it is the same one the order plane uses:
            // `DEFAULT` is admitted here where `AccountLabel::parse` (an `account` row's label) refuses it,
            // because on the wire that
            // spelling is how a client says *"the unlabelled account, deliberately"* as opposed to
            // saying nothing at all.
            let account = match account
                .as_deref()
                .map(vike_model::accounts::account_keys::parse_wire_account)
                .transpose()
            {
                Ok(a) => a,
                Err(e) => return Err(format!("mount_strategy: `account` — {e}")),
            };
            // `[strategy.params]`: the node wire carries JSON (the client crate names no
            // vike-model, decision 0091); `MountSpec` holds the profile vocabulary's `Params`,
            // which reads the JSON object directly. What a parameter table cannot hold (a `null`,
            // a non-object, an integer above `i64::MAX`) is a refusal, never a silent drop.
            let params: vike_model::params::Params = serde_json::from_value(params)
                .map_err(|e| format!("mount_strategy: `params` is not a parameter table: {e}"))?;
            let spec =
                MountSpec { venue, symbol, interval, account, controller_id, name, rhai, params };
            crate::mount_factory::validate_spec(&spec)
                .map_err(|e| format!("mount_strategy: {e}"))?;
            Ok((Command::MountStrategy(Box::new(spec)), String::new()))
        }
        // RUNTIME strategy UNMOUNT (split-plane B5): total except for an empty id (which can name
        // nothing). The core cancels the mount's attributed resting orders before removal — the
        // documented safe default (`vike_core`'s `unmount_strategy_runtime` arm is the authority).
        WireCommand::UnmountStrategy { controller_id } => {
            if controller_id.trim().is_empty() {
                return Err("unmount_strategy: empty mount id".to_string());
            }
            Ok((Command::UnmountStrategy { controller_id }, String::new()))
        }
        // The operator's RECONCILE verb: the id is a held divergence's `WireReconAlert::id`, and an
        // id the core does not hold is a recent-events note there, not an error here (the same
        // "unknown id is not a crash" contract `Command::ConfirmRecon` documents). The empty coid
        // is the account-wide-verb convention.
        WireCommand::ReconConfirm { id } => Ok((Command::ConfirmRecon(id), String::new())),
        // The operator's CANCEL of one held `UnknownOrder` row (decision 0196, rule (b)): the id is a
        // held row's `WireReconAlert::id`, and every refusal (an id not held, a row of another kind or
        // tier, one with no id the venue can cancel by) is the core's note and WARN, not an error here.
        WireCommand::ReconCancel { id } => Ok((Command::CancelReconOrder(id), String::new())),
        // The operator's PER-VENUE RESUME (decision 0180): the core releases the link dead-man's
        // halt of ONE venue. An empty venue can name nothing; a venue the node does not run was
        // refused before this point (`venue_refusal`, off `addressed_venue`), and one the link
        // dead-man holds nothing for is the core's refusal (a WARN and a note), because only the
        // fold knows what the latch holds. The empty coid is the account-wide-verb convention.
        WireCommand::ResumeVenue { venue, force } => {
            if venue.trim().is_empty() {
                return Err("resume_venue: empty venue".to_string());
            }
            Ok((Command::ResumeVenue { venue, force }, String::new()))
        }
        // ONE ACCOUNT's trading state: the book is composed with the ONE composer (`route_key_of`, the
        // core's own resolution), so `DEFAULT` is the bare venue id. A malformed label is REFUSED here,
        // before the `Ack`: it is never read as another book (the submit arm's rule). An account the
        // node does not run was refused earlier (`account_refusal`, off the published roster); on an
        // unpublished roster the core's refusal is a note, and nothing moves.
        WireCommand::SetAccountTradingState { venue, account, state } => {
            if venue.trim().is_empty() {
                return Err("set_account_trading_state: empty venue".to_string());
            }
            let label = vike_model::accounts::account_keys::parse_wire_account(&account)
                .map_err(|e| format!("set_account_trading_state: `account` — {e}"))?;
            let route_key = vike_model::accounts::account_keys::route_key_of(&venue, &label);
            Ok((
                Command::SetAccountTradingState {
                    route_key,
                    state: project_wire_trading_state(state),
                },
                String::new(),
            ))
        }
        // THE GIVE-BACK of one admitted symbol (decision 0205): the book is composed with the ONE composer
        // (`route_key_of`), `DEFAULT` is the bare venue id, and a malformed label is REFUSED here before the `Ack`
        // (never read as another book). The statement is an `AdmitSymbol` that states `release`: the engine decides
        // what it may give back. An account the node does not run was refused earlier (`account_refusal`), and the
        // symbol's spelling was settled by `plan_unadmit` against the published block.
        WireCommand::UnadmitSymbol { venue, account, symbol } => {
            if venue.trim().is_empty() {
                return Err("unadmit_symbol: empty venue".to_string());
            }
            if symbol.trim().is_empty() {
                return Err("unadmit_symbol: empty symbol".to_string());
            }
            let label = vike_model::accounts::account_keys::parse_wire_account(&account)
                .map_err(|e| format!("unadmit_symbol: `account` — {e}"))?;
            let route_key = vike_model::accounts::account_keys::route_key_of(&venue, &label);
            Ok((
                Command::AdmitSymbol(Box::new(SymbolAdmission::releasing(
                    route_key,
                    symbol.trim(),
                ))),
                String::new(),
            ))
        }
        // THE UN-LATCH of one mount's budget (decision 0239): total except for an empty id, which can name no mount.
        // The id was settled against the published rows by `plan_unlatch`, and the core sanitizes it as it does
        // an unmount's; every other refusal (not latched, not flat, a live order) is the core's note and WARN.
        WireCommand::UnlatchMount { mount_id } => {
            if mount_id.trim().is_empty() {
                return Err("unlatch_mount: empty mount id".to_string());
            }
            Ok((Command::UnlatchMount { mount_id }, String::new()))
        }
        // THE TP/SL BRACKET: the wire payload IS `BracketSpec`, field for field, so lowering is a
        // move — after `check_bracket`, which refuses before the `Ack` the VALUES the core would
        // only discover at RELEASE, with the entry already filled. The empty coid is FORCED rather
        // than chosen: the runtime mints all three ids after this returns
        // (`vike_model::build_bracket`'s doc), so there is none to echo. That is the declared
        // exception to this function's pre-minted-id rule (`crates/vike-tradehub/CLAUDE.md`). Which
        // book it reaches, and whether that book's engine can hold it, was settled before this arm
        // ran (`account_refusal`'s bracket arm, then `bracket_refusal` in `accept_command`): this
        // function holds no roster, so it checks the frame only. The account is read by
        // `parse_wire_account`, the Submit arm's one parser: a malformed label is REFUSED here, never
        // swallowed into `None`, which would route the bracket to the venue's default book.
        WireCommand::Bracket(b) => {
            check_bracket(&b)?;
            let account = match b
                .account
                .as_deref()
                .map(vike_model::accounts::account_keys::parse_wire_account)
                .transpose()
            {
                Ok(a) => a,
                Err(e) => return Err(format!("bracket: `account` — {e}")),
            };
            let spec = BracketSpec {
                venue: b.venue,
                symbol: b.symbol,
                side: b.side,
                qty: b.qty,
                entry_price: b.entry_price,
                stop_loss: b.stop_loss,
                take_profit: b.take_profit,
                account,
            };
            Ok((Command::Order(OrderIntent::Bracket(Box::new(spec))), String::new()))
        }
        // The REQ-7 settings write is NOT a core command: `accept_command` intercepts it and
        // lowers it onto disk ([`SettingsShowSource::apply_set_setting`]) before this function is
        // reached. The arm exists so the mapping stays total; a future surface that calls
        // `lower_command` directly gets an honest refusal, never a silent drop.
        WireCommand::SetSetting { .. } => Err(
            "set_setting: not a core command — it is lowered at the daemon edge (accept_command)"
                .to_string(),
        ),
    }
}

/// Read a risk-REDUCING verb's wire `account` for [`lower_command`] — `parse_wire_account`, the
/// ONE parser (so `DEFAULT` is admitted here exactly as the submit and mount arms admit it, and the
/// reserved spelling stays that function's business), with a malformed label REFUSED rather than
/// swallowed into `None`.
///
/// ⚠ The refusal matters MORE here than on a submit, and in the opposite direction. On a submit a
/// swallowed label routes to the default book; on a reducing verb `None` means *every account of
/// the venue* — so turning a typo'd `ALT` into `None` would widen a one-book cancel into a
/// whole-exchange one, which is the misroute this field exists to delete wearing its widest coat.
fn lower_reduce_account(
    verb: &str,
    account: Option<&str>,
) -> Result<Option<vike_model::accounts::account_keys::AccountLabel>, String> {
    account
        .map(vike_model::accounts::account_keys::parse_wire_account)
        .transpose()
        .map_err(|e| format!("{verb}: `account` — {e}"))
}

/// Map the wire trading-state mirror back onto `vike_model::TradingState` — the reverse of
/// `crate::publish::project_trading_state`.
fn project_wire_trading_state(ws: WireTradingState) -> TradingState {
    match ws {
        WireTradingState::Active => TradingState::Active,
        WireTradingState::Reducing => TradingState::Reducing,
        WireTradingState::Halted => TradingState::Halted,
    }
}

/// The audit VERB for a wire command — a stable, low-cardinality string for the audit record (read
/// off the wire command BEFORE it is consumed by [`lower_command`]).
pub(super) fn command_kind(wc: &WireCommand) -> &'static str {
    match wc {
        WireCommand::Submit(_) => "submit",
        WireCommand::Bracket(_) => "bracket",
        WireCommand::Cancel(_) => "cancel",
        WireCommand::Modify { .. } => "modify",
        WireCommand::MassCancel { .. } => "mass_cancel",
        WireCommand::Flatten { .. } => "flatten",
        WireCommand::MarketExit { .. } => "market_exit",
        WireCommand::SetTradingState(_) => "set_trading_state",
        WireCommand::UpdateParams { .. } => "update_params",
        WireCommand::MountStrategy { .. } => "mount_strategy",
        WireCommand::UnmountStrategy { .. } => "unmount_strategy",
        WireCommand::SetSetting { .. } => "set_setting",
        WireCommand::ReconConfirm { .. } => "recon_confirm",
        WireCommand::ReconCancel { .. } => "recon_cancel",
        WireCommand::ResumeVenue { .. } => "resume_venue",
        WireCommand::SetAccountTradingState { .. } => "set_account_trading_state",
        WireCommand::UnadmitSymbol { .. } => "unadmit_symbol",
        WireCommand::UnlatchMount { .. } => "unlatch_mount",
    }
}

/// What the door's plan opened for a symbol it was about to admit, given back when the acceptance
/// ends without admitting it (a refusal, a `Busy` lane, any `?`): the source drops the price entry
/// it counted. A no-op for a symbol that WAS admitted meanwhile (the source checks), so it is
/// simply left armed to the end of the function.
struct PriceGuard<'a> {
    source: &'a dyn SymbolFacts,
    route_key: String,
    symbol: String,
}

impl Drop for PriceGuard<'_> {
    fn drop(&mut self) {
        let venue = self.route_key.split('#').next().unwrap_or_default();
        self.source.release(&self.route_key, venue, &self.symbol);
    }
}
