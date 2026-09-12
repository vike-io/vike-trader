//! Polymarket CLOB order submit/cancel over the L2-authenticated REST, through the geo-routing
//! proxy.
//!
//! Order PLACEMENT is geo-restricted (blocked from some regions); every call here rides the
//! proxy-aware agent from [`crate::egress`] — the SOCKS resolution (`POLY_SOCKS_PROXY` /
//! `POLY_PROXY_*`, process-env-first, workspace-`.env` second), the agents and the keyless
//! `get_json` were re-homed THERE for the feeds/exec seam (split-plane Phase 5), because the feed
//! pumps dial through the same tunnel and must compile without this module. The L2 HMAC signs the
//! EXACT body string sent. See `crate::egress`'s module doc for the resolution rules and the
//! narrow `.env` allow-list ([`crate::egress::PROXY_KEYS`]).
//!
//! ## The client-side rate-budget mirror
//! This module is also the ONE place every CLOB order/cancel round-trip passes through, so it owns
//! the wiring of [`crate::rate_budget`] — the per-signer mirror of the venue's dual order/cancel
//! token buckets. That limiter entered **warning mode 2026-07-24 with enforcement roughly two weeks
//! out**, i.e. now; the hazard it flagged is that `DELETE /cancel-all` debits **1 up front plus 1
//! per order actually canceled**, which can drive the cancel bucket NEGATIVE and lock out *every*
//! subsequent cancel — a maker stuck with live quotes resting into a resolving market.
//!
//! Four capabilities, all resolved here:
//! 1. [`gate_submit`] consults the mirror BEFORE a submit costs a token. **Observe-only by
//!    default** — it counts ([`rate_gate_would_block_count`]) and logs the would-block but still
//!    sends, so the mirror can never itself become a new liveness hazard before it is proven
//!    against the real headers. `POLY_RATE_GATE=1` ([`rate_gate_enforced`]) turns it into a real
//!    local refusal, which the exec thread maps to an `OrderRejected` naming the reason.
//! 2. The cancel-token reserve floor holds routine ladder churn back so an emergency flatten is
//!    never rate-limit-locked. **This is the ARMED, LIVE path**: [`gate_cancel_shared`] is called
//!    from [`crate::client`]'s `ExecCommand::Cancel` arm — the door every SINGLE core cancel comes
//!    through — and it consults [`RateBudget::routine_cancel_decision`] for a
//!    [`CancelIntent::Routine`] cancel and for nothing else. A shed cancel is reported as a
//!    NON-terminal `OrderCancelRejected` (the order is still resting; nothing vanishes) and the
//!    caller re-offers it. [`plan_cancels`] applies the SAME invariant to a whole id list, on the
//!    batch door beside it (`ExecCommand::CancelBatch`).
//! 3. [`plan_cancels`] also picks **targeted** over `cancel-all` whenever the `1 + n` debit could
//!    overshoot: targeted spends to exactly zero and stops, `cancel-all` can go negative.
//!    [`cancel_all_orders`] is therefore only ever reached through a plan that proved it safe.
//! 4. [`log_rate_signal`] — the pre-existing `Poly-RateLimit-*` header tap on every signed
//!    response — now also folds the venue-authoritative `remaining` back into the mirror
//!    ([`RateBudget::reconcile`]), so an imperfect local model can never drift more permissive than
//!    the server for longer than one round-trip.
//!
//! ⚠ **What is armed.** Both halves now are, and the second one arrived late for a reason worth
//! keeping: the reserve floor (capability 2) reaches production through the per-order door above,
//! so the cancel bucket is no longer a write-only counter and `DEFAULT_RESERVE_FRAC` governs real
//! behaviour on the venue wired to a REAL mainnet account — tune it as a live risk parameter. The
//! BULK arm (capability 3) was unreachable DESIGN for as long as the shared `ExecutionClient` seam
//! shredded a batch before any venue code ran: `ExecActor::cancel_batch_with_intent` fanned out
//! into `n` per-id `ExecCommand::Cancel`s, so nothing in `crates/` could call [`cancel_orders`] or
//! [`cancel_all_orders`] and the `1 + n` overshoot their guards exist for could not occur either.
//! `ExecCommand::CancelBatch` (delivered because [`crate::client`] declares
//! `ExecActor::with_bulk_cancel`) carries the batch whole to the exec thread, which hands it to
//! [`cancel_orders`] — so the strategy choice and the reserve now both apply to a batch.
//!
//! ⚠ **Two residuals the bulk arm carries, declared rather than designed away.** `DELETE
//! /cancel-all` is ACCOUNT-wide while [`CancelScope::WholeBook`] can only be asserted from this
//! process's in-memory registry, and its `1 + N` debit counts the orders the VENUE cancelled, not
//! the `n` we named — so a stale resting order from a previous process is both cancelled and
//! under-debited by the mirror. The over-debit direction is corrected by the venue's own
//! `remaining` headers within one round-trip ([`RateBudget::reconcile`]); the cancel is not
//! reversible, which is why the bulk arm is reached only when the batch names the whole known book.
//!
//! ⚠ The intent itself is NOT this crate's to define — it is `vike_exec::CancelIntent`, on the
//! shared `ExecutionClient` seam (`cancel_with_intent`), because it originates in `vike_core`'s
//! runtime where a flatten is distinguishable from a requote. Its DEFAULT is
//! [`CancelIntent::Unspecified`], which this venue treats exactly as a flatten: an operator's DOM
//! click, a tradehub ticket or any caller that did not classify is never held back.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::egress::agent;

use super::auth::l2_auth_headers;
use super::config::PolymarketCreds;
use super::rate_budget::{
    CancelDecision, CancelStrategy, RateBudget, RateLimitSignal, SubmitDecision, Tier,
};
// The cancel classification is the SHARED `ExecutionClient` seam's, not this crate's: it has to
// reach here from `vike_core`'s runtime through `ExecutionClient::cancel_with_intent`, so a local
// copy would be a second name for the one fact that matters. This module's own enum was deleted
// when the seam landed — no re-export was left behind (the workspace's no-shim-on-a-move rule).
use vike_exec::CancelIntent;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// POST `/order` request body: the signed order object wrapped with owner (L2 api key) + orderType.
/// `deferExec:false` matches the official SDK wire (createSendOrderPayload).
pub fn submit_body(
    order_obj: serde_json::Value,
    api_key: &str,
    order_type: &str,
) -> serde_json::Value {
    serde_json::json!({ "deferExec": false, "order": order_obj, "owner": api_key, "orderType": order_type })
}

/// `DELETE /order` request body (the SDK cancels via DELETE, not a POST to `/cancel`).
pub fn cancel_body(order_id: &str) -> serde_json::Value {
    serde_json::json!({ "orderID": order_id })
}

// --- the client-side rate-budget mirror (see the module doc's fourth section) -------------------

/// The workspace-`.env` / process-env name of the LOCAL submit-gate ENFORCEMENT opt-in.
pub const POLY_RATE_GATE_ENV: &str = "POLY_RATE_GATE";

/// Wall-clock milliseconds — the ONLY clock [`crate::rate_budget`] ever sees. Every decision fn
/// below takes `now_ms` as a parameter (the mirror calls no `Instant::now()` internally), which is
/// what makes the whole budget layer testable with an injected clock.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The process-wide mirror of the venue's dual buckets.
///
/// The venue meters **per signer**, and a process mounts exactly one Polymarket signer (one
/// `POLY_PRIVATE_KEY`, resolved once in [`crate::mount::live_mount_from_vars`]), so one shared
/// mirror is the right granularity rather than an approximation. It starts at [`Tier::Standard`] —
/// the LOWEST documented rate, i.e. the conservative assumption — and every response carrying
/// `Poly-RateLimit-*` headers snaps it to the venue's own numbers (see [`fold_rate_signal`]), so a
/// higher-tier signer is corrected upward within one round-trip instead of being modelled by a
/// guess at mount.
///
/// A poisoned lock is recovered from rather than propagated: a panic while holding this mutex must
/// not take the order path down with it, and the worst case is a mirror one decision stale.
fn budget() -> &'static Mutex<RateBudget> {
    static BUDGET: OnceLock<Mutex<RateBudget>> = OnceLock::new();
    BUDGET.get_or_init(|| Mutex::new(RateBudget::new(Tier::Standard, now_ms())))
}

/// Run `f` against the shared mirror with a freshly-read clock.
fn with_budget<T>(f: impl FnOnce(&mut RateBudget, i64) -> T) -> T {
    let mut guard = budget().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard, now_ms())
}

/// How many submits the local gate has flagged as over budget — counted in BOTH modes, so the
/// observe-only default produces the evidence for turning enforcement on.
static WOULD_BLOCK: AtomicU64 = AtomicU64::new(0);

/// Submits the local gate found over budget since process start (see `gate_submit`). In the
/// default observe-only mode every one of these still went to the wire.
pub fn rate_gate_would_block_count() -> u64 {
    WOULD_BLOCK.load(Ordering::Relaxed)
}

/// Whether the local submit gate REFUSES (rather than merely observing) — the EXACT string `"1"`,
/// in the process env OR the workspace `.env` map. Default OFF, exactly the `POLY_EXEC` /
/// `POLY_RECONCILE` idiom (not a fuzzy truthy parse).
///
/// Kept opt-in on purpose: the venue is still inside its grace window, the per-tier rates in
/// [`crate::rate_budget`] are documented figures rather than live-verified ones, and a local model
/// that refuses orders it should have sent is a WORSE failure than the 429 it is trying to avoid.
/// Turn it on once [`rate_gate_would_block_count`] and the reconciled header numbers agree.
pub fn rate_gate_enforced() -> bool {
    rate_gate_enforced_from(
        std::env::var(POLY_RATE_GATE_ENV).ok().as_deref(),
        crate::egress::dotenv_rate_gate(),
    )
}

/// [`rate_gate_enforced`]'s pure core, split out so both tiers are unit-testable without mutating
/// the process-global env (`set_var` is unsound under threads — the repo's rule). Tolerates the
/// trailing `#` comment the real workspace `.env` annotates its lines with, like `poly_exec_enabled`.
fn rate_gate_enforced_from(from_env: Option<&str>, from_dotenv: Option<&str>) -> bool {
    from_env == Some("1") || from_dotenv.map(super::config::first_token) == Some("1")
}

/// Turn one mirror verdict into the submit path's action.
///
/// OBSERVE-ONLY unless `enforced`: a dry order bucket is counted and logged, and the order STILL
/// goes to the wire — with `POLY_RATE_GATE` unset this function can only ever return `Ok`, so the
/// submit path is byte-identical to before the mirror existed. Under the opt-in it returns the
/// refusal string the exec thread turns into an `OrderRejected` (no order silently vanishes).
fn submit_gate_action(decision: SubmitDecision, enforced: bool) -> Result<(), String> {
    match decision {
        SubmitDecision::Allow => Ok(()),
        SubmitDecision::Throttle { retry_ms } => {
            WOULD_BLOCK.fetch_add(1, Ordering::Relaxed);
            if enforced {
                Err(format!(
                    "local rate budget: polymarket order bucket is dry, retry in {retry_ms}ms"
                ))
            } else {
                tracing::warn!(
                    venue = "polymarket",
                    retry_ms,
                    would_block = WOULD_BLOCK.load(Ordering::Relaxed),
                    "submit is over the LOCAL rate budget — observing only (set POLY_RATE_GATE=1 \
                     to refuse locally instead of paying the venue's 429)"
                );
                Ok(())
            }
        }
    }
}

/// Consult the shared mirror before a submit costs an order token (see [`submit_gate_action`]).
fn gate_submit() -> Result<(), String> {
    let decision = with_budget(|b, now| b.submit_decision(now));
    submit_gate_action(decision, rate_gate_enforced())
}

/// Debit one order token. Called BEFORE the wire call, because the venue charges on receipt: a
/// request that dies in transit over-debits us by one, which is the conservative direction, and the
/// next response's headers reconcile it away.
fn note_submit() {
    with_budget(|b, now| b.on_submit(now));
}

/// Debit one cancel token, for the single-order cancel path.
fn note_cancel() {
    with_budget(|b, now| b.on_cancel(now));
}

/// Whether ONE cancel of `intent` may fire against `budget` right now, or must be shed to protect
/// the flatten reserve. The PURE half of the reserve invariant — no I/O, no globals, injected clock
/// — so the whole contract is testable without touching the process-wide mirror.
///
/// ⚠ **A cancel the reserve may not shed does not even LOOK at the budget.** Only
/// [`CancelIntent::may_be_shed`] (i.e. [`CancelIntent::Routine`]) consults
/// [`RateBudget::routine_cancel_decision`]; [`CancelIntent::Unspecified`] and
/// [`CancelIntent::RiskOff`] return `Ok` having read nothing and debited nothing, which is what
/// makes an unclassified cancel byte-identical to this venue's behaviour before the intent existed.
///
/// `Err` is the SHED, carrying the operator-facing reason: the caller must turn it into a
/// NON-terminal `OrderCancelRejected` and leave the order live (a shed cancel that vanished would
/// be worse than the rate-limit it avoids — the order is still resting at the venue).
///
/// ⚠ **This one ENFORCES, while the sibling submit gate is observe-only behind `POLY_RATE_GATE` —
/// deliberately, and the asymmetry is the argument.** A wrongly-refused SUBMIT is a trade that did
/// not happen and cannot be recovered, which is why that gate counts rather than blocks until the
/// per-tier rates are live-verified. A wrongly-shed ROUTINE cancel costs one quote left resting for
/// one refill window, is reported to the strategy, and is re-offered on its next tick — while the
/// failure it prevents is a signer with no cancel tokens and live quotes in a resolving market.
/// The mirror also self-corrects upward from the venue's own `remaining` headers within one
/// round-trip ([`RateBudget::reconcile`]), so an under-modelled tier cannot shed for long.
pub fn gate_cancel(
    budget: &mut RateBudget,
    intent: CancelIntent,
    now_ms: i64,
) -> Result<(), String> {
    if !intent.may_be_shed() {
        return Ok(());
    }
    match budget.routine_cancel_decision(now_ms) {
        CancelDecision::Allow => Ok(()),
        CancelDecision::Defer { retry_ms } => Err(format!(
            "routine cancel shed to protect the flatten reserve (retry in {retry_ms}ms)"
        )),
    }
}

/// [`gate_cancel`] against the process-wide mirror, with a freshly-read clock — the live door's
/// entry point. Runs on the venue exec thread, the ONLY thread that debits the cancel bucket, so
/// the decision and the debit that follows it cannot interleave with another cancel's.
pub fn gate_cancel_shared(intent: CancelIntent) -> Result<(), String> {
    with_budget(|b, now| gate_cancel(b, intent, now))
}

/// What the id list handed to [`cancel_orders`] represents.
///
/// Load-bearing, not decoration: `DELETE /cancel-all` is **account-wide**, so taking it for a
/// SUBSET would cancel resting orders the caller never named. The bulk arm is therefore eligible
/// only for [`CancelScope::WholeBook`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelScope {
    /// The ids ARE this signer's complete resting book, so one `cancel-all` is equivalent to
    /// cancelling them one by one.
    ///
    /// ⚠ **The caller ASSERTS this, and the strongest assertion available in-tree is narrower than
    /// the account.** [`crate::client`]'s bulk-cancel arm asserts it from
    /// `PolymarketRegistry::covers_all_live` — every order THIS PROCESS believes is resting — which
    /// is exact for what this process placed (one mount per venue, one exec thread, one registry)
    /// and blind to an order a PREVIOUS process left resting or another client placed on the same
    /// signer. `cancel-all` cancels those too. That residual is accepted rather than closed
    /// because the alternative reading is worse: a strategy's declared flatten leaving orphaned
    /// quotes resting on the same signer is the situation reconcile's `OrphanLocalOrder` exists to
    /// complain about, and the bulk arm is only ever reached when the caller asked to pull the
    /// whole book anyway. A SUBSET request never reaches it — see [`CancelScope::Subset`].
    WholeBook,
    /// A subset of the resting book — the plan stays targeted no matter how much budget is free.
    Subset,
}

/// The pure plan one [`cancel_orders`] pass resolves to under the current budget. No I/O, so the
/// whole reserve + strategy contract is testable directly with an injected clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelPlan {
    /// One bulk `cancel-all`, or targeted one-by-one.
    pub strategy: CancelStrategy,
    /// How many of the requested ids this pass sends.
    pub send: usize,
    /// How many the reserve floor held back (always `0` for [`CancelIntent::RiskOff`]).
    pub deferred: usize,
    /// Milliseconds until the deferred remainder is affordable again; `0` when nothing deferred.
    pub retry_ms: i64,
}

/// How many single-token routine cancels the budget can afford right now without breaching the
/// reserve, and the refill ETA if it ran out before `want`.
///
/// Simulates on a CLONE of the budget — `routine_cancel_decision` reads a running balance, so the
/// only faithful way to count `want` of them ahead of time is to actually walk them. Nothing here
/// debits the real mirror; [`cancel_orders`] does that per call it genuinely makes.
fn routine_affordable(budget: &RateBudget, want: usize, now_ms: i64) -> (usize, i64) {
    let mut sim = budget.clone();
    let mut affordable = 0usize;
    while affordable < want {
        match sim.routine_cancel_decision(now_ms) {
            CancelDecision::Allow => {
                sim.on_cancel(now_ms);
                affordable += 1;
            }
            CancelDecision::Defer { retry_ms } => return (affordable, retry_ms),
        }
    }
    (affordable, 0)
}

/// Decide how to cancel `n_resting` orders under the current budget — capabilities 2 and 3 of the
/// module doc, in one pure function.
///
/// `cancel-all` is the cheapest wire round-trip but debits `1 + n`, which can drive the bucket
/// negative and lock out every future cancel; it is chosen only when BOTH guards clear it: the
/// venue-level one ([`RateBudget::cancel_strategy`], "the debit cannot go below zero") and — for
/// [`CancelIntent::Routine`] — the reserve invariant ("and it cannot eat the flatten reserve
/// either"). Otherwise the plan is targeted, which spends to exactly zero and stops.
///
/// A routine pass that cannot afford every id sends what it can and reports the rest as
/// `deferred`; the caller re-offers them after `retry_ms`. A pass the reserve may not shed
/// ([`CancelIntent::RiskOff`] AND the flatten-safe [`CancelIntent::Unspecified`] default) never
/// defers anything.
pub fn plan_cancels(
    budget: &RateBudget,
    n_resting: usize,
    scope: CancelScope,
    intent: CancelIntent,
    now_ms: i64,
) -> CancelPlan {
    if n_resting == 0 {
        return CancelPlan {
            strategy: CancelStrategy::Targeted,
            send: 0,
            deferred: 0,
            retry_ms: 0,
        };
    }
    // `cancel-all` costs `1 + n` tokens, so affordability is always measured against `n + 1` —
    // i.e. the bulk arm needs strictly MORE affordable tokens than there are ids.
    let bulk_cost = n_resting + 1;
    // `may_be_shed` rather than a `match`: it is the ONE authority for which intents the reserve
    // may hold back, and an arm-by-arm copy here is exactly how `Unspecified` would drift from
    // "flatten-safe" to "routine" the day a fourth intent is added.
    let (affordable, retry_ms) = if intent.may_be_shed() {
        routine_affordable(budget, bulk_cost, now_ms)
    } else {
        (bulk_cost, 0)
    };
    let venue_safe_bulk = scope == CancelScope::WholeBook
        && budget.clone().cancel_strategy(n_resting, now_ms) == CancelStrategy::CancelAll;
    if venue_safe_bulk && affordable > n_resting {
        return CancelPlan {
            strategy: CancelStrategy::CancelAll,
            send: n_resting,
            deferred: 0,
            retry_ms: 0,
        };
    }
    let send = affordable.min(n_resting);
    let deferred = n_resting - send;
    CancelPlan {
        strategy: CancelStrategy::Targeted,
        send,
        deferred,
        retry_ms: if deferred > 0 { retry_ms } else { 0 },
    }
}

/// The outcome of one [`cancel_orders`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelBatch {
    /// The plan this pass executed.
    pub plan: CancelPlan,
    /// `(order_id, reason)` for every cancel that FAILED. The bulk arm reports its single failure
    /// under the id `"*"`, since one call covers every resting order.
    pub errors: Vec<(String, String)>,
    /// Ids the reserve floor held back — re-offer them after [`CancelPlan::retry_ms`].
    pub deferred: Vec<String>,
}

/// The relayer's two plaintext headers (deposit-wallet / `POLY_1271` accounts), built once.
fn relayer_headers(relayer_key: &str, relayer_address: &str) -> Vec<(String, String)> {
    vec![
        ("RELAYER_API_KEY".to_string(), relayer_key.to_string()),
        ("RELAYER_API_KEY_ADDRESS".to_string(), relayer_address.to_string()),
    ]
}

/// Cancel a set of resting orders under the budget: [`plan_cancels`] chooses the strategy and how
/// many to send, this executes it and debits the mirror for exactly what went out.
///
/// `relayer` is `Some((key, address))` for a deposit-wallet (`POLY_1271`) account, `None` for a
/// bare EOA — the same split [`cancel_order`] / [`cancel_order_relayer`] make.
pub fn cancel_orders(
    base: &str,
    creds: &PolymarketCreds,
    order_ids: &[String],
    scope: CancelScope,
    intent: CancelIntent,
    relayer: Option<(&str, &str)>,
) -> CancelBatch {
    let extra = relayer.map(|(k, a)| relayer_headers(k, a)).unwrap_or_default();
    let plan = with_budget(|b, now| plan_cancels(b, order_ids.len(), scope, intent, now));
    let mut errors = Vec::new();
    match plan.strategy {
        CancelStrategy::CancelAll => {
            with_budget(|b, now| b.on_cancel_all(order_ids.len(), now));
            if let Err(e) = cancel_all_orders(base, creds, relayer) {
                errors.push(("*".to_string(), e));
            }
        }
        CancelStrategy::Targeted => {
            for id in order_ids.iter().take(plan.send) {
                note_cancel();
                if let Err(e) = del_signed(base, "/order", creds, &cancel_body(id), &extra) {
                    errors.push((id.clone(), e));
                }
            }
        }
    }
    if plan.deferred > 0 {
        tracing::warn!(
            venue = "polymarket",
            sent = plan.send,
            deferred = plan.deferred,
            retry_ms = plan.retry_ms,
            "routine cancel churn shed to protect the flatten reserve"
        );
    }
    CancelBatch { plan, errors, deferred: order_ids.iter().skip(plan.send).cloned().collect() }
}

/// `DELETE /cancel-all` — the venue's account-wide bulk cancel, signed with an EMPTY body (the L2
/// HMAC covers method + path + `""`, the same shape [`get_signed`] uses).
///
/// ⚠ Two reasons never to call this directly from a strategy: it is **account-wide** (it does not
/// take an id list), and its debit is `1 + N` for the N orders the venue actually cancels, which is
/// the exact over-debit that can drive the cancel bucket negative and lock out every subsequent
/// cancel. Go through [`cancel_orders`], whose [`plan_cancels`] only reaches this arm once both
/// guards have cleared it.
///
/// ⚠ NOT live-verified from this workspace: the path is the one [`crate::rate_budget`]'s module doc
/// names (and the SDK's bulk-cancel route), but nothing in-tree has issued it against the real CLOB
/// yet. It is now REACHABLE — [`crate::client`]'s `ExecCommand::CancelBatch` arm routes a
/// whole-book batch through [`cancel_orders`], whose [`plan_cancels`] can choose this arm — so the
/// owed live round-trip is now a live-money one, and the first flatten that takes this branch is
/// the verification. Watch the returned body and the `Poly-RateLimit-*` headers on it.
pub fn cancel_all_orders(
    base: &str,
    creds: &PolymarketCreds,
    relayer: Option<(&str, &str)>,
) -> Result<serde_json::Value, String> {
    let extra = relayer.map(|(k, a)| relayer_headers(k, a)).unwrap_or_default();
    del_signed_payload(base, "/cancel-all", creds, String::new(), &extra)
}

/// Fold a parsed `Poly-RateLimit-*` signal into `budget`; returns whether anything was applied.
///
/// The server is always the source of truth — this is what keeps an imperfect local model from
/// drifting more permissive than reality for longer than one round-trip. A non-actionable signal
/// (a response carrying no rate headers at all, the overwhelmingly common case) leaves the mirror
/// untouched rather than pretending a missing header meant zero.
fn fold_rate_signal(budget: &mut RateBudget, sig: &RateLimitSignal, now_ms: i64) -> bool {
    if !sig.is_actionable() {
        return false;
    }
    budget.reconcile(sig, now_ms);
    true
}

/// Inspect a CLOB response's `Poly-RateLimit-*` headers: reconcile the client-side mirror to the
/// venue-authoritative numbers, and emit a WARN during the venue's per-signer rate-limit grace
/// window (warning mode 2026-07-24; enforcement ~2026-08 — PM deep-dive finding #2). No behavior or
/// return change to the caller; the mirror and its reserve invariant live in [`crate::rate_budget`],
/// and this remains the ONE tap that reads the live signal.
fn log_rate_signal(headers: &ureq::http::HeaderMap) {
    let sig = crate::rate_budget::parse_headers(
        headers.iter().filter_map(|(name, value)| Some((name.as_str(), value.to_str().ok()?))),
    );
    with_budget(|b, now| fold_rate_signal(b, &sig, now));
    if sig.warning {
        tracing::warn!(
            venue = "polymarket",
            raw = ?sig.raw_warning,
            order_remaining = ?sig.order_remaining,
            cancel_remaining = ?sig.cancel_remaining,
            reset_secs = ?sig.reset_secs,
            "per-signer rate-limit WARNING header present (grace window; enforcement imminent) — \
             mirror your buckets and prefer targeted cancels over cancel-all"
        );
    }
}

/// L2-signed `DELETE` with a JSON body (+ optional relayer headers). ureq 3.x's typed `.delete()`
/// can't carry a body, so build the request explicitly and run it.
fn del_signed(
    base: &str,
    path: &str,
    creds: &PolymarketCreds,
    body: &serde_json::Value,
    extra: &[(String, String)],
) -> Result<serde_json::Value, String> {
    del_signed_payload(
        base,
        path,
        creds,
        serde_json::to_string(body).map_err(|e| e.to_string())?,
        extra,
    )
}

/// [`del_signed`] over an already-serialized payload — the L2 HMAC signs the EXACT body string
/// sent, so the two must never re-serialize independently. Split out for the bodyless bulk cancel
/// ([`cancel_all_orders`] signs `""`, the shape [`get_signed`] uses).
fn del_signed_payload(
    base: &str,
    path: &str,
    creds: &PolymarketCreds,
    payload: String,
    extra: &[(String, String)],
) -> Result<serde_json::Value, String> {
    let mut headers =
        l2_auth_headers(creds, now_secs(), "DELETE", path, &payload).map_err(|e| e.to_string())?;
    headers.extend_from_slice(extra);
    let mut b = ureq::http::Request::builder()
        .method("DELETE")
        .uri(format!("{base}{path}"))
        .header("Content-Type", "application/json");
    for (k, v) in &headers {
        b = b.header(k.as_str(), v.as_str());
    }
    let req = b.body(payload).map_err(|e| e.to_string())?;
    let mut resp = agent().run(req).map_err(|e| format!("network: {e}"))?;
    log_rate_signal(resp.headers());
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("bad json ({status}): {e}: {text}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("polymarket {status}: {text}"));
    }
    Ok(v)
}

fn post_signed(
    base: &str,
    path: &str,
    creds: &PolymarketCreds,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    post_signed_extra(base, path, creds, body, &[])
}

/// Like [`post_signed`] but appends extra plaintext headers (the deposit-wallet/relayer flow sends
/// `RELAYER_API_KEY` / `RELAYER_API_KEY_ADDRESS` alongside the L2 HMAC — see the SDK's secureClob).
fn post_signed_extra(
    base: &str,
    path: &str,
    creds: &PolymarketCreds,
    body: &serde_json::Value,
    extra: &[(String, String)],
) -> Result<serde_json::Value, String> {
    let payload = serde_json::to_string(body).map_err(|e| e.to_string())?;
    let headers =
        l2_auth_headers(creds, now_secs(), "POST", path, &payload).map_err(|e| e.to_string())?;
    let ag = agent();
    let mut req = ag.post(&format!("{base}{path}")).header("Content-Type", "application/json");
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    for (k, v) in extra {
        req = req.header(k.as_str(), v.as_str());
    }
    let mut resp = req.send(payload.as_bytes()).map_err(|e| format!("network: {e}"))?;
    log_rate_signal(resp.headers());
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("bad json ({status}): {e}: {text}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("polymarket {status}: {text}"));
    }
    Ok(v)
}

/// L2-signed POST returning the RAW `(http_status, parsed_body)` WITHOUT collapsing a non-2xx into
/// an `Err` — the dead-man heartbeat ([`crate::heartbeat`]) must READ a `400 {"error":"Invalid
/// Heartbeat ID","heartbeat_id":"<correct>"}` body to resync, which [`post_signed`] cannot expose.
/// Network and JSON-decode failures are still `Err`. Signs the EXACT body string sent (the L2
/// contract), over the same proxy-aware [`agent`] every signed CLOB call uses.
pub(crate) fn post_signed_raw(
    base: &str,
    path: &str,
    creds: &PolymarketCreds,
    body: &serde_json::Value,
) -> Result<(u16, serde_json::Value), String> {
    let payload = serde_json::to_string(body).map_err(|e| e.to_string())?;
    let headers =
        l2_auth_headers(creds, now_secs(), "POST", path, &payload).map_err(|e| e.to_string())?;
    let ag = agent();
    let mut req = ag.post(&format!("{base}{path}")).header("Content-Type", "application/json");
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let mut resp = req.send(payload.as_bytes()).map_err(|e| format!("network: {e}"))?;
    log_rate_signal(resp.headers());
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("bad json ({status}): {e}: {text}"))?;
    Ok((status, v))
}

/// Submit a signed order (`order_type` = "GTC"|"FOK"|"GTD"). Returns the CLOB response.
///
/// Passes the local rate budget first (`gate_submit` — observe-only unless `POLY_RATE_GATE=1`,
/// so with the flag unset this is byte-identical to a bare `post_signed`), then debits one order
/// token BEFORE the wire call, because the venue charges on receipt.
pub fn submit_order(
    base: &str,
    creds: &PolymarketCreds,
    order_obj: serde_json::Value,
    order_type: &str,
) -> Result<serde_json::Value, String> {
    gate_submit()?;
    note_submit();
    post_signed(base, "/order", creds, &submit_body(order_obj, &creds.api_key, order_type))
}

/// Submit a signed order via the deposit-wallet / relayer (gasless) flow: the standard L2 HMAC PLUS
/// the two plaintext relayer headers. Used when the account's funder is a Polymarket deposit wallet
/// (signatureType POLY_1271) — its orders route through the relayer.
///
/// Same local rate-budget gate + debit as [`submit_order`].
pub fn submit_order_relayer(
    base: &str,
    creds: &PolymarketCreds,
    order_obj: serde_json::Value,
    order_type: &str,
    relayer_key: &str,
    relayer_address: &str,
) -> Result<serde_json::Value, String> {
    gate_submit()?;
    note_submit();
    let body = submit_body(order_obj, &creds.api_key, order_type);
    let extra = relayer_headers(relayer_key, relayer_address);
    post_signed_extra(base, "/order", creds, &body, &extra)
}

/// Cancel a resting order by its CLOB order id (`DELETE /order`).
///
/// **Fires unconditionally and debits** — this is the WIRE primitive, and it deliberately makes no
/// shedding decision of its own. The reserve floor is consulted ONE step above it, by
/// [`gate_cancel_shared`] at [`crate::client`]'s `ExecCommand::Cancel` arm, which calls this
/// function only once the intent has cleared: a [`CancelIntent::Routine`] cancel can be held back
/// there, while [`CancelIntent::Unspecified`] (the flatten-safe default) and
/// [`CancelIntent::RiskOff`] always reach here — being able to cancel then is exactly what the
/// reserve is held back FOR, and a 429 on a cancel beats not trying.
///
/// The split is deliberate: gating INSIDE this function would have to decide-and-debit in one
/// place with no way to report the shed to the caller, and the caller is the one that owes the
/// order a non-terminal `OrderCancelRejected`.
pub fn cancel_order(
    base: &str,
    creds: &PolymarketCreds,
    order_id: &str,
) -> Result<serde_json::Value, String> {
    note_cancel();
    del_signed(base, "/order", creds, &cancel_body(order_id), &[])
}

/// Cancel via the deposit-wallet / relayer flow (adds the relayer headers). Same unconditional
/// fire-and-debit contract as [`cancel_order`].
pub fn cancel_order_relayer(
    base: &str,
    creds: &PolymarketCreds,
    order_id: &str,
    relayer_key: &str,
    relayer_address: &str,
) -> Result<serde_json::Value, String> {
    note_cancel();
    let extra = relayer_headers(relayer_key, relayer_address);
    del_signed(base, "/order", creds, &cancel_body(order_id), &extra)
}

// --- A3 resync: L2-authenticated history reads -------------------------------------------------

fn trades_query(limit: u32) -> String {
    format!("limit={limit}")
}
fn orders_query(limit: u32) -> String {
    format!("limit={limit}")
}

/// L2-signed proxy-aware GET (the DELETE/POST twins already exist). Signs the method+path with an
/// empty body per the CLOB L2 scheme. Used for the audit-A3 resync history reads.
pub fn get_signed(
    base: &str,
    path: &str,
    query: &str,
    creds: &PolymarketCreds,
) -> Result<serde_json::Value, String> {
    let headers = l2_auth_headers(creds, now_secs(), "GET", path, "").map_err(|e| e.to_string())?;
    let url =
        if query.is_empty() { format!("{base}{path}") } else { format!("{base}{path}?{query}") };
    let ag = agent();
    let mut req = ag.get(&url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }
    let mut resp = req.call().map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("network: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("bad json ({status}): {e}: {text}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("polymarket {status}: {text}"));
    }
    Ok(v)
}

/// Recent account trades for the A3 replay (`GET /data/trades`).
pub fn get_trades(
    base: &str,
    creds: &PolymarketCreds,
    limit: u32,
) -> Result<serde_json::Value, String> {
    get_signed(base, "/data/trades", &trades_query(limit), creds)
}

/// Recent account orders for the A3 replay (`GET /data/orders`).
pub fn get_orders(
    base: &str,
    creds: &PolymarketCreds,
    limit: u32,
) -> Result<serde_json::Value, String> {
    get_signed(base, "/data/orders", &orders_query(limit), creds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_query_shapes() {
        assert_eq!(trades_query(50), "limit=50");
        assert_eq!(orders_query(25), "limit=25");
    }

    #[test]
    fn bodies() {
        let ob = serde_json::json!({"salt":"1","side":"BUY"});
        let s = submit_body(ob, "api-key-1", "GTC");
        assert_eq!(s["owner"], "api-key-1");
        assert_eq!(s["orderType"], "GTC");
        assert_eq!(s["order"]["side"], "BUY");
        assert_eq!(cancel_body("0xdeadbeef")["orderID"], "0xdeadbeef");
    }

    // --- the client-side rate-budget mirror ---------------------------------------------------
    //
    // `crate::rate_budget` is a pure deterministic mirror (time is injected as `now_ms`, it calls
    // no `Instant::now()` itself), so every decision this module makes from it is driven here with
    // an explicit clock and an explicitly-constructed budget — no sleeping, no network, no
    // process-env mutation (`set_var` is unsound under threads — the repo-wide rule).
    //
    // Standard tier throughout: capacity 40 tokens per bucket, reserve 25% = 10.

    /// Capability 1, and the CONTRACT of this PR: with `POLY_RATE_GATE` unset the gate can only
    /// ever return `Ok`, so an over-budget submit still reaches the wire exactly as it does today.
    /// Turning the flag on converts the same verdict into a named refusal — which the exec thread
    /// maps to an `OrderRejected`, so no order silently vanishes either way.
    #[test]
    fn the_submit_gate_is_inert_while_the_flag_is_unset() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        // A healthy bucket allows in both modes.
        assert_eq!(submit_gate_action(rb.submit_decision(0), false), Ok(()));
        assert_eq!(submit_gate_action(rb.submit_decision(0), true), Ok(()));

        let before = rate_gate_would_block_count();
        for _ in 0..40 {
            rb.on_submit(0); // drain the order bucket
        }
        let dry = rb.submit_decision(0);
        assert!(matches!(dry, SubmitDecision::Throttle { .. }), "expected a dry bucket: {dry:?}");

        // OBSERVE-ONLY (the default): logged + counted, but the order still goes out.
        assert_eq!(submit_gate_action(dry, false), Ok(()));
        // ENFORCED (`POLY_RATE_GATE=1`): a refusal naming the real cause.
        let refused = submit_gate_action(dry, true).expect_err("enforced mode must refuse");
        assert!(refused.contains("rate budget"), "the reason must name the cause: {refused}");
        // Both modes produce the evidence for deciding to enforce.
        assert!(rate_gate_would_block_count() >= before + 2);
    }

    /// The enforcement flag is OFF by default and accepts the EXACT string `"1"` only — the
    /// `POLY_EXEC` / `POLY_RECONCILE` idiom, including the `.env` tier's trailing-comment tolerance.
    #[test]
    fn the_rate_gate_flag_is_off_by_default_and_exact() {
        assert!(!rate_gate_enforced_from(None, None));
        assert!(rate_gate_enforced_from(Some("1"), None));
        assert!(rate_gate_enforced_from(None, Some("1")));
        assert!(rate_gate_enforced_from(None, Some(" 1 ")));
        assert!(rate_gate_enforced_from(None, Some("1  # enforce once the headers agree")));
        for off in ["0", "true", "yes", "on", "", "11", "1x"] {
            assert!(!rate_gate_enforced_from(Some(off), None), "process env {off:?}");
            assert!(!rate_gate_enforced_from(None, Some(off)), "workspace .env {off:?}");
        }
    }

    /// Capability 3: `cancel-all` is taken only while its `1 + n` debit provably cannot overshoot.
    /// The squeezed case is the exact hazard the deep-dive flagged — a bulk call that would drive
    /// the cancel bucket NEGATIVE and lock out every subsequent cancel.
    #[test]
    fn a_cancel_all_that_would_overshoot_picks_targeted_instead() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        // Wide open: 1 + 10 tokens is affordable AND leaves the reserve intact → one bulk call.
        let fat = plan_cancels(&rb, 10, CancelScope::WholeBook, CancelIntent::Routine, 0);
        assert_eq!(fat.strategy, CancelStrategy::CancelAll);
        assert_eq!((fat.send, fat.deferred), (10, 0));

        // Snap the cancel bucket to 12 the way a venue header would, then ask for 15:
        // 1 + 15 = 16 > 12, so `cancel-all` would go negative → targeted.
        rb.reconcile(&RateLimitSignal { cancel_remaining: Some(12.0), ..Default::default() }, 0);
        let squeezed = plan_cancels(&rb, 15, CancelScope::WholeBook, CancelIntent::Routine, 0);
        assert_eq!(squeezed.strategy, CancelStrategy::Targeted);
        // …and the reserve floor (10) caps this routine pass at the 2 tokens sitting above it.
        assert_eq!((squeezed.send, squeezed.deferred), (2, 13));
        assert!(squeezed.retry_ms > 0, "a deferred remainder must carry a refill ETA");

        // A risk-off flatten of the same book ignores the reserve and sends every id.
        let flatten = plan_cancels(&rb, 15, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
        assert_eq!(flatten.strategy, CancelStrategy::Targeted);
        assert_eq!((flatten.send, flatten.deferred, flatten.retry_ms), (15, 0, 0));
    }

    /// The case the `ExecCommand::CancelBatch` seam exists FOR, and the one the earlier tests
    /// never reach: the SAME whole-book batch is held back under `Routine` and goes out as ONE
    /// account-wide `cancel-all` under `RiskOff`. That is the trade an emergency wants — `1 + n`
    /// tokens instead of `n`, but one round trip instead of `n` — and it is only affordable
    /// because the reserve refused the routine churn that would otherwise have spent it.
    #[test]
    fn a_riskoff_batch_takes_the_bulk_arm_the_reserve_holds_back_from_routine() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        // Snap the cancel bucket to 12 the way a venue header would. `1 + 6 = 7` is affordable
        // outright, so the venue guard clears the bulk arm — the ONLY thing left deciding is which
        // intent may spend down through the reserve floor (10).
        rb.reconcile(&RateLimitSignal { cancel_remaining: Some(12.0), ..Default::default() }, 0);

        let routine = plan_cancels(&rb, 6, CancelScope::WholeBook, CancelIntent::Routine, 0);
        assert_eq!(routine.strategy, CancelStrategy::Targeted, "churn may not spend the reserve");
        assert_eq!((routine.send, routine.deferred), (2, 4), "only the 2 tokens above the floor");
        assert!(routine.retry_ms > 0, "a deferred remainder must carry a refill ETA");

        let flatten = plan_cancels(&rb, 6, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
        assert_eq!(flatten.strategy, CancelStrategy::CancelAll, "ONE round trip, not six");
        assert_eq!((flatten.send, flatten.deferred, flatten.retry_ms), (6, 0, 0));
    }

    /// Capability 2, the reserve invariant: routine churn stops with the flatten reserve intact,
    /// and a flatten issued right after it still fires every id.
    #[test]
    fn the_reserve_floor_holds_under_routine_churn() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        let plan = plan_cancels(&rb, 40, CancelScope::WholeBook, CancelIntent::Routine, 0);
        // 1 + 40 = 41 > 40 tokens, so the bulk arm is out on the venue guard alone…
        assert_eq!(plan.strategy, CancelStrategy::Targeted);
        // …and the reserve caps the churn at 30 of the 40 ids.
        assert_eq!((plan.send, plan.deferred), (30, 10));
        assert!(plan.retry_ms > 0);

        // Spending exactly what the plan authorises leaves the reserve untouched.
        for _ in 0..plan.send {
            rb.on_cancel(0);
        }
        let left = rb.cancel_available(0);
        assert!(left >= 10.0 - 1e-9, "routine churn breached the reserve: {left}");

        // Which is the whole point: the flatten that follows is NOT rate-limit-locked.
        let flatten = plan_cancels(&rb, 10, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
        assert_eq!((flatten.send, flatten.deferred), (10, 0));
    }

    /// Capability 4, driven exactly as [`log_rate_signal`] drives it: the venue's own header pairs
    /// → `parse_headers` → `fold_rate_signal`. A drifted bucket snaps to the server's number; a
    /// response carrying no rate headers at all leaves the mirror untouched.
    #[test]
    fn a_header_reconcile_snaps_a_drifted_bucket() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        for _ in 0..40 {
            rb.on_submit(0);
        }
        assert!(rb.order_available(0) < 1.0, "the bucket must be drained first");

        let sig = crate::rate_budget::parse_headers([
            ("Poly-RateLimit-Warning", "1"),
            ("poly-ratelimit-order-remaining", "25"),
            ("poly-ratelimit-cancel-remaining", "7"),
        ]);
        assert!(fold_rate_signal(&mut rb, &sig, 0), "an actionable signal must be applied");
        assert!((rb.order_available(0) - 25.0).abs() < 1e-9);
        assert!((rb.cancel_available(0) - 7.0).abs() < 1e-9);

        let quiet = crate::rate_budget::parse_headers([("content-type", "application/json")]);
        assert!(!fold_rate_signal(&mut rb, &quiet, 0), "a bare response must change nothing");
        assert!((rb.order_available(0) - 25.0).abs() < 1e-9);
    }

    /// `DELETE /cancel-all` is ACCOUNT-WIDE, so a subset request may never take it however much
    /// budget is free — and an empty request is a no-op plan, never a bulk call.
    #[test]
    fn a_subset_never_takes_the_account_wide_bulk_arm() {
        let rb = RateBudget::new(Tier::Standard, 0);
        let subset = plan_cancels(&rb, 5, CancelScope::Subset, CancelIntent::Routine, 0);
        assert_eq!(subset.strategy, CancelStrategy::Targeted);
        assert_eq!((subset.send, subset.deferred), (5, 0));

        let empty = plan_cancels(&rb, 0, CancelScope::WholeBook, CancelIntent::RiskOff, 0);
        assert_eq!(
            empty,
            CancelPlan { strategy: CancelStrategy::Targeted, send: 0, deferred: 0, retry_ms: 0 }
        );
    }

    /// A deferred routine pass is a DELAY, not a deadlock: the reported ETA really does unblock
    /// churn again, and a fully refilled bucket clears the whole batch in one bulk call.
    #[test]
    fn a_deferred_routine_pass_recovers_after_the_reported_refill() {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        rb.reconcile(&RateLimitSignal { cancel_remaining: Some(10.0), ..Default::default() }, 0);
        let stalled = plan_cancels(&rb, 4, CancelScope::WholeBook, CancelIntent::Routine, 0);
        assert_eq!((stalled.send, stalled.deferred), (0, 4), "sitting exactly on the floor");
        assert!(stalled.retry_ms > 0);

        // The ETA is "one token above the reserve again", so it unblocks the pass, not the batch.
        let after =
            plan_cancels(&rb, 4, CancelScope::WholeBook, CancelIntent::Routine, stalled.retry_ms);
        assert!(after.send > 0, "the reported ETA must actually unblock churn: {after:?}");

        // A full second of refill (Standard = 40 tokens/s) restores the cheap bulk path.
        let rested = plan_cancels(&rb, 4, CancelScope::WholeBook, CancelIntent::Routine, 1_000);
        assert_eq!(rested.strategy, CancelStrategy::CancelAll);
        assert_eq!((rested.send, rested.deferred), (4, 0));
    }

    /// A cancel budget sitting EXACTLY on the reserve floor: Standard tier is a 40-token bucket
    /// with a 25% reserve, so 10 remaining is the boundary the per-order gate must act on.
    fn budget_at_the_reserve_floor() -> RateBudget {
        let mut rb = RateBudget::new(Tier::Standard, 0);
        rb.reconcile(&RateLimitSignal { cancel_remaining: Some(10.0), ..Default::default() }, 0);
        rb
    }

    /// THE POINT OF THE WHOLE CHANGE: at the reserve floor a ROUTINE cancel is held back, and the
    /// refusal says when to retry. Above the floor it fires like any other cancel.
    #[test]
    fn the_reserve_holds_back_a_routine_cancel() {
        let mut rb = budget_at_the_reserve_floor();
        let shed = gate_cancel(&mut rb, CancelIntent::Routine, 0)
            .expect_err("a routine cancel at the reserve floor must be shed");
        assert!(shed.contains("reserve"), "the refusal must name why: {shed}");
        assert!(shed.contains("retry"), "…and when to retry: {shed}");

        // Standard refills 40 tokens/s, so a quarter second is comfortably back above the floor.
        assert!(
            gate_cancel(&mut rb, CancelIntent::Routine, 250).is_ok(),
            "above the floor, routine churn is ordinary"
        );
    }

    /// …and the reserve exists so that THIS one still fires. Same budget, same instant, drained
    /// far past the floor and even NEGATIVE (the `cancel-all` over-debit model): an emergency
    /// cancel is never held back, which is what the held-back tokens were being held FOR.
    #[test]
    fn the_reserve_never_holds_back_an_emergency_cancel() {
        let mut rb = budget_at_the_reserve_floor();
        assert!(gate_cancel(&mut rb, CancelIntent::RiskOff, 0).is_ok());

        rb.on_cancel_all(60, 0); // drive the bucket negative
        assert!(rb.cancel_available(0) < 0.0, "fixture: the bucket must be negative");
        assert!(
            gate_cancel(&mut rb, CancelIntent::RiskOff, 0).is_ok(),
            "a flatten fires even from a locked-out bucket — a 429 beats not trying"
        );
    }

    /// The backward-compatibility claim, as a test: a cancel that named no intent behaves EXACTLY
    /// as this venue behaved before the intent existed — it fires, and the gate neither reads nor
    /// moves the bucket on its way through.
    #[test]
    fn an_unspecified_cancel_behaves_exactly_as_today() {
        let mut rb = budget_at_the_reserve_floor();
        let before = rb.cancel_available(0);
        assert!(
            gate_cancel(&mut rb, CancelIntent::Unspecified, 0).is_ok(),
            "an unclassified cancel is treated as the flatten, never shed"
        );
        assert!(
            (rb.cancel_available(0) - before).abs() < 1e-9,
            "the gate must not debit — `cancel_order` owns the debit, and only for what it sends"
        );

        rb.on_cancel_all(60, 0); // negative bucket: still not this gate's business
        assert!(gate_cancel(&mut rb, CancelIntent::Unspecified, 0).is_ok());
    }

    /// The relayer header pair is now built in ONE place ([`relayer_headers`]) for submit, cancel
    /// and the bulk cancel — pin its exact wire names, since the deposit-wallet flow is rejected
    /// outright if either is misspelled.
    #[test]
    fn relayer_headers_keep_the_exact_wire_names() {
        assert_eq!(
            relayer_headers("rk", "0xaddr"),
            vec![
                ("RELAYER_API_KEY".to_string(), "rk".to_string()),
                ("RELAYER_API_KEY_ADDRESS".to_string(), "0xaddr".to_string()),
            ]
        );
    }
}
