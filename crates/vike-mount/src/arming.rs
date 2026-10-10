//! Who is armed, for what, and at what fee/margin/policy — the per-account/per-venue arming
//! decisions `make_engine_for_account` consults (`make_engine*` live in `engine.rs`, the operator
//! risk budget in `budget.rs`). The `pub` probes are public for the roster tests in
//! `crates/vike-tradehub/tests/mount_roster.rs`, which run where the registry is
//! (docs/decisions/0096).

use std::collections::HashMap;

use crate::{AccountLabel, MountPolicy};

pub(crate) use crate::budget::require_live_risk_budget;

/// PRE-CONNECT live-intent probe: would [`crate::make_engine`] build a LIVE exec client for
/// `venue`? Judged PURELY from the vars map through the bridge's own `VenueMount::resolve` (the
/// answer its mount acts on, so the two cannot drift), with NO network I/O or side effects — what
/// lets [`require_live_risk_budget`] refuse BEFORE any venue session exists. A `FeatureAbsent` row
/// and an unknown id probe `false`: they only produce `crate::contract`'s `absent_parts` paper
/// client, which the budget rule leaves unbounded.
///
/// INTENT, not outcome: `true` means the operator SUPPLIED this venue's live config, even where the
/// mount later demotes to paper (ctrader/ibkr connect failure, hyperliquid/polymarket declining a
/// bad key). An intended-live session must carry a bounded budget; a config the operator never
/// wrote can never refuse a mount.
#[must_use]
pub fn would_mount_live(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
) -> bool {
    would_mount_live_under(registry, venue, vars, vike_config::VenueMode::Live)
}

/// This deployment's arming ceiling for one venue — **the one place `Option<&MountPolicy>` becomes
/// a [`vike_config::VenueMode`]**, so no caller invents its own answer for a policy-less mount.
/// `None` reads [`VenueMode::Paper`](vike_config::VenueMode::Paper): fail-safe, the widening
/// mistake has to be typed. Written twice (here and [`crate::make_engine_for_account`]'s seam), a
/// second consumer ([`crate::startup::run_startup_preflight`]) defaulted the other way; one
/// function is the cure.
pub(crate) fn venue_ceiling(policy: Option<&MountPolicy>, venue: &str) -> vike_config::VenueMode {
    account_ceiling(policy, venue, &AccountLabel::Default)
}

/// **This deployment's arming ceiling for one ACCOUNT of one venue** — the per-account twin of
/// [`venue_ceiling`] (which delegates here, so there is one predicate), and the fold that makes the
/// `policy.accounts.<venue>.<LABEL>` rows bind. Resolution is `vike_config::VenuePolicy::account`:
/// the DEFAULT account inherits its venue's line, a LABELLED one with no line resolves `paper`, and
/// both are `min`-capped by the venue's line — a second ceiling UNDER the first that can only
/// REFUSE. `None` reads `Paper`, as [`venue_ceiling`].
pub(crate) fn account_ceiling(
    policy: Option<&MountPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> vike_config::VenueMode {
    policy.map_or(vike_config::VenueMode::Paper, |p| p.venues.account(venue, label))
}

/// **The ROUTING key for one account** — `vike_exec::ExecutionEngine::route_key`, the
/// `LIVE-<route_key>.lock` sentinel filename, and the account's name in `live_venues`. Rendered
/// only by `vike_model::accounts::account_keys::AccountRef::route_key`, which carries no tier (one
/// process mounts a venue at one tier). For `AccountLabel::Default` it is the bare venue id, as
/// `ExecutionEngine::new` seeds and every existing sentinel is named.
///
/// ⚠ A roster `venue` borrows `vike_model::VENUES`' own `&'static str`; a non-roster one (a test's
/// `"sim"`, a paper id) has no `AccountRef` and can only be a default account, so it renders as
/// itself.
pub(crate) fn account_route_key(venue: &str, label: &AccountLabel) -> String {
    let Some(id) = vike_model::VENUES.iter().copied().find(|v| *v == venue) else {
        // No roster row, no `AccountRef`: only ever a default account, so the bare id IS the key.
        return venue.to_string();
    };
    vike_model::accounts::account_keys::AccountRef { venue: id, tier: "LIVE", label: label.clone() }
        .route_key()
}

/// **The exec-event lane ONE account's venue adapters push into** — the caller's lane, scoped to
/// `route_key`, so an account-wide `Event::AccountState` reaches THAT account's engine.
///
/// Why the mount, not the bridge: `Event::AccountState` names no symbol, so unscoped it folded into
/// the venue's default engine (a second account's balances in the first's book). Other payloads
/// route by client-order-id (`Event::Fill`, via `vike_core`'s `coid_venue` map) or by a symbol
/// `route_event` uses only when EXACTLY ONE engine of the venue claims it. ⚠ That qualifier is
/// required: two accounts of one venue may now share a symbol (`vike_config::venue_accounts`).
/// A bridge holds ONE credential set and knows no labels; [`crate::make_engine_for_account`]
/// binds this result as its only `live_events`, so every venue arm pushes into the scoped lane.
///
/// A default account is inert: [`account_route_key`] renders the bare venue id and
/// `vike_exec::EventSender::routed` stamps nothing when the key equals the payload's venue, so this
/// is called UNCONDITIONALLY and a box with no `policy.accounts` rows keeps `route_key: None` off
/// the wire (`skip_serializing_if`): journal bytes unchanged.
pub(crate) fn account_event_sender(
    live_events: &vike_exec::EventSender,
    route_key: &str,
) -> vike_exec::EventSender {
    live_events.routed(route_key)
}

/// **Every account of `venue` this box knows about**, default first (always present: an empty
/// store still mounts it, as paper), then labels in order. The UNION of two sources:
///
/// * the CREDENTIAL STORE (`vike_model::accounts::account_keys::accounts_in_store`): keys with no
///   policy line must be listed, as the `AccountNotNamed` row that tells the operator how to arm;
/// * the `policy.accounts.<venue>.<LABEL>` ROWS: a label with no keys must be listed, or a typo
///   would vanish instead of showing as `NoCredentials`.
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
/// ⚠ Hoisted out of [`known_accounts`]: `accounts_in_store` re-parses every key against every
/// roster venue, so [`venue_arming`]'s roster walk (callers: its doc) did that work once per
/// venue for one answer. One scan per projection.
fn accounts_in_store(
    vars: &HashMap<String, String>,
) -> Vec<vike_model::accounts::account_keys::AccountRef> {
    vike_model::accounts::account_keys::accounts_in_store(vars.keys().map(String::as_str))
}

/// [`known_accounts`] over an already-parsed store (see [`accounts_in_store`]).
fn known_accounts_in(
    venue: &str,
    store: &[vike_model::accounts::account_keys::AccountRef],
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
    // Default FIRST (`make_engine_accounts`'s order contract: callers bind `[0]`).
    let mut out = vec![AccountLabel::Default];
    out.extend(labels);
    out
}

/// **THE per-account arming projection for one venue** — the rows the Data Manager renders AND the
/// selection [`crate::make_engine_accounts`] mounts from, so "Effective" cannot disagree with the
/// mount. Each row is [`venue_arming_under`] at that account's own ceiling
/// ([`vike_config::VenuePolicy::account`]); a LABELLED account with no line of its own is
/// reported `AccountNotNamed`, not the `Disarmed` a venue-level `paper` gives (different lines).
///
/// # ⚠ THERE IS NO SECOND, SYMBOL-SHAPED REFUSAL HERE ANY MORE
///
/// Two accounts on one instrument is an ordinary spread; refusing it was the defect
/// (`vike_config::venue_accounts`' module doc). Its replacement is [`shared_books_for`], a WARNING
/// computed beside these rows and never folded in: no row is downgraded by another row.
///
/// ⚠ **It takes no SYMBOL, on purpose**: an arming row is a fact about ONE account, and a symbol
/// parameter would reach nothing while looking as if it changed the answer.
/// [`crate::make_engine_accounts`] takes the per-account symbol map because it MOUNTS on it.
///
/// ⚠ **An id `vike_model::VENUES` does not carry answers with NO rows**:
/// `vike_config::VenueArming::venue` is a roster `&'static str`, so no row can name a `"sim"`.
/// [`crate::make_engine_accounts`] reads the empty answer as "one account, nothing to choose".
///
/// ⚠ **It takes the whole [`MountPolicy`], not its `venues` table**: the dukascopy row resolves an
/// ACCOUNT out of [`MountPolicy::accounts`] (the `account` table the composition root read) and
/// [`crate::make_engine_for_account`] reads the same object — one snapshot, two consumers.
#[must_use]
pub fn venue_account_arming(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::VenueArming> {
    account_arming_in(registry, venue, vars, &accounts_in_store(vars), policy)
}

/// **The symbol ONE account of a venue is armed on**, from the per-account map its composition root
/// derived (`crate::account_symbols_for`). An unnamed label falls back to the DEFAULT account's row
/// (the venue's wired symbol); an empty map answers `""`. Spelled once: the mount and the node
/// assembly's probe both use it, and two fallbacks would mount one symbol while the lock claimed
/// another.
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
/// accounts with the same effective trading book, which [`crate::make_engine_accounts`] warns about
/// and mounts anyway. It takes the ROWS so it cannot describe a different arming than the one
/// mounted, and is separate from [`venue_account_arming`] because it decides NOTHING: a caller
/// that never asks misses no refusal. An account whose book
/// [`crate::book_identity::book_of_account`] cannot determine contributes no
/// [`vike_config::ArmedBook`], so "warn nothing where you cannot tell" holds by ABSENCE.
///
/// ⚠ **It takes the whole [`MountPolicy`]** (as [`venue_account_arming`]): resolution is
/// RECORDED-first ([`crate::book_identity::recorded_book`] reads
/// `vike_secrets::Account::venue_account_id` from [`MountPolicy::accounts`]). Without it the five
/// venues whose credentials name no account (binance, bybit, okx, deribit, dukascopy) could never
/// report a shared book, and `vike-cli secrets set-book` would change no decision.
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

/// **What a shared BOOK does to the ACCOUNT-aggregate exposure ceiling, said in the shared-book
/// warning** — empty when that ceiling is unarmed.
///
/// ⚠ **The ceiling MULTIPLIES on exactly this shape, and nothing else says so.**
/// `vike_model::RiskLimits::max_account_exposure` is armed per ENGINE, one engine per
/// `(venue, AccountLabel)`; when `vike_config::venue_accounts`' shared-book rule fires (an agent
/// key plus its master's key, or one credential set under two labels) each engine applies the
/// whole ceiling to its half of ONE ledger — the N×-looser defect again, wearing the account label.
///
/// A SENTENCE, not a refusal (`docs/decisions/0013-degrade-vs-refuse.md`): the pair may be what the
/// operator meant. Pure so it is testable: the `warn!` itself is unreachable from a test (it needs
/// a real live arm), which [`crate::make_engine_accounts`]' comment declares a measured blind spot.
#[must_use]
pub fn shared_book_ceiling_note(max_account_exposure: Option<f64>) -> String {
    match max_account_exposure {
        // "at least": the report is per PAIR; three accounts on one book make a higher multiple.
        Some(cap) => format!(
            " ⚠ max_account_exposure ({cap}) is enforced PER ENGINE, so these two carry it \
             SEPARATELY over the one book they share — it may hold at least {} before either \
             refuses.",
            cap * 2.0
        ),
        None => String::new(),
    }
}

/// [`venue_account_arming`] over an already-parsed store (see [`accounts_in_store`]).
fn account_arming_in(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    store: &[vike_model::accounts::account_keys::AccountRef],
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
            // For a LABELLED account with no line, `Disarmed` names the wrong cause (the venue may
            // be `live`); it would send the operator to the `policy.venues` row they already set.
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

/// Whether a ceiling permits the REAL-MONEY tier — [`vike_config::VenueMode::cap`] (`min`, never
/// `max`) asked about `Live`. Named, not open-coded: [`crate::make_engine_for_account`]'s
/// `live_permitted`, [`would_mount_live_under`]'s per-row conjunct and a contract row's
/// `credential_probe` all need it, and an open-coded copy with no ceiling once sent a
/// `demo`-capped binance a SIGNED MAINNET balance read at startup while the mount bound demo.
///
/// It is the whole of what keeps a `demo` ceiling off MAINNET for venues whose network IS the
/// ceiling (decision 0095): each bridge reads it as `MountInputs::live_permitted`, which the mount
/// ([`crate::make_engine_for_account`]) and the projection (`crate::contract`'s `contract_arming`)
/// both compute here.
pub(crate) fn ceiling_permits_live(ceiling: vike_config::VenueMode) -> bool {
    ceiling.cap(vike_config::VenueMode::Live) == vike_config::VenueMode::Live
}

/// [`would_mount_live_under`] over a deployment's policy — the spelling every caller that HOLDS a
/// policy uses, so the preflight and the mount cannot disagree. A per-caller `policy.map_or(…)`
/// would be the second copy where the `None`-means-`Live` mistake lives.
pub fn would_mount_live_under_policy(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> bool {
    // ⚠ NOT `would_mount_live_under` (no policy): the DEFAULT account can DECLINE a
    // process-exclusive resource (`crate::exclusive`'s `holder`), and a preflight must not
    // authenticate for a venue the mount leaves on paper.
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

/// [`would_mount_live`] under a deployment's per-venue ARMING CEILING — what the ceiling seam,
/// [`crate::make_engine_for_account`], would reach: the ceiling changes which tier it resolves.
/// "Live" means **not paper**: a real authenticated session at whichever TIER (the budget rule asks
/// whether a session is established for the operator, not which endpoint it dials).
/// `ceiling == Paper` is `false` outright ([`venue_arming_under`] short-circuits).
///
/// ⚠ **DERIVED, not a second copy**: the coarse projection of [`venue_arming_under`] (the finer
/// "which TIER, and what holds it back" the arming screen renders). ONE row set with thin wrappers
/// is the point — every row mirrors its arm's own loader, and a second copy is the drift that
/// prevents. [`would_mount_live`] is the wrapper at the widest ceiling, the un-capped "would these
/// CREDENTIALS have armed it" that `report_capped_to_paper` and `venue_arming_migration_message`
/// need to say whether the ceiling REFUSED something.
///
/// ⚠ **The preflight is NOT a legitimate uncapped caller**
/// ([`crate::startup::run_startup_preflight`] takes [`would_mount_live_under_policy`], threaded
/// from `crate::build_node`): its clock leg's ig
/// row is a CREDENTIALED read (`crates/bridges/ig/src/mount.rs`'s `IgVenueMount::server_time_ms`
/// sends `X-IG-API-KEY`) and its credential leg a SIGNED `fetch_balance` per venue — against an
/// account the operator capped to `paper`.
///
/// ⚠ **Decision 0095: for binance/bybit/okx/hyperliquid a `Live` probe with demo-only keys answers
/// `Paper`** (they refuse to sign mainnet with demo keys). A
/// caller asking "credentials for SOME tier?" must probe `Demo` too —
/// `crate::paper_fallback`'s `would_mount_under_some_tier`.
///
/// ⚠ **`pub` for `vike-tradehub`'s roster tests** (`crates/vike-tradehub/tests/mount_roster.rs`).
/// The live sentinel's pre-mount set is NOT computed here: `crate::armed_live_venues` walks
/// `venue_account_arming`, the per-ACCOUNT form of the same projection.
#[must_use]
pub fn would_mount_live_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
) -> bool {
    venue_arming_under(registry, venue, vars, ceiling).0 != vike_config::VenueMode::Paper
}

/// **THE per-venue arming projection: which tier `make_engine` would reach for `venue` under
/// `ceiling`, and what holds it below that ceiling** — PURELY from the vars map and this binary's
/// registry (an uncompiled bridge is a `FeatureAbsent` row), with NO network I/O or side effects.
///
/// A row that stops matching its arm's loader fails the probe tests; a NEW live arm without a row
/// is still caught by [`crate::make_engine`]'s post-merge budget backstop. ⚠ An unknown venue (no
/// ROSTER venue today) answers `(Paper, NoLiveArm)`: it only produces `crate::contract`'s
/// `absent_parts` paper client, which the budget rule leaves unbounded. INTENT semantics, as
/// [`would_mount_live`]: the arming SCREEN reports what the mount will attempt. `ceiling == Paper`
/// answers `(Paper, Disarmed)` outright.
///
/// ⚠ **It threads NO policy**, so the `account` table reads UNREAD: the one row reading more than
/// the ceiling (dukascopy's) resolves the DEFAULT account without it, where the mount
/// ([`crate::make_engine_for_account`]) reads `env.policy`'s. A caller that HOLDS a policy uses the
/// exact [`would_mount_live_under_policy`].
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
/// ⚠ **The refusal was SILENT, and a silent refusal of a live-trading account is indistinguishable
/// from support.** `VenueDeclaration::addresses_accounts == false` makes [`account_arming_under`]
/// return [`vike_config::ArmingBlock::NoAccountSupport`] and [`crate::accounts_to_mount`] drop the
/// row, so [`crate::make_engine_for_account`] (where [`crate::report_capped_to_paper`] and
/// [`crate::venue_arming_migration`] speak) is never reached. MEASURED before this existed, on
/// two-dukascopy-account settings: three mount lines, none naming dukascopy; the row's own sentence
/// (`vike_config::VenueArming::why`) had no surface on the daemon
/// (`vike_config::venue_arming::ceilings_only` is the DEFAULT account only).
///
/// ⚠ **A REPORT, never a refusal** (`docs/decisions/0013-degrade-vs-refuse.md`, shaped after
/// [`crate::venue_arming_migration`]): the mount continues on each default account. It becomes a
/// refusal only for a strategy mount that NAMES such an account
/// (`crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`): a strategy
/// silently on the default account trades a book its author did not choose.
///
/// ⚠ **Keyed on the BLOCK, not a venue name**: built from whichever rows [`venue_arming`]
/// classifies `NoAccountSupport`. ⚠ **Today it names NOTHING** (dukascopy left the class when it
/// joined the then hand-written `arm_addresses_accounts` list); kept because `just new-venue`
/// scaffolds `addresses_accounts: false`, and
/// `crates/vike-tradehub/tests/unaddressable_account_warning.rs` proves the TEXT on a planted row.
///
/// Both ways of naming an account count: a `policy.accounts.<venue>.<LABEL>` line, and `…__<LABEL>`
/// store keys with no line (`vike_model::accounts::account_keys::accounts_in_store`). MEASURED: a
/// store holding `DUKASCOPY_DEMO_LOGIN__SECOND` enumerates as a real account, while the mount reads
/// the unlabelled keys. `policy` `None` answers `None` (nothing could have been named).
#[must_use]
pub fn unaddressable_accounts_message(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Option<String> {
    let policy = policy?;
    // THE SAME PROJECTION THE MOUNT SELECTS WITH, so the warning cannot describe a refusal the
    // fan-out did not make; one roster walk makes it a single process-wide message.
    unaddressable_accounts_text(&venue_arming(registry, vars, policy))
}

/// **The TEXT half of [`unaddressable_accounts_message`]**, over rows the caller already has.
///
/// ⚠ **Split out so a test can reach it at all**: no roster venue produces a
/// [`vike_config::ArmingBlock::NoAccountSupport`] row, so through the public entry point the
/// message is untestable — and a message nobody can produce rots unnoticed.
/// `crates/vike-tradehub/tests/unaddressable_account_warning.rs` drives THIS function with a
/// planted row. Public for that test; it takes rows only, so no store, credential or venue name is
/// reachable from here.
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
/// Same idiom as [`crate::venue_arming_migration`]: a process-wide fact noticed per venue.
/// Emitted from the FAN-OUT, not [`crate::make_engine_for_account`]: a refused account never
/// reaches the per-account path, and the refused VENUE may not be mounted at all (a scaffolded
/// venue has no `vike_tradehub::wired_markets::WIRED_MARKETS` row), so a line there is never read.
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
/// ⚠ **A LABELLED account is refused outright on a venue declaring `addresses_accounts: false`**,
/// or one the registry does not carry, before `resolve` is consulted ([`account_arming_raw`],
/// `crate::contract`'s `contract_arming`). A bridge that CAN address one threads `label` into the
/// loader its `mount` calls, so projection and mount read the same account's keys.
pub(crate) fn account_arming_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    ceiling: vike_config::VenueMode,
    policy: Option<&MountPolicy>,
) -> (vike_config::VenueMode, vike_config::ArmingBlock) {
    let raw = account_arming_raw(registry, venue, label, vars, ceiling, policy);
    // THE ONE-SIDECAR OVERRIDE, ABOVE every row, only where a venue declares a process-exclusive
    // resource: a fact about which OTHER account gets it, not about this account's credentials.
    // `crate::make_engine_for_account` consults the SAME function. When they disagreed, the
    // projection said `Demo` for both accounts, the mount built paper for one, and a strategy
    // naming it passed `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`
    // and ran on paper believing it was armed.
    if raw.0 != vike_config::VenueMode::Paper
        && crate::contract::is_process_exclusive(crate::row_of(registry, venue))
        && !crate::exclusive::holds(registry, venue, label, vars, policy)
    {
        return (vike_config::VenueMode::Paper, vike_config::ArmingBlock::SidecarHeldElsewhere);
    }
    raw
}

/// **The `account` table a caller's policy carries**, or the UNREAD one with no policy — the ONE
/// spelling of that fallback. A `match`, not `map_or_else(AccountDirectory::unread_ref, …)`, which
/// unifies the arms at `'static` and fails to borrow-check.
pub(crate) fn directory_of(
    policy: Option<&MountPolicy>,
) -> &vike_bridge_core::account_directory::AccountDirectory {
    match policy {
        Some(p) => &p.accounts,
        None => vike_bridge_core::account_directory::AccountDirectory::unread_ref(),
    }
}

/// [`account_arming_under`] WITHOUT the one-exclusive-resource override: a contract row through its
/// bridge's `resolve`, every other row by the fixed answers below. Besides that wrapper, only
/// `crate::exclusive`'s `holder` calls it: "would this account arm on its own merits", without
/// asking the override again.
///
/// ⚠ `arm_addresses_accounts` (the hand-written list) is gone: each bridge declares
/// `VenueDeclaration::addresses_accounts`, and an unknown venue addresses none.
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
        // A labelled account reaches the ceiling check too (as the feature-off rows always did).
        Some(crate::VenueRow::FeatureAbsent { .. }) => {
            if ceiling == Mode::Paper {
                (Mode::Paper, Block::Disarmed)
            } else {
                (Mode::Paper, Block::FeatureAbsent)
            }
        }
        // Not in the registry: a labelled account first (no named account here), then the
        // ceiling, then "no live arm".
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

/// **The arming screen's whole data source**: one [`vike_config::VenueArming`] row per ACCOUNT of
/// each `vike_model::VENUES` id, pairing the operator's ceiling with what [`venue_arming_under`] —
/// and therefore [`crate::make_engine_for_account`] — would do with it. The row type lives in
/// `vike-config` so no GUI re-derives the answer; callers are the daemon's, once per command or
/// start (`crates/vike-tradehub/src/venues_cli.rs`,
/// `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`, passing `vike-tradehub`'s `REGISTRY`),
/// and [`unaddressable_accounts_message`].
///
/// No network, no clock, no process env (decision 0095: every bridge's `resolve` reads only the
/// ceiling and the vars map's key presence).
///
/// ⚠ **Pure because the `account` table arrives as DATA**: a dukascopy account's keys are a fact
/// about its `account` ROW, which its bridge's `resolve`
/// (`crates/bridges/dukascopy/src/mount.rs`'s `resolve_in`) reads from `MountPolicy::accounts`, so
/// this opens no store; no table means the UNREAD one (a `Backend::Absent` box's answer). A test
/// may drive it from a fixture map. `vike-desktop` links no mount and renders
/// `vike_config::venue_arming::ceilings_only`.
/// ⚠ **One row per ACCOUNT, not per venue** — a box with no `policy.accounts` rows and no labelled
/// key still gets exactly one row per roster venue (the DEFAULT account's).
///
/// ⚠ **It takes no wired-market table**: that existed only for the deleted SYMBOL-COLLISION rule
/// (`vike_config::venue_accounts`' module doc), and would reach no answer while looking as if it
/// did.
#[must_use]
pub fn venue_arming(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: &MountPolicy,
) -> Vec<vike_config::VenueArming> {
    // ONE store parse for the whole roster walk — see [`accounts_in_store`].
    let store = accounts_in_store(vars);
    policy
        .venues
        .iter()
        .flat_map(|(venue, _)| account_arming_in(registry, venue, vars, &store, Some(policy)))
        .collect()
}

// No `ceiling_selects_mainnet`/`cex_cred_choice` here: each venue whose network IS the ceiling
// (decision 0095) decides in its bridge from `MountInputs::live_permitted` (`ceiling_permits_live`)
// plus the live keys it finds, and stays paper rather than sign mainnet with demo keys.

/// The `Account` per-symbol RULING-margin-mode grid for the ONE mounted symbol — the margin-axis
/// twin of `multiplier_grid` (kept separate: different source, `onlyIsolated` vs contract size).
/// `None` whenever the mode IS `Cross`, so `Account::default_margin_mode_of` short-circuits to
/// [`vike_model::MarginMode::Cross`]: byte-identical and allocation-free for every mount but
/// hyperliquid's (a `Cross` row and no row are indistinguishable through the accessor).
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

/// The `Account` per-symbol contract-multiplier grid for the ONE mounted symbol. `None` (so
/// `Account::multiplier_of` falls through to its `1.0` default) when the venue reported no usable
/// contract size (spot crypto, FX, linear perps) or exactly `1.0`: identical through
/// `multiplier_of`, and the snapshot's `multipliers` map stays empty.
pub(crate) fn multiplier_grid(
    symbol: &str,
    contract_size: f64,
) -> Option<indexmap::IndexMap<String, f64>> {
    // The model's ONE degenerate-folding rule (non-finite, non-positive → 1.0).
    let m = vike_model::SymbolProperties { contract_size, ..Default::default() }.multiplier();
    (m != 1.0).then(|| {
        let mut grid = indexmap::IndexMap::new();
        grid.insert(symbol.to_string(), m);
        grid
    })
}

/// The generic fee-schedule enrichment hook: prefers the venue's LIVE account-actual rate
/// (`ReconClient::fetch_fee_rates`, overridden by binance/bybit/okx/deribit) over `static_default`
/// (the once-evaluated [`vike_model::fee_schedule_for`], shared with the paper fill path).
/// **Fail-soft**: an error, `Ok(None)` or no recon client falls back to the default — a fee fetch
/// never blocks mounting. One blocking, best-effort account read at mount time.
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
