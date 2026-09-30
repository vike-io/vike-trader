//! Who is armed, for what, and at what fee/margin/policy — the per-account/per-venue arming
//! decisions `make_engine_for_account`'s match consults. Excludes `make_engine*` themselves and
//! the per-venue match, which stay at the crate root (see this crate's module doc).

use std::collections::HashMap;

use crate::{
    ARMED_MAX_ORDERS_PER_WINDOW, AccountLabel, CexCredChoice, MountError, MountPolicy, WithdrawGate,
};

/// Arm the operator-budget fields that have a UNIVERSALLY-safe default — the Nautilus half of
/// Task 6's two-kind split (see the module-level comment above `ARMED_MAX_ORDERS_PER_WINDOW`).
/// Called from [`crate::make_engine`] at the SAME site as, and immediately before, the pre-existing
/// `im_requirement` rescue — AFTER [`merge_operator_budget`], never before: that fn takes
/// `max_orders_per_window`/`window_ms` UNCONDITIONALLY from any profile threaded in, so a profile
/// that never mentions them carries their serde-default `None` (`ProfileRisk::default()`'s shape)
/// — rescuing first would let that `None` silently win the instant ANY profile is merged, even one
/// that only sets, say, `max_notional_per_order`. That is the exact `im_requirement`/`window_ms`
/// divergent-default hazard this task's own brief calls out, reproduced for another field had the
/// order been wrong here too.
///
/// `max_leverage` is NO LONGER rescued here (issue #822 removed #817's inert `Some(1.0)`) — it is
/// the operator-facing name for the leverage cap and converts to `im_requirement` at the config
/// edge, so the `im_requirement` rescue below the call site is the single place "no leverage
/// unless asked" is armed. Unset leverage still means 1×; it is just armed once, not twice.
///
/// `required_free_bp_pct` — the THIRD universally-defaultable field per the plan's table — needs
/// NO rescue in this fn: unlike the other two it is a plain `f64` (not `Option`), already `0.0`
/// from `RiskLimits::new()`/`Default` on every path that reaches this call (no venue fetch or
/// profile merge ever leaves it at anything else), and [`merge_operator_budget`] already threads a
/// profile's OWN value through unconditionally whenever one sets it. `0.0` is inert — no haircut on
/// free buying power — which is exactly "preserving today's behaviour while making the knob
/// reachable" per the plan: nothing here needs to change for it to already be true.
pub(crate) fn arm_universal_defaults(mut limits: vike_exec::RiskLimits) -> vike_exec::RiskLimits {
    limits.max_orders_per_window =
        limits.max_orders_per_window.or(Some(ARMED_MAX_ORDERS_PER_WINDOW));
    limits
}

/// Task 6's Freqtrade-shaped half of the split: an ACCOUNT-DEPENDENT cap has no universal safe
/// value — too large does nothing (the gate never trips), too small rejects every real order (the
/// gate is now actively harmful), and both erode operator trust in the gate more than `None` ever
/// did. So rather than guess a default, a LIVE mount REFUSES TO START unless the operator supplied
/// BOTH `max_notional_per_order` and `max_total_exposure` (from any source that reaches `limits` —
/// a `[risk]` profile today, a future per-venue default later). `Err` names EVERY missing key in
/// one message, not just the first, so a single fix cycle closes the gate.
///
/// Deliberately checked ONLY for a venue the operator intends live, from TWO call sites in
/// [`crate::make_engine`]: the PRE-CONNECT site (gated on [`would_mount_live`] — the primary, firing
/// before any venue session exists, on a preview of the profile's budget) and the post-merge
/// site (gated on `live_venues.contains(venue)` — the backstop, reachable only if a future live
/// arm is added without a probe row). A paper or backtest mount reaches neither, so it may run
/// with both caps unbounded, exactly as before this task existed.
///
/// `profile_supplied` is passed straight through to [`MountError::MissingRiskBudget`] and changes
/// only the DIAGNOSTIC, never the verdict: it is `make_engine`'s `risk_profile.is_some()`, which is
/// the one fact the message needs and the check itself cannot see (`limits` alone cannot tell "no
/// profile was supplied" from "a profile was supplied that omits these caps", and the operator's
/// next action differs — create a file versus edit the one they already have).
pub(crate) fn require_live_risk_budget(
    venue: &str,
    limits: &vike_exec::RiskLimits,
    profile_supplied: bool,
) -> Result<(), MountError> {
    let mut missing = Vec::new();
    if limits.max_notional_per_order.is_none() {
        missing.push("max_notional_per_order");
    }
    if limits.max_total_exposure.is_none() {
        missing.push("max_total_exposure");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MountError::MissingRiskBudget { venue: venue.to_string(), missing, profile_supplied })
    }
}

/// PRE-CONNECT live-intent probe: would [`crate::make_engine`]'s arm for `venue` build a LIVE exec
/// client, judged PURELY from the caller-supplied vars map — by calling the SAME config loaders
/// the arms themselves gate on, with NO network I/O and no side effects. This is what lets the
/// missing-risk-budget refusal ([`require_live_risk_budget`]) fire BEFORE any venue session is
/// established (the #817 "refusal happens POST-connect" residual; Freqtrade refuses before
/// touching a broker).
///
/// One row per live arm, each citing the arm's own gate so drift is structurally hard — a row
/// that stops matching its arm's loader fails the probe tests below, and a NEW live arm added
/// without a row here is still caught by the post-merge backstop check (then this fn gains its
/// row). Venues with NO live arm in [`crate::make_engine`] today (every unknown venue — no ROSTER venue is
/// in that class any more) probe `false`: they can only ever produce the paper `_`
/// arm, which the budget rule deliberately leaves unbounded. **fxcm was in that sentence until its
/// arm landed** and is now a feature-gated row like ibkr's, with the extra `sdk_available` conjunct
/// `fxcm_live_intent` explains.
///
/// INTENT semantics, not outcome: `true` means the operator SUPPLIED this venue's live config,
/// even where the arm would later demote to paper (ctrader/ibkr synchronous connect failure,
/// hyperliquid/polymarket factory declining a bad key). An intended-live session must carry a
/// bounded budget; a config the operator never wrote can never refuse a mount.
#[must_use]
pub fn would_mount_live(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
) -> bool {
    would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Live)
}

/// This deployment's arming ceiling for one venue — **the one place `Option<&MountPolicy>` is
/// turned into a [`vike_config::VenueMode`]**, so no caller can invent its own answer for a
/// policy-less mount.
///
/// `None` reads [`VenueMode::Paper`](vike_config::VenueMode::Paper), not `Live`: fail-safe by
/// construction, the widening mistake has to be typed. That rule was written twice — here and at
/// [`crate::make_engine_with_legs`]'s ceiling seam — for exactly as long as it took someone to add a
/// SECOND consumer ([`crate::startup::run_startup_preflight`]) and have it default the other way. Two
/// predicates that must agree is the defect shape; one function is the cure.
pub(crate) fn venue_ceiling(policy: Option<&MountPolicy>, venue: &str) -> vike_config::VenueMode {
    account_ceiling(policy, venue, &AccountLabel::Default)
}

/// **This deployment's arming ceiling for one ACCOUNT of one venue** — the per-account twin of
/// [`venue_ceiling`], and the fold that makes `policy.toml`'s `[accounts]` table bind anything.
///
/// `vike_config::VenuePolicy::account` is the resolution and carries the argument: the DEFAULT
/// account inherits its venue's line exactly, a LABELLED one with no line of its own resolves
/// `paper`, and both are `min`-capped by the venue's line. So this is a second ceiling UNDER the
/// first and, like it, can only ever REFUSE.
///
/// `None` — a caller that threads no policy — reads `Paper`, the same fail-safe [`venue_ceiling`]
/// has always had, and for the same reason: the widening mistake must have to be typed. That is why
/// [`venue_ceiling`] is now a DELEGATION to this rather than a second `map_or` beside it — two
/// predicates that must agree is the defect shape, and this file has already paid for it once
/// (`crate::startup::run_startup_preflight` defaulted the other way).
pub(crate) fn account_ceiling(
    policy: Option<&MountPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> vike_config::VenueMode {
    policy.map_or(vike_config::VenueMode::Paper, |p| p.venues.account(venue, label))
}

/// **The ROUTING key for one account** — `vike_exec::ExecutionEngine::route_key`, the
/// `LIVE-<route_key>.lock` sentinel filename, and the name an armed account appears under in
/// `live_venues`.
///
/// Rendered by `vike_model::account_keys::AccountRef::route_key` and nowhere else; the `tier` handed
/// to the ref does not reach the answer (that method carries no tier, deliberately — one process
/// mounts a venue at one tier), so the ONE spelling stays a single function whatever tier a mount
/// lands on. For `AccountLabel::Default` it is the bare venue id, which is what
/// `ExecutionEngine::new` already seeds and what every existing deployment's sentinel is named.
///
/// ⚠ `venue` is taken by value and leaked into a `&'static str`-shaped ref via
/// `vike_model::VENUES`' own row when it is a roster id, and rendered directly otherwise: a
/// non-roster venue (a test's `"sim"`, a paper id) has no `AccountRef` to build, and it can only
/// ever be a default account anyway, so it renders as itself.
pub(crate) fn account_route_key(venue: &str, label: &AccountLabel) -> String {
    let Some(id) = vike_model::VENUES.iter().copied().find(|v| *v == venue) else {
        // No roster row, so no `AccountRef`. A non-roster venue reaches this only as its own
        // default account (nothing can name a labelled account of a venue that does not exist), so
        // the bare id IS the route key — the same string `ExecutionEngine::new` seeds.
        return venue.to_string();
    };
    vike_model::account_keys::AccountRef { venue: id, tier: "LIVE", label: label.clone() }
        .route_key()
}

/// **The exec-event lane ONE account's venue adapters push into** — the caller's lane, scoped to
/// `route_key`, so an account-wide `Event::AccountState` reaches THAT account's engine.
///
/// ## Why the mount and not the bridge
///
/// `Event::AccountState` is account-WIDE and names no symbol, so it had nothing to route on at all
/// and folded into the venue's default engine: a second account's balances landed in the first
/// account's book. Every OTHER venue-tagged payload carries either a client-order-id (`Event::Fill`,
/// resolved through `vike_core`'s submit-time `coid_venue` map — exact, and needing nothing on the
/// wire) or a symbol, which `route_event` uses only when EXACTLY ONE engine of the venue claims it.
/// ⚠ That last qualifier is new: the symbol used to be an exact account key because two accounts of
/// one venue could not share one, and that refusal is gone (`vike_config::venue_accounts`).
///
/// The fix has to put the account's identity on that payload, and this is where that identity
/// exists. A bridge holds ONE credential set and emits the canonical venue id; it has no idea an
/// account label is a thing, and thirteen of them would have had to learn. This function is the
/// whole of what they are spared: [`make_engine_for_account`] shadows its `live_events` parameter
/// with the result, so every venue arm below it pushes into the scoped lane without naming it.
///
/// ## Why a default account is inert
///
/// [`account_route_key`] renders the bare venue id for `AccountLabel::Default`, and
/// `vike_exec::EventSender::routed` stamps nothing when the key equals the payload's own venue. So
/// this is called UNCONDITIONALLY — no caller decides whether an account is "the default one" —
/// and a box with no `[accounts]` table produces payloads with `route_key: None`, which
/// `skip_serializing_if` keeps off the wire entirely. Its journal bytes are unchanged.
pub(crate) fn account_event_sender(
    live_events: &vike_exec::EventSender,
    route_key: &str,
) -> vike_exec::EventSender {
    live_events.routed(route_key)
}

/// **Every account of `venue` this box knows about**, default first, then labels in order.
///
/// Two independent sources, unioned rather than intersected, because they answer different halves:
///
/// * the CREDENTIAL STORE (`vike_model::account_keys::accounts_in_store` over the caller's own vars
///   map) says which accounts EXIST — an account with keys and no policy line must still be
///   listed, because that is precisely the row whose `AccountNotNamed` block tells the operator how
///   to arm it;
/// * the `[accounts]` TABLE says which accounts the operator has stated a ceiling for — an account
///   named there with no keys must still be listed, or a typo'd label would vanish silently instead
///   of showing up as `NoCredentials`.
///
/// The DEFAULT account is always present: it is the account a single-account box has, and a venue
/// with an empty store still mounts it (as paper).
#[must_use]
pub fn known_accounts(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Vec<AccountLabel> {
    known_accounts_in(venue, &accounts_in_store(vars), policy)
}

/// The credential store's accounts, parsed ONCE.
///
/// ⚠ Hoisted out of [`known_accounts`] rather than called inside it, and the reason is a per-FRAME
/// path: the Data Manager's Venues tab calls [`venue_arming`] every frame it is open, and that walks
/// the whole roster. `accounts_in_store` re-parses every key in the store against every roster venue
/// (uppercasing each id as it goes), so calling it once per venue did that work fourteen times for
/// one answer. One scan per projection instead.
fn accounts_in_store(vars: &HashMap<String, String>) -> Vec<vike_model::account_keys::AccountRef> {
    vike_model::account_keys::accounts_in_store(vars.keys().map(String::as_str))
}

/// [`known_accounts`] over an already-parsed store — the shape [`venue_arming`] uses so its roster
/// walk parses the store once rather than once per venue.
fn known_accounts_in(
    venue: &str,
    store: &[vike_model::account_keys::AccountRef],
    policy: Option<&MountPolicy>,
) -> Vec<AccountLabel> {
    let mut labels: Vec<AccountLabel> =
        store.iter().filter(|a| a.venue == venue).map(|a| a.label.clone()).collect();
    if let Some(p) = policy {
        for (v, label, _) in p.venues.accounts() {
            if v == venue
                && let Ok(parsed) = AccountLabel::parse(label)
            {
                labels.push(parsed);
            }
        }
    }
    labels.retain(|l| !l.is_default());
    labels.sort();
    labels.dedup();
    // Default FIRST — see `make_engine_accounts`'s order contract: every caller binds `[0]` to the
    // engine it has always bound, and the labelled accounts follow in label order.
    let mut out = vec![AccountLabel::Default];
    out.extend(labels);
    out
}

/// **THE per-account arming projection for one venue** — the rows the Data Manager renders AND the
/// selection [`crate::make_engine_accounts`] mounts from. One function, so the "Effective" column cannot
/// disagree with what the mount does.
///
/// Each row is [`venue_arming_under`] evaluated at that account's own ceiling
/// ([`vike_config::VenuePolicy::account`]), with ONE account-specific block layered on top: a
/// LABELLED account with no line of its own resolves `paper` through the ceiling itself, and is
/// reported as `AccountNotNamed` rather than as the `Disarmed` a venue-level `paper` produces — the
/// two send the operator to different lines of the same file.
///
/// # ⚠ THERE IS NO SECOND, SYMBOL-SHAPED REFUSAL HERE ANY MORE
///
/// This function used to cap every account that would arm on a symbol an earlier ACTIVE account had
/// taken. That rule is DELETED: two accounts on one instrument is an ordinary spread, and refusing
/// it was the defect (`vike_config::venue_accounts`' module doc carries the correction, and
/// [`shared_books_for`] carries what replaced it — a WARNING about a shared BOOK, computed
/// alongside these rows and never folded into them). So this projection now answers exactly one
/// question per account: what would the mount reach for it. No row is ever downgraded by what
/// another row says.
///
/// ⚠ **It takes no SYMBOL at all any more, and the deletion is the point.** It used to be handed
/// the symbol each account would arm on, purely so the collision rule could compare them; with that
/// rule gone an arming row is a fact about ONE account and a symbol parameter would be a value that
/// reaches nothing — which a reader would reasonably assume changes the answer.
/// [`crate::make_engine_accounts`] still takes the per-account symbol map, because it MOUNTS each engine
/// on its own row's symbol; that is a different question from what this one answers.
///
/// ⚠ **An id `vike_model::VENUES` does not carry answers with NO rows at all**, rather than with a
/// default-account row. `vike_config::VenueArming::venue` is a `&'static str` taken from the roster
/// itself — the construction that makes a row incapable of naming a venue that does not exist — so
/// there is no row to build for a `"sim"` or a test id. [`crate::make_engine_accounts`] reads the empty
/// answer as "one account, nothing to choose between", which is exactly what such a venue has.
///
/// ⚠ **It takes the whole [`MountPolicy`], not its `venues` table**, and that widening is
/// load-bearing since 2026-09-15: the dukascopy row resolves an ACCOUNT out of
/// [`MountPolicy::accounts`] — the settings database's `account` table as the composition root read
/// it — and [`crate::make_engine_for_account`] resolves it out of the same value on the same
/// object. One snapshot, two consumers, no way for a caller to hand them different tables.
#[must_use]
pub fn venue_account_arming(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::VenueArming> {
    account_arming_in(registry, venue, vars, &accounts_in_store(vars), policy)
}

/// **The symbol ONE account of a venue is armed on**, out of the per-account map its composition
/// root derived (`vike_run::account_symbols_for`).
///
/// A label the map does not name falls back to the DEFAULT account's row — the venue's wired
/// symbol — and an empty map answers `""`, which is what a venue outside `WIRED_MARKETS` has.
/// Spelled once here because [`crate::make_engine_accounts`] mounts each engine through it and
/// `vike_run`'s own probe derives the map that feeds it; two spellings of the fallback would mount
/// an account on one symbol while the lock claimed another.
#[must_use]
pub fn symbol_for_account<'a>(
    account_symbols: &'a [(AccountLabel, String)],
    label: &AccountLabel,
) -> &'a str {
    account_symbols
        .iter()
        .find(|(l, _)| l == label)
        .or_else(|| account_symbols.iter().find(|(l, _)| l.is_default()))
        .map_or("", |(_, s)| s.as_str())
}

/// **The shared-BOOK report for one venue's already-resolved arming rows** — every pair of ACTIVE
/// accounts whose effective trading book is the same, which [`crate::make_engine_accounts`] warns about
/// and mounts anyway.
///
/// It takes the ROWS rather than re-deriving them so the report can never describe a different
/// arming from the one being mounted, and it is a separate function from
/// [`venue_account_arming`] rather than a second return value because it changes NOTHING about
/// them: the pair is a report, not an input to any decision, and a caller that never asks is not
/// silently missing a refusal.
///
/// Only rows whose book [`crate::book_identity::book_of_account`] can determine contribute an
/// `ArmedBook`; an account whose book neither the `account` table nor the credential store names
/// contributes none, so no pair forms and nothing is said. That is the "warn nothing where you
/// cannot tell" half of the rule, and it is implemented by ABSENCE rather than by a placeholder —
/// see [`vike_config::ArmedBook`].
///
/// ⚠ **It takes the whole [`MountPolicy`] for the same reason [`venue_account_arming`] does, and
/// the reason is newer than that one.** The resolution is RECORDED-first since 2026-09-19:
/// [`crate::book_identity::recorded_book`] reads `vike_secrets::Account::venue_account_id` out of
/// [`MountPolicy::accounts`] before anything is derived from the credential store. Without that
/// parameter this function could reach only the offline derivation, and the five venues whose
/// credentials name no account — binance, bybit, okx, deribit, dukascopy — could never report a
/// shared book however much the store had been told: `vike-cli secrets set-book` wrote a column
/// that changed no decision here. One snapshot, so the rows being mounted and the books being
/// compared come from the same read of the table.
#[must_use]
pub fn shared_books_for(
    registry: &'static [crate::VenueRow],
    venue: &str,
    rows: &[vike_config::VenueArming],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::SharedBook> {
    let directory = directory_of(policy);
    let books: Vec<vike_config::ArmedBook> = rows
        .iter()
        .filter_map(|r| {
            let book = crate::book_identity::book_of_account(
                registry,
                r.venue,
                &r.label,
                r.effective,
                vars,
                directory,
            )?;
            Some(vike_config::ArmedBook {
                venue: r.venue,
                label: r.label.clone(),
                book,
                mode: r.effective,
            })
        })
        .collect();
    let _ = venue;
    vike_config::shared_books(&books)
}

/// **What a shared BOOK does to the ACCOUNT-aggregate exposure ceiling, said in the warning that
/// reports the shared book** — empty when that ceiling is unarmed, so a deployment without one
/// sees the message it has always seen.
///
/// ⚠ **This exists because the ceiling MULTIPLIES on exactly this shape, and nothing else says
/// so.** `vike_exec::RiskLimits::max_account_exposure` is armed once per ENGINE, and
/// [`crate::make_engine_accounts`] mounts one engine per `(venue, AccountLabel)`; that is the same thing
/// as one wallet only while two labels really are two books at the venue. When
/// `vike_config::venue_accounts`' shared-book rule fires they are not — an agent key signing for a
/// master whose own key is also configured, or one credential set pasted under two labels — and
/// each engine then applies the whole ceiling to its own half of ONE venue ledger, so the real
/// book may carry a multiple of the number the operator wrote. That is the N×-looser defect this
/// axis was built to close, wearing the account label instead of the symbol label, and an operator
/// who is told about the shared book but not about the multiplication has been handed half a
/// finding.
///
/// It is a SENTENCE rather than a refusal for the reason the shared-book rule itself is
/// (`docs/decisions/0013-degrade-vs-refuse.md`): the pair may be exactly what the operator meant,
/// and a mount that refused would strand a venue on paper over a configuration they chose.
///
/// A pure function of the ceiling, separate from the `warn!` that emits it, precisely so it can be
/// tested — the emission itself is structurally unreachable from a test (two ACTIVE accounts need
/// a real live arm, and the mount would dial the venue on the next statement), which
/// [`crate::make_engine_accounts`]' own comment declares as a measured blind spot.
#[must_use]
pub fn shared_book_ceiling_note(max_account_exposure: Option<f64>) -> String {
    match max_account_exposure {
        // "at least", not "exactly": the report is per PAIR, so three accounts on one book produce
        // three of these lines and the true multiple is higher than any one of them states.
        Some(cap) => format!(
            " ⚠ max_account_exposure ({cap}) is enforced PER ENGINE, so these two carry it \
             SEPARATELY over the one book they share — it may hold at least {} before either \
             refuses.",
            cap * 2.0
        ),
        None => String::new(),
    }
}

/// [`venue_account_arming`] over an already-parsed store — see [`accounts_in_store`] for why the
/// parse is hoisted.
fn account_arming_in(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    store: &[vike_model::account_keys::AccountRef],
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::VenueArming> {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};

    let Some(venue) = vike_model::VENUES.iter().copied().find(|v| *v == venue) else {
        return Vec::new();
    };
    known_accounts_in(venue, store, policy)
        .into_iter()
        .map(|label| {
            let ceiling = policy.map_or(Mode::Paper, |p| p.venues.account(venue, &label));
            let (effective, block) =
                account_arming_under(registry, venue, &label, vars, ceiling, policy);
            // `Disarmed` is the venue-level answer, and for a LABELLED account with no line of its
            // own it names the wrong cause: the venue may be armed `live` and this account still
            // resolves paper, because it was never named. Reporting `Disarmed` would send the
            // operator to the `[venues]` line they already set.
            let block = if block == Block::Disarmed
                && !label.is_default()
                && policy.is_some_and(|p| p.venues.get(venue) != Mode::Paper)
            {
                Block::AccountNotNamed
            } else {
                block
            };
            vike_config::VenueArming { venue, label, ceiling, effective, block }
        })
        .collect()
}

/// Whether a ceiling permits the REAL-MONEY tier at all — [`vike_config::VenueMode::cap`] (which is
/// `min`, never `max`) asked about `Live`.
///
/// Named rather than open-coded because THREE sites need the same fold and a fourth kept being
/// added: [`crate::make_engine_with_legs`]'s `live_permitted`, [`would_mount_live_under`]'s per-row `live`
/// conjunct, and `crate::startup::authed_read_clients`' probe tier. The last one was open-coded as
/// plain [`ceiling_selects_mainnet`] with no ceiling at all, which — pre-decision-0095, when the
/// network was a `BINANCE_MAINNET=1` switch rather than the ceiling itself — is how a `demo`-capped
/// binance with that switch exported came to be sent a SIGNED MAINNET balance read at startup while
/// the mount bound the demo host.
pub(crate) fn ceiling_permits_live(ceiling: vike_config::VenueMode) -> bool {
    ceiling.cap(vike_config::VenueMode::Live) == vike_config::VenueMode::Live
}

/// [`would_mount_live_under`] over a deployment's policy rather than a bare ceiling — the spelling
/// every caller that HOLDS a policy should use, so the preflight and the mount cannot disagree
/// about which venues are armed.
///
/// It is a two-line composition on purpose. The alternative — each caller doing its own
/// `policy.map_or(…)` before calling [`would_mount_live_under`] — is the same predicate written
/// twice, and the second copy is where the `None`-means-`Live` mistake lives.
pub(crate) fn would_mount_live_under_policy(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> bool {
    // ⚠ NOT `would_mount_live_under`, which threads no policy: on dukascopy the DEFAULT account can
    // DECLINE (a labelled account holds this process's one sidecar — `crate::exclusive`'s
    // `holder`), and a preflight that authenticated for a venue the mount leaves on paper is the
    // same class of mistake as the ceiling-blind probe this function's own doc records.
    account_arming_under(
        registry,
        venue,
        &AccountLabel::Default,
        vars,
        venue_ceiling(policy, venue),
        policy,
    )
    .0 != vike_config::VenueMode::Paper
}

/// [`would_mount_live`] under a deployment's per-venue ARMING CEILING — the answer
/// [`crate::make_engine_with_legs`] itself needs, since the ceiling changes which tier the arm resolves.
///
/// ⚠ **DERIVED, not a second copy.** The rows moved into [`venue_arming_under`], which answers the
/// finer question ("which TIER, and what is holding it back") the Data Manager's arming screen
/// renders; this is that answer projected onto the coarse one the pre-connect budget refusal and
/// the preflight need. Keeping ONE row set is the whole point — the probe's value is that every row
/// mirrors its arm's own loader, and two copies taking two different return types would be exactly
/// the drift the per-row citations exist to prevent.
///
/// ONE implementation with thin wrappers above it, deliberately: the probe's whole value is that
/// every row mirrors its arm's own loader, and a second copy taking a ceiling would be the drift
/// this function's per-row citations exist to prevent. [`would_mount_live`] is that wrapper at the
/// widest ceiling — the un-capped "would these CREDENTIALS have armed it" question, which is what
/// `report_capped_to_paper` and `venue_arming_migration_message` need (they exist to say
/// whether the ceiling REFUSED something, so they must ask what the ceiling would have overruled).
///
/// ⚠ **This doc used to name [`crate::startup::run_startup_preflight`]'s clock leg and net probe as the
/// other legitimate uncapped caller** — "no policy reaches the preflight, and an over-inclusive
/// answer there costs a keyless read and arms nothing". Both halves were wrong. The policy reaches
/// the preflight now ([`would_mount_live_under_policy`], threaded from `vike_run::build_node`), and
/// the answer was never merely a keyless read: the clock leg's ig row is a CREDENTIALED read
/// (`crate::server_time`'s `ig_time` sends `X-IG-API-KEY`), and the credential leg beside it issues
/// one SIGNED `fetch_balance` per credentialed venue. An operator who caps a venue to `paper` has
/// said "do not touch this account", and the preflight was authenticating against it anyway.
///
/// `ceiling == Paper` is `false` outright and not by working through the rows: [`venue_arming_under`]
/// short-circuits to `(Paper, Disarmed)` before its venue match, and the mount returns the paper
/// client above this call anyway — so there is no arm to mirror.
///
/// "Live" here means **not paper** — a real, authenticated exec session, whichever TIER it lands on.
/// The budget rule cares about "is a venue session established on the operator's behalf", not about
/// which endpoint it dials.
///
/// ⚠ **Decision 0095 narrowed this for exactly [`would_mount_live`] (the `ceiling ==
/// vike_config::VenueMode::Live` probe), and only for binance/bybit/okx/hyperliquid.** It used to
/// read "a CEX venue with demo keys and no `{VENUE}_MAINNET` probes `true`" as the worked example —
/// true before 0095, because an armed `live` ceiling with no mainnet switch fell back to the demo
/// tier for those venues. It no longer does: those four now REFUSE to sign a mainnet host with demo
/// keys, so a `Live`-probed demo-only venue answers `Paper`, not "not paper". A caller that means
/// "does this venue have credentials for SOME tier" (as opposed to "would a maximally-permissive
/// mount reach a real session") must probe `Demo` as well —
/// `crate::paper_fallback`'s `would_mount_under_some_tier` is that caller's shape.
///
/// ⚠ **`pub`, not `pub(crate)`, and the visibility is load-bearing.** `vike_run::armed_live_venues`
/// calls this from ANOTHER CRATE to compute which venues a live sentinel must cover BEFORE the
/// mount runs — the lock that stops two processes trading one account. Narrowing it again does not
/// merely fail to compile there; it removes the only pre-mount answer that lock can be built from.
#[must_use]
pub fn would_mount_live_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
) -> bool {
    venue_arming_under(registry, venue, vars, ceiling).0 != vike_config::VenueMode::Paper
}

/// **THE per-venue arming projection: which tier would `make_engine`'s arm for `venue` actually
/// reach under `ceiling`, and what is holding it below that ceiling** — judged PURELY from the
/// caller-supplied vars map plus this binary's compiled features, with NO network I/O and no side
/// effects.
///
/// One row per live arm, each citing the arm's own gate so drift is structurally hard — a row that
/// stops matching its arm's loader fails the probe tests below, and a NEW live arm added without a
/// row here is still caught by [`crate::make_engine`]'s post-merge budget backstop (then this fn gains its
/// row). Venues with NO live arm today (every unknown venue; ⚠ this named dukascopy until its arm
/// landed on 2026-09-09, and no ROSTER venue is in that class now) answer `(Paper, NoLiveArm)`:
/// they can only ever produce the paper `_` arm, which the budget rule deliberately leaves
/// unbounded.
///
/// INTENT semantics, not outcome, exactly as [`would_mount_live`] has always had: a non-`Paper`
/// answer means the operator SUPPLIED this venue's config, even where the arm would later demote to
/// paper (ctrader/ibkr synchronous connect failure, hyperliquid/polymarket factory declining a bad
/// key). An intended-live session must carry a bounded budget; a config the operator never wrote can
/// never refuse a mount. The arming SCREEN inherits that: it reports what the mount will attempt,
/// which is the honest answer before a mount has been attempted.
///
/// `ceiling == Paper` answers `(Paper, Disarmed)` outright and not by working through the rows: the
/// mount returns the paper client above this call, so there is no arm to mirror.
///
/// ⚠ **It threads NO policy, so it answers for a deployment with no `[accounts]` table** — which is
/// exactly what a caller holding only a ceiling has. The one row that reads more than the ceiling is
/// dukascopy's, and with no policy it resolves the DEFAULT account against an unread store: the
/// answer that venue has always given, and the one [`crate::make_engine_with_legs`] (this function's
/// single-account sibling) produces. [`would_mount_live_under_policy`] is the spelling for a caller
/// that HOLDS a policy, and it is exact.
pub(crate) fn venue_arming_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    account_arming_under(registry, venue, &AccountLabel::Default, vars, ceiling, None)
}

/// **Can this venue's mount arm address a SECOND account at all?**
///
/// A venue answers `true` here only when its `make_engine_for_account` arm threads `account` into a
/// loader that reads THAT ACCOUNT'S key names — either
/// `vike_bridge_core::credentials::load_credentials_for_account` (the four generic-credential
/// venues, which match `(venue, Some(c))` and consume the credential set this function's caller
/// resolves) or the bridge's own `*_for_account` loader, each of which composes its bespoke names
/// through `vike_bridge_core::credentials::account_var`.
///
/// ⚠ **A `true` here with an arm that still reads the UNLABELLED keys is the live-money hazard this
/// whole seam exists to prevent** — a second engine signing on the FIRST account's credentials, two
/// engines for one venue account. That is why the list is not "every venue we got around to": each
/// entry is a venue whose arm was changed AND whose loader is pinned account-aware by a test that
/// fails if the pinning stops holding
/// (`crates/vike-mount/tests/account_credential_isolation.rs`).
///
/// ⚠ Still a hand-written list, and it still cannot be derived: the fact it states is "which arm
/// threads `account`", which no reflection can see. It is kept HERE, three lines above the only
/// caller, and `vike_config::ArmingBlock::NoAccountSupport` is what an operator reads when it says
/// no.
///
/// ⚠ **EVERY `vike_model::VENUES` id is now in this list, and the last one to join was
/// `dukascopy` (2026-09-15).** That venue was refused here for as long as the seam existed, and the
/// reason given changed twice as the tree moved under it: first *"no live arm in
/// [`crate::make_engine_for_account`] at all"* (false from 2026-09-09, when the `("dukascopy", _)`
/// arm landed), then *"the `DUKASCOPY_DEMO1_*` / `DEMO2` shape bakes an account INDEX into the TIER
/// token"*. The second reason is still a true statement about the KEY NAMES and it stopped being a
/// reason to refuse, because the account is no longer read out of a name:
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` ruling 13 refused every key-grammar
/// fix and said to wait for the database — *"once the account is a COLUMN, a name carries no
/// account, no tier and no index"* — and the column, its reader and its key-name reader have all
/// landed. `vike_dukascopy::resolve_account` is what threads `account` into that arm, keyed on the
/// row rather than on any name, and it REFUSES an account it cannot identify rather than falling
/// back to the first credential set.
///
/// ⚠ **A roster with no refusals left does NOT make [`vike_config::ArmingBlock::NoAccountSupport`]
/// unreachable**, and that is why this list is still a `matches!` over named venues rather than
/// `true`. A venue scaffolded by `just new-venue` never appears here: it is scaffolded
/// contract-shaped, and its declaration's `addresses_accounts` starts `false` (the scaffold's
/// `src/mount.rs`, read through `crate::contract`'s `contract_arming`), so the FIRST thing a new
/// venue's second account gets is exactly that block. The variant is what no venue produces TODAY,
/// not one nothing can produce.
///
/// ⚠ **A refused account is SAID OUT LOUD** — [`unaddressable_accounts_message`], emitted once
/// per process from [`crate::make_engine_accounts`]. Until it existed the whole refusal was silent:
/// the row was built, [`crate::accounts_to_mount`] dropped it, and nothing on the daemon's startup
/// path named the account.
fn arm_addresses_accounts(venue: &str) -> bool {
    matches!(
        venue,
        "binance"
            | "bybit"
            | "okx"
            | "deribit"
            | "aster"
            | "hyperliquid"
            | "alpaca"
            | "ctrader"
            | "ig"
            | "oanda"
            // ⚠ dukascopy addresses an account through the settings DATABASE rather than through a
            // key name — `vike_dukascopy::resolve_account` — which is the one shape in this list
            // that is not "the loader composes `…__LABEL`". The arm reads the credential family
            // THAT ROW owns, so an account is never read as another account.
            //
            // ⚠ It closes the two-engines hazard DIFFERENTLY from the others, and the difference is
            // worth carrying: a labelled dukascopy account can legitimately resolve to the same
            // family as the DEFAULT one (address the book on the row owning `DUKASCOPY_DEMO1_*` and
            // that IS the default account, reached by its number), so "different account ⇒
            // different keys" does not hold here. What holds instead is that this venue arms at
            // most ONE account per process — `crate::exclusive`'s `holder`, because the JForex
            // sidecar is a process-wide resource — so two engines over one credential set is
            // unreachable rather than merely unlikely.
            // `crates/vike-mount/tests/dukascopy_sidecar_holder.rs` drives both halves.
            // On a box whose store cannot answer (a `Backend::Files` box, or a database older than
            // the `account` table) a labelled dukascopy account is refused at the arm and at the row
            // below, which is byte-identical to the refusal this list used to give — so nothing an
            // existing deployment does changes.
            | "dukascopy"
            // The three FEATURE-GATED venues. They answer `true` in a build with no arm too, and
            // that is correct rather than sloppy: the feature-off arm is the paper `_` one, which
            // reaches no credential of any account, so the answer this function gives changes
            // nothing there. Making it `cfg`-dependent would instead make the ARMING SCREEN report
            // a different refusal on two boxes of one deployment — the portability
            // `vike_config::venue_mode`'s module doc requires — and `ArmingBlock::FeatureAbsent`
            // is already the row that names the real cause.
            | "fxcm"
            | "ibkr"
            | "polymarket"
    )
}

/// **THE REFUSAL, SAID OUT LOUD: every account this box names on a venue whose mount addresses only
/// ONE** — one process-wide message, or `None` when there is nothing to say.
///
/// ⚠ **It exists because the refusal was SILENT, and a silent refusal of a live-trading account is
/// indistinguishable from support.** [`arm_addresses_accounts`] answers `false`,
/// [`account_arming_under`] returns [`vike_config::ArmingBlock::NoAccountSupport`], and
/// [`crate::accounts_to_mount`] then drops the row — so the account gets no engine, and
/// [`crate::make_engine_for_account`] is never reached for it, which is where every other
/// arming line in this crate is emitted ([`crate::report_capped_to_paper`],
/// [`crate::venue_arming_migration`]). MEASURED before this function existed, on the settings an
/// operator who wants two dukascopy accounts would write: the whole mount emitted three lines, none
/// of which named dukascopy at all. The row's own sentence
/// (`vike_config::VenueArming::why`) was rendered by ONE surface — the Data Manager's Venues tab —
/// and `vike-desktop` stopped feeding it labelled rows when it stopped linking a mount
/// (`vike_config::venue_arming::ceilings_only` is the DEFAULT account only), so on the daemon that
/// actually signs orders there was no surface at all.
///
/// ⚠ **A REPORT, never a refusal** (`docs/decisions/0013-degrade-vs-refuse.md`, and
/// [`crate::venue_arming_migration`] is the precedent this is shaped after): the mount continues on
/// each venue's default account. Refusing would strand a whole box over a line that already arms
/// nothing. The one place this DOES become a refusal is a strategy mount that NAMES such an account
/// — `vike_run::refuse_unarmed_mount_accounts` — because a strategy silently running on the default
/// account trades a book its author did not choose.
///
/// ⚠ **It is keyed on the BLOCK, not on a venue name.** No venue is spelled here: the message is
/// assembled from whichever rows [`venue_arming`] classifies `NoAccountSupport`, so a venue that
/// joins or leaves [`arm_addresses_accounts`] changes what this says without anything here being
/// edited. ⚠ **Today it names NOTHING**: dukascopy was the last venue in that class and joined the
/// list on 2026-09-15, so this function returns `None` on every box until a venue is added to the
/// roster without being added there — which is exactly what `just new-venue` scaffolds, and why
/// this message is kept rather than deleted with its last producer. `crates/vike-mount/tests/
/// unaddressable_account_warning.rs` proves the TEXT against a planted row for that reason.
///
/// Both ways of naming an account are covered, because the projection covers both: a
/// `policy.accounts.<venue>.<LABEL>` line, and `…__<LABEL>` keys sitting in the credential store
/// with no line at all (`vike_model::account_keys::accounts_in_store`). MEASURED: a store holding
/// `DUKASCOPY_DEMO_LOGIN__SECOND` enumerates as a real account — that family's tier token parses —
/// while the keys the mount actually reads are the venue's unlabelled ones.
///
/// `policy` `None` answers `None`: a caller that threads no policy has no `[accounts]` table and no
/// ceiling, so there is nothing it could have named.
#[must_use]
pub fn unaddressable_accounts_message(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Option<String> {
    let policy = policy?;
    // THE SAME PROJECTION THE MOUNT SELECTS WITH, so the warning cannot describe a refusal the
    // fan-out did not make — `venue_arming` walks the whole roster off ONE store parse, which is
    // what lets this be a single process-wide message rather than a per-venue line at a site the
    // refused venue never reaches.
    unaddressable_accounts_text(&venue_arming(registry, vars, policy))
}

/// **The TEXT half of [`unaddressable_accounts_message`]**, over rows the caller already has.
///
/// ⚠ **It is split out for the reason [`crate::accounts_to_mount`] is split out of its loop: so a
/// test can reach it at all.** Since dukascopy joined [`arm_addresses_accounts`] on 2026-09-15 no
/// roster venue produces a [`vike_config::ArmingBlock::NoAccountSupport`] row, so the projection
/// cannot manufacture one and the message's whole content became untestable through the public
/// entry point — a message nobody can produce is a message that rots unnoticed, which is precisely
/// what happens to it between now and the next venue that needs it. `crates/vike-mount/tests/
/// unaddressable_account_warning.rs` drives THIS function with a planted row.
///
/// Public so that test can call it; it takes rows and nothing else, which is also the guard —
/// nothing about a store, a credential or a venue name can be reached from here.
#[must_use]
pub fn unaddressable_accounts_text(rows: &[vike_config::VenueArming]) -> Option<String> {
    let refused: Vec<&vike_config::VenueArming> =
        rows.iter().filter(|row| row.block == vike_config::ArmingBlock::NoAccountSupport).collect();
    if refused.is_empty() {
        return None;
    }
    let mut out = String::from(
        "A SECOND ACCOUNT IS NAMED ON A VENUE THAT ADDRESSES ONLY ONE. It arms nothing, and no \
         settings edit can change that — the venue's mount reads one account's credentials and \
         has no way to be told which:\n\n",
    );
    for row in &refused {
        let venue = row.venue;
        let label = &row.label;
        out.push_str(&format!(
            "  {venue} account `{label}` — REFUSED. {venue} addresses exactly ONE account: its \
             DEFAULT one. No `{venue}#{label}` engine is built, no `…__{label}` credential key is \
             read by anything, and every {venue} order this process sends is signed with {venue}'s \
             UNLABELLED credential keys — the account a single-account box has always traded. \
             `{}` is that account's ceiling line, and it can only ever refuse.\n",
            row.key()
        ));
    }
    out.push_str(
        "\nThis is a report, not a refusal: the mount continues on each venue's DEFAULT account. \
         To trade the second account, give it its OWN project folder — its own settings directory \
         (VIKE_SETTINGS_DIR) and its own credential store — and run a second process there. \
         `vike-cli secrets list` names the keys the default account is actually read from, and \
         `vike-cli config show --filter policy` prints what the binaries will read.\n",
    );
    Some(out)
}

/// The `Once` latch over [`unaddressable_accounts_message`]: [`crate::make_engine_accounts`] runs
/// once per VENUE, and this text names every refused account at once.
///
/// Same idiom, same site and the same reason as [`crate::venue_arming_migration`] — the fact is
/// process-wide, the site that notices it is per-venue, and repeating it once per wired venue would
/// bury it. It is emitted from the FAN-OUT rather than from
/// [`crate::make_engine_for_account`] deliberately: a refused account never reaches the per-account
/// path (that is the refusal), and the refused VENUE may not be mounted at all — dukascopy, the
/// venue this was written for, has no `vike_run::WIRED_MARKETS` row, so a line emitted from its own
/// mount would be a line nobody ever reads. (That venue is no longer refused; the placement argument
/// is unchanged, because the next venue in that class is a scaffolded one with no wired market
/// either.)
pub(crate) fn report_unaddressable_accounts(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(message) = unaddressable_accounts_message(registry, vars, policy) {
            tracing::warn!("{message}");
        }
    });
}

/// [`venue_arming_under`] for one named ACCOUNT — the projection [`venue_account_arming`] builds its
/// rows from, and the one [`crate::make_engine_accounts`] selects with.
///
/// ⚠ **A LABELLED account is refused outright on any venue [`arm_addresses_accounts`] says no to**,
/// before any row is consulted — see that function for the current set and the argument. Every row
/// below that CAN address an account threads `label` into the same loader its arm calls, so the
/// projection and the mount cannot disagree about which account's keys were read; a row that probed
/// the default account for a labelled mount would answer with the wrong keys in both directions.
pub(crate) fn account_arming_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
    policy: Option<&MountPolicy>,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    let raw = account_arming_raw(registry, venue, label, vars, ceiling, policy);
    // THE ONE-SIDECAR OVERRIDE, applied ABOVE every row and only where a venue has one. It is not
    // folded into the venue's row itself because it is not a fact about that account's credentials
    // or its `account` row: it is a fact about which OTHER account this process is about to give
    // its single process-exclusive resource (dukascopy's JForex sidecar) to.
    // `crate::make_engine_for_account` consults the SAME function, so the projection and the mount
    // cannot disagree — which they did until 2026-09-15, when the projection answered `Demo` for
    // both accounts of a two-account box and the mount then built a paper engine for one of them. A
    // strategy that named it passed `vike_run::refuse_unarmed_mount_accounts` and ran on paper
    // believing it was armed.
    if raw.0 != vike_config::VenueMode::Paper
        && crate::contract::is_process_exclusive(crate::row_of(registry, venue), venue)
        && !crate::exclusive::holds(registry, venue, label, vars, policy)
    {
        return (vike_config::VenueMode::Paper, vike_config::ArmingBlock::SidecarHeldElsewhere);
    }
    raw
}

/// **The `account` table a caller's policy carries**, or the UNREAD one when it threads no policy —
/// the ONE spelling of that fallback, so the mount and the projection cannot pick different
/// defaults.
///
/// A `match` rather than `map_or_else(AccountDirectory::unread_ref, …)`: the latter unifies the two
/// arms at `'static` (the `unread_ref` arm's own lifetime) and then demands the borrowed policy
/// outlive it, which is a borrow error rather than a longer reference.
pub(crate) fn directory_of(
    policy: Option<&MountPolicy>,
) -> &vike_bridge_core::account_directory::AccountDirectory {
    match policy {
        Some(p) => &p.accounts,
        None => vike_bridge_core::account_directory::AccountDirectory::unread_ref(),
    }
}

/// [`account_arming_under`] WITHOUT the one-exclusive-resource override — every row, judged on its
/// own credentials, ceiling and `account` row: a contract row through its bridge's `resolve`, a
/// `Legacy` one through [`legacy_arming_raw`].
///
/// Split out so `crate::exclusive`'s `holder` can ask "would this account arm on its own merits"
/// from inside the override without asking the override again. The two are one function to every
/// caller; only the holder computation reaches this one.
pub(crate) fn account_arming_raw(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
    policy: Option<&MountPolicy>,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => {
            crate::contract::contract_arming(*row, label, vars, ceiling, policy)
        }
        // The legacy feature-off rows answered exactly this: `arm_addresses_accounts` carried
        // the three optional venues, so a labelled account reached the ceiling check too.
        Some(crate::VenueRow::FeatureAbsent { .. }) => {
            if ceiling == Mode::Paper {
                (Mode::Paper, Block::Disarmed)
            } else {
                (Mode::Paper, Block::FeatureAbsent)
            }
        }
        Some(crate::VenueRow::Legacy(_)) | None => {
            legacy_arming_raw(venue, label, vars, ceiling, policy)
        }
    }
}

/// ⚠ TRANSITIONAL: the legacy arms' arming probe — one row per venue whose legacy arm still exists,
/// each citing the arm it mirrors. A venue's row leaves as its port moves the probe into its
/// bridge's `resolve`.
fn legacy_arming_raw(
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
    policy: Option<&MountPolicy>,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    use vike_bridge_core::credentials::{Environment, load_credentials_for_account};
    use vike_config::ArmingBlock as Block;
    use vike_config::VenueMode as Mode;

    // ⚠ FIRST, before the ceiling: a labelled account of a venue whose arm cannot address one is
    // refused whatever its line says, and saying `Disarmed` (or, after the reclassification one
    // level up, `AccountNotNamed`) would send the operator to write a line that can never work.
    if !label.is_default() && !arm_addresses_accounts(venue) {
        return (Mode::Paper, Block::NoAccountSupport);
    }
    // A disarmed venue has no live arm to probe — the mount never reaches one.
    if ceiling == Mode::Paper {
        return (Mode::Paper, Block::Disarmed);
    }
    // Whether the ceiling permits the REAL-MONEY tier at all — the SAME fold `make_engine_with_legs`
    // applies (`VenueMode::cap`, which is `min`), so this probe cannot disagree with the mount about
    // which tier an arm will reach. Threaded into exactly the rows whose arm picks its tier from
    // something other than a hardcoded `Environment::Demo` — the three CEX venues and hyperliquid
    // (whose network IS the ceiling — decision 0095), aster (Live-first with no flag at all) and
    // polymarket (one tier, real money). Every other row resolves DEMO unconditionally and is
    // unaffected by a `demo` ceiling.
    let live = ceiling_permits_live(ceiling);
    // The block a venue whose arm hardcodes DEMO reports when the ceiling asked for more. Below a
    // `live` ceiling the demo tier IS the ceiling, so nothing is being refused.
    let demo_under = |block: Block| (Mode::Demo, if live { block } else { Block::None });
    match venue {
        // The three CEX venues whose network is their ceiling (decision 0095): the tier the mount
        // signs with IS the ceiling's, and a `live` ceiling with no LIVE key set is paper — a
        // mainnet host is never signed with demo keys (`cex_cred_choice`'s `MainnetNoCreds`).
        "bybit" | "okx" | "binance" => {
            let tier = if live { Environment::Live } else { Environment::Demo };
            match (load_credentials_for_account(venue, tier, label, vars).is_some(), live) {
                (true, true) => (Mode::Live, Block::None),
                (true, false) => (Mode::Demo, Block::None),
                (false, true) => (Mode::Paper, Block::LiveCredentialsAbsent),
                (false, false) => (Mode::Paper, Block::NoCredentials),
            }
        }
        // deribit: the shared CEX arm, testnet-only, so it always resolves its DEMO key set.
        "deribit" => {
            if load_credentials_for_account(venue, Environment::Demo, label, vars).is_none() {
                (Mode::Paper, Block::NoCredentials)
            } else {
                demo_under(Block::DemoOnlyArm)
            }
        }
        // `("aster", _)`: agent-wallet creds, LIVE (mainnet) preferred then TESTNET — the SAME
        // chain the arm itself calls (`vike_aster::signing::mountable_tier_for_account`, decision
        // 0088 B1), INCLUDING the ceiling that deletes the Live attempt below `live`. This re-derives
        // a `(Mode, Block)` from that one resolution rather than re-spelling the two credential
        // loads.
        "aster" => match vike_aster::signing::mountable_tier_for_account(label, vars, live) {
            Some((Environment::Live, _)) => (Mode::Live, Block::None),
            Some(_) => {
                // What selects the tier here is WHICH key set exists, so a live ceiling with only
                // testnet keys is a credential fact, never a flag fact.
                (Mode::Demo, if live { Block::LiveCredentialsAbsent } else { Block::None })
            }
            None => (Mode::Paper, Block::NoCredentials),
        },
        // `("hyperliquid", _)`: the network is the ceiling (`Env::for_ceiling`, decision 0095), the
        // same mapping the mount arm hands the bridge, so the tier below is the tier it dials.
        "hyperliquid" => match (crate::hyperliquid::would_mount_live(vars, label, live), live) {
            (true, true) => (Mode::Live, Block::None),
            (true, false) => (Mode::Demo, Block::None),
            (false, true) => (Mode::Paper, Block::LiveCredentialsAbsent),
            (false, false) => (Mode::Paper, Block::NoCredentials),
        },
        // `("ctrader", _)`: the arm self-gates on `CtraderConfig::from_vars(Demo, …)`.
        "ctrader" => {
            if vike_ctrader::config::CtraderConfig::from_vars_for_account(
                Environment::Demo,
                label,
                vars,
            )
            .is_some()
            {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("alpaca", _)`: the arm self-gates on `load_alpaca_config_from(Demo, …)`.
        "alpaca" => {
            if vike_alpaca::load_alpaca_config_for_account(Environment::Demo, label, vars).is_some()
            {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("ig", _)`: the arm self-gates on `load_ig_config_from(Demo, …)`.
        "ig" => {
            if vike_ig::load_ig_config_for_account(Environment::Demo, label, vars).is_some() {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("oanda", _)`: the arm self-gates on `vike_oanda::mountable_tier`, and the ONLY variant
        // that mounts live is `Practice`. A live-armed store answers PAPER on purpose — that arm
        // refuses and lands on paper, so reporting a session here would raise a budget refusal
        // over a venue that can only be paper (the same reason the fxcm row answers paper below).
        "oanda" => {
            if matches!(
                vike_oanda::mountable_tier_for_account(label, vars),
                vike_oanda::MountableTier::Practice(_)
            ) {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("ibkr", _)` exists only under the `ibkr` feature; feature off ⇒ paper `_` arm.
        #[cfg(feature = "ibkr")]
        "ibkr" => {
            if vike_ibkr::config::load_ibkr_config_for_account(Environment::Demo, label, vars)
                .is_some()
            {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("polymarket", _)` exists only under the `polymarket` feature. A live session needs BOTH
        // halves the factory itself gates on before any network: the operator's explicit
        // `flags.poly_exec` AND key material present (`live_mount_from_vars` → the same
        // `load_polymarket_creds_from(Live, …)`, `None` on an unset private key). The flag ALONE
        // is not intent — with no key nothing can ever mount live, and the pinned contract is
        // that it stays paper and offline. A PRESENT-but-unusable key IS intent (the same stance
        // as the hyperliquid row): the factory would decline it into paper, but a key the
        // operator wrote plus the explicit exec flag must refuse over a missing budget, not
        // silently trade paper.
        // ⚠ …plus the ceiling: this venue has no testnet, so its arm refuses anything below `live`
        // outright (see the arm), and a `demo`-capped polymarket can only be paper — reporting a
        // session for it would refuse a mount over a budget it cannot need.
        #[cfg(feature = "polymarket")]
        "polymarket" => {
            if !live {
                (Mode::Paper, Block::LiveOnlyArm)
            } else if !vike_polymarket::poly_exec_enabled(vars) {
                (Mode::Paper, Block::ExecFlagUnset)
            } else if vike_polymarket::load_polymarket_creds_for_account(
                Environment::Live,
                label,
                vars,
            )
            .is_some()
            {
                (Mode::Live, Block::None)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("fxcm", _)` exists only under the `fxcm` feature, and unlike every other row this one
        // consults a property of the BINARY as well as of the vars map — see `fxcm_live_intent`,
        // which is the pure half so both answers are testable on a box that has no SDK.
        #[cfg(feature = "fxcm")]
        "fxcm" => {
            if !vike_fxcm::sdk_available() {
                (Mode::Paper, Block::SdkAbsent)
            } else if fxcm_live_intent(true, label, vars) {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // Feature-off fxcm/ibkr/polymarket: the roster names them, this BINARY has no arm, and the
        // row says which of the two it is rather than collapsing both into "no credentials". The
        // file is allowed to name them anyway — a `policy.toml` must be portable between the boxes
        // of one deployment (`vike_config::venue_mode`'s module doc argues it).
        #[cfg(not(feature = "fxcm"))]
        "fxcm" => (Mode::Paper, Block::FeatureAbsent),
        #[cfg(not(feature = "ibkr"))]
        "ibkr" => (Mode::Paper, Block::FeatureAbsent),
        #[cfg(not(feature = "polymarket"))]
        "polymarket" => (Mode::Paper, Block::FeatureAbsent),
        // `("dukascopy", _)`: the arm self-gates on `load_dukascopy_config_from`, exactly as the
        // REST venues above self-gate on their own loaders — but on the account the MOUNT would
        // resolve, not on a hardcoded one. ⚠ It gained that arm on 2026-09-09 and had none before;
        // this row used to fall through to `NoLiveArm`, then probed `Demo1` for every account.
        //
        // ⚠ **THIS IS THE ONE ROW THAT READS THE SETTINGS DATABASE'S `account` TABLE**, and it has
        // to: which credential keys a dukascopy account arms from is a fact about an `account` ROW
        // (`crate::dukascopy`), and a projection that answered from `vars` alone would say ARMED for
        // an account the mount then refuses — a paper engine built for an account nobody armed,
        // which `crate::make_engine_accounts`' own doc calls out as the thing not to do. The two
        // therefore call ONE function over ONE snapshot, `crate::dukascopy::resolve_in` over
        // `MountPolicy::accounts`, so they cannot disagree.
        //
        // ⚠ It does NOT open the store. It used to, from this library file, at a directory taken
        // from a process global — the class `crates/vike-ops/tests/settings_registry.rs`'s
        // `CREDENTIAL_STORE_PIN` ratchets down, invisible to it because neither account reader was a
        // keyed name. The composition root reads it now, once, beside the credential map.
        //
        // Demo, never Live: the sidecar authenticates against a demo server and there is no live
        // tier wired, so a store carrying these keys arms a DEMO session — the same stance the ig
        // and oanda rows take.
        "dukascopy" => match crate::dukascopy::resolve_in(label, directory_of(policy)) {
            // The account could not be identified — the store cannot be asked, or names no such
            // row. The MOUNT stays paper and says so by name; the row says the same thing, and
            // `NoCredentials` would be the wrong sentence (the keys may well be present; what is
            // missing is the account row that says WHICH broker they are).
            Err(_) => (Mode::Paper, Block::AccountNotInStore),
            Ok(mount) => {
                if vike_dukascopy::load_dukascopy_config_from(mount.account, vars).is_some() {
                    demo_under(Block::DemoOnlyArm)
                } else {
                    (Mode::Paper, Block::NoCredentials)
                }
            }
        },
        // No live arm in ANY build: every unknown venue reaches only the paper `_` arm.
        _ => (Mode::Paper, Block::NoLiveArm),
    }
}

/// **The arming screen's whole data source**: one [`vike_config::VenueArming`] row per
/// `vike_model::VENUES` id, each pairing the operator's ceiling with what
/// [`venue_arming_under`] — and therefore [`crate::make_engine_with_legs`] — would actually do with it.
///
/// It existed so the GUI did not have to re-derive the answer. `vike-app-core` (the CI-tested
/// half of the desktop shell) could not depend on this crate: `vike-mount` was `vike-app`'s
/// OPTIONAL `fat` dependency, and a thin `--observe` build linked no bridge at all. So the row type
/// lives down in `vike-config`, beside the ceiling it carries. No desktop build calls this since
/// `fat` was deleted (2026-09-09); its callers are the daemon's — `crates/vike-tradehub/src/venues_cli.rs`
/// and the live mount — which pass `vike-tradehub`'s `REGISTRY` (a `vike-run` re-export stood
/// between them until the 2026-09-29 amendment).
///
/// No network and no clock, and no process env at all (decision 0095: every arm reads only the
/// ceiling and the vars map's key presence, never a variable).
///
/// ⚠ **This said "Pure" and it is not, since 2026-09-15**: the dukascopy row in
/// [`account_arming_under`] reads the SETTINGS STORE, because which credential keys that venue's
/// account arms from is a fact about an `account` ROW rather than about the vars map, and a
/// projection that answered without it would disagree with the mount. Bounded and argued at that
/// row; the read is a `Backend` probe on a box with no settings database, and the value is the
/// root's own declared project rather than a fresh walk, so a process that declared none gets the
/// same answer every time. **A test may still drive this from a fixture map** — an undeclared
/// process reads no store at all. The per-FRAME caller the old sentence was written for is gone:
/// `vike-desktop` links no mount and renders `vike_config::venue_arming::ceilings_only`.
/// ⚠ **One row per ACCOUNT, not per venue** — a box with no `[accounts]` table and a store holding
/// no labelled key still produces exactly one row per roster venue (the DEFAULT account's), which
/// is the table this function has always returned.
///
/// ⚠ **It no longer takes the caller's wired-market table.** That parameter existed for the
/// SYMBOL-COLLISION rule, which is deleted (`vike_config::venue_accounts`' module doc carries why),
/// and an arming row is now a fact about ONE account alone — so a symbol table here would be a
/// value that reaches no answer while looking as though it does. `vike_run::venue_arming` was the
/// wrapper that supplied it; with nothing left to supply it is a plain re-export of this function.
#[must_use]
pub fn venue_arming(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: &MountPolicy,
) -> Vec<vike_config::VenueArming> {
    // ONE store parse for the whole roster walk — see [`accounts_in_store`]. This function is called
    // per FRAME while the Data Manager's Venues tab is open.
    let store = accounts_in_store(vars);
    policy
        .venues
        .iter()
        .flat_map(|(venue, _)| account_arming_in(registry, venue, vars, &store, Some(policy)))
        .collect()
}

/// The pure half of the fxcm probe: does the operator's configuration, in a process whose ForexConnect
/// shim opens (`sdk_available`), describe a mount that will actually go LIVE?
///
/// Both conjuncts are load-bearing and they fail differently:
///
/// * **credentials** — the arm self-gates on the same `load_fxcm_config_from(Demo, …)` it calls, so
///   an unconfigured store can never raise a risk-budget refusal over a paper mount.
/// * **`sdk_available`** — an arm whose shim does not open (a STUB build's, before 2026-09-09)
///   refuses and lands on paper no matter what the store
///   says, so reporting live intent there would refuse a mount that can only ever be paper. Exactly
///   the stance the oanda row takes for a live-armed store. (⚠ This sentence used to end "and the
///   reason dukascopy probes false"; that venue has had a live arm since 2026-09-09 and probes TRUE
///   with credentials present.)
///   It is a PARAMETER rather than a `cfg!` read so both answers can be exercised, which matters
///   here more than anywhere else in this function: no CI runner has the SDK, so the `true` branch
///   is unreachable in every build any gate will ever run.
///
/// INTENT semantics, not outcome, on the remaining axis: an available SDK with a WRONG password still
/// probes live. `FxcmExecutionClient::spawn` is infallible and a failed login is indistinguishable
/// from a good one from outside (`crates/bridges/fxcm/CLAUDE.md`), so nothing here could tell them
/// apart — and an operator who wrote credentials must carry a bounded budget either way.
///
/// `label` scopes the credential half to ONE account (`FXCM_DEMO_USER__ALT` &c). The SDK half is a
/// property of the BOX (whether its shim opens) — of the BINARY until 2026-09-09 — and is therefore
/// account-blind by construction: a box with no shim refuses every account, which is the same
/// refusal a stub build always gave.
#[cfg(feature = "fxcm")]
pub(crate) fn fxcm_live_intent(
    sdk_available: bool,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    use vike_bridge_core::credentials::Environment;
    sdk_available
        && vike_fxcm::load_fxcm_config_for_account(Environment::Demo, label, vars).is_some()
}

/// **The venues whose NETWORK the ceiling chooses in the shared CEX arm** (decision 0095): under a
/// `live` ceiling they mount their LIVE key set on MAINNET, under `demo` their DEMO key set on the
/// demo hosts. Deribit shares the arm and is not here — every spawn site of its exec is testnet.
/// Hyperliquid has its own arm (`vike_hyperliquid::config::Env::for_ceiling`).
///
/// Named `cex_mainnet_enabled` before decision 0095, when it also read a `{VENUE}_MAINNET`
/// process-env/credential-map flag as a THIRD condition alongside the ceiling and the credentials —
/// this function is what that flag's deletion left: the ceiling alone, over the fixed three-venue
/// set below, with no variable left to read.
pub(crate) fn ceiling_selects_mainnet(venue: &str) -> bool {
    matches!(venue, "binance" | "bybit" | "okx")
}

/// STEP-2 of the api-key-permissions capability map (`vike_bridge_core::key_permissions`'s module
/// doc is the authority on the policy; its STEP-1 shipped the seam + the binance probe + the pure
/// gate with NO caller). Binance is the one venue with a real
/// [`KeyPermissionProbe`](vike_bridge_core::key_permissions::KeyPermissionProbe) impl, so this is
/// its live arm's pre-arm question: **may a key that can WITHDRAW arm a live venue?**
///
/// - `mainnet == false` ⇒ [`WithdrawGate::Allow`] with **NO network call at all**. The probe reads
///   `/sapi/v1/account/apiRestrictions`, and `sapi` is MAINNET-ONLY — the spot testnet does not
///   serve it, so a demo mount could only ever produce a doomed round-trip whose failure maps to
///   Unknown ⇒ Allow anyway. Skipping it keeps the demo mount byte-identical (and ~one RTT faster).
/// - A FETCH ERROR is Unknown, which ALLOWS (fail-open on introspection) — the same permissive
///   fallback shape as the `RiskLimits::from_properties` pre-fetch right above the call site. We
///   refuse on evidence, never on the absence of evidence.
/// - A KNOWN withdraw-capable key REFUSES, and the caller falls through to the paper arm: the SAME
///   outcome absent credentials produce. `flags.allow_withdraw_keys` overrides it.
///
/// Blocking: at most ONE signed GET, and only for a live MAINNET binance mount — i.e. only for the
/// exact mount whose first order would otherwise be the probe.
///
/// The override is read from `vars` alone — the map the composition root folded the RESOLVED
/// `flags.allow_withdraw_keys` into (`FoldTier::Resolved`, so a credential-store line of the same
/// name is overwritten, never OR-ed in). ⚠ The residual is the caller's: a root that hands in an
/// UNFOLDED credential map gives the store a vote it never had. `vike-tradehub` is the only
/// production root that reaches this gate, and it folds.
pub(crate) fn binance_withdraw_gate(
    mainnet: bool,
    creds: &vike_bridge_core::Credentials,
    vars: &HashMap<String, String>,
) -> WithdrawGate {
    use vike_bridge_core::key_permissions::KeyPermissionProbe;
    if !mainnet {
        return WithdrawGate::Allow;
    }
    let probe = vike_binance::key_permission_probe(creds, vike_binance::spot::MAINNET_REST);
    let fetched = probe.fetch_key_permissions();
    if let Err(e) = &fetched {
        tracing::warn!(
            venue = "binance",
            error = %e,
            "key-permission introspection failed → permissions Unknown (fail-open); the withdraw \
             gate refuses only a KNOWN withdraw-capable key"
        );
    }
    // The override is `flags.allow_withdraw_keys`, folded into `vars` by the composition root
    // (`crates/vike-tradehub/src/tradehub_cli.rs`'s `fold_flags_into_vars`). Nothing else reaches
    // it: decision 0095 retired the variable, which refuses startup now, so no shell export and no
    // unit `Environment=` line can widen this gate.
    let verdict = binance_withdraw_verdict(
        fetched,
        vike_bridge_core::key_permissions::allow_withdraw_keys(vars),
    );
    if verdict == WithdrawGate::Refuse {
        tracing::error!(
            venue = "binance",
            "⚠ REFUSING to arm binance LIVE: this API key can WITHDRAW. Falling back to the PAPER \
             client (the same outcome absent credentials produce). Re-issue a trade-only key, or \
             `vike-cli config set flags.allow_withdraw_keys true` and restart to override."
        );
    }
    verdict
}

/// The pure verdict core of [`binance_withdraw_gate`], split out so the policy is unit-testable
/// with no process env and no network: a fetched permission set folds through
/// [`vike_bridge_core::key_permissions::withdraw_gate`], and a fetch ERROR is Unknown, which never
/// refuses.
pub(crate) fn binance_withdraw_verdict(
    fetched: Result<vike_bridge_core::key_permissions::KeyPermissions, String>,
    allow_override: bool,
) -> WithdrawGate {
    use vike_bridge_core::key_permissions::{KeyPermissions, withdraw_gate};
    let perms = fetched.unwrap_or(KeyPermissions::UNKNOWN);
    withdraw_gate(&perms, allow_override)
}

/// Pure flag×creds decision (see [`CexCredChoice`]). `mainnet` upgrades to LIVE only when the live
/// tier is present; otherwise it degrades to PAPER rather than to demo-on-mainnet.
pub(crate) fn cex_cred_choice(mainnet: bool, live_present: bool) -> CexCredChoice {
    match (mainnet, live_present) {
        (false, _) => CexCredChoice::Demo,
        (true, true) => CexCredChoice::LiveMainnet,
        (true, false) => CexCredChoice::MainnetNoCreds,
    }
}

/// Build the `Account` per-symbol RULING-margin-mode grid for the ONE mounted symbol — the exact
/// twin of [`multiplier_grid`] below, on the margin axis instead of the notional one.
///
/// Returns `None` — an absent grid, so `Account::default_margin_mode_of` short-circuits to
/// [`vike_model::MarginMode::Cross`] without hashing anything — whenever the resolved mode IS
/// `Cross`. That is the byte-identical case and it is nearly all of them: `Cross` is what the
/// per-venue `VenueCaps::default_margin_mode` says for every roster venue that admits a choice, and
/// what `default_margin_mode` stays at in every `make_engine` arm except hyperliquid's. A `Cross`
/// row and no row at all are indistinguishable through the accessor, so collapsing them keeps the
/// common case allocation-free — the same reason `multiplier_grid` collapses a `1.0` multiplier.
///
/// Kept a separate free function rather than folded into [`multiplier_grid`]: the two answer
/// different venue questions from different sources (contract size vs `onlyIsolated`), and only one
/// of them has a venue that populates it today.
pub(crate) fn margin_mode_grid(
    symbol: &str,
    mode: vike_model::MarginMode,
) -> Option<indexmap::IndexMap<String, vike_model::MarginMode>> {
    (mode != vike_model::MarginMode::Cross).then(|| {
        let mut grid = indexmap::IndexMap::new();
        grid.insert(symbol.to_string(), mode);
        grid
    })
}

/// Build the `Account` per-symbol contract-multiplier grid for the ONE mounted symbol.
///
/// Returns `None` — an absent grid, so `Account::multiplier_of` falls through to its `1.0` scalar
/// default — whenever the venue reported no usable contract size. That is the byte-identical case:
/// `make_engine` passed a literal `None` here for every venue before this existed, so a venue with
/// no contract size (spot crypto, FX, linear perps) mounts exactly as it did.
///
/// A multiplier of exactly `1.0` also yields `None` rather than a one-entry grid: the two are
/// arithmetically identical through `multiplier_of`, and collapsing them keeps the common case
/// allocation-free and the snapshot's `multipliers` map empty rather than carrying a no-op row.
pub(crate) fn multiplier_grid(
    symbol: &str,
    contract_size: f64,
) -> Option<indexmap::IndexMap<String, f64>> {
    // Reuse the model's ONE absent/degenerate-folding rule (non-finite and non-positive → 1.0)
    // rather than re-deriving the predicate here.
    let m = vike_model::SymbolProperties { contract_size, ..Default::default() }.multiplier();
    (m != 1.0).then(|| {
        let mut grid = indexmap::IndexMap::new();
        grid.insert(symbol.to_string(), m);
        grid
    })
}

/// The generic fee-schedule enrichment hook (fee model 5/5) — ONE hook, four producers. Prefers a
/// venue's LIVE account-actual fee rate (its `ReconClient::fetch_fee_rates`, overridden by
/// binance/bybit/okx/deribit) over the caller-supplied `static_default` (the once-evaluated
/// [`vike_model::fee_schedule_for`] result), and is **fail-soft**: a fetch error, `Ok(None)` (venue
/// not wired / field absent), or no recon client at all all fall back to that default — a fee fetch
/// never blocks mounting. This is the analog of the reconcile pre-fetches
/// (`RiskLimits::from_properties`): one blocking, best-effort account read at mount time. Takes the
/// static default as a parameter (rather than re-deriving it) so the caller's paper fill path and
/// this enrichment share ONE `fee_schedule_for` evaluation (fee model follow-up 1: the reviewer's
/// double-eval fix).
pub fn resolve_fee_schedule(
    venue: &str,
    recon: Option<&dyn vike_exec::recon::ReconClient>,
    static_default: vike_model::FeeSchedule,
) -> vike_model::FeeSchedule {
    let Some(rc) = recon else { return static_default };
    match rc.fetch_fee_rates() {
        Ok(Some(live)) => {
            tracing::info!(venue, ?live, "using live account-actual fee schedule");
            live
        }
        Ok(None) => static_default,
        Err(e) => {
            tracing::debug!(venue, error = %e, "fee-rate fetch failed; using static default");
            static_default
        }
    }
}

/// A [`MountPolicy`] that ARMS exactly one venue — every other venue keeps `paper`.
///
/// ⚠ It exists because the arming ceiling made `None` (and the default) mean PAPER, so every test
/// that wants a venue's ARM to run has to say so. That is the ceiling working, not test friction:
/// before it, "which venues does this mount arm?" was answered by whatever happened to be in the
/// vars map, and a test could exercise a live arm without ever declaring that it meant to.
///
/// ⚠ **Guards against a hazard that belongs to `lib.rs`, not to this file.**
/// `crates/vike-ops/tests/docs_constants_gate.rs`'s `code_only` STOPS reading a source file at its
/// first INLINE `#[cfg(test)]` item, so a test-only helper placed near the top of a file hides
/// every constant beneath it from that gate — measured, while this function still lived in
/// `lib.rs`: `ARMED_MAX_ORDERS_PER_WINDOW` (still there, and still what
/// [`crate::arm_universal_defaults`] arms) reported as "no definition of that shape found" once a
/// test-only helper preceded it. `lib.rs` keeps its own `#[cfg(test)]` items at the bottom, beside
/// its test modules, for exactly that reason; this file declares no documented constant of its
/// own, so the hazard does not apply HERE — it would if one ever lands in `arming.rs` too.
#[cfg(test)]
pub(crate) fn armed_policy(venue: &str, mode: vike_config::VenueMode) -> MountPolicy {
    MountPolicy {
        venues: vike_config::VenuePolicy::default().declare(venue, mode),
        ..MountPolicy::default()
    }
}

/// A [`MountPolicy`] that arms the WHOLE roster at `mode` — for the tests whose subject is the
/// OTHER gate (absent credentials), which the ceiling would otherwise short-circuit and make
/// vacuous.
#[cfg(test)]
pub(crate) fn all_armed_policy(mode: vike_config::VenueMode) -> MountPolicy {
    MountPolicy {
        venues: vike_model::VENUES
            .iter()
            .fold(vike_config::VenuePolicy::default(), |p, v| p.declare(v, mode)),
        ..MountPolicy::default()
    }
}

/// **The withdraw override's one source** — the second half of the chain that keeps a live-money
/// safety gate from being widened by anything but its settings row.
///
/// The first half lives in the composition root and is proved there
/// (`crates/vike-tradehub/src/tradehub_cli_tests.rs`'s `the_withdraw_override_is_the_row_alone`):
/// the fold writes the RESOLVED `flags.allow_withdraw_keys` into `vars` as the exact string
/// `"1"`/`"0"`, over whatever the credential store carried. These tests take that string as their
/// input and carry it to the verdict, so the two halves meet at a value rather than at a paragraph.
/// (`the_withdraw_override_is_refused_when_the_environment_says_zero` stood here while the gate
/// also swept the process environment; decision 0095 deleted that sweep and the test with it.)
#[cfg(test)]
mod withdraw_override_tests {
    use super::*;
    use vike_bridge_core::key_permissions::{
        ALLOW_WITHDRAW_KEYS_ENV, KeyPermissions, allow_withdraw_keys,
    };

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// A KNOWN withdraw-capable key — the only input that can refuse at all.
    fn withdraw_capable() -> Result<KeyPermissions, String> {
        Ok(KeyPermissions { can_withdraw: Some(true), ..KeyPermissions::UNKNOWN })
    }

    /// Decision 0095: the override is `flags.allow_withdraw_keys`, which the composition root folds
    /// into the credential map it hands the mount — and that map is the gate's ONLY source. No
    /// process environment is consulted; a set `VIKE_ALLOW_WITHDRAW_KEYS` refuses startup instead.
    #[test]
    fn the_folded_row_is_the_only_source() {
        let verdict = |m: &HashMap<String, String>| {
            binance_withdraw_verdict(withdraw_capable(), allow_withdraw_keys(m))
        };
        assert_eq!(verdict(&map(&[(ALLOW_WITHDRAW_KEYS_ENV, "1")])), WithdrawGate::Allow);
        assert_eq!(verdict(&map(&[(ALLOW_WITHDRAW_KEYS_ENV, "0")])), WithdrawGate::Refuse);
        assert_eq!(verdict(&HashMap::new()), WithdrawGate::Refuse, "the default is REFUSE");
    }
}
