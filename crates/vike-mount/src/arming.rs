//! Who is armed, for what, and at what fee/margin/policy — the per-account/per-venue arming
//! decisions `make_engine_for_account`'s match consults. Excludes `make_engine*` themselves and
//! the per-venue match, which stay at the crate root (see this crate's module doc).

use std::collections::HashMap;

use crate::{
    ARMED_MAX_ORDERS_PER_WINDOW, AccountLabel, CexCredChoice, MountError, MountPolicy, WithdrawGate,
};

/// Arm the operator-budget fields that have a UNIVERSALLY-safe default — the Nautilus half of
/// Task 6's two-kind split (see the module-level comment above `ARMED_MAX_ORDERS_PER_WINDOW`).
/// Called from [`make_engine`] at the SAME site as, and immediately before, the pre-existing
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
/// [`make_engine`]: the PRE-CONNECT site (gated on [`would_mount_live`] — the primary, firing
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

/// PRE-CONNECT live-intent probe: would [`make_engine`]'s arm for `venue` build a LIVE exec
/// client, judged PURELY from the caller-supplied vars map — by calling the SAME config loaders
/// the arms themselves gate on, with NO network I/O and no side effects. This is what lets the
/// missing-risk-budget refusal ([`require_live_risk_budget`]) fire BEFORE any venue session is
/// established (the #817 "refusal happens POST-connect" residual; Freqtrade refuses before
/// touching a broker).
///
/// One row per live arm, each citing the arm's own gate so drift is structurally hard — a row
/// that stops matching its arm's loader fails the probe tests below, and a NEW live arm added
/// without a row here is still caught by the post-merge backstop check (then this fn gains its
/// row). Venues with NO live arm in [`make_engine`] today (dukascopy — its exec factory is not
/// mounted at all; every unknown venue) probe `false`: they can only ever produce the paper `_`
/// arm, which the budget rule deliberately leaves unbounded. **fxcm was in that sentence until its
/// arm landed** and is now a feature-gated row like ibkr's, with the extra `sdk_available` conjunct
/// [`fxcm_live_intent`] explains.
///
/// INTENT semantics, not outcome: `true` means the operator SUPPLIED this venue's live config,
/// even where the arm would later demote to paper (ctrader/ibkr synchronous connect failure,
/// hyperliquid/polymarket factory declining a bad key). An intended-live session must carry a
/// bounded budget; a config the operator never wrote can never refuse a mount.
#[must_use]
pub fn would_mount_live(venue: &str, vars: &HashMap<String, String>) -> bool {
    would_mount_live_under(venue, vars, vike_config::VenueMode::Live)
}

/// This deployment's arming ceiling for one venue — **the one place `Option<&MountPolicy>` is
/// turned into a [`vike_config::VenueMode`]**, so no caller can invent its own answer for a
/// policy-less mount.
///
/// `None` reads [`VenueMode::Paper`](vike_config::VenueMode::Paper), not `Live`: fail-safe by
/// construction, the widening mistake has to be typed. That rule was written twice — here and at
/// [`make_engine_with_legs`]'s ceiling seam — for exactly as long as it took someone to add a
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
    policy: Option<&vike_config::VenuePolicy>,
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
    policy: Option<&vike_config::VenuePolicy>,
) -> Vec<AccountLabel> {
    let mut labels: Vec<AccountLabel> =
        store.iter().filter(|a| a.venue == venue).map(|a| a.label.clone()).collect();
    if let Some(p) = policy {
        for (v, label, _) in p.accounts() {
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
/// selection [`make_engine_accounts`] mounts from. One function, so the "Effective" column cannot
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
/// [`make_engine_accounts`] still takes the per-account symbol map, because it MOUNTS each engine
/// on its own row's symbol; that is a different question from what this one answers.
///
/// ⚠ **An id `vike_model::VENUES` does not carry answers with NO rows at all**, rather than with a
/// default-account row. `vike_config::VenueArming::venue` is a `&'static str` taken from the roster
/// itself — the construction that makes a row incapable of naming a venue that does not exist — so
/// there is no row to build for a `"sim"` or a test id. [`make_engine_accounts`] reads the empty
/// answer as "one account, nothing to choose between", which is exactly what such a venue has.
#[must_use]
pub fn venue_account_arming(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&vike_config::VenuePolicy>,
) -> Vec<vike_config::VenueArming> {
    account_arming_in(venue, vars, &accounts_in_store(vars), policy)
}

/// **The symbol ONE account of a venue is armed on**, out of the per-account map its composition
/// root derived (`vike_run::account_symbols_for`).
///
/// A label the map does not name falls back to the DEFAULT account's row — the venue's wired
/// symbol — and an empty map answers `""`, which is what a venue outside `WIRED_MARKETS` has.
/// Spelled once here because [`make_engine_accounts`] mounts each engine through it and
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
/// accounts whose effective trading book is the same, which [`make_engine_accounts`] warns about
/// and mounts anyway.
///
/// It takes the ROWS rather than re-deriving them so the report can never describe a different
/// arming from the one being mounted, and it is a separate function from
/// [`venue_account_arming`] rather than a second return value because it changes NOTHING about
/// them: the pair is a report, not an input to any decision, and a caller that never asks is not
/// silently missing a refusal.
///
/// Only rows whose book [`crate::book_identity::effective_book`] can determine OFFLINE contribute an
/// `ArmedBook`; a key/secret venue's accounts contribute none, so no pair forms and nothing is
/// said. That is the "warn nothing where you cannot tell" half of the rule, and it is implemented
/// by ABSENCE rather than by a placeholder — see [`vike_config::ArmedBook`].
#[must_use]
pub fn shared_books_for(
    venue: &str,
    rows: &[vike_config::VenueArming],
    vars: &HashMap<String, String>,
) -> Vec<vike_config::SharedBook> {
    let books: Vec<vike_config::ArmedBook> = rows
        .iter()
        .filter_map(|r| {
            let book = crate::book_identity::effective_book(r.venue, &r.label, r.effective, vars)?;
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
/// [`make_engine_accounts`] mounts one engine per `(venue, AccountLabel)`; that is the same thing
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
/// [`make_engine_accounts`]' own comment declares as a measured blind spot.
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
    venue: &str,
    vars: &HashMap<String, String>,
    store: &[vike_model::account_keys::AccountRef],
    policy: Option<&vike_config::VenuePolicy>,
) -> Vec<vike_config::VenueArming> {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};

    let Some(venue) = vike_model::VENUES.iter().copied().find(|v| *v == venue) else {
        return Vec::new();
    };
    known_accounts_in(venue, store, policy)
        .into_iter()
        .map(|label| {
            let ceiling = policy.map_or(Mode::Paper, |p| p.account(venue, &label));
            let (effective, block) = account_arming_under(venue, &label, vars, ceiling);
            // `Disarmed` is the venue-level answer, and for a LABELLED account with no line of its
            // own it names the wrong cause: the venue may be armed `live` and this account still
            // resolves paper, because it was never named. Reporting `Disarmed` would send the
            // operator to the `[venues]` line they already set.
            let block = if block == Block::Disarmed
                && !label.is_default()
                && policy.is_some_and(|p| p.get(venue) != Mode::Paper)
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
/// added: [`make_engine_with_legs`]'s `live_permitted`, [`would_mount_live_under`]'s per-row `live`
/// conjunct, and `crate::startup::authed_read_clients`' probe tier. The last one was open-coded as
/// plain [`cex_mainnet_enabled`] with no ceiling at all, which is how a `demo`-capped binance with
/// `BINANCE_MAINNET=1` came to be sent a SIGNED MAINNET balance read at startup while the mount
/// bound the demo host.
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
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> bool {
    would_mount_live_under(venue, vars, venue_ceiling(policy, venue))
}

/// [`would_mount_live`] under a deployment's per-venue ARMING CEILING — the answer
/// [`make_engine_with_legs`] itself needs, since the ceiling changes which tier the arm resolves.
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
/// [`report_capped_to_paper`] and [`venue_arming_migration_message`] need (they exist to say
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
/// "Live" here means **not paper** — a real, authenticated exec session, whichever TIER it lands on
/// — and that is the meaning it has always had (a CEX venue with demo keys and no
/// `{VENUE}_MAINNET` probes `true`). The budget rule cares about "is a venue session established on
/// the operator's behalf", not about which endpoint it dials.
///
/// ⚠ **`pub`, not `pub(crate)`, and the visibility is load-bearing.** `vike_run::armed_live_venues`
/// calls this from ANOTHER CRATE to compute which venues a live sentinel must cover BEFORE the
/// mount runs — the lock that stops two processes trading one account. Narrowing it again does not
/// merely fail to compile there; it removes the only pre-mount answer that lock can be built from.
#[must_use]
pub fn would_mount_live_under(
    venue: &str,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
) -> bool {
    venue_arming_under(venue, vars, ceiling).0 != vike_config::VenueMode::Paper
}

/// **THE per-venue arming projection: which tier would `make_engine`'s arm for `venue` actually
/// reach under `ceiling`, and what is holding it below that ceiling** — judged PURELY from the
/// caller-supplied vars map plus this binary's compiled features, with NO network I/O and no side
/// effects.
///
/// One row per live arm, each citing the arm's own gate so drift is structurally hard — a row that
/// stops matching its arm's loader fails the probe tests below, and a NEW live arm added without a
/// row here is still caught by [`make_engine`]'s post-merge budget backstop (then this fn gains its
/// row). Venues with NO live arm today (dukascopy — its exec factory is not mounted at all; every
/// unknown venue) answer `(Paper, NoLiveArm)`: they can only ever produce the paper `_` arm, which
/// the budget rule deliberately leaves unbounded.
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
pub(crate) fn venue_arming_under(
    venue: &str,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    account_arming_under(venue, &AccountLabel::Default, vars, ceiling)
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
/// **The one venue deliberately still refused is `dukascopy`**, for TWO independent reasons, either
/// of which alone would be enough: it has no live arm in [`make_engine_for_account`] at all (every
/// build falls through to the paper `_` arm, so there is nothing to address), and its
/// `DUKASCOPY_DEMO1_*` / `DEMO2` shape already bakes an account INDEX into the tier token —
/// `vike_model::account_keys` pins it as non-conforming ON PURPOSE, and reconciling two
/// multi-account spellings is a design question rather than a mechanical port.
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
            // The three FEATURE-GATED venues. They answer `true` in a build with no arm too, and
            // that is correct rather than sloppy: the feature-off arm is the paper `_` one, which
            // reaches no credential of any account, so the answer this function gives changes
            // nothing there. Making it `cfg`-dependent would instead make the ARMING SCREEN report
            // a different refusal on two boxes of one deployment — the portability
            // `vike_config::venue_mode`'s module doc requires — and `ArmingBlock::FeatureAbsent`
            // is already the row that names the real cause.
            | "fxcm"
            | "ibkr"
            | "polymarket" // vike:new-venue:note do NOT add "{venue}" to this list yet. A `true` here with an arm that still reads the venue's UNLABELLED keys is the two-engines-on-one-account hazard this whole seam exists to prevent — add it only once that venue's `make_engine_for_account` arm THREADS `account` into an account-aware credential loader AND `crates/vike-mount/tests/account_credential_isolation.rs` pins that loader account-aware: crates/vike-mount/src/arming.rs's `arm_addresses_accounts`
    )
}

/// [`venue_arming_under`] for one named ACCOUNT — the projection [`venue_account_arming`] builds its
/// rows from, and the one [`make_engine_accounts`] selects with.
///
/// ⚠ **A LABELLED account is refused outright on any venue [`arm_addresses_accounts`] says no to**,
/// before any row is consulted — see that function for the current set and the argument. Every row
/// below that CAN address an account threads `label` into the same loader its arm calls, so the
/// projection and the mount cannot disagree about which account's keys were read; a row that probed
/// the default account for a labelled mount would answer with the wrong keys in both directions.
pub(crate) fn account_arming_under(
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
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
    // something other than a hardcoded `Environment::Demo` — the three CEX venues
    // (`{VENUE}_MAINNET`), hyperliquid (the same flag, resolved inside its own helper), aster
    // (Live-first with no flag at all) and polymarket (one tier, real money). Every other row
    // resolves DEMO unconditionally and is unaffected by a `demo` ceiling.
    let live = ceiling_permits_live(ceiling);
    // The block a venue whose arm hardcodes DEMO reports when the ceiling asked for more. Below a
    // `live` ceiling the demo tier IS the ceiling, so nothing is being refused.
    let demo_under = |block: Block| (Mode::Demo, if live { block } else { Block::None });
    match venue {
        // The four generic-creds arms (`("bybit"|"okx"|"binance"|"deribit", Some(c))`): mirror
        // the top-of-`make_engine` mainnet×tier resolution verbatim (`cex_mainnet_enabled` is
        // `false` for deribit and every non-CEX venue, so those read the DEMO tier).
        "bybit" | "okx" | "binance" | "deribit" => {
            let mainnet = cex_mainnet_enabled(venue, vars) && live;
            let tier = if mainnet { Environment::Live } else { Environment::Demo };
            if load_credentials_for_account(venue, tier, label, vars).is_none() {
                (Mode::Paper, Block::NoCredentials)
            } else if mainnet {
                (Mode::Live, Block::None)
            } else if vike_bridge_core::mainnet::mainnet_switch_for(venue).is_some() {
                // binance/bybit/okx: the flag exists and is unset (or blank/`"true"`, which STEP 2
                // stopped honouring) — the one cause an operator can fix in `secrets.env`.
                demo_under(Block::MainnetSwitchUnset)
            } else {
                // deribit: switchless, so `make_engine` always resolves its DEMO credential set.
                demo_under(Block::DemoOnlyArm)
            }
        }
        // `("aster", _)`: agent-wallet creds, LIVE (mainnet) preferred then TESTNET — the arm's
        // own chain, INCLUDING the ceiling that deletes the Live attempt below `live`.
        "aster" => {
            if live
                && vike_aster::signing::load_aster_credentials_for_account(
                    Environment::Live,
                    label,
                    vars,
                )
                .is_some()
            {
                (Mode::Live, Block::None)
            } else if vike_aster::signing::load_aster_credentials_for_account(
                Environment::Demo,
                label,
                vars,
            )
            .is_some()
            {
                // Switchless by declaration (`mainnet_switch_for("aster")` is `None`): what selects
                // the tier here is WHICH key set exists, so a live ceiling with only testnet keys
                // is a credential fact, not a flag fact.
                (Mode::Demo, if live { Block::LiveCredentialsAbsent } else { Block::None })
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // `("hyperliquid", _)`: the pure half of `hyperliquid_live_client`'s gate (flag-selected
        // env, key present) — `hl_env` is the SAME resolution the arm performs, taking the same
        // conjunct, so the tier below is the tier the mount dials.
        "hyperliquid" => {
            if !crate::hyperliquid::would_mount_live(vars, label, live) {
                (Mode::Paper, Block::NoCredentials)
            } else if crate::hyperliquid::hl_env(vars, live) == vike_hyperliquid::config::Env::Live
            {
                (Mode::Live, Block::None)
            } else {
                demo_under(Block::MainnetSwitchUnset)
            }
        }
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
        // over a venue that can only be paper (the same reason dukascopy/fxcm answer paper below).
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
        // `POLY_EXEC=1` AND key material present (`live_mount_from_vars` → the same
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
        // consults a property of the BINARY as well as of the vars map — see [`fxcm_live_intent`],
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
        // `("dukascopy", _)`: the arm self-gates on `load_dukascopy_config_from(Demo1, …)`, exactly
        // as the REST venues above self-gate on their own loaders. ⚠ It gained that arm on
        // 2026-09-09 and had none before — this row used to fall through to `NoLiveArm` below, and
        // the comment there named dukascopy as the venue that had no arm in any build.
        //
        // DEMO1 ONLY, matching the mount arm: `arm_addresses_accounts` still refuses this venue,
        // because the second of its two reasons is untouched — the `DUKASCOPY_DEMO1_*` shape bakes
        // an account INDEX into the tier token. So the probe reads the DEFAULT account's DEMO1
        // credentials and ignores the label, which is what the arm does.
        //
        // Demo, never Live: the sidecar authenticates against a demo server and there is no live
        // tier wired, so a store carrying these keys arms a DEMO session — the same stance the ig
        // and oanda rows take.
        "dukascopy" => {
            if vike_dukascopy::load_dukascopy_config_from(
                vike_dukascopy::DukascopyAccount::Demo1,
                vars,
            )
            .is_some()
            {
                demo_under(Block::DemoOnlyArm)
            } else {
                (Mode::Paper, Block::NoCredentials)
            }
        }
        // No live arm in ANY build: every unknown venue reaches only the paper `_` arm.
        _ => (Mode::Paper, Block::NoLiveArm),
    }
}

/// **The arming screen's whole data source**: one [`vike_config::VenueArming`] row per
/// `vike_model::VENUES` id, each pairing the operator's ceiling with what
/// [`venue_arming_under`] — and therefore [`make_engine_with_legs`] — would actually do with it.
///
/// It exists so the GUI does not have to re-derive the answer. `vike-app-core` (the CI-tested
/// half of the desktop shell) cannot depend on this crate: `vike-mount` is `vike-app`'s OPTIONAL
/// `fat` dependency, and a thin `--observe` build links no bridge at all. So the row type lives
/// down in `vike-config`, beside the ceiling it carries, and this is the producer a fat build calls.
///
/// Pure: no network, no clock, no process env beyond what the individual arms already read
/// (`hl_env`'s and `cex_mainnet_enabled`'s process-env half of the `{VENUE}_MAINNET` fold), so the
/// GUI may call it per frame and a test may drive it from a fixture map.
/// ⚠ **One row per ACCOUNT, not per venue** — a box with no `[accounts]` table and a store holding
/// no labelled key still produces exactly one row per roster venue (the DEFAULT account's), which
/// is the table this function has always returned.
///
/// ⚠ **It no longer takes the caller's wired-market table.** That parameter existed for the
/// SYMBOL-COLLISION rule, which is deleted (`vike_config::venue_accounts`' module doc carries why),
/// and an arming row is now a fact about ONE account alone — so a symbol table here would be a
/// value that reaches no answer while looking as though it does. `vike_run::venue_arming` is the
/// wrapper that supplied it, and it says the same thing from the other side.
#[must_use]
pub fn venue_arming(
    vars: &HashMap<String, String>,
    policy: &vike_config::VenuePolicy,
) -> Vec<vike_config::VenueArming> {
    // ONE store parse for the whole roster walk — see [`accounts_in_store`]. This function is called
    // per FRAME while the Data Manager's Venues tab is open.
    let store = accounts_in_store(vars);
    policy
        .iter()
        .flat_map(|(venue, _)| account_arming_in(venue, vars, &store, Some(policy)))
        .collect()
}

/// The pure half of the fxcm probe: does the operator's configuration, in a binary whose ForexConnect
/// SDK linkage is `sdk_available`, describe a mount that will actually go LIVE?
///
/// Both conjuncts are load-bearing and they fail differently:
///
/// * **credentials** — the arm self-gates on the same `load_fxcm_config_from(Demo, …)` it calls, so
///   an unconfigured store can never raise a risk-budget refusal over a paper mount.
/// * **`sdk_available`** — a STUB build's arm refuses and lands on paper no matter what the store
///   says, so reporting live intent there would refuse a mount that can only ever be paper. Exactly
///   the stance the oanda row takes for a live-armed store, and the reason dukascopy probes false.
///   It is a PARAMETER rather than a `cfg!` read so both answers can be exercised, which matters
///   here more than anywhere else in this function: no CI runner has the SDK, so the `true` branch
///   is unreachable in every build any gate will ever run.
///
/// INTENT semantics, not outcome, on the remaining axis: a linked build with a WRONG password still
/// probes live. `FxcmExecutionClient::spawn` is infallible and a failed login is indistinguishable
/// from a good one from outside (`crates/bridges/fxcm/CLAUDE.md`), so nothing here could tell them
/// apart — and an operator who wrote credentials must carry a bounded budget either way.
///
/// `label` scopes the credential half to ONE account (`FXCM_DEMO_USER__ALT` &c). The SDK half is a
/// property of the BINARY and is therefore account-blind by construction: a stub build refuses
/// every account, which is the same refusal it has always given.
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

/// Whether this venue's `{VENUE}_MAINNET` real-money endpoint switch (#771) is armed, resolved
/// through the SAME per-venue reader the bridge adapter owns (`mainnet_enabled`), so the credential
/// set [`make_engine`] selects and the hosts the adapter binds always flip together. Only the three
/// mainnet-capable crypto-CEX venues have the flag; EVERY other venue — and an unset flag —
/// returns `false`, i.e. the DEMO path, byte-identical to before this switch existed.
///
/// STEP 2 of the `{VENUE}_MAINNET` convergence (`vike_bridge_core::mainnet`) changed TWO things
/// here. (1) `vars` — the workspace `.env` map — is now a real source alongside the process env
/// (exact `"1"` in either, process winning), so a `BINANCE_MAINNET=1` line written in the `.env`
/// no longer parses as UNSET and silently keeps the venue on demo. (2) This is now the ONLY place
/// the flag is read for these three venues: the resolved `bool` is threaded down into every adapter
/// site (grid pre-fetch, exec spawn, funding poller, recon client) instead of each spawned thread
/// re-reading global env for itself — so a mount can never sign mainnet credentials against demo
/// hosts, even if the environment mutates mid-session.
pub(crate) fn cex_mainnet_enabled(venue: &str, vars: &HashMap<String, String>) -> bool {
    match venue {
        "binance" => vike_binance::exec::mainnet_enabled(vars),
        "bybit" => vike_bybit::perp::mainnet_enabled(vars),
        "okx" => vike_okx::perp::mainnet_enabled(vars),
        _ => false,
    }
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
///   outcome absent credentials produce. `VIKE_ALLOW_WITHDRAW_KEYS=1` (EXACT `"1"`, read off the
///   REAL process env like every other operator toggle) overrides it.
///
/// Blocking: at most ONE signed GET, and only for a live MAINNET binance mount — i.e. only for the
/// exact mount whose first order would otherwise be the probe.
pub(crate) fn binance_withdraw_gate(
    mainnet: bool,
    creds: &vike_bridge_core::Credentials,
) -> WithdrawGate {
    use vike_bridge_core::key_permissions::{KeyPermissionProbe, allow_withdraw_keys};
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
    let process_env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let verdict = binance_withdraw_verdict(fetched, allow_withdraw_keys(&process_env));
    if verdict == WithdrawGate::Refuse {
        tracing::error!(
            venue = "binance",
            "⚠ REFUSING to arm binance LIVE: this API key can WITHDRAW. Falling back to the PAPER \
             client (the same outcome absent credentials produce). Re-issue a trade-only key, or \
             set VIKE_ALLOW_WITHDRAW_KEYS=1 to override."
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
