//! `server::control` — the CONTROL path's gating and acceptance: the server-edge limits
//! ([`ControlLimitsConfig`], [`ControlLimits`]), the ONE acceptance path every control surface
//! funnels through ([`accept_command`], with its [`ControlAcceptError`] and [`ControlAcceptReply`] verdicts) and
//! the lowering of a wire command into the core's real `Command` (`lower_command`).
//!
//! Split out of `server.rs` as a pure move; the module doc there carries the acceptance contract
//! (the gate ORDER, why a `Gone` closes the connection, why the rationale never reaches the core).
//! The routing gates `accept_command` consults are `super::refusal`'s.

use std::time::{Duration, Instant};

use vike_core::{CommandRejected, CommandSink};
use vike_execution::{Command, MountSpec, OrderIntent, ParamsUpdate, SymbolAdmission};
use vike_model::{
    ContractValue, NotionalKind, OrderBracket, OrderPeg, OrderPegAnchor, OrderRelease,
    OrderReleasePrice, OrderReleasePriceSide, OrderRequest, OrderType, StrategyParams, TimeInForce,
    attributes_refusal, display_refusal, lifetime_refusal, order_terms_refusal,
    price_source_refusal, stop_adjust_refusal,
};
use vike_tradehub_client::wire::CommandWire;

use super::post_only_door::post_only_cross_refusal;
use super::refusal::{
    account_refusal, awaiting_balance_refusal, bracket_refusal, check_bracket,
    lane_twin_closing_refusal, modify_price_refusal, not_running_refusal, one_account_addressed,
    order_client_id_reuse_refusal, release_symbol_refusal, release_venue_refusal,
    submit_side_refusal, venue_refusal,
};
use super::session_note;
use super::settings::SettingsShowSource;
use super::symbol_admission::{
    SymbolAdmitter, SymbolPlan, admission_refusal_while_not_active, await_price,
    foreign_contract_value, held_grid_refusal, lane_addressed, live_twin_extra_symbol,
    live_twin_unsized_open, market_order_to_size, modify_closes, order_closes,
    outcome_name_resolved, own_first_mark, plan_confirm_admission, plan_submit_symbol,
    priced_for_sizing, rounding_note, unconfirmed_leverage_refusal, with_confirmed_leverage,
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
/// [`vike_tradehub_client::proto::Request::Command`] arm calls for every control connection, the
/// Telegram channel's included). So even a leaked `VIKE_TRADEHUB_CONTROL_KEY` — or any
/// non-GUI client — cannot place an arbitrarily large order or flood the core. Per-CONNECTION (control
/// connections are few; each gets its own bucket), built from
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
    /// [`RECENT_MODIFY_PRICE_TTL`]: the published snapshot is coalesced, so for a moment it still shows
    /// the price from BEFORE that modify. Bounded by [`RECENT_MODIFY_PRICE_CAP`].
    recent_modify_price: Vec<(String, f64, Instant)>,
    /// The wall clock the closed-market note reads (`crates/vike-tradehub/src/server/session_note.rs`): `vike_model::now_ms`, and a
    /// field only so a test can stand the door on a closed market (`Self::set_clock`).
    now_ms: fn() -> i64,
    /// The kept rows the closed-market note reads a symbol's class from: `crate::tradehub_cli::settings`'s
    /// `session_rows`, and a field only so a test can hand it a list (`Self::set_session_rows`).
    session_rows: fn() -> Vec<vike_model::Instrument>,
    /// The node's LATE-RETRY memory (decision 0270, D7; [`Self::with_boot_order_client_ids`]): EMPTY until handed one.
    boot_order_client_ids: std::sync::Arc<std::collections::HashSet<String>>,
}

/// How long a passed modify's price outlives it here: far over the publisher's 15 ms poll plus the
/// core's coalescing, so the snapshot has caught up by the time it expires.
const RECENT_MODIFY_PRICE_TTL: Duration = Duration::from_secs(5);
/// Entries kept; the oldest is dropped first (the rate bucket already bounds how fast they arrive).
const RECENT_MODIFY_PRICE_CAP: usize = 64;

impl ControlLimits {
    /// A fresh per-surface limiter over `config` — the token bucket starts full (`rate_per_sec`).
    pub fn new(config: ControlLimitsConfig) -> Self {
        ControlLimits {
            max_notional: config.max_notional,
            rate_per_sec: config.rate_per_sec,
            tokens: config.rate_per_sec,
            last_refill: Instant::now(),
            recent_modify_price: Vec::new(),
            now_ms: vike_model::now_ms,
            session_rows: crate::tradehub_cli::settings::session_rows,
            boot_order_client_ids: std::sync::Arc::default(),
        }
    }

    /// Hand this surface the node's LATE-RETRY memory (decision 0270, D7): the order client ids its orders used
    /// before the last restart (within 24 h), built once at boot from the journal (`vike_core`'s
    /// `RestoreReport::recent_order_client_ids`) and shared read-only. [`super::refusal::order_client_id_reuse_refusal`] refuses a command
    /// that names one; a surface never handed it refuses nothing on that ground.
    #[must_use]
    pub fn with_boot_order_client_ids(
        mut self,
        ids: std::sync::Arc<std::collections::HashSet<String>>,
    ) -> Self {
        self.boot_order_client_ids = ids;
        self
    }

    /// The late-retry memory [`Self::with_boot_order_client_ids`] handed in (empty when none was).
    pub(crate) fn boot_order_client_ids(&self) -> &std::collections::HashSet<String> {
        &self.boot_order_client_ids
    }

    /// Stand this surface's clock on `now_ms` (a test of the closed-market note).
    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, now_ms: fn() -> i64) {
        self.now_ms = now_ms;
    }

    /// Hand this surface the kept rows the closed-market note reads (a test of the note).
    #[cfg(test)]
    pub(crate) fn set_session_rows(&mut self, rows: fn() -> Vec<vike_model::Instrument>) {
        self.session_rows = rows;
    }

    /// Remember the price an allowed `Modify` carries (see [`Self::recent_modify_price`]).
    fn note_modify_price(&mut self, cmd: &CommandWire) {
        let CommandWire::Modify { order_client_id, new_price, new_trigger_price, .. } = cmd else {
            return;
        };
        // The price the modify NAMES, as [`Self::notional_reason`] sizes it: its price, else its trigger.
        let Some(p) = new_price.or(*new_trigger_price) else { return };
        if !p.is_finite() {
            return;
        }
        let now = Instant::now();
        self.recent_modify_price
            .retain(|(_, _, at)| now.duration_since(*at) < RECENT_MODIFY_PRICE_TTL);
        if self.recent_modify_price.len() >= RECENT_MODIFY_PRICE_CAP {
            self.recent_modify_price.remove(0);
        }
        self.recent_modify_price.push((order_client_id.clone(), p, now));
    }

    /// The highest price a recent allowed modify of `order_client_id` carried, if any is still within the TTL.
    fn recent_modify_price(&self, order_client_id: &str) -> Option<f64> {
        let now = Instant::now();
        self.recent_modify_price
            .iter()
            .filter(|(c, _, at)| {
                c == order_client_id && now.duration_since(*at) < RECENT_MODIFY_PRICE_TTL
            })
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
    /// which instrument a bare order client id rests on. Every other verb ignores it, and `&[]` is
    /// "the node does not know that order", which sizes a `Modify` at the default value as it always
    /// did; the same `notional_reason` doc carries the rest.
    pub fn vet(
        &mut self,
        cmd: &CommandWire,
        blocks: &[vike_execution::VenueBlock],
        orders: &[vike_execution::OrderView],
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
        cmd: &CommandWire,
        blocks: &[vike_execution::VenueBlock],
        orders: &[vike_execution::OrderView],
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
    /// For a verb that is not a [`CommandWire`] at all and therefore has no notional to size: the
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
    /// `vike_execution::VenueBlock::contract_value_of` over the published `blocks` that [`accept_command`]
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
    /// **A `Modify` names an order client id and nothing else, so its instrument is read off the
    /// order that id names.** `orders` is the node's published OPEN order for that id
    /// ([`crate::publish::PublisherHandle::orders_named_by`] — the snapshot already carries
    /// every engine's registry with each order's venue, account and symbol), and the value is the
    /// one of the engine that holds it, for that order's symbol: the same two steps the core takes
    /// when it judges the projected order — `Engine::modify_order` reads the resting
    /// request out of the engine's registry, and the risk context it builds reads that request's
    /// symbol in the engine's account grid. The id is matched on OPEN orders only (a terminal order
    /// is one the core ignores a modify of).
    ///
    /// **The order this snapshot does not know is sized at the default value, exactly as a `Modify`
    /// was before this edge could resolve one — declared, not a claim that the value does not
    /// apply.** The snapshot is built by a coalesced publish, so an order client id placed moments ago may not
    /// be in it yet; an id that never existed, one already terminal, a bracket exit still HELD off
    /// the venue (those sit in `held_exits`, not in `orders`) and a surface that passes `&[]` all
    /// land there. The core's own `RiskGate` judges the projected order with the real value whenever
    /// the modify reaches an engine, and it is the enforcing gate regardless; this edge is
    /// defense-in-depth.
    ///
    /// **An INVERSE contract needs no price to be sized**, so a priceless (market) `Submit` on one
    /// is sized here too, by its quantity alone; a priceless order on a linear contract still has
    /// nothing to size at this edge and is the core's to judge. A stop or take-profit `Submit` is not
    /// priceless: it is sized at its trigger price when it names no price of its own. A MARKET `Submit`
    /// that names a price IS priceless (`OrderRequestWire::limit_price`): the venue fills it at the market, so the core
    /// sizes it at the mark like any other (`RiskGate::check_inner`), and this edge does not size it
    /// at the number it names. A `trigger_price` on one still sizes it: the core's gate reads it so, and
    /// a frozen fixture pins that (`fixtures/r5/risk.json`). ⚠ This function sizes what the FRAME
    /// carries; a market order is judged at the price this node holds by [`Self::market_reason`], the
    /// step every surface runs after this one.
    ///
    /// ⚠ **A non-finite size or price is REFUSED, not compared.** `NaN > max` is false, so a NaN
    /// notional used to read as "within the ceiling" — the same "cannot evaluate" reading the
    /// priceless-modify arm below exists to delete. A zero or negative qty is NOT this site's: a
    /// negative one is sized by its magnitude (pinned) and both are refused by the core `RiskGate`'s
    /// `non-positive-size` lane before any venue client sees them. The TCP wire cannot carry a
    /// non-finite number into this function (JSON has none), so this is the guard for a caller
    /// that builds a [`CommandWire`] in process.
    fn notional_reason(
        &self,
        cmd: &CommandWire,
        blocks: &[vike_execution::VenueBlock],
        orders: &[vike_execution::OrderView],
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
            // A CLOSE is never sized, at any arm below: a check here must not stop one ([`order_closes`]), and the
            // inverse arm, which needs no price, would refuse the desktop's ClosePosition of a big short.
            CommandWire::Submit(r) if order_closes(r, blocks) => None,
            CommandWire::Submit(r) => {
                // A symbol the engine does not trade yet has no row in its published grid; its own
                // facts' value (`foreign`) stands in, so an inverse foreign symbol is sized like one.
                let value = foreign.unwrap_or_else(|| {
                    book_value(&r.venue, r.account.as_deref(), &r.symbol, blocks)
                });
                // A stop or take-profit names its TRIGGER and no price: it is sized at the trigger,
                // the reference price the core's `RiskGate` gives it (`price.or(trigger_price)`), so
                // the door and the gate judge one number. A limit order (and a stop-limit) is sized
                // at its price as before; only an order with NEITHER (a market order) is unsized, and
                // a price named on a market order is no price (`OrderRequestWire::limit_price`): the
                // venue fills it at the market, so sizing it at the number it names let `qty 10, price
                // 1` through.
                match (r.own_price(), value.kind) {
                    (Some(p), _) => Some((r.qty, p, value)),
                    // an inverse notional does not contain the price: size it by the quantity
                    (None, NotionalKind::Inverse) => Some((r.qty, 0.0, value)),
                    (None, NotionalKind::Linear) => None,
                }
            }
            // The order keeps closing whatever it is re-priced to, while the position covers its new size.
            CommandWire::Modify { order_client_id, new_qty: Some(q), .. }
                if modify_closes(order_client_id, *q, orders, blocks) =>
            {
                None
            }
            // The value is the one of the order this order client id names, read off the published OPEN
            // order (the doc above); an order the snapshot does not hold is the default, the
            // arithmetic this arm used before it could resolve one. A modify that names a TRIGGER and no
            // price is sized at the trigger, as a stop is (`OrderRequestWire::own_price`).
            CommandWire::Modify {
                order_client_id, new_qty: Some(q), new_price: Some(p), ..
            }
            | CommandWire::Modify {
                order_client_id,
                new_qty: Some(q),
                new_price: None,
                new_trigger_price: Some(p),
            } => {
                // A resting MARKET order is worth what the market says, as the core's `gate_modify`
                // reads it: [`Self::market_reason`], not the number this names.
                open_market_order(order_client_id, orders)
                    .is_none()
                    .then(|| (*q, *p, order_value(order_client_id, orders, blocks)))
            }
            // ⚠ A qty-RAISING modify that names NO price is sized at the STANDING order's price
            // (the venue keeps the resting price), read off the published open order. This shape
            // used to fall into `_ => None` — "no price to size, therefore fine" — which let
            // `/modify <order client id> qty=<huge>` walk past the node's one order-size ceiling completely:
            // place a small in-cap order, then modify it up. When the snapshot has no price to give
            // (a market/stop order, an order client id it does not know yet) it is UNCHECKABLE and REFUSED
            // rather than waved through: "I cannot evaluate this ceiling" must never render as
            // "the ceiling passed". The operator's fix then is to name the price. The core
            // `RiskGate` DOES resolve the resting terms and is the enforcing gate regardless —
            // this is the defense-in-depth edge, failing CLOSED.
            CommandWire::Modify {
                order_client_id,
                new_qty: Some(q),
                new_price: None,
                new_trigger_price: None,
            } => {
                //
                // ⚠ The snapshot price can be STALE (a price modify moments ago, the publish
                // coalesced, the venue not yet answered), so the size is checked at the HIGHEST of
                // it, the price of a modify the order view says is still IN FLIGHT whichever surface
                // sent it (`OrderView::pending_price`), and any price an allowed modify of this order client id
                // carried through THIS surface in the last [`RECENT_MODIFY_PRICE_TTL`]. A standing price
                // that is zero, negative or non-finite is "no price": notional 0 would let any size
                // pass, and so is a pending one.
                //
                // A number left on a resting MARKET order is no standing price: the venue fills it at the
                // market, and the core sizes it at the mark.
                let open = orders
                    .iter()
                    .find(|o| o.order_client_id == *order_client_id && !o.status.is_terminal())
                    .filter(|o| o.order_type != OrderType::Market);
                if let Some(p) = open.and_then(|o| o.price).filter(|p| p.is_finite() && *p > 0.0) {
                    let pending =
                        open.and_then(|o| o.pending_price).filter(|p| p.is_finite() && *p > 0.0);
                    let p = pending.map_or(p, |r| p.max(r));
                    let p = self.recent_modify_price(order_client_id).map_or(p, |r| p.max(r));
                    Some((*q, p, order_value(order_client_id, orders, blocks)))
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
            CommandWire::UpdateParams { .. } => None,
            // VETTING DECISION (split-plane B5): the mount verbs are not orders either — a mount
            // request carries no qty×price to size, and every order the mounted strategy later
            // emits passes the core `RiskGate` + the mount's live risk budget, the enforcing gates
            // for strategy-originated size (the `UpdateParams` argument verbatim). The unmount is
            // risk-REDUCING (it cancels the mount's resting orders). The rate token in `vet` still
            // applies to both.
            CommandWire::MountStrategy { .. } | CommandWire::UnmountStrategy { .. } => None,
            // VETTING DECISION (operator reconcile verb): a confirm carries no qty x price - it
            // folds what the held divergence proposed, and the book it reaches is the one the
            // divergence names. The rate token in `vet` still applies.
            CommandWire::ReconcileConfirm { .. } => None,
            // VETTING DECISION (operator cancel of a held row): it carries no qty x price and sends a
            // single cancel; the rate token in `vet` still applies.
            CommandWire::ReconcileCancel { .. } => None,
            // VETTING DECISION (operator per-venue resume): it carries no qty x price, and it only
            // releases a halt the link dead-man itself set; the rate token in `vet` still applies.
            CommandWire::ResumeVenue { .. } => None,
            // VETTING DECISION (account-scoped trading state): carries no qty x price, and it is the
            // node-wide `SetTradingState`'s narrower twin (risk-REDUCING or a resume, never sized).
            CommandWire::SetAccountTradingState { .. } => None,
            // VETTING DECISION (the give-back of an admitted symbol, decision 0205): carries no qty x price and
            // opens nothing (it frees a slot and writes nothing at the venue); the rate token in `vet` applies.
            CommandWire::UnadmitSymbol { .. } => None,
            // VETTING DECISION (the un-latch of a mount's budget, decision 0239): carries no qty x price and
            // places nothing (it lets a mount that is flat and quiet trade again, and every order the mount
            // then emits passes the core `RiskGate` and its re-armed budget); the rate token in `vet` applies.
            CommandWire::UnlatchMount { .. } => None,
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
            CommandWire::SetSetting { .. } => None,
            // THE BRACKET: three orders of ONE size, and every priced leg is capped as a `Submit`
            // at that price would be — the entry when it is a limit, the take-profit, and the
            // stop-loss at its trigger (a stop `Submit` is sized at its trigger too). A MARKET entry is unpriced like a market `Submit`, but its exits
            // are not. The refusal NAMES the leg, which the shared sentence below cannot: the
            // breaching legs are filtered BEFORE the largest is picked, so a NaN leg (which
            // `n > max` never selects) cannot hide a finite leg that does breach. ⚠ That also
            // means a NON-FINITE bracket is not refused HERE — the finite check after this match
            // is the Submit/Modify arms', and a bracket's own is `check_bracket` (which names the
            // field), run right after this gate by `accept_command` and the Preview.
            //
            // The value is the bracket's engine's: [`bracket_refusal`] admits a bracket only
            // to the ONE engine it names (the bare venue id when it names no account), which is
            // the block read here, by the same account text a `Submit` is sized by.
            CommandWire::Bracket(b) => {
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
            // THE ORDER LIST, the bracket's rule leg by leg: every leg that names a price of its own is capped as a
            // `Submit` at it would be (a limit at its price, a trigger kind at its trigger), and the refusal names
            // the leg. A MARKET leg is unpriced here; its entry is sized at the held price by `market_reason`. A
            // non-finite value is `order_list_refusal`'s, run after this gate, which names the field.
            CommandWire::OrderList(l) => {
                let value = book_value(&l.venue, l.account.as_deref(), &l.symbol, blocks);
                let breach = l
                    .legs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, leg)| {
                        let market = leg.order_type.trim().eq_ignore_ascii_case("market");
                        let own = if market { None } else { leg.price }.or(leg.trigger_price);
                        own.map(|p| (i, value.order_notional(leg.qty, p)))
                    })
                    .filter(|(_, n)| *n > max)
                    .max_by(|x, y| x.1.total_cmp(&y.1));
                if let Some((leg, n)) = breach {
                    let note = value_note(value);
                    return Some(format!(
                        "order notional {n:.2} (the order list's leg {leg}) exceeds the node's \
                         policy ceiling max_notional_per_order {max:.2}{note}"
                    ));
                }
                None
            }
            // THE COMBO, the order list's rule leg by leg: a leg that names its own limit is capped as a `Submit` of
            // `|ratio| × qty` of its instrument at that limit would be, valued as that instrument on the combo's
            // engine, and the refusal names the leg. A combo priced by its NET, or at market, names no price per leg
            // (its signed net is never a size, `vike_model::OrderCombo::net_limit`'s doc), so its legs are sized at the
            // prices this node holds by `market_reason`. A non-finite value is `combo_refusal`'s, run after this gate.
            CommandWire::Combo(c) => {
                let breach = c
                    .legs
                    .iter()
                    .enumerate()
                    .filter_map(|(i, leg)| {
                        let value = book_value(&c.venue, c.account.as_deref(), &leg.symbol, blocks);
                        let qty = c.qty * f64::from(leg.ratio.unsigned_abs());
                        leg.limit.map(|p| (i, value, value.order_notional(qty, p)))
                    })
                    .filter(|(_, _, n)| *n > max)
                    .max_by(|x, y| x.2.total_cmp(&y.2));
                if let Some((leg, value, n)) = breach {
                    let note = value_note(value);
                    return Some(format!(
                        "order notional {n:.2} (the combo's leg {leg}) exceeds the node's \
                         policy ceiling max_notional_per_order {max:.2}{note}"
                    ));
                }
                None
            }
            // NO WILDCARD. A `_ => None` here is how a new order verb would reach this node
            // UNCAPPED without anyone deciding so — it is exactly how a bracket sat unsized while
            // its arm refused it — so a new variant must be classified on this list. These carry
            // nothing to size, and the core `RiskGate` stays the floor.
            CommandWire::Cancel(_)
            | CommandWire::Modify { new_qty: None, .. }
            | CommandWire::MassCancel { .. }
            | CommandWire::Flatten { .. }
            | CommandWire::MarketExit { .. }
            | CommandWire::SetTradingState(_) => None,
        };
        let (qty, price, value) = sized?;
        self.ceiling_reason(qty, price, value)
    }

    /// The ONE comparison with the ceiling: `(qty, price, value)` are the three factors of one order.
    fn ceiling_reason(&self, qty: f64, price: f64, value: ContractValue) -> Option<String> {
        let max = self.max_notional?;
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
        None
    }

    /// **A MARKET order against the ceiling, at the price this node holds for its symbol.**
    /// `policy.max_notional_per_order` reaches no `RiskLimits`, so an order this edge does not size meets
    /// that row nowhere, and a market order's frame carries nothing to size it with. So it is read here:
    /// an opening market `Submit`, a bracket with a MARKET entry and a qty-raising `Modify` of a resting
    /// market order, sized at the price [`SymbolAdmitter::price`] holds (a READ that starts nothing; `waited`
    /// is the price the door just held the order for), through [`priced_for_sizing`], as a live lane twin's
    /// always was. A close is never sized ([`order_closes`]): it must not be held up.
    ///
    /// **No price here invents none.** A committing surface with a source has held the order for its first price
    /// by now, or refused it (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `own_first_price`). With nothing
    /// to read (a Preview, a surface with no source, the symbol of a strategy mount whose feed the tap does not see)
    /// only a number the order itself names can bind it, so a market order that states a price is judged no more
    /// loosely than when this edge sized it at that number.
    pub(crate) fn market_reason(
        &self,
        cmd: &CommandWire,
        blocks: &[vike_execution::VenueBlock],
        orders: &[vike_execution::OrderView],
        foreign: Option<ContractValue>,
        facts: Option<&dyn SymbolAdmitter>,
        waited: Option<f64>,
    ) -> Option<String> {
        self.max_notional?;
        let held = |route_key: &str, venue: &str, symbol: &str| {
            waited
                .or_else(|| facts?.price(route_key, venue, symbol, false).ok().flatten())
                .filter(|p| p.is_finite() && *p > 0.0)
        };
        if let CommandWire::Modify {
            order_client_id,
            new_qty: Some(q),
            new_price: Some(named),
            ..
        } = cmd
        {
            let order = open_market_order(order_client_id, orders)?;
            if modify_closes(order_client_id, *q, orders, blocks) {
                return None;
            }
            let label = order.account.clone().unwrap_or_default();
            let key = vike_model::accounts::account_keys::route_key_of(&order.venue, &label);
            let px = held(&key, &order.venue, &order.symbol).unwrap_or(*named);
            return self.ceiling_reason(*q, px, order_value(order_client_id, orders, blocks));
        }
        // A COMBO priced by its net, or at market, names no price per leg: each leg is sized as a `Submit` of
        // `|ratio| × qty` at the price this node holds for its instrument, the order list's market legs' rule, the
        // first leg over the ceiling named. A leg with no price held is not sized here (no price here invents none;
        // the core's `RiskGate::check_combo` crosses every leg). One priced by its legs was sized by `notional_reason`.
        if let CommandWire::Combo(c) = cmd {
            if c.legs.iter().any(|leg| leg.limit.is_some()) {
                return None;
            }
            let label = match c.account.as_deref() {
                None => vike_model::accounts::account_keys::AccountLabel::Default,
                Some(text) => vike_model::accounts::account_keys::parse_wire_account(text).ok()?,
            };
            let key = vike_model::accounts::account_keys::route_key_of(&c.venue, &label);
            // The source's price per leg, never `waited`: that is the price of the one symbol a plan held for.
            return c.legs.iter().enumerate().find_map(|(i, leg)| {
                let px = facts?
                    .price(&key, &c.venue, &leg.symbol, false)
                    .ok()
                    .flatten()
                    .filter(|p| p.is_finite() && *p > 0.0)?;
                let value = book_value(&c.venue, c.account.as_deref(), &leg.symbol, blocks);
                self.ceiling_reason(c.qty * f64::from(leg.ratio.unsigned_abs()), px, value)
                    .map(|why| format!("the combo's leg {i}: {why}"))
            });
        }
        let (key, venue, symbol) = market_order_to_size(cmd, blocks)?;
        let named = match cmd {
            CommandWire::Submit(req) => req.price,
            _ => None,
        };
        let px = held(&key, venue, symbol).or(named)?;
        self.notional_reason(&priced_for_sizing(cmd, px)?, blocks, orders, foreign)
    }

    /// The DRY-RUN verdict for the Preview verb: the same edge policy [`Self::vet`] enforces, but
    /// WITHOUT consuming a rate token — a preview is read-only and must not drain the command budget,
    /// nor be rate-limited into uselessness. Of THIS limiter's two policies only the deterministic
    /// notional cap is evaluated; the transient rate limiter is deliberately not assessed.
    /// `Some(reason)` ⇒ would refuse; `None` ⇒ would pass. ⚠ It is the first of a preview's steps, not
    /// the whole preview (`crates/vike-tradehub/src/server/preview.rs`'s `preview_verdict`).
    ///
    /// `blocks` and `orders` are [`Self::vet`]'s, and a preview must hand them the SAME roster and
    /// the SAME named order the real command would meet — a preview that sized at 1.0 where the
    /// send sizes with the instrument's multiplier would report as accepted a command that is then
    /// refused.
    pub fn preview_vet(
        &self,
        cmd: &CommandWire,
        blocks: &[vike_execution::VenueBlock],
        orders: &[vike_execution::OrderView],
    ) -> Option<String> {
        self.notional_reason(cmd, blocks, orders, None)
    }

    /// [`Self::preview_vet`] with the foreign symbol's value ([`Self::vet_notional`]'s `foreign`), so
    /// a preview sizes an inverse foreign symbol as the send will.
    pub fn preview_vet_foreign(
        &self,
        cmd: &CommandWire,
        blocks: &[vike_execution::VenueBlock],
        orders: &[vike_execution::OrderView],
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
    blocks: &[vike_execution::VenueBlock],
) -> ContractValue {
    use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};
    // A label that does not parse is `lower_command`'s refusal, not a reason to guess a book here.
    let label = match account.map(parse_wire_account).transpose() {
        Ok(label) => label.unwrap_or(AccountLabel::Default),
        Err(_) => return ContractValue::default(),
    };
    label_value(venue, &label, symbol, blocks)
}

/// The OPEN order `order_client_id` names, when it is a MARKET order: its price is no price
/// ([`ControlLimits::market_reason`]).
fn open_market_order<'a>(
    order_client_id: &str,
    orders: &'a [vike_execution::OrderView],
) -> Option<&'a vike_execution::OrderView> {
    orders
        .iter()
        .find(|o| o.order_client_id == order_client_id && !o.status.is_terminal())
        .filter(|o| o.order_type == OrderType::Market)
}

/// The value of the instrument the OPEN order `order_client_id` rests on — the engine that holds it (its venue
/// and account, as the published order says them) and its symbol, resolved by [`label_value`]
/// exactly as [`book_value`] resolves a `Submit`'s — and **the default value when the published
/// `orders` hold no open order of that id** (`notional_reason`'s doc has the list of what that
/// covers, and why it is the arithmetic a `Modify` always had).
///
/// Open means NOT terminal, the question `vike_execution::CoreSnapshot::open_order` asks of the same
/// rows: a terminal order is one the core ignores a modify of, so it lends this edge nothing.
fn order_value(
    order_client_id: &str,
    orders: &[vike_execution::OrderView],
    blocks: &[vike_execution::VenueBlock],
) -> ContractValue {
    let Some(order) =
        orders.iter().find(|o| o.order_client_id == order_client_id && !o.status.is_terminal())
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
    blocks: &[vike_execution::VenueBlock],
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
/// connection, a `Busy`/refusal does not" rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAcceptError {
    /// Refused at the server edge — the rate limiter, the notional cap, or a command
    /// [`lower_command`] declines to lower (e.g. a submit with no pre-minted order-client-id). The
    /// surface stays open; the caller may send another command.
    Refused(String),
    /// The core's ingest lane was FULL. Transient — retry.
    Busy,
    /// The core's ingest lane is CLOSED (the core is shutting down). Terminal for the surface.
    Gone,
}

impl ControlAcceptError {
    /// The operator-visible text. The `Busy`/`Gone` strings are the verbatim `Response::Error`
    /// bodies the TCP arm sent before this was factored out — do not reword them casually, a
    /// client may key off them.
    pub fn message(&self) -> String {
        match self {
            ControlAcceptError::Refused(msg) => msg.clone(),
            ControlAcceptError::Busy => "core busy, retry".to_string(),
            ControlAcceptError::Gone => "core is shutting down".to_string(),
        }
    }

    /// True when the surface must stop after reporting this (only [`ControlAcceptError::Gone`]).
    pub fn is_fatal(&self) -> bool {
        matches!(self, ControlAcceptError::Gone)
    }
}

impl std::fmt::Display for ControlAcceptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// What [`accept_command`] accepted — the reply-shaping half of its verdict. Almost every command
/// lowers into the core and echoes an order client id ([`vike_tradehub_client::proto::Response::Ack`]); the ONE exception is the REQ-7
/// settings write, which lands on DISK at the daemon edge (nothing enters the core, there is no
/// order client id) and whose caller instead needs `restart_required`
/// ([`vike_tradehub_client::proto::Response::SettingsWritten`]). An enum rather than a stringly convention so a surface cannot
/// accidentally ack a settings write as an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlAcceptReply {
    /// The command entered the core's single-writer lane; the order client id to echo (empty for the
    /// account-wide verbs, exactly as before), and the door's NOTES on it: plain-words sentences about a
    /// command that WAS sent (an order placed into a closed market, `crates/vike-tradehub/src/server/session_note.rs`'s `note_for`;
    /// a flatten whose engines show nothing to close, `nothing_to_flatten_note` below).
    /// Empty for every other command, and for one with nothing to say.
    OrderClientId {
        /// The minted/echoed order-client-id.
        order_client_id: String,
        /// What the door says about the command it sent; never a reason to refuse.
        notes: Vec<String>,
        /// The kind word of an order the node EMULATES (`emulation_note`), whose one sentence is among `notes`;
        /// `None` for every other command. `Response::Ack`'s `emulated`.
        emulated: Option<&'static str>,
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

impl ControlAcceptReply {
    /// The echoed order client id alone, or the account-wide empty string for a settings write. ⚠ It drops the
    /// notes, so a surface that answers an operator reads [`Self::into_order_client_id_and_notes`].
    pub fn into_order_client_id(self) -> String {
        self.into_order_client_id_and_notes().0
    }

    /// The order client id and the door's notes (the empty string and none for a settings write).
    pub fn into_order_client_id_and_notes(self) -> (String, Vec<String>) {
        match self {
            ControlAcceptReply::OrderClientId { order_client_id, notes, .. } => {
                (order_client_id, notes)
            }
            ControlAcceptReply::SettingsWritten { .. } => (String::new(), Vec::new()),
        }
    }
}

/// **The ONE acceptance path.** Every remote control command — the TCP
/// [`vike_tradehub_client::proto::Request::Command`] arm's, the Telegram channel's (`vike-telegram`,
/// a client of that wire) included — funnels through here.
///
/// In order:
/// 1. [`ControlLimits::vet`] — consumes a rate token, then ([`submit_side_refusal`]) an order must be a buy or a
///    sell, then ([`one_account_addressed`], decision 0254) a
///    venue-scoped command that names no account must reach exactly one account, then the per-order notional
///    cap. Refusal short-circuits: NOTHING is lowered and nothing is audited.
///    Then [`venue_refusal`] — the ROUTING gate: a command naming a venue this node runs no engine
///    for is REFUSED rather than falling through to the primary engine. Inside this step, AFTER the
///    rate token (so mis-addressed frames cannot be flooded past the bucket) and before everything
///    below. Then [`account_refusal`] (the BOOK within that venue) and, for a bracket,
///    [`bracket_refusal`] (its values, and what the one engine it reaches can hold) and, for a plain
///    submit, [`plan_submit_symbol`] (a symbol its engine does not trade: admitted first when the
///    venue's adapter signs on the order's own symbol, refused when it signs the mounted one) and,
///    right after that plan, a post-only limit order whose price would cross the best bid or ask the
///    node's price tap holds ([`post_only_cross_refusal`]; no top of book held, no refusal).
/// 2. [`audit::sanitize_reason`] — the rationale is remote free text landing in a structured JSON
///    log line, so it is stripped/capped HERE, once, for every surface.
/// 3. [`command_kind`] — read off the wire command BEFORE [`lower_command`] consumes it.
/// 4. [`lower_command`] — into the core's real `Command`/`OrderIntent`.
/// 5. `CommandSink::try_command` — the non-blocking single-writer lane.
/// 6. [`audit::record`] — ONLY on acceptance, exactly as before.
///
/// **The ONE branch: `CommandWire::SetSetting` (REQ-7) replaces steps 4–6** — after the SAME rate
/// token and the SAME sanitizer, it lowers into the settings DATABASE instead of into the core
/// ([`SettingsShowSource::apply_set_setting`]: the loader-validated one-row write, no confirm
/// ceremony — 0086 point 7) and its audit record is [`audit::record_settings_write`], which carries
/// the old→new values. `settings` is that boot-threaded source — `None` (a server started without
/// one) refuses the verb; every other command ignores the parameter entirely.
///
/// The `reason` never reaches [`lower_command`], so it can never touch `OrderRequest`, the core
/// fold, the journal, or a venue. `peer` is the TCP peer, `None` when the socket cannot name it.
///
/// `key_id` is the caller's authenticated identity — `vike_hub_protocol::auth::HubKeys`'
/// `key_id`, the stable non-secret fingerprint of the key whose mac verified, resolved once per
/// connection in `handle_connection`. It is the ACTOR the change journal records, because this
/// daemon authenticates a key and never a person. `None` for a caller that authenticated no key
/// — recorded as an ABSENT field rather than as an invented one. ⚠ It is an `Option<&str>` next to
/// an `Option<SocketAddr>`, so the two
/// cannot be transposed at a call site the way four adjacent `&str`s could (the hazard
/// [`audit::SettingsWriteAudit`] exists for).
///
/// `engines` is the venues this node runs an engine for
/// ([`crate::publish::PublisherHandle::engine_venues`]), against which the command's addressed
/// venue is checked by [`venue_refusal`] — step 1b, immediately after the rate token so a peer
/// cannot spam mis-addressed commands for free, and before ANYTHING is lowered, audited or sent.
/// An EMPTY slice means "this node cannot answer that question" and refuses nothing (see
/// [`venue_refusal`] for why that is a deadlock-avoidance rule rather than a soft default); a
/// caller with no engine roster at all passes `&[]` and is byte-identical to before this gate
/// existed.
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
/// ([`crate::publish::PublisherHandle::orders_named_by`]), read by THREE gates: step 1c', which refuses
/// a `Submit` whose order client id an order already carries (`order_client_id_reuse_refusal`; ANY status), the
/// notional ceiling of step 1, which sizes a `Modify` with the contract multiplier of the
/// instrument that (OPEN) order rests on (`ControlLimits::vet`; an order the snapshot does not
/// hold sizes at 1.0, as a `Modify` always did), and step 1d', which refuses the give-back of a symbol
/// with an order in it (`plan_unadmit`; every order of the symbol that has not ended, of any account: it
/// keeps the addressed account's by route key). Every other verb ignores it, and a surface that
/// passes `&[]` is byte-identical to the node before the edge could resolve one.
///
/// `recon` is the HELD reconcile divergence a `ReconcileConfirm` names, off the same published snapshot
/// ([`crate::publish::PublisherHandle::reconcile_alerts_named_by`]), read by TWO consumers: the audit
/// record, which names the divergence's account, kind and description beside the id
/// ([`audit::record_reconcile_confirm`]), and step 1e', which admits the symbol a row books when the
/// account's engine does not trade it ([`plan_confirm_admission`]) and refuses the confirm in words when
/// it cannot be admitted. Every other verb passes it empty and ignores it; an id the snapshot does not hold
/// is still accepted and refused by the core.
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
/// `Ok` is the [`ControlAcceptReply`] verdict the surface shapes its reply from ([`ControlAcceptReply::OrderClientId`] ⇒
/// `Ack`, [`ControlAcceptReply::SettingsWritten`] ⇒ `SettingsWritten`).
#[expect(clippy::too_many_arguments)] // see `serve_with_link_policy`'s note — each engine roster is
// one more gate INPUT on a function whose parameter list is the whole acceptance surface, and each
// one is documented at its own name above.
pub fn accept_command(
    cmd: CommandWire,
    reason: Option<&str>,
    limits: &mut ControlLimits,
    sink: &CommandSink,
    settings: Option<&SettingsShowSource>,
    engines: &[String],
    route_keys: &[String],
    not_running: &[vike_execution::ArmingNotRunningView],
    blocks: &[vike_execution::VenueBlock],
    orders: &[vike_execution::OrderView],
    recon: &[vike_execution::ReconcileAlertView],
    mounts: &[vike_execution::MountView],
    facts: Option<&dyn SymbolAdmitter>,
    peer: Option<std::net::SocketAddr>,
    key_id: Option<&str>,
) -> Result<ControlAcceptReply, ControlAcceptError> {
    if let Some(refusal) = limits.vet_rate() {
        return Err(ControlAcceptError::Refused(refusal));
    }
    if let Some(refusal) = submit_side_refusal(&cmd) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // A venue-scoped command that names no account reaches exactly one account or does not go (decision 0254).
    // Ahead of `lane_addressed`, which writes `DEFAULT` onto an account-less order and would hide the omission.
    let cmd = one_account_addressed(cmd, route_keys, not_running, blocks, facts)
        .map_err(ControlAcceptError::Refused)?;
    // A venue whose one account runs two engines (a lane twin is up) has the order put on the engine of
    // its lane before any gate reads the address ([`lane_addressed`]); every other command passes as it is.
    let mut cmd = lane_addressed(cmd, blocks, facts);
    // A Polymarket market NAME goes on its token id before any gate asks whether the order closes.
    outcome_name_resolved(&mut cmd, blocks, facts).map_err(ControlAcceptError::Refused)?;
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
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1a. AN ACCOUNT THAT DOES NOT RUN (decision 0156), FIRST among the routing gates so the
    // refusal names its REASON rather than the venue's or the account's bare absence — and so an
    // account-less order for a venue whose default account does not run is answered before the
    // `Ack` instead of by the core after it. `not_running_refusal`'s doc carries the classification.
    if let Some(refusal) = not_running_refusal(&cmd, not_running) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1a'. AN ACCOUNT STARTED IN THIS RUNNING NODE THAT HAS NOT YET BEEN GIVEN ITS VENUE BALANCE (decision
    // 0169): an OPENING command is held in words until the first reconcile pass lands the balance, because
    // until then its engine judges on the seed cash. Right behind the not-running gate (the account
    // runs, but is not yet orderable) and ahead of everything that could cost a lookup or a lowering.
    if let Some(refusal) = awaiting_balance_refusal(&cmd, blocks, facts) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1a''. A LANE TWIN KEPT UP ONLY TO BE CLOSED (its row is gone, it still holds something): it takes no new
    // position. Reducing commands pass; `lane_twin_closing_refusal`'s doc carries the classification.
    if let Some(refusal) = lane_twin_closing_refusal(&cmd, orders, blocks, facts) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1b. THE ROUTING GATE. Deliberately here — after the rate token (so a peer cannot flood
    // mis-addressed frames past the bucket) and before the sanitizer, the lowering, the core and
    // the audit record: a command that names a book this node does not have is not a command this
    // node may reinterpret, so nothing downstream should ever see it. A refusal is audited nowhere,
    // which is this daemon's uniform rule for every refused command (see the `SetSetting` arm's
    // declared note below).
    if let Some(refusal) = venue_refusal(&cmd, engines) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1c. THE ACCOUNT GATE, immediately after the venue one and deliberately in that order: a venue
    // this node does not run is refused in the VENUE's words above, so what reaches here is the
    // narrower fault — the exchange is mounted and the named BOOK is not. Before this the node
    // Acked such a frame and the core refused it out of band, so the client printed `accepted` over
    // an order that never existed; the whole of the change is that the node answers first. Same
    // audit rule as every refusal on this path: none.
    if let Some(refusal) = account_refusal(&cmd, route_keys) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1c'. A SUBMIT WHOSE ORDER CLIENT ID AN ORDER ALREADY CARRIES, said in words before the Ack. Past this
    // door the core registers the new order over the old one and the gatewayhub does not send an order client id it has
    // seen, so the command would be Acked and then die silently. `order_client_id_reuse_refusal`'s doc has the case, and
    // the second: an id (a list leg's, a combo's, a bracket stem's legs') used before this node's last restart.
    if let Some(refusal) =
        order_client_id_reuse_refusal(&cmd, orders, limits.boot_order_client_ids())
    {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1c'. A modify's TRIGGER on an order that has none, or any price on a trailing order, would move nothing
    // behind an `Ack` (`modify_price_refusal`); the preview says it in the same place (`preview.rs`'s `door_refusal`).
    if let Some(refusal) = modify_price_refusal(&cmd, orders) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1c''. A release's price condition on a symbol the order's engine does not trade would hold the order
    // forever behind an `Ack` (`release_symbol_refusal`); the preview says it in the same place.
    if let Some(refusal) = release_symbol_refusal(&cmd, blocks) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // ...and one read on ANOTHER venue (decision 0267's 2.13), off an engine of that venue: none trades the symbol and
    // it is refused (`release_venue_refusal`); the engine that does is asked for the price below, once the order lowers.
    let price_venue = release_venue_refusal(&cmd, blocks).map_err(ControlAcceptError::Refused)?;
    // 1d. THE BRACKET'S OWN VERDICT, immediately after the account gate has established which ONE
    // engine a bracket reaches: its values, then what that engine can hold (does it trade the
    // bracket's symbol, and is its own lane one that can hold a stop-loss). It is the SAME call
    // `preview.rs`'s `preview_verdict` makes, rather than a second composition of the same checks, so a
    // Preview and a Command cannot answer one frame differently. `lower_command`
    // checks the values again for a caller that lowers a frame without this gate; the engine half
    // it cannot check, because it holds no roster. Same audit rule as every refusal here: none.
    if let Some(refusal) = bracket_refusal(&cmd, blocks) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1d. ...and an ORDER LIST's, by the same shape: its values, then what its one engine can hold. The SAME call the
    // Preview makes (`preview.rs`'s `door_refusal`).
    if let Some(refusal) = super::order_list::order_list_refusal(&cmd, blocks) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1d. ...and a COMBO's values, the same call the Preview makes (`combo_refusal`'s doc says why it reads no engine).
    if let Some(refusal) = super::refusal::combo_refusal(&cmd) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // 1d'. THE GIVE-BACK OF AN ADMITTED SYMBOL (decision 0205), judged against the same published block: the symbol
    // must be one the engine admitted, and the engine must be flat in it with no live order. The statement carries
    // the block's spelling of the symbol (`plan_unadmit`'s doc); it is advice, the engine is the authority.
    match plan_unadmit(&cmd, blocks, orders) {
        UnadmitPlan::Pass => {}
        UnadmitPlan::Refuse(why) => return Err(ControlAcceptError::Refused(why)),
        UnadmitPlan::Release { symbol: engine_spelling, .. } => {
            if let CommandWire::UnadmitSymbol { symbol, .. } = &mut cmd {
                *symbol = engine_spelling;
            }
        }
    }
    // 1d''. THE UN-LATCH OF A MOUNT'S BUDGET (decision 0239), judged against the published mount rows: the mount must
    // be one the node runs, latched, and flat. The command carries the published spelling of its id; it is advice,
    // the core is the authority (it also sees the mount's live orders and armed conditionals).
    match plan_unlatch(&cmd, mounts) {
        UnlatchPlan::Pass => {}
        UnlatchPlan::Refuse(why) => return Err(ControlAcceptError::Refused(why)),
        UnlatchPlan::Send { mount_id: published } => {
            if let CommandWire::UnlatchMount { mount_id } = &mut cmd {
                *mount_id = published;
            }
        }
    }
    // 1e. A PLAIN SUBMIT's symbol, against the same engine block: the plain-order twin of 1d. A
    // symbol the venue's adapter can sign comes back canonical (the venue's spelling) with its
    // admission; the admission is sent below, ahead of the order.
    //
    // The order is lowered ONCE here first, only to learn whether it can be: the plan below may open a
    // price subscription, and an order that cannot be lowered (an empty order client id, an unknown
    // order type) must not have cost one.
    if matches!(cmd, CommandWire::Submit(_)) {
        lower_command(cmd.clone()).map_err(ControlAcceptError::Refused)?;
    }
    // 1c''' (here, once the order lowers: a frame that cannot be lowered must not have cost a price lane). The
    // price venue's engine is asked ONCE, committing: it opens the price lane, as an admission's first
    // committing ask does, and an `Err` is "no price can come", refused in its own words before the `Ack`. With no
    // facts source no price can come either. The pump goes back to the idle reaper when this acceptance ends, refused
    // or not: the lane keeper holds it from the first published HELD list on (`crate::readmit::release_price_holds`).
    let mut _price_venue_guard = None;
    if let Some(engine) = price_venue {
        let Some(source) = facts else {
            return Err(ControlAcceptError::Refused(format!(
                "submit: `release` — this node has no price source, so it never reads {} on {}: REFUSED \
                 rather than held on a price nobody reads",
                engine.symbol, engine.venue
            )));
        };
        let asked = source.price(&engine.route_key, &engine.venue, &engine.symbol, true);
        _price_venue_guard =
            Some(PriceGuard { source, route_key: engine.route_key, symbol: engine.symbol });
        if let Err(why) = asked {
            return Err(ControlAcceptError::Refused(format!("submit: `release` — {why}")));
        }
    }
    // The symbol the ceiling could not size (step 1) is refused now, with the door's words: the committing
    // plan below is asked again and a facts source that changed its answer meanwhile must not let an
    // unsized order through.
    let foreign = match foreign {
        Ok(value) => value,
        Err(why) => return Err(ControlAcceptError::Refused(why)),
    };
    let mut admit = None;
    // What an order held for its own symbol's first price leaves the engine (`own_first_mark`).
    let mut own_mark = None;
    // Whatever the plan opened is given back on EVERY way out that does not admit the symbol.
    let mut _price_guard = None;
    let mut waited_price = None;
    match plan_submit_symbol(&cmd, blocks, facts, true) {
        SymbolPlan::Pass => {}
        SymbolPlan::Refuse(refusal) => return Err(ControlAcceptError::Refused(refusal)),
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
            if let (Some(wait), Some(source)) = (&wait, facts) {
                let held = std::time::Instant::now();
                let px = await_price(source, wait, &symbol).map_err(|why| {
                    // ⚠ The reply may go to a client that has stopped listening: a client whose
                    // `config.node_ack_wait_ms` row is shorter than this hold (`PRICE_WAIT`) hangs up
                    // INSIDE it (the default outlasts it now; it used to be 2 s against 4 s), and
                    // a refusal is audited nowhere. v0.1.81's IG drill left no row, no audit line and no
                    // log line; this line is the one place that refusal can still be read.
                    let order_client_id = match &cmd {
                        CommandWire::Submit(req) => req.order_client_id.as_str(),
                        _ => "",
                    };
                    tracing::warn!(
                        order_client_id,
                        route_key = %wait.route_key,
                        venue = %wait.venue,
                        %symbol,
                        held_ms = u64::try_from(held.elapsed().as_millis()).unwrap_or(u64::MAX),
                        "vike-tradehub control: an order was held for a price and REFUSED at the door \
                         (its client may have stopped listening before this reply): {why}"
                    );
                    ControlAcceptError::Refused(why)
                })?;
                waited_price = Some(px);
                // ...and the time it was made, which the engine's first mark keeps.
                let made = source.price_made_ms(&wait.venue, &symbol, px);
                // A symbol the engine already trades (a restored one) has no admission riding this order:
                // the cached facts carry the price instead, so its first mark does not race the tick.
                // The engine's own symbol takes the mark alone.
                if admission.is_none() {
                    own_mark = own_first_mark(wait, &symbol, px, made, blocks, facts);
                    if own_mark.is_none() {
                        admission = source
                            .admission(&wait.route_key, &wait.venue, &symbol)
                            .ok()
                            .map(Box::new);
                    }
                }
                if let Some(a) = admission.as_mut() {
                    a.initial_mark = Some(px);
                    a.initial_mark_ms = made;
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
                let (verb, what) = match cmd {
                    CommandWire::Bracket(_) => ("bracket", "bracket"),
                    CommandWire::OrderList(_) => ("order list", "order list"),
                    _ => ("submit", "order"),
                };
                let px = waited_price.or_else(|| {
                    facts.and_then(|source| {
                        source.price(&route_key, venue, &own, false).ok().flatten()
                    })
                });
                let Some(px) = px.filter(|p| p.is_finite() && *p > 0.0) else {
                    return Err(ControlAcceptError::Refused(format!(
                        "{verb}: the perp lane `{route_key}` has no price for `{own}` to size this \
                         {what} against the node's notional ceiling. The {what} was REFUSED: retry in a \
                         moment"
                    )));
                };
                if let Some(sized) = priced_for_sizing(&cmd, px)
                    && let Some(refusal) = limits.vet_notional(&sized, blocks, orders, None)
                {
                    return Err(ControlAcceptError::Refused(refusal));
                }
            }
            if let CommandWire::Submit(req) = &mut cmd {
                req.symbol = symbol;
            }
            admit = admission;
        }
    }
    // 1e'. A RECON CONFIRM whose held row books a symbol the account's engine does not trade (a polymarket wallet
    // token, 2026-11-10) has the symbol admitted first, planned as the opening order of the row's size would be: a
    // LOOK, nothing committed to the gatewayhub, so nothing is written at the venue. A refusal sends nothing.
    let reconcile_admit = match &cmd {
        CommandWire::ReconcileConfirm { id } => match recon.iter().find(|r| r.id == *id) {
            Some(row) => {
                plan_confirm_admission(row, blocks, facts).map_err(ControlAcceptError::Refused)?
            }
            None => None,
        },
        _ => None,
    };
    // 1f. A MARKET order against the ceiling, at the price this node holds: after the plan, so the symbol is the
    // canonical spelling the price is kept under and a first price the plan held the order for counts.
    if let Some(refusal) = limits.market_reason(&cmd, blocks, orders, foreign, facts, waited_price)
    {
        return Err(ControlAcceptError::Refused(refusal));
    }
    let audit_reason = audit::sanitize_reason(reason);
    let kind = command_kind(&cmd);
    if let CommandWire::SetSetting { file: _, key, value, confirm: _ } = cmd {
        // The REQ-7 settings write: same gate order as every command (the rate token above, the
        // sanitizer above), then the DATABASE write instead of the core one. NOTE the notional cap
        // is n/a by `notional_reason`'s own vetting decision at its arm. `file` and `confirm` are
        // not consulted (0086 point 6/7 — see `apply_set_setting`'s own doc): a row is named by
        // its `key`. They may arrive absent since step 1 of their retirement (`#[serde(default)]`
        // on the wire type) and are deleted from the wire in step 2.
        let src = settings.ok_or_else(|| {
            ControlAcceptError::Refused(
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
            src.apply_set_setting(&key, &value).map_err(ControlAcceptError::Refused)?;
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
                vike_change_journal::ChangeOutcome::AppliedPendingRestart
            } else {
                vike_change_journal::ChangeOutcome::Applied
            },
        });
        return Ok(ControlAcceptReply::SettingsWritten { restart_required });
    }
    // **A node that is not ACTIVE changes nothing on the exchange** (review r2, item 9). Admitting a symbol
    // commits to the gatewayhub, which on a venue that posts a leverage (bybit, okx, binance) makes the exchange
    // hold the account's figure for the symbol: a WRITE on the venue account that neither the HALT sentinel nor the
    // kill switch used to stop, and that nothing undoes. So while the addressed engine is `Halted` (no new
    // orders) or `Reducing` (the daemon cannot tell here whether the write would RAISE the leverage, so any
    // might), an OPENING order whose admission would commit is refused before anything is sent; a reducing order
    // is let through WITHOUT the admission (the engine already trades the symbol), so a close is never held up.
    let not_active = admit.as_deref().and_then(|a| admission_refusal_while_not_active(a, blocks));
    if let Some(refusal) = not_active {
        if matches!(&cmd, CommandWire::Submit(req) if order_closes(req, blocks)) {
            admit = None;
        } else {
            return Err(ControlAcceptError::Refused(refusal));
        }
    }
    // **A POST-ONLY LIMIT ORDER THAT WOULD CROSS THE BOOK THIS NODE HOLDS IS REFUSED BY NAME** (decision 0242, item
    // 8), with its price and the best bid or ask it meets, before anything is sent: a venue that accepts such an
    // order and cancels it (bybit, okx) used to leave a plain `CANCELED`. After the symbol plan on purpose: the
    // order's symbol is now the canonical spelling the price tap is keyed by, and a symbol the door just
    // subscribed has had its hold for a first price. No quote the tap holds, or only a mid, answers `None`.
    if let Some(refusal) = post_only_cross_refusal(&cmd, None, blocks, facts) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // An opening order the venue's grid refuses, on a symbol the engine already trades, judged from the grid this
    // node holds: said by name here instead of a denial the engine could only state after the Ack.
    if let Some(refusal) = held_grid_refusal(&cmd, None, orders, blocks, facts) {
        return Err(ControlAcceptError::Refused(refusal));
    }
    // Read before the command is lowered (moved): whether it is an OPENING order, for the live twin's rule below.
    // A bracket always opens (its entry does), so it is held to the same rule on an admitted symbol.
    let opens = match &cmd {
        CommandWire::Submit(req) => !order_closes(req, blocks),
        CommandWire::Bracket(_) => true,
        CommandWire::OrderList(l) => super::order_list::order_list_opens(l),
        _ => false,
    };
    // **The closed-market NOTE** (decision 0236): an order whose market is closed now is still sent, and the
    // answer says so. Read off the command as it will go (its symbol is the door's canonical spelling by
    // now), after every refusal above has passed, and said once the lane takes it, below. Never a refusal.
    let mut notes: Vec<String> =
        session_note::closed_market_note(&cmd, (limits.now_ms)(), limits.session_rows)
            .into_iter()
            .chain(rounding_note(&cmd, None, orders, blocks, facts))
            .collect();
    notes.extend(nothing_to_flatten_note(&cmd, blocks, route_keys));
    let (lowered, order_client_id) = lower_command(cmd).map_err(ControlAcceptError::Refused)?;
    // ...and the EMULATED note (decision 0275), read off the order as it will reach the core.
    let emulated = emulation_note(&lowered, blocks).map(|(word, note)| {
        notes.push(note);
        word
    });
    if let Some(mark) = own_mark
        && let Err(rejected) = sink.try_command(Command::AdmitSymbol(Box::new(mark)))
    {
        return Err(match rejected {
            CommandRejected::Busy => ControlAcceptError::Busy,
            CommandRejected::Gone => ControlAcceptError::Gone,
        });
    }
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
                    CommandRejected::Busy => ControlAcceptError::Busy,
                    CommandRejected::Gone => ControlAcceptError::Gone,
                });
            }
        }
        // **The symbol's own margin rate** (decision 0182's per-symbol twin). `admitted` just committed the
        // admission to the daemon, which on a venue that posts a leverage made the venue hold the
        // account's figure for the symbol and read it back; a figure that came back is restated as a
        // SECOND `AdmitSymbol` (a refresh: the engine writes the symbol's `initial_margin_by_symbol` row), on the lane
        // the order is about to ride, so the order is judged at the leverage the venue holds. Nothing
        // confirmed, nothing sent: the symbol stays at the engine's own rate, as before.
        if let (Some(source), Some(first)) = (facts, first.as_deref())
            && let Some(second) = with_confirmed_leverage(source, first)
        {
            tracing::info!(
                %route_key, %symbol, initial_margin_requirement = ?second.initial_margin_requirement,
                "restating the admission with the symbol's own margin rate (the venue holds the \
                 account's leverage for it)"
            );
            if let Err(rejected) = sink.try_command(Command::AdmitSymbol(Box::new(second))) {
                return Err(match rejected {
                    CommandRejected::Busy => ControlAcceptError::Busy,
                    CommandRejected::Gone => ControlAcceptError::Gone,
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
            return Err(ControlAcceptError::Refused(unconfirmed_leverage_refusal(
                &route_key, &symbol,
            )));
        }
    }
    // The confirm's admission (1e') rides the confirm's own lane, so the fold admits the symbol before it confirms.
    // It is the engine's alone: `SymbolAdmitter::admitted` is NOT called, so the gatewayhub is told nothing.
    if let Some(admission) = reconcile_admit {
        tracing::info!(
            route_key = %admission.route_key,
            symbol = %admission.symbol,
            "admitting a symbol into the account's engine for a reconcile confirm (nothing told to \
             the gatewayhub, nothing written at the venue)"
        );
        if let Err(rejected) = sink.try_command(Command::AdmitSymbol(admission)) {
            return Err(match rejected {
                CommandRejected::Busy => ControlAcceptError::Busy,
                CommandRejected::Gone => ControlAcceptError::Gone,
            });
        }
    }
    let confirmed = if let Command::ConfirmReconcile(id) = &lowered { Some(*id) } else { None };
    // The held row a `recon cancel` names, for the same audit reason: the generic line would not say
    // WHICH order was sent a cancel.
    let reconcile_cancelled =
        if let Command::CancelReconcileOrder(id) = &lowered { Some(*id) } else { None };
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
                // for it when it has (`TradeLeaseRegistry::settle_releases`).
                if let Some(source) = facts {
                    let venue = route_key.split('#').next().unwrap_or_default();
                    source.release_requested(route_key, venue, symbol);
                }
            } else {
                match (confirmed, &resumed) {
                    (Some(id), _) => audit::record_reconcile_confirm(
                        peer,
                        id,
                        recon.iter().find(|a| a.id == id),
                        audit_reason.as_deref(),
                    ),
                    (None, Some((venue, force))) => {
                        audit::record_resume_venue(peer, venue, *force, audit_reason.as_deref())
                    }
                    (None, None) => match (&account_set, reconcile_cancelled) {
                        (Some((route_key, state)), _) => audit::record_set_account_state(
                            peer,
                            route_key,
                            *state,
                            audit_reason.as_deref(),
                        ),
                        (None, Some(id)) => audit::record_reconcile_cancel(
                            peer,
                            id,
                            recon.iter().find(|a| a.id == id),
                            audit_reason.as_deref(),
                        ),
                        (None, None) => {
                            audit::record(peer, kind, &order_client_id, audit_reason.as_deref())
                        }
                    },
                }
            }
            // The log carries every note too, for whoever reads the node and not the answer.
            for note in &notes {
                tracing::info!(%order_client_id, "control door note: {note}");
            }
            Ok(ControlAcceptReply::OrderClientId { order_client_id, notes, emulated })
        }
        Err(CommandRejected::Busy) => Err(ControlAcceptError::Busy),
        Err(CommandRejected::Gone) => Err(ControlAcceptError::Gone),
    }
}

/// **The EMULATED note** (decision 0275, in decision 0236's shape: one note on an ACCEPTED order, never a refusal):
/// a submit whose kind the addressed engine's venue does not hold is worked by the node, and the answer says so
/// with the route's word (`chase`, `iceberg`) and its one sentence (`vike_venues::OrderEmulationKind::note`). Judged as the
/// core judges it: `vike_venues::emulation_of` over the published engine's venue and tier, and only when the
/// order's child passes the row, since the core refuses the order otherwise and a refusal owes no note. `None` for
/// every other command, and on a surface that publishes no engine for the order's account.
fn emulation_note(
    lowered: &Command,
    blocks: &[vike_execution::VenueBlock],
) -> Option<(&'static str, String)> {
    let Command::Order(OrderIntent::Submit(req)) = lowered else { return None };
    let key = req.account.as_ref().map_or_else(
        || req.venue.clone(),
        |label| vike_model::accounts::account_keys::route_key_of(&req.venue, label),
    );
    let engine = blocks.iter().find(|b| b.route_key == key)?;
    let route = vike_venues::emulation_of(req, &engine.venue, engine.tier)?;
    vike_venues::preflight_order_at(&vike_venues::emulated_child(req), &engine.venue, engine.tier)
        .ok()?;
    Some((route.as_word(), route.note(&engine.venue)))
}

/// **The note a `Flatten`, or a `MarketExit` that names a book, owes when the engines it reaches show nothing to
/// close**, or `None`.
///
/// The core reads the position when it applies the command and mints nothing for a flat one, after the
/// `Ack` and in silence, so `flatten binance/DEFAULT BTCUSDT.P` and `close-all binance/DEFAULT` with the lane twin up
/// (the position is the twin's) and a mistyped symbol all read as a close that worked. A `MarketExit` that names no
/// book says nothing: the bare venue reaches both lanes, and the node-wide one every book.
/// The door's published blocks can say so first. Advice and never a refusal: they lag a fill that has just
/// landed, and the core's own read decides. An address the blocks do not cover says nothing (an empty
/// roster is the door's "cannot judge"; a label nobody can parse is `lower_command`'s refusal).
///
/// `route_keys` is who RUNS: a stopped account keeps its block for what it holds, and the core's fan-out skips it, so
/// its position is no reason to stay quiet (an empty roster is "cannot judge" here too).
fn nothing_to_flatten_note(
    cmd: &CommandWire,
    blocks: &[vike_execution::VenueBlock],
    route_keys: &[String],
) -> Option<String> {
    let (venue, symbol, account) = match cmd {
        CommandWire::Flatten { venue, symbol, account } => (venue, Some(symbol), account),
        CommandWire::MarketExit { venue: Some(venue), account: account @ Some(_) } => {
            (venue, None, account)
        }
        _ => return None,
    };
    // A named account is that one engine; no account fans out over every engine of the venue.
    let named = match account.as_deref() {
        None => None,
        Some(text) => {
            let label = vike_model::accounts::account_keys::parse_wire_account(text).ok()?;
            Some(vike_model::accounts::account_keys::route_key_of(venue, &label))
        }
    };
    let holds = |b: &vike_execution::VenueBlock| {
        b.positions.iter().any(|p| symbol.is_none_or(|s| p.symbol == *s) && p.size.abs() > 1e-12)
    };
    let runs =
        |b: &vike_execution::VenueBlock| route_keys.is_empty() || route_keys.contains(&b.route_key);
    let of_venue = || blocks.iter().filter(|b| b.venue == *venue);
    let reached: Vec<&vike_execution::VenueBlock> = of_venue()
        .filter(|b| runs(b) && named.as_deref().is_none_or(|key| b.route_key == key))
        .collect();
    if reached.is_empty() || reached.iter().any(|b| holds(b)) {
        return None;
    }
    let books: Vec<&str> = reached.iter().map(|b| b.route_key.as_str()).collect();
    let hint = match of_venue().find(|b| holds(b)) {
        Some(b) if runs(b) => format!(
            "`{}` holds it: name that book, `{}`",
            b.route_key,
            vike_tradehub_client::wire::book_of_route_key(&b.venue, &b.route_key)
        ),
        Some(b) => format!(
            "`{}` holds it and that account is stopped, so no flatten reaches it until it runs again",
            b.route_key
        ),
        None => "Check the symbol against `trade position ls`".to_string(),
    };
    let books = books.join(", ");
    Some(match symbol {
        Some(symbol) => format!(
            "{symbol}: the node's published state shows no position on {books}, so this flatten closes nothing \
             unless a fill has just landed. {hint}"
        ),
        None => format!(
            "the node's published state shows no position on {books}, so this exit cancels that book's orders and \
             closes nothing unless a fill has just landed. {hint}"
        ),
    })
}

/// A mount the send would refuse, in the words `lower_command` refuses it with, so a preview cannot pass what the send
/// refuses: its account, its parameter table, the profile's refusals and its file fence (which asks the filesystem
/// and opens nothing). `None` for every other command.
pub(super) fn mount_refusal(cmd: &CommandWire) -> Option<String> {
    let CommandWire::MountStrategy { file, .. } = cmd else { return None };
    if let Err(why) = mount_spec(cmd.clone()) {
        return Some(why);
    }
    crate::mount_factory::wire_file_refusal(file.as_deref()?)
        .map(|why| format!("mount_strategy: {why}"))
}

/// The [`MountSpec`] a `MountStrategy` frame states, judged by what reads nothing: the half of a mount's lowering a
/// preview shares with [`lower_command`], whose read of a script keeps what it read for the fold and so is the send's.
fn mount_spec(frame: CommandWire) -> Result<MountSpec, String> {
    let CommandWire::MountStrategy {
        venue,
        account,
        symbol,
        interval,
        controller_id,
        file,
        name,
        rhai,
        params,
    } = frame
    else {
        return Err("mount_strategy: this frame is no mount".to_string());
    };
    // ⚠ **THE WIRE CAN NAME ONE NOW, AND THE ARGUMENT THAT SAID IT MUST NOT IS ANSWERED
    // RATHER THAN DROPPED.** This arm hard-coded `account: None` and said why: *"a remote
    // peer mounting onto a second account would be naming a book the operator never armed
    // for that channel"*, with `ProfileDaemon::account` — a file the operator edits — as
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
    // `ProfileDaemon::account` is untouched as the file-shaped door.
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
    // The strategy code is the SEND's alone (`lower_command`, which reads the store); a preview writes nothing.
    let spec = MountSpec {
        venue,
        symbol,
        interval,
        account,
        controller_id,
        file,
        name,
        rhai,
        params,
        order_client_id_code: None,
    };
    crate::mount_factory::validate_spec(&spec).map_err(|e| format!("mount_strategy: {e}"))?;
    Ok(spec)
}

/// A wire release as the model's, its price condition on the symbol the release names, else on `order_symbol`
/// (the order's own), read on the venue it names, else on `order_venue` (the order's own), or why it cannot be one:
/// a level at or below zero (JSON carries no non-finite number), a blank symbol, a symbol or a venue with no
/// condition to read it, a venue that is no roster id (`vike_model::price_source_refusal`), and another venue with
/// no symbol (the venues spell one instrument differently). A condition naming the ORDER's own venue is written
/// `venue: None`, today's stored form. Whether an engine trades that symbol needs the roster:
/// [`super::refusal::release_symbol_refusal`]'s on the order's venue, [`super::refusal::release_venue_refusal`]'s on
/// another.
fn model_release(
    r: vike_tradehub_client::wire::OrderReleaseWire,
    order_symbol: &str,
    order_venue: &str,
) -> Result<OrderRelease, String> {
    use vike_tradehub_client::wire::OrderReleasePriceWire;
    let price = match r.price {
        None => None,
        Some(OrderReleasePriceWire::Above(level)) => Some((level, OrderReleasePriceSide::Above)),
        Some(OrderReleasePriceWire::Below(level)) => Some((level, OrderReleasePriceSide::Below)),
    };
    if let Some((level, _)) = price
        && (!level.is_finite() || level <= 0.0)
    {
        return Err(format!("the price condition's level {level} is not a price above zero"));
    }
    let symbol = match (&price, r.symbol.as_deref().map(str::trim)) {
        (None, Some(named)) => {
            return Err(format!(
                "the symbol `{named}` belongs to a price condition, and this release states none"
            ));
        }
        (_, Some("")) => return Err("the price condition's symbol is blank".to_string()),
        (_, named) => named.unwrap_or(order_symbol),
    };
    let venue = match r.venue.as_deref() {
        None => None,
        Some(named) if price.is_none() => {
            return Err(format!(
                "the venue `{named}` belongs to a price condition, and this release states none"
            ));
        }
        Some(named) if named == order_venue => None,
        Some(named) => {
            if let Some(why) = price_source_refusal(named) {
                return Err(why);
            }
            if r.symbol.is_none() {
                return Err(format!(
                    "the price condition read on `{named}` names no symbol, and the venues spell one \
                     instrument differently: name it, e.g. above:BTCUSDT:100000"
                ));
            }
            Some(named)
        }
    };
    let price = price.map(|(level, side)| OrderReleasePrice {
        symbol: symbol.into(),
        level,
        side,
        venue: venue.map(Into::into),
    });
    Ok(OrderRelease { at: r.at, price })
}

/// A `Submit`'s order-terms refusal (`vike_model::order_terms_refusal`) in the words
/// [`lower_command`] refuses it with. The dry run says it too (`preview.rs`'s `door_refusal`), the
/// one lowering refusal it does and only as the lowering's first fault, so a Preview and a Command
/// answer a trigger-less stop alike.
///
/// Beside the rule, a `trail` on any kind but the trailing ones is refused: the rule leaves it to its
/// backtest meaning, and `vike_model::OrderResting::from_request` would lower it as a trailing stop in a paper
/// book, a kind the sender did not name (a `trail_percent` there is the rule's own refusal). A peg whose anchor
/// is no word of ours is [`model_peg`]'s refusal, [`lower_command`]'s earlier fault, so the rule here judges only a
/// peg that reads.
///
/// After the terms, a limit's ATTRIBUTES (`vike_model::attributes_refusal`, judged with the order's lifetime; a
/// lifetime word that does not read is the lowering's earlier fault) and a stop's ADJUSTMENT
/// (`vike_model::stop_adjust_refusal`), each naming its field.
pub(super) fn submit_terms_refusal(
    order_type: &OrderType,
    req: &vike_tradehub_client::wire::OrderRequestWire,
) -> Option<String> {
    let stray_trail = req.trail.filter(|_| !order_type.is_trailing()).map(|t| {
        format!(
            "a trail belongs to a trailing, trailing_limit or trailing_take_profit order; this \
             {order_type} order states {t}"
        )
    });
    let peg = req.peg.as_ref().and_then(|p| model_peg(p).ok());
    let tif = req
        .time_in_force
        .as_deref()
        .and_then(|word| TimeInForce::parse(word).ok())
        .unwrap_or_default();
    stray_trail
        .or_else(|| {
            order_terms_refusal(
                order_type,
                req.price,
                req.trigger_price,
                req.trail,
                req.limit_offset,
                peg.as_ref(),
                req.trail_percent,
            )
        })
        .map(|why| format!("submit: order terms — {why}"))
        .or_else(|| {
            attributes_refusal(order_type, tif, req.attributes.as_ref())
                .map(|why| format!("submit: `attributes` — {why}"))
        })
        .or_else(|| {
            stop_adjust_refusal(order_type, req.side, req.trigger_price, req.adjust.as_ref())
                .map(|why| format!("submit: `adjust` — {why}"))
        })
}

/// The wire peg as the model's, or why it is none: an anchor word that is not one of the four, exactly.
fn model_peg(p: &vike_tradehub_client::wire::OrderPegWire) -> Result<OrderPeg, String> {
    let anchor = OrderPegAnchor::parse(&p.anchor).ok_or_else(|| {
        format!(
            "the anchor {:?} is not one of {} (exactly)",
            p.anchor,
            OrderPegAnchor::accepted_words()
        )
    })?;
    Ok(OrderPeg { anchor, offset: p.offset })
}

/// Lower ONE [`CommandWire`] into the core's real `(Command, order client id)` — the TOTAL mapping (every wire
/// variant maps). The returned `order_client_id` is what the [`vike_tradehub_client::proto::Response::Ack`] echoes: the order-client-id for
/// order-scoped verbs, empty for the account-wide ones (mass-cancel / flatten / market-exit /
/// trading-state). `Err(msg)` is a request the server refuses to lower — surfaced as
/// [`vike_tradehub_client::proto::Response::Error`], never silently dropped.
pub(super) fn lower_command(wc: CommandWire) -> Result<(Command, String), String> {
    match wc {
        CommandWire::Submit(req) => {
            // SECURITY / idempotency policy: the NETWORK path must pre-mint its own order client id. An empty
            // one would let the runtime mint (fine in-process, but a remote peer then has no stable
            // handle to cancel/dedup by), so it is refused rather than lowered.
            if req.order_client_id.trim().is_empty() {
                return Err("remote submit requires a pre-minted order_client_id".to_string());
            }
            let order_client_id = req.order_client_id.clone();
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
            let now = vike_model::now_ms();
            if let Some(why) =
                lifetime_refusal(&order_type, time_in_force, req.gtd_expiry, None, now)
            {
                return Err(format!("submit: `time_in_force` — {why}"));
            }
            // The RELEASE (`release`, a mirror of the model's), by the same rule once the lifetime passed: a
            // past one is a clock in the wrong unit, and a `gtd` order must not expire before it. Its price
            // condition reads the symbol and the venue it names, else the ORDER's own, at a level above zero.
            let release = match req
                .release
                .clone()
                .map(|r| model_release(r, &req.symbol, &req.venue))
                .transpose()
            {
                Ok(release) => release,
                Err(why) => return Err(format!("submit: `release` — {why}")),
            };
            if let Some(why) =
                lifetime_refusal(&order_type, time_in_force, req.gtd_expiry, release, now)
            {
                return Err(format!("submit: `release` — {why}"));
            }
            // The PEG (`peg`, a mirror of the model's): an anchor word that is not one of ours is REFUSED
            // naming the four, never read as another; whether the kind takes a peg is the terms rule's, next.
            let peg = match req.peg.as_ref().map(model_peg).transpose() {
                Ok(peg) => peg,
                Err(why) => return Err(format!("submit: `peg` — {why}")),
            };
            // The PRICES, by the one rule the CLI and the MCP tool apply first: a trigger kind with no
            // trigger to fire at would leave as the plain order its `price` makes (hyperliquid signed a
            // trigger-less `stop_limit` as a resting limit), so it is refused here, before the `Ack`. Then the
            // attributes and the adjustment, by their rules.
            if let Some(refusal) = submit_terms_refusal(&order_type, &req) {
                return Err(refusal);
            }
            // The DISPLAY, by the one rule the CLI and the MCP tool apply first: on a limit order only, above
            // zero and below the quantity, so no venue is handed a slice that hides nothing.
            if let Some(why) = display_refusal(&order_type, req.qty, req.display_quantity) {
                return Err(format!("submit: `display_quantity` — {why}"));
            }
            let order = OrderRequest {
                order_client_id: req.order_client_id,
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
                display_quantity: req.display_quantity,
                release,
                trail: req.trail,
                limit_offset: req.limit_offset,
                peg,
                attributes: req.attributes,
                trail_percent: req.trail_percent,
                adjust: req.adjust,
                ..Default::default()
            };
            Ok((Command::Order(OrderIntent::Submit(Box::new(order))), order_client_id))
        }
        CommandWire::Cancel(order_client_id) => {
            Ok((Command::Order(OrderIntent::Cancel(order_client_id.clone())), order_client_id))
        }
        CommandWire::Modify { order_client_id, new_qty, new_price, new_trigger_price } => {
            // A trigger is a PRICE to fire at: zero and below mean nothing (JSON carries no non-finite
            // number). Whether the order HAS a trigger is `modify_price_refusal`'s, which needs the book.
            if let Some(t) = new_trigger_price
                && (t.is_nan() || t <= 0.0)
            {
                return Err(format!(
                    "modify: `new_trigger_price` must be a price above zero, got {t}"
                ));
            }
            let echoed = order_client_id.clone();
            Ok((
                Command::Order(OrderIntent::Modify {
                    order_client_id,
                    new_qty,
                    new_price,
                    new_trigger_price,
                }),
                echoed,
            ))
        }
        // THE RISK-REDUCING TRIO carry their account through now. These three used to destructure
        // as `{ venue, symbol, .. }` — the `..` swallowed the account, so a `market-exit binance
        // ALT` reached the core as `market-exit binance` and fanned over every account of the
        // exchange. The reader is the submit arm's (`lower_reduce_account`, over the ONE parser),
        // and absence still lowers to `None`, which the core fans out exactly as before: the
        // account-less frame every existing client sends is unchanged, the panic button included.
        CommandWire::MassCancel { venue, symbol, account } => {
            let account = lower_reduce_account("mass_cancel", account.as_deref())?;
            Ok((Command::Order(OrderIntent::MassCancel { venue, symbol, account }), String::new()))
        }
        CommandWire::Flatten { venue, symbol, account } => {
            let account = lower_reduce_account("flatten", account.as_deref())?;
            Ok((Command::Order(OrderIntent::Flatten { venue, symbol, account }), String::new()))
        }
        CommandWire::MarketExit { venue, account } => {
            let account = lower_reduce_account("market_exit", account.as_deref())?;
            Ok((Command::Order(OrderIntent::MarketExit { venue, account }), String::new()))
        }
        CommandWire::SetTradingState(ws) => Ok((Command::SetTradingState(ws), String::new())),
        // STRATEGY-level write (split-plane B4): the wire carries the core's own externally-tagged
        // `StrategyParams` JSON (delegated, not mirrored — see the variant's doc in the client
        // crate), so lowering IS a deserialize into the journaled schema. An undecodable payload is
        // REFUSED here — surfaced as `Response::Error`, never silently dropped — which is also what
        // keeps "the daemon folds only params shapes the core itself defines" true by construction.
        // The empty order client id is the account-wide-verb convention (nothing order-scoped to echo).
        CommandWire::UpdateParams { venue, symbol, interval, mount_id, params } => {
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
        // `ProfileDaemon` refusals a `[strategy]` table faces at load (no file, the retired
        // `name`/`rhai`, the path rule, the Rhai numeric rule), so a remote peer gets a
        // `Response::Error` carrying the profile vocabulary's own message instead of a silent
        // recent-events note. RESOLUTION (reading the file, its header and params checks,
        // compiling or opening the strategy) still happens in the core via the injected
        // `CoreConfig::strategy_factory` (which validates AGAIN — this edge is UX, the factory is
        // the authority); a refusal the core raises later (duplicate LIVE id, unknown venue, the
        // file's own) surfaces in recent-events, the `UpdateParams` unknown-target contract. The
        // empty order client id is the account-wide-verb convention. A `file` met here came over the wire, so
        // it is fenced, and the file is read HERE rather than on the fold (`crate::mount_factory`'s
        // module doc).
        mount @ CommandWire::MountStrategy { .. } => {
            let mut spec = mount_spec(mount)?;
            if let Some(file) = spec.file.as_deref() {
                crate::mount_factory::read_wire_file(file)
                    .map_err(|e| format!("mount_strategy: {e}"))?;
            }
            // The mount's strategy code (decision 0270), read here on the connection's thread, never on the fold.
            let settings = crate::tradehub_cli::settings::booted_settings_dir();
            spec.order_client_id_code =
                crate::mount_factory::runtime_mount_order_client_id_code(settings, &spec)
                    .map_err(|e| format!("mount_strategy: {e}"))?;
            Ok((Command::MountStrategy(Box::new(spec)), String::new()))
        }
        // RUNTIME strategy UNMOUNT (split-plane B5): total except for an empty id (which can name
        // nothing). The core cancels the mount's attributed resting orders before removal — the
        // documented safe default (`vike_core`'s `unmount_strategy_runtime` arm is the authority).
        CommandWire::UnmountStrategy { controller_id } => {
            if controller_id.trim().is_empty() {
                return Err("unmount_strategy: empty mount id".to_string());
            }
            Ok((Command::UnmountStrategy { controller_id }, String::new()))
        }
        // The operator's RECONCILE verb: the id is a held divergence's `ReconcileAlertViewWire::id`, and an
        // id the core does not hold is a recent-events note there, not an error here (the same
        // "unknown id is not a crash" contract `Command::ConfirmReconcile` documents). The empty order client id
        // is the account-wide-verb convention.
        CommandWire::ReconcileConfirm { id } => Ok((Command::ConfirmReconcile(id), String::new())),
        // The operator's CANCEL of one held `UnknownOrder` row (decision 0196, rule (b)): the id is a
        // held row's `ReconcileAlertViewWire::id`, and every refusal (an id not held, a row of another kind or
        // tier, one with no id the venue can cancel by) is the core's note and WARN, not an error here.
        CommandWire::ReconcileCancel { id } => {
            Ok((Command::CancelReconcileOrder(id), String::new()))
        }
        // The operator's PER-VENUE RESUME (decision 0180): the core releases the link dead-man's
        // halt of ONE venue. An empty venue can name nothing; a venue the node does not run was
        // refused before this point (`venue_refusal`, off `addressed_venue`), and one the link
        // dead-man holds nothing for is the core's refusal (a WARN and a note), because only the
        // fold knows what the latch holds. The empty order client id is the account-wide-verb convention.
        CommandWire::ResumeVenue { venue, force } => {
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
        CommandWire::SetAccountTradingState { venue, account, state } => {
            if venue.trim().is_empty() {
                return Err("set_account_trading_state: empty venue".to_string());
            }
            // Absent: the one account `one_account_addressed` left bare, the default one.
            let label = account
                .as_deref()
                .map_or(Ok(vike_model::accounts::account_keys::AccountLabel::Default), |text| {
                    vike_model::accounts::account_keys::parse_wire_account(text)
                })
                .map_err(|e| format!("set_account_trading_state: `account` — {e}"))?;
            let route_key = vike_model::accounts::account_keys::route_key_of(&venue, &label);
            Ok((Command::SetAccountTradingState { route_key, state }, String::new()))
        }
        // THE GIVE-BACK of one admitted symbol (decision 0205): the book is composed with the ONE composer
        // (`route_key_of`), `DEFAULT` is the bare venue id, and a malformed label is REFUSED here before the `Ack`
        // (never read as another book). The statement is an `AdmitSymbol` that states `release`: the engine decides
        // what it may give back. An account the node does not run was refused earlier (`account_refusal`), and the
        // symbol's spelling was settled by `plan_unadmit` against the published block.
        CommandWire::UnadmitSymbol { venue, account, symbol } => {
            if venue.trim().is_empty() {
                return Err("unadmit_symbol: empty venue".to_string());
            }
            if symbol.trim().is_empty() {
                return Err("unadmit_symbol: empty symbol".to_string());
            }
            let label = account
                .as_deref()
                .map_or(Ok(vike_model::accounts::account_keys::AccountLabel::Default), |text| {
                    vike_model::accounts::account_keys::parse_wire_account(text)
                })
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
        CommandWire::UnlatchMount { mount_id } => {
            if mount_id.trim().is_empty() {
                return Err("unlatch_mount: empty mount id".to_string());
            }
            Ok((Command::UnlatchMount { mount_id }, String::new()))
        }
        // THE TP/SL BRACKET: the wire payload IS `OrderBracket`, field for field, so lowering is a
        // move — after `check_bracket`, which refuses before the `Ack` the VALUES the core would
        // only discover at RELEASE, with the entry already filled. The empty order client id is FORCED rather
        // than chosen: the runtime mints all three ids after this returns
        // (`vike_model::build_bracket`'s doc), so there is none to echo. That is the declared
        // exception to this function's pre-minted-id rule (`crates/vike-tradehub/CLAUDE.md`). Which
        // book it reaches, and whether that book's engine can hold it, was settled before this arm
        // ran (`account_refusal`'s bracket arm, then `bracket_refusal` in `accept_command`): this
        // function holds no roster, so it checks the frame only. The account is read by
        // `parse_wire_account`, the Submit arm's one parser: a malformed label is REFUSED here, never
        // swallowed into `None`, which would route the bracket to the venue's default book.
        CommandWire::Bracket(b) => {
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
            let spec = OrderBracket {
                venue: b.venue,
                symbol: b.symbol,
                side: b.side,
                qty: b.qty,
                entry_price: b.entry_price,
                stop_loss: b.stop_loss,
                take_profit: b.take_profit,
                account,
                // The sender's group STEM (`bracket-stem`, decision 0270): the core derives the three leg ids from it
                // and refuses whole a stem that is no v2 stem or whose leg is in use; `None` mints them there.
                order_client_id: b.order_client_id,
            };
            Ok((Command::Order(OrderIntent::Bracket(Box::new(spec))), String::new()))
        }
        // THE ORDER LIST: the wire payload IS `OrderList`, field for field, after its values are judged
        // (`model_order_list`, the half of `order_list_refusal` that needs no roster, which also refuses a leg
        // without its pre-minted id, the submit arm's rule). The `Ack` echoes every leg's id, comma-joined.
        CommandWire::OrderList(l) => {
            let spec = super::order_list::model_order_list(l)?;
            let order_client_ids: Vec<String> =
                spec.legs.iter().filter_map(|leg| leg.order_client_id.clone()).collect();
            Ok((Command::Order(OrderIntent::OrderList(Box::new(spec))), order_client_ids.join(",")))
        }
        // THE COMBO: the wire payload IS `OrderCombo`, field for field, after its values are judged (`model_combo`, the
        // whole of `combo_refusal`, which also refuses a combo without its pre-minted id, the submit arm's rule). The
        // `Ack` echoes that one id. Which book it reaches was settled before this arm ran (`one_account_addressed`,
        // `account_refusal`); what the venue takes is the core's verdict (`lower_combo`: a venue whose row takes no
        // combo, or a leg limit off a paper book, is a terminal reject by name).
        CommandWire::Combo(c) => {
            let spec = super::refusal::model_combo(c)?;
            let order_client_id = spec.order_client_id.clone().unwrap_or_default();
            Ok((Command::Order(OrderIntent::Combo(Box::new(spec))), order_client_id))
        }
        // The REQ-7 settings write is NOT a core command: `accept_command` intercepts it and
        // lowers it onto disk ([`SettingsShowSource::apply_set_setting`]) before this function is
        // reached. The arm exists so the mapping stays total; a future surface that calls
        // `lower_command` directly gets an honest refusal, never a silent drop.
        CommandWire::SetSetting { .. } => Err(
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

/// The audit VERB for a wire command — a stable, low-cardinality string for the audit record (read
/// off the wire command BEFORE it is consumed by [`lower_command`]).
pub(super) fn command_kind(wc: &CommandWire) -> &'static str {
    match wc {
        CommandWire::Submit(_) => "submit",
        CommandWire::Bracket(_) => "bracket",
        CommandWire::OrderList(_) => "order_list",
        CommandWire::Combo(_) => "combo",
        CommandWire::Cancel(_) => "cancel",
        CommandWire::Modify { .. } => "modify",
        CommandWire::MassCancel { .. } => "mass_cancel",
        CommandWire::Flatten { .. } => "flatten",
        CommandWire::MarketExit { .. } => "market_exit",
        CommandWire::SetTradingState(_) => "set_trading_state",
        CommandWire::UpdateParams { .. } => "update_params",
        CommandWire::MountStrategy { .. } => "mount_strategy",
        CommandWire::UnmountStrategy { .. } => "unmount_strategy",
        CommandWire::SetSetting { .. } => "set_setting",
        CommandWire::ReconcileConfirm { .. } => "recon_confirm",
        CommandWire::ReconcileCancel { .. } => "recon_cancel",
        CommandWire::ResumeVenue { .. } => "resume_venue",
        CommandWire::SetAccountTradingState { .. } => "set_account_trading_state",
        CommandWire::UnadmitSymbol { .. } => "unadmit_symbol",
        CommandWire::UnlatchMount { .. } => "unlatch_mount",
    }
}

/// What the door's plan opened for a symbol it was about to admit, given back when the acceptance
/// ends without admitting it (a refusal, a `Busy` lane, any `?`): the source drops the price entry
/// it counted. A no-op for a symbol that WAS admitted meanwhile (the source checks), so it is
/// simply left armed to the end of the function.
struct PriceGuard<'a> {
    source: &'a dyn SymbolAdmitter,
    route_key: String,
    symbol: String,
}

impl Drop for PriceGuard<'_> {
    fn drop(&mut self) {
        let venue = self.route_key.split('#').next().unwrap_or_default();
        self.source.release(&self.route_key, venue, &self.symbol);
    }
}
