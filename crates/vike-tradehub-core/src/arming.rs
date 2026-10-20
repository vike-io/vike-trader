//! Who is armed, for what, and at what fee/margin/policy — the per-account/per-venue arming
//! decisions `make_engine_for_account` consults (`make_engine*` live in `engine.rs`, the operator
//! risk budget in `budget.rs`).
//!
//! ⚠ **This file reads the [`ArmingTable`], it never asks a bridge** (wave 2, W9a). The table is
//! one snapshot per assembly ([`crate::VenueBackend::arming_table`]) and every reader below lays the
//! startup preflight's stop over it the way `account_arming_under` always did: a venue the preflight
//! hard-FAILED stops its `demo`/`live` accounts with [`vike_config::ArmingBlock::PreflightFailed`],
//! ABOVE the one-sidecar override and below the label precondition. The registry-driven projection —
//! `would_go_live`, `venue_arming_under`, the per-account probes — is the venue half's
//! (`crates/vike-gatewayhub/src/trade/adapters/arming.rs`), which the roster tests in
//! `crates/vike-gatewayhub/tests/trade_roster/mount_roster.rs` call where the registry is (docs/decisions/0096).

use vike_config::account_arming::account_tier;

use crate::{AccountLabel, AdapterPolicy, ArmingRow, ArmingTable};

pub(crate) use crate::budget::require_live_risk_budget;

/// **A table row with the startup preflight's stop laid over it** — the one place the stop is
/// applied. `raw.1 != NoAccountSupport && tier != Paper && preflight_failed.contains(venue)` gives
/// `PreflightFailed` ABOVE `SidecarHeldElsewhere` (the table's block already includes the
/// one-sidecar override): a label the venue cannot address stays `NoAccountSupport`, a `paper`
/// account still runs, and an account the table itself does not run (no `tier`) keeps its cause.
pub(crate) fn with_the_preflight_stop(
    row: &ArmingRow,
    policy: Option<&AdapterPolicy>,
) -> vike_config::VenueArming {
    let mut arming = row.arming.clone();
    if arming.block != vike_config::ArmingBlock::NoAccountSupport
        && arming.tier.is_some_and(|tier| tier != vike_model::AccountTier::Paper)
        && policy.is_some_and(|p| p.preflight_failed.contains(arming.venue))
    {
        arming.effective = None;
        arming.block = vike_config::ArmingBlock::PreflightFailed;
    }
    arming
}

/// **THE per-account arming projection for one venue** — the rows `vike-backend venues` renders AND
/// the selection [`crate::make_engine_accounts`] starts from, so "Effective" cannot disagree with
/// the start. Each row is the account's tier from the `account` table, then the bridge's verdict at
/// that tier (the table's row), with the startup preflight's stop laid over it; an account the
/// TABLE does not run carries its cause (`NoAccountRow`, `AccountInactive`, `TierConflict`,
/// `AccountNotInStore`), a `paper` one `PaperTier`, and for neither is its bridge asked.
///
/// # ⚠ THERE IS NO SECOND, SYMBOL-SHAPED REFUSAL HERE ANY MORE
///
/// Two accounts on one instrument is an ordinary spread; refusing it was the defect
/// (`vike_config::venue_accounts`' module doc). Its replacement is [`shared_books_for`], a WARNING
/// computed beside these rows and never folded in: no row is downgraded by another row.
///
/// ⚠ **It takes no SYMBOL, on purpose**: an arming row is a fact about ONE account, and a symbol
/// parameter would reach nothing while looking as if it changed the answer.
/// [`crate::make_engine_accounts`] takes the per-account symbol map because it STARTS on it.
///
/// ⚠ **An id `vike_model::VENUES` does not carry answers with NO rows**:
/// `vike_config::VenueArming::venue` is a roster `&'static str`, so no row can name a `"sim"`.
/// [`crate::make_engine_accounts`] reads the empty answer as "one account, nothing to choose".
///
/// ⚠ **It takes the whole [`AdapterPolicy`]** for the stop (`preflight_failed`): the table was
/// computed from the SAME snapshot [`crate::make_engine_for_account`] reads — one snapshot, two
/// consumers.
#[must_use]
pub fn venue_account_arming(
    table: &ArmingTable,
    venue: &str,
    policy: Option<&AdapterPolicy>,
) -> Vec<vike_config::VenueArming> {
    table
        .rows()
        .iter()
        .filter(|row| row.arming.venue == venue)
        .map(|row| with_the_preflight_stop(row, policy))
        .collect()
}

/// **The `account` rows of EVERY roster venue** — [`venue_account_arming`] over `vike_model::VENUES`
/// in roster order, the projection `vike-backend venues` prints and the live mount's journal
/// records are cut from.
///
/// No network, no clock, no process env (decision 0095): the table was computed from the
/// `account` rows, the account's credentials and the deployment map, and this reads it.
/// ⚠ **One row per ACCOUNT, not per venue** — a box with no labelled account row and no labelled
/// key still gets exactly one row per roster venue (the DEFAULT account's).
#[must_use]
pub fn venue_arming(table: &ArmingTable, policy: &AdapterPolicy) -> Vec<vike_config::VenueArming> {
    table.rows().iter().map(|row| with_the_preflight_stop(row, Some(policy))).collect()
}

/// **Would `venue`'s DEFAULT account start a venue client at its OWN tier?** — the live-INTENT
/// predicate the startup preflight gates every leg on, and `make_engine_for_account` arms on: the
/// default account's table row (the preflight stop laid over it), `effective` a tier above `paper`.
/// An account the table does not run, a `paper` one and an id the table carries no row for answer
/// `false`. A DEFAULT account that cannot take a process-exclusive resource answers `false` too:
/// the preflight must not authenticate for a venue whose default account the start does not run.
#[must_use]
pub fn venue_would_go_live(
    table: &ArmingTable,
    venue: &str,
    policy: Option<&AdapterPolicy>,
) -> bool {
    table
        .row_of_account(venue, &AccountLabel::Default)
        .map(|row| with_the_preflight_stop(row, policy))
        .is_some_and(|arming| {
            arming.effective.is_some_and(|tier| tier != vike_model::AccountTier::Paper)
        })
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
pub fn stopped_by_the_live_switch(
    venues: &[&str],
    policy: &AdapterPolicy,
) -> Vec<crate::NotRunning> {
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
            let (tier, block) = match account_tier(Some(&policy.accounts), venue, &label) {
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
/// **This account's own exposure ceiling**: the tightest `account.max_exposure` among the ACTIVE
/// rows of `(venue, label)` (`vike_secrets::Accounts::max_exposure_of`), `None` = unbounded, or no
/// table. Folded with `vike_model::RiskLimits::narrow_account_exposure` (a `min`) AFTER the box-wide
/// `policy.max_account_exposure`, by the live assembly and the paper engine alike, so it can only
/// TIGHTEN that one.
pub(crate) fn account_max_exposure(
    policy: Option<&AdapterPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> Option<f64> {
    let Some(Ok(accounts)) = directory_of(policy).rows() else { return None };
    accounts.max_exposure_of(venue, label.text()).map(vike_secrets::MaxExposure::get)
}

/// **Why an account of a venue that has a rate source may NOT hold a symbol quoted in a currency other than
/// its own, or `None`** (decision 0183, phase 1), PURE and venue-free (WHICH venues have a rate source is the
/// root's to say: [`crate::AdapterPolicy::quote_converting_venues`]): only on the practice account (`demo`: no
/// venue's close has been measured against the converted PnL yet, which is what the decision's demo
/// experiment is for), and only with an ACCOUNT-wide exposure ceiling armed (the rule decision 0174 item 10
/// applies to a `live` account, here for the conversion). There is no switch beside these two: the tier gate IS
/// the switch, so a `live` account, a `paper` one and an uncapped account are each refused in their own words.
///
/// The engine's assembly reads it to decide whether to anchor the ledger to the venue's currency, and the
/// control door reads it (behind the venue list) to decide whether to admit a foreign-quoted symbol; one
/// function, so the two cannot disagree about which accounts convert.
#[must_use]
pub fn quote_conversion_tier_refusal(
    tier: Option<vike_model::AccountTier>,
    account_ceiling_armed: bool,
) -> Option<String> {
    match tier {
        Some(vike_model::AccountTier::Demo) => {}
        Some(other) => {
            return Some(format!(
                "a symbol quoted in another currency than the account's is admitted on a `demo` \
                 account only (the conversion is not yet measured against a venue's own balance), \
                 and this account runs at `{other}`"
            ));
        }
        None => {
            return Some(
                "this node does not say what kind of account this is, so a symbol quoted in \
                 another currency than the account's is not admitted"
                    .to_string(),
            );
        }
    }
    (!account_ceiling_armed).then(|| {
        "a symbol quoted in another currency than the account's needs an account-wide exposure \
         ceiling armed: `vike-cli config set policy.max_account_exposure <amount in the account's \
         currency>` (applied at the next restart), or this account's own `vike-cli secrets \
         account set-exposure`"
            .to_string()
    })
}

/// **This account's own margin leverage** (decision 0181): the lowest `account.leverage` among the
/// ACTIVE rows of `(venue, label)` (`vike_secrets::Accounts::leverage_of`), `None` = nothing stated
/// (the engine margins at 1x, as it always did), or no table. Only the SHARED TAIL reads it
/// ([`judge_account_leverage`] says what becomes of it), never the paper engine: a `paper` account
/// has no venue to confirm against.
pub(crate) fn account_leverage(
    policy: Option<&AdapterPolicy>,
    venue: &str,
    label: &AccountLabel,
) -> Option<f64> {
    let Some(Ok(accounts)) = directory_of(policy).rows() else { return None };
    accounts.leverage_of(venue, label.text()).map(vike_secrets::AccountLeverage::get)
}

/// **Whether this account is a LANE TWIN on MAINNET whose stated leverage MUST be what the venue holds
/// before the engine trades** (the owner opened binance's twin at `live`, 2026-10-19), PURE.
///
/// True exactly when the account is a DERIVED lane twin (`AccountDirectory::is_derived_lane_twin`, never
/// the label), runs at `live`, and its venue POSTS a leverage that the table opens at `live`. For such
/// an engine the demo rule ("a figure the venue did not confirm leaves the engine at 1x and trading") is
/// not good enough: the bridge's start WRITES a leverage to the real account, so a start that could not
/// hand over the owner's figure would leave the real account on whatever the venue held or on the
/// default, with the engine sizing at 1x. The twin then does not start (NOT RUNNING, said in words);
/// `make_engine_for_account` asks this before the venue is asked anything, and the shared tail asks it
/// again of the venue's answer.
///
/// Every other account is untouched: a `demo` twin, a labelled live account, the default live spot engine
/// and a venue the table keeps closed at `live` (bybit, okx) answer `false`, so they start exactly as they
/// did.
pub(crate) fn live_twin_must_hold_its_leverage(
    policy: Option<&AdapterPolicy>,
    venue: &str,
    account: &AccountLabel,
    tier: vike_model::AccountTier,
) -> bool {
    tier == vike_model::AccountTier::Live
        && vike_secrets::leverage_venue_at(venue, tier)
            .is_some_and(|row| row.means == vike_secrets::LeverageMeans::Posted)
        && account
            .text()
            .zip(policy)
            .is_some_and(|(label, p)| p.accounts.is_derived_lane_twin(venue, label))
}

/// The sentence a MAINNET lane twin is refused with when its stated leverage cannot be handed to the venue
/// (the ERROR line the operator reads; the arming word under it is `adapter failed`). `why` is the
/// specific cause.
pub(crate) fn live_twin_not_started(venue: &str, route_key: &str, why: &str) -> String {
    format!(
        "{venue}: the LIVE perp lane twin `{route_key}` does NOT start: {why}. Its stated leverage \
         is written to the REAL account at the start and the engine sizes against it, so a start \
         that cannot hand the figure over trades nothing (no default is written, no 1x stand-in \
         runs). Fix the cause, or clear the figure (`vike-cli secrets account set-leverage --id N \
         --leverage none`), then deactivate and activate the account"
    )
}

/// What the shared tail does with an account's stated leverage (decision 0181).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LeverageVerdict {
    /// The row states none: nothing changes (the engine's 1x default stands).
    Unstated,
    /// The venue confirmed it: the engine margins at `1 / leverage`.
    Applied { leverage: f64 },
    /// The figure is NOT applied (the engine keeps its 1x default), and why, in the operator's words.
    Refused(String),
}

/// **The whole-number leverage this account asks a venue that POSTS one to hold** (decision 0182),
/// PURE: `Some(floor(min(stated, policy.max_leverage, the profile's max_leverage)))` exactly when the
/// account's ACTIVE rows state a leverage AND `venue` is a `Posted` row of `vike_secrets::LEVERAGE_VENUES`
/// open at `tier`; `None` for every other account (nothing stated, hyperliquid, a closed tier, a
/// venue outside the table), which then starts exactly as it always did. `profile` is the run
/// profile ALREADY narrowed by `policy.max_leverage` (`ProfileRisk::narrowed_to_leverage_ceiling`),
/// the one the bridges are handed. Both the start query and the shared tail compute it, from the same
/// inputs, so the figure the daemon is asked for is the figure the engine then demands back.
pub(crate) fn account_start_leverage(
    policy: Option<&AdapterPolicy>,
    venue: &str,
    account: &AccountLabel,
    tier: vike_model::AccountTier,
    profile: Option<&vike_model::ProfileRisk>,
) -> Option<f64> {
    vike_secrets::leverage_venue_at(venue, tier)
        .filter(|row| row.means == vike_secrets::LeverageMeans::Posted)?;
    let stated = vike_secrets::AccountLeverage::new(account_leverage(policy, venue, account)?)?;
    // A figure the table would refuse (a fraction planted on a whole-number venue) is asked of nobody:
    // `judge_account_leverage` refuses it too, so the venue must not be written for it.
    if vike_secrets::leverage_refusal(venue, tier, stated.get()).is_some() {
        return None;
    }
    Some(
        stated.candidate(policy.and_then(|p| p.max_leverage), profile.and_then(|p| p.max_leverage)),
    )
}

/// **Whether a stated account leverage may reach the engine's margin rate**, PURE.
///
/// It is applied only when ALL of these hold: the row states one; the venue is a row of
/// `vike_secrets::LEVERAGE_VENUES` that is open at the account's `tier` and takes the figure as stated
/// (`vike_secrets::leverage_refusal`); and the gatewayhub returned the VENUE's own confirmation
/// (`AdapterBind::leverage_cap`) in the venue's own terms:
///
/// * a `Capped` venue (hyperliquid, decision 0181): `Ok(cap)` and the figure does not exceed `cap`;
/// * a `Posted` venue (bybit, okx, binance, decision 0182): `Ok(held)` and `held` IS `posted`, the
///   whole number the node asked the venue to hold (`account_start_leverage`). The daemon read the
///   venue back after the bridge's one write; equality is the only confirmation there is.
///
/// Every other case is a refusal that leaves the engine at 1x: a leverage the venue did not confirm
/// is never trusted, and a refusal is never a clamp (the operator asked for X and gets 1x and the
/// reason, not a silent Y).
pub(crate) fn judge_account_leverage(
    venue: &str,
    tier: vike_model::AccountTier,
    requested: Option<f64>,
    posted: Option<f64>,
    venue_answer: Option<&Result<f64, String>>,
) -> LeverageVerdict {
    let Some(requested) = requested else { return LeverageVerdict::Unstated };
    if let Some(why) = vike_secrets::leverage_refusal(venue, tier, requested) {
        return LeverageVerdict::Refused(why);
    }
    let Some(row) = vike_secrets::leverage_venue_at(venue, tier) else {
        // `leverage_refusal` answers `None` only for an open row; this is its mirror, kept so the
        // two cannot drift into an unconfirmed `Applied`.
        return LeverageVerdict::Refused(format!("{venue} at tier {tier:?} is not open"));
    };
    match venue_answer {
        None => LeverageVerdict::Refused(
            "the gatewayhub gave no venue confirmation of it (an older gatewayhub, or the venue \
             was not asked)"
                .to_string(),
        ),
        Some(Err(why)) => {
            LeverageVerdict::Refused(format!("the venue could not confirm it: {why}"))
        }
        Some(Ok(answer)) if !answer.is_finite() || *answer < 1.0 => LeverageVerdict::Refused(
            format!("the venue's confirmation ({answer}) is not a usable leverage"),
        ),
        Some(Ok(cap)) if row.means == vike_secrets::LeverageMeans::Capped => {
            if requested > *cap {
                LeverageVerdict::Refused(format!(
                    "it is above what the venue lets this account use ({cap}x)"
                ))
            } else {
                LeverageVerdict::Applied { leverage: requested }
            }
        }
        Some(Ok(held)) => match posted {
            Some(asked) if (held - asked).abs() < 1e-9 => {
                LeverageVerdict::Applied { leverage: asked }
            }
            Some(asked) => LeverageVerdict::Refused(format!(
                "the venue holds {held}x for it, not the {asked}x it was asked to hold"
            )),
            None => LeverageVerdict::Refused(
                "no figure was asked of the venue, so its answer confirms nothing".to_string(),
            ),
        },
    }
}

/// **The ROUTING key for one account** — `vike_exec::ExecutionEngine::route_key`, the
/// `LIVE-<route_key>.lock` sentinel filename, and the account's name in `live_venues`. Rendered
/// only by `vike_model::accounts::account_keys::AccountRef::route_key`, which carries no tier (one
/// process starts a venue at one tier). For `AccountLabel::Default` it is the bare venue id, as
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
/// Why the start, not the bridge: `Event::AccountState` names no symbol, so unscoped it folded into
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

/// **The symbol ONE account of a venue is armed on**, from the per-account map its composition root
/// derived (`crate::account_symbols_for`). An unnamed label falls back to the DEFAULT account's row
/// (the venue's wired symbol); an empty map answers `""`. Spelled once: the start and the node
/// assembly's probe both use it, and two fallbacks would start one symbol while the lock claimed
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
/// and starts anyway. It takes the ROWS so it cannot describe a different arming than the one
/// started, and is separate from [`venue_account_arming`] because it decides NOTHING: a caller
/// that never asks misses no refusal. An account whose book the table does not name (the backend
/// could not determine it offline) contributes no [`vike_config::ArmedBook`], so "warn nothing where
/// you cannot tell" holds by ABSENCE — and so does an account that does not run (no effective tier:
/// it has no book this process trades).
///
/// ⚠ **The book comes from the TABLE**, resolved RECORDED-first where the credentials are
/// (`crates/vike-gatewayhub/src/trade/adapters/book_identity.rs`'s `book_of_account`): the five
/// venues whose credentials name no account (binance, bybit, okx, deribit, dukascopy) can still
/// report a shared book once `vike-cli secrets set-book` recorded one.
#[must_use]
pub fn shared_books_for(
    table: &ArmingTable,
    rows: &[vike_config::VenueArming],
) -> Vec<vike_config::SharedBook> {
    let books: Vec<vike_config::ArmedBook> = rows
        .iter()
        .filter_map(|r| {
            let effective = r.effective?;
            let book = table.row_of_account(r.venue, &r.label)?.book.clone()?;
            Some(vike_config::ArmedBook {
                venue: r.venue,
                label: r.label.clone(),
                book,
                mode: effective,
            })
        })
        .collect();
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

/// **THE REFUSAL, SAID OUT LOUD: every account this box names on a venue whose start addresses only
/// ONE** — one process-wide message, or `None` when there is nothing to say.
///
/// ⚠ **The refusal was SILENT, and a silent refusal of a live-trading account is indistinguishable
/// from support.** `VenueDeclaration::addresses_accounts == false` makes the arming
/// return [`vike_config::ArmingBlock::NoAccountSupport`] and [`crate::accounts_to_run`] drop the
/// row, so [`crate::make_engine_for_account`] is never reached. MEASURED before this existed, on
/// two-dukascopy-account settings: three start
/// lines, none naming dukascopy; the row's own sentence (`vike_config::VenueArming::why`) had no
/// surface on the daemon.
///
/// ⚠ **A REPORT, never a refusal** (`docs/decisions/0013-degrade-vs-refuse.md`): the mount
/// continues on each default account. It becomes a refusal only for a strategy mount that NAMES
/// such an account
/// (`crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`): a strategy
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
    table: &ArmingTable,
    policy: Option<&AdapterPolicy>,
) -> Option<String> {
    let policy = policy?;
    // THE SAME PROJECTION THE START SELECTS WITH, so the warning cannot describe a refusal the
    // fan-out did not make; one roster walk makes it a single process-wide message.
    unaddressable_accounts_text(&venue_arming(table, policy))
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
/// A process-wide fact noticed per venue, latched once (the `vike_boot::halt` resolver's
/// idiom).
/// Emitted from the FAN-OUT, not [`crate::make_engine_for_account`]: a refused account never
/// reaches the per-account path, and the refused VENUE may not be started at all (a scaffolded
/// venue has no `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` engine row), so a line there is never read.
pub(crate) fn report_unaddressable_accounts(table: &ArmingTable, policy: Option<&AdapterPolicy>) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(message) = unaddressable_accounts_message(table, policy) {
            tracing::warn!("{message}");
        }
    });
}

/// **The `account` table a caller's policy carries**, or the UNREAD one with no policy — the ONE
/// spelling of that fallback. A `match`, not `map_or_else(AccountDirectory::unread_ref, …)`, which
/// unifies the arms at `'static` and fails to borrow-check.
pub(crate) fn directory_of(
    policy: Option<&AdapterPolicy>,
) -> &vike_secrets::account_directory::AccountDirectory {
    match policy {
        Some(p) => &p.accounts,
        None => vike_secrets::account_directory::AccountDirectory::unread_ref(),
    }
}

/// The `Account` per-symbol RULING-margin-mode grid for the ONE engine symbol — the margin-axis
/// twin of `contract_value_grid` (kept separate: different source, `onlyIsolated` vs contract value).
/// `None` whenever the mode IS `Cross`, so `Account::default_margin_mode_of` short-circuits to
/// [`vike_model::MarginMode::Cross`]: byte-identical and allocation-free for every start but
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

/// The `Account` per-symbol contract-value grid for the ONE engine symbol (decision 0170). `None`
/// (so `Account::contract_value_of` falls through to its `Linear`, `1.0` default) when the venue
/// reported no value or the DEFAULT one (`Linear`, exactly 1.0): identical through
/// `contract_value_of`, and the snapshot's `contract_values` map stays empty. A value that is
/// anything else (an inverse contract; a linear unit other than 1.0) lands under the mounted
/// symbol.
pub(crate) fn contract_value_grid(
    symbol: &str,
    value: vike_model::ContractValue,
) -> Option<indexmap::IndexMap<String, vike_model::ContractValue>> {
    // The grid is where a value becomes the engine's, so a degenerate unit is folded HERE too (the
    // other door in is `parts_from_bind`; a caller that builds a grid directly still passes this).
    let value = value.folded();
    (!value.is_default()).then(|| {
        let mut grid = indexmap::IndexMap::new();
        grid.insert(symbol.to_string(), value);
        grid
    })
}

/// The generic fee-schedule enrichment hook: prefers the venue's LIVE account-actual rate
/// (`ReconClient::fetch_fee_rates`, overridden by binance/bybit/okx/deribit) over `static_default`
/// (the once-evaluated [`vike_venues::fee_schedule_for`], shared with the paper fill path).
/// **Fail-soft**: an error, `Ok(None)` or no recon client falls back to the default — a fee fetch
/// never blocks starting. One blocking, best-effort account read at start time.
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

#[path = "arming_tests.rs"]
#[cfg(test)]
mod arming_tests;
