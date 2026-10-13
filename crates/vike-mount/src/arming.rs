//! Who is armed, for what, and at what fee/margin/policy — the per-account/per-venue arming
//! decisions `make_engine_for_account` consults (`make_engine*` live in `engine.rs`, the operator
//! risk budget in `budget.rs`). The `pub` probes are public for the roster tests in
//! `crates/vike-tradehub/tests/mount_roster.rs`, which run where the registry is
//! (docs/decisions/0096).

use std::collections::HashMap;

use vike_secrets::account_credentials::AccountCredentials;

use crate::{AccountLabel, MountPolicy};

pub(crate) use crate::budget::require_live_risk_budget;

/// PRE-CONNECT live-intent probe: would [`crate::make_engine`] build a LIVE exec client for
/// `venue`'s DEFAULT account, holding `credentials`? Judged PURELY from the account's credentials
/// and the deployment map through the bridge's own `VenueMount::resolve` (the answer its mount
/// acts on, so the two cannot drift), with NO network I/O or side effects — what
/// lets [`require_live_risk_budget`] refuse BEFORE any venue session exists. A `FeatureAbsent` row
/// and an unknown id probe `false`: a `demo`/`live` account of either does not run.
///
/// INTENT, not outcome: `true` means the operator SUPPLIED this venue's live config, even where the
/// mount later does not come up (ctrader/ibkr connect failure, hyperliquid/polymarket declining a
/// bad key: the account then does NOT RUN, `ArmingBlock::MountFailed`). An intended-live session
/// must carry a bounded budget; a config the operator never wrote can never refuse a mount.
#[must_use]
pub fn would_mount_live(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    credentials: AccountCredentials<'_>,
) -> bool {
    would_mount_live_under(registry, venue, vars, credentials, vike_model::AccountTier::Live)
}

/// The tier of one venue's DEFAULT account — [`account_tier`] at [`AccountLabel::Default`], for the
/// callers that ask about a venue rather than an account (the startup preflight's legs). `None`
/// when no active row states one: that account does not run, so no probe asks anything of it.
pub(crate) fn venue_tier(
    policy: Option<&MountPolicy>,
    venue: &str,
) -> Option<vike_model::AccountTier> {
    account_tier(policy, venue, &AccountLabel::Default).ok()
}

/// **The tier one ACCOUNT of one venue runs at, from the `account` table** — **the one place
/// `Option<&MountPolicy>` becomes a [`vike_model::AccountTier`]**, so no caller invents its own
/// answer for a policy-less mount. `Ok(tier)` when the active rows state exactly one tier (`paper`
/// included: that account runs the paper simulator), else `Err(why)` — the account does NOT RUN
/// (decision 0156), and nothing below reads a credential for it.
///
/// Over the rows of `venue` whose `label` equals the account's label text (`NULL` for the DEFAULT
/// account), `vike_secrets::Accounts::active_tier` answers:
///
/// * no policy, or a table nobody read (`MountPolicy::accounts` unread, or a box with no store):
///   `NoAccountRow`. NOT RUNNING is the fail-safe end: the widening mistake has to be typed.
/// * a store that exists and would not open: `AccountNotInStore` (loud at the root).
/// * no row: `NoAccountRow`; rows, none active: `AccountInactive`; every active row `paper`:
///   `Ok(Paper)`.
/// * exactly ONE non-paper tier among the active rows (other active rows may be `paper`, and two
///   rows of the SAME tier are fine — dukascopy's two demo books): that tier.
/// * TWO non-paper tiers active (`demo` AND `live` for one account): `TierConflict`. No pick: the
///   engine route key and the live lock carry no tier, so one process mounts one engine per
///   account, and choosing between a demo and a mainnet key set is the operator's to make
///   (`vike-cli secrets account deactivate --id N` on one of them).
/// * ⚠ **A VENUE-scoped tier** (`vike_venues::venue_tier_scope` answers `TierScope::Venue`: ibkr,
///   decision 0140) adds one rule: an account that would arm by the rows of its own label does not
///   run, `TierConflict`, when the venue's active rows of ANY label state two non-paper tiers
///   (`vike_secrets::Accounts::active_tier_of_venue`). The box rides ONE session of that venue, so
///   an unlabelled `demo` row beside a labelled `live` one arms neither. An account whose own rows
///   already refuse it keeps that cause, and a `paper` row still runs the simulator (it opens no
///   session). Every other venue is 0119's per-label rule alone.
///
/// ⚠ **It only SELECTS; the bridge still decides whether it can arm.** The tier reaches the bridge
/// AS IS, as `MountInputs::tier` (decision 0129) — the mount ([`crate::make_engine_for_account`]),
/// the projection (`crate::contract`'s `contract_arming`), the clock read and the startup credential
/// probe all hand over this answer, and none recomputes a `tier == live` bit of its own (an
/// open-coded copy of that bit once sent a `demo` binance a SIGNED MAINNET balance read at startup
/// while the mount bound demo). For binance, bybit, okx and hyperliquid it is the whole of what
/// keeps a `demo` account off MAINNET. A `live` account whose bridge can only bind DEMO does not
/// run (`crate::contract`'s `resolution_to_arming`, the no-downgrade rule).
pub fn account_tier(
    policy: Option<&MountPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> Result<vike_model::AccountTier, vike_config::ArmingBlock> {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    use vike_secrets::ActiveTier;

    let Some(policy) = policy else { return Err(Block::NoAccountRow) };
    let accounts = match policy.accounts.rows() {
        None => return Err(Block::NoAccountRow),
        Some(Err(_)) => return Err(Block::AccountNotInStore),
        Some(Ok(accounts)) => accounts,
    };
    let tier = match accounts.active_tier(venue, label.text()) {
        // `Unanswerable`: a box with no `account` table has no row to state a tier.
        None | Some(ActiveTier::NoRow) => return Err(Block::NoAccountRow),
        Some(ActiveTier::Inactive) => return Err(Block::AccountInactive),
        Some(ActiveTier::Conflict) => return Err(Block::TierConflict),
        Some(ActiveTier::Tier(tier)) => tier,
    };
    // A VENUE-scoped tier (decision 0140): an account that would arm does not run when the venue's
    // rows of ANY label state two tiers. A `paper` row runs the simulator whatever the others say.
    if tier != Mode::Paper && venue_conflicts(accounts, venue) {
        return Err(Block::TierConflict);
    }
    Ok(tier)
}

/// **The accounts of `venues` that `flags.tradehub_live` OFF stops** (decision 0156 item 4): the
/// switch is a STOP, never a demotion, so every account whose active row states `demo` or `live`
/// does NOT RUN with [`vike_config::ArmingBlock::LiveSwitchOff`] as its reason, and an account the
/// table itself does not run (two active tiers, none active, an unreadable table) carries the
/// table's own cause. A `paper` account is not listed: it runs the paper simulator wherever a
/// strategy mounts it.
///
/// Read off the `account` table ALONE — the switch-off daemon reads no credential — over the labels
/// its rows name (the DEFAULT account's are the unlabelled rows). An account with no row at all is
/// not listed: nothing was asked of it, and the daemon's ready banner already says the box is off.
/// `vike-tradehub`'s paper arm publishes the answer and refuses a strategy mount on any of them.
#[must_use]
pub fn stopped_by_the_live_switch(venues: &[&str], policy: &MountPolicy) -> Vec<crate::NotRunning> {
    let Some(Ok(accounts)) = policy.accounts.rows() else { return Vec::new() };
    let Some(rows) = accounts.known() else { return Vec::new() };
    let mut out: Vec<crate::NotRunning> = Vec::new();
    for venue in venues {
        let mut labels: Vec<AccountLabel> = rows
            .iter()
            .filter(|row| row.venue == *venue)
            .filter_map(|row| match row.label.as_deref() {
                None => Some(AccountLabel::Default),
                Some(text) => AccountLabel::parse(text).ok(),
            })
            .collect();
        labels.sort();
        labels.dedup();
        for label in labels {
            let (tier, block) = match account_tier(Some(policy), venue, &label) {
                Ok(vike_model::AccountTier::Paper) => continue,
                Ok(tier) => (Some(tier), vike_config::ArmingBlock::LiveSwitchOff),
                Err(block) => (None, block),
            };
            out.push(crate::NotRunning {
                venue: (*venue).to_string(),
                account: label,
                tier,
                block,
            });
        }
    }
    out
}

/// What a `paper` tier answers in a projection: the paper simulator runs it.
pub(crate) const PAPER_RUNS: (Option<vike_model::AccountTier>, vike_config::ArmingBlock) =
    (Some(vike_model::AccountTier::Paper), vike_config::ArmingBlock::PaperTier);

/// Whether `venue`'s tier is judged over EVERY label (`vike_venues::TierScope::Venue`) and its
/// active rows, of any label, state two non-paper tiers
/// (`vike_secrets::Accounts::active_tier_of_venue`).
/// Always `false` for an account-scoped venue: there 0119's per-label rule is the whole rule.
fn venue_conflicts(accounts: &vike_secrets::Accounts, venue: &str) -> bool {
    vike_venues::venue_tier_scope(venue) == vike_venues::TierScope::Venue
        && accounts.active_tier_of_venue(venue) == Some(vike_secrets::ActiveTier::Conflict)
}

/// **The ACTIVE `account.id`s of `(venue, label)` behind an arming answer** — what
/// `vike_config::VenueArming::account_ids` carries, so a remedy can name the `--id` to change.
///
/// * a stated tier (`demo`, `live`, or every active row `paper`): the active rows of that tier;
/// * `TierConflict`: the active NON-paper rows — the ones to choose between; for a VENUE-scoped
///   tier (`vike_venues::TierScope::Venue`, decision 0140) those of EVERY label of the venue;
/// * anything else (no row, none active, no table): empty.
///
/// Keyed on the TABLE's answer ([`account_tier`]), not the bridge's: a `NoCredentials` or
/// `NoAccountSupport` row still names the row whose keys or venue fell short.
pub(crate) fn account_ids(
    policy: Option<&MountPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> Vec<i64> {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    let answer = account_tier(policy, venue, label);
    let Some(Ok(accounts)) = directory_of(policy).rows() else { return Vec::new() };
    let Some(rows) = accounts.known() else { return Vec::new() };
    let wanted = |row_tier: Mode| match answer {
        Ok(tier) => row_tier == tier,
        Err(Block::TierConflict) => row_tier != Mode::Paper,
        Err(_) => false,
    };
    // A venue-scoped conflict is a fact about the rows of EVERY label: its remedy names them all.
    let any_label = answer == Err(Block::TierConflict)
        && vike_venues::venue_tier_scope(venue) == vike_venues::TierScope::Venue;
    rows.iter()
        .filter(|a| a.active && a.venue == venue)
        .filter(|a| any_label || a.label.as_deref() == label.text())
        .filter(|a| wanted(a.tier))
        .map(|a| a.id)
        .collect()
}

/// **This account's own exposure ceiling**: the tightest `account.max_exposure` among the ACTIVE
/// rows of `(venue, label)` (`vike_secrets::Accounts::max_exposure_of`), `None` = unbounded, or no
/// table. Folded with `vike_model::RiskLimits::narrow_account_exposure` (a `min`) AFTER the box-wide
/// `policy.max_account_exposure`, by the live assembly and the paper engine alike, so it can only
/// TIGHTEN that one.
pub(crate) fn account_max_exposure(
    policy: Option<&MountPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> Option<f64> {
    let Some(Ok(accounts)) = directory_of(policy).rows() else { return None };
    accounts.max_exposure_of(venue, label.text()).map(vike_secrets::MaxExposure::get)
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
    vike_model::accounts::account_keys::AccountRef { venue: id, label: label.clone() }.route_key()
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
/// is called UNCONDITIONALLY and a box with no labelled account keeps `route_key: None` off the
/// wire (`skip_serializing_if`): journal bytes unchanged.
pub(crate) fn account_event_sender(
    live_events: &vike_exec::EventSender,
    route_key: &str,
) -> vike_exec::EventSender {
    live_events.routed(route_key)
}

/// **Every account of `venue` this box knows about**, default first (always listed, so a box with
/// an empty store still SAYS why its default account does not run), then labels in order: the `label`s of this venue's
/// `account` ROWS, active or not — an inactive labelled row must be listed (as
/// `AccountInactive`), and a label with no keys too, or a typo would vanish instead of showing as
/// `NoCredentials`.
///
/// ⚠ **The account ROWS are the one source**: a credential belongs to its account row
/// (`credential.account_id`, decision 0156 item 6), so no account is read out of a credential's
/// NAME.
#[must_use]
pub fn known_accounts(venue: &str, policy: Option<&MountPolicy>) -> Vec<AccountLabel> {
    let mut labels: Vec<AccountLabel> = Vec::new();
    if let Some(Ok(accounts)) = directory_of(policy).rows()
        && let Some(rows) = accounts.known()
    {
        // A label the label grammar refuses names no addressable account: skipped.
        labels.extend(
            rows.iter()
                .filter(|a| a.venue == venue)
                .filter_map(|a| a.label.as_deref())
                .filter_map(|l| AccountLabel::parse(l).ok()),
        );
    }
    labels.retain(|l| !l.is_default());
    labels.sort();
    labels.dedup();
    // Default FIRST (`make_engine_accounts`'s order contract: callers bind `[0]`).
    let mut out = vec![AccountLabel::Default];
    out.extend(labels);
    out
}

/// **THE per-account arming projection for one venue** — the rows `vike-backend venues` renders AND
/// the selection [`crate::make_engine_accounts`] mounts from, so "Effective" cannot disagree with
/// the mount. Each row is the account's tier from the `account` table ([`account_tier`]), then
/// [`account_arming_under`] at that tier; an account the TABLE does not run carries its cause
/// (`NoAccountRow`, `AccountInactive`, `TierConflict`, `AccountNotInStore`), a `paper` one
/// `PaperTier`, and for neither is its bridge asked.
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
/// ⚠ **It takes the whole [`MountPolicy`]**: the tier AND the dukascopy row's ACCOUNT both come out
/// of [`MountPolicy::accounts`] (the `account` table the composition root read), and
/// [`crate::make_engine_for_account`] reads the same object — one snapshot, two consumers.
#[must_use]
pub fn venue_account_arming(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::VenueArming> {
    account_arming_in(registry, venue, vars, policy)
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
/// [`vike_config::ArmedBook`], so "warn nothing where you cannot tell" holds by ABSENCE — and so
/// does an account that does not run (no effective tier: it has no book this process trades).
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
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::SharedBook> {
    let directory = directory_of(policy);
    let books: Vec<vike_config::ArmedBook> = rows
        .iter()
        .filter_map(|r| {
            let effective = r.effective?;
            let book = crate::book_identity::book_of_account(
                registry, r.venue, &r.label, effective, directory,
            )?;
            Some(vike_config::ArmedBook {
                venue: r.venue,
                label: r.label.clone(),
                book,
                mode: effective,
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

/// [`venue_account_arming`]'s body.
fn account_arming_in(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> Vec<vike_config::VenueArming> {
    let Some(venue) = vike_model::VENUES.iter().copied().find(|v| *v == venue) else {
        return Vec::new();
    };
    known_accounts(venue, policy)
        .into_iter()
        .map(|label| {
            let (tier, effective, block) = account_arming(registry, venue, &label, vars, policy);
            let account_ids = account_ids(policy, venue, &label);
            vike_config::VenueArming { venue, label, tier, effective, block, account_ids }
        })
        .collect()
}

/// **One account's whole arming answer** — `(tier, effective, block)`: its tier from the table
/// ([`account_tier`]), then [`account_arming_under`] at that tier. `effective` is `None` when the
/// account does NOT RUN, and `tier` is `None` when no active row states one.
///
/// The order is load-bearing: (1) a LABELLED account on a venue that cannot address one is
/// `NoAccountSupport` whatever its row says (the generic precondition, [`refuses_the_label`] —
/// inside [`account_arming_raw`] too — answers before the tier is read); (2) an account the TABLE
/// does not run carries the table's own cause and never reaches the bridge, and a PAPER tier runs
/// the simulator without reaching it either (`crate::contract`'s `contract_arming` short-circuits);
/// (3) else the bridge's `resolve` under `MountInputs::tier = tier`, and (4) the preflight stop and
/// the process-exclusive override ([`account_arming_under`]).
pub(crate) fn account_arming(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> (Option<vike_model::AccountTier>, Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    match account_tier(policy, venue, label) {
        Ok(tier) => {
            let (effective, block) =
                account_arming_under(registry, venue, label, vars, tier, policy);
            (Some(tier), effective, block)
        }
        // The table runs nothing for it — but the label precondition still answers first: no row
        // could make a labelled account of a one-account venue addressable.
        Err(_) if refuses_the_label(registry, venue, label) => {
            (None, None, vike_config::ArmingBlock::NoAccountSupport)
        }
        Err(why) => (None, None, why),
    }
}

/// **The generic precondition every projection answers BEFORE the tier**: a LABELLED account on a
/// venue whose mount cannot address one (a contract row declaring `addresses_accounts: false`, or
/// an id the registry does not carry). A `FeatureAbsent` row is judged by its tier instead, as it
/// always was. `crate::contract`'s `contract_arming` and [`account_arming_raw`] ask the same
/// question in the same place.
fn refuses_the_label(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
) -> bool {
    !label.is_default()
        && match crate::row_of(registry, venue) {
            Some(crate::VenueRow::Mount(row)) => !row.declaration().addresses_accounts,
            Some(crate::VenueRow::FeatureAbsent { .. }) => false,
            None => true,
        }
}

/// [`would_mount_live_under`] over a deployment's policy — the spelling every caller that HOLDS a
/// policy uses, so the preflight and the mount cannot disagree. A per-caller `policy.map_or(…)`
/// would be the second copy where the `None`-means-`Live` mistake lives. A DEFAULT account the
/// table does not run answers `false`: nothing would be authenticated for it.
pub fn would_mount_live_under_policy(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&MountPolicy>,
) -> bool {
    let Some(tier) = venue_tier(policy, venue) else { return false };
    // ⚠ NOT `would_mount_live_under` (no policy): the DEFAULT account can DECLINE a
    // process-exclusive resource (`crate::exclusive`'s `holder`), and a preflight must not
    // authenticate for a venue whose default account the mount does not run.
    account_arming_under(registry, venue, &AccountLabel::Default, vars, tier, policy)
        .0
        .is_some_and(|t| t != vike_model::AccountTier::Paper)
}

/// [`would_mount_live`] at a given account TIER — what [`crate::make_engine_for_account`] would
/// reach for the DEFAULT account at that tier: the tier changes which keys and network it resolves.
/// "Live" means **a venue client**: a real authenticated session at whichever TIER (the budget rule
/// asks whether a session is established for the operator, not which endpoint it dials). `tier ==
/// Paper` is `false` outright ([`venue_arming_under`] short-circuits).
///
/// ⚠ **DERIVED, not a second copy**: the coarse projection of [`venue_arming_under`] (the finer
/// "which TIER, and what holds it back" `vike-backend venues` renders). ONE row set with thin
/// wrappers is the point — every row mirrors its arm's own loader, and a second copy is the drift
/// that prevents. [`would_mount_live`] is the wrapper at the `live` tier, the "would these
/// CREDENTIALS arm it" that `report_not_running` needs to say whether the account table held back
/// something the keys would have armed.
///
/// ⚠ **The preflight is NOT a legitimate tier-less caller**
/// ([`crate::startup::run_startup_preflight`] takes [`would_mount_live_under_policy`], threaded
/// from `crate::build_node`): its clock leg's ig
/// row is a CREDENTIALED read (`crates/bridges/ig/src/mount.rs`'s `IgVenueMount::server_time_ms`
/// sends `X-IG-API-KEY`) and its credential leg a SIGNED `fetch_balance` per venue — against an
/// account the operator left at `paper`.
///
/// ⚠ **A `Live` probe with demo-only keys answers `false`**: binance/bybit/okx/hyperliquid refuse
/// to sign mainnet with demo keys (decision 0095), and every other arm that would fall back to its
/// demo tier does not run under the no-downgrade rule (`crate::contract`'s `resolution_to_arming`:
/// a `live` account never trades demo). A caller asking "credentials for SOME tier?" must probe
/// `Demo` too — `crate::paper_fallback`'s `would_mount_under_some_tier`.
///
/// ⚠ **`pub` for `vike-tradehub`'s roster tests** (`crates/vike-tradehub/tests/mount_roster.rs`).
/// The live sentinel's pre-mount set is NOT computed here: `crate::armed_live_venues` walks
/// `venue_account_arming`, the per-ACCOUNT form of the same projection.
#[must_use]
pub fn would_mount_live_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    credentials: AccountCredentials<'_>,
    tier: vike_model::AccountTier,
) -> bool {
    venue_arming_under(registry, venue, vars, credentials, tier)
        .0
        .is_some_and(|t| t != vike_model::AccountTier::Paper)
}

/// **THE per-venue arming projection: which tier `make_engine` would run `venue`'s DEFAULT account
/// at, given `tier` and `credentials` (by FIELD, decision 0156 item 6), and what stops it when it
/// would not** — PURELY from those credentials, the deployment map and this binary's registry (an
/// uncompiled bridge is a `FeatureAbsent` row), with NO network I/O or side effects. `(None, why)`
/// is NOT RUNNING.
///
/// A row that stops matching its arm's loader fails the probe tests; a NEW live arm without a row
/// is still caught by [`crate::make_engine`]'s post-merge budget backstop. ⚠ An unknown venue (no
/// ROSTER venue today) answers `(None, NoLiveArm)`: a `demo`/`live` account of it does not run.
/// INTENT semantics, as [`would_mount_live`]: the projection reports what the mount will attempt.
/// `tier == Paper` answers `(Some(Paper), PaperTier)` outright: the paper simulator.
///
/// ⚠ **It threads NO policy**, so the `account` table reads UNREAD: the tier is the CALLER's, and
/// the one row reading more than the tier (dukascopy's) resolves the DEFAULT account without the
/// table, where the mount ([`crate::make_engine_for_account`]) reads `env.policy`'s. A caller that
/// HOLDS a policy uses the exact [`would_mount_live_under_policy`].
pub fn venue_arming_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    credentials: AccountCredentials<'_>,
    tier: vike_model::AccountTier,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    // No policy: the DEFAULT account holds any process-exclusive resource (`crate::exclusive`'s
    // `holder`), so the override `account_arming_under` adds can never fire here.
    account_arming_raw_with(
        registry,
        venue,
        &AccountLabel::Default,
        vars,
        Some(credentials),
        tier,
        None,
    )
}

/// **THE REFUSAL, SAID OUT LOUD: every account this box names on a venue whose mount addresses only
/// ONE** — one process-wide message, or `None` when there is nothing to say.
///
/// ⚠ **The refusal was SILENT, and a silent refusal of a live-trading account is indistinguishable
/// from support.** `VenueDeclaration::addresses_accounts == false` makes [`account_arming_under`]
/// return [`vike_config::ArmingBlock::NoAccountSupport`] and [`crate::accounts_to_mount`] drop the
/// row, so [`crate::make_engine_for_account`] is never reached. MEASURED before this existed, on
/// two-dukascopy-account settings: three mount
/// lines, none naming dukascopy; the row's own sentence (`vike_config::VenueArming::why`) had no
/// surface on the daemon.
///
/// ⚠ **A REPORT, never a refusal** (`docs/decisions/0013-degrade-vs-refuse.md`): the mount
/// continues on each default account. It becomes a refusal only for a strategy mount that NAMES
/// such an account
/// (`crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`): a strategy
/// silently on the default account trades a book its author did not choose.
///
/// ⚠ **Keyed on the BLOCK, not a venue name**: built from whichever rows [`venue_arming`]
/// classifies `NoAccountSupport`. ⚠ **Today it names NOTHING** (dukascopy left the class when it
/// joined the then hand-written `arm_addresses_accounts` list); kept because `just new-venue`
/// scaffolds `addresses_accounts: false`, and
/// `crates/vike-tradehub/tests/unaddressable_account_warning.rs` proves the TEXT on a planted row.
///
/// A labelled `account` row is what names an account. `policy` `None` answers `None` (nothing
/// could have been named).
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
        let rows = if row.account_ids.is_empty() {
            "No active account row names it".to_string()
        } else {
            let ids: Vec<String> = row.account_ids.iter().map(i64::to_string).collect();
            format!("Its account row (id {}) arms nothing here", ids.join(", "))
        };
        out.push_str(&format!(
            "  {venue} account `{label}` — REFUSED. {venue} addresses exactly ONE account: its \
             DEFAULT one. No `{venue}#{label}` engine is built, no `…__{label}` credential key is \
             read by anything, and every {venue} order this process sends is signed with {venue}'s \
             UNLABELLED credential keys — the account a single-account box has always traded. \
             {rows}.\n"
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
/// A process-wide fact noticed per venue, latched once (the `crate::halt` resolver's
/// idiom).
/// Emitted from the FAN-OUT, not [`crate::make_engine_for_account`]: a refused account never
/// reaches the per-account path, and the refused VENUE may not be mounted at all (a scaffolded
/// venue has no `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` engine row), so a line there is never read.
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
///
/// Two box-level answers sit above the bridge's: a venue the startup preflight hard-FAILED
/// (`MountPolicy::preflight_failed`) runs none of its `demo`/`live` accounts (`PreflightFailed`),
/// and the process-exclusive override below.
pub(crate) fn account_arming_under(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    tier: vike_model::AccountTier,
    policy: Option<&MountPolicy>,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    use vike_config::ArmingBlock as Block;
    let raw = account_arming_raw(registry, venue, label, vars, tier, policy);
    // THE PREFLIGHT STOP (decision 0156 item 1): a hard preflight FAIL stops the venue's `demo` and
    // `live` accounts with that reason — not the `NoCredentials` its withheld keys would otherwise
    // read as. The label precondition still answers first, and a `paper` account still runs.
    if raw.1 != Block::NoAccountSupport
        && tier != vike_model::AccountTier::Paper
        && policy.is_some_and(|p| p.preflight_failed.contains(venue))
    {
        return (None, Block::PreflightFailed);
    }
    // THE ONE-SIDECAR OVERRIDE, ABOVE every row, only where a venue declares a process-exclusive
    // resource: a fact about which OTHER account gets it, not about this account's credentials.
    // `crate::make_engine_for_account` consults the SAME function. When they disagreed, the
    // projection said `Demo` for both accounts, the mount built paper for one, and a strategy
    // naming it passed `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`
    // and ran on paper believing it was armed.
    if raw.0.is_some_and(|t| t != vike_model::AccountTier::Paper)
        && crate::contract::is_process_exclusive(crate::row_of(registry, venue))
        && !crate::exclusive::holds(registry, venue, label, vars, policy)
    {
        return (None, Block::SidecarHeldElsewhere);
    }
    raw
}

/// **The `account` table a caller's policy carries**, or the UNREAD one with no policy — the ONE
/// spelling of that fallback. A `match`, not `map_or_else(AccountDirectory::unread_ref, …)`, which
/// unifies the arms at `'static` and fails to borrow-check.
pub(crate) fn directory_of(
    policy: Option<&MountPolicy>,
) -> &vike_secrets::account_directory::AccountDirectory {
    match policy {
        Some(p) => &p.accounts,
        None => vike_secrets::account_directory::AccountDirectory::unread_ref(),
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
    tier: vike_model::AccountTier,
    policy: Option<&MountPolicy>,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    account_arming_raw_with(registry, venue, label, vars, None, tier, policy)
}

/// [`account_arming_raw`] with the account's credentials SUPPLIED (`Some`) rather than resolved
/// from `policy`'s directory (`None`) — the policy-less probes' door ([`venue_arming_under`]).
fn account_arming_raw_with(
    registry: &'static [crate::VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    credentials: Option<AccountCredentials<'_>>,
    tier: vike_model::AccountTier,
    policy: Option<&MountPolicy>,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => {
            crate::contract::contract_arming(*row, label, vars, credentials, tier, policy)
        }
        // A labelled account reaches the tier check too (as the feature-off rows always did).
        Some(crate::VenueRow::FeatureAbsent { .. }) => {
            if tier == Mode::Paper {
                PAPER_RUNS
            } else {
                (None, Block::FeatureAbsent)
            }
        }
        // Not in the registry: a labelled account first (no named account here), then the
        // tier, then "no live arm".
        None => {
            if refuses_the_label(registry, venue, label) {
                (None, Block::NoAccountSupport)
            } else if tier == Mode::Paper {
                PAPER_RUNS
            } else {
                (None, Block::NoLiveArm)
            }
        }
    }
}

/// **The arming projection's whole data source**: one [`vike_config::VenueArming`] row per ACCOUNT
/// of each `vike_model::VENUES` id, pairing the tier the `account` table states with what
/// [`account_arming_under`] — and therefore [`crate::make_engine_for_account`] — would do with it.
/// The row type lives in `vike-config` so no renderer re-derives the answer; callers are the
/// daemon's, once per command or start (`crates/vike-tradehub/src/venues_cli.rs`,
/// `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`, passing `vike-tradehub`'s `REGISTRY`),
/// and [`unaddressable_accounts_message`].
///
/// No network, no clock, no process env (decision 0095: every bridge's `resolve` reads only
/// `MountInputs::tier`, the account's credentials and the deployment map).
///
/// ⚠ **Pure because the `account` table arrives as DATA**: each account's tier, and a dukascopy
/// account's keys, are facts about its `account` ROW, which this projection and its bridge's
/// `resolve` (`crates/bridges/dukascopy/src/mount.rs`'s `resolve_in`) read from
/// `MountPolicy::accounts`, so this opens no store; no table means the UNREAD one (no account
/// runs, a `Backend::Absent` box's answer). A test may drive it from a fixture map.
/// ⚠ **One row per ACCOUNT, not per venue** — a box with no labelled account row and no labelled
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
    vike_model::VENUES
        .iter()
        .flat_map(|venue| account_arming_in(registry, venue, vars, Some(policy)))
        .collect()
}

// No `cex_cred_choice` here: each venue whose network IS the tier (decision 0095) decides in its
// bridge from `MountInputs::tier` plus the live keys it finds, and
// declines (the account does not run) rather than sign mainnet with demo keys.

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
/// (the once-evaluated [`vike_venues::fee_schedule_for`], shared with the paper fill path).
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
