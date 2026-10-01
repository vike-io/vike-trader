//! Who is armed, for what, and at what fee/margin/policy — the per-account/per-venue arming
//! decisions `make_engine_for_account`'s match consults. Excludes `make_engine*` themselves and
//! the per-venue match, which stay at the crate root (see this crate's module doc).

use std::collections::HashMap;

use crate::{ARMED_MAX_ORDERS_PER_WINDOW, AccountLabel, MountError, MountPolicy};

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
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn require_live_risk_budget(
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

/// PRE-CONNECT live-intent probe: would [`crate::make_engine`] build a LIVE exec client for
/// `venue`, judged PURELY from the caller-supplied vars map — through the venue bridge's own
/// `VenueMount::resolve`, the same answer its mount acts on, with NO network I/O and no side
/// effects. This is what lets the missing-risk-budget refusal ([`require_live_risk_budget`]) fire
/// BEFORE any venue session is established (the #817 "refusal happens POST-connect" residual;
/// Freqtrade refuses before touching a broker).
///
/// One answer per registry row, and the row is the venue's own: a contract row answers through its
/// bridge's `resolve`, so the probe and the mount cannot drift. A `FeatureAbsent` row and an id
/// the registry does not carry probe `false`: they can only ever produce `crate::contract`'s
/// `absent_parts` paper client (the legacy match's `_` arm until the dukascopy port deleted it),
/// which the budget rule deliberately leaves unbounded. **fxcm was in that sentence until its
/// arm landed**, and since the venue mount contract its row is its bridge's own `resolve` —
/// `FxcmVenueMount` in `crates/bridges/fxcm/src/mount.rs`, registered under vike-tradehub's `fxcm`
/// feature.
///
/// INTENT semantics, not outcome: `true` means the operator SUPPLIED this venue's live config,
/// even where the mount would later demote to paper (ctrader/ibkr synchronous connect failure,
/// hyperliquid/polymarket declining a bad key). An intended-live session must carry a bounded
/// budget; a config the operator never wrote can never refuse a mount.
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
/// added: [`crate::make_engine_with_legs`]'s `live_permitted`, [`would_mount_live_under`]'s
/// per-row `live` conjunct, and the `live_permitted` a contract row's `credential_probe` is asked
/// with. The last one was open-coded as plain `ceiling_selects_mainnet` (a three-venue set, since
/// deleted with the legacy arms that read it) with no ceiling at all, which — pre-decision-0095,
/// when the network was a `BINANCE_MAINNET=1` switch rather than the ceiling itself — is how a
/// `demo`-capped binance with that switch exported came to be sent a SIGNED MAINNET balance read at
/// startup while the mount bound the demo host.
///
/// It is also the whole of what keeps a `demo` ceiling off MAINNET for the venues whose network IS
/// the ceiling (decision 0095): each such bridge reads it as `MountInputs::live_permitted`, which
/// the mount ([`crate::make_engine_for_account`]) and the arming projection (`crate::contract`'s
/// `contract_arming`) both compute with this function.
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
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn would_mount_live_under_policy(
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
/// (`crates/bridges/ig/src/mount.rs`'s `IgVenueMount::server_time_ms` sends `X-IG-API-KEY`), and
/// the credential leg beside it issues one SIGNED `fetch_balance` per credentialed venue. An
/// operator who caps a venue to `paper` has said "do not touch this account", and the preflight
/// was authenticating against it anyway.
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

/// **THE per-venue arming projection: which tier would `make_engine` actually reach for `venue`
/// under `ceiling`, and what is holding it below that ceiling** — judged PURELY from the
/// caller-supplied vars map plus the registry this binary was handed (a venue whose bridge the
/// build does not compile is a `FeatureAbsent` row), with NO network I/O and no side effects.
///
/// One row per live arm, each citing the arm's own gate so drift is structurally hard — a row that
/// stops matching its arm's loader fails the probe tests below, and a NEW live arm added without a
/// row here is still caught by [`crate::make_engine`]'s post-merge budget backstop (then this fn gains its
/// row). Venues with NO live arm today (every unknown venue; ⚠ this named dukascopy until its arm
/// landed on 2026-09-09, and no ROSTER venue is in that class now) answer `(Paper, NoLiveArm)`:
/// they can only ever produce `crate::contract`'s `absent_parts` paper client (the legacy match's
/// `_` arm until the dukascopy port deleted it), which the budget rule deliberately leaves
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
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn venue_arming_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    account_arming_under(registry, venue, &AccountLabel::Default, vars, ceiling, None)
}

/// **THE REFUSAL, SAID OUT LOUD: every account this box names on a venue whose mount addresses only
/// ONE** — one process-wide message, or `None` when there is nothing to say.
///
/// ⚠ **It exists because the refusal was SILENT, and a silent refusal of a live-trading account is
/// indistinguishable from support.** A venue's `VenueDeclaration::addresses_accounts` answers
/// `false`, [`account_arming_under`] returns [`vike_config::ArmingBlock::NoAccountSupport`], and
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
/// assembled from whichever rows [`venue_arming`] classifies `NoAccountSupport`, so a venue whose
/// declaration's `addresses_accounts` changes changes what this says without anything here being
/// edited. ⚠ **Today it names NOTHING**: dukascopy was the last venue in that class and joined the
/// then hand-written `arm_addresses_accounts` list on 2026-09-15, so this function returns `None`
/// on every box until a venue joins the roster declaring `addresses_accounts: false` — which is
/// exactly what `just new-venue` scaffolds, and why this message is kept rather than deleted with
/// its last producer. `crates/vike-tradehub/tests/unaddressable_account_warning.rs` proves the TEXT
/// against a planted row for that reason.
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
/// test can reach it at all.** Since dukascopy joined the then hand-written `arm_addresses_accounts`
/// on 2026-09-15 — every bridge declares the fact itself now, as
/// `VenueDeclaration::addresses_accounts` — no roster venue produces a
/// [`vike_config::ArmingBlock::NoAccountSupport`] row, so the projection
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
/// ⚠ **A LABELLED account is refused outright on any venue whose declaration says
/// `addresses_accounts: false`**, and on a venue the registry does not carry, before that venue's
/// `resolve` is consulted ([`account_arming_raw`], `crate::contract`'s `contract_arming`). Every
/// bridge that CAN address an account threads `label` into the same loader its `mount` calls, so
/// the projection and the mount cannot disagree about which account's keys were read; a row that
/// probed the default account for a labelled mount would answer with the wrong keys in both
/// directions.
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
        && crate::contract::is_process_exclusive(crate::row_of(registry, venue))
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
/// own credentials, ceiling and `account` row: a contract row through its bridge's `resolve`, and
/// every other row by the fixed answers below.
///
/// Split out so `crate::exclusive`'s `holder` can ask "would this account arm on its own merits"
/// from inside the override without asking the override again. The two are one function to every
/// caller; only the holder computation reaches this one.
///
/// ⚠ `arm_addresses_accounts`, the hand-written list of venues whose arm addressed a named account,
/// is gone: each bridge declares it (`VenueDeclaration::addresses_accounts`), and a venue the
/// registry does not carry addresses none.
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
        // No arm in ANY build: a venue the registry does not carry. The legacy probe's own
        // preconditions, in its order: a labelled account first (such a venue addresses no named
        // account), then the ceiling, then "no live arm".
        None => {
            if !label.is_default() {
                (Mode::Paper, Block::NoAccountSupport)
            } else if ceiling == Mode::Paper {
                (Mode::Paper, Block::Disarmed)
            } else {
                (Mode::Paper, Block::NoLiveArm)
            }
        }
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
/// No network and no clock, and no process env at all (decision 0095: every bridge's `resolve`
/// reads only the ceiling and the vars map's key presence, never a variable).
///
/// ⚠ **This said "Pure" and it was not from 2026-09-15 until the dukascopy port**: the dukascopy
/// row in `account_arming_under` read the SETTINGS STORE, because which credential keys that
/// venue's account arms from is a fact about an `account` ROW rather than about the vars map, and
/// a projection that answered without it would disagree with the mount. That row is the bridge's
/// `resolve` now (`crates/bridges/dukascopy/src/mount.rs`'s `resolve_in`), and it reads the
/// `account` table the composition root read and set on the policy (`MountPolicy::accounts`), so
/// this projection opens no store; a caller that threads no table gets the UNREAD one, a
/// `Backend::Files` box's answer. **A test may still drive this from a fixture map.** The
/// per-FRAME caller the old sentence was written for is gone: `vike-desktop` links no mount and
/// renders `vike_config::venue_arming::ceilings_only`.
/// ⚠ **One row per ACCOUNT, not per venue** — a box with no `[accounts]` table and a store holding
/// no labelled key still produces exactly one row per roster venue (the DEFAULT account's), which
/// is the table this function has always returned.
///
/// ⚠ **It no longer takes the caller's wired-market table.** That parameter existed for the
/// SYMBOL-COLLISION rule, which is deleted (`vike_config::venue_accounts`' module doc carries why),
/// and an arming row is now a fact about ONE account alone — so a symbol table here would be a
/// value that reaches no answer while looking as though it does. `vike_run::venue_arming` was the
/// wrapper that supplied it; it became a plain re-export of this function, and went with vike-run's
/// other alias re-exports when vike-tradehub took its own vike-mount edge (docs/decisions/0096,
/// amended 2026-09-29).
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

// REMOVED with the last legacy arm (docs/decisions/0096): `ceiling_selects_mainnet`, the three CEX
// venues whose network the ceiling chooses (decision 0095), and `cex_cred_choice`, the pure
// ceiling×credentials matrix behind the legacy prefix's `MainnetNoCreds` warning. Nothing mounts
// through that prefix any more: each of those venues' bridges decides the same thing itself, from
// `MountInputs::live_permitted` (see `ceiling_permits_live` above) plus the live key set it finds,
// and stays paper rather than sign a mainnet host with demo keys. The arming-migration message
// asks `crate::paper_fallback`'s `network_is_the_ceiling`, a membership test on
// `vike_secrets::live_means_mainnet::SWITCHED_VENUES`.

/// Build the `Account` per-symbol RULING-margin-mode grid for the ONE mounted symbol — the exact
/// twin of `multiplier_grid` below, on the margin axis instead of the notional one.
///
/// Returns `None` — an absent grid, so `Account::default_margin_mode_of` short-circuits to
/// [`vike_model::MarginMode::Cross`] without hashing anything — whenever the resolved mode IS
/// `Cross`. That is the byte-identical case and it is nearly all of them: `Cross` is what the
/// per-venue `VenueCaps::default_margin_mode` says for every roster venue that admits a choice, and
/// what `default_margin_mode` stays at in every mount except hyperliquid's. A `Cross`
/// row and no row at all are indistinguishable through the accessor, so collapsing them keeps the
/// common case allocation-free — the same reason `multiplier_grid` collapses a `1.0` multiplier.
///
/// Kept a separate free function rather than folded into `multiplier_grid`: the two answer
/// different venue questions from different sources (contract size vs `onlyIsolated`), and only one
/// of them has a venue that populates it today.
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn margin_mode_grid(
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
