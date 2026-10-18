//! Which accounts arm: the pre-assembly live set, the mount accounts, both refusals, the fan-out.

use std::collections::HashSet;

use vike_core::CoreConfig;

#[cfg(doc)]
use super::AssembledRuntime;
#[cfg(doc)]
use super::WiredMarket;
#[cfg(doc)]
use super::build::assemble_runtime;
use super::mounts::declared_legs_for;
use super::{RuntimeConfig, RuntimeError};

/// **The accounts this node will actually ARM live — computed BEFORE anything is constructed.**
///
/// For each wired-market venue ([`crate::ArmingTable::wired_markets`], in MOUNT order),
/// every `crate::venue_account_arming` row whose effective tier is a venue client (`demo`/`live`) —
/// the projection [`assemble_runtime`]'s fan-out mounts from, asked from a PURE function that opens no
/// socket, signs nothing and builds no client.
///
/// # Why
///
/// The B11 live-account lock (`vike_boot::live_lock`) claims one sentinel per venue ACCOUNT, worth
/// something only if it covers what is actually live and is held BEFORE the first exec client
/// exists. Neither composition root could compute that set on its own:
///
/// * `vike-tradehub` locked its RUN PROFILE's mount set — which pairs carry a STRATEGY — while this
///   node arms an exec client for every credentialled wired venue. MEASURED on the CI box, one startup
///   of the shipped daemon, two lines apart:
///   `live_venues={"hyperliquid","deribit","okx","bybit","alpaca","aster","binance","ig","oanda"}`
///   beside `{"kind":"ready","mode":"LIVE (venue=bybit)"}` — nine live sessions, one lock; a second
///   process with another profile locked a venue the first never had, and both traded one account.
/// * `vike-app` locked the RIGHT set ([`AssembledRuntime::live_venues`]) at the WRONG TIME: it exists only
///   after [`assemble_runtime`] built every exec client, and three arms
///   (`crates/bridges/bybit/src/exec.rs`, `crates/bridges/okx/src/exec.rs`,
///   `crates/bridges/aster/src/exec.rs` — each posts `set_leverage` at startup) make a
///   post-construction refusal no longer side-effect-free.
///
/// # Direction of error
///
/// The probe is INTENT-based, so this set can be a strict SUPERSET of the [`AssembledRuntime::live_venues`] the
/// mount records: a venue whose synchronous connect fails (ctrader/ibkr) or whose factory declines
/// a present-but-bad key (hyperliquid/polymarket) probes live and then does not run — one extra
/// sentinel, a SECOND process refused there. The other direction, a venue arming with nothing
/// holding its lock, is what this seam prevents; [`refuse_unarmed_live_venues`] is its backstop.
///
/// # ⚠ It answers ROUTE KEYS, one per ACCOUNT — not venue ids
///
/// The lock protects a venue ACCOUNT, and the mount fans out per account. A route key IS that
/// identity — `vike_exec::ExecutionEngine::route_key`, the name `make_engine_accounts` records in
/// `live_venues`, the `LIVE-<route_key>.lock` filename — so the claimed, recorded and compared sets
/// share one vocabulary. A DEFAULT account's key is the bare venue id, so a box with no labelled
/// account claims the sentinel filenames it always did. It walks
/// `crate::venue_account_arming`, not the coarser `crate::would_go_live_under`, because only the
/// finer one enumerates ACCOUNTS and reads the policy the mount reads.
///
/// # ⚠ It does NOT depend on the strategy mount set
///
/// A labelled account ARMS on its ACTIVE `account` row's tier plus its own credentials,
/// and `crate::make_engine_accounts` mounts every active account whether or not a strategy names
/// it, so the sentinels to claim are a fact about the SETTINGS alone. Which account a strategy
/// TRADES on is answered in `vike_core` and refused here by [`refuse_unarmed_mount_accounts`].
#[must_use]
pub fn armed_live_venues(table: &crate::ArmingTable, policy: &crate::AdapterPolicy) -> Vec<String> {
    table
        .wired_markets()
        .iter()
        .flat_map(|m| crate::venue_account_arming(table, m.venue, Some(policy)))
        .filter(|row| row.effective.is_some_and(|t| t != vike_model::AccountTier::Paper))
        .map(|row| row.route_key())
        .collect()
}

/// **One strategy mount's ROUTING declaration, as strings** — what [`account_symbols_for`] and
/// `refuse_unarmed_mount_accounts` read. A named type, not a tuple, because three same-typed slots
/// say nothing about which is which. Built from a [`vike_core::CoreConfig`]'s mounts by
/// [`runnable_accounts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAccount {
    /// The mount's venue — a `vike_model::VENUES` id.
    pub venue: String,
    /// The instrument this mount trades, which is the symbol its ACCOUNT is armed on.
    pub symbol: String,
    /// Which account of that venue, `None` for the venue's default one.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
}

/// The [`RuntimeAccount`] rows a built [`CoreConfig`] declares, read back off the config so
/// [`assemble_runtime`]'s named-account refusal and per-account symbols judge the very mount set it is
/// about to assemble.
#[must_use]
pub fn runnable_accounts(core: &CoreConfig) -> Vec<RuntimeAccount> {
    core.strategy
        .iter()
        .chain(core.extra_mounts.iter())
        .map(|m| RuntimeAccount {
            venue: m.venue.clone(),
            symbol: m.symbol.clone(),
            account: m.account.clone(),
        })
        .collect()
}

/// **WHICH SYMBOL each account of `venue` is armed on**, so a labelled account's ENGINE mounts on
/// the instrument its own strategy trades.
///
/// ⚠ Two accounts may land on the SAME symbol: that is a spread (`vike_config::venue_accounts`),
/// and nothing about this map is a uniqueness claim.
///
/// * the DEFAULT account is ALWAYS present and ALWAYS `wired_symbol`, even if a mount names it on
///   another instrument: [`assemble_runtime`] mounts the venue's primary engine on
///   [`WiredMarket::symbol`], and `vike_tradehub::config::DaemonProfile::validate_for_live` already
///   refuses an account-less live row naming anything else;
/// * one entry per DISTINCT labelled account a mount on this venue names, on that mount's own
///   `symbol` — the mount decides the instrument and names the account, so no settings key
///   restates it;
/// * a labelled account NO mount names gets no entry, so `crate::symbol_for_account` falls back
///   to the wired symbol: armed by its account row, reachable INBOUND, traded by no
///   strategy;
/// * mounts on OTHER venues contribute nothing.
///
/// Order: `Default` first (load-bearing: `crate::make_engine_accounts` returns its engine at `[0]`
/// and callers bind it), then labelled accounts in DECLARATION order.
///
/// ⚠ **Two mounts naming ONE labelled account on TWO symbols is a LOAD refusal**
/// (`vike_tradehub::config::DaemonProfile`'s multi-mount validation names both rows); this function
/// takes the FIRST mount's symbol only so that it is total.
#[must_use]
pub fn account_symbols_for(
    mounts: &[RuntimeAccount],
    venue: &str,
    wired_symbol: &str,
) -> Vec<(vike_model::accounts::account_keys::AccountLabel, String)> {
    use vike_model::accounts::account_keys::AccountLabel;
    let mut out: Vec<(AccountLabel, String)> =
        vec![(AccountLabel::Default, wired_symbol.to_string())];
    for m in mounts {
        if m.venue != venue {
            continue;
        }
        let Some(label) = m.account.as_ref().filter(|l| !l.is_default()) else { continue };
        if out.iter().any(|(l, _)| l == label) {
            continue;
        }
        out.push((label.clone(), m.symbol.clone()));
    }
    out
}

/// **THE LOUD REFUSAL: a mount naming an account this box will not RUN must not start.**
///
/// A strategy runs on its account's engine, and an account that does not run has none (decision
/// 0156: no paper engine in its place). So a mount whose account does not run cannot run either,
/// and the node refuses rather than coming up without it: a strategy silently on the DEFAULT
/// account when its author named another trades the wrong book, and one silently not running at
/// all is a daemon that looks up and trades nothing. The refusal names the account and its reason.
///
/// ⚠ **Since decision 0156 this covers the DEFAULT account too.** It used to skip a mount naming no
/// account, because the default account was always mounted (on paper when it did not arm); it is
/// mounted only when it runs now, so an account-less mount on a venue whose default account does
/// not run is refused here, by name, instead of a mount refused in `vike_core`'s `resolve_mount_engines`.
///
/// Checked against the SAME `crate::venue_account_arming` the fan-out selects with (same vars, same
/// policy), so it cannot refuse a mount the fan-out would arm or pass one it would not — BEFORE
/// anything is built. An account the probe arms and whose mount then does not come up (a failed
/// connect, a declined key) is caught after the build by [`refuse_mounts_without_an_engine`].
pub(super) fn refuse_unarmed_mount_accounts(
    cfg: &RuntimeConfig,
    table: &crate::ArmingTable,
    mounts: &[RuntimeAccount],
) -> Result<(), RuntimeError> {
    for m in mounts {
        let label =
            m.account.clone().unwrap_or(vike_model::accounts::account_keys::AccountLabel::Default);
        let rows = crate::venue_account_arming(table, &m.venue, Some(&cfg.policy));
        // A venue the roster does not carry (a test's planted id) has no rows to judge by: the
        // mount itself answers for it, and the post-build check refuses what did not run.
        if rows.is_empty() {
            continue;
        }
        match rows.iter().find(|row| row.label == label) {
            Some(row) if row.runs() => {}
            row => {
                let why = row.map_or("no account row names it", |r| r.block.as_str());
                return Err(RuntimeError::Mount(unrunnable_mount(m, &label, why)));
            }
        }
    }
    Ok(())
}

/// **THE POST-BUILD HALF of [`refuse_unarmed_mount_accounts`]**: every strategy mount's account
/// has an ENGINE, or the node is refused — naming the account and the reason the mount gave
/// (`not_running`), before `spawn_core_multi` would panic on the same miss. Reached by an account
/// the probe armed whose mount did not come up (`ArmingBlock::AdapterFailed`), and by a planted
/// non-roster venue the projection cannot judge.
pub(super) fn refuse_mounts_without_an_engine(
    mounts: &[RuntimeAccount],
    engine_route_keys: &[&str],
    not_running: &[crate::NotRunning],
) -> Result<(), RuntimeError> {
    use vike_model::accounts::account_keys::AccountLabel;
    for m in mounts {
        let label = m.account.clone().unwrap_or(AccountLabel::Default);
        let key = crate::arming::account_route_key(&m.venue, &label);
        if engine_route_keys.contains(&key.as_str()) {
            continue;
        }
        let why = not_running
            .iter()
            .find(|nr| nr.route_key() == key)
            .map_or("this runtime builds no engine for it", |nr| nr.block.as_str());
        return Err(RuntimeError::Mount(unrunnable_mount(m, &label, why)));
    }
    Ok(())
}

/// The refusal sentence for a strategy mount whose account does not run.
fn unrunnable_mount(
    m: &RuntimeAccount,
    label: &vike_model::accounts::account_keys::AccountLabel,
    why: &str,
) -> String {
    let key = crate::arming::account_route_key(&m.venue, label);
    if label.is_default() {
        format!(
            "strategy mount on {}/{} trades {}'s DEFAULT account, which does NOT RUN on this box \
             ({why}) — no `{key}` engine exists, and an account that does not run is never \
             replaced by a paper engine (decision 0156). Run it: `vike-cli secrets accounts` to \
             find its row, then `vike-cli secrets account activate --id N` / `set-tier --id N \
             --tier <paper|demo|live>`, plus its credential keys for a demo or live tier; \
             `vike-backend venues` names the block. Or drop the mount.",
            m.venue, m.symbol, m.venue
        )
    } else {
        format!(
            "strategy mount on {}/{} names account `{label}`, which does NOT RUN on this box ({why}) \
             — so no `{key}` engine exists, and the mount will not run on {}'s DEFAULT account, \
             trading a book its author did not choose. Run it: `vike-cli secrets accounts` to find \
             its row, then `vike-cli secrets account activate --id N` (and `set-tier`), plus that \
             account's own `__{label}` credential keys (there is NO fallback to the unlabelled \
             keys). Or drop `account` from the mount.",
            m.venue, m.symbol, m.venue
        )
    }
}

/// **THE BACKSTOP: nothing may arm live outside the set that was locked.**
///
/// [`armed_live_venues`] is a pre-assembly probe, and a live arm it misses was otherwise caught only
/// by the post-merge RISK-BUDGET backstop (`crate::venue_arming_under`'s doc). For a LOCK,
/// under-counting is the dangerous direction: the venue arms, places real orders, and no sentinel
/// is held for its account — the accident the lock exists to refuse, looking like a correct start.
///
/// So [`assemble_runtime`] compares the pre-assembly armed set the caller locked from with the post-assembly
/// `live_venues` record the mount (`crate::make_engine_accounts`) wrote, and refuses the whole node
/// when the record is not a SUBSET. One-directional on purpose: `armed ⊋ live_venues` is the
/// accepted over-count, and refusing it would fail an ordinary bad-key/failed-connect startup.
///
/// ⚠ **Both sides are ROUTE KEYS**: an armed `binance#ALT` the probe did not name would otherwise
/// be masked by a `binance` in both sets — exactly the unlocked account this refusal catches.
pub fn refuse_unarmed_live_venues(
    armed: &[String],
    live_venues: &HashSet<String>,
) -> Result<(), RuntimeError> {
    let mut unarmed: Vec<String> =
        live_venues.iter().filter(|v| !armed.contains(v)).cloned().collect();
    if unarmed.is_empty() {
        return Ok(());
    }
    unarmed.sort();
    Err(RuntimeError::UnarmedLiveVenues { venues: unarmed, armed: armed.to_vec() })
}

/// The per-account extras the wired-market loop accumulates: engines to append to `extra` (after
/// every default engine), their reconcile legs, and every account that does NOT RUN (decision
/// 0156). A venue's SECOND account is the same market mounted again, not a second market.
#[derive(Default)]
pub(super) struct AccountExtras {
    pub engines: Vec<(f64, vike_exec::ExecutionEngine<Box<dyn vike_exec::ExecutionClient + Send>>)>,
    pub(super) recon: Vec<crate::ReconLeg>,
    pub(super) not_running: Vec<crate::NotRunning>,
}

/// Mount every account of one wired market that RUNS, hand back the DEFAULT account's engine
/// (placed in the core by the row's `engine_rank`) — `None` when the default account does not run —
/// and push the rest into `extras`, the accounts that do not run included.
///
/// ⚠ `crate::make_engine_accounts` guarantees the default account is FIRST when it runs; with one
/// account per venue `extras` never grows an engine.
///
/// Each extra account's reconcile handle becomes a `vike_tradehub_core::ReconLeg` carrying TWO facts: the
/// CANONICAL venue id and that account's route key (`venue#LABEL`). The fold thread routes the pass
/// by route key; every venue-shaped job — the health probe, the ring notes, the held-alert dedup
/// identity — reads a real `vike_model::VENUES` id.
///
/// ⚠ **Keyed by route key ALONE, the leg reached `vike_core`'s per-venue health gate as an unknown
/// venue, which reads `Healthy` on a miss.** Two accounts are different books, but the probe asks
/// whether THE EXCHANGE'S FEED is mid-gap — one fact both passes read through — so a degraded
/// binance feed must suppress both binance legs.
pub(super) fn runnable_accounts_of(
    market: (&str, &str),
    cfg: &RuntimeConfig,
    table: &crate::ArmingTable,
    live_events: &vike_exec::EventSender,
    live_venues: &mut HashSet<String>,
    recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    extras: &mut AccountExtras,
) -> Result<Option<crate::EngineAndRecon>, RuntimeError> {
    let (venue, symbol) = market;
    // ⚠ ONE row per ACCOUNT, derived from the MOUNTS in `cfg.core_config` (not passed in), the same
    // `runnable_accounts` set `refuse_unarmed_mount_accounts` judged; `refuse_unarmed_live_venues`
    // then fails the node if what armed is not a subset of what the root locked.
    let account_symbols = account_symbols_for(&runnable_accounts(&cfg.core_config), venue, symbol);
    let mut env = crate::AdapterEnv::new(&*cfg.backend, table, live_events, live_venues);
    env.recon_enabled = cfg.recon_enabled;
    env.recon_trigger = recon_trigger;
    env.properties_rec = cfg.properties_rec.clone();
    env.risk_profile = cfg.risk_profile.as_ref();
    env.policy = Some(&cfg.policy);
    let crate::AccountEngines { running, not_running } = crate::make_engine_accounts(
        &mut env,
        venue,
        &account_symbols,
        &declared_legs_for(&cfg.core_config, venue, symbol),
    )
    .map_err(RuntimeError::RiskBudget)?;
    extras.not_running.extend(not_running);
    // The default account is first when it runs (that function's order contract).
    let mut running = running.into_iter().peekable();
    let default = running.next_if(|(label, _)| label.is_default()).map(|(_, engine)| engine);
    for (label, (extra_engine, extra_recon)) in running {
        let route_key = extra_engine.route_key.clone();
        // ⚠ The engine's OWN symbol: logging the wired `symbol` would name the DEFAULT account's
        // market on a line about a different book.
        let account_symbol = extra_engine.symbol.clone();
        tracing::warn!(
            venue,
            symbol = %account_symbol,
            account = %label,
            route_key = %route_key,
            "mounting {venue} account `{label}` — its fills, positions and reconcile are its own \
             book, separate from every other {venue} account's"
        );
        extras.engines.push((cfg.seed_cash, extra_engine));
        if let Some(rc) = extra_recon {
            // BOTH facts: the canonical venue id (health gate, notes) and THIS account's route key
            // (which of the venue's engines the reports belong to).
            extras.recon.push(crate::ReconLeg::account(venue, route_key, rc));
        }
    }
    Ok(default)
}
