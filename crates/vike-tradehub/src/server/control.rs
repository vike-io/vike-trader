//! `server::control` — the CONTROL path's gating and acceptance: the server-edge limits
//! ([`ControlLimitsConfig`], [`ControlLimits`]), the ONE acceptance path every control surface
//! funnels through ([`accept_command`], with its [`AcceptError`] and [`Accepted`] verdicts) and
//! the lowering of a wire command into the core's real `Command` (`lower_command`).
//!
//! Split out of `server.rs` as a pure move; the module doc there carries the acceptance contract
//! (the gate ORDER, why a `Gone` closes the connection, why the rationale never reaches the core).
//! The routing gates `accept_command` consults are `super::refusal`'s.

use std::time::Instant;

use vike_core::{CommandRejected, CommandSink};
use vike_exec::{Command, MountSpec, OrderIntent, ParamsUpdate, TradingState};
use vike_model::{BracketSpec, OrderRequest, StrategyParams};
use vike_tradehub_client::wire::{WireCommand, WireTradingState};

use super::refusal::{account_refusal, bracket_refusal, check_bracket, venue_refusal};
use super::settings::SettingsShowSource;
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
    /// notional being `vike_model::order_notional`'s `|qty| · |price| · |contract multiplier|` —
    /// see [`ControlLimits::vet`] for where the multiplier comes from, and for the floor of 1.0
    /// under it. `None` = no size cap (the core `RiskGate` is still the floor).
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
    /// Reject a Submit/Modify/Bracket whose `|qty| · |price| · |contract multiplier|` (the
    /// multiplier floored at 1.0) exceeds this — for a bracket, at any of its priced legs. `None` =
    /// no size cap.
    max_notional: Option<f64>,
    rate_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl ControlLimits {
    /// A fresh per-surface limiter over `cfg` — the token bucket starts full (`rate_per_sec`).
    pub fn new(cfg: ControlLimitsConfig) -> Self {
        ControlLimits {
            max_notional: cfg.max_notional,
            rate_per_sec: cfg.rate_per_sec,
            tokens: cfg.rate_per_sec,
            last_refill: Instant::now(),
        }
    }

    /// Vet one command before it reaches the core. `Some(reason)` ⇒ REFUSE (surfaced as a
    /// `Response::Error`); `None` ⇒ allow. The rate token is consumed on EVERY command; the notional
    /// cap applies ONLY to order-INCREASING verbs that carry a price (Submit / Modify / Bracket) —
    /// Cancel/Flatten/MassCancel/MarketExit/SetTradingState are risk-reducing and never size-capped.
    ///
    /// `blocks` is the node's PUBLISHED engine blocks ([`crate::publish::PublisherHandle::engine_blocks`])
    /// — the one thing the notional cap reads from them is the instrument's contract multiplier,
    /// and the private `notional_reason`'s doc carries the whole argument, including the empty
    /// case: `&[]` is "this surface knows no engine roster" and sizes at multiplier 1.0.
    ///
    /// `orders` is the OPEN order a `Modify` names, off the same published snapshot
    /// ([`crate::publish::PublisherHandle::open_orders_named_by`]) — the only way this edge learns
    /// which instrument a bare client order id rests on. Every other verb ignores it, and `&[]` is
    /// "the node does not know that order", which sizes a `Modify` at multiplier 1.0 as it always
    /// was; the same `notional_reason` doc carries the rest.
    pub fn vet(
        &mut self,
        cmd: &WireCommand,
        blocks: &[vike_exec::VenueBlock],
        orders: &[vike_exec::OrderView],
    ) -> Option<String> {
        if !self.take_token() {
            return Some(format!(
                "rate limited: control commands capped at {}/s on this node",
                self.rate_per_sec
            ));
        }
        self.notional_reason(cmd, blocks, orders)
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
    /// ⚠ **THE CONTRACT MULTIPLIER IS PART OF THE NOTIONAL.** This site sized `|qty| · |price|` and
    /// nothing else, while the desktop's order preview
    /// (`crates/vike-app-core/src/orders/order_entry.rs`'s `validate_with_multiplier`) and the core's
    /// `RiskGate` both route through `vike_model::order_notional`, which also multiplies by the
    /// instrument's contract multiplier. So for an instrument whose multiplier is not 1 — of the
    /// shipped venues only a deribit future reports a contract size — the ceiling an operator wrote
    /// into `policy.max_notional_per_order` was weaker here than in the GUI that sent the command,
    /// and this is the ONLY site that enforces that row: it reaches no `RiskLimits`
    /// (`crates/vike-mount/src/policy.rs`'s module doc carries why).
    ///
    /// The multiplier is read where the core's own gate reads it — the engine's account grid, as
    /// `vike_exec::VenueBlock::multiplier_of` over the published `blocks` that [`accept_command`]
    /// already carries for the bracket gate. No wire field, no new lookup across the core's thread
    /// boundary, and the grid is immutable after construction, so a published copy cannot be stale
    /// about it. The block is the one whose `route_key` the command addresses (the SAME composer
    /// [`account_refusal`] uses, so `DEFAULT` and an absent account both name the bare-venue book).
    ///
    /// **What it cannot resolve it reads as 1.0**, the answer `CoreSnapshot::multiplier_of` gives
    /// the desktop and the mount gives an instrument that reports no contract size: an empty
    /// `blocks` (the core has not published yet — [`venue_refusal`]'s UNKNOWN rule), a venue or
    /// account with no block, a label that does not parse (`lower_command` refuses that one).
    ///
    /// **A `Modify` names a client order id and nothing else, so its instrument is read off the
    /// order that id names.** `orders` is the node's published OPEN order for that id
    /// ([`crate::publish::PublisherHandle::open_orders_named_by`] — the snapshot already carries
    /// every engine's registry with each order's venue, account and symbol), and the multiplier is
    /// the one of the engine that holds it, for that order's symbol: the same two steps the core
    /// takes when it judges the projected order — `ExecutionEngine::modify_order` reads the resting
    /// request out of the engine's registry, and the risk context it builds reads that request's
    /// symbol in the engine's account grid. The id is matched on OPEN orders only (a terminal order
    /// is one the core ignores a modify of).
    ///
    /// **The order this snapshot does not know is sized at 1.0, exactly as a `Modify` was before
    /// this edge could resolve one — declared, not a claim that the multiplier does not apply.** The
    /// snapshot is built by a coalesced publish, so a coid placed moments ago may not be in it yet;
    /// an id that never existed, one already terminal, a bracket exit still HELD off the venue
    /// (those sit in `held_exits`, not in `orders`) and a surface that passes `&[]` all land there.
    /// The core's own `RiskGate` judges the projected order with the real multiplier whenever the
    /// modify reaches an engine, and it is the enforcing gate regardless; this edge is
    /// defense-in-depth, and resolving the order can only make it STRICTER or equal (the floor
    /// below), never weaker than the 1.0 it falls back to.
    ///
    /// ⚠ **The multiplier is FLOORED at 1.0, so counting it is a one-way tightening.** For a
    /// multiplier above 1 the node refuses sooner; a multiplier below 1 (no shipped venue reports
    /// one), or one that is not a finite positive number, is read as 1.0 — so the ceiling is never
    /// weaker than `|qty| · |price|`, which is what it enforced before it counted a multiplier, and
    /// a bad (too small) multiplier in venue data cannot loosen it. The floor lives in ONE place,
    /// `floored_multiplier`, which every multiplier this edge resolves goes through — a `Submit`'s,
    /// a bracket leg's and a `Modify`'s alike. The desktop and the core keep the TRUE multiplier,
    /// so for a sub-1 contract the node is deliberately stricter than the GUI that sent the order.
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
    ) -> Option<String> {
        let max = self.max_notional?;
        // NOTIONAL IS A MAGNITUDE — `vike_model::order_notional` is the workspace definition and it
        // takes `.abs()` of every factor. This site used to abs the QTY only, so a NEGATIVE price
        // yielded a negative notional, `n > max` could never trip, and the ceiling was bypassed by a
        // sign alone. (The core `RiskGate` never had this bug: it routes through `order_notional`.)
        //
        // The arms below produce the three FACTORS, `(qty, price, multiplier)`, and the one site
        // after the match multiplies them — so the finite check and the comparison cannot be
        // skipped by an arm that sizes itself.
        let sized: Option<(f64, f64, f64)> = match cmd {
            WireCommand::Submit(r) => r.price.map(|p| {
                (r.qty, p, book_multiplier(&r.venue, r.account.as_deref(), &r.symbol, blocks))
            }),
            // The multiplier is the one of the order this coid names, read off the published OPEN
            // order (the doc above); an order the snapshot does not hold is 1.0, the arithmetic
            // this arm used before it could resolve one.
            WireCommand::Modify { client_order_id, new_qty: Some(q), new_price: Some(p) } => {
                Some((*q, *p, order_multiplier(client_order_id, orders, blocks)))
            }
            // ⚠ A qty-RAISING modify that names NO price is UNCHECKABLE at this arm, so it is
            // REFUSED rather than waved through. This shape used to fall into `_ => None` — "no
            // price to size, therefore fine" — which let `/modify <coid> qty=<huge>` walk past the
            // node's one order-size ceiling completely: place a small in-cap order, then modify it
            // up. This arm sizes what the FRAME carries and nothing it would have to look up: the
            // resting price the projected order needs is in the published order, but reading it is
            // a separate decision this arm does not take (only the order's INSTRUMENT is read, for
            // the multiplier above); "I cannot evaluate this ceiling" must never render as "the
            // ceiling passed". The operator's fix is to name the price, which makes the command
            // checkable. The core `RiskGate` DOES resolve the resting terms and is the enforcing
            // gate regardless — this is the defense-in-depth edge, now failing CLOSED.
            WireCommand::Modify { new_qty: Some(q), new_price: None, .. } => {
                return Some(format!(
                    "modify to qty {q} names no price, so its notional cannot be checked against \
                     the node's policy ceiling max_notional_per_order {max:.2} — re-send the \
                     modify with an explicit price"
                ));
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
            // stop-loss at its trigger (stronger than a stop `Submit`, which names no price and is
            // not sized here). A MARKET entry is unpriced like a market `Submit`, but its exits
            // are not. The refusal NAMES the leg, which the shared sentence below cannot: the
            // breaching legs are filtered BEFORE the largest is picked, so a NaN leg (which
            // `n > max` never selects) cannot hide a finite leg that does breach. ⚠ That also
            // means a NON-FINITE bracket is not refused HERE — the finite check after this match
            // is the Submit/Modify arms', and a bracket's own is `check_bracket` (which names the
            // field), run right after this gate by `accept_command` and both previews.
            //
            // The multiplier is the bracket's engine's: [`bracket_refusal`] admits a bracket only
            // to the ONE engine keyed by the bare venue id, which is the block read here.
            WireCommand::Bracket(b) => {
                let multiplier = book_multiplier(&b.venue, None, &b.symbol, blocks);
                let breach = [
                    ("entry", b.entry_price),
                    ("stop-loss", Some(b.stop_loss)),
                    ("take-profit", Some(b.take_profit)),
                ]
                .into_iter()
                .filter_map(|(leg, px)| {
                    px.map(|p| (leg, vike_model::order_notional(b.qty, p, multiplier)))
                })
                .filter(|(_, n)| *n > max)
                .max_by(|x, y| x.1.total_cmp(&y.1));
                if let Some((leg, n)) = breach {
                    let note = multiplier_note(multiplier);
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
        if let Some((qty, price, multiplier)) = sized {
            // ⚠ BEFORE the comparison, because a comparison cannot answer this: `NaN > max` is
            // false. An INFINITE factor would mostly be refused by the cap anyway (`inf > max`),
            // except `inf · 0` is NaN — so the check is on the FACTORS, not the product. The
            // multiplier is not checked here: every arm gets it from `label_multiplier`, which has
            // already floored it (`floored_multiplier`) to a finite number of at least 1.0.
            if !(qty.is_finite() && price.is_finite()) {
                return Some(format!(
                    "order qty {qty} at price {price} has no finite notional, so it cannot be \
                     checked against the node's policy ceiling max_notional_per_order {max:.2} — \
                     refused rather than passed (a NaN notional is never above the ceiling)"
                ));
            }
            let n = vike_model::order_notional(qty, price, multiplier);
            if n > max {
                // Names the SETTING, not a variable: the ceiling is `policy.max_notional_per_order`
                // on the node (settings unification, Phase 5) and telling the caller
                // to export something would send them looking for a knob that no longer exists.
                // The multiplier clause is EMPTY at 1.0, so every sentence that existed before the
                // node counted a multiplier reads byte-identically.
                let note = multiplier_note(multiplier);
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
        self.notional_reason(cmd, blocks, orders)
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

/// The contract multiplier of the engine a command addresses, for the instrument it names — read
/// off the published `blocks`, and **1.0 when it cannot be resolved** (`notional_reason`'s doc has
/// the argument and the list of what "cannot" covers).
///
/// `account` is the WIRE spelling: absent and `DEFAULT` both name the bare-venue book and a label
/// names `venue#LABEL`, resolved through `route_key_of` — the composer [`account_refusal`] and the
/// core's own routing use — never by suffixing text here. The symbol is the ORDER's, looked up in
/// that engine's grid exactly as the core `RiskGate` does (`Account::multiplier_of` on the
/// request's symbol), so a symbol the grid does not list falls to the engine's default, 1.0 for
/// every engine `vike_mount::make_engine` builds.
fn book_multiplier(
    venue: &str,
    account: Option<&str>,
    symbol: &str,
    blocks: &[vike_exec::VenueBlock],
) -> f64 {
    use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};
    // A label that does not parse is `lower_command`'s refusal, not a reason to guess a book here.
    let label = match account.map(parse_wire_account).transpose() {
        Ok(label) => label.unwrap_or(AccountLabel::Default),
        Err(_) => return 1.0,
    };
    label_multiplier(venue, &label, symbol, blocks)
}

/// The contract multiplier of the instrument the OPEN order `coid` rests on — the engine that holds
/// it (its venue and account, as the published order says them) and its symbol, resolved by
/// [`label_multiplier`] exactly as [`book_multiplier`] resolves a `Submit`'s — and **1.0 when the
/// published `orders` hold no open order of that id** (`notional_reason`'s doc has the list of what
/// that covers, and why it is the arithmetic a `Modify` always had).
///
/// Open means NOT terminal, the question `vike_exec::CoreSnapshot::open_order` asks of the same
/// rows: a terminal order is one the core ignores a modify of, so it lends this edge nothing.
fn order_multiplier(
    coid: &str,
    orders: &[vike_exec::OrderView],
    blocks: &[vike_exec::VenueBlock],
) -> f64 {
    let Some(order) = orders.iter().find(|o| o.client_order_id == coid && !o.status.is_terminal())
    else {
        return 1.0;
    };
    // `None` is how a snapshot spells the DEFAULT account (`OrderView::account`'s doc).
    let label = order.account.clone().unwrap_or_default();
    label_multiplier(&order.venue, &label, &order.symbol, blocks)
}

/// The multiplier of `symbol` on the engine whose routing key is `route_key_of(venue, label)`, read
/// off the published `blocks` — the ONE lookup behind [`book_multiplier`] (a frame names the book)
/// and [`order_multiplier`] (a resting order names it), so the two cannot answer differently about
/// which block is which — and the ONE place the published value is floored
/// ([`floored_multiplier`]). **1.0 when no published block carries that key.**
fn label_multiplier(
    venue: &str,
    label: &vike_model::accounts::account_keys::AccountLabel,
    symbol: &str,
    blocks: &[vike_exec::VenueBlock],
) -> f64 {
    let key = vike_model::accounts::account_keys::route_key_of(venue, label);
    blocks
        .iter()
        .find(|b| b.route_key == key)
        .map_or(1.0, |b| floored_multiplier(b.multiplier_of(symbol)))
}

/// **THE FLOOR: the multiplier the ceiling counts is never below 1.0.** `m` when it is finite and
/// above 1; 1.0 when it is below 1, zero, negative or not a finite number.
///
/// The ceiling enforced `|qty| · |price|` before it counted a multiplier at all, so a published
/// multiplier below 1 — which would size an order SMALLER than that, as the desktop and the core
/// do with the true value — must not make the node weaker than it was, and a degenerate one
/// (`0`, a negative, NaN, infinite) is bad venue data that must not loosen it either (`0` would
/// size every order at nothing). A multiplier above 1 passes through untouched. Only this edge
/// floors: the desktop's preview and the core's `RiskGate` keep the true multiplier, so for a
/// contract below 1 the node is deliberately the stricter of the two. No shipped venue reports one
/// below 1, so on today's rosters this changes nothing.
fn floored_multiplier(m: f64) -> f64 {
    if m.is_finite() && m > 0.0 { m.max(1.0) } else { 1.0 }
}

/// The clause that tells an operator why a refused notional is not `qty × price` — EMPTY at a
/// multiplier of exactly 1.0, so every refusal sentence that existed before this edge counted a
/// multiplier reads byte-identically (tests and runbooks key off those words).
fn multiplier_note(multiplier: f64) -> String {
    if multiplier == 1.0 {
        String::new()
    } else {
        format!(" (the notional counts this instrument's contract multiplier {multiplier})")
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
    /// account-wide verbs, exactly as before).
    Coid(String),
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
    /// echoed coid, or the account-wide empty string for a settings write.
    pub fn into_coid(self) -> String {
        match self {
            Accepted::Coid(coid) => coid,
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
///    [`bracket_refusal`] (its values, and what the one engine it reaches can hold).
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
/// `key_id` is the caller's authenticated identity — `vike_tradehub_client::NodeKeys`'
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
/// of one `portfolio.venues`, so they are empty together. ⚠ For every verb but a BRACKET: a
/// bracket names no account, so its gate needs a POSITIVE answer ("this venue's one account is the
/// default one") that an empty roster cannot give, and it refuses there
/// (`crates/vike-tradehub/src/server/refusal.rs`'s `bracket_book_refusal`).
///
/// `blocks` is the engine BLOCKS that same roster publishes
/// ([`crate::publish::PublisherHandle::engine_blocks`]), read by TWO gates. A bracket's, step
/// 1d ([`bracket_refusal`]), which must know what the one engine the account gate admitted it to
/// TRADES — a venue adapter places every order on its engine's own instrument, whatever the frame
/// names. And the notional ceiling of step 1, which reads the addressed engine's CONTRACT
/// MULTIPLIER for the instrument it sizes (`ControlLimits::vet`; an empty or unmatched roster
/// sizes at 1.0, as the ceiling did before it counted one). Every other verb ignores it. A surface
/// that passes `&[]` refuses every bracket rather than admitting one unchecked
/// ([`super::refusal::bracket_engine_refusal`]'s fail-closed arm).
///
/// `orders` is the OPEN order a `Modify` names, off the same published snapshot
/// ([`crate::publish::PublisherHandle::open_orders_named_by`]), read by ONE gate: the notional
/// ceiling of step 1, which sizes a `Modify` with the contract multiplier of the instrument that
/// order rests on (`ControlLimits::vet`; an order the snapshot does not hold sizes at 1.0, as a
/// `Modify` always did). Every other verb ignores it, and a surface that passes `&[]` is byte-identical
/// to the node before the edge could resolve one.
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
    blocks: &[vike_exec::VenueBlock],
    orders: &[vike_exec::OrderView],
    peer: Option<std::net::SocketAddr>,
    key_id: Option<&str>,
) -> Result<Accepted, AcceptError> {
    if let Some(refusal) = limits.vet(&cmd, blocks, orders) {
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
                vike_model::change_journal::Outcome::AppliedPendingRestart
            } else {
                vike_model::change_journal::Outcome::Applied
            },
        });
        return Ok(Accepted::SettingsWritten { restart_required });
    }
    let (lowered, coid) = lower_command(cmd).map_err(AcceptError::Refused)?;
    match sink.try_command(lowered) {
        Ok(()) => {
            audit::record(peer, kind, &coid, audit_reason.as_deref());
            Ok(Accepted::Coid(coid))
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
            let order = OrderRequest {
                client_order_id: req.client_order_id,
                venue: req.venue,
                symbol: req.symbol,
                side: req.side,
                qty: req.qty,
                order_type: req.order_type,
                price: req.price,
                trigger_price: req.trigger_price,
                reduce_only: req.reduce_only,
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
        // THE TP/SL BRACKET: the wire payload IS `BracketSpec`, field for field, so lowering is a
        // move — after `check_bracket`, which refuses before the `Ack` the VALUES the core would
        // only discover at RELEASE, with the entry already filled. The empty coid is FORCED rather
        // than chosen: the runtime mints all three ids after this returns
        // (`vike_model::build_bracket`'s doc), so there is none to echo. That is the declared
        // exception to this function's pre-minted-id rule (`crates/vike-tradehub/CLAUDE.md`). Which
        // book it reaches, and whether that book's engine can hold it, was settled before this arm
        // ran (`account_refusal`'s bracket arm, then `bracket_refusal` in `accept_command`): this
        // function holds no roster, so it checks the frame only.
        WireCommand::Bracket(b) => {
            check_bracket(&b)?;
            let spec = BracketSpec {
                venue: b.venue,
                symbol: b.symbol,
                side: b.side,
                qty: b.qty,
                entry_price: b.entry_price,
                stop_loss: b.stop_loss,
                take_profit: b.take_profit,
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

/// Map the wire trading-state mirror back onto `vike_exec::TradingState` — the reverse of
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
    }
}
